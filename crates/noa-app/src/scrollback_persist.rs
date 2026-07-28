//! On-disk store for persisted scrollback snapshots, and the worker that
//! writes them off the main thread.
//!
//! Spec: `docs/specs/scrollback-persistence.md`. Ghostty has no analog — it
//! restores window topology but never terminal contents.
//!
//! ## Why this is not the session-state writer
//!
//! `session_persist::SessionPersister` writes `session.json` on *every*
//! structural change (new tab, split, close). Snapshots cannot share that
//! trigger: a megabyte of terminal output per pane is not something to
//! re-encode each time a split moves. They are written on clean quit and on
//! idle checkpoints instead, which is why this is a second worker with its own
//! queue rather than another field on the session state.
//!
//! ## Storage posture
//!
//! Scrollback routinely contains material that has never touched a disk
//! before — exported credentials, tokens echoed by CLIs, remote URLs with
//! embedded PATs. Persisting it changes what a stolen laptop yields, which is
//! why the feature is opt-in and why the baseline here is not negotiable:
//! the directory is `0700`, files are `0600`, and the directory is excluded
//! from Time Machine so a record cannot leak onward through a backup. Nothing
//! is created at all while `scrollback-persist = never`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot file extension. Files that do not carry it are left alone by the
/// collector — this directory is noa's, but being destructive on a path the
/// user could have put something else in is not worth the tidiness.
const EXTENSION: &str = "nsb";

/// Length of a snapshot key, in hex characters.
const KEY_LEN: usize = 16;

/// Seconds in a day, for `scrollback-persist-max-age-days`.
const SECONDS_PER_DAY: u64 = 86_400;

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Whether `key` is a well-formed snapshot key.
///
/// Keys arrive from `session.json`, which is a plain file the user (or
/// anything running as them) can edit. They are interpolated into a path, so
/// an unvalidated key is a path-traversal primitive: `../../../.ssh/id_rsa`
/// would make the collector delete, and the writer overwrite, an arbitrary
/// file. Exactly [`KEY_LEN`] lowercase hex characters can express neither a
/// separator nor a `..`.
pub fn is_valid_key(key: &str) -> bool {
    key.len() == KEY_LEN && key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Mint a fresh snapshot key. Uniqueness only has to hold against the other
/// keys alive in this session and the ones left on disk by previous runs, so
/// the wall clock mixed with a per-process counter is sufficient; the
/// collector removes anything a live session does not claim anyway.
pub fn mint_key(counter: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    // Cheap 64-bit mix (splitmix64 finalizer) so consecutive keys do not share
    // a long common prefix, which would make them annoying to tell apart in a
    // directory listing.
    let mut z = nanos.wrapping_add(counter.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    format!("{z:016x}")
}

pub fn snapshot_path(dir: &Path, key: &str) -> Option<PathBuf> {
    is_valid_key(key).then(|| dir.join(format!("{key}.{EXTENSION}")))
}

/// Create the snapshot directory `0700` and mark it as backup-excluded.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    let existed = dir.is_dir();
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    if !existed {
        exclude_from_backup(dir);
    }
    Ok(())
}

/// Ask Foundation to keep `dir` out of Time Machine and iCloud backups.
///
/// Best-effort: a failure here means the records are backed up like any other
/// file in Application Support, which is the status quo for every other app,
/// not a reason to refuse to persist.
#[cfg(target_os = "macos")]
fn exclude_from_backup(dir: &Path) {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsExcludedFromBackupKey};

    let Some(path) = dir.to_str() else {
        return;
    };
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let value = NSNumber::numberWithBool(true);
        let _ = url.setResourceValue_forKey_error(Some(&value), NSURLIsExcludedFromBackupKey);
    }
}

#[cfg(not(target_os = "macos"))]
fn exclude_from_backup(_dir: &Path) {}

/// Write `bytes` to `key`'s snapshot file, atomically and `0600`.
pub fn write(dir: &Path, key: &str, bytes: &[u8]) -> io::Result<()> {
    let Some(path) = snapshot_path(dir, key) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid scrollback snapshot key",
        ));
    };
    ensure_dir(dir)?;

    let temp = path.with_extension("tmp");
    {
        use io::Write as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    // `create` honors the mode only when it actually creates the file; a
    // leftover temp from a crashed write would keep its old permissions.
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temp, &path)
}

/// Read `key`'s snapshot. `None` for a missing, unreadable, or invalid key —
/// a snapshot is a convenience, and a bad one degrades to "no record".
pub fn read(dir: &Path, key: &str) -> Option<Vec<u8>> {
    fs::read(snapshot_path(dir, key)?).ok()
}

pub fn remove(dir: &Path, key: &str) {
    if let Some(path) = snapshot_path(dir, key) {
        let _ = fs::remove_file(path);
    }
}

/// One snapshot file the collector is considering.
struct Entry {
    path: PathBuf,
    key: String,
    modified: u64,
    size: u64,
}

