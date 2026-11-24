//! Verifications on disk (L1-10):
//!
//! ```text
//! <data dir>/noa/verifications/<id>/
//!   events.jsonl  report.md
//! <repo>/.noa/verify/<id>/         the judge's reply dir
//!   c/criteria.json  r<N>/judge.json  fix-<n>.md
//!   target/                        manifest.json, diff.patch, files/, worktree.patch, untracked/, spec.md
//!   evidence/r<N>/<verifier>/      ingested logs and files; evidence/r<N>/manifest.md
//!   evidence/r<N>/<verifier>.rerun/  the widget's re-run logs, <n>.log
//!   evidence/user/                 person-attached media
//! <main>-wt/verify-<id>-<verifier>/.noa/verify/<id>/r<N>/<verifier>/   a verifier's reply dir
//!   reply.json  logs/  shots/  video/  rerun/
//! ```
//!
//! In 共有 mode a verifier's `r<N>/<verifier>/` sits in the judge's reply dir.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fsx;
use crate::model::export;
use crate::model::reduce::{self, ReduceError};
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
/// Appended to a verifier's id to name the directory its re-run logs are
/// ingested to, beside the one its reported evidence is ingested to
/// (`evidence/r<N>/<verifier>.rerun/`). A reported path is relative with no
/// `..`, so it stays inside `<verifier>/`, and a verifier id is an agent
/// kind with an optional `-<n>` ([`crate::engine::participants`]), never
/// holding a `.`: nothing a verifier reports can be stored there.
pub const RERUN_EVIDENCE_SUFFIX: &str = ".rerun";
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

/// `<reply dir>/evidence/r<round>/<part>.rerun`: where the logs of
/// `part`'s round-`round` re-runs are ingested, apart from
/// [`round_evidence_dir`] ([`RERUN_EVIDENCE_SUFFIX`]).
pub fn rerun_evidence_dir(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(&evidence_dir(reply_dir), round).join(format!("{part}{RERUN_EVIDENCE_SUFFIX}"))
}

/// `<reply dir>/evidence/r<round>/<part>.rerun/<n>.log`: the stored log of
/// re-run `n`. With an empty `reply_dir` it is the path a [`Stored`] of it
/// records (relative to the reply dir).
///
/// [`Stored`]: crate::model::types::Stored
pub fn rerun_log_path(reply_dir: &Path, round: RoundNo, part: &PartId, n: u32) -> PathBuf {
    rerun_evidence_dir(reply_dir, round, part).join(format!("{n}.log"))
}

/// `<reply dir>/evidence/user`.
pub fn user_evidence_dir(reply_dir: &Path) -> PathBuf {
    evidence_dir(reply_dir).join(USER_EVIDENCE_DIR)
}

pub fn report_path(dir: &Path) -> PathBuf {
    dir.join(REPORT_FILE)
}

/// The line [`prepare_workdir`] adds to a `.noa/.gitignore` it did not
/// write: this widget's folder, relative to `.noa/`.
const VERIFY_IGNORE_LINE: &str = "/verify/";

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, so the
/// verification's files never show in `git status`. A `.noa/.gitignore`
/// already there is kept; when git does not ignore `.noa/verify/` with it,
/// [`VERIFY_IGNORE_LINE`] is appended, and a folder git still does not
/// ignore refuses the start with the reason. A `.noa` (or anything below
/// it) that is a link is refused.
pub fn prepare_workdir(workdir: &Path) -> io::Result<()> {
    let dir = workdir.join(".noa");
    fsx::create_dirs(workdir, &dir.join(VERIFY_DIR))?;
    let gitignore = dir.join(".gitignore");
    // `create_new` does not follow a link left at the name.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&gitignore)
    {
        Ok(mut file) => return file.write_all(b"*\n"),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    if verify_dir_ignored(workdir)? {
        return Ok(());
    }
    append_ignore_line(&gitignore)?;
    if verify_dir_ignored(workdir)? {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} を書き足しても .noa/verify/ が Git に無視されません。検証の証跡が Git に含まれるおそれがあるため開始しません",
        gitignore.display()
    )))
}

