//! One atomic, checksummed snapshot per repository. The lock covers reading the
//! current version and replacement, so stale windows cannot overwrite changes.

use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::engine::validate_run;
use crate::model::{Run, SCHEMA_VERSION, WorkflowDefinition, digest_bytes};

const STATE_FILE: &str = "workflow.json";
const LOCK_FILE: &str = "workflow.lock";
const MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredWorkflow {
    pub schema_version: u32,
    pub version: u64,
    pub definition: WorkflowDefinition,
    pub run: Option<Run>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: StoredWorkflow,
    checksum: String,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Busy,
    Conflict { expected: u64, actual: u64 },
    Corrupt(String),
    Invalid(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "ワークフローを保存・読込できません: {e}"),
            Self::Busy => f.write_str("別の処理がワークフローを更新しています"),
            Self::Conflict { expected, actual } => write!(
                f,
                "保存後に更新がありました（表示版 {expected}、現在版 {actual}）。再読込してください"
            ),
            Self::Corrupt(message) => write!(
                f,
                "保存データを確認できません。上書きせず保持しました: {message}"
            ),
            Self::Invalid(message) => write!(f, "保存する内容が無効です: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    directory: File,
}

struct LockGuard(File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        // A concurrent process spawn can briefly inherit this open file
        // description before exec closes it. Explicit unlock prevents that
        // child from accidentally prolonging the parent's critical section.
        loop {
            // SAFETY: the descriptor remains owned by this guard during drop.
            if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } == 0 {
                break;
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

pub fn default_root(repository: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(repository).ok()?;
    let key = digest_bytes(path.as_os_str().as_encoded_bytes());
    Some(dirs::data_dir()?.join("noa").join("workflows").join(key))
}

fn c_name(name: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(name.as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

fn open_at(
    directory: &File,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
    // openat is variadic: mode_t is u16 on macOS, but the mode argument
    // must use a C integer type after default argument promotion.
    mode: libc::c_uint,
) -> io::Result<File> {
    let name = c_name(name)?;
    // SAFETY: the directory and C string live through openat; the returned
    // descriptor is owned exactly once by the File below.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "store directory must be absolute",
        ));
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                match open_at(&current, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                    Ok(next) => current = next,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let name_c = c_name(name)?;
                        // SAFETY: mkdirat uses the live directory descriptor and
                        // a NUL-terminated component, never an unchecked path.
                        if unsafe { libc::mkdirat(current.as_raw_fd(), name_c.as_ptr(), 0o700) }
                            != 0
                        {
                            let error = io::Error::last_os_error();
                            if error.kind() != io::ErrorKind::AlreadyExists {
                                return Err(error);
                            }
                        }
                        current.sync_all()?;
                        current = open_at(&current, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "store path contains a parent or unsupported prefix",
                ));
            }
        }
    }
    Ok(current)
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, StoreError> {
        let directory = open_directory(&root)?;
        Ok(Self { root, directory })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self, exclusive: bool) -> Result<LockGuard, StoreError> {
        let file = open_at(
            &self.directory,
            std::ffi::OsStr::new(LOCK_FILE),
            libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK,
            0o600,
        )?;
        if !file.metadata()?.is_file() {
            return Err(StoreError::Corrupt(
                "ロックが通常のファイルではありません".into(),
            ));
        }
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        } | libc::LOCK_NB;
        loop {
            // SAFETY: flock acts on a live file owned by this scope. Closing it
            // releases the lock, including every error return below.
            if unsafe { libc::flock(file.as_raw_fd(), mode) } == 0 {
                return Ok(LockGuard(file));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::Busy
            } else {
                StoreError::Io(error)
            });
        }
    }

    fn read_locked(&self) -> Result<Option<StoredWorkflow>, StoreError> {
        let file = match open_at(
            &self.directory,
            std::ffi::OsStr::new(STATE_FILE),
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StoreError::Io(error)),
        };
        if !file.metadata()?.is_file() {
            return Err(StoreError::Corrupt(
                "保存先が通常のファイルではありません".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::Corrupt(
                "保存データがサイズ上限を超えています".into(),
            ));
        }
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let encoded = serde_json::to_vec(&envelope.payload)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if digest_bytes(&encoded) != envelope.checksum {
            return Err(StoreError::Corrupt("内容のハッシュが一致しません".into()));
        }
        check_payload(&envelope.payload).map_err(StoreError::Corrupt)?;
        Ok(Some(envelope.payload))
    }

    pub fn load(&self) -> Result<Option<StoredWorkflow>, StoreError> {
        let _lock = self.lock(false)?;
        self.read_locked()
    }

    /// A display-only reader observes one immutable inode without taking the
    /// writer's lock. Atomic replacement makes that generation complete; the
    /// checksum and semantic checks still run. It may be an older generation,
    /// so callers must use load/save with a version when acquiring ownership.
    pub fn observe(&self) -> Result<Option<StoredWorkflow>, StoreError> {
        self.read_locked()
    }

    /// `expected_version` is the store version, not the draft revision. An
    /// incomplete draft may be saved; executable definitions are checked when
    /// a run is created. Invalid existing bytes are never replaced.
    pub fn save(
        &self,
        expected_version: u64,
        definition: &WorkflowDefinition,
        run: Option<&Run>,
    ) -> Result<StoredWorkflow, StoreError> {
        let _lock = self.lock(true)?;
        let existing = self.read_locked()?;
        let actual = existing.as_ref().map_or(0, |s| s.version);
        if expected_version != actual {
            return Err(StoreError::Conflict {
                expected: expected_version,
                actual,
            });
        }
        let mut definition = definition.clone();
        if let Some(previous) = &existing {
            let mut compared = definition.clone();
            compared.revision = previous.definition.revision;
            definition.revision = if compared == previous.definition {
                previous.definition.revision
            } else {
                previous
                    .definition
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Invalid("定義の版の上限です".into()))?
            };
        } else {
            definition.revision = 1;
        }
        if let (Some(old), Some(new)) = (existing.as_ref().and_then(|s| s.run.as_ref()), run)
            && old.id == new.id
            && (old.definition_digest != new.definition_digest
                || old.repository != new.repository
                || new.seq < old.seq)
        {
            return Err(StoreError::Invalid(
                "実行中の定義や実行履歴を巻き戻せません".into(),
            ));
        }
        let payload = StoredWorkflow {
            schema_version: SCHEMA_VERSION,
            version: actual
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("保存版の上限です".into()))?,
            definition,
            run: run.cloned(),
        };
        check_payload(&payload).map_err(StoreError::Invalid)?;
        let encoded =
            serde_json::to_vec(&payload).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let envelope = Envelope {
            checksum: digest_bytes(&encoded),
            payload: payload.clone(),
        };
        let bytes =
            serde_json::to_vec_pretty(&envelope).map_err(|e| StoreError::Invalid(e.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::Invalid("保存データのサイズ上限です".into()));
        }
        self.replace(&bytes)?;
        Ok(payload)
    }

    fn replace(&self, bytes: &[u8]) -> Result<(), StoreError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temp = format!(
            ".workflow-{}-{}-{}.tmp",
            std::process::id(),
            nonce,
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let temp_name = std::ffi::OsStr::new(&temp);
        let mut file = open_at(
            &self.directory,
            temp_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let result = (|| -> io::Result<()> {
            file.write_all(bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            let from = c_name(temp_name)?;
            let to = c_name(std::ffi::OsStr::new(STATE_FILE))?;
            // SAFETY: both names are components under the same open directory.
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    from.as_ptr(),
                    self.directory.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            self.directory.sync_all()
        })();
        if result.is_err()
            && let Ok(name) = c_name(temp_name)
        {
            // SAFETY: removes only this call's exclusively created sibling.
            let _ = unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) };
        }
        result.map_err(StoreError::Io)
    }
}

