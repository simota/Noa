//! A task's related items (「関連成果」, `docs/specs/task-links.md` L1-5,
//! L1-6, L2-3): its runs, its change versions, the Review and Verify
//! sessions linked to it with the version each checked, and its PRs,
//! folded from the run log and the link log ([`crate::task_links`]).
//!
//! The surface is fixed by the spec (L2-2); the bodies are unit U2's.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::model::types::{Millis, Run, RunId, TaskRef};
use crate::task_links::{LinkEvent, SessionKind};

/// One distinct head commit of a task's change: `v<no>`, numbered in the
/// order the link log first names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub no: u32,
    pub commit: String,
    pub run: RunId,
    pub seen: Millis,
}

/// A linked session's outcome: still running, a verdict (its decision
/// name), or ended without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Running,
    Decided(String),
    EndedWithout,
}

/// What a session checked (L1-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetVersion {
    Version { no: u32, uncommitted: bool },
    Base { uncommitted: bool },
    Unknown,
}

/// A Review or Verify session linked to the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub kind: SessionKind,
    pub session: String,
    pub run: RunId,
    pub target: TargetVersion,
    pub outcome: Outcome,
    pub at: Millis,
}

/// A PR linked by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLink {
    pub id: String,
    pub url: String,
    pub number: Option<u64>,
    pub at: Millis,
}

/// Everything related to one task.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Related {
    pub runs: Vec<RunId>,
    pub versions: Vec<Version>,
    pub reviews: Vec<Checked>,
    pub verifications: Vec<Checked>,
    pub prs: Vec<PrLink>,
}

impl Related {
    /// The current version: the last one.
    pub fn current(&self) -> Option<&Version> {
        todo!("U2: Related::current")
    }

    /// Whether some session of `kind` checked the current version itself
    /// (not uncommitted changes on it) (L1-5).
    pub fn registered(&self, kind: SessionKind) -> bool {
        todo!("U2: Related::registered({kind:?})")
    }
}

/// The related items of every task, from its runs and the link log (L2-3).
/// Reads no clock, no git and no file.
pub fn fold(runs: &[Run], events: &[LinkEvent]) -> BTreeMap<TaskRef, Related> {
    todo!(
        "U2: related::fold over {} runs and {} events",
        runs.len(),
        events.len()
    )
}

/// `vN`, `vN+未コミット`, `着手時点`, `着手時点+未コミット` or `版不明` (L1-5).
pub fn version_text(target: &TargetVersion) -> String {
    todo!("U2: related::version_text({target:?})")
}

/// The outcome's label for a session of `kind` (L1-5).
pub fn outcome_label(kind: SessionKind, outcome: &Outcome) -> String {
    todo!("U2: related::outcome_label({kind:?}, {outcome:?})")
}

/// The PR URL to store for what the person typed (L1-8); `Err` says what is
/// accepted.
pub fn parse_pr(input: &str, gh_repo: Option<&str>) -> Result<String, String> {
    todo!("U2: related::parse_pr({input:?}, {gh_repo:?})")
}

/// The link log, re-read when its (len, mtime) changed, at most every
/// `POLL_MS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkScan {
    log: PathBuf,
    stat: Option<(u64, SystemTime)>,
    checked: Option<Millis>,
}

impl LinkScan {
    pub fn new(log: PathBuf) -> Self {
        todo!("U2: LinkScan::new({})", log.display())
    }

    /// The log's events when it changed since the last poll, else `None`.
    pub fn poll(&mut self, now: Millis) -> Option<Vec<LinkEvent>> {
        let Self { log, stat, checked } = self;
        todo!(
            "U2: LinkScan::poll({}, {stat:?}, {checked:?}) at {now}",
            log.display()
        )
    }
}
