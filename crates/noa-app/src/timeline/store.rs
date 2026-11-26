//! The per-board durable store (`docs/specs/board-timeline.md` L2-4, §8.1):
//! `<root>/<boardId>/{meta.json, posts.jsonl, lock, quarantine/}`.
//!
//! One [`Store`] is owned by the `noa-timeline` service thread, which makes
//! it the only writer in this process; a second process is kept out per
//! board by a non-blocking `flock` on `lock`, taken on the first write and
//! held while the store lives. Reads never need the lock: a board another
//! instance writes is re-read from disk when its files change.
//!
//! A post is acknowledged only after its line was appended and
//! `sync_data` returned; the in-memory indexes change only then. A failed
//! append or sync poisons the board until the next write truncates the
//! file back to its durable length and replays it again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use noa_ipc::timeline::{
    BOARDS_DEFAULT, BoardInfo, BoardsParams, BoardsResult, CHANGES_DEFAULT, ChangesParams,
    ContextParams, ContextResult, ContextSelf, ContextWorker, GetParams, GetResult, IdKind,
    LIST_DEFAULT, ListParams, MAX_BOARD_BYTES, MAX_RESPONSE_BYTES, PageResult, Post, PostKind,
    PostParams, PostResult, PostSummary, Producer, ProducerKind, Provenance, PublishOwner,
    PublishPolicy, RegisterRunParams, RegisterRunResult, RunInfo, SCHEMA_VERSION, Seq,
    SetPublishPolicyParams, TaskRef, TimelineCall, TimelineError, TimelineErrorKind,
    UNREGISTERED_NAMESPACES, board_listing_order, canon_bytes, valid_id,
};
use noa_ipc::{Panel, WireId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids;
use super::replay::{self, CanonLine, Corrupt, PostLine, Rec, SourceKey};
use super::search::{self, Query};

/// Room kept below [`MAX_RESPONSE_BYTES`] for the JSON-RPC envelope
/// (`{"jsonrpc","id","result"}`) around a result the store sizes.
const ENVELOPE_RESERVE: usize = 1024;

/// The bytes a result may encode to.
pub(crate) const RESULT_BUDGET: usize = MAX_RESPONSE_BYTES - ENVELOPE_RESERVE;

/// How many of a pane's own runs `context` keeps however large the
/// response: enough for `+ctl timeline post` to tell one run from several.
const SELF_RUNS_KEPT: usize = 2;

/// Injectable limits (tests shrink them).
#[derive(Clone, Copy, Debug)]
pub(crate) struct StoreLimits {
    pub max_board_bytes: u64,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_board_bytes: MAX_BOARD_BYTES,
        }
    }
}

/// The store's write, sync, rename, directory sync, board lock and replay
/// read, so tests can inject faults (and another instance's writes, in
/// [`StoreIo::lock`], before the lock is taken).
pub(crate) trait StoreIo: Send {
    fn write_all(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        file.write_all(bytes)
    }
    fn sync_data(&self, file: &File) -> io::Result<()> {
        file.sync_data()
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    /// Makes `dir`'s entries durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }
    /// A non-blocking exclusive `flock` on a board's `lock` file; another
    /// holder is `WouldBlock`.
    fn lock(&self, file: &File) -> io::Result<()> {
        // SAFETY: `flock` on a descriptor the caller owns for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Opens `posts.jsonl` to replay it.
    fn open_read(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }
}

/// The real filesystem.
pub(crate) struct RealIo;

impl StoreIo for RealIo {}

// ---------------------------------------------------------------------------
// meta.json

/// One pane a root or worker was bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Binding {
    pub at: String,
    pub pane_id: WireId,
    pub server_instance_id: String,
    pub surface_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerMeta {
    agent_kind: Option<String>,
    bindings: Vec<Binding>,
    label: Option<String>,
    role: Option<String>,
}

/// The client-supplied fields of a `registerRun`, never pane ids, so a
/// re-send from a new pane (a widget after a restart) is the same run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunCanon {
    board_id: Option<String>,
    label: Option<String>,
    publish_policy: PublishPolicy,
    retry_of_run_id: Option<String>,
    role: Option<String>,
    task_ref: Option<TaskRef>,
}

impl RunCanon {
    fn of(p: &RegisterRunParams) -> Self {
        Self {
            board_id: p.board_id.clone(),
            label: p.label.clone(),
            publish_policy: p.publish_policy.clone(),
            retry_of_run_id: p.retry_of_run_id.clone(),
            role: p.role.clone(),
            task_ref: p.task_ref.clone(),
        }
    }

