//! Re-runs in the verifier's own pane (L1-5): the widget writes
//! `r<N>/<verifier>/rerun/` — one `<n>.cmd` per command exactly as
//! reported, and `run.sh`, a fixed script made from constants and
//! widget-chosen paths only — the verifier's agent starts it, and the
//! widget watches `<n>.pid`, `<n>.exit` and `done`, kills a command's
//! process group past its limit, checks the files it wrote against their
//! logged digests, and ingests each `<n>.log`. [`script`] and [`outcome`]
//! are pure; the rest does I/O.

use std::io;
use std::path::Path;

use crate::model::types::{AgentKind, EvidenceKey, PartId, Rerun, RerunRequest, RoundNo, Stored};

/// The directory under the verifier's `r<N>/<verifier>/`.
pub const RERUN_DIR: &str = "rerun";
pub const SCRIPT_FILE: &str = "run.sh";
/// Written last, holding the nonce.
pub const DONE_FILE: &str = "done";

/// Whether `kind`'s re-run brief starts `run.sh` detached (`nohup sh … &`)
/// rather than in the foreground. #TODO(agent): AC-35 settles, in a live
/// Noa, whether each kind's shell tool lets a detached process outlive the
/// tool call; a kind found to kill it changes to `false` here.
pub const fn detached(kind: AgentKind) -> bool {
    match kind {
        AgentKind::Claude => true,
        AgentKind::Codex => true,
        AgentKind::Agy => true,
    }
}

/// One command to run again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub key: EvidenceKey,
    /// Exactly as reported; written to `<n>.cmd`, never parsed or quoted.
    pub cmd: String,
    /// As reported, relative to the worktree.
    pub cwd: String,
}

/// What one command of a running script shows on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemStatus {
    /// Counted from 1.
    pub n: u32,
    /// From `<n>.pid`, once written.
    pub pid: Option<i32>,
    /// Whether that process still exists (`kill(pid, 0)`).
    pub alive: bool,
    /// From `<n>.exit`, once written.
    pub exit: Option<i32>,
}

/// A script's state as [`poll`] read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub items: Vec<ItemStatus>,
    /// What `done` holds, once written.
    pub done: Option<String>,
}

/// How a command ended, as the runner saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    Exited(i32),
    TimedOut,
    NotRun(String),
}

/// Which signal [`kill_group`] sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

/// `run.sh` for `items` (`(n, cwd)` pairs): `set -m`, then per command `cd`
/// to `<worktree>/<cwd>`, run `"${SHELL:-/bin/sh}" <dir>/<n>.cmd` in the
/// background with output to `<n>.log`, its pid to `<n>.pid`, wait, its
/// exit status to `<n>.exit` (temporary file + `mv`); at the end `done`
/// holding `nonce`. Every path single-quoted; no reported text in it.
pub fn script(_nonce: &str, _dir: &Path, _worktree: &Path, _items: &[(u32, &str)]) -> String {
    todo!("U3: rerun::script (L1-5 step 2)")
}

/// 16 bytes from `/dev/urandom`, as 32 hex digits.
pub fn nonce() -> io::Result<String> {
    todo!("U3: rerun::nonce")
}

/// Writes `<dir>/<n>.cmd` for each of `commands` (n from 1) and
/// `<dir>/run.sh`, creating `dir` below `base` exclusively, and returns
/// what [`crate::model::types::Event::RerunRequested`] logs.
pub fn write_request(
    _base: &Path,
    _dir: &Path,
    _worktree: &Path,
    _commands: &[Command],
) -> io::Result<RerunRequest> {
    todo!("U3: rerun::write_request (L1-5 step 1)")
}

/// What `dir` shows of `request`'s commands now.
pub fn poll(_dir: &Path, _request: &RerunRequest) -> io::Result<Status> {
    todo!("U3: rerun::poll (L1-5 step 4)")
}

/// Sends `signal` to the process group `pid` leads.
pub fn kill_group(_pid: i32, _signal: Signal) -> io::Result<()> {
    todo!("U3: rerun::kill_group")
}

/// Why the files in `dir` are not the ones written for `request`
/// (`run.sh` or a `<n>.cmd` changed, or `done` holding another nonce).
pub fn check_digests(_base: &Path, _dir: &Path, _request: &RerunRequest) -> Result<(), String> {
    todo!("U3: rerun::check_digests (L1-5 step 5)")
}

/// Copies each `<n>.log` of `request` from `dir` (below `src_base`) to
/// `<reply_dir>/evidence/r<round>/<part>/rerun-<n>.log`, cut at
/// [`crate::model::types::MAX_LOG_BYTES`] with the cut marked.
pub fn ingest_logs(
    _src_base: &Path,
    _dir: &Path,
    _reply_base: &Path,
    _reply_dir: &Path,
    _round: RoundNo,
    _part: &PartId,
    _request: &RerunRequest,
) -> Vec<(EvidenceKey, Result<Stored, String>)> {
    todo!("U3: rerun::ingest_logs (L1-5 step 5)")
}

/// The re-run outcome of a command the verifier reported with exit status
/// `reported`, given how it ended and its stored log.
pub fn outcome(_reported: i32, _end: End, _log: Option<Stored>) -> Rerun {
    todo!("U3: rerun::outcome")
}
