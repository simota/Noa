//! Sessions on disk. One directory per discussion:
//!
//! ```text
//! <data-dir>/noa/discussions/<session id>/
//!   events.jsonl        one event batch per line (the log the session folds from)
//!   r<N>/<part>.md      panelist reply files
//!   r<N>/moderator.json the moderator's output for round N
//! ```

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{Millis, PartId, RoundNo, Session, SessionId};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const MODERATOR_FILE: &str = "moderator.json";

/// Where discussions live: `<data-dir>/noa/discussions`, beside Noa's own
/// session state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("discussions"))
}

pub fn session_dir(root: &Path, id: &SessionId) -> PathBuf {
    root.join(id.as_str())
}

pub fn round_dir(dir: &Path, round: RoundNo) -> PathBuf {
    dir.join(format!("r{round}"))
}

/// Where the reply files of session `id` go when its agents run in
/// `workdir`: `<workdir>/.noa/discussions/<id>`, beside `noa +ctl`'s
/// `$NOA_RESULT` files.
pub fn workdir_reply_dir(workdir: &Path, id: &SessionId) -> PathBuf {
    workdir.join(".noa").join("discussions").join(id.as_str())
}

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, as `noa +ctl`
/// does, so the reply files never show in `git status`. An existing
/// `.gitignore` there is kept.
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

/// The file panelist `part` writes its round-`round` reply to.
pub fn reply_path(dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(dir, round).join(format!("{part}.md"))
}

/// The file the moderator writes its round-`round` JSON to.
pub fn moderator_reply_path(dir: &Path, round: RoundNo) -> PathBuf {
    round_dir(dir, round).join(MODERATOR_FILE)
}

/// Makes the directory a reply file goes in. A file already there is kept:
/// after a restart it may be a reply not yet taken in.
pub fn prepare_reply(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

/// Removes a rejected reply file so the rewrite is seen as new.
pub fn clear_reply(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The reply file's text, or `None` while it is not there.
pub fn read_reply(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The sessions under `root`, oldest id first.
pub fn list_sessions(root: &Path) -> io::Result<Vec<SessionId>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().join(EVENTS_FILE).is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            ids.push(SessionId::new(name));
        }
    }
    ids.sort();
    Ok(ids)
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// A line of the log is not an event batch.
    Corrupt {
        line: usize,
        error: String,
    },
    /// The log parses but does not fold.
    Reduce(ReduceError),
    /// Another runner (in this window or another) has the log open.
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

/// An open session's log, appended to one batch per line.
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

/// How long a lock found taken is waited for before the log counts as
/// running elsewhere. A `flock` lock belongs to the open file, and a child
/// process being started on another thread holds a copy of every open file
/// of this process until its exec closes them: for that moment it holds
/// the lock of a log just closed, or the shared lock of an [`is_busy`]
/// probe. The moment lasts microseconds, longer on a loaded machine.
pub const LOCK_GRACE: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(5);

/// Takes the log's exclusive lock, held until the file is closed.
fn lock(file: &File) -> Result<(), StoreError> {
    let until = Instant::now() + LOCK_GRACE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(LOCK_POLL);
            }
            Err(fs::TryLockError::WouldBlock) => return Err(StoreError::Busy),
            Err(fs::TryLockError::Error(e)) => return Err(StoreError::Io(e)),
        }
    }
}

/// Whether a runner holds `dir`'s log open right now. Looked at once: a
/// lock held for an instant by a starting child (see [`LOCK_GRACE`]) can
/// show a log busy that is not, so a decision uses [`held_elsewhere`].
pub fn is_busy(dir: &Path) -> bool {
    File::open(dir.join(EVENTS_FILE))
        .is_ok_and(|file| matches!(file.try_lock_shared(), Err(fs::TryLockError::WouldBlock)))
}