    /// Whether `resend` is this registration again. A `boardId` the
    /// re-send names matches when it is the run's board, given or not the
    /// first time: a widget saves the board its first registration created
    /// and passes it after a restart (L2-5 "the saved `boardId`").
    fn matches(&self, resend: &RunCanon, run_board: &str) -> bool {
        let board_ok = match (&self.board_id, &resend.board_id) {
            (_, None) => true,
            (_, Some(board)) => board == run_board,
        };
        board_ok
            && self.label == resend.label
            && self.publish_policy == resend.publish_policy
            && self.retry_of_run_id == resend.retry_of_run_id
            && self.role == resend.role
            && self.task_ref == resend.task_ref
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolicyChange {
    at: String,
    policy: PublishPolicy,
    request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMeta {
    canon: RunCanon,
    /// `summaryPostId` is not kept here: it is the run's first summary in
    /// `posts.jsonl`, overlaid when a [`RunInfo`] is returned.
    info: RunInfo,
    /// 1-based registration order on the board: `createdAt` can go
    /// backwards with the clock and ties within a millisecond.
    order: u64,
    #[serde(default)]
    policy_history: Vec<PolicyChange>,
    request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    board_id: String,
    created_at: String,
    name: String,
    /// `producerId` → `requestId` → `postId`: a post's request that was
    /// answered with another request's post (a key-B duplicate), so that
    /// request id stays used.
    #[serde(default)]
    request_aliases: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    roots: Vec<Binding>,
    #[serde(default)]
    runs: BTreeMap<String, RunMeta>,
    schema_version: u32,
    #[serde(default)]
    workers: BTreeMap<String, WorkerMeta>,
}

// ---------------------------------------------------------------------------
// Boards in memory

enum Health {
    Ok,
    /// An append or sync failed: truncate and replay before the next write.
    Poisoned,
    /// A complete line did not replay: read-only until repaired by hand.
    Corrupt(Corrupt),
}

/// The lock and the append handle, held from the first write on.
struct Writer {
    _lock: File,
    posts: File,
}

/// A file's `(mtime, len)`, to notice another instance's writes.
type Stamp = (u128, u64);

struct Board {
    id: String,
    dir: PathBuf,
    meta: Meta,
    meta_stamp: Option<Stamp>,
    recs: Vec<Rec>,
    /// Length of the replayed prefix of `posts.jsonl`: complete lines.
    durable_len: u64,
    /// How much of that prefix this store saw `sync_data` succeed over. A
    /// line beyond it (replayed after a restart, or written by another
    /// instance) is synced before a re-send of it is acknowledged: its own
    /// sync may have failed (§8.1).
    synced_len: u64,
    /// Bytes after `durable_len` without a newline (a torn append).
    torn: u64,
    health: Health,
    writer: Option<Writer>,
    key_a: HashMap<(String, String), usize>,
    key_b: HashMap<SourceKey, usize>,
    by_id: HashMap<String, usize>,
    superseded_by: HashMap<String, Vec<String>>,
    run_summary: HashMap<String, String>,
    last_activity: Option<String>,
}

impl Board {
    fn new(
        id: String,
        dir: PathBuf,
        meta: Meta,
        meta_stamp: Option<Stamp>,
        writer: Option<Writer>,
    ) -> Self {
        Self {
            id,
            dir,
            meta,
            meta_stamp,
            recs: Vec::new(),
            durable_len: 0,
            synced_len: 0,
            torn: 0,
            health: Health::Ok,
            writer,
            key_a: HashMap::new(),
            key_b: HashMap::new(),
            by_id: HashMap::new(),
            superseded_by: HashMap::new(),
            run_summary: HashMap::new(),
            last_activity: None,
        }
    }

    /// Replaces `meta.json` (atomically, under the lock) with `meta`, then
    /// the copy in memory.
    fn save_meta(&mut self, io: &dyn StoreIo, meta: Meta) -> Result<(), TimelineError> {
        let bytes =
            serde_json::to_vec_pretty(&meta).map_err(|err| io_error(io::Error::other(err)))?;
        let path = self.dir.join(META);
        write_private_atomic(io, &path, &bytes).map_err(io_error)?;
        self.meta_stamp = stamp(&path).map_err(io_error)?;
        self.meta = meta;
        Ok(())
    }

    fn posts_path(&self) -> PathBuf {
        self.dir.join(POSTS)
    }

    fn head(&self) -> u64 {
        self.recs.len() as u64
    }

    fn clear_posts(&mut self) {
        self.recs.clear();
        self.durable_len = 0;
        self.synced_len = 0;
        self.torn = 0;
        self.key_a.clear();
        self.key_b.clear();
        self.by_id.clear();
        self.superseded_by.clear();
        self.run_summary.clear();
        self.last_activity = None;
    }

    fn index(&mut self, rec: Rec) {
        let idx = self.recs.len();
        let summary = &rec.summary;
        self.key_a
            .insert((summary.producer.id.clone(), rec.request_id.clone()), idx);
        self.key_b.insert(rec.source.clone(), idx);
        self.by_id.insert(summary.post_id.clone(), idx);
        if let Some(old) = &summary.supersedes_post_id {
            self.superseded_by
                .entry(old.clone())
                .or_default()
                .push(summary.post_id.clone());
        }
        if summary.kind == PostKind::Summary
            && summary.supersedes_post_id.is_none()
            && let Some(run) = &summary.run_id
        {
            self.run_summary
                .entry(run.clone())
                .or_insert_with(|| summary.post_id.clone());
        }
        if self
            .last_activity
            .as_deref()
            .is_none_or(|last| instant_after(&summary.created_at, last))
        {
            self.last_activity = Some(summary.created_at.clone());
        }
        self.durable_len = rec.offset + rec.len;
        self.recs.push(rec);
    }

    /// Replays `posts.jsonl` from the end of the durable prefix (or from
    /// the start when `full`), indexing what it finds. The indexes change
    /// only once the whole read succeeded: a failed read leaves the board
    /// as it was (still poisoned, when it was), never half cleared.
    fn replay_posts(&mut self, io: &dyn StoreIo, full: bool) -> io::Result<()> {
        let from = if full { 0 } else { self.durable_len };
        let replayed = match io.open_read(&self.posts_path()) {
            Ok(mut file) => {
                let len = file.metadata()?.len();
                if len < from {
                    // Shorter than what was replayed: another instance
                    // repaired the file. Start over.
                    return self.replay_posts(io, true);
                }
                file.seek(SeekFrom::Start(from))?;
                let next_seq = if full { 1 } else { self.head() + 1 };
                replay::replay(
                    BufReader::with_capacity(256 * 1024, file),
                    from,
                    next_seq,
                    &self.id,
                )?
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && full => {
                replay::Replayed::default()
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if full {
            self.clear_posts();
            self.health = Health::Ok;
        }
        for rec in replayed.recs {
            self.index(rec);
        }
        self.durable_len = replayed.end;
        self.torn = replayed.torn;
        if let Some(corrupt) = replayed.corrupt {
            log::warn!(
                "timeline: board {} posts.jsonl line {} is corrupt ({}); the board is read-only",
                self.id,
                corrupt.line,
                corrupt.message
            );
            self.torn = 0;
            self.health = Health::Corrupt(corrupt);
        }
        Ok(())
    }

    /// Re-reads what another instance may have written: `meta.json` when
    /// its stamp changed, new lines of `posts.jsonl`. `true`: `meta.json`
    /// was re-read (its run request ids need indexing).
    fn catch_up(&mut self, io: &dyn StoreIo) -> io::Result<bool> {
        let stamp = stamp(&self.dir.join(META))?;
        let meta_changed = stamp.is_some() && stamp != self.meta_stamp;
        if meta_changed {
            self.meta = read_meta(&self.dir.join(META))?;
            self.meta_stamp = stamp;
        }
        if matches!(self.health, Health::Corrupt(_)) {
            return Ok(meta_changed);
        }
        let len = match fs::metadata(self.posts_path()) {
            Ok(meta) => meta.len(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err),
        };
        if len != self.durable_len + self.torn {
            self.replay_posts(io, false)?;
        }
        Ok(meta_changed)
    }

    fn rec(&self, seq: u64) -> &Rec {
        &self.recs[(seq - 1) as usize]
    }

    fn summary(&self, rec: &Rec) -> PostSummary {
        let mut summary = rec.summary.clone();
        if let Some(newer) = self.superseded_by.get(&summary.post_id) {
            summary.superseded_by = newer.clone();
        }
        summary
    }

    fn read_line(&self, rec: &Rec) -> Result<Vec<u8>, TimelineError> {
        let file = File::open(self.posts_path()).map_err(io_error)?;
        read_at(&file, rec)
    }

    fn run_info(&self, run: &RunMeta) -> RunInfo {
        let mut info = run.info.clone();
        info.summary_post_id = self.run_summary.get(&info.run_id).cloned();
        info
    }

    /// The board's runs in registration order.
    fn runs_in_order(&self) -> Vec<&RunMeta> {
        let mut runs: Vec<&RunMeta> = self.meta.runs.values().collect();
        runs.sort_by_key(|run| run.order);
        runs
    }

    /// The post a request id of `producer` was answered with: its own
    /// post (key A), or another request's post it was a key-B duplicate
    /// of. The flag is `true` for the latter.
    fn request_post(&self, producer: &str, request: &str) -> Option<(usize, bool)> {
        if let Some(&idx) = self.key_a.get(&(producer.to_owned(), request.to_owned())) {
            return Some((idx, false));
        }
        let post = self.meta.request_aliases.get(producer)?.get(request)?;
        self.by_id.get(post).map(|&idx| (idx, true))
    }

    /// Makes sure the line of `recs[idx]` is durable before a re-send of it
    /// is acknowledged (§8.1): a line this store did not see synced is
    /// synced now. A failed sync poisons the board, as a failed append does.
    fn ensure_durable(&mut self, io: &dyn StoreIo, idx: usize) -> Result<(), TimelineError> {
        let rec = &self.recs[idx];
        if rec.offset + rec.len <= self.synced_len {
            return Ok(());
        }
        let writer = self.writer.as_ref().expect("a post holds the lock");
        if let Err(err) = io.sync_data(&writer.posts) {
            self.health = Health::Poisoned;
            return Err(io_error(err));
        }
        self.synced_len = self.durable_len;
        Ok(())
    }
}

fn read_at(file: &File, rec: &Rec) -> Result<Vec<u8>, TimelineError> {
    let mut buf = vec![0; rec.len as usize];
    file.read_exact_at(&mut buf, rec.offset).map_err(io_error)?;
    Ok(buf)
}

const META: &str = "meta.json";
const POSTS: &str = "posts.jsonl";
const LOCK: &str = "lock";
const QUARANTINE: &str = "quarantine";

// ---------------------------------------------------------------------------
// Store

pub(crate) struct Store {
    root: PathBuf,
    instance_id: String,
    limits: StoreLimits,
    io: Box<dyn StoreIo>,
    boards: HashMap<String, Board>,
    /// Every board in the directory was loaded once (a failed first scan is
    /// tried again by the next call).
    scanned: bool,
    /// `registerRun` request id → (board, run).
    run_requests: HashMap<String, (String, String)>,
    /// Set by the service thread's tests: the name of the thread each call
    /// ran on.
    #[cfg(test)]
    pub(crate) thread_probe: Option<Arc<parking_lot::Mutex<Vec<String>>>>,
}

impl Store {
    /// Opens (creating, `0700`) the boards directory and replays every
    /// board in it. A board whose files cannot be read is left out with a
    /// warning; one with a corrupt line is kept read-only. (The service
    /// does the same in two steps, [`Store::create`] then [`Store::scan`].)
    #[cfg(test)]
    pub(crate) fn open(
        root: PathBuf,
        instance_id: String,
        limits: StoreLimits,
        io: Box<dyn StoreIo>,
    ) -> io::Result<Self> {
        let mut store = Self::create(root, instance_id, limits, io)?;
        store.scan()?;
        Ok(store)
    }

    /// Creates (or takes over) the boards directory, `0700`, with every
    /// directory entry it adds made durable, and loads no board yet: an
    /// error here means the store cannot be used at all.
    pub(crate) fn create(
        root: PathBuf,
        instance_id: String,
        limits: StoreLimits,
        io: Box<dyn StoreIo>,
    ) -> io::Result<Self> {
        create_private_dir(&*io, &root)?;
        Ok(Self {
            root,
            instance_id,
            limits,
            io,
            boards: HashMap::new(),
            scanned: false,
            run_requests: HashMap::new(),
            #[cfg(test)]
            thread_probe: None,
        })
    }

    /// Loads boards that appeared in the directory since the last scan
    /// (created by another instance).
    pub(crate) fn scan(&mut self) -> io::Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !valid_id(IdKind::Board, &id) || self.boards.contains_key(&id) {
                continue;
            }
            if let Err(err) = self.load(&id) {
                log::warn!("timeline: board {id} not loaded: {err}");
            }
        }
        self.scanned = true;
        Ok(())
    }

    /// Loads one board from disk. `Ok(false)`: no such board (yet).
    fn load(&mut self, id: &str) -> io::Result<bool> {
        let dir = self.root.join(id);
        let meta_path = dir.join(META);
        let meta_stamp = stamp(&meta_path)?;
        if meta_stamp.is_none() {
            // Not a board, or one another instance is still creating.
            return Ok(false);
        }
        let meta = read_meta(&meta_path)?;
        if meta.board_id != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("meta.json names board {:?}", meta.board_id),
            ));
        }
        let mut board = Board::new(id.to_owned(), dir, meta, meta_stamp, None);
        board.replay_posts(&*self.io, true)?;
        index_runs(&mut self.run_requests, &board);
        self.boards.insert(id.to_owned(), board);
        Ok(true)
    }

    /// One call, answered with its result object.
    pub(crate) fn handle(
        &mut self,
        call: TimelineCall,
        panels: &[Panel],
    ) -> Result<Value, TimelineError> {
        #[cfg(test)]
        if let Some(probe) = &self.thread_probe {
            probe
                .lock()
                .push(std::thread::current().name().unwrap_or_default().to_owned());
        }
        fn json<T: Serialize>(value: T) -> Result<Value, TimelineError> {
            serde_json::to_value(value)
                .map_err(|err| TimelineError::new(TimelineErrorKind::IoError, err.to_string()))
        }
        if !self.scanned {
            self.scan().map_err(io_error)?;
        }
        match call {
            TimelineCall::Context(p) => json(self.context(&p, panels)?),
            TimelineCall::Boards(p) => json(self.boards(&p, panels)?),
            TimelineCall::RegisterRun(p) => json(self.register_run(&p, panels)?),
            TimelineCall::SetPublishPolicy(p) => json(self.set_publish_policy(&p)?),
            TimelineCall::Post(p) => json(self.post(p)?),
            TimelineCall::List(p) => json(self.list(&p)?),
            TimelineCall::Get(p) => json(self.get(&p)?),
            TimelineCall::Changes(p) => json(self.changes(&p)?),
        }
    }

    // -- board access ------------------------------------------------------

    /// The board for reading: loaded (from disk if another instance made
    /// it) and caught up with what another instance wrote.
    fn read_board(&mut self, id: &str) -> Result<&mut Board, TimelineError> {
        if !valid_id(IdKind::Board, id) {
            return Err(TimelineError::invalid_params(format!(
                "boardId {id:?} is not bd-<16 lowercase hex digits>"
            )));
        }
        if !self.boards.contains_key(id) && !self.load(id).map_err(io_error)? {
            return Err(not_found(format!("no board {id}")));
        }
        // Its directory removed by hand (the `storage_full` advice) while
        // this store had it loaded, write lock or not: no board any more.
        if stamp(&self.boards[id].dir.join(META))
            .map_err(io_error)?
            .is_none()
        {
            self.forget(id);
            return Err(not_found(format!("no board {id}")));
        }
        let io = &*self.io;
        let board = self.boards.get_mut(id).expect("loaded above");
        if board.writer.is_none() && board.catch_up(io).map_err(io_error)? {
            index_runs(&mut self.run_requests, board);
        }
        Ok(self.boards.get_mut(id).expect("loaded above"))
    }

    /// Drops a board whose files are gone, with the run request ids that
    /// pointed into it (its lock, when held, is released).
    fn forget(&mut self, id: &str) {
        self.boards.remove(id);
        self.run_requests.retain(|_, (board, _)| board != id);
    }

    /// The board for writing: locked (`busy` when another instance holds
    /// it), caught up, repaired after a failed write or a torn tail, and
    /// refused while corrupt.
    fn write_board(&mut self, id: &str) -> Result<&mut Board, TimelineError> {
        self.read_board(id)?;
        let io = &*self.io;
        let board = self.boards.get_mut(id).expect("read_board loaded it");
        if board.writer.is_none() {
            let lock = open_private(io, &board.dir.join(LOCK), false).map_err(io_error)?;
            if let Err(err) = io.lock(&lock) {
                return Err(if err.kind() == io::ErrorKind::WouldBlock {
                    TimelineError::new(
                        TimelineErrorKind::Busy,
                        format!("board {id} is open for writing in another Noa"),
                    )
                } else {
                    io_error(err)
                });
            }
            let posts = open_private(io, &board.posts_path(), true).map_err(io_error)?;
            // Whatever the previous holder wrote before we locked. The
            // writer is kept only once that is read: holding it means the
            // indexes are current, so a later call skips this read, and a
            // stale index would hand out a seq or a run another instance
            // already used. On an error the lock is dropped here.
            let meta_changed = board.catch_up(io).map_err(io_error)?;
            board.writer = Some(Writer { _lock: lock, posts });
            if meta_changed {
                index_runs(&mut self.run_requests, board);
            }
        }
        match &board.health {
            Health::Ok => {}
            Health::Corrupt(corrupt) => {
                let mut err = TimelineError::new(
                    TimelineErrorKind::CorruptStore,
                    format!(
                        "board {id} posts.jsonl line {} is corrupt ({}); it takes no writes until \
                         the file is repaired by hand",
                        corrupt.line, corrupt.message
                    ),
                );
                err.line = Some(corrupt.line);
                return Err(err);
            }
            Health::Poisoned => {
                let writer = board.writer.as_ref().expect("locked above");
                writer
                    .posts
                    .set_len(board.durable_len)
                    .and_then(|()| io.sync_data(&writer.posts))
                    .map_err(io_error)?;
                board.replay_posts(io, true).map_err(io_error)?;
                // The file was synced at the length it was replayed from.
                board.synced_len = board.durable_len;
                if let Health::Corrupt(_) = board.health {
                    return self.write_board(id);
                }
            }
        }
        if board.torn > 0 {
            repair_torn_tail(io, board).map_err(io_error)?;
        }
        Ok(self.boards.get_mut(id).expect("loaded above"))
    }

    /// Creates a board (exclusively: a minted id whose directory exists is
    /// drawn again), locked by this store.
    fn create_board(&mut self, name: String, roots: Vec<Binding>) -> Result<String, TimelineError> {
        let io = &*self.io;
        let (id, dir) = loop {
            let id = ids::mint(IdKind::Board, |id| self.boards.contains_key(id));
            let dir = self.root.join(&id);
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => break (id, dir),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(io_error(err)),
            }
        };
        let created = (|| -> io::Result<Board> {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
            let lock = open_private(io, &dir.join(LOCK), false)?;
            // A fresh directory has no other holder.
            io.lock(&lock)?;
            let posts = open_private(io, &dir.join(POSTS), true)?;
            io.sync_data(&posts)?;
            let meta = Meta {
                board_id: id.clone(),
                created_at: ids::now_utc(),
                name,
                request_aliases: BTreeMap::new(),
                roots,
                runs: BTreeMap::new(),
                schema_version: SCHEMA_VERSION,
                workers: BTreeMap::new(),
            };
            let bytes = serde_json::to_vec_pretty(&meta).map_err(io::Error::other)?;
            write_private_atomic(io, &dir.join(META), &bytes)?;
            io.sync_dir(&self.root)?;
            let meta_stamp = stamp(&dir.join(META))?;
            Ok(Board::new(
                id.clone(),
                dir.clone(),
                meta,
                meta_stamp,
                Some(Writer { _lock: lock, posts }),
            ))
        })();
        match created {
            Ok(board) => {
                self.boards.insert(id.clone(), board);
                Ok(id)
            }
            Err(err) => {
                // Without meta.json the directory is no board; remove it so
                // a later scan does not trip over it.
                let _ = fs::remove_dir_all(&dir);
                Err(io_error(err))
            }
        }
    }

