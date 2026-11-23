//! The spawners' journals (`Noa +ctl`'s, `crates/noa-app/src/ctl/journal.rs`):
//! one JSONL file per orchestrator pane, `<data-dir>/noa/ctl/<surface>.jsonl`
//! (`noa_config::ctl_journal_path`), each line an event with `"ev"` and
//! `"ts"` (unix seconds): `spawn`, `send`, `outcome` and `close`. The live
//! workers take their tasks and result files from them; a worker whose pane
//! is gone is built from them alone, as a closed one.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use noa_ipc::Panel;

use crate::fleet::{Phase, State, Worker, minutes_text};

use super::{Spawned, is_worker, model_and_effort, name_of, placement};

/// A worker is shown as closed for this long after it closed (or, when
/// that is not known, after it was spawned).
pub(super) const CLOSED_MAX_AGE_SECS: f64 = 24.0 * 3600.0;
/// The most closed workers shown, the newest first.
pub(super) const CLOSED_MAX: usize = 50;
/// The journal directory is looked through at most this often.
pub(super) const ARCHIVE_SCAN_SECS: f64 = 10.0;
/// The most journals of closed orchestrators read: the newest.
pub(super) const ARCHIVE_FILES: usize = 32;
/// How much of the end of such a journal is read. `+ctl` halves a journal
/// over 4 MiB, so this is at most a quarter of one.
pub(super) const ARCHIVE_READ_BYTES: u64 = 1024 * 1024;
/// A pane that started this long before its spawn was journaled, or any
/// time after, is another spawn under the same id: the journal stamps a
/// spawn once Noa answered it (and a worktree or a board was made), and a
/// recovered spawn later still.
const SPAWN_LEAD_SECS: u64 = 600;
const SPAWN_LAG_SECS: u64 = 5;
/// What a closed worker's key has set, so it never names a pane: pane ids
/// are counted from 1 and never get near it.
const CLOSED_KEY_BIT: u64 = 1 << 63;

/// One worker a journal records: from a `spawn` of its pane to the next
/// one (a pane id is given out again after Noa restarts).
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Record {
    pub(super) pane: u64,
    /// The spawn's `ts`.
    pub(super) spawned_at: u64,
    pub(super) label: Option<String>,
    pub(super) argv: Vec<String>,
    pub(super) cwd: Option<String>,
    /// A `close --prune` removed the worktree (`worktree.removed`): the
    /// cwd is a path that no longer exists.
    pub(super) worktree_removed: bool,
    pub(super) branch: Option<String>,
    /// The worker's result file as the spawn named it; `moved_to` once a
    /// `close --prune` moved it out of the worktree it removed.
    pub(super) result: Option<PathBuf>,
    pub(super) moved_to: Option<PathBuf>,
    /// The first non-empty line of the last task sent (answers to prompts
    /// are not tasks).
    pub(super) task: String,
    /// The `reason` of the last wait outcome.
    pub(super) outcome: Option<String>,
    /// The `close`'s `ts`.
    pub(super) closed_at: Option<u64>,
}

impl Record {
    /// The result file, its relative path taken from the worker's cwd.
    fn result_path(&self) -> Option<PathBuf> {
        let result = self.moved_to.as_ref().or(self.result.as_ref())?;
        if result.is_absolute() {
            return Some(result.clone());
        }
        Some(Path::new(self.cwd.as_deref()?).join(result))
    }
}

/// A journal as read: its records in spawn order, and the latest record of
/// each pane as the live workers take it.
pub(super) struct Journal {
    /// The file's name without `.jsonl`: the orchestrator's surface id as
    /// `noa_config::ctl_journal_path_in` writes it.
    pub(super) name: String,
    /// When the file was last changed, and its length, when it was read.
    pub(super) stamp: Option<(SystemTime, u64)>,
    pub(super) records: Vec<Record>,
    pub(super) spawns: HashMap<u64, Spawned>,
    /// The result files a `close --prune` moved, from where to where, the
    /// workers of other spawners' included (theirs name no pane here).
    pub(super) moves: Vec<(PathBuf, PathBuf)>,
    /// An open orchestrator's journal, of the Noa the feed follows. Any
    /// other (the journal directory's) may be another Noa's, sharing the
    /// data directory, or from before a restart: a record there counts as
    /// closed only when it says so, or the feed saw its pane leave.
    pub(super) this_instance: bool,
}

