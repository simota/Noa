//! The review round state machine. Pure: takes inputs with timestamps,
//! commits event batches to its session and returns them with effects to
//! carry out. Never does I/O (beyond checking a stored snapshot on resume)
//! and never reads a clock.
//!
//! A round runs Briefing (every reviewer's brief is built and sent) →
//! Reviewing (each reviewer writes `r<N>/<reviewer>.json`) → Consolidating
//! (the lead writes `r<N>/lead.json`) → Closed, then the next round. Round
//! 1 is blind; every later round cross-examines the findings so far.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::brief;
use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{
    AgentChoice, Author, Brief, LEAD_ID, Lens, LensChoice, MAX_CUSTOM_LENS, MAX_MAX_ROUNDS,
    MAX_PANEL, MIN_PANEL, Message, Millis, MsgKind, PartId, Participant, Phase, ReviewerChoice,
    Role, RoundKind, RoundNo, Session, SessionId, Target, TargetManifest,
};
use crate::model::validate;
use crate::store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// How long a sent brief may go without the agent starting to work
    /// before it is sent again.
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
            reply_timeout: 15 * 60_000,
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
    /// Begin the review, or pick it up again after a restart.
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
        at: Millis,
    },
    SendFailed {
        round: RoundNo,
        part: PartId,
        error: String,
    },
    AgentStateChanged {
        part: PartId,
        state: AgentState,
        at: Millis,
    },
    /// A watched reply file appeared (or was rewritten) with this text.
    ReplyFileWritten {
        round: RoundNo,
        part: PartId,
        content: String,
        at: Millis,
    },
    /// A watched reply file stayed unreadable (not UTF-8, too large, not a
    /// plain file, or behind a link): it is turned down like an invalid
    /// reply. `digest` identifies what was read.
    ReplyUnreadable {
        round: RoundNo,
        part: PartId,
        error: String,
        digest: u64,
        at: Millis,
    },
    /// The user sends an instruction; `to` empty means everyone.
    UserIntervention {
        to: Vec<PartId>,
        body: String,
        at: Millis,
    },
    /// Try the current phase's failed briefs again.
    UserRetry,
    /// The user ends the review: nothing more is sent, no verdict is given.
    UserClose,
    /// The user holds the review after each consolidated round (`true`),
    /// or lets rounds follow each other again: a round that waits starts now.
    UserSetHold {
        hold: bool,
        at: Millis,
    },
    /// The user starts the next round of a held review.
    UserContinue {
        at: Millis,
    },
    /// The user gives the review `by` more rounds.
    UserExtend {
        by: RoundNo,
        at: Millis,
    },
    /// The agent's pane was lost while the review was stopped: its open
    /// brief goes out again as it was, without counting a retry.
    Redeliver {
        round: RoundNo,
        part: PartId,
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub batches: Vec<Vec<Event>>,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The input is refused; the reason is for the user.
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

/// The lead and the reviewers a panel gives: reviewer ids by kind (`claude`,
/// `claude-2`, …), custom lens ids in panel order (`custom`, `custom-2`, …).
pub fn participants(
    lead: &AgentChoice,
    panel: &[ReviewerChoice],
) -> (Participant, Vec<Participant>) {
    let mut reviewers: Vec<Participant> = Vec::with_capacity(panel.len());
    let mut customs = 0;
    for choice in panel {
        let kind = choice.agent.kind;
        let seen = reviewers.iter().filter(|p| p.kind == kind).count();
        let id = match seen {
            0 => kind.command().to_owned(),
            n => format!("{}-{}", kind.command(), n + 1),
        };
        let lens = match &choice.lens {
            LensChoice::Builtin(id) => Lens::Builtin { id: *id },
            LensChoice::Custom(text) => {
                customs += 1;
                let id = match customs {
                    1 => "custom".to_owned(),
                    n => format!("custom-{n}"),
                };
                Lens::Custom {
                    id,
                    text: text.trim().to_owned(),
                }
            }
        };
        reviewers.push(Participant {
            id: PartId::new(id),
            kind,
            role: Role::Reviewer,
            pane_id: None,
            setup: choice.agent.setup.clone(),
            lens: Some(lens),
        });
    }
    let lead = Participant {
        id: PartId::new(LEAD_ID),
        kind: lead.kind,
        role: Role::Lead,
        pane_id: None,
        setup: lead.setup.clone(),
        lens: None,
    };
    (lead, reviewers)
}

/// Why a panel cannot start a review.
pub fn check_panel(lead: &AgentChoice, panel: &[ReviewerChoice]) -> Result<(), String> {
    if panel.len() < MIN_PANEL {
        return Err(format!("レビュアーは{MIN_PANEL}体以上必要です"));
    }
    if panel.len() > MAX_PANEL {
        return Err(format!("レビュアーは{MAX_PANEL}体までです"));
    }
    lead.setup.check(lead.kind)?;
    for (i, choice) in panel.iter().enumerate() {
        choice.agent.setup.check(choice.agent.kind)?;
        if let LensChoice::Custom(text) = &choice.lens {
            let n = text.trim().chars().count();
            if n == 0 {
                return Err(format!("レビュアー {} のカスタム観点が空です", i + 1));
            }
            if n > MAX_CUSTOM_LENS {
                return Err(format!(
                    "レビュアー {} のカスタム観点は{MAX_CUSTOM_LENS}文字までです（{n}文字）",
                    i + 1
                ));
            }
        }
    }
    Ok(())
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

/// Why the review logged in `dir` cannot be resumed from its log. What the
/// log names is used as paths and in shell scripts, so a log edited by hand
/// (or moved) gets the checks a new session does, and its snapshot must be
/// the one captured.
pub fn check_resumable(session: &Session, dir: &Path) -> Result<(), String> {
    check_home(session, dir)?;
    for part in std::iter::once(&session.lead).chain(&session.reviewers) {
        part.setup.check(part.kind)?;
    }
    let choice = |p: &Participant| AgentChoice {
        kind: p.kind,
        setup: p.setup.clone(),
    };
    let panel: Vec<ReviewerChoice> = session
        .reviewers
        .iter()
        .map(|p| ReviewerChoice {
            agent: choice(p),
            lens: match &p.lens {
                Some(Lens::Custom { text, .. }) => LensChoice::Custom(text.clone()),
                Some(Lens::Builtin { id }) => LensChoice::Builtin(*id),
                None => LensChoice::Custom(String::new()),
            },
        })
        .collect();
    let (lead, reviewers) = participants(&choice(&session.lead), &panel);
    let same =
        |a: &Participant, b: &Participant| a.id == b.id && a.role == b.role && a.lens == b.lens;
    if !same(&lead, &session.lead)
        || reviewers.len() != session.reviewers.len()
        || !reviewers
            .iter()
            .zip(&session.reviewers)
            .all(|(a, b)| same(a, b))
    {
        return Err("ログの参加者 ID がパネルと一致しません".to_owned());
    }
    check_reply_paths(session, dir)?;
    let reply_dir = store::reply_dir(dir, session);
    let target = store::target_dir(&reply_dir);
    crate::fsx::check_no_links(&store::reply_base(dir, session), &target)
        .map_err(|e| format!("スナップショットの場所が変わっています: {e}"))?;
    crate::capture::verify(&target, &session.manifest)
}

/// That the log in `dir` is this folder's (its id is the folder's name)
/// and its workdir is there.
fn check_home(session: &Session, dir: &Path) -> Result<(), String> {
    if dir.file_name() != Some(std::ffi::OsStr::new(session.id.as_str())) {
        return Err(format!(
            "ログのレビュー ID {} がフォルダ名と一致しません",
            session.id
        ));
    }
    if let Some(workdir) = &session.workdir
        && !workdir.is_dir()
    {
        return Err(format!(
            "作業ディレクトリ {} がありません",
            workdir.display()
        ));
    }
    Ok(())
}

/// The paths a stored review's log points at are its own, as a resume
/// checks them: its id names its folder `dir`, its workdir exists, and
/// every reply path it logged is the one the store gives for it. A request
/// that writes below the review's reply dir (修正を依頼) checks this first.
pub fn check_store_paths(session: &Session, dir: &Path) -> Result<(), String> {
    check_home(session, dir)?;
    check_reply_paths(session, dir)
}

/// Every reply path the log holds is the one the store gives for `dir`.
fn check_reply_paths(session: &Session, dir: &Path) -> Result<(), String> {
    let reply_dir = store::reply_dir(dir, session);
    for round in &session.rounds {
        for brief in &round.briefs {
            if !session.is_reviewer(&brief.to)
                || !same_file(
                    &brief.reply_path,
                    &store::reply_path(&reply_dir, round.no, &brief.to),
                )
            {
                return Err(format!(
                    "R{} の {} の返答パスがこのレビューの保存先と一致しません",
                    round.no, brief.to
                ));
            }
        }
        if let Some(brief) = &round.lead_brief
            && (brief.to != session.lead.id
                || !same_file(
                    &brief.reply_path,
                    &store::lead_reply_path(&reply_dir, round.no),
                ))
        {
            return Err(format!(
                "R{} のリードの返答パスがこのレビューの保存先と一致しません",
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
    pub workdir: Option<PathBuf>,
    pub max_rounds: RoundNo,
    pub lead: AgentChoice,
    pub panel: &'a [ReviewerChoice],
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
    /// A new review logged in `dir`; the returned output holds its first
    /// batch. Nothing runs until [`Input::Start`].
    pub fn create_in(
        id: SessionId,
        setup: Setup<'_>,
        dir: &Path,
        config: Config,
        now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        if let Err(why) = check_panel(&setup.lead, setup.panel) {
            return rejected(why);
        }
        if !(1..=MAX_MAX_ROUNDS).contains(&setup.max_rounds) {
            return rejected(format!("ラウンド上限は1〜{MAX_MAX_ROUNDS}です"));
        }
        if setup.manifest.files.is_empty() && setup.manifest.omitted.is_empty() {
            return rejected("レビュー対象が空です");
        }
        let (lead, reviewers) = participants(&setup.lead, setup.panel);
        let batch = vec![Event::Created {
            id,
            request: setup.request.trim().to_owned(),
            target: Box::new(setup.target),
            manifest: Box::new(setup.manifest),
            workdir: setup.workdir,
            max_rounds: setup.max_rounds,
            lead,
            reviewers,
            at: now,
        }];
        let session = reduce::create(&batch)?;
        let engine = Engine {
            session,
            dir: dir.to_owned(),
            config,
            waiting: BTreeMap::new(),
        };
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
        }
    }

    /// Root of the snapshot and the reply files.
    pub fn reply_dir(&self) -> PathBuf {
        store::reply_dir(&self.dir, &self.session)
    }

    /// What every reply and snapshot path is checked against
    /// ([`store::reply_base`]).
    pub fn reply_base(&self) -> PathBuf {
        store::reply_base(&self.dir, &self.session)
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
                Input::UserIntervention { .. }
                | Input::UserRetry
                | Input::UserClose
                | Input::UserSetHold { .. }
                | Input::UserContinue { .. } => rejected("レビューは終了しています"),
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
                if let Some(no) = self.current_no()
                    && self
                        .open_brief(no, &part)
                        .is_some_and(|b| b.receipt.rejected.is_some() && b.receipt.sent.is_none())
                {
                    self.retry_or_fail(
                        &mut out,
                        no,
                        &part,
                        "the agent's pane was reopened before the correction went out".to_owned(),
                    )?;
                }
            }
            Input::Sent { round, part, at } => {
                self.waiting.remove(&part);
                if self.open_brief(round, &part).is_some() {
                    self.commit(
                        &mut out,
                        vec![Event::BriefSent {
                            round,
                            to: part,
                            at,
                        }],
                    )?;
                }
                self.advance(&mut out)?;
            }
            Input::SendFailed { round, part, error } => {
                if self.open_brief(round, &part).is_some() {
                    self.retry_or_fail(&mut out, round, &part, format!("send failed: {error}"))?;
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
                at,
            } => {
                self.reply(&mut out, round, &part, &content, at)?;
                self.advance(&mut out)?;
            }
            Input::ReplyUnreadable {
                round,
                part,
                error,
                digest,
                at,
            } => {
                if let Some(brief) = self.open_brief(round, &part).cloned() {
                    self.mark_started(&mut out, round, &part, &brief, at)?;
                    self.reject_reply(&mut out, round, &brief, digest, error)?;
                }
                self.advance(&mut out)?;
            }
            Input::UserIntervention { to, body, at } => {
                self.intervention(&mut out, to, body, at)?
            }
            Input::UserRetry => self.user_retry(&mut out)?,
            Input::UserSetHold { hold, at } => {
                if hold != self.session.hold {
                    let waiting = self.session.waits_to_continue();
                    self.commit(&mut out, vec![Event::HoldSet { hold }])?;
                    if waiting && !hold {
                        self.next_round(&mut out, at)?;
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
            Input::Redeliver { round, part } => {
                if let Some(brief) = self.open_brief(round, &part)
                    && brief.receipt.sent.is_some()
                {
                    Self::send(&mut out, round, brief);
                }
            }
            Input::UserClose => {
                self.commit(&mut out, vec![Event::SessionClosed])?;
                out.effects.push(Effect::WriteReport);
            }
            Input::Tick { now } => {
                self.timeouts(&mut out, now)?;
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

    fn is_lead(&self, part: &PartId) -> bool {
        *part == self.session.lead.id
    }

    fn current_no(&self) -> Option<RoundNo> {
        self.session.current_round().map(|r| r.no)
    }

    /// `part`'s brief in `round` while it still waits for a reply.
    fn open_brief(&self, round: RoundNo, part: &PartId) -> Option<&Brief> {
        let current = self.session.current_round()?;
        if current.no != round {
            return None;
        }
        let in_play = if self.is_lead(part) {
            current.phase == Phase::Consolidating
        } else {
            matches!(current.phase, Phase::Briefing | Phase::Reviewing)
        };
        current
            .brief(part)
            .filter(|b| in_play && b.receipt.replied.is_none() && b.receipt.failed.is_none())
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
            body: Self::outgoing(brief),
        });
    }

    fn watch(out: &mut Output, round: RoundNo, brief: &Brief) {
        out.effects.push(Effect::WatchReply {
            round,
            part: brief.to.clone(),
            path: brief.reply_path.clone(),
        });
    }

    fn start(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        let Some(round) = self.session.current_round() else {
            return self.next_round(out, now);
        };
        // A resumed session: briefs out before it stopped get their full
        // time again from now, as the agents may have worked meanwhile.
        for brief in round.briefs.iter().chain(&round.lead_brief) {
            if brief.receipt.sent.is_some() && brief.receipt.replied.is_none() {
                self.waiting.insert(brief.to.clone(), Some(now));
            }
        }
        for brief in &round.briefs {
            if brief.receipt.replied.is_some() || round.phase == Phase::Closed {
                continue;
            }
            Self::watch(out, round.no, brief);
            if matches!(round.phase, Phase::Briefing | Phase::Reviewing)
                && brief.receipt.failed.is_none()
                && brief.receipt.sent.is_none()
            {
                Self::send(out, round.no, brief);
            }
        }
        match (round.phase, &round.lead_brief) {
            (Phase::Consolidating, Some(brief)) if brief.receipt.failed.is_none() => {
                Self::watch(out, round.no, brief);
                if brief.receipt.sent.is_none() {
                    Self::send(out, round.no, brief);
                }
            }
            (Phase::Closed, _) => return self.next_round(out, now),
            _ => {}
        }
        self.advance(out)
    }

    /// Gives the review `by` more rounds past its limit. One that ended at
    /// its limit is open again, and its next round starts unless it is held.
    fn extend(&mut self, out: &mut Output, by: RoundNo, at: Millis) -> Result<(), EngineError> {
        if by == 0 {
            return rejected("1ラウンド以上追加してください");
        }
        if let Err(why) = self.session.extendable() {
            return rejected(why);
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
        self.next_round(out, at)
    }

    fn next_round(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        self.start_next(out, now, false)
    }

    /// Starts the round after a consolidated one (or the first), unless the
    /// review is over or held; with `go`, past the hold.
    fn start_next(&mut self, out: &mut Output, now: Millis, go: bool) -> Result<(), EngineError> {
        if self.session.closed {
            return Ok(());
        }
        if let Some(round) = self.session.current_round()
            && round.phase != Phase::Closed
        {
            return Ok(());
        }
        if self.session.rounds.len() as RoundNo >= self.session.max_rounds {
            self.commit(out, vec![Event::SessionClosed])?;
            out.effects.push(Effect::WriteReport);
            return Ok(());
        }
        if self.session.hold && !go && !self.session.rounds.is_empty() {
            return Ok(());
        }
        let no = self.session.rounds.len() as RoundNo + 1;
        let started = Event::RoundStarted {
            no,
            kind: RoundKind::for_round(no),
            at: now,
        };
        // The round and its briefs go into one batch: a log cut after the
        // round's start alone would show a round with no one in it.
        let mut with_round = self.session.clone();
        reduce::apply(&mut with_round, std::slice::from_ref(&started))?;
        let parts: Vec<PartId> = self.session.reviewer_ids().cloned().collect();
        let mut batch = vec![started];
        batch.extend(self.brief_events(&with_round, no, &parts));
        self.commit(out, batch)?;
        if let Some(round) = self.session.current_round() {
            for brief in &round.briefs {
                Self::watch(out, no, brief);
                Self::send(out, no, brief);
            }
        }
        Ok(())
    }

    fn brief_events(&self, session: &Session, no: RoundNo, parts: &[PartId]) -> Vec<Event> {
        let reply_dir = self.reply_dir();
        parts
            .iter()
            .map(|part| {
                let path = store::reply_path(&reply_dir, no, part);
                let brief = brief::reviewer_brief(session, part, no, &reply_dir, &path);
                Event::BriefIssued { round: no, brief }
            })
            .collect()
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
                        phase: Phase::Reviewing,
                    }],
                )?;
                self.advance(out)
            }
            Phase::Reviewing
                if settled(|b| b.receipt.started.is_some())
                    && settled(|b| b.receipt.replied.is_some()) =>
            {
                self.consolidate(out, no)
            }
            _ => Ok(()),
        }
    }

    fn consolidate(&mut self, out: &mut Output, no: RoundNo) -> Result<(), EngineError> {
        let reply_dir = self.reply_dir();
        let path = store::lead_reply_path(&reply_dir, no);
        let mut with_phase = self.session.clone();
        let phase = Event::PhaseChanged {
            round: no,
            phase: Phase::Consolidating,
        };
        reduce::apply(&mut with_phase, std::slice::from_ref(&phase))?;
        let brief = brief::lead_brief(&with_phase, no, &reply_dir, &path);
        self.commit(out, vec![phase, Event::BriefIssued { round: no, brief }])?;
        if let Some(brief) = self
            .session
            .current_round()
            .and_then(|r| r.lead_brief.as_ref())
        {
            Self::watch(out, no, brief);
            Self::send(out, no, brief);
        }
        Ok(())
    }

    fn retry_or_fail(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        reason: String,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part) else {
            return Ok(());
        };
        if brief.receipt.retries < self.config.max_retries {
            self.commit(
                out,
                vec![Event::BriefRetried {
                    round,
                    to: part.clone(),
                    reason,
                }],
            )?;
            if let Some(brief) = self.open_brief(round, part) {
                Self::send(out, round, brief);
            }
        } else {
            self.commit(
                out,
                vec![Event::BriefFailed {
                    round,
                    to: part.clone(),
                    reason,
                }],
            )?;
        }
        Ok(())
    }

    fn timeouts(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        let Some(round) = self.session.current_round() else {
            return Ok(());
        };
        let no = round.no;
        let mut due = Vec::new();
        for brief in round.briefs.iter().chain(&round.lead_brief) {
            if self.open_brief(no, &brief.to).is_none() {
                continue;
            }
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
                    brief.to.clone(),
                    format!("did not start within {}s", self.config.start_timeout / 1000),
                ));
            } else if brief.receipt.started.is_some() && waited >= self.config.reply_timeout {
                due.push((
                    brief.to.clone(),
                    format!("no reply within {}s", self.config.reply_timeout / 1000),
                ));
            }
        }
        for (part, reason) in due {
            self.retry_or_fail(out, no, &part, reason)?;
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
        let Some(no) = self.current_no() else {
            return Ok(());
        };
        let Some(brief) = self.open_brief(no, part) else {
            return Ok(());
        };
        if brief.receipt.sent.is_none() {
            return Ok(());
        }
        match state {
            AgentState::Running if brief.receipt.started.is_none() => {
                self.commit(
                    out,
                    vec![Event::BriefStarted {
                        round: no,
                        to: part.clone(),
                        at,
                    }],
                )?;
            }
            AgentState::Error => {
                self.retry_or_fail(out, no, part, "agent reported an error".to_owned())?;
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
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part).cloned() else {
            return Ok(());
        };
        self.mark_started(out, round, part, &brief, at)?;
        let digest = store::digest(content);
        if self.is_lead(part) {
            match validate::parse_lead(content, &self.session, round) {
                Ok(output) => {
                    let ends = output.verdict.is_some();
                    let mut batch = vec![
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
                        Event::LeadAccepted { round, output },
                        Event::PhaseChanged {
                            round,
                            phase: Phase::Closed,
                        },
                    ];
                    if ends {
                        batch.push(Event::SessionClosed);
                    }
                    self.commit(out, batch)?;
                    out.effects.push(Effect::WriteReport);
                    if !self.session.closed {
                        self.next_round(out, at)?;
                    }
                }
                Err(error) => self.reject_reply(out, round, &brief, digest, error.to_string())?,
            }
        } else {
            match validate::parse_review(content, &self.session, part, round) {
                Ok(review) => {
                    self.commit(
                        out,
                        vec![
                            Event::ReplyAccepted {
                                round,
                                to: part.clone(),
                                at,
                            },
                            Event::ReviewRecorded {
                                round,
                                part: part.clone(),
                                summary: review.summary,
                                findings: review.findings,
                                stances: review.stances,
                                coverage: review.coverage,
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
        part: &PartId,
        brief: &Brief,
        at: Millis,
    ) -> Result<(), EngineError> {
        if brief.receipt.started.is_none() {
            self.commit(
                out,
                vec![Event::BriefStarted {
                    round,
                    to: part.clone(),
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
            if let Some(updated) = self.open_brief(round, &brief.to).cloned() {
                Self::watch(out, round, &updated);
                Self::send(out, round, &updated);
            }
        } else {
            self.commit(
                out,
                vec![Event::BriefFailed {
                    round,
                    to: brief.to.clone(),
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
        let Some(round) = self.session.current_round().cloned() else {
            return rejected("開始されたラウンドがありません");
        };
        let retryable = round.retryable();
        if retryable.is_empty() {
            return rejected("再試行できる失敗ブリーフがありません");
        }
        let batch: Vec<Event> = retryable
            .iter()
            .map(|part| Event::BriefReset {
                round: round.no,
                to: part.clone(),
            })
            .collect();
        self.commit(out, batch)?;
        for part in retryable {
            if let Some(brief) = self.open_brief(round.no, &part).cloned() {
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