    // -- live bindings -----------------------------------------------------

    fn binding_of(&self, panel: &Panel) -> Binding {
        Binding {
            at: ids::now_utc(),
            pane_id: panel.pane_id,
            server_instance_id: self.instance_id.clone(),
            surface_id: panel.surface_id.clone(),
        }
    }

    /// `binding` names this live `panel`: same instance, pane and surface.
    fn binds(&self, binding: &Binding, panel: &Panel) -> bool {
        binding.server_instance_id == self.instance_id
            && binding.pane_id == panel.pane_id
            && binding.surface_id == panel.surface_id
    }

    /// `binding` names a pane of the current listing (the Glossary's live
    /// link).
    fn live(&self, binding: &Binding, panels: &[Panel]) -> Option<WireId> {
        panels
            .iter()
            .find(|panel| self.binds(binding, panel))
            .map(|panel| panel.pane_id)
    }

    /// The board `panel` is live-bound to: as a worker first (its children
    /// share its board, D9), else as a root; the latest binding wins.
    fn board_of_pane(&self, panel: &Panel) -> Option<(String, Option<String>)> {
        let mut worker: Option<(&str, &str, &str)> = None;
        let mut root: Option<(&str, &str)> = None;
        for board in self.boards.values() {
            for (worker_id, meta) in &board.meta.workers {
                for binding in meta.bindings.iter().filter(|b| self.binds(b, panel)) {
                    if worker.is_none_or(|(_, _, at)| instant_after(&binding.at, at)) {
                        worker = Some((&board.id, worker_id, &binding.at));
                    }
                }
            }
            for binding in board.meta.roots.iter().filter(|b| self.binds(b, panel)) {
                if root.is_none_or(|(_, at)| instant_after(&binding.at, at)) {
                    root = Some((&board.id, &binding.at));
                }
            }
        }
        match (worker, root) {
            (Some((board, worker, _)), _) => Some((board.to_owned(), Some(worker.to_owned()))),
            (None, Some((board, _))) => Some((board.to_owned(), None)),
            (None, None) => None,
        }
    }

