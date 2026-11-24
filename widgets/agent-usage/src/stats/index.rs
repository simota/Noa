//! The on-disk, incrementally updated index of every parsed log file.
//!
//! Per file the index keeps its length, mtime, a hash of its head (first
//! 4 KiB), the offset of the first byte not yet consumed and the parser's tail
//! state, plus everything parsed from it (hour buckets, tool counts, session
//! spans, per-session event times, workers). A grown file with the same head
//! resumes at `offset`; a shrunk file or a changed head is parsed again from
//! 0, which drops its earlier contribution because that lives in the file's
//! own entry. Only complete lines advance `offset`.
//!
//! Entries are keyed by path, except that a Codex rollout is identified by its
//! rollout id: moved between `sessions/` and `archived_sessions/`, it takes
//! over its old entry instead of being counted again. An entry whose file is
//! gone (Claude Code deletes old transcripts) is kept, so totals keep it.
//!
//! Sessions are derived at index time (`Index::sessions`): each one's hours
//! and active time, over the events of all of its files merged. A pass
//! rebuilds only the sessions of the files it parsed, so `report` sums
//! per-hour buckets and never reads event times.
//!
//! Every log root is opened read-only; the only file written is
//! `<own_dir>/index-v1.json` (temp file + rename).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::types::{Bucket, Counters, Coverage, SessionSpan, Tokens};
use super::{agy, claude, codex, journal};
use crate::model::{Agent, Unix};
use crate::paths::{self, Roots};

/// 2: per-session event times replace the per-file last event time, Codex
/// fresh input no longer subtracts cache writes. 3: a file's active time of
/// events naming no session is kept with it; files under a Claude project
/// other than transcripts and subagent logs are no longer indexed.
pub(crate) const VERSION: u32 = 3;
/// Lines longer than this (without the `\n`) are skipped and counted.
pub(crate) const MAX_LINE: usize = 8 << 20;
const HEAD_LEN: u64 = 4096;
/// A gap between two events of one session up to this counts as active time.
pub(crate) const ACTIVE_GAP: i64 = 300;
const SAVE_EVERY: Duration = Duration::from_secs(30);
const PUBLISH_EVERY: Duration = Duration::from_millis(200);
const STOP_CHECK_LINES: u32 = 1024;

/// FNV-1a 64: a hash that is stable across builds, for ids kept on disk.
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

pub(crate) fn hour_of(ts: Unix) -> Unix {
    ts - ts.rem_euclid(3600)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Kind {
    Claude,
    Codex,
    AgyHistory,
    /// `conversations/<uuid>.db`: file count and mtime only, never opened.
    AgyDb,
    Journal,
}

impl Kind {
    fn agent(self) -> Agent {
        match self {
            Kind::Claude => Agent::Claude,
            Kind::Codex => Agent::Codex,
            // A journal's workers carry their own agent; its buckets are empty.
            Kind::AgyHistory | Kind::AgyDb | Kind::Journal => Agent::Agy,
        }
    }

    fn tail(self) -> Tail {
        match self {
            Kind::Claude => Tail::Claude(claude::Tail::default()),
            Kind::Codex => Tail::Codex(codex::Tail::default()),
            Kind::AgyHistory | Kind::AgyDb | Kind::Journal => Tail::None,
        }
    }
}

/// Parser state carried across passes of one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Tail {
    Claude(claude::Tail),
    Codex(codex::Tail),
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolCount {
    pub hour: Unix,
    pub name: u32,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Worker {
    pub ts: Unix,
    pub agent: Agent,
}

/// Everything one file contributed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileData {
    pub buckets: Vec<Bucket>,
    /// Codex `token_count` tokens, used only when the file has no
    /// `token_usage_record` (`has_records`).
    pub fallback: Vec<Bucket>,
    pub has_records: bool,
    pub tools: Vec<ToolCount>,
    pub sessions: Vec<SessionSpan>,
    pub workers: Vec<Worker>,
    /// Event times per session key, for active time.
    pub activity: Vec<Activity>,
    /// Active time of the events that name no session (`Activity` key 0),
    /// which are linked only to each other: buckets holding only
    /// `active_secs`.
    pub unlinked_active: Vec<Bucket>,
    pub lines_bad: u64,
    pub lines_skipped: u64,
}

/// An event's `(time, model, project)`.
pub(crate) type EventAt = (Unix, u32, u32);

/// One session's events in one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Activity {
    /// Session key; 0 holds the file's events that name no session, which are
    /// linked only to each other.
    pub key: u64,
    /// `(time, model, project)` per event, ascending, one per second (the
    /// first event of a second is kept).
    pub events: Vec<EventAt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub kind: Kind,
    pub len: u64,
    /// Nanoseconds since the epoch.
    pub mtime: u64,
    pub head_len: u64,
    pub head_hash: u64,
    pub offset: u64,
    pub tail: Tail,
    pub data: FileData,
}

impl FileEntry {
    fn new(kind: Kind) -> FileEntry {
        FileEntry {
            kind,
            len: 0,
            mtime: 0,
            head_len: 0,
            head_hash: fnv(&[]),
            offset: 0,
            tail: kind.tail(),
            data: FileData::default(),
        }
    }
}

/// One session over all of its files (a Claude transcript and its subagent
/// logs, a Codex thread and its sub-threads), derived from the file entries
/// whenever one of them changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Session {
    /// The first project its files name; 0 when none does.
    pub project: u32,
    /// UTC hours with activity, ascending.
    pub hours: Vec<Unix>,
    /// Active time: buckets holding only `active_secs`.
    pub active: Vec<Bucket>,
}

impl Session {
    /// Whether it had activity in an hour that overlaps `start..=end`.
    pub fn active_between(&self, start: Unix, end: Unix) -> bool {
        let first = self.hours.partition_point(|h| h + 3600 <= start);
        self.hours.get(first).is_some_and(|h| *h <= end)
    }
}

/// Buckets, sessions, interned strings, workers and coverage of every parsed
/// log. Snapshots are cheap: file entries and sessions are shared `Arc`s.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Interned model, project and tool names; id 0 is "".
    pub(crate) strings: Arc<Vec<String>>,
    pub(crate) files: BTreeMap<PathBuf, Arc<FileEntry>>,
    /// Every session by agent and key (for Codex the root thread's key),
    /// derived from `files` (`derive_sessions`); not saved.
    pub(crate) sessions: BTreeMap<(Agent, u64), Arc<Session>>,
    /// Whether each agent's log root exists (by `Agent::index`).
    pub(crate) roots_present: [bool; 3],
    pub(crate) coverage: Coverage,
    pub(crate) generation: u64,
}

