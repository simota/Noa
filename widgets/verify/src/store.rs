//! Verifications on disk (L1-10):
//!
//! ```text
//! <data dir>/noa/verifications/<id>/
//!   events.jsonl  report.md
//! <repo>/.noa/verify/<id>/         the judge's reply dir
//!   c/criteria.json  r<N>/judge.json  fix-<n>.md
//!   target/                        manifest.json, diff.patch, files/, worktree.patch, untracked/, spec.md
//!   evidence/r<N>/<verifier>/      ingested logs and files, rerun-<n>.log; evidence/r<N>/manifest.md
//!   evidence/user/                 person-attached media
//! <main>-wt/verify-<id>-<verifier>/.noa/verify/<id>/r<N>/<verifier>/   a verifier's reply dir
//!   reply.json  logs/  shots/  video/  rerun/
//! ```
//!
//! In 共有 mode a verifier's `r<N>/<verifier>/` sits in the judge's reply dir.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::reduce::ReduceError;
use crate::model::types::{
    Decision, Event, Isolation, Millis, PartId, RoundNo, Session, SessionId, TargetKind,
};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const REPORT_FILE: &str = "report.md";
/// The snapshot, under the judge's reply dir.
pub const TARGET_DIR: &str = "target";
/// Ingested and attached evidence, under the judge's reply dir, beside
/// [`TARGET_DIR`].
pub const EVIDENCE_DIR: &str = "evidence";
/// Person-attached media, under [`EVIDENCE_DIR`].
pub const USER_EVIDENCE_DIR: &str = "user";
/// The criteria phase's directory, under the judge's reply dir.
pub const CRITERIA_DIR: &str = "c";
pub const CRITERIA_FILE: &str = "criteria.json";
pub const JUDGE_FILE: &str = "judge.json";
/// A verifier's reply, in its `r<N>/<verifier>/`.
pub const REPLY_FILE: &str = "reply.json";
/// Under a work tree's `.noa/`.
pub const VERIFY_DIR: &str = "verify";

/// The largest reply file read; a bigger one is turned down unread.
pub const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;

/// The most fix requests one verification keeps (`fix-1.md` …).
pub const MAX_FIX_TASKS: u32 = 999;

/// How long a lock found taken is waited for before the log counts as
/// running elsewhere (review's reason: a child being spawned holds a copy
/// of every open file until its exec).
pub const LOCK_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Where verifications live: `<data dir>/noa/verifications`, beside Noa's
/// own session state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("verifications"))
}

pub fn session_dir(root: &Path, id: &SessionId) -> PathBuf {
    root.join(id.as_str())
}

/// `<dir>/r<round>`.
pub fn round_dir(dir: &Path, round: RoundNo) -> PathBuf {
    dir.join(format!("r{round}"))
}

/// `<workdir>/.noa/verify/<id>`.
pub fn workdir_reply_dir(workdir: &Path, id: &SessionId) -> PathBuf {
    workdir.join(".noa").join(VERIFY_DIR).join(id.as_str())
}

/// A stable 64-bit FNV-1a digest of `text`, which a log may keep: what
/// identifies a reply file's content across runs.
pub fn digest(text: &str) -> u64 {
    crate::capture::digest_bytes(text.as_bytes())
}

/// The judge's reply dir: `<workdir>/.noa/verify/<id>`, which holds the
/// snapshot and the evidence too.
pub fn reply_dir(session: &Session) -> PathBuf {
    workdir_reply_dir(&session.workdir, &session.id)
}

/// What every path below the judge's reply dir is checked against: the
/// target's checkout (`workdir`). Nothing between it and a file may be a
/// link.
pub fn reply_base(session: &Session) -> PathBuf {
    session.workdir.clone()
}

/// Where verifier `part` runs: its worktree, under the main worktree's
/// `<main>-wt/`, or the target's checkout (`workdir`) in 共有 mode. Also
/// the base its reply paths are checked against.
pub fn verifier_root(session: &Session, part: &PartId) -> PathBuf {
    match session.isolation {
        Isolation::Worktree => crate::worktree::path_for(&session.main_worktree, &session.id, part),
        Isolation::Shared => session.workdir.clone(),
    }
}

