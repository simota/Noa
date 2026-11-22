//! Local git inspection for live worktrees discovered via `noa-ipc`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{ChangedFile, DiffMode};

#[derive(Debug, Clone)]
pub struct WorktreeGitInfo {
    pub repo_name: String,
    pub repo_root: String,
    pub worktree_root: String,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<ChangedFile>,
    pub note: Option<String>,
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn canonical_or_raw(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub fn inspect_worktree(cwd: &str, mode: DiffMode) -> Option<WorktreeGitInfo> {
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return None;
    }
    let toplevel = run_git(dir, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_owned();
    if toplevel.is_empty() {
        return None;
    }
    let common_raw = run_git(dir, &["rev-parse", "--git-common-dir"])
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| ".git".into());
    // A relative `--git-common-dir` is relative to the directory git ran in,
    // not to the worktree top level.
    let common_path = if Path::new(&common_raw).is_absolute() {
        PathBuf::from(&common_raw)
    } else {
        dir.join(&common_raw)
    };
    // The common dir identifies the repository. Only a `<repo>/.git` common
    // dir names its parent; a bare repo (`a.git`) or a submodule
    // (`.git/modules/<name>`) is its own root, so siblings never merge.
    let common = PathBuf::from(canonical_or_raw(&common_path));
    let (repo_root, repo_name) = if common.file_name().is_some_and(|n| n == ".git") {
        let root = common
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common.clone());
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
        (root, name)
    } else {
        let name = common.file_name().map(|n| {
            let n = n.to_string_lossy();
            n.strip_suffix(".git").unwrap_or(&n).to_owned()
        });
        (common.clone(), name)
    };
    let repo_root = repo_root.to_string_lossy().into_owned();
    let repo_name = repo_name.unwrap_or_else(|| "repo".into());
    // Paths must be relative to the worktree root, whatever the pane's cwd
    // or `diff.relative` say, so run the listing commands from there.
    let top = Path::new(&toplevel);

