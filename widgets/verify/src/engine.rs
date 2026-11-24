//! The verification state machine. Pure: takes inputs with timestamps,
//! commits event batches to its session and returns them with effects to
//! carry out. Never does I/O (beyond checking a stored snapshot and the
//! worktrees on resume) and never reads a clock.
//!
//! Criteria (the judge writes `c/criteria.json`) → Confirming (held for
//! the person when 基準を確認してから検証 is on) → per round: Briefing →
//! Verifying (each verifier writes `r<N>/<verifier>/reply.json`) →
//! Ingesting (the runner copies and hashes evidence and runs re-runs on
//! [`Effect::Ingest`] / [`Effect::Rerun`]) → Ruling (the judge writes
//! `r<N>/judge.json`) → Closed, then the next round.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::reduce::ReduceError;
use crate::model::types::{
    AgentChoice, BriefKind, CriteriaEdit, Event, EvidenceBody, EvidenceKey, FileFact, Isolation,
    Millis, PartId, Participant, Rebuild, Rerun, RerunRequest, RoundNo, Session, SessionId,
    SpecFile, Stored, Target, TargetManifest, VerifierChoice,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// How long a sent brief may go without the agent starting to work
    /// (for a re-run brief: without the first `.pid`) before it is sent
    /// again.
    pub start_timeout: Millis,
    /// How long after a send the reply file may take before the brief is
    /// sent again.
    pub reply_timeout: Millis,
    /// How many times a brief is sent again (or a reply sent back) before
    /// it is marked failed.
    pub max_retries: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start_timeout: 90_000,
            reply_timeout: 45 * 60_000,
            max_retries: 2,
        }
    }
}

