//! Sessions and artifacts on disk. One directory per competition:
//!
//! ```text
//! <data-dir>/noa/competitions/<session id>/
//!   events.jsonl                  one event batch per line
//!   entries/<part>/...            each contestant's private live output folder
//!   r<N>/<part>.md                contestant submission report for round N
//!   r<N>/judge.json               judge evaluation output for round N
//!   r<N>/artifacts/<part>/...     round-N snapshot of <part>'s deliverables
//!   compare.html                  standalone side-by-side comparison viewer
//! ```

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{
    ArtifactFile, ArtifactKind, Millis, PartId, RoundNo, Session, SessionId,
};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const JUDGE_FILE: &str = "judge.json";
pub const VIEWER_FILE: &str = "compare.html";
/// Beside the log of every competition a lock-taking runner created or
/// resumed: there, an unlocked log is not running anywhere. Without it the
/// log may be one a widget from before the lock still appends to.
pub const LOCKS_FILE: &str = "locks";
/// How long after its last write a log without [`LOCKS_FILE`] is taken to
/// be possibly still running in such an older widget.
pub const LEGACY_QUIET: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Maximum total bytes copied into one round's artifact snapshot per contestant.
const MAX_SNAPSHOT_BYTES: u64 = 50 * 1024 * 1024;
/// Maximum number of files recorded in one contestant's snapshot.
const MAX_SNAPSHOT_FILES: usize = 200;

/// Where competitions live: `<data-dir>/noa/competitions`, beside Noa's own
/// session state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("competitions"))
}

pub fn session_dir(root: &Path, id: &SessionId) -> PathBuf {
    root.join(id.as_str())
}

pub fn round_dir(dir: &Path, round: RoundNo) -> PathBuf {
    dir.join(format!("r{round}"))
}

/// Where reply files and contestant output directories go when agents run in
/// `workdir`: `<workdir>/.noa/competitions/<id>`.
pub fn workdir_reply_dir(workdir: &Path, id: &SessionId) -> PathBuf {
    workdir.join(".noa").join("competitions").join(id.as_str())
}