/// Verifier `part`'s reply dir: `<its root>/.noa/verify/<id>`.
pub fn verifier_reply_dir(session: &Session, part: &PartId) -> PathBuf {
    workdir_reply_dir(&verifier_root(session, part), &session.id)
}

/// `<reply_dir>/r<round>/<part>`: what a verifier's reply cites paths
/// relative to.
pub fn part_dir(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(reply_dir, round).join(part.as_str())
}

/// The file verifier `part` writes its round-`round` JSON to, under its
/// reply dir.
pub fn reply_path(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    part_dir(reply_dir, round, part).join(REPLY_FILE)
}

/// `<part dir>/rerun`, under the verifier's reply dir.
pub fn rerun_dir(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    part_dir(reply_dir, round, part).join(crate::rerun::RERUN_DIR)
}

/// The file the judge writes its round-`round` ruling to.
pub fn judge_reply_path(reply_dir: &Path, round: RoundNo) -> PathBuf {
    round_dir(reply_dir, round).join(JUDGE_FILE)
}

/// The file the judge writes the criteria to.
pub fn criteria_path(reply_dir: &Path) -> PathBuf {
    reply_dir.join(CRITERIA_DIR).join(CRITERIA_FILE)
}

/// The snapshot directory under a reply dir.
pub fn target_dir(reply_dir: &Path) -> PathBuf {
    reply_dir.join(TARGET_DIR)
}

/// `<reply dir>/evidence`.
pub fn evidence_dir(reply_dir: &Path) -> PathBuf {
    reply_dir.join(EVIDENCE_DIR)
}

/// `<reply dir>/evidence/r<round>/<part>`: where `part`'s evidence of
/// `round` is ingested.
pub fn round_evidence_dir(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(&evidence_dir(reply_dir), round).join(part.as_str())
}

/// `<reply dir>/evidence/user`.
pub fn user_evidence_dir(reply_dir: &Path) -> PathBuf {
    evidence_dir(reply_dir).join(USER_EVIDENCE_DIR)
}

pub fn report_path(dir: &Path) -> PathBuf {
    dir.join(REPORT_FILE)
}

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, so the
/// verification's files never show in `git status`. A `.noa` (or anything
/// below it) that is a link is refused.
pub fn prepare_workdir(_workdir: &Path) -> io::Result<()> {
    todo!("U2: prepare_workdir (review's)")
}

/// Writes the report whole, through a temporary file created exclusively
/// beside it.
pub fn write_report(_dir: &Path, _text: &str) -> io::Result<PathBuf> {
    todo!("U2: write_report")
}

/// Whether `dir`'s stored report is not what its log gives.
pub fn report_is_stale(_dir: &Path, _session: &Session) -> bool {
    todo!("U2: report_is_stale")
}

/// Writes `dir`'s report again from its log, holding the log's lock.
pub fn rewrite_report(_dir: &Path) -> Result<(), StoreError> {
    todo!("U2: rewrite_report")
}

/// Writes `text` as the next free `fix-<n>.md` in the judge's reply dir
/// and returns its path; nothing is written over or through a link.
/// Callers check first that the log belongs to `dir`
/// ([`crate::engine::check_store_paths`]).
pub fn write_fix_task(_dir: &Path, _session: &Session, _text: &str) -> io::Result<PathBuf> {
    todo!("U2: write_fix_task")
}

/// Makes the folder a reply goes to, below `base`.
pub fn prepare_reply(_base: &Path, _path: &Path) -> io::Result<()> {
    todo!("U2: prepare_reply")
}

/// Removes a reply file; a link on the way to it is refused.
pub fn clear_reply(_base: &Path, _path: &Path) -> io::Result<()> {
    todo!("U2: clear_reply")
}

