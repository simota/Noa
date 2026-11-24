//! Reviews on disk. One directory per review:
//!
//! ```text
//! <data-dir>/noa/reviews/<session id>/
//!   events.jsonl          one event batch per line
//!   report.md             the report as of the last accepted lead output
//!   target/, r<N>/        here only when the review has no workdir
//! <workdir>/.noa/reviews/<session id>/   (the reply dir, with a workdir)
//!   target/               the snapshot (manifest.json, diff.patch, files/, document.md)
//!   r<N>/<reviewer>.json  reviewer replies
//!   r<N>/lead.json        the lead's output
//!   fix-<n>.md            a fix request's task (修正を依頼)
//! ```

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::fsx;
use crate::model::export;
use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{Decision, Millis, PartId, RoundNo, Session, SessionId, TargetKind};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const LEAD_FILE: &str = "lead.json";
pub const REPORT_FILE: &str = "report.md";
pub const TARGET_DIR: &str = "target";

/// Where reviews live: `<data-dir>/noa/reviews`, beside Noa's own session
/// state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("reviews"))
}

pub fn session_dir(root: &Path, id: &SessionId) -> PathBuf {
    root.join(id.as_str())
}

pub fn round_dir(dir: &Path, round: RoundNo) -> PathBuf {
    dir.join(format!("r{round}"))
}

/// Where the snapshot and the reply files go when agents run in `workdir`:
/// `<workdir>/.noa/reviews/<id>`.
pub fn workdir_reply_dir(workdir: &Path, id: &SessionId) -> PathBuf {
    workdir.join(".noa").join("reviews").join(id.as_str())
}

/// A stable 64-bit FNV-1a digest of `text`, which a log may keep: what
/// identifies a reply file's content across runs.
pub fn digest(text: &str) -> u64 {
    crate::capture::digest_bytes(text.as_bytes())
}

/// The root of `session`'s snapshot and reply files: its workdir's
/// `.noa/reviews/<id>` when it has one, else its store directory `dir`.
pub fn reply_dir(dir: &Path, session: &Session) -> PathBuf {
    match &session.workdir {
        Some(workdir) => workdir_reply_dir(workdir, &session.id),
        None => dir.to_owned(),
    }
}

/// The directory every path below `session`'s reply dir is checked
/// against: its workdir (the person's choice) or its store directory.
/// Nothing between it and a reply or snapshot file may be a link.
pub fn reply_base(dir: &Path, session: &Session) -> PathBuf {
    match &session.workdir {
        Some(workdir) => workdir.clone(),
        None => dir.to_owned(),
    }
}

/// The largest reply file read; a bigger one is turned down unread.
pub const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;

/// The snapshot directory under a reply dir.
pub fn target_dir(reply_dir: &Path) -> PathBuf {
    reply_dir.join(TARGET_DIR)
}

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, so review
/// snapshots and replies never show in `git status`.
/// A `.noa` (or anything below it) that is a link is refused.
pub fn prepare_workdir(workdir: &Path) -> io::Result<()> {
    let dir = workdir.join(".noa");
    fsx::create_dirs(workdir, &dir.join("reviews"))?;
    // `create_new` does not follow a link left at the name.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut file) => io::Write::write_all(&mut file, b"*\n"),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// The file reviewer `part` writes its round-`round` JSON to.
pub fn reply_path(dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(dir, round).join(format!("{part}.json"))
}

/// The file the lead writes its round-`round` JSON to.
pub fn lead_reply_path(dir: &Path, round: RoundNo) -> PathBuf {
    round_dir(dir, round).join(LEAD_FILE)
}

pub fn report_path(dir: &Path) -> PathBuf {
    dir.join(REPORT_FILE)
}

/// Writes the report whole, through a temporary file created exclusively
/// beside it, so a reader never sees half of it and a link left in the
/// directory is never written through.
pub fn write_report(dir: &Path, text: &str) -> io::Result<PathBuf> {
    let path = report_path(dir);
    fsx::write_replace(dir, &path, text.as_bytes())?;
    Ok(path)
}

/// Whether `dir`'s stored report is not what its log gives (missing, or
/// the review stopped between writing its log and its report). A review not
/// yet consolidated once has no report to compare.
pub fn report_is_stale(dir: &Path, session: &Session) -> bool {
    let has_report = session.closed || session.rounds.iter().any(|r| r.outcome.is_some());
    has_report
        && fs::read_to_string(report_path(dir)).ok() != Some(export::report_markdown(session))
}

/// Writes `dir`'s report again from its log, holding the log's lock while
/// it does (a runner that took the review up meanwhile is not written
/// under: the log is then [`StoreError::Busy`]).
pub fn rewrite_report(dir: &Path) -> Result<(), StoreError> {
    let (store, session) = Store::open(dir)?;
    write_report(dir, &export::report_markdown(&session))?;
    drop(store);
    Ok(())
}

/// Where an agent asked to fix `session`'s findings works: the workdir of
/// a git or files review (the tree the findings point into). A document
/// review has no tree to fix, and one without a workdir nowhere to work.
pub fn fix_workdir(session: &Session) -> Result<&Path, &'static str> {
    if session.target.kind == TargetKind::Document {
        return Err("文書のレビューでは修正を依頼できません（直す作業ツリーがありません）");
    }
    session
        .workdir
        .as_deref()
        .ok_or("修正の依頼には、作業ディレクトリのあるレビューが必要です")
}

