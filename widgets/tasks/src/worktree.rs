//! Work trees for runs isolated in their own (`Isolation::Worktree`, the
//! default): `git worktree add -b task-<run-id> <main>-wt/task-<run-id> HEAD`.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::types::RunId;
use crate::store::without_git_env;

/// `git -C dir <args>`, without the person's `GIT_*` variables; its stdout,
/// or why it failed.
fn git(dir: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    without_git_env(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// A path git printed, its line end dropped.
fn printed_path(mut out: Vec<u8>) -> PathBuf {
    while out.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        out.pop();
    }
    PathBuf::from(OsString::from_vec(out))
}

/// The top of the work tree holding `dir` (`git rev-parse --show-toplevel`).
pub fn toplevel(dir: &Path) -> Result<PathBuf, String> {
    git(dir, &["rev-parse".as_ref(), "--show-toplevel".as_ref()])
        .map(printed_path)
        .map_err(|why| format!("{} は Git の作業ツリーではありません: {why}", dir.display()))
}

/// The main work tree of the repository `repo` is in: the first entry of
/// `git worktree list --porcelain` (from a linked worktree
/// `/src/app-wt/feature` of `/src/app`, it is `/src/app`).
pub fn main_worktree(repo: &Path) -> Result<PathBuf, String> {
    let out = git(
        repo,
        &[
            "worktree".as_ref(),
            "list".as_ref(),
            "--porcelain".as_ref(),
            "-z".as_ref(),
        ],
    )
    .map_err(|why| format!("{} の worktree を一覧できません: {why}", repo.display()))?;
    out.split(|b| *b == 0)
        .find_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .ok_or_else(|| "git worktree list が worktree を返しませんでした".to_owned())
}

/// `<main>-wt/task-<run-id>`, beside the main work tree (`main`'s trailing
/// slash, if any, dropped).
pub fn path_for(main: &Path, run: &RunId) -> PathBuf {
    let main: PathBuf = main.components().collect();
    let mut wt = main.into_os_string();
    wt.push("-wt");
    PathBuf::from(wt).join(branch_for(run))
}

/// `task-<run-id>`.
pub fn branch_for(run: &RunId) -> String {
    format!("task-{run}")
}

/// `git worktree add -b <branch> <path> HEAD`; the path and the branch.
pub fn add(repo: &Path, run: &RunId) -> Result<(PathBuf, String), String> {
    let main = main_worktree(repo)?;
    let path = path_for(&main, run);
    let branch = branch_for(run);
    git(
        repo,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "-b".as_ref(),
            branch.as_ref(),
            path.as_os_str(),
            "HEAD".as_ref(),
        ],
    )
    .map_err(|why| format!("worktree {} を作れません: {why}", path.display()))?;
    Ok((path, branch))
}

/// Takes back a worktree [`add`] made for a run that then never started:
/// it holds only what the start wrote (`.noa/`), so it goes with
/// `--force`; its branch, still at the commit it came from, with `-d`.
pub fn remove(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    git(
        repo,
        &[
            "worktree".as_ref(),
            "remove".as_ref(),
            "--force".as_ref(),
            path.as_os_str(),
        ],
    )
    .map_err(|why| format!("worktree {} を消せません: {why}", path.display()))?;
    git(repo, &["branch".as_ref(), "-d".as_ref(), branch.as_ref()])
        .map(drop)
        .map_err(|why| format!("ブランチ {branch} を消せません: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    /// A repository with one commit at `dir`.
    fn repo_at(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        ] {
            git(dir, &args.iter().map(|a| a.as_ref()).collect::<Vec<_>>()).unwrap();
        }
    }

    #[test]
    fn the_toplevel_and_main_worktree_are_found_from_inside() {
        let tmp = TempDir::new("wt-find");
        let repo = tmp.path().join("app");
        repo_at(&repo);
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        assert_eq!(toplevel(&repo.join("src/deep")).unwrap(), repo);
        assert_eq!(main_worktree(&repo.join("src")).unwrap(), repo);
        assert!(toplevel(tmp.path()).is_err() || toplevel(tmp.path()).unwrap() != repo);
    }

    #[test]
    fn add_makes_the_runs_worktree_on_its_branch_and_main_is_found_from_it() {
        let tmp = TempDir::new("wt-add");
        let repo = tmp.path().join("app");
        repo_at(&repo);
        let run = RunId::new("run-3");
        let (path, branch) = add(&repo, &run).unwrap();
        assert_eq!(path, tmp.path().join("app-wt/task-run-3"));
        assert_eq!(branch, "task-run-3");
        assert!(path.is_dir());
        let head = git(
            &path,
            &[
                "rev-parse".as_ref(),
                "--abbrev-ref".as_ref(),
                "HEAD".as_ref(),
            ],
        )
        .unwrap();
        assert_eq!(String::from_utf8(head).unwrap().trim(), "task-run-3");
        // From the linked worktree, the main one is still the repository.
        assert_eq!(main_worktree(&path).unwrap(), repo);
        assert_eq!(toplevel(&path).unwrap(), path);
        // The same run again: the branch is taken, refused with git's reason.
        assert!(add(&repo, &run).is_err());
    }

    #[test]
    fn a_runs_worktree_sits_beside_the_main_one_on_its_own_branch() {
        let run = RunId::new("run-9");
        assert_eq!(branch_for(&run), "task-run-9");
        assert_eq!(
            path_for(Path::new("/src/noa"), &run),
            Path::new("/src/noa-wt/task-run-9")
        );
    }

    #[test]
    fn a_trailing_slash_on_the_main_worktree_still_puts_the_runs_beside_it() {
        let run = RunId::new("run-9");
        assert_eq!(
            path_for(Path::new("/src/noa/"), &run).as_os_str(),
            "/src/noa-wt/task-run-9"
        );
    }
}
