//! Private files, durable generations, and one process-wide writer lease.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{Artifact, MAX_CONTEXT_BYTES, MAX_STATE_BYTES, SCHEMA_VERSION, State};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn name(value: &OsStr) -> Result<CString, String> {
    CString::new(value.as_bytes()).map_err(|_| "保存名にNULがあります".into())
}

pub(crate) fn directory(path: &Path, create: bool) -> Result<File, String> {
    if !path.is_absolute() {
        return Err("保存先には絶対パスが必要です".into());
    }
    let mut current = File::open("/").map_err(|e| e.to_string())?;
    for component in path.components() {
        let component = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(s) => s,
            _ => return Err("親ディレクトリを含む保存先は使えません".into()),
        };
        let part = name(component)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        // SAFETY: the descriptor is retained while resolving this one component.
        let mut fd = unsafe { libc::openat(current.as_raw_fd(), part.as_ptr(), flags) };
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            // SAFETY: the name is a single NUL-terminated component.
            if unsafe { libc::mkdirat(current.as_raw_fd(), part.as_ptr(), 0o700) } < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            current.sync_all().map_err(|e| e.to_string())?;
            // SAFETY: links introduced during creation are refused here too.
            fd = unsafe { libc::openat(current.as_raw_fd(), part.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned an owned descriptor.
        current = unsafe { File::from_raw_fd(fd) };
    }
    Ok(current)
}

pub fn private_directory(path: &Path) -> Result<(), String> {
    let file = directory(path, true)?;
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no arguments and does not access user memory.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("保存先の所有者が現在のユーザーではありません".into());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())
}

pub fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let parent = path.parent().ok_or("読込先が不正です")?;
    let dir = directory(parent, false)?;
    let filename = name(path.file_name().ok_or("読込名が不正です")?)?;
    // SAFETY: a single leaf is opened beneath the retained directory.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            filename.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.to_string());
    }
    // SAFETY: openat returned an owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("通常ファイルまたはサイズ上限を確認してください".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("ファイルが読込中にサイズ上限を超えました".into());
    }
    Ok(Some(bytes))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("保存先が不正です")?;
    let dir = directory(parent, true)?;
    let filename = name(path.file_name().ok_or("保存名が不正です")?)?;
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err("通常ファイルでない保存先は上書きしません".into());
    }
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let temp = CString::new(format!(
        ".ember-{}-{}-{}.tmp",
        std::process::id(),
        crate::model::now(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ))
    .map_err(|e| e.to_string())?;
    // SAFETY: exclusive creation under an already opened directory.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: openat returned an owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // SAFETY: both names are sibling leaves under the retained directory.
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                dir.as_raw_fd(),
                filename.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        dir.sync_all().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        // SAFETY: only this call's uniquely created temporary leaf is removed.
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0);
        }
    }
    result
}

pub struct Lease(File);