/// The most fix requests one review keeps (`fix-1.md` … `fix-999.md`).
pub const MAX_FIX_TASKS: u32 = 999;

/// Writes `text` as the next free `fix-<n>.md` in `session`'s reply dir
/// (`dir` is its store directory) and returns its path. Nothing is written
/// over an earlier request, or through a link.
pub fn write_fix_task(dir: &Path, session: &Session, text: &str) -> io::Result<PathBuf> {
    let base = reply_base(dir, session);
    let reply = reply_dir(dir, session);
    fsx::create_dirs(&base, &reply)?;
    for n in 1..=MAX_FIX_TASKS {
        let path = reply.join(format!("fix-{n}.md"));
        match fsx::write_new(&base, &path, text.as_bytes()) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "{} に修正の依頼が {MAX_FIX_TASKS} 件あります",
        reply.display()
    )))
}

/// Makes the folder a reply goes to, below `base` ([`reply_base`]).
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

    /// Opens `dir`'s log to append to it, folding it into its session. A
    /// torn last line (a crash mid-write) is cut off. The log stays locked
    /// while the store is open, so two runners never append to one log.
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

/// How long a lock found taken is waited for before the log counts as
/// running elsewhere. A `flock` lock belongs to the open file, and a child
/// process being started on another thread holds a copy of every open file
/// of this process until its exec closes them: for that moment it holds
/// the lock of a log just closed, or the shared lock of an [`is_busy`]
/// probe.
pub const LOCK_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
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

/// Folds `dir`'s log without writing to it or taking its lock: what a
/// read-only view of a review, and the list of past ones, show.
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

/// One past review under the store root, as the start view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub dir: PathBuf,
    pub id: SessionId,
    pub created_at: Millis,
    pub kind: TargetKind,
    pub label: String,
    pub decision: Option<Decision>,
    /// Live findings per severity, most severe first.
    pub severities: [usize; 5],
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
}

/// The newest `limit` reviews under `root`, newest first. Only the listed
/// logs are read; a log that cannot be read is listed with why.
pub fn list_sessions(root: &Path, limit: usize) -> Vec<Listed> {
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
    dirs.into_iter()
        .take(limit)
        .map(|(_, dir)| {
            let summary = read_session(&dir)
                .map(|session| Summary {
                    busy: is_busy(&dir),
                    ..Summary::of(&dir, &session)
                })
                .map_err(|e| e.to_string());
            Listed { dir, summary }
        })
        .collect()
}