    fn archived(&self, board: &Board, panels: &[Panel]) -> bool {
        let root_live = board
            .meta
            .roots
            .iter()
            .any(|b| self.live(b, panels).is_some());
        let worker_live = board
            .meta
            .workers
            .values()
            .filter_map(|w| w.bindings.last())
            .any(|b| self.live(b, panels).is_some());
        !(root_live || worker_live)
    }

    // -- registerRun ---------------------------------------------------------

    fn register_run(
        &mut self,
        p: &RegisterRunParams,
        panels: &[Panel],
    ) -> Result<RegisterRunResult, TimelineError> {
        if p.publish_policy.owner == PublishOwner::Worker
            && p.publish_policy.producer_id.is_none()
            && p.worker_pane_id.is_none()
        {
            return Err(TimelineError::invalid_params(
                "publishPolicy owner worker without producerId needs workerPaneId: the producer \
                 is the run's worker",
            ));
        }
        let canon = RunCanon::of(p);
        let open = |pane: WireId| panels.iter().find(|panel| panel.pane_id == pane);

        // A request id already registered answers with its run, even when
        // its panes have closed since: the caller lost the first answer
        // and asks again. An orchestrator still open is rebound.
        if let Some(found) = self.known_request(p)? {
            let orchestrator = p.orchestrator_pane_id.and_then(open);
            return self.register_again(found, &canon, orchestrator);
        }

        let find = |what: &str, pane: Option<WireId>| -> Result<Option<&Panel>, TimelineError> {
            pane.map(|pane| {
                open(pane).ok_or_else(|| not_found(format!("{what} pane {} is not open", pane.0)))
            })
            .transpose()
        };
        let orchestrator = find("orchestrator", p.orchestrator_pane_id)?;
        let worker_panel = find("worker", p.worker_pane_id)?;
        let unknown_retry = |retry_of: &str| {
            TimelineError::invalid_params(format!(
                "retryOfRunId {retry_of} is not a run of this board"
            ))
        };
        let existing = match (&p.board_id, orchestrator) {
            (Some(board), _) => Some(board.clone()),
            (None, Some(panel)) => self.board_of_pane(panel).map(|(board, _)| board),
            (None, None) => None,
        };
        let board_id = match existing {
            Some(board) => {
                self.write_board(&board)?;
                // Another instance may have registered this request id
                // between our read and the lock.
                if let Some(found) = self.run_requests.get(&p.request_id).cloned() {
                    return self.register_again(found, &canon, orchestrator);
                }
                board
            }
            None => {
                if let Some(retry_of) = &p.retry_of_run_id {
                    return Err(unknown_retry(retry_of));
                }
                let name = p
                    .board_name
                    .clone()
                    .or_else(|| orchestrator.and_then(|panel| panel.label.clone()))
                    .unwrap_or_else(|| "board".to_owned());
                let roots = orchestrator
                    .map(|panel| vec![self.binding_of(panel)])
                    .unwrap_or_default();
                self.create_board(name, roots)?
            }
        };

        // Built from the meta.json read under the lock.
        let board = &self.boards[&board_id];
        if let Some(retry_of) = &p.retry_of_run_id
            && !board.meta.runs.contains_key(retry_of)
        {
            return Err(unknown_retry(retry_of));
        }
        let mut meta = board.meta.clone();
        if let Some(panel) = orchestrator
            && !self.root_bound(&meta, panel)
        {
            meta.roots.push(self.binding_of(panel));
        }
        let worker_id = worker_panel.map(|panel| {
            let reuse = meta.workers.iter().find_map(|(id, worker)| {
                worker
                    .bindings
                    .last()
                    .is_some_and(|b| self.binds(b, panel))
                    .then(|| id.clone())
            });
            reuse.unwrap_or_else(|| {
                let id = ids::mint(IdKind::Worker, |id| {
                    self.boards
                        .values()
                        .any(|board| board.meta.workers.contains_key(id))
                });
                meta.workers.insert(
                    id.clone(),
                    WorkerMeta {
                        agent_kind: panel.agent.as_ref().and_then(|agent| agent.kind.clone()),
                        bindings: vec![self.binding_of(panel)],
                        label: p.label.clone().or_else(|| panel.label.clone()),
                        role: p.role.clone(),
                    },
                );
                id
            })
        });
        let producer_id = resolve_producer(&p.publish_policy, worker_id.as_deref())?;
        let run_id = ids::mint(IdKind::Run, |id| {
            self.boards
                .values()
                .any(|board| board.meta.runs.contains_key(id))
        });
        let info = RunInfo {
            board_id: board_id.clone(),
            created_at: ids::now_utc(),
            publish_policy: PublishPolicy {
                owner: p.publish_policy.owner,
                producer_id: Some(producer_id.clone()),
            },
            retry_of_run_id: p.retry_of_run_id.clone(),
            run_id: run_id.clone(),
            summary_post_id: None,
            task_ref: p.task_ref.clone(),
            worker_id: worker_id.clone(),
        };
        let order = meta.runs.values().map(|run| run.order).max().unwrap_or(0) + 1;
        meta.runs.insert(
            run_id.clone(),
            RunMeta {
                canon,
                info,
                order,
                policy_history: Vec::new(),
                request_id: p.request_id.clone(),
            },
        );
        let io = &*self.io;
        let board = self.boards.get_mut(&board_id).expect("resolved above");
        board.save_meta(io, meta)?;
        self.run_requests
            .insert(p.request_id.clone(), (board_id.clone(), run_id.clone()));
        Ok(RegisterRunResult {
            board_id,
            created: true,
            producer_id,
            run_id,
            worker_id,
        })
    }