/// A stable 64-bit FNV-1a digest of `text`, which a log may keep: what
/// identifies a reply file's content across runs.
pub fn digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The root of `session`'s reply files and artifacts: its workdir's
/// `.noa/competitions/<id>` when it has one, else its store directory `dir`.
pub fn reply_dir(dir: &Path, session: &Session) -> PathBuf {
    match &session.workdir {
        Some(workdir) => workdir_reply_dir(workdir, &session.id),
        None => dir.to_owned(),
    }
}

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, so competition
/// artifacts never pollute `git status`.
pub fn prepare_workdir(workdir: &Path) -> io::Result<()> {
    let dir = workdir.join(".noa");
    fs::create_dir_all(&dir)?;
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

/// Contestant `part`'s private working output directory.
pub fn entry_dir(base_dir: &Path, part: &PartId) -> PathBuf {
    base_dir.join("entries").join(part.as_str())
}

/// Round-`round` artifact snapshot directory for `part`.
pub fn artifact_snapshot_dir(base_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(base_dir, round)
        .join("artifacts")
        .join(part.as_str())
}

/// `<base>/r<N>/checks/<part>/`: the snapshot check (manifest and
/// viewer-condition screenshots) of `part`'s round-`round` entry.
pub fn checks_dir(base_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(base_dir, round)
        .join("checks")
        .join(part.as_str())
}

/// Path to the standalone HTML comparison viewer.
pub fn viewer_path(base_dir: &Path) -> PathBuf {
    base_dir.join(VIEWER_FILE)
}

/// The file contestant `part` writes its round-`round` report to.
///
/// Prefixed so a contestant named `claude` or `agents` does not write
/// `claude.md`, which case-insensitive filesystems (macOS) resolve as
/// `CLAUDE.md` and agents then load as project instructions.
pub fn reply_path(dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(dir, round).join(format!("report-{part}.md"))
}

/// The file the judge writes its round-`round` JSON to.
pub fn judge_reply_path(dir: &Path, round: RoundNo) -> PathBuf {
    round_dir(dir, round).join(JUDGE_FILE)
}

pub fn prepare_reply(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

pub fn clear_reply(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn read_reply(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn ignored_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "dist" | "__pycache__" | ".DS_Store"
    )
}

/// Copies `src_dir` (`entries/<part>`) into `dst_dir` (`r<N>/artifacts/<part>`)
/// and returns the sorted list of captured [`ArtifactFile`]s.
pub fn snapshot_artifacts(src_dir: &Path, dst_dir: &Path) -> io::Result<Vec<ArtifactFile>> {
    if !src_dir.is_dir() {
        return Ok(Vec::new());
    }
    if dst_dir.exists() {
        let _ = fs::remove_dir_all(dst_dir);
    }
    fs::create_dir_all(dst_dir)?;
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    copy_dir_rec(src_dir, src_dir, dst_dir, &mut files, &mut total_bytes)?;
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

fn copy_dir_rec(
    root: &Path,
    cur: &Path,
    dst_root: &Path,
    out: &mut Vec<ArtifactFile>,
    total_bytes: &mut u64,
) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(cur)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_SNAPSHOT_FILES {
            break;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ignored_dir_name(&name_str) || name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir_rec(root, &path, dst_root, out, total_bytes)?;
        } else if meta.is_file() {
            let size = meta.len();
            if total_bytes.saturating_add(size) > MAX_SNAPSHOT_BYTES {
                continue;
            }
            // Joined from its components, never by rewriting separators: a
            // file name may hold `\` (`a\..\..\x` is one name on Unix).
            let rel_path = path.strip_prefix(root).unwrap_or(&path);
            let rel = rel_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let target = dst_root.join(rel_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &target)?;
            *total_bytes += size;
            out.push(ArtifactFile {
                kind: ArtifactKind::from_path(&rel),
                rel_path: rel,
                bytes: size,
            });
        }
    }
    Ok(())
}

/// Writes the comparison viewer HTML file and returns its path.
pub fn write_viewer(base_dir: &Path, session: &Session) -> io::Result<PathBuf> {
    fs::create_dir_all(base_dir)?;
    let path = viewer_path(base_dir);
    let html = crate::model::viewer::render_html(session, base_dir);
    fs::write(&path, html)?;
    Ok(path)
}

/// Like [`write_viewer`], reusing cached artifact previews across refreshes.
pub fn write_viewer_cached(
    base_dir: &Path,
    session: &Session,
    cache: &mut crate::model::viewer::ViewerCache,
) -> io::Result<PathBuf> {
    fs::create_dir_all(base_dir)?;
    let path = viewer_path(base_dir);
    let html = crate::model::viewer::render_html_cached(session, base_dir, cache);
    fs::write(&path, html)?;
    Ok(path)
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
    /// An unfinished log without [`LOCKS_FILE`] written to lately: a widget
    /// from before the lock may still be running it.
    MaybeRunning,
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
            StoreError::MaybeRunning => write!(
                f,
                "旧版のウィジェットで実行中の可能性があります（最後の書き込みから{}分経つと再開できます）",
                LEGACY_QUIET.as_secs() / 60
            ),
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
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(dir)?;
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join(EVENTS_FILE))?;
        lock(&file)?;
        mark_locking(dir)?;
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
        if !session.closed && may_run_unlocked(dir) {
            return Err(StoreError::MaybeRunning);
        }
        mark_locking(dir)?;
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

/// Takes the log's exclusive lock, held until the file is closed.
fn lock(file: &File) -> Result<(), StoreError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(fs::TryLockError::WouldBlock) => Err(StoreError::Busy),
        Err(fs::TryLockError::Error(e)) => Err(StoreError::Io(e)),
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
/// read-only view of a competition, and the list of past ones, show.
pub fn read_session(dir: &Path) -> Result<Session, StoreError> {
    let text = fs::read(dir.join(EVENTS_FILE))?;
    let parsed = parse(&text)?;
    reduce::replay(&parsed.batches).map_err(StoreError::Reduce)
}

fn mark_locking(dir: &Path) -> io::Result<()> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(LOCKS_FILE))
    {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// Whether `dir`'s log has no [`LOCKS_FILE`] and was written within
/// [`LEGACY_QUIET`]: a widget from before the lock may be running it.
pub fn may_run_unlocked(dir: &Path) -> bool {
    if dir.join(LOCKS_FILE).exists() {
        return false;
    }
    fs::metadata(dir.join(EVENTS_FILE))
        .and_then(|m| m.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age < LEGACY_QUIET))
}

/// Whether a runner holds `dir`'s log open right now.
pub fn is_busy(dir: &Path) -> bool {
    File::open(dir.join(EVENTS_FILE))
        .is_ok_and(|file| matches!(file.try_lock_shared(), Err(fs::TryLockError::WouldBlock)))
}