/// Whether git ignores `<workdir>/.noa/verify/` (`git check-ignore`, run
/// without the person's `GIT_*` variables).
fn verify_dir_ignored(workdir: &Path) -> io::Result<bool> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(workdir)
        .args(["check-ignore", "-q", "--"])
        .arg(Path::new(".noa").join(VERIFY_DIR))
        .stdin(Stdio::null());
    without_git_env(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| io::Error::other(format!("git を起動できません: {e}")))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(io::Error::other(format!(
            ".noa/verify/ が Git に無視されているか確かめられないため開始しません: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Appends [`VERIFY_IGNORE_LINE`] to `gitignore`, on a line of its own,
/// keeping every line it has. A link or anything but a plain file is
/// refused.
fn append_ignore_line(gitignore: &Path) -> io::Result<()> {
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    let file = OpenOptions::new()
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
    let line = format!(
        "{}{VERIFY_IGNORE_LINE}\n",
        if open_line { "\n" } else { "" }
    );
    (&file).write_all(line.as_bytes())
}

/// Clears every `GIT_*` variable `cmd` would inherit, so git finds the
/// repository from its directory alone, not from the person's environment
/// (a `GIT_DIR` or `GIT_WORK_TREE` set in their shell, say).
fn without_git_env(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
}

/// Writes the report whole, through a temporary file created exclusively
/// beside it.
pub fn write_report(dir: &Path, text: &str) -> io::Result<PathBuf> {
    let path = report_path(dir);
    fsx::write_replace(dir, &path, text.as_bytes())?;
    Ok(path)
}

/// The report `session` gives: media linked by absolute path into its
/// judge's reply dir (L1-8).
pub fn report_text(session: &Session) -> String {
    export::report_markdown(session, Some(&reply_dir(session)))
}

/// Whether `dir`'s stored report is not what its log gives (missing, or
/// the verification stopped between writing its log and its report). One
/// whose judge has had no output accepted (not even the criteria) has no
/// report to compare.
pub fn report_is_stale(dir: &Path, session: &Session) -> bool {
    let has_report = session.closed
        || session.criteria_accepted
        || session.rounds.iter().any(|r| r.outcome.is_some());
    has_report && fs::read_to_string(report_path(dir)).ok() != Some(report_text(session))
}

/// Writes `dir`'s report again from its log, holding the log's lock.
pub fn rewrite_report(dir: &Path) -> Result<(), StoreError> {
    let (store, session) = Store::open(dir)?;
    write_report(dir, &report_text(&session))?;
    drop(store);
    Ok(())
}

/// Writes `text` as the next free `fix-<n>.md` in the judge's reply dir
/// and returns its path; nothing is written over or through a link.
/// Callers check first that the log belongs to `dir`
/// ([`crate::engine::check_store_paths`]).
pub fn write_fix_task(dir: &Path, session: &Session, text: &str) -> io::Result<PathBuf> {
    let id = Path::new(session.id.as_str());
    let plain = matches!(
        id.components().collect::<Vec<_>>().as_slice(),
        [std::path::Component::Normal(_)]
    );
    if !plain || dir.file_name() != Some(std::ffi::OsStr::new(session.id.as_str())) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("検証 ID {} はこのフォルダの名前になりません", session.id),
        ));
    }
    let base = reply_base(session);
    let reply = reply_dir(session);
    let folder = fsx::open_dir(&base, &reply, true)?;
    for n in 1..=MAX_FIX_TASKS {
        let name = format!("fix-{n}.md");
        match fsx::write_new_at(&folder, std::ffi::OsStr::new(&name), text.as_bytes()) {
            Ok(()) => return Ok(reply.join(name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "{} に修正の依頼が {MAX_FIX_TASKS} 件あります",
        reply.display()
    )))
}

/// Makes the folder a reply goes to, below `base`.
pub fn prepare_reply(base: &Path, path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fsx::create_dirs(base, parent),
        None => Ok(()),
    }
}