impl Index {
    /// Bumped on every publish; a changed value means a changed index or
    /// coverage.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn coverage(&self) -> Coverage {
        self.coverage.clone()
    }

    pub(crate) fn string(&self, id: u32) -> &str {
        self.strings.get(id as usize).map_or("", String::as_str)
    }
}

#[derive(Serialize)]
struct DiskRef<'a> {
    version: u32,
    strings: &'a [String],
    roots_present: [bool; 3],
    files: Vec<(&'a Path, &'a FileEntry)>,
}

#[derive(Deserialize)]
struct DiskOwned {
    version: u32,
    strings: Vec<String>,
    roots_present: [bool; 3],
    files: Vec<(PathBuf, FileEntry)>,
}

/// The index at `path`; `None` when missing, damaged or of another version.
pub(crate) fn load(path: &Path) -> Option<Index> {
    let bytes = std::fs::read(path).ok()?;
    let disk: DiskOwned = serde_json::from_slice(&bytes).ok()?;
    if disk.version != VERSION || disk.strings.first().is_none_or(|s| !s.is_empty()) {
        return None;
    }
    let mut index = Index {
        strings: Arc::new(disk.strings),
        files: disk
            .files
            .into_iter()
            .map(|(p, e)| (p, Arc::new(e)))
            .collect(),
        sessions: BTreeMap::new(),
        roots_present: disk.roots_present,
        coverage: Coverage::default(),
        generation: 0,
    };
    index.coverage = summed_coverage(&index, 0, 0, false);
    let parents = codex_parents(&index);
    derive_sessions(&mut index, &parents, None);
    Some(index)
}

/// Writes the index to `path` through a temp file and a rename.
pub(crate) fn save(index: &Index, path: &Path) -> io::Result<()> {
    let disk = DiskRef {
        version: VERSION,
        strings: &index.strings,
        roots_present: index.roots_present,
        files: index
            .files
            .iter()
            .map(|(p, e)| (p.as_path(), e.as_ref()))
            .collect(),
    };
    let bytes = serde_json::to_vec(&disk).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn summed_coverage(index: &Index, total: u64, done: u64, running: bool) -> Coverage {
    let (bad, skipped) = index.files.values().fold((0, 0), |(b, s), e| {
        (b + e.data.lines_bad, s + e.data.lines_skipped)
    });
    Coverage {
        files_total: total,
        files_done: done,
        lines_bad: bad,
        lines_skipped: skipped,
        running,
    }
}

/// What the parsers write into: one file's data plus the shared interner.
pub(crate) struct Sink<'a> {
    pub agent: Agent,
    home: Option<&'a Path>,
    strings: &'a mut Arc<Vec<String>>,
    lookup: &'a mut HashMap<String, u32>,
    data: &'a mut FileData,
    buckets: HashMap<(Unix, u32, u32), usize>,
    fallback: HashMap<(Unix, u32, u32), usize>,
    tools: HashMap<(Unix, u32), usize>,
    sessions: HashMap<u64, usize>,
    activity: HashMap<u64, usize>,
    events: Vec<Event>,
    /// Sessions given events or hours in this pass.
    touched: BTreeSet<u64>,
}

struct Event {
    ts: Unix,
    session: u64,
    model: u32,
    project: u32,
}

fn positions<K: std::hash::Hash + Eq>(
    items: &[impl Sized],
    key: impl Fn(usize) -> K,
) -> HashMap<K, usize> {
    (0..items.len()).map(|i| (key(i), i)).collect()
}

impl<'a> Sink<'a> {
    fn new(
        agent: Agent,
        home: Option<&'a Path>,
        strings: &'a mut Arc<Vec<String>>,
        lookup: &'a mut HashMap<String, u32>,
        data: &'a mut FileData,
    ) -> Sink<'a> {
        let b = &data.buckets;
        let f = &data.fallback;
        let t = &data.tools;
        let s = &data.sessions;
        let a = &data.activity;
        Sink {
            agent,
            home,
            buckets: positions(b, |i| (b[i].hour, b[i].model, b[i].project)),
            fallback: positions(f, |i| (f[i].hour, f[i].model, f[i].project)),
            tools: positions(t, |i| (t[i].hour, t[i].name)),
            sessions: positions(s, |i| s[i].key),
            activity: positions(a, |i| a[i].key),
            strings,
            lookup,
            data,
            events: Vec::new(),
            touched: BTreeSet::new(),
        }
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(id) = self.lookup.get(s) {
            return *id;
        }
        let strings = Arc::make_mut(self.strings);
        let id = strings.len() as u32;
        strings.push(s.to_owned());
        self.lookup.insert(s.to_owned(), id);
        id
    }

    /// A project path, shown with `~` for the home directory.
    pub fn project(&mut self, path: &str) -> u32 {
        let shown = match self.home.and_then(|h| Path::new(path).strip_prefix(h).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.to_owned(),
        };
        self.intern(&shown)
    }

    pub fn counters(&mut self, ts: Unix, model: u32, project: u32) -> &mut Counters {
        let key = (hour_of(ts), model, project);
        let agent = self.agent;
        let data = &mut *self.data;
        let i = *self.buckets.entry(key).or_insert_with(|| {
            data.buckets.push(Bucket {
                hour: key.0,
                agent,
                model,
                project,
                c: Counters::default(),
            });
            data.buckets.len() - 1
        });
        &mut data.buckets[i].c
    }

    pub fn fallback_tokens(&mut self, ts: Unix, model: u32, project: u32) -> &mut Tokens {
        let key = (hour_of(ts), model, project);
        let agent = self.agent;
        let data = &mut *self.data;
        let i = *self.fallback.entry(key).or_insert_with(|| {
            data.fallback.push(Bucket {
                hour: key.0,
                agent,
                model,
                project,
                c: Counters::default(),
            });
            data.fallback.len() - 1
        });
        &mut data.fallback[i].c.tokens
    }

    pub fn set_has_records(&mut self) {
        self.data.has_records = true;
    }

    pub fn tool(&mut self, ts: Unix, name: &str) {
        let name = self.intern(name);
        let key = (hour_of(ts), name);
        let data = &mut *self.data;
        let i = *self.tools.entry(key).or_insert_with(|| {
            data.tools.push(ToolCount {
                hour: key.0,
                name,
                count: 0,
            });
            data.tools.len() - 1
        });
        data.tools[i].count += 1;
    }

    pub fn worker(&mut self, ts: Unix, agent: Agent) {
        self.data.workers.push(Worker { ts, agent });
    }

    /// An instant of activity: feeds active time and the session's hours.
    /// `session` 0 is "no session".
    pub fn event(&mut self, ts: Unix, session: u64, model: u32, project: u32) {
        self.events.push(Event {
            ts,
            session,
            model,
            project,
        });
    }

    /// Records the session's existence in the hour of `ts` without activity.
    pub fn session_hour(&mut self, session: u64, project: u32, ts: Unix) {
        self.touched.insert(session);
        let hour = hour_of(ts);
        let agent = self.agent;
        let data = &mut *self.data;
        let i = *self.sessions.entry(session).or_insert_with(|| {
            data.sessions.push(SessionSpan {
                agent,
                key: session,
                project,
                hours: Vec::new(),
            });
            data.sessions.len() - 1
        });
        let hours = &mut data.sessions[i].hours;
        if let Err(at) = hours.binary_search(&hour) {
            hours.insert(at, hour);
        }
    }

    fn bad(&mut self) {
        self.data.lines_bad += 1;
    }

    fn skipped(&mut self) {
        self.data.lines_skipped += 1;
    }

    /// Files the pass's events under their sessions: the session's hours and
    /// its event times (its active time is derived from those, across files,
    /// by `derive_sessions`). Returns the sessions given events or hours.
    fn finish(mut self) -> BTreeSet<u64> {
        let mut events = std::mem::take(&mut self.events);
        events.sort_by_key(|e| (e.session, e.ts));
        let unlinked = events.first().is_some_and(|e| e.session == 0);
        for e in events {
            if e.session != 0 {
                self.session_hour(e.session, e.project, e.ts);
            }
            let data = &mut *self.data;
            let i = *self.activity.entry(e.session).or_insert_with(|| {
                data.activity.push(Activity {
                    key: e.session,
                    events: Vec::new(),
                });
                data.activity.len() - 1
            });
            let list = &mut data.activity[i].events;
            let event = (e.ts, e.model, e.project);
            match list.last() {
                Some(last) if last.0 >= e.ts => {
                    if let Err(at) = list.binary_search_by_key(&e.ts, |x| x.0) {
                        list.insert(at, event);
                    }
                }
                _ => list.push(event),
            }
        }
        if unlinked {
            let FileData {
                activity,
                unlinked_active,
                ..
            } = &mut *self.data;
            let events = activity.iter().find(|a| a.key == 0).map(|a| &a.events);
            *unlinked_active = active_time(self.agent, events.map_or(&[], Vec::as_slice));
        }
        std::mem::take(&mut self.touched)
    }
}