/// One past competition under the store root, as the start view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub dir: PathBuf,
    pub id: SessionId,
    pub created_at: Millis,
    pub request: String,
    pub winner: Option<PartId>,
    pub closed: bool,
    pub rounds: usize,
    /// `claude`, `claude-2 (sonnet)`, … in panel order.
    pub contestants: Vec<String>,
    /// A runner holds its log: it is running in some window.
    pub busy: bool,
    /// Unfinished, without [`LOCKS_FILE`], and written lately: it may be
    /// running in a widget from before the lock.
    pub maybe_running: bool,
}

/// A directory under the store root and what reading its log gave.
#[derive(Debug)]
pub struct Listed {
    pub dir: PathBuf,
    pub summary: Result<Summary, String>,
}

/// The newest `limit` competitions under `root`, newest first. Only the
/// listed logs are read; a log that cannot be read is listed with why.
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
                .map(|session| summarize(&dir, &session))
                .map_err(|e| e.to_string());
            Listed { dir, summary }
        })
        .collect()
}

/// `comp-<millis>` or `comp-<millis>-<n>` as `(millis, n)`, for ordering.
fn id_order(name: &str) -> (Millis, u32) {
    let rest = name.strip_prefix("comp-").unwrap_or_default();
    let (at, n) = rest.split_once('-').unwrap_or((rest, "0"));
    (at.parse().unwrap_or(0), n.parse().unwrap_or(0))
}

fn summarize(dir: &Path, session: &Session) -> Summary {
    Summary {
        busy: is_busy(dir),
        maybe_running: !session.closed && may_run_unlocked(dir),
        ..Summary::of(dir, session)
    }
}