/// Removes a reply file; a link on the way to it is refused.
pub fn clear_reply(base: &Path, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fsx::check_no_links(base, parent)?;
    }
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// A reply file's bytes, or `None` while it is not there. A link on the
/// way, a file that is not a plain file and one over [`MAX_REPLY_BYTES`]
/// are errors.
pub fn read_reply(base: &Path, path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match fsx::open_read(base, path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let (bytes, over) = fsx::read_capped(file, MAX_REPLY_BYTES)?;
    if over {
        return Err(io::Error::other(format!(
            "返答が大きすぎます（{} MiB 超）",
            MAX_REPLY_BYTES / (1024 * 1024)
        )));
    }
    Ok(Some(bytes))
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

fn line(batch: &[Event]) -> io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(batch).map_err(io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

impl Store {
    /// Creates `dir`'s log with its first batch, locked.
    pub fn create(dir: &Path, first: &[Event]) -> Result<Store, StoreError> {
        reduce::create(first).map_err(StoreError::Reduce)?;
        fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join(EVENTS_FILE))?;
        lock(&file)?;
        let mut store = Store {
            dir: dir.to_owned(),
            file,
        };
        store.append(first)?;
        Ok(store)
    }

    /// Opens `dir`'s log to append to it, folding it into its session; a
    /// torn last line is cut off. The log stays locked while open.
    pub fn open(dir: &Path) -> Result<(Store, Session), StoreError> {
        let path = dir.join(EVENTS_FILE);
        let mut file = OpenOptions::new().append(true).open(&path)?;
        lock(&file)?;
        let text = fs::read(&path)?;
        let parsed = parse(&text)?;
        let session = reduce::replay(&parsed.batches).map_err(StoreError::Reduce)?;
        if parsed.torn {
            if parsed.kept < text.len() {
                file.set_len(parsed.kept as u64)?;
            } else {
                file.write_all(b"\n")?;
            }
        }
        Ok((
            Store {
                dir: dir.to_owned(),
                file,
            },
            session,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn append(&mut self, batch: &[Event]) -> io::Result<()> {
        self.file.write_all(&line(batch)?)
    }

    pub fn append_all(&mut self, batches: &[Vec<Event>]) -> io::Result<()> {
        for batch in batches {
            self.append(batch)?;
        }
        Ok(())
    }
}

const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// Takes the log's exclusive lock, held until the file is closed.
fn lock(file: &File) -> Result<(), StoreError> {
    let until = std::time::Instant::now() + LOCK_GRACE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < until => {
                std::thread::sleep(LOCK_POLL);
            }
            Err(fs::TryLockError::WouldBlock) => return Err(StoreError::Busy),
            Err(fs::TryLockError::Error(e)) => return Err(StoreError::Io(e)),
        }
    }
}

struct Parsed {
    batches: Vec<Vec<Event>>,
    /// Bytes up to the end of the last whole line.
    kept: usize,
    /// Whether the last line was cut short.
    torn: bool,
}

fn parse(text: &[u8]) -> Result<Parsed, StoreError> {
    let mut batches = Vec::new();
    let mut kept = 0;
    let mut offset = 0;
    let mut torn = false;
    for (n, raw) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        let complete = raw.ends_with(b"\n");
        offset += raw.len();
        if raw.trim_ascii().is_empty() {
            kept = offset;
            continue;
        }
        match serde_json::from_slice::<Vec<Event>>(raw) {
            Ok(batch) => {
                batches.push(batch);
                kept = offset;
                torn = !complete;
            }
            Err(_) if !complete => torn = true,
            Err(error) => {
                return Err(StoreError::Corrupt {
                    line: n + 1,
                    error: error.to_string(),
                });
            }
        }
    }
    Ok(Parsed {
        batches,
        kept,
        torn,
    })
}

/// Folds `dir`'s log without writing to it or taking its lock.
pub fn read_session(dir: &Path) -> Result<Session, StoreError> {
    let text = fs::read(dir.join(EVENTS_FILE))?;
    let parsed = parse(&text)?;
    reduce::replay(&parsed.batches).map_err(StoreError::Reduce)
}

/// Whether a runner holds `dir`'s log open right now.
pub fn is_busy(dir: &Path) -> bool {
    File::open(dir.join(EVENTS_FILE))
        .is_ok_and(|file| matches!(file.try_lock_shared(), Err(fs::TryLockError::WouldBlock)))
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
pub fn log_stamp(dir: &Path) -> Option<LogStamp> {
    let meta = fs::metadata(dir.join(EVENTS_FILE)).ok()?;
    Some(LogStamp {
        modified: meta.modified().ok()?,
        len: meta.len(),
    })
}

/// `dir` listed: its log read now (stamped first, so a write during the
/// read shows as a change next time).
pub fn listed(dir: &Path) -> Listed {
    let stamp = log_stamp(dir);
    let summary = read_session(dir)
        .map(|session| Summary {
            busy: is_busy(dir),
            ..Summary::of(dir, &session)
        })
        .map_err(|e| e.to_string());
    Listed {
        dir: dir.to_owned(),
        summary,
        stamp,
    }
}

/// The newest `limit` verifications under `root`, newest first.
pub fn list_sessions(root: &Path, limit: usize) -> Vec<Listed> {
    page_sessions(root, &[], limit).0
}

/// The newest `count` verifications under `root` and how many it holds;
/// an unchanged row of `shown` is kept.
pub fn page_sessions(root: &Path, shown: &[Listed], count: usize) -> (Vec<Listed>, usize) {
    let dirs = session_dirs(root);
    let total = dirs.len();
    let rows = dirs
        .into_iter()
        .take(count)
        .map(|dir| match shown.iter().find(|l| l.dir == dir) {
            // Only a summary that was read is kept; a row that failed is
            // tried again (a log made readable keeps its stamp).
            Some(old)
                if old.summary.is_ok() && old.stamp.is_some() && old.stamp == log_stamp(&dir) =>
            {
                let mut row = old.clone();
                if let Ok(summary) = &mut row.summary {
                    summary.busy = is_busy(&dir);
                }
                row
            }
            _ => listed(&dir),
        })
        .collect();
    (rows, total)
}

/// How many verifications `root` holds (no log is read).
pub fn count_sessions(root: &Path) -> usize {
    session_dirs(root).len()
}

/// The verification folders under `root` (those with a log), newest first.
fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<((Millis, u32), PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|dir| dir.join(EVENTS_FILE).is_file())
        .map(|dir| {
            let order = dir
                .file_name()
                .map(|name| id_order(&name.to_string_lossy()))
                .unwrap_or_default();
            (order, dir)
        })
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs.into_iter().map(|(_, dir)| dir).collect()
}

/// `ver-<millis>` or `ver-<millis>-<n>` as `(millis, n)`, for ordering.
fn id_order(name: &str) -> (Millis, u32) {
    let rest = name.strip_prefix("ver-").unwrap_or_default();
    let (at, n) = rest.split_once('-').unwrap_or((rest, "0"));
    (at.parse().unwrap_or(0), n.parse().unwrap_or(0))
}

impl Summary {
    /// `session` (logged in `dir`) as listed, before its lock is looked at.
    pub fn of(dir: &Path, session: &Session) -> Summary {
        Summary {
            dir: dir.to_owned(),
            id: session.id.clone(),
            created_at: session.created_at,
            kind: session.target.kind,
            label: session.manifest.label.clone(),
            decision: session.verdict().map(|v| v.decision),
            rulings: session.ruling_counts(),
            closed: session.closed,
            rounds: session.rounds.len(),
            max_rounds: session.max_rounds,
            busy: false,
            extendable: session.extendable().is_ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-verify-store-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// `git -C dir <args>`'s exit status, run without the test's `GIT_*`
    /// variables.
    fn git(dir: &Path, args: &[&str]) -> i32 {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        without_git_env(&mut cmd);
        cmd.status().unwrap().code().unwrap()
    }

    #[test]
    fn an_existing_noa_gitignore_gets_the_verify_folder_ignored_and_keeps_its_lines() {
        let repo = temp_root("ignore-repo");
        fs::create_dir_all(repo.join(".noa")).unwrap();
        assert_eq!(git(&repo, &["init", "-q"]), 0);
        let gitignore = repo.join(".noa/.gitignore");
        // The person's own file, its last line without a newline.
        fs::write(&gitignore, "logs/").unwrap();
        prepare_workdir(&repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "logs/\n/verify/\n");
        let shot = repo.join(".noa/verify/ver-1/evidence/user/1.png");
        fs::create_dir_all(shot.parent().unwrap()).unwrap();
        fs::write(&shot, "png").unwrap();
        assert_eq!(
            git(
                &repo,
                &[
                    "check-ignore",
                    "-q",
                    ".noa/verify/ver-1/evidence/user/1.png"
                ]
            ),
            0
        );
        // Ignored now: the next start leaves the file as it is.
        prepare_workdir(&repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "logs/\n/verify/\n");
        // A file that already ignores the folder is not touched.
        fs::write(&gitignore, "*\n").unwrap();
        prepare_workdir(&repo).unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "*\n");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn a_start_is_refused_when_git_does_not_confirm_the_verify_folder_is_ignored() {
        let dir = temp_root("ignore-nogit");
        fs::create_dir_all(dir.join(".noa")).unwrap();
        let gitignore = dir.join(".noa/.gitignore");
        fs::write(&gitignore, "logs/\n").unwrap();
        assert_eq!(
            git(&dir, &["rev-parse", "--git-dir"]),
            128,
            "not a repository"
        );
        let err = prepare_workdir(&dir).unwrap_err();
        assert!(err.to_string().contains(".noa/verify/"), "{err}");
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "logs/\n");
        let _ = fs::remove_dir_all(dir);
    }

    /// A first batch for a verification `id` created at `at`.
    fn created(id: &str, at: Millis) -> Vec<Event> {
        let rig = testkit::Rig::new(2);
        let mut log = rig.log[0].clone();
        if let Some(Event::Created {
            id: own, at: when, ..
        }) = log.first_mut()
        {
            *own = SessionId::new(id);
            *when = at;
        }
        log
    }

    #[test]
    fn rerun_logs_are_stored_where_no_reported_evidence_can_be() {
        let part = PartId::new("claude");
        let reported = round_evidence_dir(Path::new(""), 1, &part);
        let rerun = rerun_log_path(Path::new(""), 1, &part, 1);
        assert_eq!(rerun, Path::new("evidence/r1/claude.rerun/1.log"));
        assert_eq!(
            rerun_log_path(Path::new("/r"), 2, &PartId::new("codex-2"), 3),
            Path::new("/r/evidence/r2/codex-2.rerun/3.log")
        );
        // A verifier's own `file` evidence named like a re-run log stays
        // under its own directory.
        for cited in ["rerun-1.log", "1.log", "rerun/1.log"] {
            let path = reported.join(cited);
            assert_ne!(path, rerun, "{cited}");
            assert!(!path.starts_with(rerun_evidence_dir(Path::new(""), 1, &part)));
        }
        assert!(!rerun.starts_with(&reported));
    }

    #[test]
    fn a_second_open_of_a_held_log_is_busy_and_a_read_still_works() {
        let root = temp_root("busy");
        let dir = root.join("ver-1");
        let store = Store::create(&dir, &created("ver-1", 1)).unwrap();
        assert!(is_busy(&dir));
        assert!(matches!(Store::open(&dir), Err(StoreError::Busy)));
        assert_eq!(read_session(&dir).unwrap().id.as_str(), "ver-1");
        drop(store);
        assert!(!is_busy(&dir));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_torn_last_line_is_cut_on_open() {
        let root = temp_root("torn");
        let dir = root.join("ver-1");
        drop(Store::create(&dir, &created("ver-1", 1)).unwrap());
        let path = dir.join(EVENTS_FILE);
        let whole = fs::read(&path).unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"[{"event":"hold_set","ho"#).unwrap();
        drop(file);
        let (mut store, session) = Store::open(&dir).unwrap();
        assert!(!session.hold);
        assert_eq!(fs::read(&path).unwrap(), whole);
        store.append(&[Event::HoldSet { hold: true }]).unwrap();
        drop(store);
        assert!(read_session(&dir).unwrap().hold);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sessions_list_newest_first_and_an_unreadable_log_says_why() {
        let root = temp_root("list");
        for (id, at) in [("ver-5", 5), ("ver-20", 20), ("ver-20-2", 21)] {
            drop(Store::create(&root.join(id), &created(id, at)).unwrap());
        }
        fs::create_dir_all(root.join("ver-30")).unwrap();
        fs::write(root.join("ver-30").join(EVENTS_FILE), "not json\n").unwrap();
        let listed = list_sessions(&root, 10);
        let names: Vec<String> = listed
            .iter()
            .map(|l| l.dir.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["ver-30", "ver-20-2", "ver-20", "ver-5"]);
        assert!(listed[0].summary.as_ref().unwrap_err().contains("line 1"));
        let s = listed[1].summary.as_ref().unwrap();
        assert_eq!((s.rounds, s.closed, s.rulings), (0, false, [0; 5]));
        assert_eq!(list_sessions(&root, 2).len(), 2);
        assert_eq!(count_sessions(&root), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_report_is_replaced_whole_and_never_through_a_link() {
        use std::os::unix::fs::symlink;
        let root = temp_root("report");
        let victim = temp_root("report-victim");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("v"), "keep").unwrap();
        symlink(victim.join("v"), root.join(format!(".{REPORT_FILE}.tmp"))).unwrap();
        write_report(&root, "one").unwrap();
        write_report(&root, "two").unwrap();
        assert_eq!(fs::read_to_string(report_path(&root)).unwrap(), "two");
        assert_eq!(fs::read_to_string(victim.join("v")).unwrap(), "keep");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(victim);
    }

    #[test]
    fn replies_are_read_cleared_and_prepared_only_below_their_base_without_links() {
        use std::os::unix::fs::symlink;
        let base = temp_root("reply-base");
        let elsewhere = temp_root("reply-elsewhere");
        let dir = base.join(".noa/verify/ver-1");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("a.json"), "{}").unwrap();
        symlink(&elsewhere, dir.join("r1")).unwrap();
        let path = dir.join("r1/a.json");
        assert!(fsx::is_link_error(&read_reply(&base, &path).unwrap_err()));
        assert!(clear_reply(&base, &path).is_err());
        assert!(elsewhere.join("a.json").exists());
        assert!(prepare_reply(&base, &dir.join("r1/sub/b.json")).is_err());
        assert!(!elsewhere.join("sub").exists());
        let work = temp_root("reply-work");
        fs::create_dir_all(&work).unwrap();
        symlink(&elsewhere, work.join(".noa")).unwrap();
        assert!(prepare_workdir(&work).is_err());
        assert!(!elsewhere.join(".gitignore").exists());
        let fresh = temp_root("reply-fresh");
        fs::create_dir_all(&fresh).unwrap();
        prepare_workdir(&fresh).unwrap();
        assert_eq!(
            fs::read_to_string(fresh.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        assert!(fresh.join(".noa/verify").is_dir());
        fs::create_dir_all(dir.join("r2")).unwrap();
        fs::write(dir.join("r2/a.json"), "{}").unwrap();
        assert_eq!(
            read_reply(&base, &dir.join("r2/a.json")).unwrap().unwrap(),
            b"{}"
        );
        assert_eq!(read_reply(&base, &dir.join("r2/none.json")).unwrap(), None);
        let big = File::create(dir.join("r2/big.json")).unwrap();
        big.set_len(MAX_REPLY_BYTES + 1).unwrap();
        assert!(read_reply(&base, &dir.join("r2/big.json")).is_err());
        for d in [base, elsewhere, work, fresh] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_report_is_stale_until_rewritten_and_fix_tasks_take_the_next_free_name() {
        let root = temp_root("stale");
        let mut rig = testkit::Rig::in_workdir(2, &root.join("repo"));
        fs::create_dir_all(root.join("repo")).unwrap();
        testkit::through_round_one(&mut rig);
        let dir = root.join("data").join(rig.engine.session().id.as_str());
        let mut store = Store::create(&dir, &rig.log[0]).unwrap();
        store.append_all(&rig.log[1..]).unwrap();
        drop(store);
        let session = rig.engine.session();
        assert!(report_is_stale(&dir, session), "missing");
        rewrite_report(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(report_path(&dir)).unwrap(),
            report_text(session)
        );
        assert!(!report_is_stale(&dir, session));
        let (held, _) = Store::open(&dir).unwrap();
        assert!(matches!(rewrite_report(&dir), Err(StoreError::Busy)));
        drop(held);
        let first = write_fix_task(&dir, session, "one").unwrap();
        let second = write_fix_task(&dir, session, "two").unwrap();
        assert_eq!(first, reply_dir(session).join("fix-1.md"));
        assert_eq!(second, reply_dir(session).join("fix-2.md"));
        assert_eq!(fs::read_to_string(first).unwrap(), "one");
        assert!(write_fix_task(&root.join("other"), session, "x").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_report_is_due_once_the_judges_criteria_are_accepted() {
        let root = temp_root("criteria");
        let mut rig = testkit::Rig::in_workdir(2, &root.join("repo"));
        testkit::accept_default_criteria(&mut rig);
        let dir = root.join("data").join(rig.engine.session().id.as_str());
        let mut store = Store::create(&dir, &rig.log[0]).unwrap();
        store.append_all(&rig.log[1..]).unwrap();
        drop(store);
        let session = rig.engine.session();
        assert!(session.rounds.is_empty(), "held for confirmation");
        assert!(report_is_stale(&dir, session), "missing");
        rewrite_report(&dir).unwrap();
        assert!(!report_is_stale(&dir, session));
        let _ = fs::remove_dir_all(root);
    }
}