/// One line handed to a parser.
pub(crate) enum Line<'b> {
    Full(&'b [u8]),
    /// Longer than `MAX_LINE`; its bytes were dropped.
    Oversized,
}

/// Reads complete lines from `r`, calling `f` for each; stops early when `f`
/// returns false. Returns the bytes consumed by complete lines (a cut last
/// line is not consumed).
pub(crate) fn read_lines(
    r: &mut impl BufRead,
    cap: usize,
    mut f: impl FnMut(Line<'_>) -> bool,
) -> io::Result<u64> {
    let mut consumed = 0u64;
    let mut pending = 0u64;
    let mut line: Vec<u8> = Vec::new();
    let mut oversized = false;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(consumed);
        }
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => {
                let keep_going = if oversized || line.len() + i > cap {
                    f(Line::Oversized)
                } else if line.is_empty() {
                    f(Line::Full(&buf[..i]))
                } else {
                    line.extend_from_slice(&buf[..i]);
                    f(Line::Full(&line))
                };
                r.consume(i + 1);
                consumed += pending + i as u64 + 1;
                pending = 0;
                line.clear();
                oversized = false;
                if !keep_going {
                    return Ok(consumed);
                }
            }
            None => {
                let n = buf.len();
                if !oversized {
                    if line.len() + n > cap {
                        oversized = true;
                        line = Vec::new();
                    } else {
                        line.extend_from_slice(buf);
                    }
                }
                r.consume(n);
                pending += n as u64;
            }
        }
    }
}

/// Hash of the first `len` bytes of the file (fewer when it is shorter).
fn head_hash(file: &mut File, len: u64) -> io::Result<(u64, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut head = Vec::with_capacity(len as usize);
    Read::by_ref(file).take(len).read_to_end(&mut head)?;
    Ok((head.len() as u64, fnv(&head)))
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PassStats {
    /// Files read (new, grown or reparsed).
    pub parsed: u64,
    /// Files parsed again from 0 after shrinking or a changed head.
    pub reparsed: u64,
    pub bytes_read: u64,
}

/// Every log file to index, sorted, with its kind.
fn enumerate(roots: &Roots) -> (Vec<(PathBuf, Kind)>, [bool; 3]) {
    let mut files = Vec::new();
    let mut present = [false; 3];
    if let Some(dir) = &roots.claude_projects {
        present[Agent::Claude.index()] = dir.is_dir();
        claude_logs(dir, &mut files);
    }
    for dir in &roots.codex_sessions {
        present[Agent::Codex.index()] |= dir.is_dir();
        walk(dir, 5, &mut |p| {
            if p.extension().is_some_and(|e| e == "jsonl") {
                files.push((p.to_path_buf(), Kind::Codex));
            }
        });
    }
    if let Some(dir) = &roots.agy_dir {
        present[Agent::Agy.index()] = dir.is_dir();
        let history = dir.join("history.jsonl");
        if history.is_file() {
            files.push((history, Kind::AgyHistory));
        }
        walk(&dir.join("conversations"), 1, &mut |p| {
            if p.extension().is_some_and(|e| e == "db") {
                files.push((p.to_path_buf(), Kind::AgyDb));
            }
        });
    }
    if let Some(dir) = &roots.ctl_dir {
        walk(dir, 1, &mut |p| {
            if p.extension().is_some_and(|e| e == "jsonl") {
                files.push((p.to_path_buf(), Kind::Journal));
            }
        });
    }
    files.sort();
    (files, present)
}

/// Claude Code's conversation logs, and nothing else under `projects/`:
/// `<encoded-cwd>/<sessionId>.jsonl` and
/// `<encoded-cwd>/<sessionId>/subagents/agent-*.jsonl`. Other JSONL there
/// (tool output saved under `<sessionId>/tool-results/`, say) is not a
/// conversation, whatever records it holds.
fn claude_logs(projects: &Path, out: &mut Vec<(PathBuf, Kind)>) {
    let jsonl = |p: &PathBuf| p.extension().is_some_and(|e| e == "jsonl");
    let agent_log = |p: &PathBuf| {
        jsonl(p)
            && p.file_name()
                .is_some_and(|n| n.as_encoded_bytes().starts_with(b"agent-"))
    };
    for project in children(projects).1 {
        let (transcripts, sessions) = children(&project);
        out.extend(
            transcripts
                .into_iter()
                .filter(jsonl)
                .map(|p| (p, Kind::Claude)),
        );
        for session in sessions {
            for subagents in children(&session).1 {
                if subagents.ends_with("subagents") {
                    let logs = children(&subagents).0.into_iter().filter(agent_log);
                    out.extend(logs.map(|p| (p, Kind::Claude)));
                }
            }
        }
    }
}

/// The regular files and the directories directly in `dir`; symlinks are
/// not followed.
fn children(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(ty) = entry.file_type() else { continue };
        if ty.is_file() {
            files.push(entry.path());
        } else if ty.is_dir() {
            dirs.push(entry.path());
        }
    }
    (files, dirs)
}