fn list(dir: &Path) -> Vec<Entry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(EXTENSION) {
            continue;
        }
        let Some(key) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !is_valid_key(key) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        out.push(Entry {
            key: key.to_string(),
            path,
            modified,
            size: meta.len(),
        });
    }
    out
}

/// Drop snapshots that no live session claims, that have expired, or that push
/// the directory over its total budget. Run at launch, before restore, so a
/// record the user asked to expire is never shown and then deleted.
///
/// `max_age_days == 0` disables expiry; `total_limit == 0` keeps nothing.
pub fn collect(dir: &Path, referenced: &HashSet<String>, total_limit: u64, max_age_days: u64) {
    let mut entries = list(dir);
    let now = now_unix();

    entries.retain(|entry| {
        let orphaned = !referenced.contains(&entry.key);
        let expired = max_age_days > 0
            && now.saturating_sub(entry.modified) > max_age_days.saturating_mul(SECONDS_PER_DAY);
        if orphaned || expired {
            let _ = fs::remove_file(&entry.path);
            return false;
        }
        true
    });

    let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
    if total <= total_limit {
        return;
    }
    // Oldest first: the pane a user has not touched in the longest is the one
    // whose record they are least likely to be coming back for.
    entries.sort_by_key(|entry| entry.modified);
    for entry in &entries {
        if total <= total_limit {
            break;
        }
        if fs::remove_file(&entry.path).is_ok() {
            total = total.saturating_sub(entry.size);
        }
    }
}

// ---------------------------------------------------------------------------
// Off-main-thread writer
// ---------------------------------------------------------------------------

enum Job {
    Write { key: String, bytes: Vec<u8> },
    Remove { key: String },
}

