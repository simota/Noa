//! The on-disk, incrementally updated index of every parsed log file.
//!
//! Per file the index keeps its length, mtime, a hash of its head (first
//! 4 KiB), the offset of the first byte not yet consumed, per-session last
//! event times and the parser's tail state, plus everything parsed from it
//! (hour buckets, tool counts, session spans, workers). A grown file with the
//! same head resumes at `offset`; a shrunk file or a changed head is parsed
//! again from 0, which drops its earlier contribution because that lives in
//! the file's own entry. Only complete lines advance `offset`.
//!
//! Every log root is opened read-only; the only file written is
//! `<own_dir>/index-v1.json` (temp file + rename).

use std::collections::{BTreeMap, BTreeSet, HashMap};
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

pub(crate) const VERSION: u32 = 1;
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
    /// Last event time per session key (0 = no session), for active time.
    pub last_event: BTreeMap<u64, Unix>,
    pub lines_bad: u64,
    pub lines_skipped: u64,
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

/// Buckets, sessions, interned strings, workers and coverage of every parsed
/// log. Snapshots are cheap: file entries are shared `Arc`s.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Interned model, project and tool names; id 0 is "".
    pub(crate) strings: Arc<Vec<String>>,
    pub(crate) files: BTreeMap<PathBuf, Arc<FileEntry>>,
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
        roots_present: disk.roots_present,
        coverage: Coverage::default(),
        generation: 0,
    };
    index.coverage = summed_coverage(&index, 0, 0, false);
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
    events: Vec<Event>,
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
        Sink {
            agent,
            home,
            buckets: positions(b, |i| (b[i].hour, b[i].model, b[i].project)),
            fallback: positions(f, |i| (f[i].hour, f[i].model, f[i].project)),
            tools: positions(t, |i| (t[i].hour, t[i].name)),
            sessions: positions(s, |i| s[i].key),
            strings,
            lookup,
            data,
            events: Vec::new(),
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

    /// Active time: per session, events in time order; a gap of at most
    /// `ACTIVE_GAP` adds to the hour bucket of the later event.
    fn finish(mut self) {
        let mut events = std::mem::take(&mut self.events);
        events.sort_by_key(|e| (e.session, e.ts));
        for e in events {
            let prev = self.data.last_event.get(&e.session).copied();
            if let Some(prev) = prev {
                let gap = e.ts - prev;
                if gap > 0 && gap <= ACTIVE_GAP {
                    self.counters(e.ts, e.model, e.project).active_secs += gap as u64;
                }
            }
            if prev.is_none_or(|p| e.ts > p) {
                self.data.last_event.insert(e.session, e.ts);
            }
            if e.session != 0 {
                self.session_hour(e.session, e.project, e.ts);
            }
        }
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
        walk(dir, 4, &mut |p| {
            if p.extension().is_some_and(|e| e == "jsonl") {
                files.push((p.to_path_buf(), Kind::Claude));
            }
        });
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

/// Regular files below `dir`, `depth` levels deep at most; symlinks are not
/// followed.
fn walk(dir: &Path, depth: u32, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ty) = entry.file_type() else { continue };
        let path = entry.path();
        if ty.is_file() {
            f(&path);
        } else if ty.is_dir() && depth > 1 {
            walk(&path, depth - 1, f);
        }
    }
}

/// Owns the index and runs passes over the roots.
pub(crate) struct Indexer {
    roots: Roots,
    home: Option<PathBuf>,
    index: Index,
    lookup: HashMap<String, u32>,
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
            index,
            lookup,
            stop,
            last_save: Instant::now(),
            dirty: false,
        }
    }

    #[cfg(test)]
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// A snapshot to publish, with a new generation.
    pub fn snapshot(&mut self) -> Index {
        self.index.generation += 1;
        self.index.clone()
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// One pass: stat every file, parse what changed. `publish` receives
    /// progress snapshots (at the start, every `PUBLISH_EVERY`, at the end).
    pub fn pass(&mut self, publish: &mut dyn FnMut(Index)) -> PassStats {
        let (files, present) = enumerate(&self.roots);
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
        let mut entry = match (resume, self.index.files.remove(path)) {
            (true, Some(e)) => Arc::unwrap_or_clone(e),
            _ => FileEntry::new(kind),
        };
        let start = entry.offset;
        file.seek(SeekFrom::Start(start))?;
        let mut reader = BufReader::with_capacity(1 << 16, &mut file);
        let consumed = {
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
            sink.finish();
            consumed?
        };
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

/// Every distinct `(agent, session key)` with its project and merged hours.
pub(crate) fn merged_sessions(index: &Index) -> BTreeMap<(Agent, u64), (u32, BTreeSet<Unix>)> {
    let mut out: BTreeMap<(Agent, u64), (u32, BTreeSet<Unix>)> = BTreeMap::new();
    for e in index.files.values() {
        for s in &e.data.sessions {
            let slot = out
                .entry((s.agent, s.key))
                .or_insert_with(|| (s.project, BTreeSet::new()));
            if slot.0 == 0 {
                slot.0 = s.project;
            }
            slot.1.extend(s.hours.iter().copied());
        }
    }
    // A `.db` file the history never names is a session of its own, active
    // in the hour of its mtime.
    for (path, e) in &index.files {
        if e.kind == Kind::AgyDb {
            let hour = hour_of((e.mtime / 1_000_000_000) as Unix);
            out.entry((Agent::Agy, agy_db_key(path)))
                .or_insert_with(|| (0, BTreeSet::from([hour])));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stats::testutil::*;
    use std::io::Cursor;

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
        std::fs::write(&path, text.replacen("\"version\":1", "\"version\":2", 1)).unwrap();
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
}