/// An agent pane's state, as Noa reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Permission,
    Input,
    Finished,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Begin the verification, or pick it up again after a restart.
    Start {
        now: Millis,
    },
    PaneSpawned {
        part: PartId,
        pane_id: u64,
    },
    /// A `SendBrief` reached the agent's pane.
    Sent {
        round: RoundNo,
        part: PartId,
        kind: BriefKind,
        at: Millis,
    },
    SendFailed {
        round: RoundNo,
        part: PartId,
        kind: BriefKind,
        error: String,
    },
    AgentStateChanged {
        part: PartId,
        state: AgentState,
        at: Millis,
    },
    /// A watched reply file appeared (or was rewritten) with this text;
    /// `files` is what [`crate::evidence::inspect`] found at every path a
    /// verifier's reply cites (empty for the judge's files).
    ReplyFileWritten {
        round: RoundNo,
        part: PartId,
        content: String,
        files: Vec<FileFact>,
        at: Millis,
    },
    /// A watched reply file stayed unreadable: it is turned down like an
    /// invalid reply. `digest` identifies what was read.
    ReplyUnreadable {
        round: RoundNo,
        part: PartId,
        error: String,
        digest: u64,
        at: Millis,
    },
    /// The person changes the criteria while they wait for confirmation.
    CriteriaEdit {
        edit: CriteriaEdit,
        at: Millis,
    },
    /// 検証を開始: freezes the criteria and starts round 1.
    UserStartVerify {
        at: Millis,
    },
    /// A [`Effect::MakeWorktrees`] worktree was built and checked.
    WorktreeMade {
        part: PartId,
        path: PathBuf,
    },
    WorktreeFailed {
        part: PartId,
        path: PathBuf,
        error: String,
    },
    /// A [`Effect::RemoveWorktrees`] worktree was removed.
    WorktreeRemoved {
        part: PartId,
    },
    /// `part`'s evidence of `round` was copied; an item missing from
    /// `items` was gone.
    Ingested {
        round: RoundNo,
        part: PartId,
        items: Vec<(EvidenceKey, Stored)>,
    },
    /// The runner wrote `part`'s re-run script for `round`
    /// ([`Effect::Rerun`]); the engine logs it and issues the re-run brief
    /// for `script` (absolute).
    RerunWritten {
        round: RoundNo,
        part: PartId,
        request: RerunRequest,
        script: PathBuf,
    },
    /// The re-run script's first `.pid` appeared: the re-run brief started.
    RerunStarted {
        round: RoundNo,
        part: PartId,
        at: Millis,
    },
    /// One re-run item has its outcome.
    RerunReported {
        round: RoundNo,
        key: EvidenceKey,
        rerun: Rerun,
        at: Millis,
    },
    /// The person attached media (証跡を追加), already copied and hashed.
    AttachEvidence {
        key: EvidenceKey,
        body: EvidenceBody,
        stored: Stored,
        shows: Vec<String>,
        at: Millis,
    },
    /// The person sends an instruction; `to` empty means everyone.
    UserIntervention {
        to: Vec<PartId>,
        body: String,
        at: Millis,
    },
    /// Try the current phase's failed briefs again.
    UserRetry,
    /// The person ends the verification: nothing more is sent, no verdict.
    UserClose,
    UserSetHold {
        hold: bool,
        at: Millis,
    },
    /// The person starts the next round of a held verification.
    UserContinue {
        at: Millis,
    },
    UserExtend {
        by: RoundNo,
        at: Millis,
    },
    /// The agent's pane was lost while the verification was stopped: its
    /// open brief goes out again as it was, without counting a retry.
    Redeliver {
        round: RoundNo,
        part: PartId,
        kind: BriefKind,
    },
    Tick {
        now: Millis,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Paste `body` into `part`'s pane and submit it; answer with `Sent` or
    /// `SendFailed`.
    SendBrief {
        round: RoundNo,
        part: PartId,
        kind: BriefKind,
        body: String,
    },
    /// Watch `path` and report its text with `ReplyFileWritten` once it is
    /// written (and again whenever it is rewritten).
    WatchReply {
        round: RoundNo,
        part: PartId,
        path: PathBuf,
    },
    /// Delete a rejected reply file so its rewrite is seen.
    ClearReply { path: PathBuf },
    /// Write the report to `<session dir>/report.md`.
    WriteReport,
    /// Build and check every verifier's worktree (worktree isolation);
    /// answer with `WorktreeMade` / `WorktreeFailed` per verifier.
    MakeWorktrees,
    /// Copy and hash every replied verifier's evidence of `round`; answer
    /// with `Ingested` per verifier.
    Ingest { round: RoundNo },
    /// Write `part`'s re-run script for `round`; answer with
    /// `RerunWritten`, then watch it and answer with `RerunStarted` and
    /// `RerunReported`.
    Rerun { round: RoundNo, part: PartId },
    /// Remove every worktree the log records as made.
    RemoveWorktrees,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub batches: Vec<Vec<Event>>,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The input is refused; the reason is for the person.
    Rejected(String),
    /// The engine's own events do not fold: a bug.
    Reduce(ReduceError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Rejected(why) => f.write_str(why),
            EngineError::Reduce(e) => write!(f, "internal error: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<ReduceError> for EngineError {
    fn from(e: ReduceError) -> Self {
        EngineError::Reduce(e)
    }
}

/// The judge and the verifiers a panel gives: verifier ids by kind
/// (`claude`, `claude-2`, …), custom strategy ids in panel order (`custom`,
/// `custom-2`, …).
pub fn participants(
    _judge: &AgentChoice,
    _panel: &[VerifierChoice],
) -> (Participant, Vec<Participant>) {
    todo!("U2: participants")
}

/// Why a panel cannot start a verification.
pub fn check_panel(_judge: &AgentChoice, _panel: &[VerifierChoice]) -> Result<(), String> {
    todo!("U2: check_panel")
}

/// Why the verification logged in `dir` cannot be resumed from its log:
/// review's checks, the snapshot ([`crate::capture::verify`]) and each
/// recorded worktree ([`crate::worktree::check`]).
pub fn check_resumable(_session: &Session, _dir: &Path) -> Result<(), String> {
    todo!("U2: check_resumable")
}

/// The paths a stored verification's log points at are its own: its id
/// names its folder `dir`, its workdir exists, and every reply path it
/// logged is the one the store gives for it.
pub fn check_store_paths(_session: &Session, _dir: &Path) -> Result<(), String> {
    todo!("U2: check_store_paths")
}

#[derive(Debug, Clone)]
pub struct Setup<'a> {
    pub request: &'a str,
    pub target: Target,
    pub manifest: TargetManifest,
    pub rebuild: Rebuild,
    pub spec: Option<SpecFile>,
    pub isolation: Isolation,
    /// The person's criteria, one per line as typed.
    pub user_criteria: Vec<String>,
    /// 基準を確認してから検証.
    pub confirm: bool,
    /// The main worktree's top level.
    pub workdir: PathBuf,
    pub max_rounds: RoundNo,
    /// The highest `max_rounds` accepted (review's).
    pub round_cap: RoundNo,
    pub judge: AgentChoice,
    pub panel: &'a [VerifierChoice],
}

#[derive(Debug, Clone)]
pub struct Engine {
    session: Session,
    dir: PathBuf,
    config: Config,
    /// Per agent: `None` while it sits on a permission prompt (its time
    /// limit does not run), else when its limit restarted (a resume).
    waiting: BTreeMap<PartId, Option<Millis>>,
}

impl Engine {
    /// A new verification logged in `dir`; the returned output holds its
    /// first batch. Nothing runs until [`Input::Start`].
    pub fn create_in(
        _id: SessionId,
        _setup: Setup<'_>,
        _dir: &Path,
        _config: Config,
        _now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        todo!("U2: Engine::create_in")
    }

    pub fn resume(session: Session, dir: &Path, config: Config) -> Engine {
        Engine {
            session,
            dir: dir.to_owned(),
            config,
            waiting: BTreeMap::new(),
        }
    }

    /// The judge's reply dir: snapshot, evidence, criteria and rulings.
    pub fn reply_dir(&self) -> PathBuf {
        crate::store::reply_dir(&self.session)
    }

    /// What every path below the judge's reply dir is checked against.
    pub fn reply_base(&self) -> PathBuf {
        crate::store::reply_base(&self.session)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn handle(&mut self, _input: Input) -> Result<Output, EngineError> {
        let _ = (&self.config, &self.waiting);
        todo!("U2: Engine::handle")
    }
}
