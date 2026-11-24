//! One detached worktree per verifier (L1-3): made from the rebuild recipe
//! when round 1 starts, checked against the manifest, removed at close
//! only when it is still the worktree the log recorded. Naming and the
//! porcelain parse are pure; the rest runs `git`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::capture::{self, MAX_WORKTREE_PATCH_BYTES, UNTRACKED_DIR, WORKTREE_PATCH};
use crate::fsx;
use crate::model::types::{FileStatus, PartId, Rebuild, SessionId, TargetManifest};

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// The checked-out commit; `None` for a bare entry.
    pub head: Option<String>,
    /// `refs/heads/…` when a branch is checked out.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// The repository's main worktree, as seen from `repo` (any directory of
/// any of its worktrees): the first entry of `git worktree list
/// --porcelain`, as `main_worktree` in `crates/noa-app/src/ctl/worktree.rs`
/// picks it. From a linked worktree `/src/app-wt/feature` of `/src/app`,
/// it is `/src/app`.
pub fn main_worktree(repo: &Path) -> Result<PathBuf, String> {
    list(repo)?
        .into_iter()
        .next()
        .map(|entry| entry.path)
        .ok_or_else(|| "git worktree list が worktree を返しませんでした".to_owned())
}

/// Where verifier `part`'s worktree goes: `<main>-wt/verify-<id>-<part>`
/// (`main` from [`main_worktree`]), beside the worktrees `Noa +ctl spawn
/// --worktree` makes.
pub fn path_for(main: &Path, id: &SessionId, part: &PartId) -> PathBuf {
    let mut dir: OsString = main.as_os_str().to_owned();
    dir.push("-wt");
    PathBuf::from(dir).join(format!("verify-{id}-{part}"))
}

/// Git run in `dir` with its output in the C locale, no prompt, and no
/// hook: a checkout into a verifier's worktree must not run the
/// repository's `post-checkout` hook.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Runs `cmd` with `input` on its stdin; its stdout, or why it failed.
fn run(mut cmd: Command, input: &[u8]) -> Result<Vec<u8>, String> {
    cmd.stdin(if input.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    let writer = child.stdin.take().map(|mut stdin| {
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input))
    });
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        text
    });
    let mut out = Vec::new();
    let read = child.stdout.take().expect("piped").read_to_end(&mut out);
    let status = child.wait().map_err(|e| e.to_string())?;
    let errors = errors.join().unwrap_or_default();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    read.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(out)
    } else {
        Err(String::from_utf8_lossy(&errors).trim().to_owned())
    }
}

