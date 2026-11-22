//! The round state machine. Pure: it takes inputs (what the live layer saw
//! happen, with the time it happened), commits event batches to its session
//! and returns them with the effects to carry out. It never does I/O and
//! never reads a clock.
//!
//! A round runs Briefing (every panelist's brief is built and sent) →
//! Awaiting (every brief has started, or failed and shows it; then every
//! reply is in, or failed) → Summarizing (the moderator's JSON, checked and
//! sent back with its problems until it passes) → Closed, then the next
//! round. A debate the moderator proposes waits for the user's approval.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::brief;
use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{
    AgentKind, Author, Brief, DebateId, DebateStatus, IssueId, IssueOp, MODERATOR_ID, Message,
    Millis, MsgKind, PartId, Participant, Phase, Position, Role, RoundKind, RoundNo, Session,
    SessionId,
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
    /// Exchanges a debate runs at most before the moderator must rule.
    pub debate_max_exchanges: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start_timeout: 90_000,
            reply_timeout: 15 * 60_000,
            max_retries: 2,
            debate_max_exchanges: 3,
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
    /// Begin the discussion, or pick it up again after a restart.
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
    /// The user speaks into the meeting; `to` empty means everyone.
    UserIntervention {
        to: Vec<PartId>,
        body: String,
        at: Millis,
    },
    /// The user approves the proposed debate, optionally with new sides.
    UserApproveDebate {
        id: DebateId,
        pro: Option<Vec<PartId>>,
        con: Option<Vec<PartId>>,
        at: Millis,
    },
    UserDeclineDebate {
        id: DebateId,
        at: Millis,
    },
    /// The user starts a debate on an issue (⚔); sides default to the
    /// panelists' latest stances.
    UserStartDebate {
        issue: IssueId,
        pro: Option<Vec<PartId>>,
        con: Option<Vec<PartId>>,
        at: Millis,
    },
    /// Try the current round's failed briefs again.
    UserRetry,
    /// The user ends the discussion: nothing more is sent, and no round
    /// starts.
    UserClose,
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
}

/// What one input produced: batches already applied to the engine's
/// session, to be appended to the store in order, then effects to carry out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub batches: Vec<Vec<Event>>,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The input is not allowed now; nothing changed.
    Rejected(String),
    /// A batch did not apply: a bug in the engine.
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

/// The participants a session with this moderator and panel gets: the
/// moderator is `noa`; panelists are named after their kind, `-2`, `-3`, …
/// when a kind repeats.
pub fn participants(moderator: AgentKind, panel: &[AgentKind]) -> (Participant, Vec<Participant>) {
    let mut panelists: Vec<Participant> = Vec::with_capacity(panel.len());
    for &kind in panel {
        let seen = panelists.iter().filter(|p| p.kind == kind).count();
        let id = match seen {
            0 => kind.command().to_owned(),
            n => format!("{}-{}", kind.command(), n + 1),
        };
        panelists.push(Participant {
            id: PartId::new(id),
            kind,
            role: Role::Panelist,
            pane_id: None,
        });
    }
    let moderator = Participant {
        id: PartId::new(MODERATOR_ID),
        kind: moderator,
        role: Role::Moderator,
        pane_id: None,
    };
    (moderator, panelists)
}

/// What a new session is about and who takes part.
#[derive(Debug, Clone)]
pub struct Setup<'a> {
    pub topic: &'a str,
    /// The most rounds the discussion runs; `None` for no limit.
    pub max_rounds: Option<RoundNo>,
    /// The directory (or repository) the agents run in. Their reply files
    /// go under it ([`store::workdir_reply_dir`]), so an agent whose sandbox
    /// writes only inside its working directory can write them.
    pub workdir: Option<PathBuf>,
    pub moderator: AgentKind,
    pub panel: &'a [AgentKind],
}

#[derive(Debug, Clone)]
pub struct Engine {
    session: Session,
    dir: PathBuf,
    config: Config,
    /// Agents waiting on the user (an approval) since their last report,
    /// and when each last stopped waiting: their brief's deadlines do not
    /// run while they wait. Not stored; a restart starts without it.
    waiting: BTreeMap<PartId, Option<Millis>>,
}

impl Engine {
    /// A new discussion stored at `dir`. The output holds the `Created`
    /// batch; send `Start` once the agents' panes are up.
    pub fn create(
        id: SessionId,
        topic: &str,
        moderator: AgentKind,
        panel: &[AgentKind],
        dir: &Path,
        config: Config,
        now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        let setup = Setup {
            topic,
            workdir: None,
            max_rounds: None,
            moderator,
            panel,
        };
        Engine::create_in(id, setup, dir, config, now)
    }

    /// [`Engine::create`] from a [`Setup`], which can name the directory
    /// the agents run in.
    pub fn create_in(
        id: SessionId,
        setup: Setup<'_>,
        dir: &Path,
        config: Config,
        now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        let Setup {
            topic,
            workdir,
            max_rounds,
            moderator,
            panel,
        } = setup;
        let topic = topic.trim();
        if topic.is_empty() {
            return rejected("the topic is empty");
        }
        if panel.is_empty() {
            return rejected("the panel is empty");
        }
        let (moderator, panelists) = participants(moderator, panel);
        let batch = vec![Event::Created {
            id,
            topic: topic.to_owned(),
            workdir,
            max_rounds,
            moderator,
            panelists,
            at: now,
        }];
        let session = reduce::create(&batch)?;
        let engine = Engine {
            session,
            dir: dir.to_owned(),
            config,
            waiting: BTreeMap::new(),
        };
        let output = Output {
            batches: vec![batch],
            effects: Vec::new(),
        };
        Ok((engine, output))
    }

    /// Where the agents' reply files go: under the session's working
    /// directory when it has one, else the session directory.
    pub fn reply_dir(&self) -> PathBuf {
        match &self.session.workdir {
            Some(workdir) => store::workdir_reply_dir(workdir, &self.session.id),
            None => self.dir.clone(),
        }
    }