/// Regular files below `dir`, `depth` levels deep at most; symlinks are not
/// followed.
fn walk(dir: &Path, depth: u32, f: &mut dyn FnMut(&Path)) {
    let (files, dirs) = children(dir);
    files.iter().for_each(|p| f(p));
    if depth > 1 {
        dirs.iter().for_each(|d| walk(d, depth - 1, f));
    }
}

/// Owns the index and runs passes over the roots.
pub(crate) struct Indexer {
    roots: Roots,
    home: Option<PathBuf>,
    index: Index,
    lookup: HashMap<String, u32>,
    /// Codex thread → parent, as `index.sessions` was last derived with.
    parents: HashMap<u64, u64>,
    /// `(agent, key)` of every session whose files changed since then.
    touched: BTreeSet<(Agent, u64)>,
    stop: Arc<AtomicBool>,
    last_save: Instant,
    dirty: bool,
}

impl Indexer {
    /// Starts from the on-disk index when it is readable and current.
    pub fn new(roots: Roots, home: Option<PathBuf>, stop: Arc<AtomicBool>) -> Indexer {
        let mut index = roots
            .own_dir
            .as_deref()
            .and_then(|own| load(&paths::index_path(own)))
            .unwrap_or_default();
        if index.strings.is_empty() {
            index.strings = Arc::new(vec![String::new()]);
        }
        let lookup = index
            .strings
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();
        Indexer {
            roots,
            home,
            parents: codex_parents(&index),
            index,
            lookup,
            touched: BTreeSet::new(),
            stop,
            last_save: Instant::now(),
            dirty: false,
        }
    }

    #[cfg(test)]
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// A snapshot to publish, with its sessions up to date and a new
    /// generation.
    pub fn snapshot(&mut self) -> Index {
        self.refresh_sessions();
        self.index.generation += 1;
        self.index.clone()
    }