impl Journal {
    pub(super) fn parsed(name: String, stamp: Option<(SystemTime, u64)>, text: &str) -> Self {
        let (records, moves) = parse(text);
        let spawns = spawns(&records);
        Journal {
            name,
            stamp,
            records,
            spawns,
            moves,
            this_instance: false,
        }
    }

    /// The journal as an open orchestrator's (`this_instance`).
    pub(super) fn of_this_instance(self) -> Self {
        Journal {
            this_instance: true,
            ..self
        }
    }
}

/// A surface id as its journal's file is named: anything but ASCII letters,
/// digits, `-` and `_` becomes `_` (`noa_config::ctl_journal_path_in`).
pub(super) fn file_name(surface: &str) -> String {
    noa_config::ctl_journal_path_in(Path::new(""), surface)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The directory the journals are in: `<data-dir>/noa/ctl`.
pub(crate) fn default_dir() -> Option<PathBuf> {
    Some(noa_config::ctl_journal_path("_")?.parent()?.to_owned())
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn text_of(event: &serde_json::Value, key: &str) -> Option<String> {
    event[key].as_str().map(str::to_owned)
}

/// Every worker `text` records, in spawn order. Only a `spawn` makes one;
/// another event attaches to the latest record of its pane, and one for a
/// pane never spawned here (`send --any-pane`, a mistyped `close`) to none.
/// A line that does not parse is skipped.
#[cfg(test)]
pub(super) fn records(text: &str) -> Vec<Record> {
    parse(text).0
}

/// The records of `text` (`records`), and every result move it journals.
fn parse(text: &str) -> (Vec<Record>, Vec<(PathBuf, PathBuf)>) {
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut records: Vec<Record> = Vec::new();
    let mut latest: HashMap<u64, usize> = HashMap::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts = event["ts"].as_u64().unwrap_or(0);
        // `close --prune` moved other workers' results too.
        for moved in event["worktree"]["moved"].as_array().into_iter().flatten() {
            if let (Some(from), Some(to)) = (moved["from"].as_str(), moved["to"].as_str()) {
                moves.push((PathBuf::from(from), PathBuf::from(to)));
            }
            let pane = moved["paneId"].as_str().and_then(|id| id.parse().ok());
            if let (Some(at), Some(to)) = (pane.and_then(|p| latest.get(&p)), moved["to"].as_str())
            {
                records[*at].moved_to = Some(PathBuf::from(to));
            }
        }
        let Some(pane) = event["pane"].as_u64() else {
            continue;
        };
        if event["ev"] == "spawn" {
            let argv = event["argv"]
                .as_array()
                .map(|argv| {
                    argv.iter()
                        .filter_map(|arg| arg.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            latest.insert(pane, records.len());
            records.push(Record {
                pane,
                spawned_at: ts,
                label: text_of(&event, "label").filter(|label| !label.is_empty()),
                argv,
                cwd: text_of(&event, "cwd"),
                branch: text_of(&event, "branch"),
                result: text_of(&event, "result").map(PathBuf::from),
                ..Record::default()
            });
            continue;
        }
        let Some(record) = latest.get(&pane).map(|at| &mut records[*at]) else {
            continue;
        };
        match event["ev"].as_str() {
            Some("send") if event["answer"].as_bool() != Some(true) => {
                if let Some(text) = event["text"].as_str() {
                    record.task = first_line(text).to_owned();
                }
            }
            Some("outcome") => {
                record.outcome = text_of(&event, "reason");
                if record.label.is_none() {
                    record.label = text_of(&event, "label").filter(|label| !label.is_empty());
                }
            }
            Some("close") => {
                // `+ctl close` journals a close of a pane already gone too
                // (a retried prune): the first one is when it closed.
                record.closed_at.get_or_insert(ts);
                record.worktree_removed |= event["worktree"]["removed"].as_bool() == Some(true);
                if let Some(result) = event["worktree"]["result"].as_str() {
                    record.moved_to = Some(PathBuf::from(result));
                }
            }
            _ => {}
        }
    }
    (records, moves)
}

/// Each pane's latest record, as a live worker takes it: its task and the
/// result file its spawn named.
fn spawns(records: &[Record]) -> HashMap<u64, Spawned> {
    records
        .iter()
        .map(|record| {
            let spawned = Spawned {
                task: record.task.clone(),
                result: record.result.clone(),
            };
            (record.pane, spawned)
        })
        .collect()
}

/// Reads the end of the journal at `path`, at most `ARCHIVE_READ_BYTES`,
/// from the first whole line. Only a regular file is read: the open
/// neither follows a symbolic link nor waits for a FIFO's writer.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let len = file.metadata().ok().filter(|meta| meta.is_file())?.len();
    let from = len.saturating_sub(ARCHIVE_READ_BYTES);
    // From the byte before, when there is one: a line that starts exactly
    // where the read does is whole when that byte ends a line.
    let before = from.min(1);
    file.seek(SeekFrom::Start(from - before)).ok()?;
    let mut bytes = Vec::new();
    file.take(ARCHIVE_READ_BYTES + before)
        .read_to_end(&mut bytes)
        .ok()?;
    if before > 0 {
        // Drops the byte before and whatever of a cut line follows it.
        let start = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |at| at + 1);
        bytes.drain(..start);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The journals in `dir` worth reading for closed workers: regular
/// `*.jsonl` files changed within `CLOSED_MAX_AGE_SECS` of `now`, none named
/// in `skip` (journals already read as open orchestrators'), the newest
/// `ARCHIVE_FILES` of them, with their stamps.
fn archive_files(
    dir: &Path,
    now: SystemTime,
    skip: &HashSet<String>,
) -> Vec<(PathBuf, String, (SystemTime, u64))> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let oldest = now - Duration::from_secs_f64(CLOSED_MAX_AGE_SECS);
    let mut found: Vec<(PathBuf, String, (SystemTime, u64))> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            if skip.contains(&name) {
                return None;
            }
            // Not followed: a link is not a journal `+ctl` wrote.
            let meta = std::fs::symlink_metadata(&path).ok()?;
            let modified = meta.modified().ok()?;
            (meta.is_file() && modified >= oldest).then_some((path, name, (modified, meta.len())))
        })
        .collect();
    found.sort_by(|a, b| b.2.0.cmp(&a.2.0).then_with(|| a.1.cmp(&b.1)));
    found.truncate(ARCHIVE_FILES);
    found
}

/// The journals of orchestrators that are not open (closed, or from before
/// Noa restarted), read from the journal directory and kept between scans:
/// a file is read again only when it changed.
#[derive(Default)]
pub(super) struct Archive {
    pub(super) dir: Option<PathBuf>,
    pub(super) journals: HashMap<PathBuf, Journal>,
    /// When the directory was last looked through (unix seconds).
    scanned: Option<f64>,
}

impl Archive {
    pub(super) fn at(dir: Option<PathBuf>) -> Self {
        Archive {
            dir,
            ..Archive::default()
        }
    }

    /// Looks through the directory, at most every `ARCHIVE_SCAN_SECS`.
    /// `open` names the journals read as open orchestrators' already.
    pub(super) fn scan(&mut self, now: f64, open: &HashSet<String>) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        if self
            .scanned
            .is_some_and(|at| (0.0..ARCHIVE_SCAN_SECS).contains(&(now - at)))
        {
            return;
        }
        self.scanned = Some(now);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now.max(0.0));
        let found = archive_files(&dir, wall, open);
        let kept: HashSet<&PathBuf> = found.iter().map(|(path, _, _)| path).collect();
        self.journals.retain(|path, _| kept.contains(path));
        for (path, name, stamp) in found {
            if self
                .journals
                .get(&path)
                .is_some_and(|j| j.stamp == Some(stamp))
            {
                continue;
            }
            let text = read_tail(&path).unwrap_or_default();
            self.journals
                .insert(path, Journal::parsed(name, Some(stamp), &text));
        }
    }
}