    /// The (board, run) a `registerRun`'s request id already registered.
    /// The board the call names is read first when the index misses:
    /// another instance may have registered the request there.
    fn known_request(
        &mut self,
        p: &RegisterRunParams,
    ) -> Result<Option<(String, String)>, TimelineError> {
        if let Some(found) = self.run_requests.get(&p.request_id) {
            return Ok(Some(found.clone()));
        }
        if let Some(board) = &p.board_id {
            self.read_board(board)?;
            return Ok(self.run_requests.get(&p.request_id).cloned());
        }
        Ok(None)
    }

    /// A `registerRun` whose request id is known: the same run when the
    /// payload is, rebinding the orchestrator as the board's root (a widget
    /// reopened after a restart); `conflict` otherwise.
    fn register_again(
        &mut self,
        (board_id, run_id): (String, String),
        canon: &RunCanon,
        orchestrator: Option<&Panel>,
    ) -> Result<RegisterRunResult, TimelineError> {
        self.read_board(&board_id)?;
        let board = &self.boards[&board_id];
        let run = board_run(board, &run_id)?;
        if !run.canon.matches(canon, &board_id) {
            let mut err = TimelineError::new(
                TimelineErrorKind::Conflict,
                format!(
                    "requestId {} already registered run {run_id} with another payload",
                    run.request_id
                ),
            );
            err.existing_run_id = Some(run_id);
            return Err(err);
        }
        if let Some(panel) = orchestrator
            && !self.root_bound(&board.meta, panel)
        {
            // The binding goes onto the meta.json read under the lock, never
            // onto the copy read before it: another instance may have
            // registered a run in between.
            self.write_board(&board_id)?;
            let board = &self.boards[&board_id];
            if !self.root_bound(&board.meta, panel) {
                let mut meta = board.meta.clone();
                meta.roots.push(self.binding_of(panel));
                let io = &*self.io;
                let board = self.boards.get_mut(&board_id).expect("read above");
                board.save_meta(io, meta)?;
            }
        }
        let board = &self.boards[&board_id];
        let run = board_run(board, &run_id)?;
        Ok(RegisterRunResult {
            board_id: board_id.clone(),
            created: false,
            producer_id: run
                .info
                .publish_policy
                .producer_id
                .clone()
                .unwrap_or_default(),
            run_id: run_id.clone(),
            worker_id: run.info.worker_id.clone(),
        })
    }

    /// `panel` is already live-bound to the board described by `meta`: as a
    /// root, or as a worker spawning children.
    fn root_bound(&self, meta: &Meta, panel: &Panel) -> bool {
        meta.roots.iter().any(|b| self.binds(b, panel))
            || meta
                .workers
                .values()
                .any(|w| w.bindings.iter().any(|b| self.binds(b, panel)))
    }

    // -- setPublishPolicy ----------------------------------------------------

    fn set_publish_policy(&mut self, p: &SetPublishPolicyParams) -> Result<RunInfo, TimelineError> {
        self.write_board(&p.board_id)?;
        let io = &*self.io;
        let board = self.boards.get_mut(&p.board_id).expect("locked above");
        let Some(run) = board.meta.runs.get(&p.run_id) else {
            return Err(not_found(format!(
                "no run {} on board {}",
                p.run_id, p.board_id
            )));
        };
        if let Some(change) = run
            .policy_history
            .iter()
            .find(|change| change.request_id == p.request_id)
        {
            if change.policy == p.publish_policy {
                return Ok(board.run_info(run));
            }
            let mut err = TimelineError::new(
                TimelineErrorKind::Conflict,
                format!(
                    "requestId {} already changed run {}'s policy to another one",
                    p.request_id, p.run_id
                ),
            );
            err.existing_run_id = Some(p.run_id.clone());
            return Err(err);
        }
        let producer = resolve_producer(&p.publish_policy, run.info.worker_id.as_deref())?;
        let mut meta = board.meta.clone();
        let run = meta.runs.get_mut(&p.run_id).expect("checked above");
        run.info.publish_policy = PublishPolicy {
            owner: p.publish_policy.owner,
            producer_id: Some(producer),
        };
        run.policy_history.push(PolicyChange {
            at: ids::now_utc(),
            policy: p.publish_policy.clone(),
            request_id: p.request_id.clone(),
        });
        board.save_meta(io, meta)?;
        Ok(board.run_info(&board.meta.runs[&p.run_id]))
    }

    // -- post ------------------------------------------------------------------

