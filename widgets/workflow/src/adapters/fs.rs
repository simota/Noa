//! Bounded, link-refusing files for adapter requests and receipts.

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

pub const MAX_REPORT_BYTES: u64 = 1024 * 1024;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && value != "."
        && value != ".."
}

pub fn plain_dirs(path: &Path) -> Result<(), String> {
    open_directory(path, true)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn name(value: &std::ffi::OsStr) -> std::io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| std::io::Error::other("ファイル名にNULがあります"))
}

/// Keep a descriptor for every directory while descending. A component
/// renamed to a symlink between validation and use is never followed.
fn open_directory(path: &Path, create: bool) -> std::io::Result<File> {
    let mut directory = File::open(if path.is_absolute() { "/" } else { "." })?;
    for component in path.components() {
        let part = match component {
            Component::Normal(value) => name(value)?,
            Component::CurDir | Component::RootDir => continue,
            _ => {
                return Err(std::io::Error::other(
                    "親ディレクトリを含むパスは使用できません",
                ));
            }
        };
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: directory owns a live descriptor; part is NUL terminated.
        let mut descriptor = unsafe { libc::openat(directory.as_raw_fd(), part.as_ptr(), flags) };
        if descriptor < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            // SAFETY: creating one component relative to the retained parent.
            if unsafe { libc::mkdirat(directory.as_raw_fd(), part.as_ptr(), 0o700) } < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: as above; a concurrently inserted link is refused.
            descriptor = unsafe { libc::openat(directory.as_raw_fd(), part.as_ptr(), flags) };
        }
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

pub fn read(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    let parent = path.parent().ok_or("結果ファイルの保存先が不正です")?;
    let directory = match open_directory(parent, false) {
        Ok(directory) => directory,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let filename =
        name(path.file_name().ok_or("結果ファイル名が不正です")?).map_err(|e| e.to_string())?;
    // SAFETY: the opened parent cannot be redirected by a symlink swap.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            filename.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.to_string());
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max {
        return Err("結果は上限内の通常ファイルである必要があります".into());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("結果が読込中に上限を超えました".into());
    }
    Ok(Some(bytes))
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    read(path, MAX_REPORT_BYTES)?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} のJSONが不正です: {e}", path.display()))
        })
        .transpose()
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    atomic_write(path, &bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("保存先に親ディレクトリがありません")?;
    let directory = open_directory(parent, true).map_err(|e| e.to_string())?;
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err("通常ファイルでない保存先は置き換えません".into());
    }
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("保存名が不正です")?;
    let filename = CString::new(name).map_err(|e| e.to_string())?;
    let temp =
        CString::new(format!(".{name}.{}.tmp", std::process::id())).map_err(|e| e.to_string())?;
    // SAFETY: both names are single components under the retained parent.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: openat returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // SAFETY: rename only the sibling temporary file, without reopening
        // an ancestor that a worker could have replaced.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temp.as_ptr(),
                directory.as_raw_fd(),
                filename.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        directory.sync_all().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        // SAFETY: cleanup names only this call's temporary sibling.
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temp.as_ptr(), 0);
        }
    }
    result
}

/// A runner owns this lease until its thread exits. A second window can
/// never dispatch a side effect from the same repository concurrently.
pub struct Lease(File);

impl Lease {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        Self::try_acquire(path)?
            .ok_or_else(|| "このリポジトリのワークフローは別の実行サービスが処理中です".into())
    }

    /// Only an actual flock conflict becomes a read-only observer. Invalid
    /// paths and permission errors must not silently weaken ownership.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>, String> {
        let parent = path.parent().ok_or("ロック先が不正です")?;
        let directory = open_directory(parent, true).map_err(|e| e.to_string())?;
        let filename =
            name(path.file_name().ok_or("ロック名が不正です")?).map_err(|e| e.to_string())?;
        // SAFETY: a single leaf under an opened, link-refusing parent.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                filename.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("実行ロックが通常ファイルではありません".into());
        }
        loop {
            // SAFETY: the descriptor belongs to this lease and remains open.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Some(Self(file)));
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(format!("実行ロックを確認できません: {error}")),
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: unlocking the descriptor owned by this lease.
            if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } == 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn path_identifiers_cannot_escape_their_directory() {
        for bad in ["", ".", "..", "../run", "/run", "node/a", "node\\a"] {
            assert!(!valid_id(bad));
        }
        assert!(valid_id("run-1700000000.node_2"));
    }

    #[test]
    fn reports_refuse_a_symlink_and_a_second_runner() {
        let root = std::env::temp_dir().join(format!("noa-workflow-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        fs::write(root.join("real"), b"{}").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("report")).unwrap();
        assert!(read(&root.join("report"), 50).is_err());
        let lease = Lease::acquire(&root.join("runner.lock")).unwrap();
        assert!(Lease::acquire(&root.join("runner.lock")).is_err());
        drop(lease);
        assert!(Lease::acquire(&root.join("runner.lock")).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