/// Whether a pane that started at `started_at` is the spawn journaled at
/// `spawned_at` (see `SPAWN_LEAD_SECS`).
fn same_spawn(started_at: u64, spawned_at: u64) -> bool {
    started_at <= spawned_at + SPAWN_LAG_SECS && spawned_at <= started_at + SPAWN_LEAD_SECS
}

/// Whether the open pane `record` names is the worker it records, and not
/// another pane given the same id (after Noa restarted). The pane's own
/// start decides when Noa reports it; else the spawner its parent is,
/// when that one is open; else it is taken for the same, so no worker is
/// shown twice.
fn is_open(record: &Record, journal: &str, panels: &HashMap<u64, Panel>) -> bool {
    let Some(panel) = panels.get(&record.pane) else {
        return false;
    };
    if !is_worker(panel) {
        return false;
    }
    // A spawner known to be another is decisive: two spawns can be close
    // in time (Noa restarted at once).
    let parent = panel.parent_pane_id.and_then(|id| panels.get(&id.0));
    if let Some(surface) = parent.and_then(|parent| parent.surface_id.as_deref())
        && file_name(surface) != journal
    {
        return false;
    }
    match &panel.spawn {
        Some(spawn) => same_spawn(spawn.started_at, record.spawned_at),
        None => true,
    }
}