/// Whether `dir`'s log stays locked for [`LOCK_GRACE`]: a runner holds it,
/// not a child that was starting when it was looked at.
pub fn held_elsewhere(dir: &Path) -> bool {
    let until = Instant::now() + LOCK_GRACE;
    while is_busy(dir) {
        if Instant::now() >= until {
            return true;
        }
        std::thread::sleep(LOCK_POLL);
    }
    false
}

/// A log as read: its complete batches, how many bytes they take, and
/// whether a last line was cut short.
struct Log {
    batches: Vec<Vec<Event>>,
    kept: usize,
    torn: bool,
}

fn read_log(text: &[u8]) -> Result<Log, StoreError> {
    // Bytes, not a string: a last line torn inside a multi-byte character
    // is not valid UTF-8, and is dropped like any torn line.
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
    Ok(Log {
        batches,
        kept,
        torn,
    })
}

/// Folds the session at `dir` without opening its log for writing: a torn
/// last line is skipped, not cut from the file. For showing a stored
/// discussion (another window may still be writing to it).
pub fn load(dir: &Path) -> Result<Session, StoreError> {
    let text = fs::read(dir.join(EVENTS_FILE))?;
    let read = read_log(&text)?;
    reduce::replay(&read.batches).map_err(StoreError::Reduce)
}

/// One stored discussion, as the start view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: SessionId,
    pub dir: PathBuf,
    /// What the log folds to, or why it does not.
    pub state: Result<Outline, String>,
}

/// The facts a listed discussion shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    pub topic: String,
    pub rounds: usize,
    pub max_rounds: Option<RoundNo>,
    pub closed: bool,
    pub created_at: Millis,
}

impl Outline {
    pub fn of(session: &Session) -> Self {
        Outline {
            topic: session.topic.clone(),
            rounds: session.rounds.len(),
            max_rounds: session.max_rounds,
            closed: session.closed,
            created_at: session.created_at,
        }
    }
}

/// The sessions under `root`, newest first, each folded read-only
/// ([`load`]). A session whose log does not fold is listed with why.
pub fn summaries(root: &Path) -> io::Result<Vec<Summary>> {
    let mut ids = list_sessions(root)?;
    ids.reverse();
    Ok(ids
        .into_iter()
        .map(|id| {
            let dir = session_dir(root, &id);
            let state = load(&dir)
                .map(|session| Outline::of(&session))
                .map_err(|e| e.to_string());
            Summary { id, dir, state }
        })
        .collect())
}