/// `rev-<millis>` or `rev-<millis>-<n>` as `(millis, n)`, for ordering.
fn id_order(name: &str) -> (Millis, u32) {
    let rest = name.strip_prefix("rev-").unwrap_or_default();
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
            severities: session.severity_counts(),
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
    use crate::model::types::{AgentKind, Head, Participant, Role, Target, TargetManifest};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-review-store-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn created(id: &str, at: Millis) -> Vec<Event> {
        let p = |id: &str, role| Participant {
            id: PartId::new(id),
            kind: AgentKind::Claude,
            role,
            pane_id: None,
            setup: Default::default(),
            lens: None,
        };
        vec![Event::Created {
            id: SessionId::new(id),
            request: String::new(),
            target: Box::new(Target {
                kind: TargetKind::Document,
                path: None,
                base: None,
                head: Head::Worktree,
                paths: Vec::new(),
                text: Some("doc".into()),
                workdir: None,
            }),
            manifest: Box::new(TargetManifest {
                label: format!("doc {id}"),
                ..TargetManifest::default()
            }),
            workdir: None,
            max_rounds: 3,
            lead: p("lead", Role::Lead),
            reviewers: vec![p("claude", Role::Reviewer), p("codex", Role::Reviewer)],
            at,
        }]
    }

    #[test]
    fn a_second_open_of_a_held_log_is_busy_and_a_read_still_works() {
        let root = temp_root("busy");
        let dir = root.join("rev-1");
        let _store = Store::create(&dir, &created("rev-1", 1)).unwrap();
        assert!(is_busy(&dir));
        match Store::open(&dir) {
            Err(StoreError::Busy) => {}
            other => panic!("expected Busy, got {other:?}"),
        }
        assert_eq!(read_session(&dir).unwrap().id.as_str(), "rev-1");
        drop(_store);
        assert!(!is_busy(&dir));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_torn_last_line_is_cut_on_open() {
        let root = temp_root("torn");
        let dir = root.join("rev-1");
        drop(Store::create(&dir, &created("rev-1", 1)).unwrap());
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
        for (id, at) in [("rev-5", 5), ("rev-20", 20), ("rev-20-2", 21)] {
            drop(Store::create(&root.join(id), &created(id, at)).unwrap());
        }
        fs::create_dir_all(root.join("rev-30")).unwrap();
        fs::write(root.join("rev-30").join(EVENTS_FILE), "not json\n").unwrap();
        let listed = list_sessions(&root, 10);
        let names: Vec<String> = listed
            .iter()
            .map(|l| l.dir.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["rev-30", "rev-20-2", "rev-20", "rev-5"]);
        assert!(listed[0].summary.as_ref().unwrap_err().contains("line 1"));
        let s = listed[1].summary.as_ref().unwrap();
        assert_eq!(
            (s.label.as_str(), s.rounds, s.closed),
            ("doc rev-20-2", 0, false)
        );
        assert_eq!(list_sessions(&root, 2).len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_report_is_replaced_whole() {
        let root = temp_root("report");
        fs::create_dir_all(&root).unwrap();
        write_report(&root, "one").unwrap();
        write_report(&root, "two").unwrap();
        assert_eq!(fs::read_to_string(report_path(&root)).unwrap(), "two");
        let leftovers: Vec<_> = fs::read_dir(&root).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_link_left_where_the_report_or_its_temporary_file_go_is_not_written_through() {
        use std::os::unix::fs::symlink;
        let root = temp_root("report-link");
        let victim = temp_root("report-victim");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("v"), "keep").unwrap();
        symlink(victim.join("v"), root.join(format!(".{REPORT_FILE}.tmp"))).unwrap();
        write_report(&root, "report").unwrap();
        assert_eq!(fs::read_to_string(victim.join("v")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(report_path(&root)).unwrap(), "report");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(victim);
    }

    #[test]
    fn replies_are_read_cleared_and_prepared_only_below_their_base_without_links() {
        use std::os::unix::fs::symlink;
        let base = temp_root("reply-base");
        let elsewhere = temp_root("reply-elsewhere");
        fs::create_dir_all(base.join(".noa/reviews/rev-1")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let dir = base.join(".noa/reviews/rev-1");
        // A round folder swapped for a link to somewhere else.
        fs::write(elsewhere.join("a.json"), "{}").unwrap();
        symlink(&elsewhere, dir.join("r1")).unwrap();
        let path = dir.join("r1/a.json");
        assert!(fsx::is_link_error(&read_reply(&base, &path).unwrap_err()));
        assert!(clear_reply(&base, &path).is_err());
        assert!(
            elsewhere.join("a.json").exists(),
            "a reply elsewhere was removed"
        );
        assert!(prepare_reply(&base, &dir.join("r1/sub/b.json")).is_err());
        assert!(!elsewhere.join("sub").exists());
        // `.noa` itself a link: the workdir is not prepared through it.
        let work = temp_root("reply-work");
        fs::create_dir_all(&work).unwrap();
        symlink(&elsewhere, work.join(".noa")).unwrap();
        assert!(prepare_workdir(&work).is_err());
        assert!(!elsewhere.join(".gitignore").exists());
        // A plain reply reads back; an oversized one is an error.
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
        for d in [base, elsewhere, work] {
            let _ = fs::remove_dir_all(d);
        }
    }

    fn stored(dir: &Path, log: &[Vec<Event>]) {
        let mut store = Store::create(dir, &log[0]).unwrap();
        store.append_all(&log[1..]).unwrap();
    }

    #[test]
    fn a_report_is_stale_until_rewritten_from_the_log() {
        let root = temp_root("report");
        // Not consolidated yet: there is no report to compare.
        let rig = crate::testkit::Rig::new(3);
        let fresh = root.join("rev-fresh");
        stored(&fresh, &rig.log);
        assert!(!report_is_stale(&fresh, rig.engine.session()));

        let mut rig = crate::testkit::Rig::new(3);
        crate::testkit::through_round_one(&mut rig);
        let dir = root.join("rev-1");
        stored(&dir, &rig.log);
        let session = rig.engine.session();
        assert!(report_is_stale(&dir, session), "missing");
        rewrite_report(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(report_path(&dir)).unwrap(),
            export::report_markdown(session)
        );
        assert!(!report_is_stale(&dir, session));
        fs::write(report_path(&dir), "edited").unwrap();
        assert!(report_is_stale(&dir, session), "differs");
        // A runner holding the log is not written under.
        let (held, _) = Store::open(&dir).unwrap();
        assert!(matches!(rewrite_report(&dir), Err(StoreError::Busy)));
        drop(held);
        let _ = fs::remove_dir_all(root);
    }
}
