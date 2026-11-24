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

use crate::model::brief;
use crate::model::reduce::{self, ReduceError};
use crate::model::rules;
use crate::model::types::{
    AgentChoice, Author, Brief, BriefKind, CRITERIA_ROUND, CriteriaEdit, CriterionSource, Event,
    EvidenceBody, EvidenceKey, FileFact, Isolation, JUDGE_ID, MAX_CAPTION, MAX_CRITERIA,
    MAX_CUSTOM_STRATEGY, MAX_MAX_ROUNDS, MAX_PANEL, MAX_STATEMENT, MIN_PANEL, Message, Millis,
    MsgKind, PartId, Participant, Phase, Rebuild, Rerun, RerunRequest, Role, RoundKind, RoundNo,
    Session, SessionId, SpecFile, Stored, Strategy, StrategyChoice, Target, TargetKind,
    TargetManifest, VerifierChoice, WorktreeState,
};
use crate::model::validate;
use crate::store;

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
    /// `items` was gone. `tails` holds, for each `command` item whose
    /// reported exit status is not 0 and whose log was stored, the last
    /// [`JUDGE_TAIL_LINES`](crate::model::types::JUDGE_TAIL_LINES) lines of
    /// the stored copy ([`crate::evidence::tail`]); the engine logs them
    /// with `EvidenceIngested` for the judge's ruling brief.
    Ingested {
        round: RoundNo,
        part: PartId,
        items: Vec<(EvidenceKey, Stored)>,
        tails: BTreeMap<EvidenceKey, String>,
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
    /// open brief goes out again in full, without counting a retry (the
    /// new pane never saw what a pending correction would refer to).
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

fn rejected<T>(why: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::Rejected(why.into()))
}

/// Why a criterion's 検証方法 (`how`) cannot be left blank: the judge's
/// criteria are held to the same rule (L1-6).
const HOW_EMPTY: &str = "検証方法（how）を書いてください";

/// Why a re-run item that never ran is `not_run` in 共有 mode (L1-5 step 1).
pub const SHARED_NOT_RUN: &str =
    "shared (read-only) mode: a re-run would run in the person's own tree, so none is made";

/// The judge and the verifiers a panel gives: verifier ids by kind
/// (`claude`, `claude-2`, …), custom strategy ids in panel order (`custom`,
/// `custom-2`, …).
pub fn participants(
    judge: &AgentChoice,
    panel: &[VerifierChoice],
) -> (Participant, Vec<Participant>) {
    let mut verifiers: Vec<Participant> = Vec::with_capacity(panel.len());
    let mut customs = 0;
    for choice in panel {
        let kind = choice.agent.kind;
        let seen = verifiers.iter().filter(|p| p.kind == kind).count();
        let id = match seen {
            0 => kind.command().to_owned(),
            n => format!("{}-{}", kind.command(), n + 1),
        };
        let strategy = match &choice.strategy {
            StrategyChoice::Builtin(id) => Strategy::Builtin { id: *id },
            StrategyChoice::Custom(text) => {
                customs += 1;
                let id = match customs {
                    1 => "custom".to_owned(),
                    n => format!("custom-{n}"),
                };
                Strategy::Custom {
                    id,
                    text: text.trim().to_owned(),
                }
            }
        };
        verifiers.push(Participant {
            id: PartId::new(id),
            kind,
            role: Role::Verifier,
            pane_id: None,
            setup: choice.agent.setup.clone(),
            strategy: Some(strategy),
        });
    }
    let judge = Participant {
        id: PartId::new(JUDGE_ID),
        kind: judge.kind,
        role: Role::Judge,
        pane_id: None,
        setup: judge.setup.clone(),
        strategy: None,
    };
    (judge, verifiers)
}

/// Why a panel cannot start a verification.
pub fn check_panel(judge: &AgentChoice, panel: &[VerifierChoice]) -> Result<(), String> {
    if panel.len() < MIN_PANEL {
        return Err(format!("検証者は{MIN_PANEL}体以上必要です"));
    }
    if panel.len() > MAX_PANEL {
        return Err(format!("検証者は{MAX_PANEL}体までです"));
    }
    judge.setup.check(judge.kind)?;
    for (i, choice) in panel.iter().enumerate() {
        choice.agent.setup.check(choice.agent.kind)?;
        if let StrategyChoice::Custom(text) = &choice.strategy {
            let n = text.trim().chars().count();
            if n == 0 {
                return Err(format!("検証者 {} のカスタム手法が空です", i + 1));
            }
            if n > MAX_CUSTOM_STRATEGY {
                return Err(format!(
                    "検証者 {} のカスタム手法は{MAX_CUSTOM_STRATEGY}文字までです（{n}文字）",
                    i + 1
                ));
            }
        }
    }
    Ok(())
}

/// The person's criteria as typed (one per line): trimmed, empty lines
/// dropped.
pub fn user_criteria(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|l| l.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `logged` and `want` name the same file: the same path, or the
/// same name in one directory reached another way (`dir` given through a
/// symlink). The file need not exist yet.
fn same_file(logged: &Path, want: &Path) -> bool {
    if logged == want {
        return true;
    }
    resolved(logged).is_some_and(|logged| Some(logged) == resolved(want))
}

/// `path` with its deepest existing ancestor made canonical.
fn resolved(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|dir| {
        let rest = path.strip_prefix(dir).ok()?;
        Some(std::fs::canonicalize(dir).ok()?.join(rest))
    })
}

/// Why the verification logged in `dir` cannot be resumed from its log:
/// review's checks, the snapshot and its spec copy against the logged
/// digests ([`crate::capture::verify`] with `Session::spec`) and each
/// recorded worktree ([`crate::worktree::check`]).
pub fn check_resumable(session: &Session, dir: &Path) -> Result<(), String> {
    check_home(session, dir)?;
    for part in std::iter::once(&session.judge).chain(&session.verifiers) {
        part.setup.check(part.kind)?;
    }
    let choice = |p: &Participant| AgentChoice {
        kind: p.kind,
        setup: p.setup.clone(),
    };
    let panel: Vec<VerifierChoice> = session
        .verifiers
        .iter()
        .map(|p| VerifierChoice {
            agent: choice(p),
            strategy: match &p.strategy {
                Some(Strategy::Custom { text, .. }) => StrategyChoice::Custom(text.clone()),
                Some(Strategy::Builtin { id }) => StrategyChoice::Builtin(*id),
                None => StrategyChoice::Custom(String::new()),
            },
        })
        .collect();
    let (judge, verifiers) = participants(&choice(&session.judge), &panel);
    let same = |a: &Participant, b: &Participant| {
        a.id == b.id && a.role == b.role && a.strategy == b.strategy
    };
    if !same(&judge, &session.judge)
        || verifiers.len() != session.verifiers.len()
        || !verifiers
            .iter()
            .zip(&session.verifiers)
            .all(|(a, b)| same(a, b))
    {
        return Err("ログの参加者 ID がパネルと一致しません".to_owned());
    }
    check_reply_paths(session)?;
    let reply_dir = store::reply_dir(session);
    let target = store::target_dir(&reply_dir);
    crate::fsx::check_no_links(&store::reply_base(session), &target)
        .map_err(|e| format!("スナップショットの場所が変わっています: {e}"))?;
    crate::capture::verify(
        &target,
        &session.manifest,
        &session.rebuild,
        session.spec.as_ref(),
    )?;
    for (part, tree) in &session.worktrees {
        let want = crate::worktree::path_for(&session.main_worktree, &session.id, part);
        if !same_file(&tree.path, &want) {
            return Err(format!(
                "{part} のワークツリー {} がこの検証の置き場所と一致しません",
                tree.path.display()
            ));
        }
        // A missing one is rebuilt from the recipe when the round needs it.
        if tree.state == WorktreeState::Made && tree.path.exists() {
            crate::worktree::check(
                &session.main_worktree,
                &tree.path,
                &session.rebuild.head_sha,
            )
            .map_err(|e| format!("{part} のワークツリーを再開できません: {e}"))?;
        }
    }
    Ok(())
}

