//! A worker's result file (AU-03): `spawn` gives each worker its own
//! `NOA_RESULT=<cwd>/.noa/result-<label>.md` (a random name without a plain
//! label), so workers sharing a cwd never share one, in a `.noa/` directory that ignores
//! itself (`.gitignore` holding `*`), so the file never shows in `git
//! status` and never keeps `git worktree remove` (`close --prune`) from
//! removing a worker's worktree. A wait line names the file once the worker
//! wrote it, and `+ctl result PANE` prints it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The variable that tells a worker where to write its final report.
pub(crate) const RESULT_ENV: &str = "NOA_RESULT";

/// The result file of a worker labelled `label` in `cwd`, when the label
/// is a plain name (`worktree::check_label`): `<cwd>/.noa/result-<label>.md`.
/// Also how a pane the journal does not know is looked up.
pub(crate) fn labelled_path(cwd: &Path, label: Option<&str>) -> Option<PathBuf> {
    let label = label.filter(|label| super::worktree::check_label(label).is_ok())?;
    Some(cwd.join(".noa").join(format!("result-{label}.md")))
}

/// The result file a new worker in `cwd` gets: [`labelled_path`], else
/// `<cwd>/.noa/result-<8 random hex digits>.md`.
pub(crate) fn path_for(cwd: &Path, label: Option<&str>) -> PathBuf {
    labelled_path(cwd, label).unwrap_or_else(|| {
        cwd.join(".noa")
            .join(format!("result-{:08x}.md", rand::random::<u32>()))
    })
}

/// Make `<cwd>/.noa/` with its self-ignoring `.gitignore`, and return the
/// result path of a worker labelled `label` ([`path_for`]). An existing
/// `.gitignore` there is left as it is.
pub(crate) fn prepare(cwd: &Path, label: Option<&str>) -> std::io::Result<PathBuf> {
    let dir = cwd.join(".noa");
    std::fs::create_dir_all(&dir)?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut file) => std::io::Write::write_all(&mut file, b"*\n")?,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    Ok(path_for(cwd, label))
}

/// `{"path":…,"bytes":N}` when the file at `path` was written at or after
/// `since` (the wait, or the send of `send --wait`, began): a result left
/// from an earlier task does not count.
pub(crate) fn written_since(path: &Path, since: SystemTime) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    (meta.is_file() && modified >= since).then(|| {
        serde_json::json!({
            "path": path.to_string_lossy(),
            "bytes": meta.len(),
        })
    })
}

/// `+ctl result`: the file's text, the first `max_bytes` of it (cut on a
/// character boundary) when given, and whether it was cut. `Err` says why
/// there is none.
pub(crate) fn read(
    pane: u64,
    path: &Path,
    max_bytes: Option<usize>,
) -> Result<(String, bool), String> {
    let bytes = std::fs::read(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "pane {pane} has no result: {} does not exist (the worker writes its report \
                 to ${RESULT_ENV})",
                path.display()
            )
        } else {
            format!("cannot read pane {pane}'s result {}: {err}", path.display())
        }
    })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match max_bytes {
        Some(max) if text.len() > max => {
            let mut end = max;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            Ok((text[..end].to_string(), true))
        }
        _ => Ok((text, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-result-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git runs")
    }

    // Audit finding (AU-03): a report written inside a worker's worktree
    // left it untracked, so `close --prune` (`git worktree remove`, never
    // forced) kept the worktree and its branch.
    #[test]
    fn a_result_file_leaves_git_status_clean_and_the_worktree_removable() {
        let base = temp_dir("git");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(&repo, &["init", "-q", "-b", "main"]).status.success());
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        assert!(git(&repo, &["add", "."]).status.success());
        assert!(git(&repo, &["commit", "-q", "-m", "init"]).status.success());
        let wt = base.join("wt");
        let wt_arg = wt.to_string_lossy().into_owned();
        let added = git(
            &repo,
            &["worktree", "add", "-q", "-b", "orch/x", &wt_arg, "HEAD"],
        );
        assert!(added.status.success());

        let path = prepare(&wt, Some("x")).unwrap();
        assert_eq!(path, wt.join(".noa").join("result-x.md"));
        std::fs::write(&path, "done\n").unwrap();
        let status = git(&wt, &["status", "--porcelain", "--untracked-files=all"]);
        assert_eq!(String::from_utf8_lossy(&status.stdout), "");
        // Another worker in the same cwd: its own file, still ignored.
        let other = prepare(&wt, None).unwrap();
        assert_ne!(other, path);
        std::fs::write(&other, "done\n").unwrap();
        let status = git(&wt, &["status", "--porcelain", "--untracked-files=all"]);
        assert_eq!(String::from_utf8_lossy(&status.stdout), "");

        let removed = git(&repo, &["worktree", "remove", &wt_arg]);
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
        assert!(!wt.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    // Follow-up to AU-03: workers spawned in one cwd without `--worktree`
    // all wrote `.noa/result.md`, so one's report read as another's.
    #[test]
    fn each_worker_gets_its_own_result_file() {
        let cwd = Path::new("/r");
        assert_eq!(
            path_for(cwd, Some("fix-1")),
            PathBuf::from("/r/.noa/result-fix-1.md")
        );
        assert_eq!(
            labelled_path(cwd, Some("fix-1")),
            Some(PathBuf::from("/r/.noa/result-fix-1.md"))
        );
        // No label, or one that is not a plain name: a random name.
        for label in [None, Some("a b"), Some("../x")] {
            assert_eq!(labelled_path(cwd, label), None, "{label:?}");
            let name = path_for(cwd, label)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let hex = name
                .strip_prefix("result-")
                .and_then(|rest| rest.strip_suffix(".md"))
                .unwrap();
            assert_eq!(hex.len(), 8, "{name}");
            assert!(hex.chars().all(|ch| ch.is_ascii_hexdigit()), "{name}");
        }
        assert_ne!(path_for(cwd, None), path_for(cwd, None));
    }

    #[test]
    fn a_result_counts_only_once_written_after_the_wait_began() {
        let dir = temp_dir("since");
        let path = path_for(&dir, Some("w"));
        assert_eq!(written_since(&path, SystemTime::now()), None);
        prepare(&dir, Some("w")).unwrap();
        std::fs::write(&path, "old").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        assert_eq!(written_since(&path, later), None);
        let earlier = SystemTime::now() - std::time::Duration::from_secs(60);
        let info = written_since(&path, earlier).unwrap();
        assert_eq!(info["bytes"], 3);
        assert_eq!(info["path"], path.to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_result_is_printed_whole_or_cut_at_a_character() {
        let dir = temp_dir("read");
        let path = prepare(&dir, Some("w")).unwrap();
        let missing = read(4, &path, None).unwrap_err();
        assert!(missing.contains("pane 4 has no result"), "{missing}");
        assert!(missing.contains("$NOA_RESULT"), "{missing}");
        std::fs::write(&path, "aあb").unwrap();
        assert_eq!(read(4, &path, None).unwrap(), ("aあb".to_string(), false));
        assert_eq!(read(4, &path, Some(2)).unwrap(), ("a".to_string(), true));
        assert_eq!(
            read(4, &path, Some(9)).unwrap(),
            ("aあb".to_string(), false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