    /// Rebuilds the sessions whose files changed since the last call, and
    /// both sessions of every Codex key that a changed parent link moved
    /// from one root thread to another; every other session is unchanged.
    fn refresh_sessions(&mut self) {
        let parents = codex_parents(&self.index);
        // A key's session as last derived, and as of now.
        let old_and_new = |agent, key| {
            [
                (agent, session_root(&self.parents, agent, key)),
                (agent, session_root(&parents, agent, key)),
            ]
        };
        let mut roots = BTreeSet::new();
        for &(agent, key) in &self.touched {
            roots.extend(old_and_new(agent, key));
        }
        if parents != self.parents {
            let codex = self
                .index
                .files
                .iter()
                .filter(|(_, e)| e.kind == Kind::Codex);
            for (agent, key) in codex.flat_map(|(path, e)| session_keys(path, e)) {
                let [old, new] = old_and_new(agent, key);
                if old != new {
                    roots.extend([old, new]);
                }
            }
        }
        self.touched.clear();
        self.parents = parents;
        if !roots.is_empty() {
            derive_sessions(&mut self.index, &self.parents, Some(&roots));
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// One pass: stat every file, parse what changed. `publish` receives
    /// progress snapshots (at the start, every `PUBLISH_EVERY`, at the end).
    pub fn pass(&mut self, publish: &mut dyn FnMut(Index)) -> PassStats {
        let (files, present) = enumerate(&self.roots);
        let copies = self.follow_moved_rollouts(&files);
        let total = files.len() as u64;
        self.index.roots_present = present;
        self.index.coverage = summed_coverage(&self.index, total, 0, true);
        publish(self.snapshot());
        let mut stats = PassStats::default();
        let mut last_publish = Instant::now();
        for (done, (path, kind)) in files.iter().enumerate() {
            if self.stopped() {
                break;
            }
            if copies.contains(path) {
                continue;
            }
            match self.process(path, *kind) {
                Ok(Some((read, reparsed))) => {
                    stats.parsed += 1;
                    stats.reparsed += u64::from(reparsed);
                    stats.bytes_read += read;
                    self.dirty = true;
                }
                Ok(None) => {}
                // Unreadable now (permissions, removed mid-pass): keep what the
                // index already has and try again next pass.
                Err(_) => {}
            }
            if last_publish.elapsed() >= PUBLISH_EVERY {
                self.index.coverage = summed_coverage(&self.index, total, done as u64 + 1, true);
                publish(self.snapshot());
                last_publish = Instant::now();
            }
            if self.dirty && self.last_save.elapsed() >= SAVE_EVERY {
                self.save();
            }
        }
        let done = if self.stopped() { 0 } else { total };
        self.index.coverage = summed_coverage(&self.index, total, done, false);
        if self.dirty {
            self.save();
        }
        publish(self.snapshot());
        stats
    }

    /// Codex entries go by rollout id, not path: a listed rollout whose id
    /// belongs to an entry whose path is no longer listed (moved between
    /// `sessions/` and `archived_sessions/`) takes over that entry; a listed
    /// rollout whose id another listed path already has is a second copy.
    /// Returns the copies, which the pass skips.
    fn follow_moved_rollouts(&mut self, files: &[(PathBuf, Kind)]) -> HashSet<PathBuf> {
        let listed: HashSet<&Path> = files.iter().map(|(p, _)| p.as_path()).collect();
        let mut owner: HashMap<String, PathBuf> = self
            .index
            .files
            .iter()
            .filter(|(_, e)| e.kind == Kind::Codex)
            .filter_map(|(p, _)| Some((rollout_id(p)?.to_owned(), p.clone())))
            .collect();
        let mut copies = HashSet::new();
        for (path, kind) in files {
            if *kind != Kind::Codex || self.index.files.contains_key(path) {
                continue;
            }
            let Some(id) = rollout_id(path) else { continue };
            match owner.insert(id.to_owned(), path.clone()) {
                Some(old) if listed.contains(old.as_path()) => {
                    owner.insert(id.to_owned(), old);
                    copies.insert(path.clone());
                }
                Some(old) => {
                    if let Some(entry) = self.index.files.remove(&old) {
                        // Its place among its sessions' files changes.
                        self.touched.extend(session_keys(path, &entry));
                        self.index.files.insert(path.clone(), entry);
                        self.dirty = true;
                    }
                }
                None => {}
            }
        }
        copies
    }

    fn save(&mut self) {
        self.last_save = Instant::now();
        let Some(own) = self.roots.own_dir.as_deref() else {
            return;
        };
        // A failed save is retried at the next save point; the in-memory index
        // stays authoritative.
        if save(&self.index, &paths::index_path(own)).is_ok() {
            self.dirty = false;
        }
    }

    /// Parses what is new in one file: `None` when it is unchanged, else the
    /// bytes read and whether it was parsed again from 0.
    fn process(&mut self, path: &Path, kind: Kind) -> io::Result<Option<(u64, bool)>> {
        let meta = std::fs::metadata(path)?;
        let (len, mtime) = (meta.len(), mtime_nanos(&meta));
        let old = self.index.files.get(path);
        if old.is_some_and(|e| e.len == len && e.mtime == mtime) {
            return Ok(None);
        }
        if kind == Kind::AgyDb {
            let mut entry = FileEntry::new(kind);
            (entry.len, entry.mtime) = (len, mtime);
            self.touched.extend(session_keys(path, &entry));
            self.index.files.insert(path.to_path_buf(), Arc::new(entry));
            return Ok(Some((0, false)));
        }
        let mut file = File::open(path)?;
        let resume = match old {
            Some(e) if len >= e.offset && e.kind == kind => {
                head_hash(&mut file, e.head_len)? == (e.head_len, e.head_hash)
            }
            _ => false,
        };
        let reparsed = old.is_some() && !resume;
        // Parsed again from 0, the file no longer holds what it did.
        if let Some(e) = old.filter(|_| reparsed) {
            self.touched.extend(session_keys(path, e));
        }
        let mut entry = match (resume, self.index.files.remove(path)) {
            (true, Some(e)) => Arc::unwrap_or_clone(e),
            _ => FileEntry::new(kind),
        };
        let start = entry.offset;
        file.seek(SeekFrom::Start(start))?;
        let mut reader = BufReader::with_capacity(1 << 16, &mut file);
        let (consumed, sessions) = {
            let FileEntry { tail, data, .. } = &mut entry;
            let mut sink = Sink::new(
                kind.agent(),
                self.home.as_deref(),
                &mut self.index.strings,
                &mut self.lookup,
                data,
            );
            let stop = &self.stop;
            let mut n = 0u32;
            let consumed = read_lines(&mut reader, MAX_LINE, |line| {
                match line {
                    Line::Oversized => sink.skipped(),
                    Line::Full(bytes) => {
                        if !bytes.iter().all(u8::is_ascii_whitespace)
                            && !parse_line(kind, bytes, tail, &mut sink)
                        {
                            sink.bad();
                        }
                    }
                }
                n += 1;
                !n.is_multiple_of(STOP_CHECK_LINES) || !stop.load(Ordering::Relaxed)
            });
            (consumed?, sink.finish())
        };
        let agent = kind.agent();
        self.touched
            .extend(sessions.into_iter().map(|key| (agent, key)));
        drop(reader);
        entry.offset = start + consumed;
        let (head_len, hash) = head_hash(&mut file, HEAD_LEN)?;
        (entry.head_len, entry.head_hash) = (head_len, hash);
        // Stopped part-way: an mtime that never matches makes the next pass
        // resume at `offset` instead of taking the file as unchanged.
        let mtime = if self.stopped() { 0 } else { mtime };
        (entry.len, entry.mtime) = (len, mtime);
        self.index.files.insert(path.to_path_buf(), Arc::new(entry));
        Ok(Some((consumed, reparsed)))
    }
}

/// Parses one non-blank line; false when it is not valid for its kind.
fn parse_line(kind: Kind, bytes: &[u8], tail: &mut Tail, sink: &mut Sink<'_>) -> bool {
    match (kind, tail) {
        (Kind::Claude, Tail::Claude(t)) => claude::line(bytes, t, sink),
        (Kind::Codex, Tail::Codex(t)) => codex::line(bytes, t, sink),
        (Kind::AgyHistory, _) => agy::line(bytes, sink),
        (Kind::Journal, _) => journal::line(bytes, sink),
        _ => false,
    }
}

/// The session key of an agy conversation store: its file stem, the
/// conversation id the history names.
pub(crate) fn agy_db_key(path: &Path) -> u64 {
    fnv(path
        .file_stem()
        .map(|s| s.as_encoded_bytes())
        .unwrap_or_default())
}

/// The rollout id of a Codex log: the uuid that ends its file name
/// (`rollout-<ts>-<uuid>.jsonl`), which is also its `session_meta.id`. A file
/// named otherwise has none and is identified by its path.
fn rollout_id(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let uuid = id.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    });
    uuid.then_some(id)
}

/// Codex sub-thread → its parent thread, from every rollout's `session_meta`.
fn codex_parents(index: &Index) -> HashMap<u64, u64> {
    index
        .files
        .values()
        .filter_map(|e| match &e.tail {
            Tail::Codex(t) if t.sub_thread && t.thread != 0 => Some((t.thread, t.session)),
            _ => None,
        })
        .collect()
}

/// The session a key counts toward: for Codex the root of its thread's
/// parent chain (a sub-thread of a sub-thread counts toward the root), else
/// the key itself.
fn session_root(parents: &HashMap<u64, u64>, agent: Agent, key: u64) -> u64 {
    let mut k = key;
    if agent == Agent::Codex {
        // Bounded, so that a cycle in damaged logs ends.
        for _ in 0..parents.len() {
            match parents.get(&k) {
                Some(&p) if p != k => k = p,
                _ => break,
            }
        }
    }
    k
}

/// The `(agent, key)` of every session a file holds events or hours of.
fn session_keys(path: &Path, e: &FileEntry) -> Vec<(Agent, u64)> {
    let mut keys: Vec<_> = e.data.sessions.iter().map(|s| (s.agent, s.key)).collect();
    if e.kind == Kind::AgyDb {
        keys.push((Agent::Agy, agy_db_key(path)));
    }
    keys
}