    /// An engine over a session folded from its log; send `Start` to pick
    /// it up again.
    pub fn resume(session: Session, dir: &Path, config: Config) -> Engine {
        Engine {
            session,
            dir: dir.to_owned(),
            config,
            waiting: BTreeMap::new(),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn handle(&mut self, input: Input) -> Result<Output, EngineError> {
        let mut out = Output::default();
        if self.session.closed {
            return match input {
                Input::UserIntervention { .. }
                | Input::UserApproveDebate { .. }
                | Input::UserDeclineDebate { .. }
                | Input::UserStartDebate { .. }
                | Input::UserRetry
                | Input::UserClose => rejected("the discussion has ended"),
                // Reports and ticks after the end move nothing.
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
                // A correction still waiting to go out would reach an agent
                // that never saw the brief: send the whole brief instead.
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
                // An agent that took a brief is at its input box, not on a
                // prompt: the deadlines run from this send.
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
            Input::UserIntervention { to, body, at } => {
                self.intervention(&mut out, to, body, at)?
            }
            Input::UserApproveDebate { id, pro, con, at } => {
                let Some(debate) = self.session.debate(&id) else {
                    return rejected(format!("no debate {id}"));
                };
                if debate.status != DebateStatus::Proposed {
                    return rejected(format!("debate {id} is not waiting for approval"));
                }
                let pro = pro.unwrap_or_else(|| debate.pro.clone());
                let con = con.unwrap_or_else(|| debate.con.clone());
                self.check_sides(&pro, &con)?;
                self.commit(&mut out, vec![Event::DebateApproved { id, pro, con }])?;
                self.next_round(&mut out, at)?;
            }
            Input::UserDeclineDebate { id, at } => {
                match self.session.debate(&id) {
                    Some(debate) if debate.status == DebateStatus::Proposed => {}
                    Some(_) => return rejected(format!("debate {id} is not waiting for approval")),
                    None => return rejected(format!("no debate {id}")),
                }
                self.commit(&mut out, vec![Event::DebateDeclined { id }])?;
                self.next_round(&mut out, at)?;
            }
            Input::UserStartDebate {
                issue,
                pro,
                con,
                at,
            } => self.user_debate(&mut out, issue, pro, con, at)?,
            Input::UserRetry => self.user_retry(&mut out)?,
            Input::UserClose => self.commit(&mut out, vec![Event::SessionClosed])?,
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

    fn is_moderator(&self, part: &PartId) -> bool {
        *part == self.session.moderator.id
    }

    fn current_no(&self) -> Option<RoundNo> {
        self.session.current_round().map(|r| r.no)
    }

    /// The brief to `part` in `round` while that is the current round and
    /// the brief is still in play (not replied, not failed).
    fn open_brief(&self, round: RoundNo, part: &PartId) -> Option<&Brief> {
        let current = self.session.current_round()?;
        if current.no != round {
            return None;
        }
        let in_play = if self.is_moderator(part) {
            current.phase == Phase::Summarizing
        } else {
            matches!(current.phase, Phase::Briefing | Phase::Awaiting)
        };
        current
            .brief(part)
            .filter(|b| in_play && b.receipt.replied.is_none() && b.receipt.failed.is_none())
    }

    /// What to send for `brief` now: the correction request when its reply
    /// was turned down, else the brief itself.
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
        if self.session.closed {
            return Ok(());
        }
        let Some(round) = self.session.current_round().cloned() else {
            return self.next_round(out, now);
        };
        // Pick up a round where a restart left it.
        for brief in &round.briefs {
            if brief.receipt.replied.is_some() {
                continue;
            }
            if round.phase != Phase::Closed {
                Self::watch(out, round.no, brief);
            }
            if matches!(round.phase, Phase::Briefing | Phase::Awaiting)
                && brief.receipt.failed.is_none()
                && brief.receipt.sent.is_none()
            {
                Self::send(out, round.no, brief);
            }
        }
        match (round.phase, &round.moderator_brief) {
            (Phase::Summarizing, Some(brief)) if brief.receipt.failed.is_none() => {
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

    /// Starts the round that follows a closed one (or the first), unless the
    /// session is over or a proposed debate waits for the user.
    fn next_round(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        if self.session.closed {
            return Ok(());
        }
        if let Some(round) = self.session.current_round()
            && round.phase != Phase::Closed
        {
            return Ok(());
        }
        // The last round is over: the discussion ends here, even with a
        // debate proposed in it (there is no round left to hold it in).
        if self
            .session
            .max_rounds
            .is_some_and(|max| self.session.rounds.len() as RoundNo >= max)
        {
            return self.commit(out, vec![Event::SessionClosed]);
        }
        if self.session.pending_debate().is_some() {
            return Ok(());
        }
        let debate = |status| {
            self.session
                .debates
                .iter()
                .find(|d| d.status == status)
                .map(|d| d.id.clone())
        };
        let kind = match debate(DebateStatus::Active).or_else(|| debate(DebateStatus::Approved)) {
            Some(id) => RoundKind::Debate(id),
            None => RoundKind::Open,
        };
        self.start_round(out, kind, now)
    }

    fn start_round(
        &mut self,
        out: &mut Output,
        kind: RoundKind,
        now: Millis,
    ) -> Result<(), EngineError> {
        let no = self.session.rounds.len() as RoundNo + 1;
        let asked: BTreeMap<PartId, String> = self
            .session
            .current_round()
            .and_then(|r| r.outcome.as_ref())
            .map(|o| o.questions.clone())
            .unwrap_or_default();
        let debate = match &kind {
            RoundKind::Debate(id) => Some(id.clone()),
            RoundKind::Open => None,
        };
        let mut questions = Vec::new();
        let mut batch = vec![Event::RoundStarted {
            no,
            kind: kind.clone(),
            at: now,
        }];
        for (offset, part) in self.session.panelist_ids().enumerate() {
            let question = asked
                .get(part)
                .cloned()
                .unwrap_or_else(|| brief::default_question(&self.session, &kind, part).to_owned());
            batch.push(Event::MessagePosted {
                message: Message {
                    id: self.session.next_message_id(offset),
                    round: no,
                    author: Author::Part(self.session.moderator.id.clone()),
                    kind: MsgKind::Brief,
                    issues: Vec::new(),
                    reply_to: Vec::new(),
                    to: vec![part.clone()],
                    debate: debate.clone(),
                    body: question.clone(),
                    at: now,
                },
            });
            questions.push((part.clone(), question));
        }
        self.commit(out, batch)?;

        let briefs: Vec<Brief> = questions
            .iter()
            .map(|(part, question)| {
                let path = store::reply_path(&self.reply_dir(), no, part);
                brief::panelist_brief(&self.session, part, no, question, &path)
            })
            .collect();
        let batch = briefs
            .iter()
            .map(|brief| Event::BriefIssued {
                round: no,
                brief: brief.clone(),
            })
            .collect();
        self.commit(out, batch)?;
        for brief in &briefs {
            Self::watch(out, no, brief);
            Self::send(out, no, brief);
        }
        Ok(())
    }

    /// Moves the current round on as far as its receipts allow.
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
                        phase: Phase::Awaiting,
                    }],
                )?;
                self.advance(out)
            }
            Phase::Awaiting
                if settled(|b| b.receipt.started.is_some())
                    && settled(|b| b.receipt.replied.is_some()) =>
            {
                self.summarize(out, no)
            }
            _ => Ok(()),
        }
    }

    fn summarize(&mut self, out: &mut Output, no: RoundNo) -> Result<(), EngineError> {
        let path = store::moderator_reply_path(&self.reply_dir(), no);
        let brief = brief::moderator_brief(&self.session, no, &path);
        self.commit(
            out,
            vec![
                Event::PhaseChanged {
                    round: no,
                    phase: Phase::Summarizing,
                },
                Event::BriefIssued {
                    round: no,
                    brief: brief.clone(),
                },
            ],
        )?;
        Self::watch(out, no, &brief);
        Self::send(out, no, &brief);
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
            let brief = self.open_brief(round, part).cloned();
            if let Some(brief) = brief {
                Self::send(out, round, &brief);
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
        for brief in round.briefs.iter().chain(&round.moderator_brief) {
            if self.open_brief(no, &brief.to).is_none() {
                continue;
            }
            let Some(sent) = brief.receipt.sent else {
                continue;
            };
            // The deadlines stop while the agent waits on the user, and
            // run again from when it stopped waiting.
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
        // Every report moves the wait, whatever the brief: a wait that
        // ended after the reply was taken in must not outlive it.
        // `None`: waiting now; `Some(at)`: stopped waiting at `at`.
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
        let not_started = brief.receipt.started.is_none();
        // Only a state seen after the send counts.
        if brief.receipt.sent.is_none() {
            return Ok(());
        }
        match state {
            AgentState::Running | AgentState::Permission if not_started => self.commit(
                out,
                vec![Event::BriefStarted {
                    round: no,
                    to: part.clone(),
                    at,
                }],
            ),
            AgentState::Error => {
                self.retry_or_fail(out, no, part, "the agent reported an error".to_owned())
            }
            _ => Ok(()),
        }
    }

    fn reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        content: &str,
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(this) = self.session.round(round) else {
            return Ok(());
        };
        let Some(brief) = this.brief(part) else {
            return Ok(());
        };
        if brief.receipt.replied.is_some() {
            return Ok(());
        }
        let in_play = self.open_brief(round, part).is_some();
        if self.is_moderator(part) {
            if !in_play {
                return Ok(());
            }
            return match validate::parse_moderator(content, &self.session, round) {
                Ok(output) => self.apply_moderator(out, round, output, at),
                Err(error) => self.reject(out, round, part, error.to_string()),
            };
        }
        let reply = match validate::parse_reply(content, &self.session) {
            Ok(reply) => reply,
            // A late reply that does not parse is left alone.
            Err(_) if !in_play => return Ok(()),
            Err(error) => return self.reject(out, round, part, error.to_string()),
        };
        let debate = this.debate().cloned();
        let side = debate
            .as_ref()
            .and_then(|id| self.session.debate(id))
            .and_then(|d| d.side(part));
        let id = self.session.next_message_id(0);
        let mut batch = vec![Event::MessagePosted {
            message: Message {
                id: id.clone(),
                round,
                author: Author::Part(part.clone()),
                kind: if side.is_some() {
                    MsgKind::Argument
                } else {
                    MsgKind::Reply
                },
                issues: reply.issues,
                reply_to: reply.reply_to,
                to: Vec::new(),
                debate,
                body: reply.body,
                at,
            },
        }];
        for (issue, pos) in reply.stances {
            batch.push(Event::IssueChanged {
                round,
                op: IssueOp::SetStance {
                    id: issue,
                    part: part.clone(),
                    pos,
                    basis: id.clone(),
                },
            });
        }
        batch.push(Event::ReplyAccepted {
            round,
            to: part.clone(),
            at,
        });
        self.commit(out, batch)
    }

    /// Sends a rejected reply back with its problems, or gives up on it.
    fn reject(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        part: &PartId,
        error: String,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part) else {
            return Ok(());
        };
        let path = brief.reply_path.clone();
        if brief.receipt.retries >= self.config.max_retries {
            return self.commit(
                out,
                vec![Event::BriefFailed {
                    round,
                    to: part.clone(),
                    reason: format!("reply rejected:\n{error}"),
                }],
            );
        }
        self.commit(
            out,
            vec![Event::ReplyRejected {
                round,
                to: part.clone(),
                error,
            }],
        )?;
        out.effects.push(Effect::ClearReply { path });
        if let Some(brief) = self.open_brief(round, part).cloned() {
            Self::watch(out, round, &brief);
            Self::send(out, round, &brief);
        }
        Ok(())
    }

    fn apply_moderator(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        output: crate::model::types::ModeratorOutput,
        at: Millis,
    ) -> Result<(), EngineError> {
        let moderator = self.session.moderator.id.clone();
        let debate = self.session.round(round).and_then(|r| r.debate()).cloned();
        let mut touched: Vec<IssueId> = Vec::new();
        for op in &output.issue_ops {
            let (IssueOp::Add { id, .. }
            | IssueOp::SetState { id, .. }
            | IssueOp::SetStance { id, .. }) = op;
            if !touched.contains(id) {
                touched.push(id.clone());
            }
        }
        let mut posted = 0;
        let mut message = |kind, issues, body, session: &Session| {
            let message = Message {
                id: session.next_message_id(posted),
                round,
                author: Author::Part(moderator.clone()),
                kind,
                issues,
                reply_to: Vec::new(),
                to: Vec::new(),
                debate: debate.clone(),
                body,
                at,
            };
            posted += 1;
            Event::MessagePosted { message }
        };
        let mut batch = vec![
            Event::ReplyAccepted {
                round,
                to: moderator.clone(),
                at,
            },
            Event::ModeratorAccepted {
                round,
                output: output.clone(),
            },
            message(
                MsgKind::Summary,
                touched,
                output.summary.clone(),
                &self.session,
            ),
        ];
        if let (Some(verdict), Some(id)) = (&output.verdict, &debate) {
            let issue = self.session.debate(id).map(|d| d.issue.clone());
            batch.push(message(
                MsgKind::Verdict,
                issue.into_iter().collect(),
                format!("{} — {}", verdict.outcome.name(), verdict.rationale),
                &self.session,
            ));
            batch.push(Event::DebateVerdict {
                round,
                id: id.clone(),
                verdict: verdict.clone(),
            });
        }
        if let Some(proposal) = &output.propose_debate {
            batch.push(Event::DebateProposed {
                issue: proposal.issue.clone(),
                pro: proposal.pro.clone(),
                con: proposal.con.clone(),
                max_exchanges: self.config.debate_max_exchanges,
                status: DebateStatus::Proposed,
            });
        }
        batch.push(Event::PhaseChanged {
            round,
            phase: Phase::Closed,
        });
        if output.close {
            batch.push(Event::SessionClosed);
        }
        self.commit(out, batch)?;
        self.next_round(out, at)
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
            return rejected("the intervention is empty");
        }
        if let Some(unknown) = to.iter().find(|p| self.session.participant(p).is_none()) {
            return rejected(format!("no participant {unknown}"));
        }
        let message = Message {
            id: self.session.next_message_id(0),
            round: self.current_no().unwrap_or(1),
            author: Author::User,
            kind: MsgKind::Intervention,
            issues: Vec::new(),
            reply_to: Vec::new(),
            to,
            debate: None,
            body,
            at,
        };
        self.commit(out, vec![Event::MessagePosted { message }])
    }