    let status_out = run_git(
        top,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    )
    .unwrap_or_default();
    let PorcelainStatus {
        mut branch,
        ahead,
        behind,
        files: mut status_map,
    } = parse_porcelain_v2(&status_out);
    if branch.is_empty() {
        branch = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|| "HEAD".into());
    }

    let range = base_range(top);
    let numstat_args: Vec<&str> = match mode {
        DiffMode::Unstaged => vec!["diff", "--numstat", "-z"],
        DiffMode::Staged => vec!["diff", "--cached", "--numstat", "-z"],
        DiffMode::VsMain => match &range {
            Some(range) => vec!["diff", "--numstat", "-z", range.as_str()],
            None => Vec::new(),
        },
    };
    let note = (mode == DiffMode::VsMain && range.is_none())
        .then(|| "比較元ブランチ (main / master / origin/HEAD) が見つかりません".to_owned());
    let numstat_out = if numstat_args.is_empty() {
        String::new()
    } else {
        run_git(top, &numstat_args).unwrap_or_default()
    };
    let mut stats = parse_numstat(&numstat_out);

    let mut files = Vec::new();
    match mode {
        DiffMode::VsMain => {
            for (path, (added, deleted)) in stats.drain() {
                files.push(ChangedFile {
                    path,
                    status: "M".into(),
                    added,
                    deleted,
                    diff: String::new(),
                    conflict_worktrees: Vec::new(),
                });
            }
        }
        DiffMode::Staged | DiffMode::Unstaged => {
            for (path, code) in status_map.drain(..) {
                let want = match mode {
                    DiffMode::Staged => code.0 != '.' && code.0 != '?',
                    DiffMode::Unstaged => code.1 != '.' || code.0 == '?',
                    DiffMode::VsMain => true,
                };
                if !want {
                    continue;
                }
                let (added, deleted) = stats.remove(&path).unwrap_or((0, 0));
                let status = if code.0 == '?' {
                    "?".to_owned()
                } else if code.0 == 'U' {
                    "U".to_owned()
                } else if mode == DiffMode::Staged {
                    code.0.to_string()
                } else {
                    code.1.to_string()
                };
                files.push(ChangedFile {
                    path,
                    status,
                    added,
                    deleted,
                    diff: String::new(),
                    conflict_worktrees: Vec::new(),
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Some(WorktreeGitInfo {
        repo_name,
        repo_root,
        worktree_root: canonical_or_raw(Path::new(&toplevel)),
        branch,
        ahead,
        behind,
        files,
        note,
    })
}

/// `<base>...HEAD` for the "vs main" mode.
fn base_range(dir: &Path) -> Option<String> {
    Some(format!("{}...HEAD", base_ref(dir)?))
}

/// The branch the "vs main" mode and the merge prediction compare against:
/// the local `main`, else the local `master`, else whatever `origin/HEAD`
/// points at.
fn base_ref(dir: &Path) -> Option<String> {
    ["main", "master"]
        .into_iter()
        .find(|name| {
            run_git(
                dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{name}"),
                ],
            )
            .is_some()
        })
        .map(str::to_owned)
        .or_else(|| {
            run_git(
                dir,
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        })
}

/// Every worktree of the repository `dir` is in, as canonical paths, from
/// `git worktree list --porcelain`. A bare entry has no checkout and a
/// prunable one has lost its directory, so neither is listed.
pub fn list_worktrees(dir: &str) -> Vec<String> {
    let out = run_git(Path::new(dir), &["worktree", "list", "--porcelain"]).unwrap_or_default();
    parse_worktree_list(&out)
        .into_iter()
        .map(|path| canonical_or_raw(Path::new(&path)))
        .collect()
}

/// Parses `git worktree list --porcelain`: blank-line separated records
/// whose first line is `worktree <path>`, followed by attribute lines such
/// as `bare` or `prunable <reason>`.
fn parse_worktree_list(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for record in text.split("\n\n") {
        let mut path = None;
        let mut skip = false;
        for line in record.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.to_owned());
            } else if line == "bare" || line == "prunable" || line.starts_with("prunable ") {
                skip = true;
            }
        }
        if let Some(path) = path.filter(|_| !skip) {
            paths.push(path);
        }
    }
    paths
}

/// The paths that would conflict if a worktree's branch were merged into the
/// base, cached per worktree root and keyed by the two commits merged, so
/// `git merge-tree` only runs again when the base or HEAD moves.
#[derive(Debug, Default)]
pub struct MergePredictions {
    entries: HashMap<String, MergeEntry>,
    /// How many times `git merge-tree` ran, for tests.
    runs: usize,
}

#[derive(Debug)]
struct MergeEntry {
    base: String,
    head: String,
    conflicts: Option<Vec<String>>,
}

impl MergePredictions {
    /// `Some(paths)` that conflict (empty when the merge is clean); `None`
    /// when there is nothing to predict (the worktree is on the base, or
    /// the repository has no base) or `git merge-tree` could not tell
    /// (git before 2.38, an error).
    pub fn predict(&mut self, worktree_root: &str, branch: &str) -> Option<Vec<String>> {
        let dir = Path::new(worktree_root);
        let base = base_ref(dir)?;
        if branch == base {
            return None;
        }
        let shas = run_git(
            dir,
            &["rev-parse", &format!("{base}^{{commit}}"), "HEAD^{commit}"],
        )?;
        let mut shas = shas.lines().map(str::to_owned);
        let (base_sha, head_sha) = (shas.next()?, shas.next()?);
        if let Some(entry) = self.entries.get(worktree_root)
            && entry.base == base_sha
            && entry.head == head_sha
        {
            return entry.conflicts.clone();
        }
        self.runs += 1;
        let conflicts = merge_tree_conflicts(dir, &base_sha, &head_sha);
        self.entries.insert(
            worktree_root.to_owned(),
            MergeEntry {
                base: base_sha,
                head: head_sha,
                conflicts: conflicts.clone(),
            },
        );
        conflicts
    }

    /// Forgets the worktrees `keep` rejects (gone from the browser).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|root, _| keep(root));
    }
}