    fn post(&mut self, p: PostParams) -> Result<PostResult, TimelineError> {
        // Repeated from `TimelineCall::parse` for callers that build params
        // directly (the store trusts no path component it did not check).
        p.post.validate()?;
        if p.run_id.is_none()
            && !UNREGISTERED_NAMESPACES.contains(&p.post.source_ref.namespace.as_str())
        {
            return Err(TimelineError::invalid_params(format!(
                "a post without runId needs sourceRef.namespace {}",
                UNREGISTERED_NAMESPACES.join(" or ")
            )));
        }
        let max_board_bytes = self.limits.max_board_bytes;
        self.write_board(&p.board_id)?;
        let io = &*self.io;
        let board = self.boards.get_mut(&p.board_id).expect("locked above");
        let input = p.post.normalized();
        let run_id = p.run_id.as_deref();
        let canon_a = canon_bytes(run_id, Some(&p.producer_id), &input);
        let canon_b = || canon_bytes(run_id, None, &input);

        // Key A: this request id of this producer, answered before with
        // its own post or, as a key-B duplicate, with another one (which
        // has another producer's or request's canon: compare without them).
        if let Some((idx, alias)) = board.request_post(&p.producer_id, &p.request_id) {
            let stored = stored_canon(board, idx)?;
            let same = if alias {
                stored.key_b() == canon_b()
            } else {
                stored.key_a() == canon_a
            };
            if same {
                board.ensure_durable(io, idx)?;
            }
            return replay_or_conflict(board, idx, same, "requestId");
        }
        let source = SourceKey::of(&input.source_ref);
        if let Some(&idx) = board.key_b.get(&source) {
            let stored = stored_canon(board, idx)?;
            let same = stored.key_b() == canon_b();
            if same {
                board.ensure_durable(io, idx)?;
                // The request id is used now: its re-send is this post
                // again, and another payload under it is a conflict.
                let mut meta = board.meta.clone();
                meta.request_aliases
                    .entry(p.producer_id.clone())
                    .or_default()
                    .insert(
                        p.request_id.clone(),
                        board.recs[idx].summary.post_id.clone(),
                    );
                board.save_meta(io, meta)?;
            }
            return replay_or_conflict(board, idx, same, "sourceRef");
        }

        let (author, producer_kind, worker_id) = match run_id {
            Some(run_id) => {
                let Some(run) = board.meta.runs.get(run_id) else {
                    return Err(not_found(format!(
                        "no run {run_id} on board {}",
                        p.board_id
                    )));
                };
                if run.info.publish_policy.producer_id.as_deref() != Some(&p.producer_id) {
                    return Err(TimelineError::invalid_params(format!(
                        "producer not registered for run: {run_id} takes posts from {}",
                        run.info
                            .publish_policy
                            .producer_id
                            .as_deref()
                            .unwrap_or("nobody")
                    )));
                }
                if input.kind == PostKind::Summary
                    && input.supersedes_post_id.is_none()
                    && let Some(existing) = board.run_summary.get(run_id)
                {
                    let mut err = TimelineError::new(
                        TimelineErrorKind::Conflict,
                        format!(
                            "run {run_id} already has its summary {existing}; a correction names \
                             it in supersedesPostId"
                        ),
                    );
                    err.existing_post_id = Some(existing.clone());
                    return Err(err);
                }
                let worker = run
                    .info
                    .worker_id
                    .as_ref()
                    .and_then(|id| board.meta.workers.get(id));
                let author = noa_ipc::timeline::Author {
                    agent_kind: worker.and_then(|w| w.agent_kind.clone()),
                    label: worker
                        .and_then(|w| w.label.clone())
                        .or_else(|| run.canon.label.clone()),
                    role: worker
                        .and_then(|w| w.role.clone())
                        .or_else(|| run.canon.role.clone()),
                    worker_id: run.info.worker_id.clone(),
                };
                let kind = match run.info.publish_policy.owner {
                    PublishOwner::Worker => ProducerKind::Worker,
                    PublishOwner::Runner => ProducerKind::Runner,
                    PublishOwner::Manual => ProducerKind::Human,
                };
                (author, kind, run.info.worker_id.clone())
            }
            None => {
                let hint = input.author.clone().unwrap_or_default();
                let author = noa_ipc::timeline::Author {
                    agent_kind: None,
                    label: hint.label,
                    role: hint.role,
                    worker_id: None,
                };
                (author, ProducerKind::Human, None)
            }
        };
        if let Some(retry_of) = &input.retry_of_run_id
            && !board.meta.runs.contains_key(retry_of)
        {
            return Err(TimelineError::invalid_params(format!(
                "retryOfRunId {retry_of} is not a run of this board"
            )));
        }
        for post in input
            .based_on_post_ids
            .iter()
            .chain(input.supersedes_post_id.iter())
        {
            if !board.by_id.contains_key(post) {
                return Err(TimelineError::invalid_params(format!(
                    "post {post} is not on this board"
                )));
            }
        }
        // A correction is another source event than the post it corrects
        // (AC-1 rule 5): a new eventId, whatever else of its sourceRef
        // changed.
        if let Some(old) = &input.supersedes_post_id
            && board.recs[board.by_id[old]].source.event_id == input.source_ref.event_id
        {
            return Err(TimelineError::invalid_params(format!(
                "a correction needs a new sourceRef.eventId: {} is the eventId of {old}",
                input.source_ref.event_id
            )));
        }

        let post_id = ids::mint(IdKind::Post, |id| board.by_id.contains_key(id));
        let seq = Seq(board.head() + 1);
        let created_at = ids::now_utc();
        let mut stored_input = input;
        stored_input.author = None;
        let post = Post {
            author,
            board_id: p.board_id.clone(),
            created_at: created_at.clone(),
            post_id: post_id.clone(),
            producer: Producer {
                id: p.producer_id.clone(),
                kind: producer_kind,
            },
            provenance: if run_id.is_some() {
                Provenance::Registered
            } else {
                Provenance::Unverified
            },
            request_id: p.request_id.clone(),
            run_id: p.run_id.clone(),
            schema_version: SCHEMA_VERSION,
            seq,
            worker_id,
            input: stored_input,
        };
        let line = replay::encode_line(&canon_a, &post);
        if board.durable_len + line.len() as u64 > max_board_bytes {
            return Err(TimelineError::new(
                TimelineErrorKind::StorageFull,
                format!(
                    "board {} would exceed {max_board_bytes} bytes; export it (+ctl timeline \
                     export) and remove its directory by hand to free the space",
                    p.board_id
                ),
            ));
        }
        let writer = board.writer.as_mut().expect("locked above");
        if let Err(err) = io
            .write_all(&mut writer.posts, &line)
            .and_then(|()| io.sync_data(&writer.posts))
        {
            board.health = Health::Poisoned;
            return Err(io_error(err));
        }
        let offset = board.durable_len;
        board.index(Rec::of_post(&post, offset, line.len() as u64));
        board.synced_len = board.durable_len;
        Ok(PostResult {
            created_at,
            duplicate: false,
            post_id,
            seq,
        })
    }

    // -- reads -------------------------------------------------------------------

    fn list(&mut self, p: &ListParams) -> Result<PageResult, TimelineError> {
        let board = &*self.read_board(&p.board_id)?;
        let head = board.head();
        let limit = p.limit.unwrap_or(LIST_DEFAULT);
        let start = p
            .before_seq
            .map_or(head, |before| before.0.saturating_sub(1).min(head));
        let query = p.query.as_deref().and_then(Query::new);
        let filter = p.filter.clone().unwrap_or_default();
        let file = match &query {
            Some(_) => Some(File::open(board.posts_path()).map_err(io_error)?),
            None => None,
        };
        let matches = |rec: &Rec| -> Result<bool, TimelineError> {
            if !search::filter_matches(&filter, &rec.summary) {
                return Ok(false);
            }
            match (&query, &file) {
                (Some(query), Some(file)) => query.matches(rec, || read_at(file, rec)),
                _ => Ok(true),
            }
        };
        let mut page = Page::new(head);
        let mut seq = start;
        while seq >= 1 {
            let rec = board.rec(seq);
            seq -= 1;
            if !matches(rec)? {
                continue;
            }
            if page.posts.len() == limit || !page.push(board.summary(rec)) {
                page.has_more = true;
                break;
            }
        }
        let next_before = page.posts.last().map(|post| post.seq);
        Ok(page.finish(next_before, None))
    }

    fn changes(&mut self, p: &ChangesParams) -> Result<PageResult, TimelineError> {
        let board = &*self.read_board(&p.board_id)?;
        let head = board.head();
        let limit = p.limit.unwrap_or(CHANGES_DEFAULT);
        let mut page = Page::new(head);
        for seq in p.after_seq.0.saturating_add(1)..=head {
            if page.posts.len() == limit || !page.push(board.summary(board.rec(seq))) {
                page.has_more = true;
                break;
            }
        }
        let next_after = page.posts.last().map_or(p.after_seq, |post| post.seq);
        Ok(page.finish(None, Some(next_after)))
    }