    fn check_sides(&self, pro: &[PartId], con: &[PartId]) -> Result<(), EngineError> {
        if pro.is_empty() || con.is_empty() {
            return rejected("both sides of a debate need a panelist");
        }
        for part in pro.iter().chain(con) {
            if !self.session.is_panelist(part) {
                return rejected(format!("{part} is not a panelist"));
            }
        }
        if let Some(both) = pro.iter().find(|p| con.contains(p)) {
            return rejected(format!("{both} is on both sides"));
        }
        Ok(())
    }

    fn user_debate(
        &mut self,
        out: &mut Output,
        issue: IssueId,
        pro: Option<Vec<PartId>>,
        con: Option<Vec<PartId>>,
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(found) = self.session.issues.get(&issue) else {
            return rejected(format!("no issue {issue}"));
        };
        if self.session.debates.iter().any(|d| {
            matches!(
                d.status,
                DebateStatus::Proposed | DebateStatus::Approved | DebateStatus::Active
            )
        }) {
            return rejected("another debate is already proposed or running");
        }
        let by = |pos| {
            self.session
                .panelist_ids()
                .filter(|p| found.stances.get(*p).is_some_and(|s| s.pos == pos))
                .cloned()
                .collect::<Vec<_>>()
        };
        let pro = pro.unwrap_or_else(|| by(Position::For));
        let con = con.unwrap_or_else(|| by(Position::Against));
        self.check_sides(&pro, &con)?;
        self.commit(
            out,
            vec![Event::DebateProposed {
                issue,
                pro,
                con,
                max_exchanges: self.config.debate_max_exchanges,
                status: DebateStatus::Approved,
            }],
        )?;
        self.next_round(out, at)
    }