/// Runs `git merge-tree --write-tree` (git >= 2.38) to merge `head` into
/// `base` without touching any worktree or ref. Exit status 1 means
/// conflicts, 0 a clean merge, anything else that git could not tell.
fn merge_tree_conflicts(dir: &Path, base: &str, head: &str) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            // NUL-separated, so paths are never C-quoted.
            "-z",
            base,
            head,
        ])
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(Vec::new()),
        Some(1) => {
            let text = String::from_utf8_lossy(&out.stdout);
            // `<tree oid>\0` then one `<path>\0` per conflicted file.
            Some(
                text.split('\0')
                    .skip(1)
                    .take_while(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        }
        _ => None,
    }
}

pub fn file_diff(cwd: &str, path: &str, mode: DiffMode) -> String {
    let dir = Path::new(cwd);
    let range = base_range(dir);
    // Plain unified diff of exactly `path`: the path is a literal, not a
    // pathspec pattern, and no external diff tool or colour reformats it.
    let mut args: Vec<&str> = vec!["--literal-pathspecs", "diff", "--no-ext-diff", "--no-color"];
    match mode {
        DiffMode::Unstaged => {}
        DiffMode::Staged => args.push("--cached"),
        DiffMode::VsMain => match &range {
            Some(range) => args.push(range.as_str()),
            None => return format!("(比較元ブランチ main / master が見つかりません: {path})"),
        },
    }
    // A rename is only paired, and its edits shown, when both of its paths
    // are in the pathspec.
    let orig = (mode != DiffMode::Unstaged)
        .then(|| rename_source(dir, &args, path))
        .flatten();
    args.push("--");
    args.extend(orig.as_deref());
    args.push(path);
    let out = run_git(dir, &args).unwrap_or_default();
    if !out.trim().is_empty() {
        return out;
    }
    // Untracked file fallback: show contents as added lines.
    if mode == DiffMode::Unstaged {
        let full = dir.join(path);
        // A symlink's content is its target path (as git stores it), never
        // the file it points at, which may lie outside the repository.
        if let Ok(target) = std::fs::read_link(&full) {
            return format!(
                "new file mode 120000\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{}\n",
                target.display()
            );
        }
        if let Some(preview) = read_preview(&full) {
            let lines: Vec<&str> = preview.lines().take(PREVIEW_LINES).collect();
            let mut buf = format!(
                "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
                lines.len()
            );
            for line in lines {
                buf.push('+');
                buf.push_str(line);
                buf.push('\n');
            }
            return buf;
        }
    }
    format!("(差分なし: {path})")
}

/// How much of an untracked file the preview shows: at most this many lines
/// of at most this many leading bytes, so a huge file is never read whole.
const PREVIEW_LINES: usize = 500;
const PREVIEW_BYTES: u64 = 64 * 1024;

