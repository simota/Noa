//! Crash- and race-safe whole-file replacement: write to a sibling staging
//! file, `sync_all`, then `rename` over the target, so a reader never sees a
//! truncated file and a crash mid-write leaves the previous good file.
//!
//! The staging name is unique per process *and* per call (`create_new`), so
//! two noa processes saving the same path at once can never share one
//! staging file — the shared `path.with_extension("tmp")` a store used to
//! pick let the loser's open descriptor rewrite the winner's already
//! published file and then fail its own rename, leaving "save failed" in the
//! UI with the failed content on disk (session store B03, favorites N05).
//! Which of two concurrent whole-file saves lands last is still unordered;
//! only corruption and phantom failures are ruled out here.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Replace the file at `path` with `bytes`, creating the parent directory.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let (tmp, mut file) = loop {
        let tmp = staging_path(path);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            // A crashed process with a reused PID may have left this exact
            // name behind; the counter makes the next candidate fresh.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `.<name>.<pid>.<seq>.tmp` beside `path`, unique per process and call.
fn staging_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "noa-atomic-write-{tag}-{}-{}",
            std::process::id(),
            SEQ_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    static SEQ_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn staging_names_are_unique_per_call() {
        let path = Path::new("/some/dir/favorites");
        let a = staging_path(path);
        let b = staging_path(path);
        assert_ne!(a, b);
        assert_eq!(a.parent(), path.parent());
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".favorites.")
        );
        assert!(a.extension().is_some_and(|ext| ext == "tmp"));
    }

    #[test]
    fn write_replaces_the_file_and_leaves_no_staging_file_behind() {
        let dir = temp_dir("replace");
        let path = dir.join("nested").join("store");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "store")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