impl Lease {
    pub fn acquire(path: &Path) -> Result<Option<Self>, String> {
        let parent = directory(path.parent().ok_or("ロック先が不正です")?, true)?;
        let filename = name(path.file_name().ok_or("ロック名が不正です")?)?;
        // SAFETY: a link-refusing leaf under an opened parent directory.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                filename.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned an owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("実行ロックが通常ファイルではありません".into());
        }
        loop {
            // SAFETY: the live file is kept for the lifetime of this lease.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Some(Self(file)));
            }
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return if e.kind() == std::io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(e.to_string())
            };
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // SAFETY: releasing only the lock this value owns.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    revision: u64,
    snapshot: String,
    digest: String,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        private_directory(&root)?;
        private_directory(&root.join("artifacts"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self) -> Result<State, String> {
        let Some(bytes) = read(&self.root.join("state.json"), 16 * 1024)? else {
            // An interrupted first commit may leave an orphan generation. It
            // is preserved for inspection rather than silently reset to empty.
            if std::fs::read_dir(&self.root)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with("snapshot-"))
            {
                return Err(
                    "保存 manifest がありません。残っている世代を上書きせず保持しました".into(),
                );
            }
            return Ok(State::default());
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| "保存 manifest が壊れています")?;
        if manifest.schema_version != SCHEMA_VERSION
            || manifest.snapshot
                != format!("snapshot-{}-{}.json", manifest.revision, manifest.digest)
            || manifest.digest.len() != 64
            || !manifest.digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("保存 manifest の形式が不正です".into());
        }
        let bytes = read(&self.root.join(&manifest.snapshot), MAX_STATE_BYTES)?
            .ok_or("保存 manifest が参照するデータがありません。元データを保持しています")?;
        if digest(&bytes) != manifest.digest {
            return Err("保存データのハッシュが一致しません".into());
        }
        let state: State =
            serde_json::from_slice(&bytes).map_err(|_| "保存データのJSONが不正です")?;
        if state.revision != manifest.revision {
            return Err("保存データの版が一致しません".into());
        }
        state.validate()?;
        for run in &state.runs {
            if let Some(artifact) = &run.artifact {
                self.artifact(artifact)?;
            }
        }
        Ok(state)
    }

    pub fn commit(&self, expected: u64, state: &State) -> Result<(), String> {
        state.validate()?;
        if self.load()?.revision != expected
            || state.revision != expected.checked_add(1).ok_or("保存版の上限です")?
        {
            return Err("保存版が更新されました。再接続してください".into());
        }
        let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err("保存容量の上限です。履歴を整理してください".into());
        }
        let hash = digest(&bytes);
        let snapshot = format!("snapshot-{}-{hash}.json", state.revision);
        // Each object and its directory entry are durable before the manifest
        // can refer to it. atomic_write also syncs the manifest's parent.
        atomic_write(&self.root.join(&snapshot), &bytes)?;
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            revision: state.revision,
            snapshot: snapshot.clone(),
            digest: hash,
        };
        atomic_write(
            &self.root.join("state.json"),
            &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
        )?;
        self.collect(&snapshot, state)?;
        Ok(())
    }

    fn collect(&self, current: &str, state: &State) -> Result<(), String> {
        for entry in std::fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("snapshot-") && name.ends_with(".json") && name != current {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        let referenced: std::collections::HashSet<_> = state
            .runs
            .iter()
            .filter_map(|r| r.artifact.as_ref().map(|a| a.path.as_str()))
            .collect();
        for entry in std::fs::read_dir(self.root.join("artifacts")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let relative = format!("artifacts/{}", entry.file_name().to_string_lossy());
            if !referenced.contains(relative.as_str())
                && entry.file_type().map_err(|e| e.to_string())?.is_file()
            {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        directory(&self.root.join("artifacts"), false)?
            .sync_all()
            .map_err(|e| e.to_string())?;
        let handoffs = self.root.join("handoffs");
        if handoffs.exists() {
            let dir = directory(&handoffs, false)?;
            for entry in std::fs::read_dir(&handoffs).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if !state
                    .runs
                    .iter()
                    .any(|r| name.starts_with(&format!("{}-", r.id)))
                    && entry.file_type().map_err(|e| e.to_string())?.is_file()
                {
                    std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                }
            }
            dir.sync_all().map_err(|e| e.to_string())?;
        }
        directory(&self.root, false)?
            .sync_all()
            .map_err(|e| e.to_string())
    }

    pub fn write_artifact(
        &self,
        run_id: &str,
        title: &str,
        text: &str,
    ) -> Result<Artifact, String> {
        if !crate::model::valid_id(run_id)
            || text.trim().is_empty()
            || text.len() > MAX_CONTEXT_BYTES
        {
            return Err("成果物の識別子・内容・容量を確認してください".into());
        }
        let path = format!("artifacts/{run_id}.md");
        atomic_write(&self.root.join(&path), text.as_bytes())?;
        Ok(Artifact {
            path,
            title: title.chars().take(100).collect(),
            digest: digest(text.as_bytes()),
        })
    }

    pub fn artifact(&self, artifact: &Artifact) -> Result<String, String> {
        let Some(id) = artifact
            .path
            .strip_prefix("artifacts/")
            .and_then(|s| s.strip_suffix(".md"))
        else {
            return Err("成果物のパスが不正です".into());
        };
        if !crate::model::valid_id(id) {
            return Err("成果物の識別子が不正です".into());
        }
        let bytes = read(&self.root.join(&artifact.path), MAX_CONTEXT_BYTES)?
            .ok_or("成果物がありません")?;
        if digest(&bytes) != artifact.digest {
            return Err("成果物が保存時から変更されています".into());
        }
        String::from_utf8(bytes).map_err(|_| "成果物がUTF-8ではありません".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn durable_generation_rejects_corruption_and_stale_writers() {
        let temp = root();
        let store = Store::open(temp.path().canonicalize().unwrap()).unwrap();
        let mut state = State {
            revision: 1,
            ..State::default()
        };
        store.commit(0, &state).unwrap();
        assert_eq!(store.load().unwrap().revision, 1);
        assert!(store.commit(0, &state).is_err());
        let m: Manifest =
            serde_json::from_slice(&std::fs::read(temp.path().join("state.json")).unwrap())
                .unwrap();
        std::fs::write(temp.path().join(m.snapshot), b"{}").unwrap();
        assert!(store.load().is_err());
        state.revision = 2;
        assert!(store.commit(1, &state).is_err());
    }

    #[test]
    fn manifest_cannot_follow_links_or_missing_objects() {
        let temp = root();
        let store = Store::open(temp.path().canonicalize().unwrap()).unwrap();
        std::fs::write(temp.path().join("other"), b"{}").unwrap();
        std::os::unix::fs::symlink(temp.path().join("other"), temp.path().join("state.json"))
            .unwrap();
        assert!(store.load().is_err());
        assert!(atomic_write(&temp.path().join("state.json"), b"{}").is_err());
    }

    #[test]
    fn lease_and_private_permissions_are_enforced() {
        let temp = root();
        let path = temp.path().canonicalize().unwrap();
        let _store = Store::open(path.clone()).unwrap();
        let first = Lease::acquire(&path.join("service.lock")).unwrap().unwrap();
        assert!(
            Lease::acquire(&path.join("service.lock"))
                .unwrap()
                .is_none()
        );
        drop(first);
        assert!(
            Lease::acquire(&path.join("service.lock"))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