/// `path` with its existing part resolved (`/var` → `/private/var` on
/// macOS), so a path the log records compares with the one git prints.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolved(parent).join(name),
        _ => path.to_owned(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || resolved(a) == resolved(b)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// `repo`'s entry for `path`, if git lists one.
fn entry_at(repo: &Path, path: &Path) -> Result<Option<Entry>, String> {
    Ok(list(repo)?
        .into_iter()
        .find(|entry| same_path(&entry.path, path)))
}

/// A relative path from the manifest or the recipe, as components that
/// stay inside the worktree.
fn relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Makes a detached worktree of `repo` at `path` on `rebuild.head_sha`
/// (`git worktree add --detach`), applies `target/worktree.patch` and
/// copies `target/untracked/` from the snapshot `target_dir`, then checks
/// every manifest file and untracked file against its digest. A mismatch
/// is an error naming the files. No branch is created.
///
/// The snapshot's files are read (and checked against the recipe's
/// digests) before anything is made. A worktree this call made and could
/// not complete is removed again, so a later call can build it afresh;
/// one registered at `path` whose folder is gone (deleted by hand) is
/// rebuilt in its place.
pub fn add(
    repo: &Path,
    path: &Path,
    target_dir: &Path,
    manifest: &TargetManifest,
    rebuild: &Rebuild,
) -> Result<(), String> {
    let read = |rel: &Path, cap: u64| -> Result<Vec<u8>, String> {
        let file = fsx::open_read(target_dir, &target_dir.join(rel))
            .map_err(|e| format!("{} を読めません: {e}", rel.display()))?;
        let (bytes, over) = fsx::read_capped(file, cap)
            .map_err(|e| format!("{} を読めません: {e}", rel.display()))?;
        if over {
            return Err(format!("{} が大きすぎます", rel.display()));
        }
        Ok(bytes)
    };
    let patch = match rebuild.worktree_patch {
        Some(want) => {
            let bytes = read(Path::new(WORKTREE_PATCH), MAX_WORKTREE_PATCH_BYTES)?;
            if capture::digest_bytes(&bytes) != want {
                return Err(format!(
                    "スナップショットの {WORKTREE_PATCH} が記録と一致しません"
                ));
            }
            bytes
        }
        None => Vec::new(),
    };
    let mut untracked = Vec::new();
    let mut tampered = Vec::new();
    for (file, want) in &rebuild.untracked {
        let rel =
            relative(file).ok_or_else(|| format!("未追跡のファイル {file:?} のパスが不正です"))?;
        let bytes = read(
            &Path::new(UNTRACKED_DIR).join(&rel),
            capture::MAX_UNTRACKED_BINARY_BYTES,
        )?;
        if capture::digest_bytes(&bytes) != *want {
            tampered.push(file.as_str());
        }
        untracked.push((rel, bytes));
    }
    if !tampered.is_empty() {
        return Err(format!(
            "スナップショットの未追跡ファイルが記録と一致しません: {}",
            tampered.join(", ")
        ));
    }

    let stale = !path.exists() && entry_at(repo, path)?.is_some();
    let mut cmd = git(repo);
    cmd.args(["worktree", "add", "--detach", "--quiet"]);
    if stale {
        // Only overrides "missing but already registered" for this path:
        // with `--detach` no branch is involved.
        cmd.arg("--force");
    }
    cmd.arg(path).arg(&rebuild.head_sha);
    run(cmd, b"").map_err(|e| format!("worktree を作れません: {e}"))?;

    let built = fill(path, &patch, &untracked).and_then(|()| check_files(path, manifest, rebuild));
    if let Err(why) = built {
        let mut cmd = git(repo);
        cmd.args(["worktree", "remove", "--force"]).arg(path);
        return Err(match run(cmd, b"") {
            Ok(_) => why,
            Err(e) => format!("{why}（作りかけの worktree を消せませんでした: {e}）"),
        });
    }
    Ok(())
}

/// Applies the work-tree patch and copies the untracked files into the
/// fresh worktree `path`.
fn fill(path: &Path, patch: &[u8], untracked: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    if !patch.is_empty() {
        let mut cmd = git(path);
        cmd.args(["apply", "--binary", "--whitespace=nowarn", "-"]);
        run(cmd, patch).map_err(|e| format!("{WORKTREE_PATCH} を当てられません: {e}"))?;
    }
    for (rel, bytes) in untracked {
        let dir = fsx::open_dir(
            path,
            &path.join(rel.parent().unwrap_or(Path::new(""))),
            true,
        )
        .and_then(|dir| fsx::write_new_at(&dir, rel.file_name().unwrap_or(OsStr::new("")), bytes));
        dir.map_err(|e| format!("{} を写せません: {e}", rel.display()))?;
    }
    Ok(())
}

/// Every manifest file and untracked file of the rebuilt worktree `path`
/// against its digest; the error names each that differs.
fn check_files(path: &Path, manifest: &TargetManifest, rebuild: &Rebuild) -> Result<(), String> {
    let digest = |file: &str| -> Option<u64> {
        let full = path.join(relative(file)?);
        let file = fsx::open_read(path, &full).ok()?;
        let (bytes, over) = fsx::read_capped(file, MAX_WORKTREE_PATCH_BYTES).ok()?;
        (!over).then(|| capture::digest_bytes(&bytes))
    };
    let mut differ: Vec<&str> = Vec::new();
    for file in &manifest.files {
        let ok = if file.status == FileStatus::Deleted {
            relative(&file.path).is_some_and(|rel| fs::symlink_metadata(path.join(rel)).is_err())
        } else {
            digest(&file.path) == Some(file.digest)
        };
        if !ok {
            differ.push(&file.path);
        }
    }
    for (file, want) in &rebuild.untracked {
        if digest(file) != Some(*want) && !differ.contains(&file.as_str()) {
            differ.push(file);
        }
    }
    if differ.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "再構築した worktree が検証対象と一致しません: {}",
            differ.join(", ")
        ))
    }
}

