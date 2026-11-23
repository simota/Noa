//! Local git inspection for live worktrees discovered via `noa-ipc`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::{ChangedFile, CommitInfo, DiffMode};

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

/// `git -C <cwd>` that never writes the index: `git status` would otherwise
/// refresh and write it back under `index.lock` (the optional lock), and
/// porcelain `git diff` does the same unless `diff.autoRefreshIndex` is off,
/// in a worktree someone else may be committing in; this browser only reads.
fn git_command(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("--no-optional-locks")
        .args(["-c", "diff.autoRefreshIndex=false"])
        .arg("-C")
        .arg(cwd);
    cmd
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = git_command(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The most of a `git show` or `git diff` the diff view is given.
pub const DIFF_CAP: usize = 2 * 1024 * 1024;

/// [`run_git`] reading at most `cap` bytes of the output: a longer one is cut
/// at its last line break within `cap` and ends with a line saying so.
fn run_git_capped(cwd: &Path, args: &[&str], cap: usize) -> Option<String> {
    use std::io::Read;
    let mut child = git_command(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|out| out.take(cap as u64 + 1).read_to_end(&mut bytes));
    // The pipe is closed by now, so a longer output ends with SIGPIPE.
    let status = child.wait().ok()?;
    read?.ok()?;
    if bytes.len() > cap {
        return Some(truncate_at_line(bytes, cap));
    }
    status
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// The first `cap` bytes of `bytes` up to the last line break among them,
/// followed by a line saying the rest was left out.
fn truncate_at_line(mut bytes: Vec<u8>, cap: usize) -> String {
    let cut = bytes[..cap.min(bytes.len())]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(cap.min(bytes.len()), |i| i + 1);
    bytes.truncate(cut);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "(差分が {} MiB を超えたため、以降を省略しました)\n",
        cap / (1024 * 1024)
    ));
    text
}

fn canonical_or_raw(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
thread_local! {
    /// How many times this thread called [`inspect_worktree`], for tests.
    pub(crate) static INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn inspect_worktree(
    cwd: &str,
    mode: DiffMode,
    bases: &mut BaseRefs,
) -> Option<WorktreeGitInfo> {
    #[cfg(test)]
    INSPECTIONS.with(|n| n.set(n.get() + 1));
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return None;
    }
    let (toplevel, common_raw) = top_and_common_dir(dir)?;
    if toplevel.is_empty() {
        return None;
    }
    let common_raw = common_raw.unwrap_or_else(|| ".git".into());
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

    // Only "vs main" compares against the base.
    let range = (mode == DiffMode::VsMain)
        .then(|| bases.get(&repo_root, top))
        .flatten()
        .map(|base| format!("{}...HEAD", base.name));
    // "vs main" also needs each file's status letter, which `--raw` prints
    // ahead of the counts in the same git.
    let numstat_args: Vec<&str> = match mode {
        DiffMode::Unstaged => vec!["diff", "--numstat", "-z"],
        DiffMode::Staged => vec!["diff", "--cached", "--numstat", "-z"],
        DiffMode::VsMain => match &range {
            Some(range) => vec!["diff", "--raw", "--numstat", "-z", range.as_str()],
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
    let (statuses, mut stats) = parse_raw_numstat(&numstat_out);

    let mut files = Vec::new();
    match mode {
        DiffMode::VsMain => {
            for (path, (added, deleted)) in stats.drain() {
                let status = statuses.get(&path).map_or("M", String::as_str).to_owned();
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

/// `git rev-parse --show-toplevel` and `--git-common-dir` (`None` if that
/// one fails) of `dir`, trimmed; `None` outside a work tree.
fn top_and_common_dir(dir: &Path) -> Option<(String, Option<String>)> {
    top_and_common_dir_with(|args| run_git(dir, args))
}

/// [`top_and_common_dir`] running git through `git`. One git asks for both;
/// whenever its answer is not exactly an absolute top level and a common
/// dir, one per line (it failed, or a path holds a line break), the two are
/// asked one at a time, so the result is always what those two calls give.
fn top_and_common_dir_with(
    git: impl Fn(&[&str]) -> Option<String>,
) -> Option<(String, Option<String>)> {
    if let Some(both) = git(&["rev-parse", "--show-toplevel", "--git-common-dir"]) {
        let lines: Vec<&str> = both
            .strip_suffix('\n')
            .unwrap_or(&both)
            .split('\n')
            .collect();
        if let [top, common] = lines[..]
            && Path::new(top.trim()).is_absolute()
            && !common.trim().is_empty()
        {
            return Some((top.trim().to_owned(), Some(common.trim().to_owned())));
        }
    }
    let top = git(&["rev-parse", "--show-toplevel"])?.trim().to_owned();
    let common = git(&["rev-parse", "--git-common-dir"]).map(|s| s.trim().to_owned());
    Some((top, common))
}

/// `<base>...HEAD` for the "vs main" mode.
fn base_range(dir: &Path) -> Option<String> {
    Some(format!("{}...HEAD", resolve_base(dir)?.name))
}

/// The branch the "vs main" mode, the merge prediction and the commit list
/// compare against, with the commit it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRef {
    pub name: String,
    pub sha: String,
}

/// The base of each repository, resolved at most once: one is made per
/// refresh, so every worktree of a repository compares against the same
/// commit and git runs once for it, not once per worktree and use.
#[derive(Debug, Default)]
pub struct BaseRefs {
    by_repo: HashMap<String, Option<BaseRef>>,
    /// How many times a base was resolved with git, for tests.
    #[cfg(test)]
    pub(crate) resolutions: usize,
}

impl BaseRefs {
    /// The base of the repository at `repo_root`, resolved from `dir` (any
    /// of its worktrees) the first time it is asked for.
    pub fn get(&mut self, repo_root: &str, dir: &Path) -> Option<BaseRef> {
        if let Some(base) = self.by_repo.get(repo_root) {
            return base.clone();
        }
        #[cfg(test)]
        {
            self.resolutions += 1;
        }
        let base = resolve_base(dir);
        self.by_repo.insert(repo_root.to_owned(), base.clone());
        base
    }
}

/// The local `main`, else the local `master`, else whatever `origin/HEAD`
/// points at.
fn resolve_base(dir: &Path) -> Option<BaseRef> {
    let sha_of = |rev: &str| {
        run_git(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    };
    for name in ["main", "master"] {
        if let Some(sha) = sha_of(&format!("refs/heads/{name}")) {
            return Some(BaseRef {
                name: name.to_owned(),
                sha,
            });
        }
    }
    let name = run_git(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty())?;
    let sha = sha_of(&name)?;
    Some(BaseRef { name, sha })
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
    pub fn predict(
        &mut self,
        worktree_root: &str,
        branch: &str,
        base: Option<&BaseRef>,
    ) -> Option<Vec<String>> {
        let dir = Path::new(worktree_root);
        let base = base?;
        if branch == base.name {
            return None;
        }
        let head_sha = run_git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())?;
        let base_sha = base.sha.clone();
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

    /// How many times `git merge-tree` ran.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> usize {
        self.runs
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

/// At most this many commits are listed per worktree.
pub const COMMIT_CAP: usize = 50;

/// The commits on HEAD that the base branch lacks, newest first, capped at
/// [`COMMIT_CAP`]. Empty when HEAD is on the base.
pub fn branch_commits(dir: &Path, base: &str) -> Vec<CommitInfo> {
    let range = format!("{base}..HEAD");
    // `\x1e` opens each record, `\x1f` separates its fields; the file names
    // `--name-only` prints after the header line are only counted.
    let limit = COMMIT_CAP.to_string();
    let out = run_git(
        dir,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "--no-color",
            "-n",
            &limit,
            "--format=%x1e%H%x1f%h%x1f%an%x1f%ct%x1f%s",
            "--name-only",
            &range,
        ],
    )
    .unwrap_or_default();
    parse_commit_log(&out)
}

fn parse_commit_log(text: &str) -> Vec<CommitInfo> {
    text.split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let mut fields = lines.next()?.splitn(5, '\x1f');
            let (sha, short, author, time, subject) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            Some(CommitInfo {
                sha: sha.to_owned(),
                short: short.to_owned(),
                subject: subject.to_owned(),
                author: author.to_owned(),
                time: time.parse().unwrap_or(0),
                files: lines.filter(|l| !l.is_empty()).count(),
                diff: String::new(),
            })
        })
        .collect()
}

/// Per-worktree memo of [`branch_commits`], so the periodic refresh re-reads
/// the log only when HEAD or the base branch moved.
#[derive(Default)]
pub struct CommitCache {
    /// worktree root -> ("<HEAD sha> <base sha>", its commits)
    entries: std::collections::HashMap<String, (String, Vec<CommitInfo>)>,
    /// How many times the log was actually read.
    #[cfg(test)]
    reads: usize,
}

impl CommitCache {
    /// The commits of `worktree_root` against `base` (none without one).
    pub fn commits(&mut self, worktree_root: &str, base: Option<&BaseRef>) -> Vec<CommitInfo> {
        let dir = Path::new(worktree_root);
        let head = run_git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let Some((base, key)) = base
            .zip(head)
            .map(|(base, head)| (base, format!("{head} {}", base.sha)))
        else {
            self.entries.remove(worktree_root);
            return Vec::new();
        };
        if let Some((cached, commits)) = self.entries.get(worktree_root)
            && *cached == key
        {
            return commits.clone();
        }
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let commits = branch_commits(dir, &base.name);
        self.entries
            .insert(worktree_root.to_owned(), (key, commits.clone()));
        commits
    }
}

/// `git show` of one commit (header, message and patch), for the diff view.
pub fn commit_diff(cwd: &str, sha: &str) -> String {
    // The sha comes from our own listing; refuse anything else so it can
    // never be read as an option.
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return format!("(不正なコミット指定: {sha})");
    }
    let args = [
        "-c",
        "log.showSignature=false",
        "show",
        "--format=fuller",
        "--patch",
        // `diff.noprefix` / `diff.mnemonicPrefix` would change the
        // `diff --git a/… b/…` lines the editor reads the file from.
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--no-ext-diff",
        "--no-color",
        sha,
        "--",
    ];
    match run_git_capped(Path::new(cwd), &args, DIFF_CAP) {
        Some(out) if !out.trim().is_empty() => out,
        _ => format!("(コミットを読めません: {sha})"),
    }
}

pub fn file_diff(cwd: &str, path: &str, mode: DiffMode) -> String {
    let dir = Path::new(cwd);
    let range = (mode == DiffMode::VsMain)
        .then(|| base_range(dir))
        .flatten();
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
    let out = run_git_capped(dir, &args, DIFF_CAP).unwrap_or_default();
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

/// Parses `git diff [--raw] --numstat -z`: each file's status letter (`A`,
/// `D`, `M`, `R`, …) from the `--raw` records, which come first, and its
/// added and deleted line counts from the numstat ones, both by new path.
fn parse_raw_numstat(text: &str) -> (HashMap<String, String>, HashMap<String, (usize, usize)>) {
    let mut statuses = HashMap::new();
    let mut entries = text.split('\0').peekable();
    // `:<modes> <oids> <status>` then the path, or the old and the new path
    // of a rename or copy, each its own entry.
    while let Some(raw) = entries.next_if(|entry| entry.starts_with(':')) {
        let Some(letter) = raw.rsplit(' ').next().and_then(|code| code.chars().next()) else {
            continue;
        };
        if matches!(letter, 'R' | 'C') {
            entries.next();
        }
        if let Some(path) = entries.next() {
            statuses.insert(path.to_owned(), letter.to_string());
        }
    }
    (statuses, parse_numstat(entries))
}

/// Parses `git diff --numstat -z` entries. A rename is `added\tdeleted\t`
/// followed by the old and new paths as separate entries; it is keyed by
/// the new path.
fn parse_numstat<'a>(
    mut entries: impl Iterator<Item = &'a str>,
) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
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

    #[test]
    fn raw_records_give_the_status_and_numstat_the_counts() {
        let sample = ":100644 100644 422c2b7 7be73ce M\0a\0\
:000000 100644 0000000 8ba3a16 A\0:colon\0\
:100644 100644 587be6b 587be6b R100\0old\0new\0\
:100644 100644 587be6b 587be6b C075\0src\0copy\0\
2\t1\ta\0-\t-\t:colon\00\t0\t\0old\0new\03\t0\t\0src\0copy\0";
        let (statuses, stats) = parse_raw_numstat(sample);
        let mut statuses: Vec<_> = statuses.into_iter().collect();
        statuses.sort();
        let s = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            statuses,
            vec![s(":colon", "A"), s("a", "M"), s("copy", "C"), s("new", "R")]
        );
        let mut stats: Vec<_> = stats.into_iter().collect();
        stats.sort();
        let n = |p: &str, a, d| (p.to_owned(), (a, d));
        assert_eq!(
            stats,
            vec![
                n(":colon", 0, 0),
                n("a", 2, 1),
                n("copy", 3, 0),
                n("new", 0, 0)
            ]
        );
        // Plain numstat has no raw records.
        let (statuses, stats) = parse_raw_numstat("1\t2\tx\0");
        assert!(statuses.is_empty());
        assert_eq!(stats.get("x"), Some(&(1, 2)));
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

    /// The base a refresh would resolve for `dir` right now.
    fn base_of(dir: impl AsRef<Path>) -> Option<BaseRef> {
        resolve_base(dir.as_ref())
    }

    /// [`branch_commits`] against the current base; none without one.
    fn commits_of(dir: &Path) -> Vec<CommitInfo> {
        base_of(dir).map_or_else(Vec::new, |base| branch_commits(dir, &base.name))
    }

    #[test]
    fn the_base_is_resolved_once_per_repository_and_only_for_vs_main() {
        let repo = temp_repo("base-once", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let foo = add_worktree(&repo, "foo", "orch/foo");
        let mut bases = BaseRefs::default();
        for mode in [DiffMode::Unstaged, DiffMode::Staged] {
            inspect_worktree(cwd(&repo), mode, &mut bases).unwrap();
            inspect_worktree(foo.to_str().unwrap(), mode, &mut bases).unwrap();
        }
        assert_eq!(bases.resolutions, 0, "a working-tree mode needs no base");
        let main = inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut bases).unwrap();
        let other = inspect_worktree(foo.to_str().unwrap(), DiffMode::VsMain, &mut bases).unwrap();
        assert_eq!(main.repo_root, other.repo_root);
        assert_eq!(bases.resolutions, 1);
        let base = bases.get(&main.repo_root, &foo);
        assert_eq!(base.as_ref().map(|b| b.name.as_str()), Some("main"));
        assert_eq!(bases.resolutions, 1);
    }

    #[test]
    fn reading_never_writes_the_index() {
        let repo = temp_repo("no-locks", "main");
        let index = repo.0.join(".git/index");
        // Same content, another mtime: `git status` refreshes the stat data
        // and writes the index back unless optional locks are off.
        std::fs::write(repo.0.join("old.rs"), "fn a() {}\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(repo.0.join("old.rs"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        let before = std::fs::read(&index).unwrap();
        let status = [
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ];
        assert!(run_git(&repo.0, &status).is_some());
        assert_eq!(std::fs::read(&index).unwrap(), before, "index rewritten");
        // Porcelain `git diff` (numstat, the file diff) leaves it alone too.
        assert!(run_git(&repo.0, &["diff", "--numstat", "-z"]).is_some());
        assert!(run_git(&repo.0, &["diff", "--", "old.rs"]).is_some());
        assert_eq!(
            std::fs::read(&index).unwrap(),
            before,
            "index rewritten by diff"
        );
        // The same status without the flag does write it.
        git(&repo.0, &status);
        assert_ne!(std::fs::read(&index).unwrap(), before);
    }

    #[test]
    fn long_output_is_cut_at_a_line_break_with_a_note() {
        let text = truncate_at_line(b"one\ntwo\nthree\n".to_vec(), 9);
        assert_eq!(
            text,
            "one\ntwo\n(差分が 0 MiB を超えたため、以降を省略しました)\n"
        );
        // A first line longer than the cap is cut inside it.
        let text = truncate_at_line(b"abcdef".to_vec(), 3);
        assert!(text.starts_with("abc\n("), "{text}");
    }

    #[test]
    fn huge_commit_and_file_diffs_are_capped() {
        let repo = temp_repo("huge-diff", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        let body: String = (0..400_000).map(|i| format!("line {i:08}\n")).collect();
        assert!(body.len() > DIFF_CAP + DIFF_CAP / 2);
        commit_file(&repo.0, "big.txt", &body);
        let check = |diff: &str| {
            assert!(diff.len() <= DIFF_CAP + 200, "{} bytes", diff.len());
            let mut lines = diff.lines().rev();
            assert!(
                lines.next().unwrap().contains("2 MiB を超えた"),
                "{}",
                &diff[diff.len() - 200..]
            );
            let last = lines.next().unwrap();
            assert!(
                last.len() == "+line 00000000".len() && last.starts_with("+line "),
                "{last}"
            );
        };
        let sha = commits_of(&repo.0)[0].sha.clone();
        check(&commit_diff(cwd(&repo), &sha));
        check(&file_diff(cwd(&repo), "big.txt", DiffMode::VsMain));
        // A diff under the cap is passed through whole.
        std::fs::write(
            repo.0.join("big.txt"),
            body.replace("line 00000003", "changed"),
        )
        .unwrap();
        let small = file_diff(cwd(&repo), "big.txt", DiffMode::Unstaged);
        assert!(
            small.contains("+changed") && !small.contains("MiB"),
            "{small}"
        );
    }

    #[test]
    fn staged_rename_uses_new_path_and_shows_diff() {
        let repo = temp_repo("rename", "main");
        git(&repo.0, &["mv", "old.rs", "new.rs"]);
        let info =
            inspect_worktree(cwd(&repo), DiffMode::Staged, &mut BaseRefs::default()).unwrap();
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
        let info =
            inspect_worktree(cwd(&repo), DiffMode::Unstaged, &mut BaseRefs::default()).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["old.rs", "日本語.txt"]);
        let diff = file_diff(cwd(&repo), "日本語.txt", DiffMode::Unstaged);
        assert!(diff.contains("+こんにちは"), "{diff}");
    }

    #[test]
    fn a_directory_outside_any_repository_is_not_a_worktree() {
        let dir = scratch_dir("not-a-repo");
        let mut bases = BaseRefs::default();
        assert!(inspect_worktree(cwd(&dir), DiffMode::Unstaged, &mut bases).is_none());
        let gone = dir.0.join("missing");
        assert!(inspect_worktree(gone.to_str().unwrap(), DiffMode::Unstaged, &mut bases).is_none());
        // A bare repository has no work tree to inspect.
        let seed = temp_repo("bare-seed", "main");
        let bare = dir.0.join("r.git");
        git(
            &dir.0,
            &["clone", "-q", "--bare", cwd(&seed), bare.to_str().unwrap()],
        );
        assert!(inspect_worktree(bare.to_str().unwrap(), DiffMode::Unstaged, &mut bases).is_none());
    }

    #[test]
    fn worktree_info_names_the_repository_and_canonical_roots() {
        let repo = temp_repo("names", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let foo = add_worktree(&repo, "foo", "orch/foo");
        std::fs::create_dir_all(foo.join("a b")).unwrap();
        let info = inspect_worktree(
            foo.join("a b").to_str().unwrap(),
            DiffMode::Unstaged,
            &mut BaseRefs::default(),
        )
        .unwrap();
        assert_eq!(info.repo_root, canonical_or_raw(&repo.0));
        assert_eq!(
            info.repo_name,
            repo.0.file_name().unwrap().to_string_lossy()
        );
        assert_eq!(info.worktree_root, canonical_or_raw(&foo));
        assert_eq!(info.branch, "orch/foo");
    }

    #[test]
    fn top_and_common_dir_falls_back_to_one_call_each() {
        use std::cell::RefCell;
        const BOTH: &str = "rev-parse --show-toplevel --git-common-dir";
        const TOP: &str = "rev-parse --show-toplevel";
        const COMMON: &str = "rev-parse --git-common-dir";
        // A fake git answering each argument list from `answers` (a missing
        // one fails), recording what was asked.
        let run = |answers: &[(&str, &str)]| {
            let asked = RefCell::new(Vec::new());
            let result = top_and_common_dir_with(|args| {
                let args = args.join(" ");
                let answer = answers
                    .iter()
                    .find(|(a, _)| *a == args)
                    .map(|(_, out)| (*out).to_owned());
                asked.borrow_mut().push(args);
                answer
            });
            (result, asked.into_inner())
        };
        let some =
            |top: &str, common: Option<&str>| Some((top.to_owned(), common.map(str::to_owned)));

        // The combined call answers: nothing else runs.
        let (result, asked) = run(&[(BOTH, "/w\n/r/.git\n")]);
        assert_eq!(result, some("/w", Some("/r/.git")));
        assert_eq!(asked, [BOTH]);
        // It fails: the two calls decide, each failure as main treats it.
        let (result, asked) = run(&[(TOP, "/w\n"), (COMMON, ".git\n")]);
        assert_eq!(result, some("/w", Some(".git")));
        assert_eq!(asked, [BOTH, TOP, COMMON]);
        assert_eq!(run(&[(TOP, "/w\n")]).0, some("/w", None));
        assert_eq!(run(&[(COMMON, ".git\n")]).0, None);
        // Unexpected answers (not two lines, no absolute top level, an empty
        // common dir) are asked again one at a time.
        for odd in ["/a\nb\n/r/.git\n", "x\n/r/.git\n", "/w\n\n", "/w\n"] {
            let (result, asked) = run(&[(BOTH, odd), (TOP, "/w\n"), (COMMON, "/r/.git\n")]);
            assert_eq!(result, some("/w", Some("/r/.git")), "{odd:?}");
            assert_eq!(asked, [BOTH, TOP, COMMON], "{odd:?}");
        }
    }

    #[test]
    fn a_worktree_whose_path_holds_a_line_break_is_inspected() {
        let root = scratch_dir("newline");
        let seed = temp_repo("newline-seed", "main");
        let odd = root.0.join("a\nb ");
        git(&root.0, &["clone", "-q", cwd(&seed), odd.to_str().unwrap()]);
        let info = inspect_worktree(
            odd.to_str().unwrap(),
            DiffMode::Unstaged,
            &mut BaseRefs::default(),
        )
        .unwrap();
        // `git rev-parse` output is trimmed, the top level's trailing space
        // with it, so the status cannot run there and lists nothing.
        assert_eq!(
            info.worktree_root,
            format!("{}/a\nb", canonical_or_raw(&root.0))
        );
        assert_eq!(info.repo_name, "a\nb ");
        assert_eq!(info.repo_root, canonical_or_raw(&odd));
        assert_eq!(info.branch, "main");
        assert!(info.files.is_empty());
    }

    #[test]
    fn repo_root_from_subdirectory_matches_toplevel() {
        let repo = temp_repo("subdir", "main");
        let sub = repo.0.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let top =
            inspect_worktree(cwd(&repo), DiffMode::Unstaged, &mut BaseRefs::default()).unwrap();
        let nested = inspect_worktree(
            sub.to_str().unwrap(),
            DiffMode::Unstaged,
            &mut BaseRefs::default(),
        )
        .unwrap();
        assert_eq!(nested.repo_root, top.repo_root);
        assert_eq!(nested.repo_root, canonical_or_raw(&repo.0));
    }

    #[test]
    fn untracked_directory_lists_its_files() {
        let repo = temp_repo("untracked", "main");
        std::fs::create_dir_all(repo.0.join("new-dir")).unwrap();
        std::fs::write(repo.0.join("new-dir/x.rs"), "fn x() {}\n").unwrap();
        let info =
            inspect_worktree(cwd(&repo), DiffMode::Unstaged, &mut BaseRefs::default()).unwrap();
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
            let info = inspect_worktree(cwd(&repo), mode, &mut BaseRefs::default()).unwrap();
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
        let info =
            inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut BaseRefs::default()).unwrap();
        let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["old.rs"]);
        let diff = file_diff(cwd(&repo), "old.rs", DiffMode::VsMain);
        assert!(diff.contains("+fn changed() {}"), "{diff}");
    }

    #[test]
    fn vs_main_without_base_branch_says_so() {
        let repo = temp_repo("nobase", "trunk");
        let info =
            inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut BaseRefs::default()).unwrap();
        assert!(info.files.is_empty());
        let note = info.note.expect("a note explaining the empty list");
        assert!(note.contains("比較元ブランチ"), "{note}");
        // A repo that has a base has no note.
        let repo = temp_repo("withbase", "main");
        assert_eq!(
            inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut BaseRefs::default())
                .unwrap()
                .note,
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
            let info = inspect_worktree(
                wt.to_str().unwrap(),
                DiffMode::Unstaged,
                &mut BaseRefs::default(),
            )
            .unwrap();
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
            let info = inspect_worktree(
                sup.0.join(name).to_str().unwrap(),
                DiffMode::Unstaged,
                &mut BaseRefs::default(),
            )
            .unwrap();
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
        let info = inspect_worktree(
            sub.to_str().unwrap(),
            DiffMode::Unstaged,
            &mut BaseRefs::default(),
        )
        .unwrap();
        let got: Vec<(&str, usize)> = info
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.added))
            .collect();
        assert_eq!(got, vec![("old.rs", 1), ("sub/x.rs", 1)]);
        git(&repo.0, &["commit", "-q", "-am", "change"]);
        let info = inspect_worktree(
            sub.to_str().unwrap(),
            DiffMode::VsMain,
            &mut BaseRefs::default(),
        )
        .unwrap();
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
        let info = inspect_worktree(
            foo.to_str().unwrap(),
            DiffMode::Unstaged,
            &mut BaseRefs::default(),
        )
        .unwrap();
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
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            Some(vec!["old.rs".to_owned()])
        );
        // The base worktree itself has nothing to predict.
        assert_eq!(
            predictions.predict(cwd(&repo), "main", base_of(cwd(&repo)).as_ref()),
            None
        );
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
            predictions.predict(
                &canonical_or_raw(&wt),
                "orch/foo",
                base_of(canonical_or_raw(&wt)).as_ref()
            ),
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
        let clean = predictions.predict(&root, "orch/foo", base_of(&root).as_ref());
        assert_eq!(clean, Some(Vec::new()));
        // Uncommitted edits move neither commit: still cached.
        std::fs::write(wt.join("old.rs"), "dirty\n").unwrap();
        assert_eq!(
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            clean
        );
        assert_eq!(predictions.runs, 1);
        // The base moving (main commits a clashing change) recomputes it.
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        assert_eq!(
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            Some(vec!["old.rs".to_owned()])
        );
        assert_eq!(predictions.runs, 2);
        assert_eq!(
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            Some(vec!["old.rs".to_owned()])
        );
        assert_eq!(predictions.runs, 2);
        // So does HEAD moving.
        git(&wt, &["checkout", "-q", "--", "old.rs"]);
        commit_file(&wt, "more.rs", "x\n");
        predictions.predict(&root, "orch/foo", base_of(&root).as_ref());
        assert_eq!(predictions.runs, 3);
    }

    #[test]
    fn an_unreadable_head_commit_is_not_predicted_or_cached() {
        let repo = temp_repo("merge-missing", "main");
        let _cleanup = TempRepo(PathBuf::from(format!("{}-wt", repo.0.display())));
        let wt = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&wt, "old.rs", "fn theirs() {}\n");
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        let root = canonical_or_raw(&wt);
        let out = Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        let object = repo.0.join(".git/objects").join(&sha[..2]).join(&sha[2..]);
        let aside = object.with_extension("aside");
        std::fs::rename(&object, &aside).unwrap();
        let mut predictions = MergePredictions::default();
        assert_eq!(
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            None
        );
        // Once the object is back, the conflict shows: nothing was cached.
        std::fs::rename(&aside, &object).unwrap();
        assert_eq!(
            predictions.predict(&root, "orch/foo", base_of(&root).as_ref()),
            Some(vec!["old.rs".to_owned()])
        );
    }

    #[test]
    fn vs_main_marks_files_the_branch_deleted_or_added() {
        let repo = temp_repo("vs-main-status", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        std::fs::remove_file(repo.0.join("old.rs")).unwrap();
        std::fs::write(repo.0.join("new.rs"), "n\n").unwrap();
        git(&repo.0, &["add", "-A"]);
        git(&repo.0, &["commit", "-q", "-m", "swap"]);
        let info =
            inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut BaseRefs::default()).unwrap();
        let status = |p: &str| {
            info.files
                .iter()
                .find(|f| f.path == p)
                .map(|f| f.status.clone())
        };
        assert_eq!(status("old.rs").as_deref(), Some("D"), "{:?}", info.files);
        assert_eq!(status("new.rs").as_deref(), Some("A"), "{:?}", info.files);
    }

    #[test]
    fn vs_main_reports_every_kind_of_change() {
        let repo = temp_repo("vs-main-kinds", "main");
        let lines: String = (0..20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.0.join("moved.rs"), &lines).unwrap();
        std::fs::write(repo.0.join("gone.rs"), "g\n").unwrap();
        std::fs::write(repo.0.join("bin.dat"), b"\x00\x01\x02").unwrap();
        git(&repo.0, &["add", "."]);
        git(&repo.0, &["commit", "-q", "-m", "base"]);
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        git(&repo.0, &["mv", "moved.rs", ":renamed to.rs"]);
        std::fs::write(repo.0.join(":renamed to.rs"), lines.replace("line 3\n", "")).unwrap();
        std::fs::remove_file(repo.0.join("gone.rs")).unwrap();
        std::fs::write(repo.0.join("bin.dat"), b"\x00\x03").unwrap();
        std::fs::write(repo.0.join("old.rs"), "fn b() {}\nfn c() {}\n").unwrap();
        std::fs::write(repo.0.join("日本語 new.txt"), "a\nb\n").unwrap();
        git(&repo.0, &["add", "-A"]);
        git(&repo.0, &["commit", "-q", "-m", "change"]);
        let info =
            inspect_worktree(cwd(&repo), DiffMode::VsMain, &mut BaseRefs::default()).unwrap();
        let got: Vec<String> = info
            .files
            .iter()
            .map(|f| format!("{} {} +{}-{}", f.status, f.path, f.added, f.deleted))
            .collect();
        assert_eq!(
            got,
            vec![
                "R :renamed to.rs +0-1",
                "M bin.dat +0-0",
                "D gone.rs +0-1",
                "M old.rs +2-1",
                "A 日本語 new.txt +2-0",
            ]
        );
    }

    #[test]
    fn commit_diff_names_files_with_a_b_prefixes_whatever_the_config() {
        let repo = temp_repo("noprefix", "main");
        git(&repo.0, &["config", "diff.noprefix", "true"]);
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        commit_msg_file(&repo, "x.rs", "x\n", "add x");
        let sha = run_git(&repo.0, &["rev-parse", "HEAD"]).unwrap();
        let show = commit_diff(cwd(&repo), sha.trim());
        assert!(
            show.lines().any(|l| l == "diff --git a/x.rs b/x.rs"),
            "{show}"
        );
    }

    fn commit_msg_file(repo: &TempRepo, name: &str, body: &str, msg: &str) {
        std::fs::write(repo.0.join(name), body).unwrap();
        git(&repo.0, &["add", "."]);
        git(&repo.0, &["commit", "-q", "-m", msg]);
    }

    #[test]
    fn branch_commits_lists_newest_first_with_file_counts() {
        let repo = temp_repo("commits", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        commit_msg_file(&repo, "a.rs", "a\n", "first");
        std::fs::write(repo.0.join("b.rs"), "b\n").unwrap();
        std::fs::write(repo.0.join("old.rs"), "fn changed() {}\n").unwrap();
        git(&repo.0, &["add", "."]);
        git(&repo.0, &["commit", "-q", "-m", "second: two files"]);
        let commits = commits_of(&repo.0);
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["second: two files", "first"]);
        assert_eq!(commits[0].files, 2);
        assert_eq!(commits[1].files, 1);
        assert_eq!(commits[0].author, "t");
        assert!(commits[0].sha.starts_with(&commits[0].short));
        assert!(commits[0].time >= commits[1].time && commits[1].time > 0);
    }

    #[test]
    fn branch_commits_is_empty_on_the_base_or_without_one() {
        let repo = temp_repo("on-base", "main");
        commit_msg_file(&repo, "a.rs", "a\n", "on main");
        assert!(commits_of(&repo.0).is_empty());
        let repo = temp_repo("no-base", "trunk");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        commit_msg_file(&repo, "a.rs", "a\n", "no base to compare");
        assert!(commits_of(&repo.0).is_empty());
    }

    #[test]
    fn branch_commits_is_capped() {
        let repo = temp_repo("cap", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        for i in 0..COMMIT_CAP + 2 {
            git(
                &repo.0,
                &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")],
            );
        }
        let commits = commits_of(&repo.0);
        assert_eq!(commits.len(), COMMIT_CAP);
        assert_eq!(commits[0].subject, format!("c{}", COMMIT_CAP + 1));
        assert_eq!(commits[0].files, 0);
    }

    #[test]
    fn commit_cache_rereads_only_when_head_or_base_moves() {
        let repo = temp_repo("cache", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        commit_msg_file(&repo, "a.rs", "a\n", "one");
        let root = cwd(&repo);
        let mut cache = CommitCache::default();
        assert_eq!(cache.commits(root, base_of(root).as_ref()).len(), 1);
        assert_eq!(cache.commits(root, base_of(root).as_ref()).len(), 1);
        assert_eq!(cache.reads, 1);
        // An unstaged edit moves neither HEAD nor the base.
        std::fs::write(repo.0.join("a.rs"), "changed\n").unwrap();
        assert_eq!(cache.commits(root, base_of(root).as_ref()).len(), 1);
        assert_eq!(cache.reads, 1);
        git(&repo.0, &["commit", "-q", "-am", "two"]);
        assert_eq!(cache.commits(root, base_of(root).as_ref()).len(), 2);
        assert_eq!(cache.reads, 2);
        // The base moving (here: main gains a commit) changes the list too.
        git(&repo.0, &["checkout", "-q", "main"]);
        commit_msg_file(&repo, "m.rs", "m\n", "main moves");
        git(&repo.0, &["checkout", "-q", "feature"]);
        assert_eq!(cache.commits(root, base_of(root).as_ref()).len(), 2);
        assert_eq!(cache.reads, 3);
    }

    #[test]
    fn commit_diff_shows_the_fuller_header_and_patch() {
        let repo = temp_repo("show", "main");
        git(&repo.0, &["checkout", "-q", "-b", "feature"]);
        commit_msg_file(&repo, "a.rs", "fn shown() {}\n", "add shown");
        let commits = commits_of(&repo.0);
        let diff = commit_diff(cwd(&repo), &commits[0].sha);
        assert!(diff.contains("AuthorDate:"), "{diff}");
        assert!(diff.contains("add shown"), "{diff}");
        assert!(diff.lines().any(|l| l == "+fn shown() {}"), "{diff}");
        assert!(!diff.contains('\x1b'), "{diff:?}");
        assert!(commit_diff(cwd(&repo), "--output=x").starts_with("(不正なコミット指定"));
        assert!(commit_diff(cwd(&repo), "deadbeef").starts_with("(コミットを読めません"));
    }
}
