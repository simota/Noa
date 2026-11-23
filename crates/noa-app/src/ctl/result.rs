//! A worker's result file (AU-03): `spawn` gives each worker its own
//! `NOA_RESULT=<cwd>/.noa/result-<label>.md` (a random name without a plain
//! label, `-<8 hex>` added when that file exists), so workers sharing a cwd
//! never share one, in a `.noa/` directory that ignores
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

/// Make `<cwd>/.noa/` with its self-ignoring `.gitignore` and return it. An
/// existing `.gitignore` there is left as it is.
pub(crate) fn prepare_dir(cwd: &Path) -> std::io::Result<PathBuf> {
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
    Ok(dir)
}

/// Make `<cwd>/.noa/` ([`prepare_dir`]) and reserve the result file of a
/// worker labelled `label` there: `result-<label>.md` ([`path_for`]), or
/// `result-<label>-<8 hex>.md` when that name is taken. Labels are not
/// unique, and a worker writes its report only at the end, so the file is
/// created (empty) here: a second worker of that label in this cwd, spawned
/// while the first still works, cannot be handed the same path.
pub(crate) fn prepare(cwd: &Path, label: Option<&str>) -> std::io::Result<PathBuf> {
    let dir = prepare_dir(cwd)?;
    let first = path_for(cwd, label);
    let stem = first
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a result name is ASCII")
        .to_string();
    let mut path = first;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                path = dir.join(format!("{stem}-{:08x}.md", rand::random::<u32>()));
            }
            Err(err) => return Err(err),
        }
    }
}

/// `<dir>/<stem>.md`, or `<dir>/<stem>-<8 random hex digits>.md` when that
/// is taken.
fn unused_path(dir: &Path, stem: &str) -> PathBuf {
    let plain = dir.join(format!("{stem}.md"));
    if !plain.exists() {
        return plain;
    }
    loop {
        let path = dir.join(format!("{stem}-{:08x}.md", rand::random::<u32>()));
        if !path.exists() {
            return path;
        }
    }
}

/// `close --prune`, before `git worktree remove`: move every
/// `<worktree>/.noa/result-*.md` into `<main>/.noa/` (made the same
/// self-ignoring way), so a worker's report outlives its worktree. A name
/// already taken there gets `-<8 hex>` ([`unused_path`]). Each move made
/// is pushed onto `moved` as `(from, to)`; `Err` says which file could not
/// be moved (those before it were).
pub(crate) fn move_out(
    worktree: &Path,
    main: &Path,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    let from_dir = worktree.join(".noa");
    let entries = match std::fs::read_dir(&from_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("cannot read {}: {err}", from_dir.display())),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        // A file still empty is a name `prepare` reserved for a worker that
        // wrote no report: nothing to keep.
        .filter(|entry| {
            entry
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
        })
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("result-") && name.ends_with(".md"))
        })
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    files.sort();
    let to_dir = prepare_dir(main)
        .map_err(|err| format!("cannot make {}: {err}", main.join(".noa").display()))?;
    for from in files {
        let stem = from
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("filtered to UTF-8 names");
        let to = unused_path(&to_dir, stem);
        std::fs::rename(&from, &to).map_err(|err| {
            format!(
                "cannot move the result file {} to {}: {err}",
                from.display(),
                to.display()
            )
        })?;
        moved.push((from, to));
    }
    Ok(())
}