/// A worker gone from Noa, as its journal records it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Gone {
    /// The dashboard's key for it (`closed_key`).
    pub(super) key: u64,
    pub(super) record: Record,
    /// When it closed: the journal's `close`, else when the feed saw its
    /// pane leave.
    pub(super) ended_at: Option<f64>,
    /// Its result file, when it is a regular file with something in it.
    pub(super) result: Option<PathBuf>,
    pub(super) orchestrator: String,
    pub(super) orchestrator_pane: Option<u64>,
    /// The worker that spawned it, when that one hangs under another (as
    /// `placement` places a live grandchild).
    pub(super) parent: Option<u64>,
}

/// The key a closed worker is shown under: its journal, pane and spawn,
/// hashed, with `CLOSED_KEY_BIT` set. The same record gets the same key in
/// every snapshot, and no pane id is ever one.
fn closed_key(journal: &str, record: &Record) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    (journal, record.pane, record.spawned_at).hash(&mut hasher);
    hasher.finish() | CLOSED_KEY_BIT
}

/// What the open panes are for closed workers: the orchestrator each
/// journal belongs to, by its file name.
fn open_spawners(panels: &HashMap<u64, Panel>) -> HashMap<String, &Panel> {
    panels
        .values()
        .filter_map(|panel| Some((file_name(panel.surface_id.as_deref()?), panel)))
        .collect()
}

/// A regular file with something in it: the dashboard reads no other.
fn has_content(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// The workers `journals` record whose panes are gone (closed, or another
/// pane has their id now), ended within `CLOSED_MAX_AGE_SECS` of `now`, the
/// newest `CLOSED_MAX` first. `vanished` says when the feed saw a pane
/// leave, and when that pane had started.
pub(super) fn gone<'a>(
    journals: impl IntoIterator<Item = &'a Journal>,
    panels: &HashMap<u64, Panel>,
    vanished: &HashMap<u64, (f64, Option<u64>)>,
    now: f64,
) -> Vec<Gone> {
    let spawners = open_spawners(panels);
    let journals: Vec<&Journal> = journals.into_iter().collect();
    // Moves are found across journals: a prune moves other spawners'
    // results too.
    let moves: HashMap<&Path, &Path> = journals
        .iter()
        .flat_map(|journal| &journal.moves)
        .map(|(from, to)| (from.as_path(), to.as_path()))
        .collect();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut gone: Vec<Gone> = Vec::new();
    for journal in journals {
        // An open orchestrator's journal also when it was read from the
        // journal directory (its workers had all closed before the
        // dashboard started, so none led to it).
        let followed = journal.this_instance || spawners.contains_key(&journal.name);
        for record in &journal.records {
            let closed = record.closed_at.is_some();
            if !closed && is_open(record, &journal.name, panels) {
                continue;
            }
            let left = vanished.get(&record.pane).and_then(|(at, started)| {
                let ours = started.is_none_or(|started| same_spawn(started, record.spawned_at));
                (ours && *at >= record.spawned_at as f64).then_some(*at)
            });
            if !closed && left.is_none() && !followed {
                // Maybe running in another Noa: nothing says it closed.
                continue;
            }
            let ended_at = record.closed_at.map(|ts| ts as f64).or(left);
            let last = ended_at.unwrap_or(record.spawned_at as f64);
            if now - last > CLOSED_MAX_AGE_SECS {
                continue;
            }
            let key = closed_key(&journal.name, record);
            // The same journal read twice (open, then archived) names it once.
            if !seen.insert(key) {
                continue;
            }
            let (orchestrator, orchestrator_pane, parent) = match spawners.get(&journal.name) {
                // A worker under another: its child hangs as a live
                // grandchild does, under the chain's root.
                Some(panel) if is_worker(panel) && panel.parent_pane_id.is_some() => {
                    let spawner = panel.pane_id.0;
                    let placed = placement(spawner, panels);
                    (placed.orchestrator, placed.root, Some(spawner))
                }
                Some(panel) => (name_of(panel), Some(panel.pane_id.0), None),
                None => {
                    let tail: String = journal.name.chars().rev().take(4).collect();
                    let tail: String = tail.chars().rev().collect();
                    (format!("閉じた親 {tail}"), None, None)
                }
            };
            let result = record
                .result_path()
                .map(|path| match moves.get(path.as_path()) {
                    Some(to) if record.moved_to.is_none() => to.to_path_buf(),
                    _ => path,
                });
            gone.push(Gone {
                key,
                result: result.filter(|path| has_content(path)),
                record: record.clone(),
                ended_at,
                orchestrator,
                orchestrator_pane,
                parent,
            });
        }
    }
    let last = |gone: &Gone| gone.ended_at.unwrap_or(gone.record.spawned_at as f64);
    gone.sort_by(|a, b| last(b).total_cmp(&last(a)).then(a.key.cmp(&b.key)));
    gone.truncate(CLOSED_MAX);
    gone
}

