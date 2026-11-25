//! The widget's own files, outside the repository:
//!
//! ```text
//! <data dir>/noa/tasks/<repo-key>/
//!   runs.jsonl    the run log (append-only)
//!   github.json   the last GitHub listing, so the window opens with it
//! ```
//!
//! and, in a run's work tree, `.noa/` made and gitignored (`*`) by
//! [`prepare_workdir`], so local tasks and run files are never committed.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fsx;

pub const RUNS_FILE: &str = "runs.jsonl";
pub const GH_CACHE_FILE: &str = "github.json";

/// Where the widget keeps its files: `<data dir>/noa/tasks`, beside Noa's
/// own session state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("tasks"))
}

/// The directory name of `repo`'s files under the root:
/// `<dir name>-<FNV-1a 64 of the path, hex>`, readable and unique per path
/// (`/src/noa/` and `/src/noa` are one path).
pub fn repo_key(repo: &Path) -> String {
    let repo: PathBuf = repo.components().collect();
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    format!("{name}-{:016x}", fnv1a(repo.as_os_str().as_bytes()))
}

/// `<root>/<key>/runs.jsonl`.
pub fn runs_log(root: &Path, key: &str) -> PathBuf {
    root.join(key).join(RUNS_FILE)
}

/// `<root>/<key>/github.json`.
pub fn gh_cache(root: &Path, key: &str) -> PathBuf {
    root.join(key).join(GH_CACHE_FILE)
}

/// The line [`prepare_workdir`] adds to a `.noa/.gitignore` it did not
/// write: the tasks folder, relative to `.noa/`.
const TASKS_IGNORE_LINE: &str = "/tasks/";

/// Makes `<workdir>/.noa/tasks/` and a `.noa/.gitignore` holding `*` when
/// there is none; when git still does not ignore `.noa/tasks/` (a tracked
/// `.noa/.gitignore`), appends the ignore line, and refuses with the reason
/// if git still does not. A `.noa` (or anything below it) that is a link is
/// refused.
pub fn prepare_workdir(workdir: &Path) -> io::Result<()> {
    if make_workdir(workdir)? {
        return Ok(());
    }
    if tasks_dir_ignored(workdir)? {
        return Ok(());
    }
    let gitignore = workdir.join(".noa").join(".gitignore");
    append_ignore_line(&gitignore)?;
    if tasks_dir_ignored(workdir)? {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} を書き足しても .noa/tasks/ が Git に無視されません。タスクと報告が Git に含まれるおそれがあるため書きません",
        gitignore.display()
    )))
}

/// `<workdir>/.noa/tasks/`, and a `.noa/.gitignore` holding `*` when there
/// is none; whether it wrote the `.gitignore`.
fn make_workdir(workdir: &Path) -> io::Result<bool> {
    let dir = workdir.join(".noa");
    fsx::create_dirs(workdir, &crate::local::tasks_dir(workdir))?;
    // `create_new` does not follow a link left at the name.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut file) => file.write_all(b"*\n").map(|()| true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether git ignores `<workdir>/.noa/tasks/` (`git check-ignore`, run
/// without the person's `GIT_*` variables).
fn tasks_dir_ignored(workdir: &Path) -> io::Result<bool> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(workdir)
        .args(["check-ignore", "-q", "--"])
        .arg(crate::local::TASKS_DIR)
        .stdin(Stdio::null());
    without_git_env(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| io::Error::other(format!("git を起動できません: {e}")))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(io::Error::other(format!(
            ".noa/tasks/ が Git に無視されているか確かめられないため書きません: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Appends [`TASKS_IGNORE_LINE`] to `gitignore`, on a line of its own,
/// keeping every line it has. A link or anything but a plain file is
/// refused.
fn append_ignore_line(gitignore: &Path) -> io::Result<()> {
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    let file = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(gitignore)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} は通常のファイルではありません", gitignore.display()),
        ));
    }
    let mut last = [0u8; 1];
    let open_line = meta.len() > 0 && {
        file.read_exact_at(&mut last, meta.len() - 1)?;
        last[0] != b'\n'
    };
    let line = format!("{}{TASKS_IGNORE_LINE}\n", if open_line { "\n" } else { "" });
    (&file).write_all(line.as_bytes())
}

/// Clears every `GIT_*` variable `cmd` would inherit, so git finds the
/// repository from its directory alone, not from the person's environment
/// (a `GIT_DIR` or `GIT_WORK_TREE` set in their shell, say).
pub(crate) fn without_git_env(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
}