/// Derives `index.sessions` from the file entries: the sessions in `only`
/// (each dropped when no file holds it any more), or all of them. A session
/// is the events and hours of all of its files (parent and subagent logs,
/// Codex sub-threads under the root of their parent chain in `parents`).
fn derive_sessions(
    index: &mut Index,
    parents: &HashMap<u64, u64>,
    only: Option<&BTreeSet<(Agent, u64)>>,
) {
    #[derive(Default)]
    struct Parts {
        project: u32,
        hours: Vec<Unix>,
        events: Vec<EventAt>,
    }
    let wanted = |key: &(Agent, u64)| only.is_none_or(|o| o.contains(key));
    let mut parts: BTreeMap<(Agent, u64), Parts> = BTreeMap::new();
    for e in index.files.values() {
        for s in &e.data.sessions {
            let key = (s.agent, session_root(parents, s.agent, s.key));
            if wanted(&key) {
                let p = parts.entry(key).or_default();
                if p.project == 0 {
                    p.project = s.project;
                }
                p.hours.extend_from_slice(&s.hours);
            }
        }
        let agent = e.kind.agent();
        for a in e.data.activity.iter().filter(|a| a.key != 0) {
            let key = (agent, session_root(parents, agent, a.key));
            if wanted(&key) {
                parts
                    .entry(key)
                    .or_default()
                    .events
                    .extend_from_slice(&a.events);
            }
        }
    }
    // A `.db` file the history never names is a session of its own, active
    // in the hour of its mtime.
    for (path, e) in index.files.iter().filter(|(_, e)| e.kind == Kind::AgyDb) {
        let key = (Agent::Agy, agy_db_key(path));
        if wanted(&key) {
            parts.entry(key).or_insert_with(|| Parts {
                hours: vec![hour_of((e.mtime / 1_000_000_000) as Unix)],
                ..Parts::default()
            });
        }
    }
    let session = |agent: Agent, mut p: Parts| {
        p.hours.sort_unstable();
        p.hours.dedup();
        // Stable: of one second in several files, the first file's event.
        p.events.sort_by_key(|e| e.0);
        Arc::new(Session {
            project: p.project,
            hours: p.hours,
            active: active_time(agent, &p.events),
        })
    };
    match only {
        None => {
            index.sessions = parts
                .into_iter()
                .map(|(key, p)| (key, session(key.0, p)))
                .collect();
        }
        Some(only) => {
            for key in only {
                match parts.remove(key) {
                    Some(p) => index.sessions.insert(*key, session(key.0, p)),
                    None => index.sessions.remove(key),
                };
            }
        }
    }
}