fn check_payload(payload: &StoredWorkflow) -> Result<(), String> {
    if payload.schema_version != SCHEMA_VERSION
        || payload.version == 0
        || payload.definition.schema_version != SCHEMA_VERSION
        || payload.definition.revision == 0
    {
        return Err("未対応の形式または保存版です".into());
    }
    if payload.definition.nodes.len() > crate::model::MAX_NODES
        || payload.definition.edges.len() > crate::model::MAX_EDGES
        || payload
            .definition
            .nodes
            .iter()
            .any(|n| !n.position.x.is_finite() || !n.position.y.is_finite())
    {
        return Err("工程数・接続数・位置が保存できる範囲を超えています".into());
    }
    if let Some(run) = &payload.run {
        validate_run(run).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("workflow");
        let store = Store::open(root).unwrap();
        (temp, store)
    }

    #[test]
    fn stale_window_cannot_overwrite_a_newer_definition() {
        let (_temp, store) = fixture();
        let first = store
            .save(0, &WorkflowDefinition::standard(), None)
            .unwrap();
        let other = Store::open(store.root().to_owned()).unwrap();
        let mut changed = first.definition.clone();
        changed.nodes[0].position.x += 40.0;
        let second = other.save(first.version, &changed, None).unwrap();
        assert_eq!(second.definition.revision, 2);
        assert!(matches!(
            store.save(first.version, &first.definition, None),
            Err(StoreError::Conflict { .. })
        ));
        assert_eq!(store.load().unwrap().unwrap(), second);
    }

    #[test]
    fn incomplete_drafts_are_saved_without_becoming_executable() {
        let (_temp, store) = fixture();
        let mut definition = WorkflowDefinition::standard();
        definition.edges.clear();
        assert!(!definition.validate().is_empty());
        let saved = store.save(0, &definition, None).unwrap();
        assert_eq!(store.load().unwrap().unwrap().definition, saved.definition);
    }

    #[test]
    fn truncated_and_checksum_changed_state_are_never_overwritten() {
        let (_temp, store) = fixture();
        store
            .save(0, &WorkflowDefinition::standard(), None)
            .unwrap();
        let path = store.root().join(STATE_FILE);
        let original = std::fs::read(&path).unwrap();
        let broken = &original[..original.len() / 2];
        std::fs::write(&path, broken).unwrap();
        assert!(store.load().is_err());
        assert!(
            store
                .save(1, &WorkflowDefinition::standard(), None)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        let changed = String::from_utf8(original)
            .unwrap()
            .replace("開発サイクル", "改ざんされた名前");
        std::fs::write(&path, changed.as_bytes()).unwrap();
        assert!(matches!(store.load(), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn symlinked_state_does_not_touch_the_target() {
        use std::os::unix::fs::symlink;
        let (temp, store) = fixture();
        let target = temp.path().join("outside");
        std::fs::write(&target, b"keep me").unwrap();
        symlink(&target, store.root().join(STATE_FILE)).unwrap();
        assert!(
            store
                .save(0, &WorkflowDefinition::standard(), None)
                .is_err()
        );
        assert_eq!(std::fs::read(target).unwrap(), b"keep me");
    }

    #[test]
    fn exclusive_lock_is_observed_by_other_windows() {
        let (_temp, store) = fixture();
        let other = Store::open(store.root().to_owned()).unwrap();
        let _lock = store.lock(true).unwrap();
        assert!(matches!(other.load(), Err(StoreError::Busy)));
        assert!(matches!(
            other.save(0, &WorkflowDefinition::standard(), None),
            Err(StoreError::Busy)
        ));
    }

    #[test]
    fn display_observers_do_not_block_a_writer_or_require_its_lock() {
        let (_temp, store) = fixture();
        let saved = store
            .save(0, &WorkflowDefinition::standard(), None)
            .unwrap();
        let other = Store::open(store.root().to_owned()).unwrap();
        let _lock = store.lock(true).unwrap();
        assert_eq!(other.observe().unwrap(), Some(saved));
        assert!(matches!(other.load(), Err(StoreError::Busy)));
    }
}