/// That the log in `dir` is this folder's (its id is the folder's name)
/// and its workdir is there.
fn check_home(session: &Session, dir: &Path) -> Result<(), String> {
    if dir.file_name() != Some(std::ffi::OsStr::new(session.id.as_str())) {
        return Err(format!(
            "ログの検証 ID {} がフォルダ名と一致しません",
            session.id
        ));
    }
    if !session.workdir.is_dir() {
        return Err(format!(
            "作業ディレクトリ {} がありません",
            session.workdir.display()
        ));
    }
    Ok(())
}

/// The paths a stored verification's log points at are its own: its id
/// names its folder `dir`, its workdir exists, and every reply path it
/// logged is the one the store gives for it.
pub fn check_store_paths(session: &Session, dir: &Path) -> Result<(), String> {
    check_home(session, dir)?;
    check_reply_paths(session)
}

/// Every reply path the log holds is the one the store gives.
fn check_reply_paths(session: &Session) -> Result<(), String> {
    let reply_dir = store::reply_dir(session);
    if let Some(brief) = &session.criteria_brief
        && (brief.to != session.judge.id
            || !same_file(&brief.reply_path, &store::criteria_path(&reply_dir)))
    {
        return Err("基準の返答パスがこの検証の保存先と一致しません".to_owned());
    }
    for round in &session.rounds {
        for brief in round.briefs.iter().chain(&round.rerun_briefs) {
            let dir = store::verifier_reply_dir(session, &brief.to);
            let want = match brief.kind {
                BriefKind::Work => store::reply_path(&dir, round.no, &brief.to),
                BriefKind::Rerun => store::rerun_dir(&dir, round.no, &brief.to),
            };
            if !session.is_verifier(&brief.to) || !same_file(&brief.reply_path, &want) {
                return Err(format!(
                    "R{} の {} の返答パスがこの検証の保存先と一致しません",
                    round.no, brief.to
                ));
            }
        }
        if let Some(brief) = &round.judge_brief
            && (brief.to != session.judge.id
                || !same_file(
                    &brief.reply_path,
                    &store::judge_reply_path(&reply_dir, round.no),
                ))
        {
            return Err(format!(
                "R{} の判定役の返答パスがこの検証の保存先と一致しません",
                round.no
            ));
        }
    }
    Ok(())
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
    /// The top level of the target's checkout ([`crate::capture::repo_root`]
    /// of the target's repository): see [`Session::workdir`].
    pub workdir: PathBuf,
    /// The repository's main worktree ([`crate::worktree::main_worktree`]):
    /// see [`Session::main_worktree`].
    pub main_worktree: PathBuf,
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
    /// A round waits for the worktrees [`Effect::MakeWorktrees`] asked for:
    /// it starts on the first tick after every one is made.
    pending_start: bool,
}

impl Engine {
    /// A new verification logged in `dir`; the returned output holds its
    /// first batch. Nothing runs until [`Input::Start`].
    pub fn create_in(
        id: SessionId,
        setup: Setup<'_>,
        dir: &Path,
        config: Config,
        now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        if let Err(why) = check_panel(&setup.judge, setup.panel) {
            return rejected(why);
        }
        let cap = setup.round_cap.max(MAX_MAX_ROUNDS);
        if !(1..=cap).contains(&setup.max_rounds) {
            return rejected(format!("ラウンド上限は1〜{cap}です"));
        }
        if setup.target.kind == TargetKind::Diff
            && setup.manifest.files.is_empty()
            && setup.manifest.omitted.is_empty()
        {
            return rejected("検証する差分が空です");
        }
        let user = user_criteria(&setup.user_criteria);
        if user.len() > MAX_CRITERIA {
            return rejected(format!(
                "受け入れ基準は{MAX_CRITERIA}件までです（{}件）",
                user.len()
            ));
        }
        if let Some(line) = user.iter().find(|l| l.chars().count() > MAX_STATEMENT) {
            return rejected(format!(
                "受け入れ基準は1件{MAX_STATEMENT}文字までです: {line}"
            ));
        }
        let (judge, verifiers) = participants(&setup.judge, setup.panel);
        let batch = vec![Event::Created {
            id,
            request: setup.request.trim().to_owned(),
            target: Box::new(setup.target),
            manifest: Box::new(setup.manifest),
            rebuild: setup.rebuild,
            isolation: setup.isolation,
            spec: setup.spec,
            user_criteria: user,
            workdir: setup.workdir,
            main_worktree: setup.main_worktree,
            max_rounds: setup.max_rounds,
            judge,
            verifiers,
            confirm: setup.confirm,
            at: now,
        }];
        let session = reduce::create(&batch)?;
        let engine = Engine::resume(session, dir, config);
        Ok((
            engine,
            Output {
                batches: vec![batch],
                effects: Vec::new(),
            },
        ))
    }