/// Active time of events in time order, as buckets holding only
/// `active_secs`: a gap of at most `ACTIVE_GAP` adds to the bucket of the
/// later event, so events of one second count once.
fn active_time(agent: Agent, events: &[EventAt]) -> Vec<Bucket> {
    let mut secs: BTreeMap<(Unix, u32, u32), u64> = BTreeMap::new();
    for w in events.windows(2) {
        let ((prev, ..), (ts, model, project)) = (w[0], w[1]);
        let gap = ts - prev;
        if gap > 0 && gap <= ACTIVE_GAP {
            *secs.entry((hour_of(ts), model, project)).or_default() += gap as u64;
        }
    }
    secs.into_iter()
        .map(|((hour, model, project), s)| Bucket {
            hour,
            agent,
            model,
            project,
            c: Counters {
                active_secs: s,
                ..Counters::default()
            },
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stats::testutil::*;
    use std::io::Cursor;

    /// `index` with every session derived again from its files, as a
    /// restart derives them.
    pub(crate) fn rederived(index: &Index) -> Index {
        let mut index = index.clone();
        let parents = codex_parents(&index);
        derive_sessions(&mut index, &parents, None);
        index
    }

    fn lines_of(input: &[u8], cap: usize) -> (Vec<Option<Vec<u8>>>, u64) {
        let mut out = Vec::new();
        let mut r = BufReader::with_capacity(4, Cursor::new(input.to_vec()));
        let n = read_lines(&mut r, cap, |l| {
            out.push(match l {
                Line::Full(b) => Some(b.to_vec()),
                Line::Oversized => None,
            });
            true
        })
        .unwrap();
        (out, n)
    }

    #[test]
    fn line_reader_keeps_cut_lines_and_caps_long_ones() {
        let (lines, n) = lines_of(b"ab\ncdefgh\nxy\ncut", 5);
        assert_eq!(
            lines,
            vec![Some(b"ab".to_vec()), None, Some(b"xy".to_vec())]
        );
        assert_eq!(n, 13);
    }

    #[test]
    fn grown_file_resumes_at_offset() {
        let t = Tree::new();
        let log = t.claude_file("p", "s1.jsonl");
        write(&log, &claude_turn_and_reply(0, "m1", 10));
        let mut ix = t.indexer();
        let first = ix.pass(&mut |_| {});
        assert_eq!(first.parsed, 1);
        let before = std::fs::metadata(&log).unwrap().len();
        let more = claude_turn_and_reply(60, "m2", 20);
        append(&log, &more);
        let second = ix.pass(&mut |_| {});
        assert_eq!(second.parsed, 1);
        assert_eq!(second.reparsed, 0);
        assert_eq!(second.bytes_read, more.len() as u64);
        let e = &ix.index().files[&log];
        assert_eq!(e.offset, before + more.len() as u64);
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages, c.tokens.output), (2, 2, 30));
        // Unchanged: nothing is read.
        assert_eq!(ix.pass(&mut |_| {}).parsed, 0);
    }

    #[test]
    fn cut_last_line_is_read_next_pass() {
        let t = Tree::new();
        let log = t.claude_file("p", "s1.jsonl");
        let full = claude_turn_and_reply(0, "m1", 10);
        let (head, cut) = full.split_at(full.len() - 30);
        write(&log, head);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages), (1, 0));
        assert_eq!(ix.index().coverage.lines_bad, 0);
        let e = &ix.index().files[&log];
        assert_eq!(&head[e.offset as usize - 1..e.offset as usize], b"\n");
        append(&log, cut);
        ix.pass(&mut |_| {});
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages, c.tokens.output), (1, 1, 10));
        assert_eq!(ix.index().coverage.lines_bad, 0);
    }

    #[test]
    fn file_left_part_way_by_a_stop_resumes_next_pass() {
        let t = Tree::new();
        let log = t.claude_file("p", "s1.jsonl");
        let mut bytes = Vec::new();
        for i in 0..1500 {
            bytes.extend(claude_turn_and_reply(i, &format!("m{i}"), 1));
        }
        write(&log, &bytes);
        let mut ix = t.indexer();
        ix.stop.store(true, Ordering::Relaxed);
        ix.process(&log, Kind::Claude).unwrap();
        let part = ix.index().files[&log].offset;
        assert!(part > 0 && part < bytes.len() as u64, "stopped at {part}");
        ix.stop.store(false, Ordering::Relaxed);
        let s = ix.pass(&mut |_| {});
        assert_eq!((s.parsed, s.reparsed), (1, 0));
        assert_eq!(s.bytes_read, bytes.len() as u64 - part);
        assert_eq!(sum(ix.index()).messages, 1500);
    }

    #[test]
    fn shrunk_or_rewritten_file_is_reparsed() {
        let t = Tree::new();
        let log = t.claude_file("p", "s1.jsonl");
        let mut both = claude_turn_and_reply(0, "m1", 10);
        both.extend(claude_turn_and_reply(60, "m2", 20));
        write(&log, &both);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        assert_eq!(sum(ix.index()).tokens.output, 30);
        // Shrunk: only what is left counts.
        write(&log, &claude_turn_and_reply(0, "m1", 10));
        let s = ix.pass(&mut |_| {});
        assert_eq!((s.parsed, s.reparsed), (1, 1));
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages, c.tokens.output), (1, 1, 10));
        // Rewritten to the same length with a different head.
        let other = claude_turn_and_reply(0, "m9", 70);
        assert_eq!(
            other.len(),
            claude_turn_and_reply(0, "m1", 10).len(),
            "same length"
        );
        write(&log, &other);
        let s = ix.pass(&mut |_| {});
        assert_eq!((s.parsed, s.reparsed), (1, 1));
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages, c.tokens.output), (1, 1, 70));
    }

    #[test]
    fn index_round_trips_and_bad_version_rebuilds() {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let path = paths::index_path(&t.own);
        assert!(path.is_file());
        let loaded = load(&path).expect("current index loads");
        assert_eq!(loaded.strings, ix.index().strings);
        assert_eq!(loaded.files, ix.index().files);
        assert_eq!(loaded.roots_present, ix.index().roots_present);
        // A restarted indexer reads nothing again.
        let mut again = t.indexer();
        assert_eq!(again.pass(&mut |_| {}).parsed, 0);
        assert_eq!(sum(again.index()), sum(ix.index()));

        // Another version and a damaged file are ignored and rebuilt.
        let text = std::fs::read_to_string(&path).unwrap();
        let current = format!("\"version\":{VERSION}");
        let other = format!("\"version\":{}", VERSION + 1);
        assert!(text.contains(&current));
        std::fs::write(&path, text.replacen(&current, &other, 1)).unwrap();
        assert!(load(&path).is_none());
        let mut rebuilt = t.indexer();
        assert!(rebuilt.index().files.is_empty());
        assert!(rebuilt.pass(&mut |_| {}).parsed > 0);
        assert_eq!(sum(rebuilt.index()), sum(ix.index()));
        assert!(load(&path).is_some());
        std::fs::write(&path, &text.as_bytes()[..text.len() / 2]).unwrap();
        assert!(load(&path).is_none());
        assert!(t.indexer().index().files.is_empty());
    }

    #[test]
    fn nothing_is_written_outside_own_dir() {
        let t = Tree::fixtures();
        let before = snapshot_tree(&t.root);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        ix.pass(&mut |_| {});
        let after = snapshot_tree(&t.root);
        let own_prefix = t.own.strip_prefix(&t.root).unwrap().to_path_buf();
        let changed: Vec<_> = after
            .iter()
            .filter(|(p, meta)| !p.starts_with(&own_prefix) && before.get(*p) != Some(*meta))
            .map(|(p, _)| p.clone())
            .collect();
        assert!(changed.is_empty(), "written outside own_dir: {changed:?}");
        assert_eq!(before.keys().filter(|p| !after.contains_key(*p)).count(), 0);
        let own: Vec<_> = after
            .keys()
            .filter(|p| p.starts_with(&own_prefix))
            .collect();
        assert_eq!(own, vec![&own_prefix.join("index-v1.json")]);
    }

    #[test]
    fn only_transcripts_and_subagent_logs_are_claude_logs() {
        let t = Tree::new();
        let reply =
            |at: &str, id: &str, output| claude_reply_on(&format!("2026-11-20T{at}Z"), id, output);
        // Tool output saved as JSONL holding an assistant record is no
        // conversation: alone, it is not indexed at all.
        let nested = t.claude_file("p", "sess-gen/tool-results/nested.jsonl");
        write(&nested, &reply("00:00:00", "m_tool", 90));
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        assert!(ix.index().files.is_empty(), "{:?}", ix.index().files.keys());
        assert_eq!(sum(ix.index()), Counters::default());

        // Only `<encoded-cwd>/<sessionId>.jsonl` and
        // `<encoded-cwd>/<sessionId>/subagents/agent-*.jsonl` are.
        let main = t.claude_file("p", "sess-gen.jsonl");
        let sub = t.claude_file("p", "sess-gen/subagents/agent-x.jsonl");
        write(&main, &reply("00:01:00", "m_main", 10));
        write(&sub, &reply("00:02:00", "m_sub", 20));
        let projects = t.root.join("home/.claude/projects");
        for stray in [
            projects.join("stray.jsonl"),
            t.claude_file("p", "sess-gen/agent-y.jsonl"),
            t.claude_file("p", "sess-gen/subagents/notes.jsonl"),
            t.claude_file("p", "sess-gen/subagents/deeper/agent-z.jsonl"),
            t.claude_file("p", "sess-gen/tool-results/agent-w.jsonl"),
        ] {
            write(&stray, &reply("00:03:00", "m_stray", 90));
        }
        ix.pass(&mut |_| {});
        let logs: BTreeSet<_> = ix.index().files.keys().cloned().collect();
        assert_eq!(logs, BTreeSet::from([main, sub]));
        let c = sum(ix.index());
        assert_eq!((c.messages, c.tokens.output), (2, 30));
    }

    #[test]
    fn archived_rollout_takes_over_its_entry_and_deleted_logs_stay_counted() {
        const ROLLOUT: &str = "codex/sessions/2026/11/21/rollout-2026-11-21T09-00-00-00000000-0000-4000-8000-00000000c0de.jsonl";
        const TRANSCRIPT: &str =
            "claude/projects/-synthetic-alpha/0b1c2d3e-0000-4000-8000-000000000001.jsonl";
        let codex_paths = |ix: &Indexer| -> Vec<PathBuf> {
            ix.index()
                .files
                .iter()
                .filter(|(_, e)| e.kind == Kind::Codex)
                .map(|(p, _)| p.clone())
                .collect()
        };
        let t = Tree::new();
        t.copy_fixture(ROLLOUT);
        t.copy_fixture(TRANSCRIPT);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let before = sum(ix.index());
        let sessions = ix.index().sessions.len();
        assert_eq!(sessions, 2);

        // Archived (moved), then a restart from the saved index: the entry
        // follows the file and nothing is read or counted again.
        let live = t.fixture_path(ROLLOUT);
        let archived = t
            .root
            .join("home/.codex/archived_sessions/2026/11/21")
            .join(live.file_name().unwrap());
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::rename(&live, &archived).unwrap();
        let mut ix = t.indexer();
        assert_eq!(ix.pass(&mut |_| {}).parsed, 0);
        assert_eq!(codex_paths(&ix), vec![archived.clone()]);
        assert_eq!(sum(ix.index()), before);
        assert_eq!(ix.index().sessions.len(), sessions);
        let saved = load(&paths::index_path(&t.own)).unwrap();
        assert!(saved.files.contains_key(&archived) && !saved.files.contains_key(&live));

        // A second copy is skipped while the indexed one is there, and takes
        // the entry over once it is gone.
        std::fs::copy(&archived, &live).unwrap();
        ix.pass(&mut |_| {});
        assert_eq!(codex_paths(&ix), vec![archived.clone()]);
        assert_eq!(sum(ix.index()), before);
        std::fs::remove_file(&archived).unwrap();
        ix.pass(&mut |_| {});
        assert_eq!(codex_paths(&ix), vec![live.clone()]);
        assert_eq!(sum(ix.index()), before);

        // A log deleted without a replacement keeps counting.
        std::fs::remove_file(t.fixture_path(TRANSCRIPT)).unwrap();
        ix.pass(&mut |_| {});
        assert_eq!(sum(ix.index()), before);
        assert_eq!(ix.index().sessions.len(), sessions);
    }

    #[test]
    fn sessions_kept_by_passes_match_a_rebuild() {
        const A: &str = "00000000-0000-4000-8000-00000000000a";
        const B: &str = "00000000-0000-4000-8000-00000000000b";
        const C: &str = "00000000-0000-4000-8000-00000000000c";
        const D: &str = "00000000-0000-4000-8000-00000000000d";
        // A Codex thread opened at `minute` with one turn 30 s later; B
        // works in another project.
        let thread = |id: &str, parent: Option<&str>, minute: u32| {
            let parent = parent.map_or(String::new(), |p| format!(r#","parent_thread_id":"{p}""#));
            let project = if id == B { "beta" } else { "alpha" };
            jsonl(&[
                format!(
                    r#"{{"timestamp":"2026-11-21T00:{minute:02}:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/home/synthetic/work/{project}"{parent}}}}}"#
                ),
                format!(
                    r#"{{"timestamp":"2026-11-21T00:{minute:02}:30Z","type":"event_msg","payload":{{"type":"task_started"}}}}"#
                ),
            ])
        };
        let rollout =
            |id: &str, minute: u32| format!("rollout-2026-11-21T09-{minute:02}-00-{id}.jsonl");
        let prompt = |conversation: &str, ms: i64| {
            format!(
                r#"{{"timestamp":{ms},"workspace":"/home/synthetic/work/a","conversationId":"{conversation}"}}"#
            )
        };
        const X: &str = "00000000-0000-4000-8000-0000000a9101";
        const Y: &str = "00000000-0000-4000-8000-0000000a9102";
        const T0: i64 = 1_795_410_000_000;
        let active = |ix: &Indexer, agent| sum_agent(ix.index(), agent).active_secs;
        let t = Tree::new();
        let mut ix = t.indexer();
        let pass = |ix: &mut Indexer| {
            ix.pass(&mut |_| {});
            assert_eq!(ix.index().sessions, rederived(ix.index()).sessions);
        };

        // A sub-thread before its parent's log, a transcript.
        write(&t.codex_file(&rollout(D, 6)), &thread(D, Some(C), 6));
        let main = t.claude_file("-synthetic-gaps", "sess-gen.jsonl");
        write(&main, &[claude_turn_at(0), claude_turn_at(60)].concat());
        pass(&mut ix);
        assert_eq!(
            (active(&ix, Agent::Codex), active(&ix, Agent::Claude)),
            (30, 60)
        );

        // Its parent arrives (itself a sub-thread), the transcript grows and
        // gains a subagent log; agy names one conversation and stores two.
        write(&t.codex_file(&rollout(C, 4)), &thread(C, Some(B), 4));
        append(&main, &claude_turn_at(200));
        let sub = t.claude_file("-synthetic-gaps", "sess-gen/subagents/agent-x.jsonl");
        write(&sub, &claude_turn_at(90));
        let history = t.agy_history();
        write(&history, &jsonl(&[prompt(X, T0), prompt(X, T0 + 60_000)]));
        let conversations = history.with_file_name("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        for id in [X, Y] {
            write(&conversations.join(format!("{id}.db")), b"");
        }
        pass(&mut ix);
        assert_eq!(active(&ix, Agent::Codex), 30 + 90 + 30);
        assert_eq!(active(&ix, Agent::Claude), 60 + 30 + 110);
        assert_eq!(
            (active(&ix, Agent::Agy), ix.index().sessions.len()),
            (60, 1 + 1 + 2)
        );

        // The root and its sub-thread close the chain; the transcript is
        // rewritten shorter; the history names the other conversation.
        write(&t.codex_file(&rollout(A, 0)), &thread(A, None, 0));
        write(&t.codex_file(&rollout(B, 2)), &thread(B, Some(A), 2));
        write(&main, &claude_turn_at(0));
        append(&history, &jsonl(&[prompt(Y, T0 + 90_000)]));
        pass(&mut ix);
        assert_eq!(active(&ix, Agent::Codex), 3 * (30 + 90) + 30);
        assert_eq!(
            (active(&ix, Agent::Claude), active(&ix, Agent::Agy)),
            (90, 60)
        );
        assert_eq!(ix.index().sessions.len(), 1 + 1 + 2);
        let y = ix.index().sessions[&(Agent::Agy, fnv(Y.as_bytes()))]
            .hours
            .clone();
        assert_eq!(y, vec![hour_of((T0 + 90_000) / 1000)]);

        // The subagent log is rewritten as another session's.
        let other = String::from_utf8(claude_turn_at(90)).unwrap();
        write(&sub, other.replace("sess-gen", "sess-oth").as_bytes());
        pass(&mut ix);
        assert_eq!(active(&ix, Agent::Claude), 0);
        assert_eq!(ix.index().sessions.len(), 1 + 2 + 2);

        // A sub-thread's log is archived, which puts it first among its
        // session's files; then the middle thread is rewritten as a root,
        // which splits the chain in two.
        let project = |ix: &Indexer| {
            let s = &ix.index().sessions[&(Agent::Codex, fnv(A.as_bytes()))];
            ix.index().string(s.project).to_owned()
        };
        assert_eq!(project(&ix), "~/work/alpha");
        let live = t.codex_file(&rollout(B, 2));
        let archived = t
            .root
            .join("home/.codex/archived_sessions")
            .join(live.file_name().unwrap());
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::rename(&live, &archived).unwrap();
        pass(&mut ix);
        assert_eq!(active(&ix, Agent::Codex), 3 * (30 + 90) + 30);
        assert_eq!(project(&ix), "~/work/beta");
        write(&t.codex_file(&rollout(C, 4)), &thread(C, None, 4));
        pass(&mut ix);
        assert_eq!(active(&ix, Agent::Codex), 2 * (30 + 90 + 30));
        assert_eq!(ix.index().sessions.len(), 2 + 2 + 2);

        // A restart derives the same sessions from the saved files.
        assert_eq!(t.indexer().index().sessions, ix.index().sessions);
    }
}
