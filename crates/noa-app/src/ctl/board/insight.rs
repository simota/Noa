//! What the board knows of each worker's task and result file
//! (agent-orchestration D18), read from files the board does not own: the
//! journal of the worker's parent (`noa_config::ctl_journal_path` of the
//! parent pane's `surfaceId`) and the worker's `$NOA_RESULT`.
//!
//! The board draws a frame every half second or so, and none of that reads
//! a file: [`Insight::refresh`] does, from the board's loop, at most every
//! [`FILES_EVERY`]. A journal is read again only when its modification time
//! or length changed; a result file is only `lstat`ed, and its text is read
//! only while the detail view shows it, again only when it changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use noa_ipc::Panel;

use crate::ctl::journal;
use crate::ctl::result::{self, StartError};

/// How often journals and result files are looked at.
pub(super) const FILES_EVERY: Duration = Duration::from_secs(2);

/// What tells whether a file changed: its modification time and length.
type Stamp = (Option<SystemTime>, u64);

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    (meta.modified().ok(), meta.len())
}

/// A worker's task, as its parent's journal has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Task {
    /// No journal to read: the parent pane is not listed, or has no
    /// `surfaceId` (a remote pane).
    NoJournal,
    JournalMissing,
    JournalUnreadable(String),
    /// The journal has no spawn of this pane (it was started otherwise).
    NotJournaled,
    NotSent,
    /// The last task sent: whole when the journal kept it whole (D19's
    /// `full`), without the preface `retry` put before it; else the last
    /// non-answer send's preview, `cut` when it may be incomplete.
    /// Neither form displays the timeline header. `retry_of`: the pane this
    /// worker was started again from (`+ctl retry`).
    Sent {
        text: String,
        cut: bool,
        retry_of: Option<u64>,
    },
}

impl Task {
    fn from_worker(worker: &journal::Worker) -> Self {
        let (text, cut) = match (&worker.task, &worker.last_task) {
            (Some(whole), _) => (crate::ctl::run_attr::strip_header(whole), false),
            (None, Some(start)) => (start.text.as_str(), start.cut),
            (None, None) => return Self::NotSent,
        };
        Self::Sent {
            text: crate::ctl::retry::strip_preface(text).to_owned(),
            cut,
            retry_of: worker.retry_of,
        }
    }
}

/// A worker's result file, from an `lstat` of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FileState {
    Missing,
    /// Reserved at spawn, not written yet.
    Empty,
    Written {
        bytes: u64,
        /// How old it was when looked at, and when that was.
        age: Duration,
        seen: Instant,
        stamp: Stamp,
    },
    NotRegular,
    Unreadable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ResultFile {
    /// Neither the journal nor a plain label names one.
    Unknown,
    Known(PathBuf, FileState),
}

impl ResultFile {
    pub(super) fn written(&self) -> bool {
        matches!(self, Self::Known(_, FileState::Written { .. }))
    }
}

/// The result text the detail view shows: whose, read at which stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shown {
    pub(super) pane: u64,
    stamp: Stamp,
    pub(super) text: Result<(String, bool), StartError>,
}

/// A journal as last read.
struct JournalCopy {
    stamp: Stamp,
    workers: Result<Vec<journal::Worker>, String>,
}

#[derive(Default)]
pub(super) struct Insight {
    /// The data directory journals are under; `None`: the user's
    /// (`noa_config::ctl_journal_path`). Tests point it at a temp dir.
    pub(super) root: Option<PathBuf>,
    journals: HashMap<PathBuf, JournalCopy>,
    tasks: HashMap<u64, Task>,
    results: HashMap<u64, ResultFile>,
    shown: Option<Shown>,
    /// When the files were last looked at.
    at: Option<Instant>,
}

impl Insight {
    pub(super) fn task(&self, pane: u64) -> Option<&Task> {
        self.tasks.get(&pane)
    }

    pub(super) fn result(&self, pane: u64) -> Option<&ResultFile> {
        self.results.get(&pane)
    }

    /// The result text read for `pane`, if it is the one shown.
    pub(super) fn shown(&self, pane: u64) -> Option<&Shown> {
        self.shown.as_ref().filter(|shown| shown.pane == pane)
    }