/// The first [`PREVIEW_BYTES`] of `path` as text, or `None` if it cannot be
/// read (a directory, a missing file).
fn read_preview(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(PREVIEW_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The source path of `path` when the diff `args` (without pathspecs) pair
/// it as the destination of a rename or copy.
fn rename_source(dir: &Path, args: &[&str], path: &str) -> Option<String> {
    let mut status = args.to_vec();
    status.extend(["--name-status", "-z"]);
    let out = run_git(dir, &status)?;
    let mut entries = out.split('\0');
    while let Some(code) = entries.next() {
        if code.starts_with('R') || code.starts_with('C') {
            let (from, to) = (entries.next()?, entries.next()?);
            if to == path {
                return Some(from.to_owned());
            }
        } else {
            entries.next();
        }
    }
    None
}

struct PorcelainStatus {
    branch: String,
    ahead: usize,
    behind: usize,
    files: Vec<(String, (char, char))>,
}

/// Parses `git status --porcelain=v2 --branch -z` output. Paths are raw
/// (never C-quoted) in `-z` mode; a rename/copy record is followed by its
/// original path as a separate NUL-terminated entry.
fn parse_porcelain_v2(text: &str) -> PorcelainStatus {
    let mut branch = String::new();
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();
    let mut entries = text.split('\0');
    while let Some(entry) = entries.next() {
        if let Some(rest) = entry.strip_prefix("# branch.head ") {
            branch = rest.trim().to_owned();
        } else if let Some(rest) = entry.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if entry.starts_with("1 ") || entry.starts_with("2 ") {
            // `1 XY sub mH mI mW hH hI path`
            // `2 XY sub mH mI mW hH hI Xscore path` + NUL + orig path
            let renamed = entry.starts_with("2 ");
            let fields = if renamed { 10 } else { 9 };
            let parts: Vec<&str> = entry.splitn(fields, ' ').collect();
            if renamed {
                entries.next();
            }
            if parts.len() == fields {
                let mut chars = parts[1].chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                files.push((parts[fields - 1].to_owned(), (x, y)));
            }
        } else if entry.starts_with("u ") {
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`: unmerged, shown in both modes.
            let parts: Vec<&str> = entry.splitn(11, ' ').collect();
            if parts.len() == 11 {
                files.push((parts[10].to_owned(), ('U', 'U')));
            }
        } else if let Some(path) = entry.strip_prefix("? ") {
            files.push((path.to_owned(), ('?', '?')));
        }
    }
    PorcelainStatus {
        branch,
        ahead,
        behind,
        files,
    }
}

/// Parses `git diff --numstat -z`. A rename is `added\tdeleted\t` followed by
/// the old and new paths as separate entries; it is keyed by the new path.
fn parse_numstat(text: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    let mut entries = text.split('\0');
    while let Some(entry) = entries.next() {
        let mut parts = entry.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if p.is_empty() {
            entries.next();
            match entries.next() {
                Some(new) => new,
                None => continue,
            }
        } else {
            p
        };
        let added = a.parse().unwrap_or(0);
        let deleted = d.parse().unwrap_or(0);
        map.insert(path.to_owned(), (added, deleted));
    }
    map
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2_branch_and_files() {
        let sample = "# branch.oid 123456\0\
# branch.head feat/test\0\
# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 a b src/main.rs\0\
1 A. N... 000000 100644 100644 0 c src/new.rs\0\
? notes.txt\0\
2 R. N... 100644 100644 100644 a a R100 src/renamed name.rs\0src/orig.rs\0\
u UU N... 100644 100644 100644 100644 a b c src/conflict.rs\0";
        let parsed = parse_porcelain_v2(sample);
        assert_eq!(parsed.branch, "feat/test");
        assert_eq!(parsed.ahead, 2);
        assert_eq!(parsed.behind, 1);
        assert_eq!(parsed.files.len(), 5);
        assert_eq!(parsed.files[3], ("src/renamed name.rs".into(), ('R', '.')));
        assert_eq!(parsed.files[4], ("src/conflict.rs".into(), ('U', 'U')));
        assert_eq!(parsed.files[0], ("src/main.rs".into(), ('.', 'M')));
        assert_eq!(parsed.files[1], ("src/new.rs".into(), ('A', '.')));
        assert_eq!(parsed.files[2], ("notes.txt".into(), ('?', '?')));
    }

    // ── Regression tests against real temporary repositories ─────────────────

    pub(crate) struct TempRepo(pub PathBuf);

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success() || args.first() == Some(&"merge"),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    pub(crate) fn temp_repo(name: &str, branch: &str) -> TempRepo {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "repo-browser-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", branch]);
        git(&dir, &["config", "core.quotePath", "true"]);
        std::fs::write(dir.join("old.rs"), "fn a() {}\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        TempRepo(dir)
    }

    fn cwd(repo: &TempRepo) -> &str {
        repo.0.to_str().unwrap()
    }

    #[test]
    fn staged_rename_uses_new_path_and_shows_diff() {
        let repo = temp_repo("rename", "main");
        git(&repo.0, &["mv", "old.rs", "new.rs"]);
        let info = inspect_worktree(cwd(&repo), DiffMode::Staged).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["new.rs"]);
        let diff = file_diff(cwd(&repo), "new.rs", DiffMode::Staged);
        assert!(!diff.starts_with("(差分なし"), "{diff}");
    }

    #[test]
    fn quoted_non_ascii_path_is_decoded() {
        let repo = temp_repo("quote", "main");
        std::fs::write(repo.0.join("日本語.txt"), "こんにちは\n").unwrap();
        std::fs::write(repo.0.join("old.rs"), "fn b() {}\n").unwrap();
        let info = inspect_worktree(cwd(&repo), DiffMode::Unstaged).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["old.rs", "日本語.txt"]);
        let diff = file_diff(cwd(&repo), "日本語.txt", DiffMode::Unstaged);
        assert!(diff.contains("+こんにちは"), "{diff}");
    }

    #[test]
    fn repo_root_from_subdirectory_matches_toplevel() {
        let repo = temp_repo("subdir", "main");
        let sub = repo.0.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let top = inspect_worktree(cwd(&repo), DiffMode::Unstaged).unwrap();
        let nested = inspect_worktree(sub.to_str().unwrap(), DiffMode::Unstaged).unwrap();
        assert_eq!(nested.repo_root, top.repo_root);
        assert_eq!(nested.repo_root, canonical_or_raw(&repo.0));
    }

    #[test]
    fn untracked_directory_lists_its_files() {
        let repo = temp_repo("untracked", "main");
        std::fs::create_dir_all(repo.0.join("new-dir")).unwrap();
        std::fs::write(repo.0.join("new-dir/x.rs"), "fn x() {}\n").unwrap();
        let info = inspect_worktree(cwd(&repo), DiffMode::Unstaged).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["new-dir/x.rs"]);
        let diff = file_diff(cwd(&repo), "new-dir/x.rs", DiffMode::Unstaged);
        assert!(diff.contains("+fn x() {}"), "{diff}");
    }

    #[test]
    fn unmerged_conflict_file_is_listed() {
        let repo = temp_repo("unmerged", "main");
        git(&repo.0, &["checkout", "-q", "-b", "other"]);
        std::fs::write(repo.0.join("old.rs"), "fn other() {}\n").unwrap();
        git(&repo.0, &["commit", "-q", "-am", "other"]);
        git(&repo.0, &["checkout", "-q", "main"]);
        std::fs::write(repo.0.join("old.rs"), "fn mine() {}\n").unwrap();
        git(&repo.0, &["commit", "-q", "-am", "mine"]);
        git(&repo.0, &["merge", "-q", "other"]);
        for mode in [DiffMode::Unstaged, DiffMode::Staged] {
            let info = inspect_worktree(cwd(&repo), mode).unwrap();
            let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, vec!["old.rs"], "{mode:?}");
            assert_eq!(info.files[0].status, "U", "{mode:?}");
        }
    }

    #[test]
    fn vs_main_works_when_default_branch_is_master() {
        let repo = temp_repo("master", "master");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.0.join("old.rs"), "fn changed() {}\n").unwrap();
        git(&repo.0, &["commit", "-q", "-am", "change"]);
        let info = inspect_worktree(cwd(&repo), DiffMode::VsMain).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["old.rs"]);
        let diff = file_diff(cwd(&repo), "old.rs", DiffMode::VsMain);
        assert!(diff.contains("+fn changed() {}"), "{diff}");
    }

    #[test]
    fn vs_main_without_base_branch_says_so() {
        let repo = temp_repo("nobase", "trunk");
        let info = inspect_worktree(cwd(&repo), DiffMode::VsMain).unwrap();
        assert!(info.files.is_empty());
        let note = info.note.expect("a note explaining the empty list");
        assert!(note.contains("比較元ブランチ"), "{note}");
        // A repo that has a base has no note.
        let repo = temp_repo("withbase", "main");
        assert_eq!(
            inspect_worktree(cwd(&repo), DiffMode::VsMain).unwrap().note,
            None
        );
    }

    fn scratch_dir(name: &str) -> TempRepo {
        let repo = temp_repo(name, "main");
        let _ = std::fs::remove_dir_all(&repo.0);
        std::fs::create_dir_all(&repo.0).unwrap();
        repo
    }

    #[test]
    fn bare_repos_in_one_directory_stay_separate() {
        let root = scratch_dir("bare");
        let mut roots = Vec::new();
        for name in ["a", "b"] {
            let seed = temp_repo(&format!("seed-{name}"), "main");
            let bare = root.0.join(format!("{name}.git"));
            git(
                &root.0,
                &["clone", "-q", "--bare", cwd(&seed), bare.to_str().unwrap()],
            );
            let wt = root.0.join(format!("{name}-wt"));
            git(
                &bare,
                &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
            );
            let info = inspect_worktree(wt.to_str().unwrap(), DiffMode::Unstaged).unwrap();
            assert_eq!(info.repo_name, name);
            roots.push(info.repo_root);
        }
        assert_ne!(roots[0], roots[1]);
    }

    #[test]
    fn submodules_stay_separate_repos() {
        let sup = temp_repo("super", "main");
        let mut roots = Vec::new();
        for name in ["s1", "s2"] {
            let seed = temp_repo(&format!("sub-{name}"), "main");
            git(
                &sup.0,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    cwd(&seed),
                    name,
                ],
            );
            let info =
                inspect_worktree(sup.0.join(name).to_str().unwrap(), DiffMode::Unstaged).unwrap();
            assert_eq!(info.repo_name, name);
            roots.push(info.repo_root);
        }
        assert_ne!(roots[0], roots[1]);
    }

    #[test]
    fn diff_relative_config_does_not_change_paths() {
        let repo = temp_repo("relative", "main");
        git(&repo.0, &["config", "diff.relative", "true"]);
        std::fs::create_dir_all(repo.0.join("sub")).unwrap();
        std::fs::write(repo.0.join("sub/x.rs"), "fn x() {}\n").unwrap();
        git(&repo.0, &["add", "."]);
        git(&repo.0, &["commit", "-q", "-m", "sub"]);
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.0.join("old.rs"), "fn changed() {}\n").unwrap();
        std::fs::write(repo.0.join("sub/x.rs"), "fn y() {}\n").unwrap();
        let sub = repo.0.join("sub");
        let info = inspect_worktree(sub.to_str().unwrap(), DiffMode::Unstaged).unwrap();
        let got: Vec<(&str, usize)> = info
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.added))
            .collect();
        assert_eq!(got, vec![("old.rs", 1), ("sub/x.rs", 1)]);
        git(&repo.0, &["commit", "-q", "-am", "change"]);
        let info = inspect_worktree(sub.to_str().unwrap(), DiffMode::VsMain).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["old.rs", "sub/x.rs"]);
        let diff = file_diff(&info.worktree_root, "old.rs", DiffMode::VsMain);
        assert!(diff.contains("+fn changed() {}"), "{diff}");
    }

    #[test]
    fn untracked_symlink_shows_the_link_not_its_target() {
        let repo = temp_repo("symlink", "main");
        let outside = scratch_dir("symlink-target");
        let secret = outside.0.join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET\n").unwrap();
        std::os::unix::fs::symlink(&secret, repo.0.join("link")).unwrap();
        let diff = file_diff(cwd(&repo), "link", DiffMode::Unstaged);
        assert!(!diff.contains("TOP-SECRET"), "{diff}");
        assert!(diff.contains(&format!("+{}", secret.display())), "{diff}");
    }

    #[test]
    fn selected_path_is_matched_literally() {
        let repo = temp_repo("literal", "main");
        std::fs::write(repo.0.join("a[1].rs"), "one\n").unwrap();
        std::fs::write(repo.0.join("a1.rs"), "one\n").unwrap();
        git(&repo.0, &["add", "."]);
        git(&repo.0, &["commit", "-q", "-m", "files"]);
        std::fs::write(repo.0.join("a[1].rs"), "bracket\n").unwrap();
        std::fs::write(repo.0.join("a1.rs"), "plain\n").unwrap();
        git(&repo.0, &["add", "."]);
        for mode in [DiffMode::Staged, DiffMode::VsMain] {
            if mode == DiffMode::VsMain {
                git(&repo.0, &["checkout", "-q", "-b", "feature"]);
                git(&repo.0, &["commit", "-q", "-m", "change"]);
            }
            let diff = file_diff(cwd(&repo), "a[1].rs", mode);
            assert!(diff.contains("+bracket"), "{mode:?}: {diff}");
            assert!(!diff.contains("a1.rs"), "{mode:?}: {diff}");
        }
        std::fs::write(repo.0.join("a[1].rs"), "again\n").unwrap();
        std::fs::write(repo.0.join("a1.rs"), "again\n").unwrap();
        let diff = file_diff(cwd(&repo), "a[1].rs", DiffMode::Unstaged);
        assert!(diff.contains("+again"), "{diff}");
        assert!(!diff.contains("a1.rs"), "{diff}");
    }

    #[test]
    fn user_diff_config_does_not_reformat_the_body() {
        let repo = temp_repo("extdiff", "main");
        git(
            &repo.0,
            &["config", "diff.external", "sh -c 'echo EXTERNAL' --"],
        );
        git(&repo.0, &["config", "color.diff", "always"]);
        std::fs::write(repo.0.join("old.rs"), "fn b() {}\n").unwrap();
        let diff = file_diff(cwd(&repo), "old.rs", DiffMode::Unstaged);
        assert!(!diff.contains("EXTERNAL"), "{diff}");
        assert!(!diff.contains('\x1b'), "{diff:?}");
        assert!(diff.lines().any(|l| l == "+fn b() {}"), "{diff}");
    }

    #[test]
    fn edited_rename_diff_shows_the_edits_not_a_new_file() {
        let repo = temp_repo("rename-edit", "main");
        let body = "a\nb\nc\nd\ne\nf\ng\nh\n";
        std::fs::write(repo.0.join("old.rs"), body).unwrap();
        git(&repo.0, &["commit", "-q", "-am", "body"]);
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        git(&repo.0, &["mv", "old.rs", "new.rs"]);
        std::fs::write(repo.0.join("new.rs"), body.replace("d\n", "D\n")).unwrap();
        git(&repo.0, &["add", "."]);
        let check = |diff: &str| {
            assert!(diff.contains("rename from old.rs"), "{diff}");
            assert!(diff.contains("-d\n") && diff.contains("+D\n"), "{diff}");
            assert!(!diff.contains("+a\n"), "{diff}");
        };
        check(&file_diff(cwd(&repo), "new.rs", DiffMode::Staged));
        git(&repo.0, &["commit", "-q", "-m", "rename"]);
        check(&file_diff(cwd(&repo), "new.rs", DiffMode::VsMain));
    }

    #[test]
    fn untracked_preview_reads_a_bounded_prefix() {
        let repo = temp_repo("big-untracked", "main");
        let line = "x".repeat(4 * 1024 * 1024);
        std::fs::write(repo.0.join("huge.log"), format!("{line}\nsecond\n")).unwrap();
        let diff = file_diff(cwd(&repo), "huge.log", DiffMode::Unstaged);
        assert!(diff.len() <= 80 * 1024, "{} bytes", diff.len());
        assert!(diff.starts_with("--- /dev/null"), "{}", &diff[..80]);
    }

    // ── Every worktree of a repository, and merge-conflict prediction ────────

    /// A worktree of `repo` on a new `branch`, at `<repo>-wt/<name>` the way
    /// orchestration workers lay theirs out.
    pub(crate) fn add_worktree(repo: &TempRepo, name: &str, branch: &str) -> PathBuf {
        let path = PathBuf::from(format!("{}-wt/{name}", repo.0.display()));
        git(
            &repo.0,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        path
    }

    pub(crate) fn commit_file(dir: &Path, file: &str, body: &str) {
        std::fs::write(dir.join(file), body).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", file]);
    }

    #[test]
    fn worktree_list_skips_bare_and_prunable_entries() {
        let text = "worktree /r.git\nbare\n\n\
                    worktree /r-wt/a\nHEAD 1\nbranch refs/heads/a\n\n\
                    worktree /r-wt/gone\nHEAD 2\ndetached\nprunable gitdir file points to non-existent location\n\n\
                    worktree /r-wt/locked\nHEAD 3\nbranch refs/heads/l\nlocked\n";
        assert_eq!(parse_worktree_list(text), vec!["/r-wt/a", "/r-wt/locked"]);
    }

    #[test]
    fn every_worktree_of_the_repo_is_listed() {
        let repo = temp_repo("wtlist", "main");
        let wt_root = PathBuf::from(format!("{}-wt", repo.0.display()));
        let _cleanup = TempRepo(wt_root);
        let foo = add_worktree(&repo, "foo", "orch/foo");
        let gone = add_worktree(&repo, "gone", "orch/gone");
        std::fs::remove_dir_all(&gone).unwrap();
        let listed = list_worktrees(cwd(&repo));
        assert_eq!(
            listed,
            vec![canonical_or_raw(&repo.0), canonical_or_raw(&foo)],
            "the removed worktree is prunable and left out"
        );
        // Listed the same way from inside the other worktree.
        assert_eq!(list_worktrees(foo.to_str().unwrap()), listed);
        let info = inspect_worktree(foo.to_str().unwrap(), DiffMode::Unstaged).unwrap();
        assert_eq!(info.branch, "orch/foo");
        assert_eq!(info.worktree_root, listed[1]);
    }

    #[test]
    fn branch_conflicting_with_main_reports_the_paths() {
        let repo = temp_repo("merge-conflict", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let wt = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&wt, "old.rs", "fn theirs() {}\n");
        commit_file(&wt, "only-here.rs", "fn new() {}\n");
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        let mut predictions = MergePredictions::default();
        let root = canonical_or_raw(&wt);
        assert_eq!(
            predictions.predict(&root, "orch/foo"),
            Some(vec!["old.rs".to_owned()])
        );
        // The base worktree itself has nothing to predict.
        assert_eq!(predictions.predict(cwd(&repo), "main"), None);
    }

    #[test]
    fn clean_branch_is_reported_clean() {
        let repo = temp_repo("merge-clean", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let wt = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&wt, "new.rs", "fn new() {}\n");
        commit_file(&repo.0, "old.rs", "fn moved_on() {}\n");
        let mut predictions = MergePredictions::default();
        assert_eq!(
            predictions.predict(&canonical_or_raw(&wt), "orch/foo"),
            Some(Vec::new())
        );
    }

    #[test]
    fn prediction_is_cached_until_a_commit_moves() {
        let repo = temp_repo("merge-cache", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let wt = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&wt, "old.rs", "fn theirs() {}\n");
        let root = canonical_or_raw(&wt);
        let mut predictions = MergePredictions::default();
        let clean = predictions.predict(&root, "orch/foo");
        assert_eq!(clean, Some(Vec::new()));
        // Uncommitted edits move neither commit: still cached.
        std::fs::write(wt.join("old.rs"), "dirty\n").unwrap();
        assert_eq!(predictions.predict(&root, "orch/foo"), clean);
        assert_eq!(predictions.runs, 1);
        // The base moving (main commits a clashing change) recomputes it.
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        assert_eq!(
            predictions.predict(&root, "orch/foo"),
            Some(vec!["old.rs".to_owned()])
        );
        assert_eq!(predictions.runs, 2);
        assert_eq!(
            predictions.predict(&root, "orch/foo"),
            Some(vec!["old.rs".to_owned()])
        );
        assert_eq!(predictions.runs, 2);
        // So does HEAD moving.
        git(&wt, &["checkout", "-q", "--", "old.rs"]);
        commit_file(&wt, "more.rs", "x\n");
        predictions.predict(&root, "orch/foo");
        assert_eq!(predictions.runs, 3);
    }
}