    fn get(&mut self, p: &GetParams) -> Result<GetResult, TimelineError> {
        let board = self.read_board(&p.board_id)?;
        let Some(&idx) = board.by_id.get(&p.post_id) else {
            return Err(not_found(format!(
                "no post {} on board {}",
                p.post_id, p.board_id
            )));
        };
        let rec = &board.recs[idx];
        let line = board.read_line(rec)?;
        let full: PostLine = serde_json::from_slice(&line).map_err(|err| {
            let mut e = TimelineError::new(
                TimelineErrorKind::CorruptStore,
                format!("post {} does not decode: {err}", p.post_id),
            );
            e.line = Some(rec.summary.seq.0);
            e
        })?;
        Ok(GetResult {
            post: full.post,
            superseded_by: board
                .superseded_by
                .get(&p.post_id)
                .cloned()
                .unwrap_or_default(),
        })
    }

    fn boards(
        &mut self,
        p: &BoardsParams,
        panels: &[Panel],
    ) -> Result<BoardsResult, TimelineError> {
        self.scan().map_err(io_error)?;
        let ids: Vec<String> = self.boards.keys().cloned().collect();
        // One board that cannot be read is left out, as `scan` leaves out
        // one it cannot load, instead of failing the whole listing; one
        // whose directory is gone was dropped by `read_board`.
        let mut unreadable: HashSet<String> = HashSet::new();
        for id in ids {
            match self.read_board(&id) {
                Ok(_) => {}
                Err(err) if err.kind == TimelineErrorKind::NotFound => {}
                Err(err) => {
                    log::warn!(
                        "timeline: board {id} left out of the listing: {}",
                        err.message
                    );
                    unreadable.insert(id);
                }
            }
        }
        let mut infos: Vec<BoardInfo> = self
            .boards
            .values()
            .filter(|board| !unreadable.contains(&board.id))
            .map(|board| BoardInfo {
                archived: self.archived(board, panels),
                board_id: board.id.clone(),
                bytes: board.durable_len,
                created_at: board.meta.created_at.clone(),
                head_seq: Seq(board.head()),
                last_activity_at: board.last_activity.clone(),
                name: board.meta.name.clone(),
                posts: board.head(),
            })
            .filter(|info| p.before.as_ref().is_none_or(|cursor| cursor.admits(info)))
            .collect();
        infos.sort_by(board_listing_order);
        let limit = p.limit.unwrap_or(BOARDS_DEFAULT);
        let has_more = infos.len() > limit;
        infos.truncate(limit);
        Ok(BoardsResult {
            next_before: infos.last().map(BoardInfo::cursor),
            boards: infos,
            has_more,
        })
    }

    fn context(
        &mut self,
        p: &ContextParams,
        panels: &[Panel],
    ) -> Result<ContextResult, TimelineError> {
        let (board_id, me) = match (&p.board_id, p.pane_id) {
            (Some(board), _) => (board.clone(), None),
            (None, Some(pane)) => {
                let Some(panel) = panels.iter().find(|panel| panel.pane_id == pane) else {
                    return Err(not_found(format!("pane {} is not open", pane.0)));
                };
                let Some((board, worker)) = self.board_of_pane(panel) else {
                    return Err(not_found(format!("pane {} is on no board", pane.0)));
                };
                (board, Some((pane, worker)))
            }
            (None, None) => {
                return Err(TimelineError::invalid_params(
                    "context takes exactly one of paneId and boardId",
                ));
            }
        };
        self.read_board(&board_id)?;
        let board = &self.boards[&board_id];
        let runs = board.runs_in_order();
        let runs_of = |worker: Option<&str>| -> Vec<RunInfo> {
            runs.iter()
                .filter(|run| run.info.worker_id.as_deref() == worker)
                .map(|run| board.run_info(run))
                .collect()
        };
        let me = me.map(|(pane, worker)| ContextSelf {
            pane_id: pane,
            role: if worker.is_some() { "worker" } else { "root" }.to_owned(),
            runs: runs_of(worker.as_deref()),
            worker_id: worker,
        });
        let mut workers: Vec<(&String, &WorkerMeta)> = board.meta.workers.iter().collect();
        workers.sort_by(|a, b| {
            let first = |w: &WorkerMeta| w.bindings.first().map(|b| b.at.clone());
            first(a.1).cmp(&first(b.1)).then_with(|| a.0.cmp(b.0))
        });
        let workers = workers
            .into_iter()
            .map(|(id, worker)| {
                let latest = worker.bindings.last();
                let pane_id = latest.and_then(|b| self.live(b, panels));
                ContextWorker {
                    agent_kind: worker.agent_kind.clone(),
                    label: worker.label.clone(),
                    live: pane_id.is_some(),
                    pane_id,
                    runs: runs_of(Some(id)),
                    surface_id: latest.and_then(|b| b.surface_id.clone()),
                    worker_id: id.clone(),
                }
            })
            .collect();
        let mut result = ContextResult {
            archived: self.archived(board, panels),
            board_id: board_id.clone(),
            head_seq: Seq(board.head()),
            me,
            name: board.meta.name.clone(),
            server_instance_id: self.instance_id.clone(),
            workers,
        };
        let order: HashMap<&str, u64> = runs
            .iter()
            .map(|run| (run.info.run_id.as_str(), run.order))
            .collect();
        bound_context(&mut result, |run_id| order[run_id]);
        Ok(result)
    }
}

/// Indexes the run request ids of `board`'s `meta.json`.
fn index_runs(requests: &mut HashMap<String, (String, String)>, board: &Board) {
    for (run_id, run) in &board.meta.runs {
        requests.insert(run.request_id.clone(), (board.id.clone(), run_id.clone()));
    }
}

/// Keeps a `context` result inside [`RESULT_BUDGET`]: the oldest runs go
/// first (the pane's own last [`SELF_RUNS_KEPT`] stay), then the workers
/// that are not live, oldest first, then the rest. The lists stay in
/// registration order (`order` of a run id); what is left out is older
/// than what is kept.
fn bound_context(result: &mut ContextResult, order: impl Fn(&str) -> u64) {
    let mut total = encoded_len(result);
    if total <= RESULT_BUDGET {
        return;
    }
    // (order, runId, owner) of every droppable run; owner None = self.
    let mut runs: Vec<(u64, String, Option<usize>)> = Vec::new();
    if let Some(me) = &result.me {
        let keep = me.runs.len().saturating_sub(SELF_RUNS_KEPT);
        for run in &me.runs[..keep] {
            runs.push((order(&run.run_id), run.run_id.clone(), None));
        }
    }
    for (i, worker) in result.workers.iter().enumerate() {
        for run in &worker.runs {
            runs.push((order(&run.run_id), run.run_id.clone(), Some(i)));
        }
    }
    runs.sort_by_key(|run| run.0);
    for (_, run_id, owner) in runs {
        if total <= RESULT_BUDGET {
            return;
        }
        let list = match owner {
            None => &mut result.me.as_mut().expect("collected from it").runs,
            Some(i) => &mut result.workers[i].runs,
        };
        let pos = list
            .iter()
            .position(|run| run.run_id == run_id)
            .expect("collected from it");
        let removed = list.remove(pos);
        total -= encoded_len(&removed) + 1;
    }
    while total > RESULT_BUDGET && !result.workers.is_empty() {
        let pos = result
            .workers
            .iter()
            .position(|worker| !worker.live)
            .unwrap_or(0);
        let removed = result.workers.remove(pos);
        total -= encoded_len(&removed) + 1;
    }
}

/// The encoded JSON size of `value`.
fn encoded_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

/// A `list`/`changes` page being filled under the byte budget.
struct Page {
    posts: Vec<PostSummary>,
    head: u64,
    has_more: bool,
    bytes: usize,
}

impl Page {
    fn new(head: u64) -> Self {
        // The result's frame with both cursors at their widest.
        let frame = PageResult {
            has_more: false,
            head_seq: Seq(u64::MAX),
            next_after_seq: Some(Seq(u64::MAX)),
            next_before_seq: Some(Seq(u64::MAX)),
            posts: Vec::new(),
        };
        Self {
            posts: Vec::new(),
            head,
            has_more: false,
            bytes: encoded_len(&frame),
        }
    }