/// 64-bit FNV-1a.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    /// `git -C dir <args>`'s exit status, run without the test's `GIT_*`
    /// variables.
    fn git(dir: &Path, args: &[&str]) -> i32 {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        without_git_env(&mut cmd);
        cmd.status().unwrap().code().unwrap()
    }

    fn ignored(dir: &Path, path: &str) -> bool {
        git(dir, &["check-ignore", "-q", path]) == 0
    }

    #[test]
    fn ac22_a_missing_noa_gitignore_is_written_as_a_star() {
        let tmp = TempDir::new("prep-fresh");
        let repo = tmp.path();
        assert_eq!(git(repo, &["init", "-q"]), 0);
        prepare_workdir(repo).unwrap();
        assert!(repo.join(".noa/tasks").is_dir());
        assert_eq!(
            fs::read_to_string(repo.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        fs::write(repo.join(".noa/tasks/t-1.md"), "x").unwrap();
        assert!(ignored(repo, ".noa/tasks/t-1.md"));
        // Again: kept as it is.
        prepare_workdir(repo).unwrap();
        assert_eq!(
            fs::read_to_string(repo.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn ac22_an_existing_gitignore_is_kept_and_gets_the_tasks_line_only_when_needed() {
        let tmp = TempDir::new("prep-kept");
        let repo = tmp.path();
        assert_eq!(git(repo, &["init", "-q"]), 0);
        fs::create_dir_all(repo.join(".noa")).unwrap();
        let gitignore = repo.join(".noa/.gitignore");
        // Already ignores everything: untouched.
        fs::write(&gitignore, "*\n").unwrap();
        prepare_workdir(repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "*\n");
        // A tracked one of the person's that ignores something else, its
        // last line without a newline.
        fs::write(&gitignore, "logs/").unwrap();
        assert_eq!(git(repo, &["add", "-f", ".noa/.gitignore"]), 0);
        assert_eq!(git(repo, &["commit", "-qm", "noa"]), 0);
        prepare_workdir(repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "logs/\n/tasks/\n");
        fs::write(repo.join(".noa/tasks/t-1.md"), "x").unwrap();
        assert!(ignored(repo, ".noa/tasks/t-1.md"));
        prepare_workdir(repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "logs/\n/tasks/\n");
    }

    #[test]
    fn ac22_refused_with_the_reason_when_git_still_does_not_ignore_the_tasks() {
        // A file tracked under .noa/tasks/: git never reports the folder
        // ignored, whatever the .gitignore says.
        let tmp = TempDir::new("prep-tracked");
        let repo = tmp.path();
        assert_eq!(git(repo, &["init", "-q"]), 0);
        fs::create_dir_all(repo.join(".noa/tasks")).unwrap();
        fs::write(repo.join(".noa/tasks/keep"), "x").unwrap();
        fs::write(repo.join(".noa/.gitignore"), "logs/\n").unwrap();
        assert_eq!(git(repo, &["add", "-f", ".noa"]), 0);
        assert_eq!(git(repo, &["commit", "-qm", "noa"]), 0);
        let err = prepare_workdir(repo).unwrap_err();
        assert!(err.to_string().contains(".noa/tasks/"), "{err}");
        assert!(err.to_string().contains("書きません"), "{err}");

        // Not a repository: git cannot say, so it is refused too.
        let tmp = TempDir::new("prep-nogit");
        let dir = tmp.path();
        fs::create_dir_all(dir.join(".noa")).unwrap();
        fs::write(dir.join(".noa/.gitignore"), "logs/\n").unwrap();
        if git(dir, &["rev-parse", "--git-dir"]) != 0 {
            let err = prepare_workdir(dir).unwrap_err();
            assert!(err.to_string().contains(".noa/tasks/"), "{err}");
            assert_eq!(
                fs::read_to_string(dir.join(".noa/.gitignore")).unwrap(),
                "logs/\n"
            );
        }
    }

    #[test]
    fn ac22_a_noa_that_is_a_link_is_refused() {
        let tmp = TempDir::new("prep-link");
        let repo = tmp.path().join("repo");
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(git(&repo, &["init", "-q"]), 0);
        std::os::unix::fs::symlink(&elsewhere, repo.join(".noa")).unwrap();
        assert!(prepare_workdir(&repo).is_err());
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }

    #[test]
    fn a_trailing_slash_does_not_change_the_repo_key() {
        assert_eq!(
            repo_key(Path::new("/src/noa/")),
            repo_key(Path::new("/src/noa"))
        );
        assert_eq!(
            repo_key(Path::new("/src/noa/")),
            format!("noa-{:016x}", fnv1a(b"/src/noa"))
        );
    }

    #[test]
    fn the_repo_key_names_the_directory_and_hashes_the_path() {
        // FNV-1a's published vectors.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        let key = repo_key(Path::new("/src/noa"));
        assert_eq!(key, format!("noa-{:016x}", fnv1a(b"/src/noa")));
        assert_ne!(key, repo_key(Path::new("/other/noa")));
        assert_eq!(
            runs_log(Path::new("/d"), &key),
            Path::new("/d").join(&key).join("runs.jsonl")
        );
        assert_eq!(
            gh_cache(Path::new("/d"), "k"),
            Path::new("/d/k/github.json")
        );
    }
}