    fn user_retry(&mut self, out: &mut Output) -> Result<(), EngineError> {
        let Some(round) = self.session.current_round() else {
            return rejected("nothing to retry");
        };
        let no = round.no;
        let failed = round.retryable();
        if failed.is_empty() {
            return rejected("nothing to retry");
        }
        let batch = failed
            .iter()
            .map(|part| Event::BriefReset {
                round: no,
                to: part.clone(),
            })
            .collect();
        self.commit(out, batch)?;
        for part in &failed {
            if let Some(brief) = self.open_brief(no, part).cloned() {
                // A brief that failed on rejected replies leaves the last
                // one on disk; it must not count as the retry's reply.
                out.effects.push(Effect::ClearReply {
                    path: brief.reply_path.clone(),
                });
                Self::watch(out, no, &brief);
                Self::send(out, no, &brief);
            }
        }
        self.advance(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::brief::{MODERATOR_SECTIONS, PANELIST_SECTIONS, render_message};
    use crate::model::types::{IssueState, MessageId};

    #[test]
    fn a_session_with_a_workdir_sends_replies_under_its_dot_noa() {
        let setup = Setup {
            topic: "Pick a database",
            workdir: Some(PathBuf::from("/repo")),
            max_rounds: None,
            moderator: AgentKind::Codex,
            panel: &[AgentKind::Claude, AgentKind::Agy],
        };
        let (mut engine, _) =
            Engine::create_in(SessionId::new("disc-1"), setup, Path::new("/d"), CONFIG, 0).unwrap();
        assert_eq!(
            engine.session().workdir.as_deref(),
            Some(Path::new("/repo"))
        );
        assert_eq!(
            engine.reply_dir(),
            Path::new("/repo/.noa/discussions/disc-1")
        );
        engine.handle(Input::Start { now: 1 }).unwrap();
        for brief in &round(&engine).briefs {
            assert!(
                brief
                    .reply_path
                    .starts_with("/repo/.noa/discussions/disc-1/r1"),
                "{}",
                brief.reply_path.display()
            );
            assert!(brief.body.contains("/repo/.noa/discussions/disc-1/r1/"));
        }
        // Without a workdir the replies stay in the session directory.
        assert_eq!(self::engine().reply_dir(), Path::new("/d"));
    }

    const CONFIG: Config = Config {
        start_timeout: 1_000,
        reply_timeout: 10_000,
        max_retries: 2,
        debate_max_exchanges: 2,
    };

    fn p(id: &str) -> PartId {
        PartId::new(id)
    }

    fn engine() -> Engine {
        let (engine, out) = Engine::create(
            SessionId::new("disc-1"),
            "Pick a database",
            AgentKind::Codex,
            &[AgentKind::Claude, AgentKind::Codex, AgentKind::Agy],
            Path::new("/d"),
            CONFIG,
            0,
        )
        .unwrap();
        assert_eq!(out.batches.len(), 1);
        engine
    }

    fn sends(out: &Output) -> Vec<(PartId, String)> {
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::SendBrief { part, body, .. } => Some((part.clone(), body.clone())),
                _ => None,
            })
            .collect()
    }

    fn send_to(out: &Output, part: &str) -> String {
        sends(out)
            .into_iter()
            .find(|(p, _)| p.as_str() == part)
            .unwrap_or_else(|| panic!("no send to {part} in {out:#?}"))
            .1
    }

    fn panel(engine: &Engine) -> Vec<PartId> {
        engine.session().panelist_ids().cloned().collect()
    }

    fn round(engine: &Engine) -> &crate::model::types::Round {
        engine.session().current_round().unwrap()
    }

    fn phase(engine: &Engine) -> Phase {
        round(engine).phase
    }

    fn no(engine: &Engine) -> RoundNo {
        round(engine).no
    }

    fn feed(engine: &mut Engine, input: Input) -> Output {
        engine.handle(input).unwrap()
    }

    fn sent(engine: &mut Engine, part: &PartId, at: Millis) -> Output {
        let round = no(engine);
        feed(
            engine,
            Input::Sent {
                round,
                part: part.clone(),
                at,
            },
        )
    }

    fn running(engine: &mut Engine, part: &PartId, at: Millis) -> Output {
        feed(
            engine,
            Input::AgentStateChanged {
                part: part.clone(),
                state: AgentState::Running,
                at,
            },
        )
    }

    fn write(engine: &mut Engine, part: &PartId, content: &str, at: Millis) -> Output {
        let round = no(engine);
        feed(
            engine,
            Input::ReplyFileWritten {
                round,
                part: part.clone(),
                content: content.to_owned(),
                at,
            },
        )
    }

    /// Delivers the current round's briefs and takes in one reply each.
    fn run_panel(engine: &mut Engine, at: Millis) -> Output {
        let mut last = Output::default();
        for part in panel(engine) {
            sent(engine, &part, at);
            running(engine, &part, at);
        }
        for part in panel(engine) {
            last = write(
                engine,
                &part,
                &format!("{part} thinks so, round {}", no(engine)),
                at,
            );
        }
        last
    }

    fn moderate(engine: &mut Engine, json: &str, at: Millis) -> Output {
        let moderator = p(MODERATOR_ID);
        sent(engine, &moderator, at);
        running(engine, &moderator, at);
        write(engine, &moderator, json, at)
    }