    /// Adds `post` unless it would push the encoded result over the
    /// budget. `false`: not added.
    fn push(&mut self, post: PostSummary) -> bool {
        let size = encoded_len(&post) + 1;
        if self.bytes + size > RESULT_BUDGET {
            return false;
        }
        self.bytes += size;
        self.posts.push(post);
        true
    }

    fn finish(self, next_before: Option<Seq>, next_after: Option<Seq>) -> PageResult {
        PageResult {
            has_more: self.has_more,
            head_seq: Seq(self.head),
            next_after_seq: next_after,
            next_before_seq: next_before,
            posts: self.posts,
        }
    }
}

fn stored_canon(board: &Board, idx: usize) -> Result<replay::StoredCanon, TimelineError> {
    let rec = &board.recs[idx];
    let line = board.read_line(rec)?;
    serde_json::from_slice::<CanonLine>(&line)
        .map(|line| line.canon)
        .map_err(|err| {
            let mut e = TimelineError::new(
                TimelineErrorKind::CorruptStore,
                format!(
                    "stored canon of seq {} does not decode: {err}",
                    rec.summary.seq.0
                ),
            );
            e.line = Some(rec.summary.seq.0);
            e
        })
}

/// A key hit: the stored post again when the payload is the same,
/// `conflict` naming it otherwise. Nothing is written either way.
fn replay_or_conflict(
    board: &Board,
    idx: usize,
    same: bool,
    key: &str,
) -> Result<PostResult, TimelineError> {
    let summary = &board.recs[idx].summary;
    if same {
        return Ok(PostResult {
            created_at: summary.created_at.clone(),
            duplicate: true,
            post_id: summary.post_id.clone(),
            seq: summary.seq,
        });
    }
    let mut err = TimelineError::new(
        TimelineErrorKind::Conflict,
        format!(
            "this {key} already stored post {} with another payload",
            summary.post_id
        ),
    );
    err.existing_post_id = Some(summary.post_id.clone());
    Err(err)
}

/// The producer a policy names: its `producerId`, or for owner `worker`
/// the run's worker.
fn resolve_producer(policy: &PublishPolicy, worker: Option<&str>) -> Result<String, TimelineError> {
    match (&policy.producer_id, policy.owner, worker) {
        (Some(producer), _, _) => Ok(producer.clone()),
        (None, PublishOwner::Worker, Some(worker)) => Ok(worker.to_owned()),
        _ => Err(TimelineError::invalid_params(
            "publishPolicy needs a producerId: the run has no worker to default to",
        )),
    }
}

/// Run `run_id` of `board`; the request index named it, so a miss is a
/// `meta.json` changed by hand.
fn board_run<'b>(board: &'b Board, run_id: &str) -> Result<&'b RunMeta, TimelineError> {
    board
        .meta
        .runs
        .get(run_id)
        .ok_or_else(|| not_found(format!("no run {run_id} on board {}", board.id)))
}

fn not_found(message: String) -> TimelineError {
    TimelineError::new(TimelineErrorKind::NotFound, message)
}

fn io_error(err: io::Error) -> TimelineError {
    TimelineError::new(TimelineErrorKind::IoError, format!("timeline store: {err}"))
}

/// `a` is a later instant than `b` (both RFC 3339 UTC).
fn instant_after(a: &str, b: &str) -> bool {
    instant_order(a, b) == std::cmp::Ordering::Greater
}

/// Orders RFC 3339 UTC timestamps as instants (`…:05Z` = `…:05.000Z`):
/// the fixed-width date and time, then the fraction padded to nine digits.
pub(crate) fn instant_order(a: &str, b: &str) -> std::cmp::Ordering {
    fn key(s: &str) -> (&[u8], [u8; 9]) {
        let (whole, rest) = s.as_bytes().split_at(s.len().min(19));
        let mut fraction = [b'0'; 9];
        for (slot, digit) in fraction
            .iter_mut()
            .zip(rest.iter().filter(|b| b.is_ascii_digit()))
        {
            *slot = *digit;
        }
        (whole, fraction)
    }
    key(a).cmp(&key(b))
}

// ---------------------------------------------------------------------------
// Files

fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            Ok(Some((mtime, meta.len())))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn read_meta(path: &Path) -> io::Result<Meta> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// `create_dir_all`, then the leaf to `0700` whatever the umask. Every
/// directory this adds has its entry in its parent made durable: on a
/// first launch that is `noa/` in the data dir as well as `boards/` in
/// `noa/`, so a board acknowledged later cannot vanish with them.
fn create_private_dir(io: &dyn StoreIo, dir: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut at = Some(dir);
    while let Some(path) = at.filter(|path| !path.is_dir()) {
        missing.push(path);
        at = path.parent();
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    for path in missing {
        if let Some(parent) = path.parent() {
            io.sync_dir(parent)?;
        }
    }
    Ok(())
}

/// Opens (creating) a `0600` file, `fchmod`ed against the umask; a newly
/// created one has its directory entry made durable.
fn open_private(io: &dyn StoreIo, path: &Path, append: bool) -> io::Result<File> {
    let existed = path.exists();
    let file = OpenOptions::new()
        .read(true)
        .write(!append)
        .append(append)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    if !existed && let Some(parent) = path.parent() {
        io.sync_dir(parent)?;
    }
    Ok(file)
}

/// Staging file (`0600`) + sync + rename + directory sync.
fn write_private_atomic(io: &dyn StoreIo, path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("a board file has a directory");
    let name = path.file_name().expect("a file name").to_string_lossy();
    let staging = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        io.write_all(&mut file, bytes)?;
        io.sync_data(&file)?;
        io.rename(&staging, path)?;
        io.sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

/// Moves the torn tail after the durable prefix to `quarantine/<ms>.tail`
/// and truncates the file to its last newline. The writer lock is held.
fn repair_torn_tail(io: &dyn StoreIo, board: &mut Board) -> io::Result<()> {
    let writer = board.writer.as_ref().expect("repair runs under the lock");
    let mut tail = Vec::new();
    let mut file = File::open(board.posts_path())?;
    file.seek(SeekFrom::Start(board.durable_len))?;
    file.read_to_end(&mut tail)?;
    if !tail.is_empty() {
        let quarantine = board.dir.join(QUARANTINE);
        create_private_dir(io, &quarantine)?;
        let mut path = quarantine.join(format!("{}.tail", ids::now_millis()));
        let mut n = 0;
        while path.exists() {
            n += 1;
            path = quarantine.join(format!("{}-{n}.tail", ids::now_millis()));
        }
        write_private_atomic(io, &path, &tail)?;
        log::warn!(
            "timeline: board {}: moved a torn {}-byte tail of posts.jsonl to {}",
            board.id,
            tail.len(),
            path.display()
        );
    }
    writer.posts.set_len(board.durable_len)?;
    io.sync_data(&writer.posts)?;
    board.synced_len = board.durable_len;
    board.torn = 0;
    Ok(())
}