    fn journal_path(&self, surface_id: &str) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(noa_config::ctl_journal_path_in(root, surface_id)),
            None => noa_config::ctl_journal_path(surface_id),
        }
    }

    /// Look at the files of the workers `panes` (all from `panels`, the
    /// whole listing) when [`FILES_EVERY`] has passed, and read the result
    /// text of `show` (the worker whose result the detail view shows) when
    /// it changed. Returns whether anything was looked at.
    pub(super) fn refresh(
        &mut self,
        panels: &[Panel],
        panes: &[u64],
        show: Option<u64>,
        now: Instant,
    ) -> bool {
        let due = self
            .at
            .is_none_or(|at| now.saturating_duration_since(at) >= FILES_EVERY);
        if due {
            self.at = Some(now);
            self.look(panels, panes, now);
        }
        let Some(pane) = show else {
            self.shown = None;
            return due;
        };
        let Some(ResultFile::Known(path, FileState::Written { stamp, .. })) =
            self.results.get(&pane)
        else {
            // Nothing written to read: the detail view says why from the
            // file's state.
            self.shown = None;
            return due;
        };
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.pane == pane && shown.stamp == *stamp)
        {
            return due;
        }
        self.shown = Some(Shown {
            pane,
            stamp: *stamp,
            text: result::read_start(path),
        });
        true
    }

    fn look(&mut self, panels: &[Panel], panes: &[u64], now: Instant) {
        let by_id: HashMap<u64, &Panel> = panels
            .iter()
            .map(|panel| (panel.pane_id.0, panel))
            .collect();
        let mut journals_seen: HashSet<PathBuf> = HashSet::new();
        self.tasks.clear();
        self.results.clear();
        for &pane in panes {
            let Some(panel) = by_id.get(&pane) else {
                continue;
            };
            // A worker's journal is its parent's (the orchestrator's, or for
            // a worker's worker its parent worker's, D13).
            let path = panel
                .parent_pane_id
                .and_then(|parent| by_id.get(&parent.0))
                .and_then(|parent| parent.surface_id.as_deref())
                .and_then(|surface_id| self.journal_path(surface_id));
            let workers: Result<&[journal::Worker], Task> = match &path {
                None => Err(Task::NoJournal),
                Some(path) => {
                    journals_seen.insert(path.clone());
                    self.read_journal(path)
                }
            };
            let task = match &workers {
                Err(why) => why.clone(),
                Ok(workers) => match workers.iter().find(|worker| worker.pane == pane) {
                    None => Task::NotJournaled,
                    Some(worker) => Task::from_worker(worker),
                },
            };
            let result = match crate::ctl::result_path(workers.unwrap_or(&[]), panel) {
                None => ResultFile::Unknown,
                Some(path) => {
                    let state = file_state(&path, now);
                    ResultFile::Known(path, state)
                }
            };
            self.tasks.insert(pane, task);
            self.results.insert(pane, result);
        }
        self.journals.retain(|path, _| journals_seen.contains(path));
    }

    /// The workers of the journal at `path`, read again only when its
    /// stamp changed; `Err` is the task to say instead.
    fn read_journal(&mut self, path: &Path) -> Result<&[journal::Worker], Task> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.journals.remove(path);
                return Err(Task::JournalMissing);
            }
            Err(err) => {
                self.journals.remove(path);
                return Err(Task::JournalUnreadable(err.to_string()));
            }
        };
        let stamp = stamp_of(&meta);
        let fresh = self
            .journals
            .get(path)
            .is_some_and(|copy| copy.stamp == stamp);
        if !fresh {
            let workers =
                read_whole(path).map(|text| journal::workers(&journal::parse_events(&text)));
            self.journals
                .insert(path.to_path_buf(), JournalCopy { stamp, workers });
        }
        match &self.journals[path].workers {
            Ok(workers) => Ok(workers),
            Err(why) => Err(Task::JournalUnreadable(why.clone())),
        }
    }
}

/// A journal's text. It is the board's own kind of file, but a FIFO put in
/// its place must not block the board: only a regular file is read.
fn read_whole(path: &Path) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| err.to_string())?;
    let meta = file.metadata().map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What an `lstat` of a result file says. A link is not followed: the
/// report is read from the file itself or not at all.
fn file_state(path: &Path, now: Instant) -> FileState {
    match std::fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
        Err(err) => FileState::Unreadable(err.to_string()),
        Ok(meta) if !meta.is_file() => FileState::NotRegular,
        Ok(meta) if meta.len() == 0 => FileState::Empty,
        Ok(meta) => FileState::Written {
            bytes: meta.len(),
            age: meta
                .modified()
                .ok()
                .and_then(|at| SystemTime::now().duration_since(at).ok())
                .unwrap_or_default(),
            seen: now,
            stamp: stamp_of(&meta),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl::{retry, run_attr};

    #[test]
    fn ac4_clipped_board_task_uses_the_full_send_when_available() {
        let body = "界".repeat(journal::TEXT_CHARS + 1);
        let sent = run_attr::with_longest_header(&body);
        // A raw send is shown as the last send but is never used as a whole task for retry.
        let workers = journal::workers(&[
            serde_json::json!({"ev":"spawn", "pane":4}),
            serde_json::json!({"ev":"send", "pane":4, "text":journal::clip(&sent),
                "full":sent, "bytes":sent.len(), "raw":true}),
        ]);
        assert_eq!(
            Task::from_worker(&workers[0]),
            Task::Sent {
                text: journal::clip(&body),
                cut: true,
                retry_of: None
            }
        );
    }

    #[test]
    fn ac4_task_text_strips_header() {
        let task = "\n調査\n";
        let worker = journal::Worker {
            task: Some(run_attr::with_longest_header(&retry::compose(
                &retry::preface(9, "claude", None, None),
                task,
            ))),
            retry_of: Some(9),
            ..Default::default()
        };
        assert_eq!(
            Task::from_worker(&worker),
            Task::Sent {
                text: task.to_owned(),
                cut: false,
                retry_of: Some(9)
            }
        );

        let text = "長".repeat(journal::TEXT_CHARS);
        let clipped = journal::clip(&run_attr::with_longest_header(&text));
        let workers = journal::workers(&[
            serde_json::json!({"ev":"spawn", "pane":4}),
            serde_json::json!({"ev":"send", "pane":4, "text":clipped}),
        ]);
        let shown_chars = journal::TEXT_CHARS - run_attr::with_longest_header("").len();
        assert_eq!(
            Task::from_worker(&workers[0]),
            Task::Sent {
                text: "長".repeat(shown_chars),
                cut: true,
                retry_of: None
            }
        );
    }
}