    pub fn resume(session: Session, dir: &Path, config: Config) -> Engine {
        Engine {
            session,
            dir: dir.to_owned(),
            config,
            waiting: BTreeMap::new(),
            pending_start: false,
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

    pub fn handle(&mut self, input: Input) -> Result<Output, EngineError> {
        let mut out = Output::default();
        if self.session.closed {
            return match input {
                Input::UserExtend { by, at } => {
                    self.extend(&mut out, by, at)?;
                    Ok(out)
                }
                Input::WorktreeRemoved { part } => {
                    self.worktree_removed(&mut out, &part)?;
                    Ok(out)
                }
                // One the runner was still making when the verification
                // closed: logged, so it is on record, and removed (L1-3).
                Input::WorktreeMade { part, path } => {
                    if !self.session.is_verifier(&part) {
                        return rejected(format!("no verifier {part}"));
                    }
                    self.commit(&mut out, vec![Event::WorktreeMade { part, path }])?;
                    out.effects.push(Effect::RemoveWorktrees);
                    Ok(out)
                }
                Input::UserIntervention { .. }
                | Input::UserRetry
                | Input::UserClose
                | Input::UserSetHold { .. }
                | Input::UserContinue { .. }
                | Input::UserStartVerify { .. }
                | Input::CriteriaEdit { .. }
                | Input::AttachEvidence { .. } => rejected("検証は終了しています"),
                _ => Ok(out),
            };
        }
        match input {
            Input::Start { now } => self.start(&mut out, now)?,
            Input::PaneSpawned { part, pane_id } => {
                if self.session.participant(&part).is_none() {
                    return rejected(format!("no participant {part}"));
                }
                self.commit(
                    &mut out,
                    vec![Event::PaneAssigned {
                        part: part.clone(),
                        pane_id,
                    }],
                )?;
                if let Some((round, kind)) = self.open_of(&part)
                    && self
                        .open_brief(round, &part, kind)
                        .is_some_and(|b| b.receipt.rejected.is_some() && b.receipt.sent.is_none())
                {
                    self.retry_or_fail(
                        &mut out,
                        round,
                        &part,
                        kind,
                        "the agent's pane was reopened before the correction went out".to_owned(),
                    )?;
                }
            }
            Input::Sent {
                round,
                part,
                kind,
                at,
            } => {
                self.waiting.remove(&part);
                if self.open_brief(round, &part, kind).is_some() {
                    self.commit(
                        &mut out,
                        vec![Event::BriefSent {
                            round,
                            to: part,
                            kind,
                            at,
                        }],
                    )?;
                }
                self.advance(&mut out)?;
            }
            Input::SendFailed {
                round,
                part,
                kind,
                error,
            } => {
                if self.open_brief(round, &part, kind).is_some() {
                    self.retry_or_fail(
                        &mut out,
                        round,
                        &part,
                        kind,
                        format!("send failed: {error}"),
                    )?;
                }
                self.advance(&mut out)?;
            }
            Input::AgentStateChanged { part, state, at } => {
                self.agent_state(&mut out, &part, state, at)?;
                self.advance(&mut out)?;
            }
            Input::ReplyFileWritten {
                round,
                part,
                content,
                files,
                at,
            } => {
                self.reply(&mut out, round, &part, &content, &files, at)?;
                self.advance(&mut out)?;
            }
            Input::ReplyUnreadable {
                round,
                part,
                error,
                digest,
                at,
            } => {
                if let Some(brief) = self.open_brief(round, &part, BriefKind::Work).cloned() {
                    self.mark_started(&mut out, round, &brief, at)?;
                    self.reject_reply(&mut out, round, &brief, digest, error)?;
                }
                self.advance(&mut out)?;
            }
            Input::CriteriaEdit { edit, at } => self.edit_criteria(&mut out, edit, at)?,
            Input::UserStartVerify { at } => {
                if self.session.phase() != Phase::Confirming {
                    return rejected("基準確認待ちではありません");
                }
                self.commit(&mut out, vec![Event::CriteriaFrozen { at }])?;
                self.start_next(&mut out, at, false)?;
            }
            Input::WorktreeMade { part, path } => {
                if !self.session.is_verifier(&part) {
                    return rejected(format!("no verifier {part}"));
                }
                self.commit(&mut out, vec![Event::WorktreeMade { part, path }])?;
            }
            Input::WorktreeFailed { part, path, error } => {
                if !self.session.is_verifier(&part) {
                    return rejected(format!("no verifier {part}"));
                }
                self.pending_start = false;
                self.commit(
                    &mut out,
                    vec![Event::WorktreeFailed {
                        part,
                        path,
                        reason: error,
                    }],
                )?;
            }
            Input::WorktreeRemoved { part } => self.worktree_removed(&mut out, &part)?,
            Input::Ingested {
                round,
                part,
                items,
                tails,
            } => {
                self.ingested(&mut out, round, &part, items, tails)?;
                self.advance(&mut out)?;
            }
            Input::RerunWritten {
                round,
                part,
                request,
                script,
            } => {
                self.rerun_written(&mut out, round, &part, request, &script)?;
                self.advance(&mut out)?;
            }
            Input::RerunStarted { round, part, at } => {
                self.waiting.remove(&part);
                if let Some(brief) = self.open_brief(round, &part, BriefKind::Rerun)
                    && brief.receipt.started.is_none()
                {
                    self.commit(
                        &mut out,
                        vec![Event::BriefStarted {
                            round,
                            to: part,
                            kind: BriefKind::Rerun,
                            at,
                        }],
                    )?;
                }
            }
            Input::RerunReported {
                round,
                key,
                rerun,
                at,
            } => {
                self.rerun_reported(&mut out, round, key, rerun, at)?;
                self.advance(&mut out)?;
            }
            Input::AttachEvidence {
                key,
                body,
                stored,
                shows,
                at,
            } => self.attach(&mut out, key, body, stored, shows, at)?,
            Input::UserIntervention { to, body, at } => {
                self.intervention(&mut out, to, body, at)?
            }
            Input::UserRetry => self.user_retry(&mut out)?,
            Input::UserSetHold { hold, at } => {
                if hold != self.session.hold {
                    let waiting = self.session.waits_to_continue();
                    self.commit(&mut out, vec![Event::HoldSet { hold }])?;
                    if waiting && !hold {
                        self.start_next(&mut out, at, false)?;
                    }
                }
            }
            Input::UserContinue { at } => {
                if !self.session.hold {
                    return rejected("ラウンド後に停止していません");
                }
                if !self.session.waits_to_continue() {
                    return rejected("ラウンドはまだ進行中です");
                }
                self.start_next(&mut out, at, true)?;
            }
            Input::UserExtend { by, at } => self.extend(&mut out, by, at)?,
            Input::Redeliver { round, part, kind } => {
                if let Some(brief) = self.open_brief(round, &part, kind)
                    && brief.receipt.sent.is_some()
                    && !Self::script_started(brief)
                {
                    // The whole brief, not a correction: the agent in the
                    // new pane never saw the brief or the rejected reply
                    // (it was cleared), as a retry sends it.
                    out.effects.push(Effect::SendBrief {
                        round,
                        part: brief.to.clone(),
                        kind: brief.kind,
                        body: brief.body.clone(),
                    });
                }
            }
            Input::UserClose => self.close(&mut out)?,
            Input::Tick { now } => {
                self.timeouts(&mut out, now)?;
                if self.pending_start && self.worktrees_ready() {
                    self.pending_start = false;
                    self.start_next(&mut out, now, true)?;
                }
                self.advance(&mut out)?;
            }
        }
        Ok(out)
    }

    fn commit(&mut self, out: &mut Output, batch: Vec<Event>) -> Result<(), EngineError> {
        if batch.is_empty() {
            return Ok(());
        }
        reduce::apply(&mut self.session, &batch)?;
        out.batches.push(batch);
        Ok(())
    }

    fn is_judge(&self, part: &PartId) -> bool {
        *part == self.session.judge.id
    }

    fn current_no(&self) -> Option<RoundNo> {
        self.session.current_round().map(|r| r.no)
    }

    /// The brief `part` has open now, as `(round, kind)`.
    fn open_of(&self, part: &PartId) -> Option<(RoundNo, BriefKind)> {
        if self
            .open_brief(CRITERIA_ROUND, part, BriefKind::Work)
            .is_some()
        {
            return Some((CRITERIA_ROUND, BriefKind::Work));
        }
        let no = self.current_no()?;
        [BriefKind::Work, BriefKind::Rerun]
            .into_iter()
            .find(|kind| self.open_brief(no, part, *kind).is_some())
            .map(|kind| (no, kind))
    }

    /// `part`'s brief of `kind` in `round` while it still waits for its
    /// reply (round [`CRITERIA_ROUND`]: the judge's criteria brief).
    fn open_brief(&self, round: RoundNo, part: &PartId, kind: BriefKind) -> Option<&Brief> {
        let open = |b: &&Brief| b.receipt.replied.is_none() && b.receipt.failed.is_none();
        if round == CRITERIA_ROUND {
            return self
                .session
                .criteria_brief
                .as_ref()
                .filter(|b| {
                    kind == BriefKind::Work && &b.to == part && !self.session.criteria_accepted
                })
                .filter(open);
        }
        let current = self.session.current_round()?;
        if current.no != round {
            return None;
        }
        let in_play = match (self.is_judge(part), kind) {
            (true, BriefKind::Work) => current.phase == Phase::Ruling,
            (false, BriefKind::Work) => matches!(current.phase, Phase::Briefing | Phase::Verifying),
            (false, BriefKind::Rerun) => current.phase == Phase::Ingesting,
            (true, BriefKind::Rerun) => false,
        };
        current.brief(part, kind).filter(|_| in_play).filter(open)
    }

    fn outgoing(brief: &Brief) -> String {
        match &brief.receipt.rejected {
            Some(error) => brief::correction_note(&brief.to, error, &brief.reply_path),
            None => brief.body.clone(),
        }
    }

    fn send(out: &mut Output, round: RoundNo, brief: &Brief) {
        out.effects.push(Effect::SendBrief {
            round,
            part: brief.to.clone(),
            kind: brief.kind,
            body: Self::outgoing(brief),
        });
    }

    fn watch(out: &mut Output, round: RoundNo, brief: &Brief) {
        // A re-run brief has no reply file: the runner watches the script
        // it wrote ([`Effect::Rerun`]).
        if brief.kind == BriefKind::Work {
            out.effects.push(Effect::WatchReply {
                round,
                part: brief.to.clone(),
                path: brief.reply_path.clone(),
            });
        }
    }

    fn start(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        // The criteria phase.
        if !self.session.criteria_accepted {
            let Some(brief) = self.session.criteria_brief.clone() else {
                return self.issue_criteria_brief(out);
            };
            if brief.receipt.failed.is_none() {
                if brief.receipt.sent.is_some() {
                    self.waiting.insert(brief.to.clone(), Some(now));
                }
                Self::watch(out, CRITERIA_ROUND, &brief);
                if brief.receipt.sent.is_none() {
                    Self::send(out, CRITERIA_ROUND, &brief);
                }
            }
            return Ok(());
        }
        if self.session.frozen_at.is_none() {
            return Ok(());
        }
        let Some(round) = self.session.current_round().cloned() else {
            return self.start_next(out, now, false);
        };
        // A resumed verification: briefs out before it stopped get their full
        // time again from now, as the agents may have worked meanwhile.
        for brief in round
            .briefs
            .iter()
            .chain(&round.rerun_briefs)
            .chain(&round.judge_brief)
        {
            if brief.receipt.sent.is_some() && brief.receipt.replied.is_none() {
                self.waiting.insert(brief.to.clone(), Some(now));
            }
        }
        match round.phase {
            Phase::Briefing | Phase::Verifying => {
                for brief in &round.briefs {
                    if brief.receipt.replied.is_some() || brief.receipt.failed.is_some() {
                        continue;
                    }
                    Self::watch(out, round.no, brief);
                    if brief.receipt.sent.is_none() {
                        Self::send(out, round.no, brief);
                    }
                }
            }
            Phase::Ingesting => {
                if round
                    .replies
                    .keys()
                    .any(|part| !round.ingested.contains(part))
                {
                    out.effects.push(Effect::Ingest { round: round.no });
                }
                for part in &round.ingested {
                    if !self.reruns_pending(round.no, part) {
                        continue;
                    }
                    // A log cut between the ingest and its not_run batch:
                    // still no script or brief in 共有 mode (AC-11).
                    if self.session.isolation == Isolation::Shared {
                        self.shared_not_run(out, round.no, part)?;
                        continue;
                    }
                    match round.brief(part, BriefKind::Rerun) {
                        // The runner picks up watching every request whose
                        // items lack an outcome; an unsent brief goes now,
                        // unless its script started before `Sent` was
                        // logged (L1-5 step 4).
                        Some(brief)
                            if brief.receipt.sent.is_none()
                                && brief.receipt.failed.is_none()
                                && !Self::script_started(brief) =>
                        {
                            Self::send(out, round.no, brief)
                        }
                        Some(_) => {}
                        None if !round.reruns.contains_key(part) => {
                            out.effects.push(Effect::Rerun {
                                round: round.no,
                                part: part.clone(),
                            })
                        }
                        None => {}
                    }
                }
            }
            Phase::Ruling => {
                if let Some(brief) = &round.judge_brief
                    && brief.receipt.failed.is_none()
                {
                    Self::watch(out, round.no, brief);
                    if brief.receipt.sent.is_none() {
                        Self::send(out, round.no, brief);
                    }
                }
            }
            Phase::Closed => return self.start_next(out, now, false),
            Phase::Criteria | Phase::Confirming => {}
        }
        self.advance(out)
    }

    fn issue_criteria_brief(&mut self, out: &mut Output) -> Result<(), EngineError> {
        let reply_dir = self.reply_dir();
        let path = store::criteria_path(&reply_dir);
        let brief = brief::criteria_brief(&self.session, &reply_dir, &path);
        self.commit(
            out,
            vec![Event::BriefIssued {
                round: CRITERIA_ROUND,
                brief,
            }],
        )?;
        if let Some(brief) = &self.session.criteria_brief {
            Self::watch(out, CRITERIA_ROUND, brief);
            Self::send(out, CRITERIA_ROUND, brief);
        }
        Ok(())
    }

    /// Gives the verification `by` more rounds past its limit. One that
    /// ended at its limit is open again, and its next round starts unless
    /// it is held. An ended one whose worktrees are not all removed yet
    /// (closing asks for their removal, L1-3) waits for that: its next
    /// round is never sent into a worktree being removed, and rebuilds
    /// them.
    fn extend(&mut self, out: &mut Output, by: RoundNo, at: Millis) -> Result<(), EngineError> {
        if by == 0 {
            return rejected("1ラウンド以上追加してください");
        }
        if let Err(why) = self.session.extendable() {
            return rejected(why);
        }
        if self.session.closed
            && self
                .session
                .worktrees
                .values()
                .any(|t| t.state == WorktreeState::Made)
        {
            return rejected(
                "ワークツリーの削除が終わっていません（削除が終わってから追加してください）",
            );
        }
        let base = self
            .session
            .max_rounds
            .max(self.session.rounds.len() as RoundNo);
        let Some(max_rounds) = base.checked_add(by) else {
            return rejected(format!("ラウンド上限をこれ以上増やせません（{base}）"));
        };
        self.commit(out, vec![Event::RoundsExtended { max_rounds, at }])?;
        // The stored report loses its verdict (it is provisional again).
        out.effects.push(Effect::WriteReport);
        self.start_next(out, at, false)
    }

    /// Ends the verification: the report is written and the worktrees made
    /// for it are removed (L1-3).
    fn close(&mut self, out: &mut Output) -> Result<(), EngineError> {
        self.commit(out, vec![Event::SessionClosed])?;
        out.effects.push(Effect::WriteReport);
        if self
            .session
            .worktrees
            .values()
            .any(|t| t.state == WorktreeState::Made)
        {
            out.effects.push(Effect::RemoveWorktrees);
        }
        Ok(())
    }

    fn worktree_removed(&mut self, out: &mut Output, part: &PartId) -> Result<(), EngineError> {
        if self
            .session
            .worktrees
            .get(part)
            .is_some_and(|t| t.state != WorktreeState::Removed)
        {
            self.commit(out, vec![Event::WorktreeRemoved { part: part.clone() }])?;
        }
        Ok(())
    }

    /// Whether every verifier can run: always in 共有 mode, else once each
    /// has its worktree.
    fn worktrees_ready(&self) -> bool {
        self.session.isolation == Isolation::Shared
            || self.session.verifier_ids().all(|part| {
                self.session
                    .worktrees
                    .get(part)
                    .is_some_and(|t| t.state == WorktreeState::Made)
            })
    }

    /// Starts the round after a ruled one (or the first), unless the
    /// verification is over or held; with `go`, past the hold. In worktree
    /// mode the worktrees are made first; the round then starts on the
    /// next tick ([`Engine::pending_start`]).
    fn start_next(&mut self, out: &mut Output, now: Millis, go: bool) -> Result<(), EngineError> {
        if self.session.closed || self.session.frozen_at.is_none() {
            return Ok(());
        }
        if let Some(round) = self.session.current_round()
            && round.phase != Phase::Closed
        {
            return Ok(());
        }
        // A ruled round that gave the verdict ends the verification, also
        // when the log was cut before its close (an early close resumed).
        if self.session.verdict().is_some() {
            return self.close(out);
        }
        if self.session.rounds.len() as RoundNo >= self.session.max_rounds {
            return self.close(out);
        }
        if self.session.hold && !go && !self.session.rounds.is_empty() {
            return Ok(());
        }
        if !self.worktrees_ready() {
            let failed = self
                .session
                .worktrees
                .values()
                .any(|t| matches!(t.state, WorktreeState::Failed { .. }));
            // A failed worktree refuses the round until the person retries.
            if !failed && !self.pending_start {
                self.pending_start = true;
                out.effects.push(Effect::MakeWorktrees);
            }
            return Ok(());
        }
        let no = self.session.rounds.len() as RoundNo + 1;
        let kind = RoundKind::for_round(no);
        let assignments = match kind {
            RoundKind::Blind => BTreeMap::new(),
            RoundKind::Cross => {
                let given = self
                    .session
                    .round(no - 1)
                    .and_then(|r| r.outcome.as_ref())
                    .map(|o| o.assign.clone())
                    .unwrap_or_default();
                if given.is_empty() {
                    rules::default_assignments(&self.session, no)
                } else {
                    given
                }
            }
        };
        let started = Event::RoundStarted {
            no,
            kind,
            assignments,
            at: now,
        };
        // The round and its briefs go into one batch: a log cut after the
        // round's start alone would show a round with no one in it.
        let mut with_round = self.session.clone();
        reduce::apply(&mut with_round, std::slice::from_ref(&started))?;
        let parts: Vec<PartId> = self.session.verifier_ids().cloned().collect();
        let mut batch = vec![started];
        for part in &parts {
            let reply_dir = store::verifier_reply_dir(&with_round, part);
            let path = store::reply_path(&reply_dir, no, part);
            let worktree = store::verifier_root(&with_round, part);
            let brief = brief::verifier_brief(&with_round, part, no, &reply_dir, &path, &worktree);
            batch.push(Event::BriefIssued { round: no, brief });
        }
        self.commit(out, batch)?;
        if let Some(round) = self.session.current_round() {
            for brief in &round.briefs {
                Self::watch(out, no, brief);
                Self::send(out, no, brief);
            }
        }
        Ok(())
    }

    /// Whether `part`'s re-run items of `round` still lack an outcome.
    fn reruns_pending(&self, round: RoundNo, part: &PartId) -> bool {
        self.session
            .rerun_items(round, part)
            .iter()
            .any(|e| e.rerun.is_none())
    }

    fn advance(&mut self, out: &mut Output) -> Result<(), EngineError> {
        let Some(round) = self.session.current_round() else {
            return Ok(());
        };
        let no = round.no;
        let settled = |done: fn(&Brief) -> bool| {
            round
                .briefs
                .iter()
                .all(|b| done(b) || b.receipt.failed.is_some())
        };
        match round.phase {
            Phase::Briefing if settled(|b| b.receipt.sent.is_some()) => {
                self.commit(
                    out,
                    vec![Event::PhaseChanged {
                        round: no,
                        phase: Phase::Verifying,
                    }],
                )?;
                self.advance(out)
            }
            Phase::Verifying
                if settled(|b| b.receipt.started.is_some())
                    && settled(|b| b.receipt.replied.is_some()) =>
            {
                let any_reply = !round.replies.is_empty();
                self.commit(
                    out,
                    vec![Event::PhaseChanged {
                        round: no,
                        phase: Phase::Ingesting,
                    }],
                )?;
                if any_reply {
                    out.effects.push(Effect::Ingest { round: no });
                }
                self.advance(out)
            }
            Phase::Ingesting => {
                let done = round
                    .replies
                    .keys()
                    .all(|part| round.ingested.contains(part) && !self.reruns_pending(no, part));
                if done { self.rule(out, no) } else { Ok(()) }
            }
            _ => Ok(()),
        }
    }

    /// Ingesting → Ruling: every replied verifier's evidence is stored and
    /// every re-run of the round has its outcome.
    fn rule(&mut self, out: &mut Output, no: RoundNo) -> Result<(), EngineError> {
        let reply_dir = self.reply_dir();
        let path = store::judge_reply_path(&reply_dir, no);
        let mut with_phase = self.session.clone();
        let phase = Event::PhaseChanged {
            round: no,
            phase: Phase::Ruling,
        };
        reduce::apply(&mut with_phase, std::slice::from_ref(&phase))?;
        let tails = with_phase
            .round(no)
            .map(|r| r.tails.clone())
            .unwrap_or_default();
        let brief = brief::judge_brief(&with_phase, no, &reply_dir, &path, &tails);
        self.commit(out, vec![phase, Event::BriefIssued { round: no, brief }])?;
        if let Some(brief) = self
            .session
            .current_round()
            .and_then(|r| r.judge_brief.as_ref())
        {
            Self::watch(out, no, brief);
            Self::send(out, no, brief);
        }
        Ok(())
    }

    fn ingested(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        items: Vec<(EvidenceKey, Stored)>,
        tails: BTreeMap<EvidenceKey, String>,
    ) -> Result<(), EngineError> {
        let Some(current) = self.session.current_round() else {
            return Ok(());
        };
        if current.no != round
            || current.phase != Phase::Ingesting
            || !current.replies.contains_key(part)
            || current.ingested.contains(part)
        {
            return Ok(());
        }
        let mine = |key: &EvidenceKey| {
            key.split()
                .is_some_and(|(p, r, _)| &p == part && r == round)
                && self.session.evidence_item(key).is_some()
        };
        let items: Vec<(EvidenceKey, Stored)> =
            items.into_iter().filter(|(k, _)| mine(k)).collect();
        let tails: BTreeMap<EvidenceKey, String> =
            tails.into_iter().filter(|(k, _)| mine(k)).collect();
        self.commit(
            out,
            vec![Event::EvidenceIngested {
                round,
                part: part.clone(),
                items,
                tails,
            }],
        )?;
        if self.session.rerun_items(round, part).is_empty() {
            return Ok(());
        }
        match self.session.isolation {
            Isolation::Shared => self.shared_not_run(out, round, part)?,
            Isolation::Worktree => out.effects.push(Effect::Rerun {
                round,
                part: part.clone(),
            }),
        }
        Ok(())
    }

    /// In 共有 mode, every re-run item of `part` in `round` still without an
    /// outcome ends `not_run`: a re-run never runs in the person's tree
    /// (AC-11), so no script or brief is made.
    fn shared_not_run(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
    ) -> Result<(), EngineError> {
        let batch = self
            .session
            .rerun_items(round, part)
            .into_iter()
            .filter(|e| e.rerun.is_none())
            .map(|e| Event::RerunFinished {
                key: e.key.clone(),
                rerun: Rerun::NotRun {
                    why: SHARED_NOT_RUN.to_owned(),
                    log: None,
                },
            })
            .collect();
        self.commit(out, batch)
    }

    fn rerun_written(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        request: RerunRequest,
        script: &Path,
    ) -> Result<(), EngineError> {
        let Some(current) = self.session.current_round() else {
            return Ok(());
        };
        if current.no != round
            || current.phase != Phase::Ingesting
            || !current.ingested.contains(part)
            || current.reruns.contains_key(part)
            || self.session.isolation == Isolation::Shared
        {
            return Ok(());
        }
        let items: Vec<&EvidenceKey> = self
            .session
            .rerun_items(round, part)
            .into_iter()
            .map(|e| &e.key)
            .collect();
        if let Some(item) = request.items.iter().find(|i| !items.contains(&&i.key)) {
            return rejected(format!(
                "{} is not a re-run item of {part} in round {round}",
                item.key
            ));
        }
        let reply_dir = store::verifier_reply_dir(&self.session, part);
        let rerun_dir = store::rerun_dir(&reply_dir, round, part);
        let requested = Event::RerunRequested {
            round,
            part: part.clone(),
            nonce: request.nonce,
            items: request.items,
            script_digest: request.script_digest,
        };
        let mut with_request = self.session.clone();
        reduce::apply(&mut with_request, std::slice::from_ref(&requested))?;
        let brief = brief::rerun_brief(&with_request, part, round, script, &rerun_dir);
        self.commit(out, vec![requested, Event::BriefIssued { round, brief }])?;
        if let Some(brief) = self.open_brief(round, part, BriefKind::Rerun) {
            Self::send(out, round, brief);
        }
        Ok(())
    }

    fn rerun_reported(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        key: EvidenceKey,
        rerun: Rerun,
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some((part, key_round, _)) = key.split() else {
            return Ok(());
        };
        let open = self
            .session
            .current_round()
            .is_some_and(|r| r.no == round && r.phase == Phase::Ingesting);
        let item = self
            .session
            .rerun_items(round, &part)
            .into_iter()
            .find(|e| e.key == key)
            .map(|e| e.rerun.is_none());
        if !open || key_round != round || item != Some(true) {
            return Ok(());
        }
        let mut batch = vec![Event::RerunFinished { key, rerun }];
        let mut after = self.session.clone();
        reduce::apply(&mut after, &batch)?;
        let all_done = after
            .rerun_items(round, &part)
            .iter()
            .all(|e| e.rerun.is_some());
        let brief_open = after
            .round(round)
            .and_then(|r| r.brief(&part, BriefKind::Rerun))
            .is_some_and(|b| b.receipt.replied.is_none() && b.receipt.failed.is_none());
        if all_done && brief_open {
            batch.push(Event::ReplyAccepted {
                round,
                to: part,
                at,
            });
        }
        self.commit(out, batch)
    }

    fn attach(
        &mut self,
        out: &mut Output,
        key: EvidenceKey,
        body: EvidenceBody,
        stored: Stored,
        shows: Vec<String>,
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(round) = self.current_no() else {
            return rejected("証跡は検証の開始後に追加できます");
        };
        if !key.is_user() || self.session.evidence_item(&key).is_some() {
            return rejected(format!("証跡 {key} は追加できません"));
        }
        let EvidenceBody::File {
            caption,
            media: Some(_),
            marks,
            ..
        } = &body
        else {
            return rejected("追加できるのは画像と動画だけです");
        };
        let caption = caption.as_deref().map(str::trim).unwrap_or_default();
        if caption.is_empty() || caption.chars().count() > MAX_CAPTION {
            return rejected(format!("説明は1〜{MAX_CAPTION}文字で書いてください"));
        }
        if marks.windows(2).any(|w| w[1].t_ms < w[0].t_ms) {
            return rejected("マークは時刻順に並べてください");
        }
        if shows.is_empty() {
            return rejected("この証跡が示す基準を選んでください");
        }
        if let Some(id) = shows.iter().find(|id| self.session.criterion(id).is_none()) {
            return rejected(format!("不明な基準です: {id}"));
        }
        self.commit(
            out,
            vec![Event::EvidenceAttached {
                key,
                round,
                body,
                stored,
                shows,
                at,
            }],
        )
    }

    fn edit_criteria(
        &mut self,
        out: &mut Output,
        edit: CriteriaEdit,
        at: Millis,
    ) -> Result<(), EngineError> {
        if self.session.phase() != Phase::Confirming {
            return rejected("基準を変えられるのは確認待ちの間だけです");
        }
        let statement_ok = |text: &str| {
            let n = text.chars().count();
            (1..=MAX_STATEMENT).contains(&n) && !text.contains('\n')
        };
        let find = |id: &str| self.session.criterion(id);
        let edit = match edit {
            CriteriaEdit::Add {
                statement,
                how,
                priority,
            } => {
                if self.session.criteria.len() >= MAX_CRITERIA {
                    return rejected(format!("基準は{MAX_CRITERIA}件までです"));
                }
                let statement = statement.trim().to_owned();
                if !statement_ok(&statement) {
                    return rejected(format!("基準は1行・{MAX_STATEMENT}文字までです"));
                }
                let how = how.trim().to_owned();
                if how.is_empty() {
                    return rejected(HOW_EMPTY);
                }
                CriteriaEdit::Add {
                    statement,
                    how,
                    priority,
                }
            }
            CriteriaEdit::Edit {
                id,
                statement,
                how,
                priority,
            } => {
                if find(&id).is_none() {
                    return rejected(format!("不明な基準です: {id}"));
                }
                let statement = statement.map(|s| s.trim().to_owned());
                if statement.as_deref().is_some_and(|s| !statement_ok(s)) {
                    return rejected(format!("基準は1行・{MAX_STATEMENT}文字までです"));
                }
                if statement.is_none() && how.is_none() && priority.is_none() {
                    return rejected("変更がありません");
                }
                if how.as_deref().is_some_and(|h| h.trim().is_empty()) {
                    return rejected(HOW_EMPTY);
                }
                CriteriaEdit::Edit {
                    id,
                    statement,
                    how: how.map(|h| h.trim().to_owned()),
                    priority,
                }
            }
            CriteriaEdit::Remove { id } => {
                let Some(criterion) = find(&id) else {
                    return rejected(format!("不明な基準です: {id}"));
                };
                if !matches!(
                    criterion.source,
                    CriterionSource::Inferred | CriterionSource::Request
                ) {
                    return rejected(format!(
                        "{id} は削除できません（削除できるのは依頼・推定から作られた基準だけです。編集はできます）"
                    ));
                }
                if self.session.criteria.len() <= 1 {
                    return rejected("基準は1件以上必要です");
                }
                CriteriaEdit::Remove { id }
            }
            CriteriaEdit::Move { id, to } => {
                if find(&id).is_none() {
                    return rejected(format!("不明な基準です: {id}"));
                }
                if to >= self.session.criteria.len() {
                    return rejected("その位置には移動できません");
                }
                CriteriaEdit::Move { id, to }
            }
        };
        self.commit(out, vec![Event::CriteriaEdited { edit, at }])
    }

    /// Whether `brief` is a re-run brief whose script has started
    /// (`RerunStarted`): it is never sent again, whatever its pane reports,
    /// and the runner's watch takes its commands to their outcomes (L1-5
    /// step 4). A second start would run the same commands twice into the
    /// same logs and exit files.
    fn script_started(brief: &Brief) -> bool {
        brief.kind == BriefKind::Rerun && brief.receipt.started.is_some()
    }

    fn retry_or_fail(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        kind: BriefKind,
        reason: String,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part, kind) else {
            return Ok(());
        };
        if Self::script_started(brief) {
            return Ok(());
        }
        if brief.receipt.retries < self.config.max_retries {
            self.commit(
                out,
                vec![Event::BriefRetried {
                    round,
                    to: part.clone(),
                    kind,
                    reason,
                }],
            )?;
            if let Some(brief) = self.open_brief(round, part, kind) {
                Self::send(out, round, brief);
            }
        } else {
            let mut batch = vec![Event::BriefFailed {
                round,
                to: part.clone(),
                kind,
                reason: reason.clone(),
            }];
            // A re-run brief that never got its script going leaves every
            // item without a run (L1-5 step 4).
            if kind == BriefKind::Rerun {
                batch.extend(
                    self.session
                        .rerun_items(round, part)
                        .into_iter()
                        .filter(|e| e.rerun.is_none())
                        .map(|e| Event::RerunFinished {
                            key: e.key.clone(),
                            rerun: Rerun::NotRun {
                                why: format!("re-run did not start ({reason})"),
                                log: None,
                            },
                        }),
                );
            }
            self.commit(out, batch)?;
        }
        Ok(())
    }

    fn timeouts(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        let mut open: Vec<(RoundNo, Brief)> = Vec::new();
        if let Some(brief) =
            self.open_brief(CRITERIA_ROUND, &self.session.judge.id, BriefKind::Work)
        {
            open.push((CRITERIA_ROUND, brief.clone()));
        }
        if let Some(round) = self.session.current_round() {
            for brief in round
                .briefs
                .iter()
                .chain(&round.rerun_briefs)
                .chain(&round.judge_brief)
            {
                if self.open_brief(round.no, &brief.to, brief.kind).is_some() {
                    open.push((round.no, brief.clone()));
                }
            }
        }
        let mut due = Vec::new();
        for (round, brief) in open {
            let Some(sent) = brief.receipt.sent else {
                continue;
            };
            let since = match self.waiting.get(&brief.to) {
                Some(None) => continue,
                Some(Some(resumed)) => sent.max(*resumed),
                None => sent,
            };
            let waited = now.saturating_sub(since);
            if brief.receipt.started.is_none() && waited >= self.config.start_timeout {
                due.push((
                    round,
                    brief.to.clone(),
                    brief.kind,
                    format!("did not start within {}s", self.config.start_timeout / 1000),
                ));
            } else if brief.kind == BriefKind::Work
                && brief.receipt.started.is_some()
                && waited >= self.config.reply_timeout
            {
                // A started re-run is timed per command by the runner's
                // watch (L1-5 step 4), not here.
                due.push((
                    round,
                    brief.to.clone(),
                    brief.kind,
                    format!("no reply within {}s", self.config.reply_timeout / 1000),
                ));
            }
        }
        for (round, part, kind, reason) in due {
            self.retry_or_fail(out, round, &part, kind, reason)?;
        }
        Ok(())
    }

    fn agent_state(
        &mut self,
        out: &mut Output,
        part: &PartId,
        state: AgentState,
        at: Millis,
    ) -> Result<(), EngineError> {
        match (state, self.waiting.get(part)) {
            (AgentState::Permission, _) => {
                self.waiting.insert(part.clone(), None);
            }
            (_, Some(None)) => {
                self.waiting.insert(part.clone(), Some(at));
            }
            _ => {}
        }
        let Some((round, kind)) = self.open_of(part) else {
            return Ok(());
        };
        let Some(brief) = self.open_brief(round, part, kind) else {
            return Ok(());
        };
        if brief.receipt.sent.is_none() {
            return Ok(());
        }
        match state {
            // A re-run brief starts with its script's first `.pid`.
            AgentState::Running if brief.receipt.started.is_none() && kind == BriefKind::Work => {
                self.commit(
                    out,
                    vec![Event::BriefStarted {
                        round,
                        to: part.clone(),
                        kind,
                        at,
                    }],
                )?;
            }
            AgentState::Error => {
                self.retry_or_fail(out, round, part, kind, "agent reported an error".to_owned())?;
            }
            _ => {}
        }
        Ok(())
    }

    fn reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        content: &str,
        files: &[FileFact],
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part, BriefKind::Work).cloned() else {
            return Ok(());
        };
        self.mark_started(out, round, &brief, at)?;
        let digest = store::digest(content);
        if round == CRITERIA_ROUND {
            return match validate::parse_criteria(content, &self.session) {
                Ok(output) => {
                    let mut batch = vec![
                        Event::ReplyAccepted {
                            round,
                            to: part.clone(),
                            at,
                        },
                        Event::CriteriaAccepted {
                            summary: output.summary,
                            criteria: output.criteria,
                        },
                    ];
                    if !self.session.confirm {
                        batch.push(Event::CriteriaFrozen { at });
                    }
                    self.commit(out, batch)?;
                    // Every accepted judge output is reported (L1-8), the
                    // criteria too.
                    out.effects.push(Effect::WriteReport);
                    self.start_next(out, at, false)
                }
                Err(error) => self.reject_reply(out, round, &brief, digest, error.to_string()),
            };
        }
        if self.is_judge(part) {
            match validate::parse_judge(content, &self.session, round) {
                Ok(output) => {
                    let ends = output.verdict.is_some();
                    let batch = vec![
                        Event::ReplyAccepted {
                            round,
                            to: part.clone(),
                            at,
                        },
                        Event::MessagePosted {
                            message: Message {
                                id: self.session.next_message_id(0),
                                round,
                                author: Author::Part(part.clone()),
                                kind: MsgKind::Summary,
                                to: Vec::new(),
                                body: output.summary.clone(),
                                at,
                            },
                        },
                        Event::JudgeAccepted { round, output },
                        Event::PhaseChanged {
                            round,
                            phase: Phase::Closed,
                        },
                    ];
                    if ends {
                        self.commit(out, batch)?;
                        self.close(out)?;
                    } else {
                        self.commit(out, batch)?;
                        out.effects.push(Effect::WriteReport);
                        self.start_next(out, at, false)?;
                    }
                }
                Err(error) => self.reject_reply(out, round, &brief, digest, error.to_string())?,
            }
        } else {
            match validate::parse_reply(content, &self.session, part, round, files) {
                Ok(reply) => {
                    self.commit(
                        out,
                        vec![
                            Event::ReplyAccepted {
                                round,
                                to: part.clone(),
                                at,
                            },
                            Event::ReplyRecorded {
                                round,
                                part: part.clone(),
                                summary: reply.summary,
                                environment: reply.environment,
                                results: reply.results,
                                evidence: reply.evidence,
                                defects: reply.defects,
                                cross_checks: reply.cross_checks,
                                at,
                            },
                        ],
                    )?;
                }
                Err(error) => self.reject_reply(out, round, &brief, digest, error.to_string())?,
            }
        }
        Ok(())
    }

    /// A reply file written before Running was observed still proves the
    /// agent started.
    fn mark_started(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        brief: &Brief,
        at: Millis,
    ) -> Result<(), EngineError> {
        if brief.receipt.started.is_none() {
            self.commit(
                out,
                vec![Event::BriefStarted {
                    round,
                    to: brief.to.clone(),
                    kind: brief.kind,
                    at,
                }],
            )?;
        }
        Ok(())
    }

    fn reject_reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        brief: &Brief,
        digest: u64,
        error: String,
    ) -> Result<(), EngineError> {
        out.effects.push(Effect::ClearReply {
            path: brief.reply_path.clone(),
        });
        if brief.receipt.retries < self.config.max_retries {
            self.commit(
                out,
                vec![Event::ReplyRejected {
                    round,
                    to: brief.to.clone(),
                    error,
                    digest: Some(digest),
                }],
            )?;
            if let Some(updated) = self.open_brief(round, &brief.to, BriefKind::Work).cloned() {
                Self::watch(out, round, &updated);
                Self::send(out, round, &updated);
            }
        } else {
            self.commit(
                out,
                vec![Event::BriefFailed {
                    round,
                    to: brief.to.clone(),
                    kind: BriefKind::Work,
                    reason: format!("invalid output after retries: {error}"),
                }],
            )?;
        }
        Ok(())
    }

    fn intervention(
        &mut self,
        out: &mut Output,
        to: Vec<PartId>,
        body: String,
        at: Millis,
    ) -> Result<(), EngineError> {
        let body = body.trim().to_owned();
        if body.is_empty() {
            return rejected("メッセージが空です");
        }
        for part in &to {
            if self.session.participant(part).is_none() {
                return rejected(format!("不明な宛先です: {part}"));
            }
        }
        let round = self.current_no().unwrap_or(1);
        self.commit(
            out,
            vec![Event::MessagePosted {
                message: Message {
                    id: self.session.next_message_id(0),
                    round,
                    author: Author::User,
                    kind: MsgKind::Intervention,
                    to,
                    body,
                    at,
                },
            }],
        )
    }

    fn user_retry(&mut self, out: &mut Output) -> Result<(), EngineError> {
        // The criteria brief.
        if let Some(brief) = &self.session.criteria_brief
            && !self.session.criteria_accepted
        {
            if brief.receipt.failed.is_none() {
                return rejected("再試行できる失敗ブリーフがありません");
            }
            let to = brief.to.clone();
            self.commit(
                out,
                vec![Event::BriefReset {
                    round: CRITERIA_ROUND,
                    to: to.clone(),
                    kind: BriefKind::Work,
                }],
            )?;
            if let Some(brief) = self
                .open_brief(CRITERIA_ROUND, &to, BriefKind::Work)
                .cloned()
            {
                Self::watch(out, CRITERIA_ROUND, &brief);
                Self::send(out, CRITERIA_ROUND, &brief);
            }
            return Ok(());
        }
        // Worktrees that could not be made: try again, then start the round.
        let between = self
            .session
            .current_round()
            .is_none_or(|r| r.phase == Phase::Closed);
        let failed = self
            .session
            .worktrees
            .values()
            .any(|t| matches!(t.state, WorktreeState::Failed { .. }));
        if self.session.frozen_at.is_some() && between && failed {
            self.pending_start = true;
            out.effects.push(Effect::MakeWorktrees);
            return Ok(());
        }
        let Some(round) = self.session.current_round().cloned() else {
            return rejected("開始されたラウンドがありません");
        };
        let retryable: Vec<(PartId, BriefKind)> = match round.phase {
            Phase::Briefing | Phase::Verifying => round
                .briefs
                .iter()
                .filter(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
                .map(|b| (b.to.clone(), b.kind))
                .collect(),
            Phase::Ruling => round
                .judge_brief
                .iter()
                .filter(|b| b.receipt.failed.is_some())
                .map(|b| (b.to.clone(), b.kind))
                .collect(),
            _ => Vec::new(),
        };
        if retryable.is_empty() {
            return rejected("再試行できる失敗ブリーフがありません");
        }
        let batch: Vec<Event> = retryable
            .iter()
            .map(|(part, kind)| Event::BriefReset {
                round: round.no,
                to: part.clone(),
                kind: *kind,
            })
            .collect();
        self.commit(out, batch)?;
        for (part, kind) in retryable {
            if let Some(brief) = self.open_brief(round.no, &part, kind).cloned() {
                Self::watch(out, round.no, &brief);
                Self::send(out, round.no, &brief);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