/// Why `path` is not a detached worktree of `repo` at `head_sha` (named in
/// the error), as a resume checks it.
pub fn check(repo: &Path, path: &Path, head_sha: &str) -> Result<(), String> {
    let shown = path.display();
    if fs::symlink_metadata(path).is_err() {
        return Err(format!("worktree {shown} がありません"));
    }
    let Some(entry) = entry_at(repo, path)? else {
        return Err(format!(
            "{shown} はこのリポジトリの worktree ではありません"
        ));
    };
    if let Some(branch) = &entry.branch {
        return Err(format!(
            "worktree {shown} はブランチ {} を取り出しています（detached ではありません）",
            branch.strip_prefix("refs/heads/").unwrap_or(branch)
        ));
    }
    if entry.bare || !entry.detached {
        return Err(format!("worktree {shown} は detached ではありません"));
    }
    match entry.head.as_deref() {
        Some(head) if head == head_sha => Ok(()),
        Some(head) => Err(format!(
            "worktree {shown} は {} ではなく {} を取り出しています",
            short(head_sha),
            short(head)
        )),
        None => Err(format!("worktree {shown} の HEAD がわかりません")),
    }
}

/// Removes `path` with `git worktree remove --force` when `git worktree
/// list --porcelain` shows it as a detached worktree of `repo` at that
/// path; anything else is left, and the error says why.
pub fn remove(repo: &Path, path: &Path) -> Result<(), String> {
    let shown = path.display();
    let entries = list(repo)?;
    let Some(index) = entries.iter().position(|e| same_path(&e.path, path)) else {
        return Err(format!(
            "{shown} はこのリポジトリの worktree ではないため削除しません（移動されたか、別のものです）"
        ));
    };
    let entry = &entries[index];
    if index == 0 || entry.bare {
        return Err(format!("{shown} はリポジトリ本体のため削除しません"));
    }
    if let Some(branch) = &entry.branch {
        return Err(format!(
            "worktree {shown} はブランチ {} を取り出しているため削除しません",
            branch.strip_prefix("refs/heads/").unwrap_or(branch)
        ));
    }
    if !entry.detached {
        return Err(format!(
            "worktree {shown} は detached ではないため削除しません"
        ));
    }
    if entry.locked {
        return Err(format!(
            "worktree {shown} はロックされているため削除しません"
        ));
    }
    let mut cmd = git(repo);
    cmd.args(["worktree", "remove", "--force"]).arg(&entry.path);
    run(cmd, b"")
        .map(|_| ())
        .map_err(|e| format!("worktree {shown} を削除できません: {e}"))
}

/// `repo`'s worktrees, as `git worktree list --porcelain` gives them.
pub fn list(repo: &Path) -> Result<Vec<Entry>, String> {
    let mut cmd = git(repo);
    cmd.args(["worktree", "list", "--porcelain", "-z"]);
    run(cmd, b"").map(|out| parse_porcelain(&out)).map_err(|_| {
        capture::repo_root(repo)
            .err()
            .unwrap_or_else(|| format!("{} の worktree を一覧できません", repo.display()))
    })
}