impl Summary {
    /// `session` (logged in `dir`) as listed, before its log's lock and
    /// age are looked at.
    pub fn of(dir: &Path, session: &Session) -> Summary {
        Summary {
            dir: dir.to_owned(),
            id: session.id.clone(),
            created_at: session.created_at,
            request: session.request.clone(),
            winner: session.winner.clone(),
            closed: session.closed,
            rounds: session.rounds.len(),
            contestants: session
                .contestants
                .iter()
                .map(|p| match p.setup.label() {
                    Some(setup) => format!("{} ({setup})", p.id),
                    None => p.id.to_string(),
                })
                .collect(),
            busy: false,
            maybe_running: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-competition-store-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn snapshot_artifacts_copies_deliverables_and_skips_hidden_dirs() {
        let root = temp_root("snap");
        let src = entry_dir(&root, &PartId::new("claude"));
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("index.html"), "<h1>Hi</h1>").unwrap();
        fs::write(src.join("assets/logo.svg"), "<svg/>").unwrap();
        fs::write(src.join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        let dst = artifact_snapshot_dir(&root, 1, &PartId::new("claude"));
        let files = snapshot_artifacts(&src, &dst).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rel_path, "assets/logo.svg");
        assert_eq!(files[0].kind, ArtifactKind::Svg);
        assert_eq!(files[1].rel_path, "index.html");
        assert_eq!(files[1].kind, ArtifactKind::Html);
        assert!(dst.join("index.html").is_file());
        assert!(!dst.join(".git").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_report_file_never_collides_with_agent_instruction_files() {
        let path = reply_path(Path::new("/c"), 3, &PartId::new("claude"));
        assert_eq!(path.file_name().unwrap(), "report-claude.md");
        let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(name != "claude.md" && name != "agents.md");
    }

    #[test]
    fn a_backslash_in_a_file_name_does_not_reach_outside_the_snapshot() {
        let root = temp_root("escape");
        let src = entry_dir(&root, &PartId::new("claude"));
        fs::create_dir_all(&src).unwrap();
        let name = r"asset\..\..\..\judge.json";
        fs::write(src.join(name), "{}").unwrap();

        let dst = artifact_snapshot_dir(&root, 1, &PartId::new("claude"));
        let files = snapshot_artifacts(&src, &dst).unwrap();
        assert!(!judge_reply_path(&root, 1).exists());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel_path, name);
        assert!(dst.join(name).is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    fn create(root: &Path, id: &str, request: &str) -> (Store, PathBuf) {
        use crate::engine::{Config, Engine, Setup};
        use crate::model::types::{AgentChoice, AgentKind};
        let id = SessionId::new(id);
        let dir = session_dir(root, &id);
        let panel: Vec<AgentChoice> = vec![
            AgentKind::Claude.into(),
            AgentChoice {
                kind: AgentKind::Claude,
                setup: crate::model::types::AgentSetup {
                    model: Some("sonnet".into()),
                    effort: None,
                },
            },
        ];
        let setup = Setup {
            request,
            max_rounds: Some(3),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Codex.into(),
            panel: &panel,
        };
        let (_, out) = Engine::create_in(id, setup, &dir, Config::default(), 7).unwrap();
        (Store::create(&dir, &out.batches[0]).unwrap(), dir)
    }

    #[test]
    fn past_competitions_are_listed_newest_first() {
        let root = temp_root("list");
        let (a, _) = create(&root, "comp-100", "older");
        let (b, _) = create(&root, "comp-300", "newest");
        let (c, _) = create(&root, "comp-200-2", "middle");
        fs::create_dir_all(root.join("not-a-competition")).unwrap();
        drop((a, b, c));

        let listed = list_sessions(&root, 10);
        let requests: Vec<String> = listed
            .iter()
            .map(|l| l.summary.as_ref().unwrap().request.clone())
            .collect();
        assert_eq!(requests, ["newest", "middle", "older"]);
        let top = listed[0].summary.as_ref().unwrap();
        assert_eq!(top.contestants, ["claude", "claude-2 (sonnet)"]);
        assert!(!top.closed && top.winner.is_none() && !top.busy);
        assert_eq!(list_sessions(&root, 2).len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_damaged_log_is_listed_with_why() {
        let root = temp_root("damaged");
        let (store, dir) = create(&root, "comp-5", "x");
        drop(store);
        fs::write(dir.join(EVENTS_FILE), "not json\n").unwrap();
        let listed = list_sessions(&root, 10);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].summary.as_ref().unwrap_err().contains("line 1"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_log_open_in_a_store_is_busy_and_cannot_be_opened_twice() {
        let root = temp_root("busy");
        let (store, dir) = create(&root, "comp-9", "x");
        assert!(is_busy(&dir));
        assert!(list_sessions(&root, 10)[0].summary.as_ref().unwrap().busy);
        assert!(matches!(Store::open(&dir), Err(StoreError::Busy)));
        drop(store);
        // A process another test forks in this instant inherits the open
        // file (and its lock) until it execs.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while is_busy(&dir) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!is_busy(&dir));
        let (_reopened, session) = Store::open(&dir).unwrap();
        assert_eq!(session.request, "x");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reading_a_session_leaves_a_torn_log_alone() {
        let root = temp_root("torn");
        let (store, dir) = create(&root, "comp-11", "x");
        drop(store);
        let path = dir.join(EVENTS_FILE);
        let mut text = fs::read(&path).unwrap();
        text.extend_from_slice(b"[{\"half");
        fs::write(&path, &text).unwrap();
        assert_eq!(read_session(&dir).unwrap().request, "x");
        assert_eq!(fs::read(&path).unwrap(), text);
        Store::open(&dir).unwrap();
        assert!(fs::read(&path).unwrap().len() < text.len());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_log_from_before_the_lock_written_lately_is_not_resumed() {
        let root = temp_root("legacy");
        let (store, dir) = create(&root, "comp-12", "x");
        drop(store);
        // What a widget from before the lock leaves: no marker, and a log
        // it may still be appending to.
        fs::remove_file(dir.join(LOCKS_FILE)).unwrap();
        let log = File::options()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        log.set_modified(std::time::SystemTime::now()).unwrap();
        assert!(matches!(Store::open(&dir), Err(StoreError::MaybeRunning)));
        let listed = list_sessions(&root, 10);
        assert!(listed[0].summary.as_ref().unwrap().maybe_running);

        // Quiet for long enough: it is taken over, and locked from then on.
        let long_ago =
            std::time::SystemTime::now() - LEGACY_QUIET - std::time::Duration::from_secs(1);
        log.set_modified(long_ago).unwrap();
        drop(log);
        assert!(
            !list_sessions(&root, 10)[0]
                .summary
                .as_ref()
                .unwrap()
                .maybe_running
        );
        let (_store, _) = Store::open(&dir).unwrap();
        assert!(dir.join(LOCKS_FILE).exists());
        assert!(!may_run_unlocked(&dir));
        fs::remove_dir_all(&root).unwrap();
    }
}
