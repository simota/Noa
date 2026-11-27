//! Git facts read from files, without `git` (L2-5, Decision 2): `git` would
//! honour the repo's own `.git/config` (`core.fsmonitor`, hooks), which is
//! code the scanned repo controls. Every path is lstat'ed and read with the
//! scan's size cap; any failure gives `None` (shown as 不明).

use std::path::{Path, PathBuf};

use crate::scan::source::read_capped;

/// At most this many `ref:` hops from HEAD.
const MAX_HOPS: usize = 5;

fn read_text(path: &Path) -> Option<String> {
    String::from_utf8(read_capped(path).ok()?).ok()
}

/// `<repo>/.git` when it is a folder; the target of a `gitdir: <p>` file
/// (relative to `repo`) when it is a file.
pub fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    let meta = std::fs::symlink_metadata(&dot).ok()?;
    if meta.file_type().is_dir() {
        return Some(dot);
    }
    let text = read_text(&dot)?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let dir = repo.join(target);
    std::fs::metadata(&dir).ok()?.is_dir().then_some(dir)
}

/// The work tree `dir` lies in: the first of `dir` and its ancestors with a
/// `.git` folder or a `gitdir:` file, found without `git` (Decision 2); `dir` itself
/// when none has one. Noa passes the focused pane's directory, often a
/// subdirectory of the repo.
pub fn repo_root(dir: &Path) -> PathBuf {
    dir.ancestors()
        .find(|d| {
            std::fs::symlink_metadata(d.join(".git")).is_ok_and(|m| {
                m.file_type().is_dir()
                    || (m.file_type().is_file()
                        && read_text(&d.join(".git")).is_some_and(|text| {
                            text.lines().next().is_some_and(|line| {
                                line.strip_prefix("gitdir:")
                                    .is_some_and(|target| !target.trim().is_empty())
                            })
                        }))
            })
        })
        .unwrap_or(dir)
        .to_owned()
}

/// The contents of `<git dir>/commondir` (relative to the git dir), else the
/// git dir.
pub fn common_dir(git_dir: &Path) -> PathBuf {
    read_text(&git_dir.join("commondir"))
        .and_then(|t| t.lines().next().map(|l| l.trim().to_owned()))
        .filter(|l| !l.is_empty())
        .map_or_else(|| git_dir.to_owned(), |rel| git_dir.join(rel))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A ref name that stays inside the git dir.
fn safe_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The commit HEAD names, lowercase.
pub fn head_commit(repo: &Path) -> Option<String> {
    let git = git_dir(repo)?;
    let common = common_dir(&git);
    let mut content = read_text(&git.join("HEAD"))?.trim().to_owned();
    for _ in 0..=MAX_HOPS {
        if is_sha(&content) {
            return Some(content.to_ascii_lowercase());
        }
        let name = content.strip_prefix("ref:")?.trim().to_owned();
        if !safe_ref(&name) {
            return None;
        }
        content = read_text(&git.join(&name))
            .or_else(|| read_text(&common.join(&name)))
            .map(|t| t.trim().to_owned())
            .or_else(|| packed(&common, &name))?;
    }
    None
}

/// The `<sha> <name>` line of `packed-refs`.
fn packed(common: &Path, name: &str) -> Option<String> {
    read_text(&common.join("packed-refs"))?
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
        .find_map(|l| {
            let (sha, n) = l.split_once(' ')?;
            (n.trim() == name).then(|| sha.to_owned())
        })
}

/// The repo's identity for candidate ids (I3): the canonical path of the
/// git common dir, or the canonical repo path when no `.git` is found.
pub fn repo_identity(repo: &Path) -> String {
    let path = git_dir(repo)
        .map(|g| common_dir(&g))
        .and_then(|c| std::fs::canonicalize(c).ok())
        .or_else(|| std::fs::canonicalize(repo).ok())
        .unwrap_or_else(|| repo.to_owned());
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0123456789abcdef0123456789abcdef01234567";
    const B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_plain_repo_on_a_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        put(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
        put(&repo.join(".git/refs/heads/main"), &format!("{A}\n"));
        assert_eq!(head_commit(repo).as_deref(), Some(A));
        assert_eq!(git_dir(repo), Some(repo.join(".git")));
        assert_eq!(
            repo_identity(repo),
            std::fs::canonicalize(repo.join(".git"))
                .unwrap()
                .to_string_lossy()
        );
    }

    #[test]
    fn a_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        put(
            &dir.path().join(".git/HEAD"),
            &format!("{}\n", B.to_uppercase()),
        );
        assert_eq!(head_commit(dir.path()).as_deref(), Some(B));
    }

    #[test]
    fn packed_refs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        put(&repo.join(".git/HEAD"), "ref: refs/heads/feature\n");
        put(
            &repo.join(".git/packed-refs"),
            &format!(
                "# pack-refs with: peeled\n{A} refs/heads/main\n{B} refs/heads/feature\n^{A}\n"
            ),
        );
        assert_eq!(head_commit(repo).as_deref(), Some(B));
    }

    #[test]
    fn a_linked_worktree_reads_the_common_dir() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        let gd = main.join(".git/worktrees/wt");
        put(&wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n");
        put(&gd.join("HEAD"), "ref: refs/heads/topic\n");
        put(&gd.join("commondir"), "../..\n");
        put(&main.join(".git/refs/heads/topic"), &format!("{A}\n"));
        assert_eq!(head_commit(&wt).as_deref(), Some(A));
        assert_eq!(
            repo_identity(&wt),
            std::fs::canonicalize(main.join(".git"))
                .unwrap()
                .to_string_lossy()
        );
        assert_eq!(repo_identity(&wt), repo_identity(&main));
    }

    #[test]
    fn the_root_is_the_nearest_ancestor_with_git() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let deep = repo.join("widgets/alpha/src");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(repo_root(&deep), repo);
        assert_eq!(repo_root(&repo), repo);
        // A linked worktree's `.git` is a file, and nearer wins.
        let wt = repo.join("widgets/alpha");
        put(&wt.join(".git"), "gitdir: /elsewhere\n");
        assert_eq!(repo_root(&deep), wt);
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(repo_root(&plain), plain, "no repo: the directory itself");
    }

    #[test]
    fn an_invalid_git_file_does_not_change_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let nested = repo.join("project");
        let deep = nested.join("src");
        std::fs::create_dir_all(&deep).unwrap();
        for invalid in ["", "not a worktree\n", "gitdir: \n"] {
            put(&nested.join(".git"), invalid);
            assert_eq!(repo_root(&deep), repo, "{invalid:?}");
        }
        put(&nested.join(".git"), "gitdir: ../.git/worktrees/project\n");
        assert_eq!(repo_root(&deep), nested);
    }

    #[test]
    fn failures_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        assert_eq!(head_commit(repo), None);
        assert_eq!(
            repo_identity(repo),
            std::fs::canonicalize(repo).unwrap().to_string_lossy()
        );
        put(&repo.join(".git/HEAD"), "ref: ../../etc/passwd\n");
        assert_eq!(head_commit(repo), None);
        put(&repo.join(".git/HEAD"), "ref: refs/heads/a\n");
        put(&repo.join(".git/refs/heads/a"), "ref: refs/heads/a\n");
        assert_eq!(head_commit(repo), None, "a ref loop ends");
        put(&repo.join(".git/HEAD"), "garbage\n");
        assert_eq!(head_commit(repo), None);
    }
}