impl Store {
    /// Starts a new session directory at `dir` with its first batch (the
    /// `Created` one). Fails if the directory already exists. The log stays
    /// locked while the store is open, as with [`Store::open`].
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
        let mut store = Store {
            dir: dir.to_owned(),
            file,
        };
        store.append(first)?;
        Ok(store)
    }

    /// Opens the session at `dir` and folds its log. A last line cut short
    /// (the app died while writing it) is dropped from the file; damage
    /// anywhere else is an error. The log is locked (an exclusive
    /// `File::try_lock`, across processes) before it is read and stays
    /// locked until the store is dropped, so two runners never fold and
    /// append to one log: the second gets [`StoreError::Busy`].
    pub fn open(dir: &Path) -> Result<(Store, Session), StoreError> {
        let path = dir.join(EVENTS_FILE);
        let mut file = OpenOptions::new().append(true).open(&path)?;
        lock(&file)?;
        let text = fs::read(&path)?;
        let read = read_log(&text)?;
        let session = reduce::replay(&read.batches).map_err(StoreError::Reduce)?;
        if read.torn {
            if read.kept < text.len() {
                file.set_len(read.kept as u64)?;
            } else {
                // The last batch parsed but lacks its newline.
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

    /// Writes one batch as one line.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, Author, Message, MessageId, MsgKind, Participant, Role, RoundKind,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_workdir_gets_a_self_ignoring_dot_noa_and_keeps_an_existing_gitignore() {
        let root = temp_root("workdir");
        fs::create_dir_all(&root).unwrap();
        prepare_workdir(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        fs::write(root.join(".noa/.gitignore"), "mine\n").unwrap();
        prepare_workdir(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".noa/.gitignore")).unwrap(),
            "mine\n"
        );
        assert_eq!(
            workdir_reply_dir(&root, &SessionId::new("disc-9")),
            root.join(".noa/discussions/disc-9")
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-discussion-store-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn created() -> Vec<Event> {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        vec![Event::Created {
            id: SessionId::new("disc-1"),
            topic: "t".into(),
            workdir: None,
            max_rounds: None,
            moderator: p("noa", AgentKind::Codex, Role::Moderator),
            panelists: vec![p("claude", AgentKind::Claude, Role::Panelist)],
            at: 1,
        }]
    }

    fn posted(seq: usize) -> Vec<Event> {
        vec![Event::MessagePosted {
            message: Message {
                id: MessageId::nth(seq),
                round: 1,
                author: Author::User,
                kind: MsgKind::Intervention,
                issues: vec![],
                reply_to: vec![],
                to: vec![],
                debate: None,
                body: format!("line one\nline \"two\" {seq}"),
                at: 2,
            },
        }]
    }

    #[test]
    fn a_session_round_trips_through_the_log() {
        let root = temp_root("roundtrip");
        let dir = session_dir(&root, &SessionId::new("disc-1"));
        let batches = vec![
            created(),
            vec![Event::RoundStarted {
                no: 1,
                kind: RoundKind::Open,
                at: 3,
            }],
            posted(1),
            posted(2),
        ];
        let mut store = Store::create(&dir, &batches[0]).unwrap();
        store.append_all(&batches[1..]).unwrap();
        drop(store);
        let (mut store, session) = Store::open(&dir).unwrap();
        assert_eq!(session, reduce::replay(&batches).unwrap());
        assert_eq!(session.rev, 4);
        // Appending after a resume continues the same log.
        store.append(&posted(3)).unwrap();
        drop(store);
        let (_, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 3);
        assert_eq!(
            list_sessions(&root).unwrap(),
            vec![SessionId::new("disc-1")]
        );
        assert!(matches!(
            Store::create(&dir, &created()),
            Err(StoreError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_torn_last_line_is_dropped_and_writing_continues() {
        let root = temp_root("torn");
        let dir = root.join("s");
        let mut store = Store::create(&dir, &created()).unwrap();
        store.append(&posted(1)).unwrap();
        drop(store);
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        file.write_all(br#"[{"event":"message_po"#).unwrap();
        drop(file);
        let (mut store, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 1);
        store.append(&posted(2)).unwrap();
        drop(store);
        let (_, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_last_line_torn_inside_a_multibyte_character_is_dropped() {
        let root = temp_root("torn-utf8");
        let dir = root.join("s");
        let mut store = Store::create(&dir, &created()).unwrap();
        store.append(&posted(1)).unwrap();
        drop(store);
        // A Japanese reply cut off one byte into "議" (3 bytes in UTF-8).
        let mut torn = br#"[{"event":"message_posted","body":""#.to_vec();
        torn.extend_from_slice(&"議".as_bytes()[..1]);
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        file.write_all(&torn).unwrap();
        drop(file);
        let (mut store, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 1);
        store.append(&posted(2)).unwrap();
        drop(store);
        let (_, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_complete_last_line_without_newline_is_kept() {
        let root = temp_root("nonl");
        let dir = root.join("s");
        drop(Store::create(&dir, &created()).unwrap());
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        let mut batch = line(&posted(1)).unwrap();
        batch.pop();
        file.write_all(&batch).unwrap();
        drop(file);
        let (mut store, session) = Store::open(&dir).unwrap();
        assert_eq!(session.messages.len(), 1);
        store.append(&posted(2)).unwrap();
        drop(store);
        assert_eq!(Store::open(&dir).unwrap().1.messages.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn damage_before_the_end_is_an_error() {
        let root = temp_root("corrupt");
        let dir = root.join("s");
        drop(Store::create(&dir, &created()).unwrap());
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        file.write_all(b"garbage\n").unwrap();
        file.write_all(&line(&posted(1)).unwrap()).unwrap();
        drop(file);
        assert!(matches!(
            Store::open(&dir),
            Err(StoreError::Corrupt { line: 2, .. })
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reply_files_live_in_round_directories() {
        let root = temp_root("replies");
        let path = reply_path(&root, 3, &PartId::new("codex"));
        assert!(path.ends_with("r3/codex.md"));
        assert!(moderator_reply_path(&root, 3).ends_with("r3/moderator.json"));
        assert_eq!(read_reply(&path).unwrap(), None);
        prepare_reply(&path).unwrap();
        fs::write(&path, "hello").unwrap();
        prepare_reply(&path).unwrap();
        assert_eq!(read_reply(&path).unwrap().as_deref(), Some("hello"));
        clear_reply(&path).unwrap();
        clear_reply(&path).unwrap();
        assert_eq!(read_reply(&path).unwrap(), None);
        assert!(list_sessions(&root.join("missing")).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_log_open_in_one_store_is_busy_for_another_until_dropped() {
        let root = temp_root("lock");
        let dir = root.join("s");
        let created = Store::create(&dir, &created()).unwrap();
        assert!(is_busy(&dir));
        assert!(matches!(Store::open(&dir), Err(StoreError::Busy)));
        // Reading it stays possible.
        assert!(load(&dir).is_ok());
        drop(created);
        assert!(!held_elsewhere(&dir));
        let (first, _) = Store::open(&dir).unwrap();
        assert!(matches!(Store::open(&dir), Err(StoreError::Busy)));
        drop(first);
        assert!(Store::open(&dir).is_ok());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_folds_without_touching_a_torn_last_line() {
        let root = temp_root("load");
        let dir = root.join("s");
        let mut store = Store::create(&dir, &created()).unwrap();
        store.append(&posted(1)).unwrap();
        drop(store);
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .unwrap();
        file.write_all(br#"[{"event":"message_po"#).unwrap();
        drop(file);
        let before = fs::read(dir.join(EVENTS_FILE)).unwrap();
        let session = load(&dir).unwrap();
        assert_eq!(session.messages.len(), 1);
        // A writer may still be mid-line: the file is left as it was.
        assert_eq!(fs::read(dir.join(EVENTS_FILE)).unwrap(), before);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn summaries_list_newest_first_and_name_a_damaged_log() {
        let root = temp_root("summaries");
        let old = session_dir(&root, &SessionId::new("disc-1000"));
        let mut store = Store::create(&old, &created()).unwrap();
        store
            .append(&[Event::RoundStarted {
                no: 1,
                kind: RoundKind::Open,
                at: 3,
            }])
            .unwrap();
        store.append(&[Event::SessionClosed]).unwrap();
        drop(store);
        let new = session_dir(&root, &SessionId::new("disc-2000"));
        drop(Store::create(&new, &created()).unwrap());
        let broken = session_dir(&root, &SessionId::new("disc-1500"));
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(EVENTS_FILE), "garbage\n").unwrap();
        // Not a session: no log.
        fs::create_dir_all(root.join("stray")).unwrap();

        let listed = summaries(&root).unwrap();
        let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["disc-2000", "disc-1500", "disc-1000"]);
        assert_eq!(listed[0].dir, new);
        assert_eq!(
            listed[0].state,
            Ok(Outline {
                topic: "t".into(),
                rounds: 0,
                max_rounds: None,
                closed: false,
                created_at: 1,
            })
        );
        assert!(
            listed[1]
                .state
                .as_ref()
                .is_err_and(|why| why.contains("line 1")),
            "{:?}",
            listed[1].state
        );
        let closed = listed[2].state.as_ref().unwrap();
        assert!(closed.closed);
        assert_eq!(closed.rounds, 1);
        assert!(summaries(&root.join("missing")).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_default_root_sits_beside_noa_state() {
        if let Some(root) = default_root() {
            assert!(root.ends_with("noa/discussions"), "{}", root.display());
        }
    }
}
