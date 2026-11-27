//! Setup task files are the durable record of a hand-off. Recovering their
//! small headers makes observation failures and deleted tasks recoverable.

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use crate::scan::source::open_nofollow;
use crate::setup::Setup;
use crate::time::Millis;

const MAX_ENTRIES: usize = 4096;
const HEADER_BYTES: u64 = 8192;

/// This does not create directories or trust a possibly stale observation.
pub fn find_setup_task(repo: &Path, key: &str) -> Result<Option<(String, PathBuf)>, String> {
    for relative in [".noa", ".noa/tasks"] {
        match std::fs::symlink_metadata(repo.join(relative)) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return Err(format!("{relative} が通常のフォルダではありません")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        }
    }
    let dir = repo.join(".noa/tasks");
    let mut paths = Vec::new();
    for (count, entry) in std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if count == MAX_ENTRIES {
            return Err(
                "タスクフォルダの探索上限を超えています。既存タスクを確認してください".into(),
            );
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.len() > 5
            && name.starts_with("t-")
            && name.ends_with(".md")
            && !name.chars().any(char::is_control)
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    for path in paths {
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let file = match open_nofollow(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        file.take(HEADER_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 == HEADER_BYTES {
            let mut lines = bytes
                .split(|byte| *byte == b'\n')
                .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
            if lines.next() == Some(b"---".as_slice()) && !lines.any(|line| line == b"---") {
                return Err(format!(
                    "{} のヘッダーが読み取り上限を超えています",
                    path.display()
                ));
            }
        }
        if matching_header(&bytes, key) {
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            return Ok(Some((id, path)));
        }
    }
    Ok(None)
}

fn matching_header(bytes: &[u8], key: &str) -> bool {
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    if lines.next() != Some(b"---".as_slice()) {
        return false;
    }
    let mut matching = false;
    let mut title = false;
    for line in lines {
        if line == b"---" {
            return matching && title;
        }
        let Ok(line) = std::str::from_utf8(line) else {
            return false;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        let (name, value) = (name.trim(), value.trim());
        match name {
            "scaffold-setup" => matching = value == key,
            "title" => title = !value.is_empty(),
            // Tasks defaults a missing state to open and trims field values.
            "state" if !matches!(value, "open" | "closed") => return false,
            _ => {}
        }
    }
    false
}

fn lock(repo: &Path) -> Result<File, String> {
    let dir = super::tasks_dir(repo).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(".scaffold-setup.lock"))
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("セットアップタスクのロックが通常のファイルではありません".into());
    }
    // Each open owns a separate descriptor. Keep this inode in place so all
    // windows coordinate on the same lock; closing the file releases it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
            "別のウィンドウがタスクを作成しています。再スキャンして確認してください".into()
        } else {
            error.to_string()
        });
    }
    Ok(file)
}

/// Checks and creates under a short, nonblocking interprocess lock. The task
/// keeps the existing t-<millis> naming contract and is recoverable by its
/// front matter even when the optional observation cannot be recorded.
pub fn hand_off_setup(
    repo: &Path,
    setup: &Setup,
    now: Millis,
) -> Result<(String, PathBuf, bool), String> {
    let _lock = lock(repo)?;
    if let Some((id, path)) = find_setup_task(repo, &setup.task_key())? {
        return Ok((id, path, false));
    }
    let (id, path) =
        super::create_task(repo, &setup.task_text(), now).map_err(|e| e.to_string())?;
    Ok((id, path, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_is_reused_after_restart_and_recreated_after_deletion() {
        let repo = tempfile::tempdir().unwrap();
        let setup = crate::setup::scan(repo.path());
        assert!(
            find_setup_task(repo.path(), &setup.task_key())
                .unwrap()
                .is_none()
        );
        assert!(!repo.path().join(".noa").exists());
        let (id, path, created) = hand_off_setup(repo.path(), &setup, 1).unwrap();
        assert!(created);
        assert_eq!(id, "t-1");
        assert_eq!(
            hand_off_setup(repo.path(), &setup, 2).unwrap(),
            (id.clone(), path.clone(), false)
        );
        std::fs::remove_file(path).unwrap();
        assert!(
            find_setup_task(repo.path(), &setup.task_key())
                .unwrap()
                .is_none()
        );
        assert_eq!(hand_off_setup(repo.path(), &setup, 3).unwrap().0, "t-3");
    }

    #[test]
    fn a_second_window_cannot_pass_the_first_windows_lock() {
        let repo = tempfile::tempdir().unwrap();
        let setup = crate::setup::scan(repo.path());
        let first = lock(repo.path()).unwrap();
        assert!(
            hand_off_setup(repo.path(), &setup, 1)
                .unwrap_err()
                .contains("別のウィンドウ")
        );
        drop(first);
        assert!(hand_off_setup(repo.path(), &setup, 2).unwrap().2);
        assert!(!hand_off_setup(repo.path(), &setup, 3).unwrap().2);
    }

    #[test]
    fn headers_are_bounded_and_body_markers_do_not_match() {
        let repo = tempfile::tempdir().unwrap();
        let setup = crate::setup::scan(repo.path());
        let key = setup.task_key();
        let text = setup.task_text();
        assert!(matching_header(text.as_bytes(), &key));
        assert!(matching_header(text.replace('\n', "\r\n").as_bytes(), &key));
        assert!(matching_header(
            text.replace("state: open\n", "").as_bytes(),
            &key
        ));
        assert!(matching_header(
            text.replace("scaffold-setup: ", "scaffold-setup :\t")
                .as_bytes(),
            &key
        ));
        assert!(!matching_header(
            format!("---\ntitle: x\nstate: open\n---\nscaffold-setup: {key}\n").as_bytes(),
            &key
        ));
        assert!(!matching_header(
            format!("---\nscaffold-setup: {key}\n").as_bytes(),
            &key
        ));
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("task.md"), &text).unwrap();
        let dir = super::super::tasks_dir(repo.path()).unwrap();
        std::os::unix::fs::symlink(outside.path().join("task.md"), dir.join("t-linked.md"))
            .unwrap();
        assert!(find_setup_task(repo.path(), &key).unwrap().is_none());
        std::fs::write(dir.join("t-.md"), &text).unwrap();
        assert!(find_setup_task(repo.path(), &key).unwrap().is_none());
        std::fs::write(
            dir.join("t-oversized.md"),
            format!(
                "---\ntitle: {}\nscaffold-setup: {key}\n---\n",
                "x".repeat(HEADER_BYTES as usize)
            ),
        )
        .unwrap();
        assert!(
            find_setup_task(repo.path(), &key)
                .unwrap_err()
                .contains("読み取り上限")
        );
    }
}