/// The kind of what `argv` ran: its command's name, else `shell`.
fn kind_of(argv: &[String]) -> String {
    argv.first()
        .map(|argv0| argv0.rsplit('/').next().unwrap_or(argv0).to_owned())
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

/// A closed worker as the dashboard shows it, as of `now`: done, tagged
/// 閉じた, its times from its spawn and its close.
pub(super) fn closed_worker(gone: &Gone, now: f64) -> Worker {
    let record = &gone.record;
    let kind = kind_of(&record.argv);
    let (model, effort) = model_and_effort(&kind, &record.argv);
    let spawned = record.spawned_at as f64;
    let started_minutes = (now - spawned).max(0.0) / 60.0;
    let took = gone.ended_at.map(|end| (end - spawned).max(0.0) / 60.0);
    let since = match gone.ended_at {
        Some(end) => format!("{}前に閉じた", minutes_text((now - end) / 60.0)),
        None => "閉じた".to_owned(),
    };
    let phases = match (gone.ended_at, took) {
        (Some(end), Some(took)) => vec![
            (Phase::Running, took),
            (Phase::Ended, (now - end).max(0.0) / 60.0),
        ],
        _ => Vec::new(),
    };
    Worker {
        pane: gone.key,
        label: record
            .label
            .clone()
            .unwrap_or_else(|| format!("ペイン {}", record.pane)),
        kind,
        branch: record.branch.clone().unwrap_or_default(),
        model,
        effort,
        orchestrator: gone.orchestrator.clone(),
        orchestrator_pane: gone.orchestrator_pane,
        parent: gone.parent,
        // A closed worker is done, or failed when its last outcome says so:
        // closing a pane is not the task's success.
        state: if record.outcome.as_deref() == Some("error") {
            State::Error
        } else {
            State::Finished
        },
        attention: false,
        task: record.task.clone(),
        detail: record
            .outcome
            .as_ref()
            .map(|reason| format!("最後の待ち結果: {reason}"))
            .unwrap_or_default(),
        since,
        wait_minutes: 0.0,
        quiet: false,
        reports: 0,
        started_minutes: Some(started_minutes),
        took_minutes: took,
        has_result: gone.result.is_some(),
        result: gone.result.clone(),
        screen_tail: Vec::new(),
        lines_per_tick: Vec::new(),
        freshness: 0.0,
        phases,
        closed_pane: Some(record.pane),
        // A removed worktree is nothing to look at (and its path may be
        // another worker's by now).
        cwd: if record.worktree_removed {
            String::new()
        } else {
            record.cwd.clone().unwrap_or_default()
        },
    }
    .on_one_line()
}