/// A reply file's bytes, or `None` while it is not there. A link on the
/// way, a file that is not a plain file and one over [`MAX_REPLY_BYTES`]
/// are errors.
pub fn read_reply(_base: &Path, _path: &Path) -> io::Result<Option<Vec<u8>>> {
    todo!("U2: read_reply")
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt {
        line: usize,
        error: String,
    },
    Reduce(ReduceError),
    /// Another runner (in this or another process) holds the log open.
    Busy,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Corrupt { line, error } => {
                write!(f, "{EVENTS_FILE} line {line} is damaged: {error}")
            }
            StoreError::Reduce(e) => write!(f, "{EVENTS_FILE} does not fold: {e}"),
            StoreError::Busy => f.write_str("別のウィンドウで実行中です"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// A verification's log, open and locked for appending.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    file: File,
}

impl Store {
    /// Creates `dir`'s log with its first batch, locked.
    pub fn create(_dir: &Path, _first: &[Event]) -> Result<Store, StoreError> {
        todo!("U2: Store::create")
    }

    /// Opens `dir`'s log to append to it, folding it into its session; a
    /// torn last line is cut off. The log stays locked while open.
    pub fn open(_dir: &Path) -> Result<(Store, Session), StoreError> {
        todo!("U2: Store::open")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn append(&mut self, _batch: &[Event]) -> io::Result<()> {
        let _ = &self.file;
        todo!("U2: Store::append")
    }

    pub fn append_all(&mut self, _batches: &[Vec<Event>]) -> io::Result<()> {
        todo!("U2: Store::append_all")
    }
}

/// Folds `dir`'s log without writing to it or taking its lock.
pub fn read_session(_dir: &Path) -> Result<Session, StoreError> {
    todo!("U2: read_session")
}

/// Whether a runner holds `dir`'s log open right now.
pub fn is_busy(_dir: &Path) -> bool {
    todo!("U2: is_busy")
}

/// One past verification under the store root, as the start view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub dir: PathBuf,
    pub id: SessionId,
    pub created_at: Millis,
    pub kind: TargetKind,
    pub label: String,
    pub decision: Option<Decision>,
    /// Criteria per latest ruling: verified, failed, inconclusive,
    /// not applicable, unruled (検証済 4 · 不合格 1 · 判定不能 1).
    pub rulings: [usize; 5],
    pub closed: bool,
    pub rounds: usize,
    pub max_rounds: RoundNo,
    /// A runner holds its log: it is running in some window.
    pub busy: bool,
    pub extendable: bool,
}

/// A directory under the store root and what reading its log gave.
#[derive(Debug, Clone)]
pub struct Listed {
    pub dir: PathBuf,
    pub summary: Result<Summary, String>,
    /// The log's stamp when it was read: a log whose stamp has moved on is
    /// read again.
    pub stamp: Option<LogStamp>,
}

/// When a log was last written and how long it was then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogStamp {
    pub modified: std::time::SystemTime,
    pub len: u64,
}

/// `dir`'s log stamp, or `None` when it cannot be read.
pub fn log_stamp(_dir: &Path) -> Option<LogStamp> {
    todo!("U2: log_stamp")
}

/// `dir` listed: its log read now.
pub fn listed(_dir: &Path) -> Listed {
    todo!("U2: listed")
}

/// The newest `limit` verifications under `root`, newest first.
pub fn list_sessions(_root: &Path, _limit: usize) -> Vec<Listed> {
    todo!("U2: list_sessions")
}

/// The newest `count` verifications under `root` and how many it holds;
/// an unchanged row of `shown` is kept.
pub fn page_sessions(_root: &Path, _shown: &[Listed], _count: usize) -> (Vec<Listed>, usize) {
    todo!("U2: page_sessions")
}

/// How many verifications `root` holds (no log is read).
pub fn count_sessions(_root: &Path) -> usize {
    todo!("U2: count_sessions")
}

impl Summary {
    /// `session` (logged in `dir`) as listed, before its lock is looked at.
    pub fn of(_dir: &Path, _session: &Session) -> Summary {
        todo!("U2: Summary::of")
    }
}