/// Parses `git worktree list --porcelain` output (pure): one attribute per
/// line, a blank line after each entry; with `-z`, NUL in place of each
/// newline, so a path may hold any byte but NUL.
pub fn parse_porcelain(text: &[u8]) -> Vec<Entry> {
    let separator = if text.contains(&0) { 0 } else { b'\n' };
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    for field in text.split(|&b| b == separator) {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|&b| b == b' ') {
            Some(at) => (&field[..at], Some(&field[at + 1..])),
            None => (field, None),
        };
        if key == b"worktree" {
            entries.extend(current.take());
            current = Some(Entry {
                path: PathBuf::from(OsString::from_vec(value.unwrap_or_default().to_vec())),
                ..Entry::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        let text = || String::from_utf8_lossy(value.unwrap_or_default()).into_owned();
        match key {
            b"HEAD" => entry.head = Some(text()),
            b"branch" => entry.branch = Some(text()),
            b"detached" => entry.detached = true,
            b"bare" => entry.bare = true,
            b"locked" => entry.locked = true,
            b"prunable" => entry.prunable = true,
            _ => {}
        }
    }
    entries.extend(current);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::tests::{diff_target, dirty, repo, sh, temp};
    use crate::capture::{Captured, capture};
    use crate::model::types::Head;

    /// The snapshot of `head` (captured into a folder of its own) and
    /// where verifier `claude` of session `ver-1` would get its worktree.
    fn setup(repo: &Path, head: Head) -> (Captured, PathBuf, PathBuf) {
        let target_dir = temp("snap").join("target");
        let captured = capture(&diff_target(repo, head), &target_dir, 0).unwrap();
        let main = main_worktree(repo).unwrap();
        let path = path_for(&main, &SessionId::new("ver-1"), &PartId::new("claude"));
        (captured, target_dir, path)
    }

    fn branches(repo: &Path) -> String {
        sh(repo, &["branch", "--list", "--all"])
    }

    fn detached_head(path: &Path) -> String {
        sh(path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_owned()
    }

    #[test]
    fn ac03_a_ref_head_is_rebuilt_as_a_detached_worktree_beside_the_main_one() {
        let repo = repo();
        let before = branches(&repo);
        let (c, target_dir, path) = setup(&repo, Head::Ref("feat".into()));
        let mut want = repo.as_os_str().to_owned();
        want.push("-wt/verify-ver-1-claude");
        assert_eq!(path, PathBuf::from(want));
        add(&repo, &path, &target_dir, &c.manifest, &c.rebuild).unwrap();
        assert_eq!(detached_head(&path), "HEAD");
        assert_eq!(sh(&path, &["rev-parse", "HEAD"]).trim(), c.rebuild.head_sha);
        assert_eq!(
            fs::read_to_string(path.join("a.txt")).unwrap(),
            "one\nTWO\nthree\nfour\n"
        );
        assert!(!path.join("gone.txt").exists());
        assert_eq!(branches(&repo), before, "a branch was created");
        check(&repo, &path, &c.rebuild.head_sha).unwrap();
    }

    #[test]
    fn ac03_the_work_tree_mode_rebuilds_uncommitted_and_untracked_files() {
        let repo = repo();
        dirty(&repo);
        let before = branches(&repo);
        let (c, target_dir, path) = setup(&repo, Head::Worktree);
        add(&repo, &path, &target_dir, &c.manifest, &c.rebuild).unwrap();
        assert_eq!(
            fs::read_to_string(path.join("a.txt")).unwrap(),
            "one\nTWO\nthree\nfour\nfive\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("staged.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("notes/draft.txt")).unwrap(),
            "wip\nmore\n"
        );
        assert_eq!(
            fs::read(path.join("blob.bin")).unwrap(),
            [0u8, 159, 146, 150, 0, 1]
        );
        assert!(!path.join("ignored.txt").exists());
        assert!(!path.join(".noa/verify/ver-1/x.txt").exists());
        assert_eq!(detached_head(&path), "HEAD");
        assert_eq!(branches(&repo), before, "a branch was created");
        // The person's tree is as it was.
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\nTWO\nthree\nfour\nfive\n"
        );
    }

    #[test]
    fn ac03_a_tampered_patch_refuses_round_one_naming_the_files() {
        let repo = repo();
        dirty(&repo);
        let (c, target_dir, path) = setup(&repo, Head::Worktree);
        let patch_path = target_dir.join(WORKTREE_PATCH);
        let tampered = String::from_utf8(fs::read(&patch_path).unwrap())
            .unwrap()
            .replace("+five", "+FIVE");
        fs::write(&patch_path, &tampered).unwrap();

        // The snapshot no longer hashes as the recipe says: nothing is made.
        let err = add(&repo, &path, &target_dir, &c.manifest, &c.rebuild).unwrap_err();
        assert!(err.contains(WORKTREE_PATCH), "{err}");
        assert!(!path.exists());

        // A recipe that hashes the tampered patch builds a tree that is not
        // the manifest's: refused, the file named, the worktree gone again.
        let rebuild = Rebuild {
            worktree_patch: Some(capture::digest_bytes(tampered.as_bytes())),
            ..c.rebuild.clone()
        };
        let err = add(&repo, &path, &target_dir, &c.manifest, &rebuild).unwrap_err();
        assert!(
            err.contains("一致しません") && err.contains("a.txt"),
            "{err}"
        );
        assert!(!err.contains("staged.txt"), "{err}");
        assert!(!path.exists(), "the failed worktree was left");
        assert!(entry_at(&repo, &path).unwrap().is_none());
        // And the honest recipe still builds it afterwards.
        fs::write(&patch_path, tampered.replace("+FIVE", "+five")).unwrap();
        add(&repo, &path, &target_dir, &c.manifest, &c.rebuild).unwrap();
    }

    #[test]
    fn ac04_only_a_detached_worktree_at_the_recorded_path_is_removed() {
        let repo = repo();
        let (c, target_dir, path) = setup(&repo, Head::Ref("feat".into()));
        let make = |path: &Path| add(&repo, path, &target_dir, &c.manifest, &c.rebuild).unwrap();

        make(&path);
        remove(&repo, &path).unwrap();
        assert!(!path.exists());
        assert!(entry_at(&repo, &path).unwrap().is_none());

        // Re-pointed to a branch: left, and named.
        make(&path);
        sh(&path, &["switch", "-q", "-c", "mine"]);
        let err = remove(&repo, &path).unwrap_err();
        assert!(err.contains("ブランチ mine"), "{err}");
        assert!(path.join("a.txt").exists());

        // Moved away: the recorded path is no worktree any more.
        let other = path.with_file_name("verify-ver-1-codex");
        make(&other);
        let moved = path.with_file_name("moved-elsewhere");
        sh(
            &repo,
            &[
                "worktree",
                "move",
                other.to_str().unwrap(),
                moved.to_str().unwrap(),
            ],
        );
        let err = remove(&repo, &other).unwrap_err();
        assert!(err.contains("worktree ではない"), "{err}");
        assert!(moved.join("a.txt").exists());

        // Never the repository itself.
        assert!(remove(&repo, &repo).is_err());
        assert!(repo.join("a.txt").exists());
    }

    #[test]
    fn ac19_check_refuses_a_worktree_not_detached_at_the_head_and_a_missing_one_is_rebuilt() {
        let repo = repo();
        let (c, target_dir, path) = setup(&repo, Head::Ref("feat".into()));
        let head = c.rebuild.head_sha.clone();
        add(&repo, &path, &target_dir, &c.manifest, &c.rebuild).unwrap();
        check(&repo, &path, &head).unwrap();

        sh(&path, &["checkout", "-q", "--detach", "HEAD~1"]);
        let err = check(&repo, &path, &head).unwrap_err();
        assert!(err.contains(short(&head)), "{err}");
        sh(&path, &["switch", "-q", "-c", "other"]);
        assert!(
            check(&repo, &path, &head)
                .unwrap_err()
                .contains("ブランチ other")
        );
        assert!(
            check(&repo, &repo, &head)
                .unwrap_err()
                .contains("ブランチ feat")
        );
        let plain = temp("not-a-worktree");
        assert!(
            check(&repo, &plain, &head)
                .unwrap_err()
                .contains("worktree ではありません")
        );

        // Deleted by hand: missing, and rebuilt in its place from the recipe.
        let second = path.with_file_name("verify-ver-1-codex");
        add(&repo, &second, &target_dir, &c.manifest, &c.rebuild).unwrap();
        fs::remove_dir_all(&second).unwrap();
        assert!(
            check(&repo, &second, &head)
                .unwrap_err()
                .contains("ありません")
        );
        add(&repo, &second, &target_dir, &c.manifest, &c.rebuild).unwrap();
        check(&repo, &second, &head).unwrap();
    }

    #[test]
    fn the_main_worktree_is_found_from_a_linked_one() {
        let repo = repo();
        let mut linked = repo.as_os_str().to_owned();
        linked.push("-wt/feature");
        let linked = PathBuf::from(linked);
        sh(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
                "main",
            ],
        );
        assert_eq!(main_worktree(&linked).unwrap(), repo);
        let mut want = repo.as_os_str().to_owned();
        want.push("-wt/verify-ver-2-codex");
        assert_eq!(
            path_for(
                &main_worktree(&linked).unwrap(),
                &SessionId::new("ver-2"),
                &PartId::new("codex")
            ),
            PathBuf::from(want)
        );
        assert!(main_worktree(&temp("plain")).is_err());
    }

    #[test]
    fn porcelain_entries_are_read_with_and_without_nul_separators() {
        let lines = b"worktree /src/app\nHEAD aaaa\nbranch refs/heads/main\n\n\
worktree /src/app-wt/verify-ver-1-claude\nHEAD bbbb\ndetached\nlocked reason here\n\n\
worktree /src/bare\nbare\n\nworktree /gone\nHEAD cccc\ndetached\nprunable gitdir file points to non-existent location\n\n";
        let entries = parse_porcelain(lines);
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[0],
            Entry {
                path: "/src/app".into(),
                head: Some("aaaa".into()),
                branch: Some("refs/heads/main".into()),
                ..Entry::default()
            }
        );
        assert!(entries[1].detached && entries[1].locked && entries[1].branch.is_none());
        assert!(entries[2].bare && entries[2].head.is_none());
        assert!(entries[3].prunable);

        let nul = b"worktree /a b\nc\0HEAD dddd\0detached\0\0worktree /x\0HEAD eeee\0branch refs/heads/x\0\0";
        let entries = parse_porcelain(nul);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("/a b\nc"));
        assert!(entries[0].detached);
        assert_eq!(entries[1].branch.as_deref(), Some("refs/heads/x"));
        assert_eq!(parse_porcelain(b""), Vec::new());
        // An attribute before any `worktree` line is ignored.
        assert_eq!(parse_porcelain(b"HEAD x\n\n").len(), 0);
    }
}