/// `{"path":…,"bytes":N}` when the file at `path` was written at or after
/// `since` (the wait, or the send of `send --wait`, began): a result left
/// from an earlier task does not count.
pub(crate) fn written_since(path: &Path, since: SystemTime) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    (meta.is_file() && meta.len() > 0 && modified >= since).then(|| {
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
    if bytes.is_empty() {
        return Err(format!(
            "pane {pane} has no result yet: {} is empty (the worker writes its report to \
             ${RESULT_ENV})",
            path.display()
        ));
    }
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

/// How much of a result file [`read_start`] reads, and how many of its
/// lines it keeps (as the Agent Dashboard's drawer does).
pub(crate) const START_BYTES: u64 = 16 * 1024;
pub(crate) const START_LINES: usize = 200;

/// Why [`read_start`] read nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartError {
    /// A FIFO, a device, a directory or a symbolic link: never read.
    NotRegular,
    Io(String),
}

/// The start of the result file at `path` (`+ctl board`, D18): at most
/// [`START_BYTES`] and [`START_LINES`] of it, and whether it was cut. Only
/// a regular file is read: a FIFO would block the board and a device may
/// never end. What is checked is the file opened, not the path, and the
/// open neither follows a link nor waits for a FIFO's writer.
pub(crate) fn read_start(path: &Path) -> Result<(String, bool), StartError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| match err.raw_os_error() {
            Some(libc::ELOOP) => StartError::NotRegular,
            _ => StartError::Io(err.to_string()),
        })?;
    let meta = file
        .metadata()
        .map_err(|err| StartError::Io(err.to_string()))?;
    if !meta.is_file() {
        return Err(StartError::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(START_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| StartError::Io(err.to_string()))?;
    let mut cut = bytes.len() as u64 > START_BYTES;
    if cut {
        bytes.truncate(START_BYTES as usize);
        // A character the limit cut in two is left out whole.
        if let Err(err) = std::str::from_utf8(&bytes)
            && err.error_len().is_none()
        {
            bytes.truncate(err.valid_up_to());
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().take(START_LINES + 1).collect();
    cut |= lines.len() > START_LINES;
    let kept = &lines[..lines.len().min(START_LINES)];
    Ok((kept.join("\n"), cut))
}

/// `+ctl result --path`: the path and a newline. Touches no file, so an
/// existing, empty or missing file all give the same line.
pub(crate) fn write_path(out: &mut impl std::io::Write, path: &Path) -> std::io::Result<()> {
    writeln!(out, "{}", path.display())
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

    // Review finding #5: two workers with one label in one cwd shared
    // `.noa/result-<label>.md`, so one's report overwrote the other's.
    #[test]
    fn a_label_whose_file_exists_gets_a_disambiguated_name() {
        let dir = temp_dir("same-label");
        let first = prepare(&dir, Some("review")).unwrap();
        assert_eq!(first, dir.join(".noa").join("result-review.md"));
        // The name is taken from the moment it is handed out, written or not.
        let second = prepare(&dir, Some("review")).unwrap();
        assert_ne!(second, first);
        assert_eq!(second.parent(), first.parent());
        let name = second.file_name().unwrap().to_string_lossy().into_owned();
        let hex = name
            .strip_prefix("result-review-")
            .and_then(|rest| rest.strip_suffix(".md"))
            .unwrap();
        assert_eq!(hex.len(), 8, "{name}");
        assert!(hex.chars().all(|ch| ch.is_ascii_hexdigit()), "{name}");
        let _ = std::fs::remove_dir_all(&dir);
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

    // PR #210 review: two workers of one label spawned in one cwd before
    // either wrote its report were handed the same path.
    #[test]
    fn a_result_name_is_reserved_when_it_is_handed_out() {
        let dir = temp_dir("reserve");
        let first = prepare(&dir, Some("fix")).unwrap();
        let second = prepare(&dir, Some("fix")).unwrap();
        assert_ne!(first, second);
        assert!(first.is_file() && second.is_file());
        let a = prepare(&dir, None).unwrap();
        let b = prepare(&dir, None).unwrap();
        assert_ne!(a, b);
        // A reserved, still empty file is no result yet.
        let long_ago = SystemTime::UNIX_EPOCH;
        assert!(written_since(&first, long_ago).is_none());
        assert!(read(1, &first, None).unwrap_err().contains("no result yet"));
        std::fs::write(&first, "done\n").unwrap();
        assert!(written_since(&first, long_ago).is_some());
    }

    #[test]
    fn the_start_of_a_result_is_bounded_and_only_a_regular_file_is_read() {
        let dir = temp_dir("start");
        let path = dir.join("r.md");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(read_start(&path).unwrap(), ("one\ntwo".to_string(), false));
        // Cut at the line limit.
        let many: String = (0..START_LINES + 5).map(|n| format!("{n}\n")).collect();
        std::fs::write(&path, many).unwrap();
        let (text, cut) = read_start(&path).unwrap();
        assert!(cut);
        assert_eq!(text.lines().count(), START_LINES);
        // Cut at the byte limit, never inside a character.
        std::fs::write(&path, "あ".repeat(START_BYTES as usize)).unwrap();
        let (text, cut) = read_start(&path).unwrap();
        assert!(cut);
        assert!(text.len() as u64 <= START_BYTES);
        assert!(text.chars().all(|ch| ch == 'あ'));
        // A directory, a FIFO (which would block) and a link are refused.
        assert_eq!(read_start(&dir), Err(StartError::NotRegular));
        let fifo = dir.join("fifo");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: mkfifo reads the NUL-terminated path only.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert_eq!(read_start(&fifo), Err(StartError::NotRegular));
        let link = dir.join("link.md");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(read_start(&link), Err(StartError::NotRegular));
        assert!(matches!(
            read_start(&dir.join("missing.md")),
            Err(StartError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_path_is_printed_whether_the_file_exists_is_empty_or_is_missing() {
        let dir = temp_dir("write-path");
        let full = dir.join("full.md");
        std::fs::write(&full, "done\n").unwrap();
        let empty = prepare(&dir, Some("empty")).unwrap();
        let missing = dir.join("never-created.md");
        for path in [&full, &empty, &missing] {
            let mut out = Vec::new();
            write_path(&mut out, path).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("{}\n", path.display())
            );
        }
        assert!(!missing.exists());
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "done\n");
        assert_eq!(std::fs::read(&empty).unwrap(), b"");
    }
}