/// Serializes snapshot writes onto one background thread.
///
/// Capture has to happen on the main thread (it reads the shared `Terminal`),
/// but the encode result is just bytes, so the disk write moves here. A burst
/// — a checkpoint firing across ten panes, or quit capturing all of them —
/// coalesces per key: only the newest bytes for a pane are ever written.
pub struct ScrollbackPersister {
    tx: Option<crossbeam_channel::Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl ScrollbackPersister {
    pub fn spawn(dir: PathBuf) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        let worker_dir = dir;
        let worker = std::thread::Builder::new()
            .name("scrollback-persist".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // Coalesce whatever else is queued, keeping the newest
                    // action per key.
                    let mut pending: HashMap<String, Job> = HashMap::new();
                    let key = match &job {
                        Job::Write { key, .. } | Job::Remove { key } => key.clone(),
                    };
                    pending.insert(key, job);
                    while let Ok(next) = rx.try_recv() {
                        let key = match &next {
                            Job::Write { key, .. } | Job::Remove { key } => key.clone(),
                        };
                        pending.insert(key, next);
                    }
                    for job in pending.into_values() {
                        match job {
                            Job::Write { key, bytes } => {
                                if let Err(err) = write(&worker_dir, &key, &bytes) {
                                    log::warn!("failed to save scrollback snapshot {key}: {err}");
                                }
                            }
                            Job::Remove { key } => remove(&worker_dir, &key),
                        }
                    }
                }
            })
            .expect("failed to spawn the scrollback-persist thread");
        Self {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Queue `bytes` for `key`. Never blocks.
    pub fn save(&self, key: String, bytes: Vec<u8>) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(Job::Write { key, bytes });
        }
    }

    /// Queue deletion of `key`'s snapshot. Never blocks.
    pub fn discard(&self, key: String) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(Job::Remove { key });
        }
    }

    /// Flush everything queued and stop the worker. Later calls are ignored.
    pub fn flush(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ScrollbackPersister {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "noa-scrollback-{name}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_key_that_could_escape_the_directory_is_rejected() {
        // session.json is user-writable, so these are reachable inputs.
        for key in [
            "../../../etc/passwd",
            "..",
            "/etc/passwd",
            "a/b",
            "0123456789abcde",   // too short
            "0123456789abcdef0", // too long
            "0123456789ABCDEF",  // uppercase
            "0123456789abcdeg",  // not hex
            "",
        ] {
            assert!(!is_valid_key(key), "{key:?} must be rejected");
            assert!(snapshot_path(Path::new("/tmp"), key).is_none(), "{key:?}");
        }
        assert!(is_valid_key(&mint_key(0)));
        assert!(is_valid_key("0123456789abcdef"));
    }

    #[test]
    fn writing_an_invalid_key_fails_instead_of_touching_a_path() {
        let dir = temp_dir("invalid-key");
        assert!(write(&dir, "../escape", b"x").is_err());
        assert!(!dir.exists(), "a rejected key must not even create the dir");
    }

    #[test]
    fn minted_keys_are_distinct() {
        let keys: HashSet<String> = (0..1000).map(mint_key).collect();
        assert_eq!(keys.len(), 1000);
    }

    #[test]
    fn a_snapshot_roundtrips_through_the_directory_with_locked_down_modes() {
        let dir = temp_dir("roundtrip");
        let key = mint_key(1);
        write(&dir, &key, b"payload").expect("write succeeds");

        assert_eq!(read(&dir, &key).as_deref(), Some(&b"payload"[..]));
        let file_mode = fs::metadata(snapshot_path(&dir, &key).unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "snapshots must not be world-readable");
        assert_eq!(dir_mode, 0o700);

        remove(&dir, &key);
        assert!(read(&dir, &key).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_a_missing_snapshot_is_none_not_an_error() {
        let dir = temp_dir("missing");
        assert!(read(&dir, &mint_key(2)).is_none());
    }

    #[test]
    fn the_collector_drops_snapshots_no_session_claims() {
        let dir = temp_dir("orphans");
        let kept = mint_key(3);
        let orphan = mint_key(4);
        write(&dir, &kept, b"kept").unwrap();
        write(&dir, &orphan, b"orphan").unwrap();

        collect(&dir, &HashSet::from([kept.clone()]), u64::MAX, 0);

        assert!(read(&dir, &kept).is_some());
        assert!(read(&dir, &orphan).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_collector_enforces_the_total_budget_oldest_first() {
        let dir = temp_dir("budget");
        let old = mint_key(5);
        let new = mint_key(6);
        write(&dir, &old, &vec![0u8; 400]).unwrap();
        write(&dir, &new, &vec![0u8; 400]).unwrap();
        // Backdate `old` so the ordering is deterministic rather than relying
        // on two writes landing in different filesystem timestamp ticks.
        let old_path = snapshot_path(&dir, &old).unwrap();
        let stale = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::open(&old_path)
            .unwrap()
            .set_modified(stale)
            .unwrap();

        let referenced = HashSet::from([old.clone(), new.clone()]);
        collect(&dir, &referenced, 500, 0);

        assert!(read(&dir, &old).is_none(), "the oldest goes first");
        assert!(read(&dir, &new).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_collector_expires_by_age_and_zero_days_never_expires() {
        let dir = temp_dir("age");
        let key = mint_key(7);
        write(&dir, &key, b"old").unwrap();
        let stale = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::open(snapshot_path(&dir, &key).unwrap())
            .unwrap()
            .set_modified(stale)
            .unwrap();
        let referenced = HashSet::from([key.clone()]);

        collect(&dir, &referenced, u64::MAX, 0);
        assert!(read(&dir, &key).is_some(), "0 days means never expire");

        collect(&dir, &referenced, u64::MAX, 7);
        assert!(read(&dir, &key).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_collector_leaves_foreign_files_alone() {
        let dir = temp_dir("foreign");
        ensure_dir(&dir).unwrap();
        let foreign = dir.join("notes.txt");
        fs::write(&foreign, b"not ours").unwrap();

        collect(&dir, &HashSet::new(), 0, 1);

        assert!(foreign.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bytes_written_by_the_store_decode_as_a_grid_snapshot() {
        // The store is format-agnostic, so nothing else pins the two halves
        // together: this is the seam where a framing change in `noa-grid`
        // would otherwise surface only as an empty record at runtime.
        use noa_core::GridSize;
        let mut terminal = noa_grid::Terminal::new(GridSize::new(40, 4));
        let mut stream = noa_vt::Stream::new();
        stream.feed(b"error: something broke\r\n", &mut terminal);
        let encoded = terminal
            .scrollback_snapshot_bytes(1 << 20, 1_700_000_000)
            .expect("a terminal with output encodes");

        let dir = temp_dir("seam");
        let key = mint_key(42);
        write(&dir, &key, &encoded).expect("write succeeds");

        let read_back = read(&dir, &key).expect("the file is there");
        let decoded = noa_grid::snapshot::decode(&read_back).expect("it decodes");
        assert_eq!(decoded.saved_at, 1_700_000_000);
        let text: String = decoded.rows[0].cells.iter().map(|cell| cell.ch).collect();
        assert!(text.starts_with("error: something broke"), "{text:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_the_persister_flushes_the_newest_bytes_per_key() {
        let dir = temp_dir("persister");
        let key = mint_key(8);
        let persister = ScrollbackPersister::spawn(dir.clone());
        for generation in 0..20u8 {
            persister.save(key.clone(), vec![generation; 4]);
        }
        drop(persister);

        assert_eq!(read(&dir, &key), Some(vec![19u8; 4]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_queued_discard_wins_over_an_earlier_write_for_the_same_key() {
        let dir = temp_dir("discard");
        let key = mint_key(9);
        let mut persister = ScrollbackPersister::spawn(dir.clone());
        persister.save(key.clone(), vec![1u8; 4]);
        persister.discard(key.clone());
        persister.flush();

        assert!(read(&dir, &key).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