    #[test]
    fn participants_are_named_after_their_kind() {
        let (moderator, panel) = participants(
            AgentKind::Agy,
            &[
                AgentKind::Claude,
                AgentKind::Claude,
                AgentKind::Codex,
                AgentKind::Claude,
            ],
        );
        assert_eq!(moderator.id, p("noa"));
        assert_eq!(moderator.kind, AgentKind::Agy);
        let ids: Vec<&str> = panel.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude", "claude-2", "codex", "claude-3"]);
        assert!(
            Engine::create(
                SessionId::new("x"),
                " ",
                AgentKind::Claude,
                &[AgentKind::Claude],
                Path::new("/d"),
                CONFIG,
                0
            )
            .is_err()
        );
        assert!(
            Engine::create(
                SessionId::new("x"),
                "t",
                AgentKind::Claude,
                &[],
                Path::new("/d"),
                CONFIG,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn a_round_runs_from_briefing_to_the_next_round() {
        let mut e = engine();
        let out = feed(&mut e, Input::Start { now: 10 });
        assert_eq!(phase(&e), Phase::Briefing);
        assert_eq!(sends(&out).len(), 3);
        assert!(out.effects.contains(&Effect::WatchReply {
            round: 1,
            part: p("codex"),
            path: PathBuf::from("/d/r1/codex.md"),
        }));
        for (part, body) in sends(&out) {
            let brief = round(&e).brief(&part).unwrap();
            assert_eq!(body, brief.body, "the exact text sent is stored");
        }

        for part in panel(&e) {
            sent(&mut e, &part, 20);
        }
        assert_eq!(phase(&e), Phase::Awaiting);
        for part in panel(&e) {
            running(&mut e, &part, 30);
        }
        write(&mut e, &p("claude"), "first", 40);
        write(&mut e, &p("codex"), "second", 40);
        assert_eq!(phase(&e), Phase::Awaiting);
        let out = write(&mut e, &p("agy"), "third", 40);
        assert_eq!(phase(&e), Phase::Summarizing);
        let moderator_text = send_to(&out, "noa");
        assert!(moderator_text.contains("/d/r1/moderator.json"));
        for message in e
            .session()
            .messages
            .iter()
            .filter(|m| m.kind == MsgKind::Reply)
        {
            assert!(moderator_text.contains(&render_message(e.session(), message)));
        }

        let rev = e.session().rev;
        let out = moderate(
            &mut e,
            r#"{"summary":"All agree on A.","issue_ops":[{"op":"add","id":"A","title":"Use Postgres"}],
                "questions":{"codex":"Any risk?"}}"#,
            50,
        );
        assert!(e.session().rev > rev);
        assert_eq!(no(&e), 2);
        assert_eq!(phase(&e), Phase::Briefing);
        assert_eq!(e.session().round(1).unwrap().phase, Phase::Closed);
        assert!(e.session().issues.contains(&IssueId::new("A")));
        let summary = e
            .session()
            .messages
            .iter()
            .find(|m| m.kind == MsgKind::Summary)
            .unwrap();
        assert_eq!(summary.body, "All agree on A.");
        assert_eq!(summary.issues, vec![IssueId::new("A")]);
        assert!(send_to(&out, "codex").contains("Any risk?"));
        assert!(send_to(&out, "claude").contains(brief::DEFAULT_QUESTION));
        for (_, body) in sends(&out) {
            assert!(body.contains("All agree on A."));
            assert!(body.contains("added: [A] Use Postgres"));
        }
    }

    #[test]
    fn reply_headers_become_tags_and_stances() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(
            &mut e,
            r#"{"summary":"s","issue_ops":[{"op":"add","id":"B","title":"b"}]}"#,
            2,
        );
        for part in panel(&e) {
            sent(&mut e, &part, 3);
            running(&mut e, &part, 3);
        }
        write(
            &mut e,
            &p("codex"),
            "---\nissues: [B]\nreply_to: [m4]\nstance: {B: against}\n---\nNo.",
            4,
        );
        let reply = e.session().messages.last().unwrap();
        assert_eq!(reply.kind, MsgKind::Reply);
        assert_eq!(reply.issues, vec![IssueId::new("B")]);
        assert_eq!(reply.reply_to, vec![MessageId::nth(4)]);
        let stance = &e.session().issues.get(&IssueId::new("B")).unwrap().stances[&p("codex")];
        assert_eq!((stance.pos, &stance.basis), (Position::Against, &reply.id));
    }

    #[test]
    fn the_round_waits_for_every_brief_to_start_and_resends_until_failed() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 100);
        }
        running(&mut e, &p("claude"), 200);
        running(&mut e, &p("codex"), 200);
        write(&mut e, &p("claude"), "a", 300);
        write(&mut e, &p("codex"), "b", 300);
        assert_eq!(phase(&e), Phase::Awaiting, "agy has not started");

        // Not due yet.
        assert!(feed(&mut e, Input::Tick { now: 1_000 }).effects.is_empty());
        for (attempt, at) in [(1, 1_100), (2, 2_200)] {
            let out = feed(&mut e, Input::Tick { now: at });
            assert_eq!(sends(&out).len(), 1, "resend {attempt}");
            let receipt = &round(&e).brief(&p("agy")).unwrap().receipt;
            assert_eq!(
                (receipt.retries, receipt.sent, receipt.started),
                (attempt, None, None)
            );
            assert_eq!(
                send_to(&out, "agy"),
                round(&e).brief(&p("agy")).unwrap().body
            );
            sent(&mut e, &p("agy"), at);
        }
        assert_eq!(phase(&e), Phase::Awaiting);
        let out = feed(&mut e, Input::Tick { now: 3_300 });
        let receipt = &e
            .session()
            .round(1)
            .unwrap()
            .brief(&p("agy"))
            .unwrap()
            .receipt;
        assert_eq!(receipt.failed.as_deref(), Some("did not start within 1s"));
        assert_eq!(phase(&e), Phase::Summarizing);
        assert!(send_to(&out, "noa").contains("- agy: no reply: did not start within 1s"));
    }

    #[test]
    fn state_before_the_send_does_not_count_as_started() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        running(&mut e, &p("claude"), 1);
        assert_eq!(round(&e).brief(&p("claude")).unwrap().receipt.started, None);
        sent(&mut e, &p("claude"), 2);
        feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("claude"),
                state: AgentState::Input,
                at: 3,
            },
        );
        assert_eq!(round(&e).brief(&p("claude")).unwrap().receipt.started, None);
        feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("claude"),
                state: AgentState::Permission,
                at: 4,
            },
        );
        assert_eq!(
            round(&e).brief(&p("claude")).unwrap().receipt.started,
            Some(4)
        );
    }

    #[test]
    fn a_failed_send_is_retried_then_marked_failed() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        let fail = |e: &mut Engine| {
            feed(
                e,
                Input::SendFailed {
                    round: 1,
                    part: p("agy"),
                    error: "pane gone".into(),
                },
            )
        };
        assert_eq!(sends(&fail(&mut e)).len(), 1);
        assert_eq!(sends(&fail(&mut e)).len(), 1);
        assert!(sends(&fail(&mut e)).is_empty());
        let receipt = &round(&e).brief(&p("agy")).unwrap().receipt;
        assert_eq!(receipt.failed.as_deref(), Some("send failed: pane gone"));
        assert_eq!(receipt.retries, 2);

        // The user retries it from the start.
        let out = feed(&mut e, Input::UserRetry);
        assert_eq!(sends(&out).len(), 1);
        let receipt = &round(&e).brief(&p("agy")).unwrap().receipt;
        assert_eq!((receipt.failed.clone(), receipt.retries), (None, 0));
        assert!(matches!(
            e.handle(Input::UserRetry),
            Err(EngineError::Rejected(_))
        ));
    }

    #[test]
    fn a_started_brief_without_a_reply_is_resent_at_the_deadline() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 10);
        }
        write(&mut e, &p("claude"), "a", 20);
        write(&mut e, &p("codex"), "b", 20);
        let out = feed(&mut e, Input::Tick { now: 10_000 });
        assert_eq!(sends(&out).len(), 1);
        assert!(round(&e).brief(&p("agy")).unwrap().receipt.retries == 1);
        // A reply arriving after the resend is taken in.
        sent(&mut e, &p("agy"), 10_001);
        write(&mut e, &p("agy"), "c", 10_500);
        assert_eq!(phase(&e), Phase::Summarizing);
    }

    #[test]
    fn invalid_moderator_json_goes_back_with_its_error() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        let out = moderate(
            &mut e,
            r#"{"summary":"s","issue_ops":[{"op":"set_state","id":"Z","state":"agreed"}]}"#,
            2,
        );
        assert_eq!(phase(&e), Phase::Summarizing);
        assert!(out.effects.contains(&Effect::ClearReply {
            path: PathBuf::from("/d/r1/moderator.json"),
        }));
        let note = send_to(&out, "noa");
        assert!(note.contains("unknown issue \"Z\""), "{note}");
        assert!(note.contains("/d/r1/moderator.json"));
        let receipt = &round(&e).moderator_brief.as_ref().unwrap().receipt;
        assert_eq!(receipt.retries, 1);
        assert!(receipt.rejected.is_some());

        let out = write(&mut e, &p("noa"), r#"{"summary":"fixed"}"#, 3);
        assert_eq!(no(&e), 2);
        assert_eq!(sends(&out).len(), 3);
    }

    #[test]
    fn a_moderator_that_keeps_failing_halts_the_round_until_retried() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(&mut e, "nope", 2);
        write(&mut e, &p("noa"), "nope", 3);
        let out = write(&mut e, &p("noa"), "nope", 4);
        assert!(sends(&out).is_empty());
        let receipt = &round(&e).moderator_brief.as_ref().unwrap().receipt;
        assert!(
            receipt
                .failed
                .as_deref()
                .unwrap()
                .starts_with("reply rejected:")
        );
        assert_eq!(phase(&e), Phase::Summarizing);
        // Ticks do nothing more.
        assert!(
            feed(&mut e, Input::Tick { now: 1_000_000 })
                .effects
                .is_empty()
        );
        let out = feed(&mut e, Input::UserRetry);
        assert_eq!(
            send_to(&out, "noa"),
            round(&e).moderator_brief.as_ref().unwrap().body
        );
        moderate(&mut e, r#"{"summary":"ok"}"#, 6);
        assert_eq!(no(&e), 2);
    }

    #[test]
    fn an_invalid_reply_header_is_sent_back_and_late_replies_are_kept() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 0);
        }
        let out = write(&mut e, &p("claude"), "---\nissues: [Q]\n---\nbody", 1);
        assert!(send_to(&out, "claude").contains("unknown issue \"Q\""));
        assert!(out.effects.contains(&Effect::ClearReply {
            path: PathBuf::from("/d/r1/claude.md"),
        }));
        write(&mut e, &p("claude"), "fixed", 2);
        write(&mut e, &p("codex"), "b", 2);
        // agy never replies: resent twice, then failed at the deadline.
        feed(&mut e, Input::Tick { now: 10_000 });
        sent(&mut e, &p("agy"), 10_000);
        running(&mut e, &p("agy"), 10_000);
        feed(&mut e, Input::Tick { now: 20_000 });
        sent(&mut e, &p("agy"), 20_000);
        running(&mut e, &p("agy"), 20_000);
        feed(&mut e, Input::Tick { now: 30_000 });
        assert_eq!(phase(&e), Phase::Summarizing);
        // agy's late reply still lands in the log for the next briefs.
        let count = e.session().messages.len();
        write(&mut e, &p("agy"), "late but here", 30_001);
        assert_eq!(e.session().messages.len(), count + 1);
        assert!(
            e.session()
                .round(1)
                .unwrap()
                .brief(&p("agy"))
                .unwrap()
                .receipt
                .replied
                .is_some()
        );
        // A late reply that does not parse is ignored.
        let out = e
            .handle(Input::ReplyFileWritten {
                round: 1,
                part: p("agy"),
                content: String::new(),
                at: 30_002,
            })
            .unwrap();
        assert!(out.batches.is_empty());
    }

    #[test]
    fn every_brief_carries_every_message_since_the_previous_one() {
        let mut e = engine();
        let mut previous: BTreeMap<PartId, Option<MessageId>> = BTreeMap::new();
        feed(&mut e, Input::Start { now: 0 });
        for r in 1..=4u64 {
            let current = no(&e);
            for brief in &round(&e).briefs {
                assert_eq!(&brief.since, previous.get(&brief.to).unwrap_or(&None));
                for heading in PANELIST_SECTIONS {
                    assert!(brief.body.contains(heading));
                }
                for message in e.session().messages_after(brief.since.as_ref()) {
                    if brief.through.as_ref().is_some_and(|t| {
                        e.session().message_index(&message.id) > e.session().message_index(t)
                    }) {
                        break;
                    }
                    assert!(brief.body.contains(&render_message(e.session(), message)));
                }
                previous.insert(brief.to.clone(), brief.through.clone());
            }
            feed(
                &mut e,
                Input::UserIntervention {
                    to: vec![],
                    body: format!("user note {r}"),
                    at: r * 100,
                },
            );
            run_panel(&mut e, r * 100 + 1);
            let moderator = round(&e).moderator_brief.clone().unwrap();
            for heading in MODERATOR_SECTIONS {
                assert!(moderator.body.contains(heading));
            }
            assert!(moderator.body.contains(&format!("user note {r}")));
            moderate(&mut e, r#"{"summary":"next"}"#, r * 100 + 2);
            assert_eq!(no(&e), current + 1);
        }
        // Each panelist's briefs chain without a gap: one's `through` is the
        // next one's `since`, and the first starts from the beginning.
        for part in panel(&e) {
            let briefs: Vec<&Brief> = e
                .session()
                .rounds
                .iter()
                .filter_map(|r| r.brief(&part))
                .collect();
            assert_eq!(briefs[0].since, None);
            for pair in briefs.windows(2) {
                assert_eq!(pair[1].since, pair[0].through);
            }
        }
        let last = round(&e).brief(&p("claude")).unwrap();
        assert!(last.body.contains("user note 4"));
        assert!(
            last.body.contains("- [m"),
            "interventions are listed in section 5"
        );
    }

    #[test]
    fn a_brief_that_was_never_sent_is_not_the_base_of_the_next_one() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(&mut e, r#"{"summary":"round one summary"}"#, 2);
        assert_eq!(no(&e), 2);
        let r1_through = e
            .session()
            .round(1)
            .unwrap()
            .brief(&p("agy"))
            .unwrap()
            .through
            .clone();
        // Round 2: agy's brief never goes out; the others reply.
        for _ in 0..3 {
            feed(
                &mut e,
                Input::SendFailed {
                    round: 2,
                    part: p("agy"),
                    error: "pane gone".into(),
                },
            );
        }
        let failed = round(&e).brief(&p("agy")).unwrap().receipt.clone();
        assert!(failed.failed.is_some() && failed.sent.is_none());
        for part in [p("claude"), p("codex")] {
            sent(&mut e, &part, 3);
            running(&mut e, &part, 3);
            write(&mut e, &part, &format!("{part} in round 2"), 3);
        }
        moderate(&mut e, r#"{"summary":"round two summary"}"#, 4);
        assert_eq!(no(&e), 3);
        // agy's round-3 brief starts where its last delivered one ended, so
        // round 1's summary and everything after reach it.
        let brief = round(&e).brief(&p("agy")).unwrap();
        assert_eq!(brief.since, r1_through);
        assert!(brief.body.contains("round one summary"), "{}", brief.body);
        assert!(brief.body.contains("round two summary"));
    }

    /// Round 1 with claude's first reply rejected (an unknown issue).
    fn rejected_reply() -> Engine {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 10);
        }
        let out = write(&mut e, &p("claude"), "---\nissues: [Z]\n---\nbody", 20);
        assert_eq!(sends(&out).len(), 1, "the correction goes out");
        assert!(
            round(&e)
                .brief(&p("claude"))
                .unwrap()
                .receipt
                .rejected
                .is_some()
        );
        e
    }

    #[test]
    fn a_correction_not_yet_sent_is_sent_again_after_a_restart() {
        let e = rejected_reply();
        let mut resumed = Engine::resume(e.session().clone(), Path::new("/d"), CONFIG);
        let out = feed(&mut resumed, Input::Start { now: 30 });
        let body = send_to(&out, "claude");
        assert!(body.contains("unknown issue"), "{body}");
    }

    #[test]
    fn a_correction_the_agent_never_starts_on_is_resent_at_the_start_timeout() {
        let mut e = rejected_reply();
        sent(&mut e, &p("claude"), 30);
        // No report that the agent started on the correction.
        let out = feed(
            &mut e,
            Input::Tick {
                now: 30 + CONFIG.start_timeout,
            },
        );
        assert_eq!(sends(&out).len(), 1, "{out:#?}");
    }

    #[test]
    fn an_agent_waiting_on_an_approval_does_not_time_out() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 10);
        }
        // agy used up its resends, then asks the user for an approval.
        for n in 1..=2u64 {
            feed(
                &mut e,
                Input::Tick {
                    now: n * CONFIG.reply_timeout + n,
                },
            );
            sent(&mut e, &p("agy"), n * CONFIG.reply_timeout + n);
        }
        assert_eq!(round(&e).brief(&p("agy")).unwrap().receipt.retries, 2);
        let asked = 2 * CONFIG.reply_timeout + 5;
        feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("agy"),
                state: AgentState::Permission,
                at: asked,
            },
        );
        // The user takes longer than the reply deadline to answer.
        feed(
            &mut e,
            Input::Tick {
                now: asked + 3 * CONFIG.reply_timeout,
            },
        );
        let receipt = &round(&e).brief(&p("agy")).unwrap().receipt;
        assert_eq!(receipt.failed, None, "waiting on the user is not a failure");
    }

    #[test]
    fn deadlines_paused_for_an_approval_run_again_in_the_next_round() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 1);
        }
        // agy asks for an approval, gets it, replies, and only then reports
        // that its turn ended.
        feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("agy"),
                state: AgentState::Permission,
                at: 2,
            },
        );
        for part in panel(&e) {
            write(&mut e, &part, "ok", 3);
        }
        feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("agy"),
                state: AgentState::Finished,
                at: 4,
            },
        );
        moderate(&mut e, r#"{"summary":"next"}"#, 5);
        assert_eq!(no(&e), 2);
        // Round 2: agy's brief goes out, and agy never starts on it.
        sent(&mut e, &p("agy"), 10);
        let out = feed(
            &mut e,
            Input::Tick {
                now: 10 + CONFIG.start_timeout,
            },
        );
        assert!(
            sends(&out).iter().any(|(part, _)| part.as_str() == "agy"),
            "the start deadline runs again: {out:#?}"
        );
    }

    #[test]
    fn replies_that_arrive_before_their_send_was_confirmed_still_end_the_round() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        // The link dropped right after each Enter: no `Sent`, but the
        // agents got their briefs and replied.
        for part in panel(&e) {
            write(&mut e, &part, "ok", 5);
        }
        assert_eq!(phase(&e), Phase::Summarizing);
    }

    #[test]
    fn a_new_pane_before_the_correction_went_out_gets_the_whole_brief() {
        let mut e = rejected_reply();
        // The correction is queued, not sent, when the pane closes and the
        // runner opens a new one for the agent.
        assert!(
            round(&e)
                .brief(&p("claude"))
                .unwrap()
                .receipt
                .sent
                .is_none()
        );
        let out = feed(
            &mut e,
            Input::PaneSpawned {
                part: p("claude"),
                pane_id: 9,
            },
        );
        let body = send_to(&out, "claude");
        for heading in PANELIST_SECTIONS {
            assert!(body.contains(heading), "{heading} missing from:\n{body}");
        }
    }

    #[test]
    fn a_brief_resent_after_a_rejection_carries_the_whole_situation() {
        let mut e = rejected_reply();
        sent(&mut e, &p("claude"), 30);
        // The pane closes (the runner reports it as an agent error): a new
        // agent gets the resend and knows nothing yet.
        let out = feed(
            &mut e,
            Input::AgentStateChanged {
                part: p("claude"),
                state: AgentState::Error,
                at: 40,
            },
        );
        let body = send_to(&out, "claude");
        for heading in PANELIST_SECTIONS {
            assert!(body.contains(heading), "{heading} missing from:\n{body}");
        }
    }

    fn engine_with_max_rounds(max_rounds: Option<RoundNo>) -> Engine {
        let setup = Setup {
            topic: "Pick a database",
            workdir: None,
            max_rounds,
            moderator: AgentKind::Codex,
            panel: &[AgentKind::Claude, AgentKind::Codex, AgentKind::Agy],
        };
        Engine::create_in(SessionId::new("disc-1"), setup, Path::new("/d"), CONFIG, 0)
            .unwrap()
            .0
    }

    #[test]
    fn the_discussion_closes_after_its_last_round() {
        let mut e = engine_with_max_rounds(Some(2));
        feed(&mut e, Input::Start { now: 0 });
        assert!(
            round(&e)
                .brief(&p("claude"))
                .unwrap()
                .body
                .contains("Round: 1 of 2")
        );
        run_panel(&mut e, 1);
        moderate(&mut e, r#"{"summary":"one"}"#, 2);
        assert_eq!(no(&e), 2);
        run_panel(&mut e, 3);
        let out = moderate(&mut e, r#"{"summary":"two"}"#, 4);
        assert!(e.session().closed);
        assert_eq!(e.session().rounds.len(), 2, "no third round");
        assert!(sends(&out).is_empty());
        // A restart does not open another round either.
        let mut resumed = Engine::resume(e.session().clone(), Path::new("/d"), CONFIG);
        assert!(sends(&feed(&mut resumed, Input::Start { now: 5 })).is_empty());
    }

    #[test]
    fn a_debate_proposed_in_the_last_round_does_not_keep_the_discussion_open() {
        let mut e = engine_with_max_rounds(Some(1));
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(
            &mut e,
            r#"{"summary":"split",
                "issue_ops":[{"op":"add","id":"A","parent":null,"title":"engine"},
                             {"op":"set_state","id":"A","state":"contested"}],
                "propose_debate":{"issue":"A","pro":["claude"],"con":["codex"]}}"#,
            2,
        );
        assert!(e.session().closed, "the last round is over");
        assert_eq!(e.session().rounds.len(), 1);
    }

    #[test]
    fn the_user_ends_the_discussion_mid_round_and_nothing_more_goes_out() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        for part in panel(&e) {
            sent(&mut e, &part, 0);
            running(&mut e, &part, 1);
        }
        feed(&mut e, Input::UserClose);
        assert!(e.session().closed);
        // Late replies, deadlines and retries move nothing.
        for part in panel(&e) {
            let out = write(&mut e, &part, "late", 2);
            assert!(sends(&out).is_empty());
        }
        assert_ne!(phase(&e), Phase::Summarizing);
        let out = feed(
            &mut e,
            Input::Tick {
                now: 10 * CONFIG.reply_timeout,
            },
        );
        assert!(sends(&out).is_empty());
        assert!(matches!(
            e.handle(Input::UserClose),
            Err(EngineError::Rejected(_))
        ));
        assert!(matches!(
            e.handle(Input::UserRetry),
            Err(EngineError::Rejected(_))
        ));
    }

    #[test]
    fn a_proposed_debate_waits_for_approval_and_runs_to_a_verdict() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(
            &mut e,
            r#"{"summary":"split","issue_ops":[{"op":"add","id":"B","title":"b"},{"op":"set_state","id":"B","state":"contested"}],
                "propose_debate":{"issue":"B","pro":["claude"],"con":["codex"]}}"#,
            2,
        );
        assert_eq!(no(&e), 1, "no new round before the user approves");
        assert_eq!(phase(&e), Phase::Closed);
        let id = e.session().pending_debate().unwrap().id.clone();
        // A restart in this state still waits.
        assert!(feed(&mut e, Input::Start { now: 3 }).effects.is_empty());
        assert!(matches!(
            e.handle(Input::UserApproveDebate {
                id: id.clone(),
                pro: Some(vec![p("codex")]),
                con: Some(vec![p("codex")]),
                at: 4,
            }),
            Err(EngineError::Rejected(_))
        ));
        // The user swaps the sides.
        let out = feed(
            &mut e,
            Input::UserApproveDebate {
                id: id.clone(),
                pro: Some(vec![p("codex")]),
                con: Some(vec![p("claude")]),
                at: 5,
            },
        );
        assert_eq!(round(&e).kind, RoundKind::Debate(id.clone()));
        assert!(send_to(&out, "codex").contains("you argue FOR issue B"));
        assert!(send_to(&out, "claude").contains("you argue AGAINST issue B"));
        assert!(send_to(&out, "agy").contains(brief::DEBATE_OBSERVER_QUESTION));

        run_panel(&mut e, 6);
        let debate = e.session().debate(&id).unwrap();
        assert_eq!(debate.status, DebateStatus::Active);
        assert_eq!(
            debate.turns.len(),
            2,
            "only the sides' replies are arguments"
        );
        moderate(&mut e, r#"{"summary":"go on"}"#, 7);
        assert_eq!(
            round(&e).kind,
            RoundKind::Debate(id.clone()),
            "second exchange"
        );

        run_panel(&mut e, 8);
        let out = moderate(&mut e, r#"{"summary":"done"}"#, 9);
        assert!(send_to(&out, "noa").contains("verdict: required"));
        moderate(
            &mut e,
            r#"{"summary":"done","verdict":{"outcome":"conditional","rationale":"if cheap"}}"#,
            10,
        );
        let debate = e.session().debate(&id).unwrap();
        assert_eq!(debate.status, DebateStatus::Closed);
        assert_eq!(
            debate.verdict.as_ref().unwrap().outcome,
            IssueState::Conditional
        );
        assert_eq!(
            e.session().issues.get(&IssueId::new("B")).unwrap().state,
            IssueState::Conditional
        );
        assert!(e.session().minutes.decisions[0].text.contains("if cheap"));
        assert!(
            e.session()
                .messages
                .iter()
                .any(|m| m.kind == MsgKind::Verdict)
        );
        assert_eq!(round(&e).kind, RoundKind::Open);
    }

    #[test]
    fn a_declined_debate_resumes_open_rounds() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(
            &mut e,
            r#"{"summary":"s","issue_ops":[{"op":"add","id":"B","title":"b"}],
                "questions":{"agy":"agy, your view?"},
                "propose_debate":{"issue":"B","pro":["claude"],"con":["codex"]}}"#,
            2,
        );
        let id = e.session().pending_debate().unwrap().id.clone();
        let out = feed(
            &mut e,
            Input::UserDeclineDebate {
                id: id.clone(),
                at: 3,
            },
        );
        assert_eq!(round(&e).kind, RoundKind::Open);
        assert_eq!(
            e.session().debate(&id).unwrap().status,
            DebateStatus::Declined
        );
        assert!(send_to(&out, "agy").contains("agy, your view?"));
        assert!(e.handle(Input::UserDeclineDebate { id, at: 4 }).is_err());
    }

    #[test]
    fn the_user_starts_a_debate_with_sides_from_stances() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        moderate(
            &mut e,
            r#"{"summary":"s","issue_ops":[{"op":"add","id":"B","title":"b"}]}"#,
            2,
        );
        let start = |e: &mut Engine| {
            e.handle(Input::UserStartDebate {
                issue: IssueId::new("B"),
                pro: None,
                con: None,
                at: 3,
            })
        };
        assert!(
            matches!(start(&mut e), Err(EngineError::Rejected(_))),
            "no stances yet"
        );
        for part in panel(&e) {
            sent(&mut e, &part, 4);
            running(&mut e, &part, 4);
        }
        write(&mut e, &p("claude"), "---\nstance: {B: for}\n---\nyes", 5);
        write(&mut e, &p("codex"), "---\nstance: {B: against}\n---\nno", 5);
        start(&mut e).unwrap();
        let debate = e.session().debates.last().unwrap().clone();
        assert_eq!(
            (debate.pro, debate.con),
            (vec![p("claude")], vec![p("codex")])
        );
        assert_eq!(debate.status, DebateStatus::Approved);
        assert_eq!(
            round(&e).kind,
            RoundKind::Open,
            "runs after the current round"
        );
        assert!(start(&mut e).is_err(), "one debate at a time");
        write(&mut e, &p("agy"), "hm", 6);
        moderate(&mut e, r#"{"summary":"s2"}"#, 7);
        assert_eq!(round(&e).kind, RoundKind::Debate(debate.id));
    }

    #[test]
    fn close_ends_the_session() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        let out = moderate(&mut e, r#"{"summary":"bye","close":true}"#, 2);
        assert!(e.session().closed);
        assert!(sends(&out).is_empty());
        assert_eq!(no(&e), 1);
        assert!(feed(&mut e, Input::Start { now: 3 }).effects.is_empty());
    }

    #[test]
    fn interventions_are_checked_and_logged() {
        let mut e = engine();
        assert!(
            e.handle(Input::UserIntervention {
                to: vec![],
                body: " ".into(),
                at: 0
            })
            .is_err()
        );
        assert!(
            e.handle(Input::UserIntervention {
                to: vec![p("gemini")],
                body: "x".into(),
                at: 0
            })
            .is_err()
        );
        feed(
            &mut e,
            Input::UserIntervention {
                to: vec![p("codex")],
                body: "focus".into(),
                at: 0,
            },
        );
        let message = e.session().messages.last().unwrap();
        assert_eq!(
            (message.kind, &message.to),
            (MsgKind::Intervention, &vec![p("codex")])
        );
        let out = feed(&mut e, Input::Start { now: 1 });
        assert!(send_to(&out, "claude").contains("- [m1] to codex: focus"));
    }

    #[test]
    fn a_restart_replays_to_the_same_state_and_picks_up_the_round() {
        let mut e = engine();
        let mut batches: Vec<Vec<Event>> = Vec::new();
        let (_, created) = Engine::create(
            SessionId::new("disc-1"),
            "Pick a database",
            AgentKind::Codex,
            &[AgentKind::Claude, AgentKind::Codex, AgentKind::Agy],
            Path::new("/d"),
            CONFIG,
            0,
        )
        .unwrap();
        batches.extend(created.batches);
        let mut step = |e: &mut Engine, input| {
            let out = feed(e, input);
            batches.extend(out.batches.clone());
            out
        };
        step(&mut e, Input::Start { now: 0 });
        step(
            &mut e,
            Input::Sent {
                round: 1,
                part: p("claude"),
                at: 1,
            },
        );
        step(
            &mut e,
            Input::PaneSpawned {
                part: p("agy"),
                pane_id: 7,
            },
        );

        let replayed = reduce::replay(&batches).unwrap();
        assert_eq!(&replayed, e.session());
        assert_eq!(replayed.panelists[2].pane_id, Some(7));

        let mut resumed = Engine::resume(replayed, Path::new("/d"), CONFIG);
        let out = resumed.handle(Input::Start { now: 2 }).unwrap();
        // Unsent briefs go out again; every unreplied one is watched.
        let mut resent: Vec<String> = sends(&out).into_iter().map(|(p, _)| p.0).collect();
        resent.sort();
        assert_eq!(resent, ["agy", "codex"]);
        let watched = out
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::WatchReply { .. }))
            .count();
        assert_eq!(watched, 3);
        assert!(out.batches.is_empty());
    }

    #[test]
    fn a_restart_while_summarizing_resends_the_moderator_brief() {
        let mut e = engine();
        feed(&mut e, Input::Start { now: 0 });
        run_panel(&mut e, 1);
        let mut resumed = Engine::resume(e.session().clone(), Path::new("/d"), CONFIG);
        let out = resumed.handle(Input::Start { now: 2 }).unwrap();
        assert_eq!(sends(&out).len(), 1);
        assert!(send_to(&out, "noa").contains("## 7. Your output"));
        assert!(out.effects.contains(&Effect::WatchReply {
            round: 1,
            part: p("noa"),
            path: PathBuf::from("/d/r1/moderator.json"),
        }));
    }

    #[test]
    fn unknown_inputs_are_rejected_or_ignored() {
        let mut e = engine();
        assert!(
            e.handle(Input::PaneSpawned {
                part: p("gemini"),
                pane_id: 1
            })
            .is_err()
        );
        assert!(
            e.handle(Input::UserApproveDebate {
                id: DebateId::nth(9),
                pro: None,
                con: None,
                at: 0
            })
            .is_err()
        );
        assert!(
            e.handle(Input::UserStartDebate {
                issue: IssueId::new("Z"),
                pro: None,
                con: None,
                at: 0
            })
            .is_err()
        );
        feed(&mut e, Input::Start { now: 0 });
        // A reply for a round that does not exist, or from no one, changes nothing.
        let out = feed(
            &mut e,
            Input::ReplyFileWritten {
                round: 9,
                part: p("claude"),
                content: "x".into(),
                at: 1,
            },
        );
        assert!(out.batches.is_empty());
        let out = feed(
            &mut e,
            Input::Sent {
                round: 1,
                part: p("gemini"),
                at: 1,
            },
        );
        assert!(out.batches.is_empty());
        // The moderator's file outside Summarizing is ignored.
        let out = feed(
            &mut e,
            Input::ReplyFileWritten {
                round: 1,
                part: p("noa"),
                content: "{}".into(),
                at: 1,
            },
        );
        assert!(out.batches.is_empty());
    }
}
