//! The competition round state machine. Pure: takes inputs with timestamps,
//! commits event batches to its session and returns them with effects to carry
//! out. Never does I/O or reads a clock.
//!
//! A round runs Briefing -> Competing (each contestant works in isolation in
//! `entries/<part>/` and writes `r<N>/<part>.md`) -> Judging (judge evaluates
//! all submissions and snapshots into `r<N>/judge.json`) -> Closed, then the
//! next round. Every `reveal_interval` completed rounds (default 5), the next
//! round is `RoundKind::Reveal` so contestants can inspect peers' work.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::brief;
use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{
    AgentChoice, ArtifactFile, Author, Brief, JUDGE_ID, MAX_PANEL, Message, Millis, MsgKind,
    PartId, Participant, Phase, Role, RoundKind, RoundNo, Session, SessionId, Submission,
};
use crate::model::validate;
use crate::store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub start_timeout: Millis,
    pub reply_timeout: Millis,
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
    Start {
        now: Millis,
    },
    PaneSpawned {
        part: PartId,
        pane_id: u64,
    },
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
    /// A watched reply file appeared. For contestants, `artifacts` lists the
    /// files snapshotted from `entries/<part>/` for this round.
    ReplyFileWritten {
        round: RoundNo,
        part: PartId,
        content: String,
        artifacts: Vec<ArtifactFile>,
        at: Millis,
    },
    /// The user sends an instruction; `to` empty means everyone.
    UserIntervention {
        to: Vec<PartId>,
        body: String,
        at: Millis,
    },
    /// The user manually picks the winner (and closes the competition if open).
    UserSelectWinner {
        winner: PartId,
    },
    UserRetry,
    UserClose,
    /// The user holds the competition after each judged round (`true`), or
    /// lets rounds follow each other again: a round that waits starts now.
    UserSetHold {
        hold: bool,
        at: Millis,
    },
    /// The user starts the next round of a held competition.
    UserContinue {
        at: Millis,
    },
    /// The user gives the competition `by` more rounds; one that ended at
    /// its round limit goes on.
    UserExtend {
        by: RoundNo,
        at: Millis,
    },
    /// The agent's pane was lost while the competition was stopped: its
    /// open brief goes out again as it was, without counting a retry.
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
    SendBrief {
        round: RoundNo,
        part: PartId,
        body: String,
    },
    WatchReply {
        round: RoundNo,
        part: PartId,
        path: PathBuf,
    },
    ClearReply {
        path: PathBuf,
    },
    /// Regenerate `compare.html` after new submissions or judgments land.
    RefreshViewer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub batches: Vec<Vec<Event>>,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    Rejected(String),
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

pub fn participants(judge: &AgentChoice, panel: &[AgentChoice]) -> (Participant, Vec<Participant>) {
    let mut contestants: Vec<Participant> = Vec::with_capacity(panel.len());
    for choice in panel {
        let kind = choice.kind;
        let seen = contestants.iter().filter(|p| p.kind == kind).count();
        let id = match seen {
            0 => kind.command().to_owned(),
            n => format!("{}-{}", kind.command(), n + 1),
        };
        contestants.push(Participant {
            id: PartId::new(id),
            kind,
            role: Role::Contestant,
            pane_id: None,
            setup: choice.setup.clone(),
        });
    }
    let judge = Participant {
        id: PartId::new(JUDGE_ID),
        kind: judge.kind,
        role: Role::Judge,
        pane_id: None,
        setup: judge.setup.clone(),
    };
    (judge, contestants)
}

/// Why the competition logged in `dir` cannot be resumed from its log.
/// What the log names is used as paths and in shell scripts, so a log
/// edited by hand (or moved) gets the checks a new session does: each
/// agent's setup, the participant ids its panel gives, and every reply
/// path being the one this competition's directory gives.
pub fn check_resumable(session: &Session, dir: &Path) -> Result<(), String> {
    if dir.file_name() != Some(std::ffi::OsStr::new(session.id.as_str())) {
        return Err(format!(
            "ログのコンペ ID {} がフォルダ名と一致しません",
            session.id
        ));
    }
    let choice = |p: &Participant| AgentChoice {
        kind: p.kind,
        setup: p.setup.clone(),
    };
    for part in std::iter::once(&session.judge).chain(&session.contestants) {
        part.setup.check(part.kind)?;
    }
    let panel: Vec<AgentChoice> = session.contestants.iter().map(choice).collect();
    let (judge, contestants) = participants(&choice(&session.judge), &panel);
    let same = |a: &Participant, b: &Participant| a.id == b.id && a.role == b.role;
    if !same(&judge, &session.judge)
        || contestants.len() != session.contestants.len()
        || !contestants
            .iter()
            .zip(&session.contestants)
            .all(|(a, b)| same(a, b))
    {
        return Err("ログの参加者 ID がパネルと一致しません".to_owned());
    }
    let reply_dir = store::reply_dir(dir, session);
    for round in &session.rounds {
        for brief in &round.briefs {
            if !session.is_contestant(&brief.to)
                || brief.reply_path != store::reply_path(&reply_dir, round.no, &brief.to)
            {
                return Err(format!(
                    "R{} の {} の返答パスがこのコンペの保存先と一致しません",
                    round.no, brief.to
                ));
            }
        }
        if let Some(brief) = &round.judge_brief
            && (brief.to != session.judge.id
                || brief.reply_path != store::judge_reply_path(&reply_dir, round.no))
        {
            return Err(format!(
                "R{} の審判の返答パスがこのコンペの保存先と一致しません",
                round.no
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Setup<'a> {
    pub request: &'a str,
    pub max_rounds: Option<RoundNo>,
    pub reveal_interval: RoundNo,
    pub workdir: Option<PathBuf>,
    pub judge: AgentChoice,
    pub panel: &'a [AgentChoice],
}

#[derive(Debug, Clone)]
pub struct Engine {
    session: Session,
    dir: PathBuf,
    config: Config,
    waiting: BTreeMap<PartId, Option<Millis>>,
}

impl Engine {
    pub fn create_in(
        id: SessionId,
        setup: Setup<'_>,
        dir: &Path,
        config: Config,
        now: Millis,
    ) -> Result<(Engine, Output), EngineError> {
        let request = setup.request.trim();
        if request.is_empty() {
            return rejected("依頼内容が空です");
        }
        if setup.panel.len() < 2 {
            return rejected("コンペティションには2体以上の参加パネルが必要です");
        }
        if setup.panel.len() > MAX_PANEL {
            return rejected(format!("参加パネルは{MAX_PANEL}体までです"));
        }
        for choice in std::iter::once(&setup.judge).chain(setup.panel) {
            if let Err(why) = choice.setup.check(choice.kind) {
                return rejected(why);
            }
        }
        let (judge, contestants) = participants(&setup.judge, setup.panel);
        let batch = vec![Event::Created {
            id,
            request: request.to_owned(),
            workdir: setup.workdir,
            max_rounds: setup.max_rounds,
            reveal_interval: setup.reveal_interval,
            judge,
            contestants,
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

    /// Root directory holding `entries/<part>/`, `r<N>/`, and `compare.html`.
    pub fn reply_dir(&self) -> PathBuf {
        store::reply_dir(&self.dir, &self.session)
    }

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

    pub fn handle(&mut self, input: Input) -> Result<Output, EngineError> {
        let mut out = Output::default();
        if self.session.closed {
            return match input {
                Input::UserSelectWinner { winner } => {
                    if !self.session.is_contestant(&winner) {
                        return rejected(format!("不明な参加者です: {winner}"));
                    }
                    self.commit(&mut out, vec![Event::WinnerSelected { winner }])?;
                    out.effects.push(Effect::RefreshViewer);
                    Ok(out)
                }
                Input::UserIntervention { .. }
                | Input::UserRetry
                | Input::UserClose
                | Input::UserSetHold { .. }
                | Input::UserContinue { .. } => rejected("コンペティションは終了しています"),
                Input::UserExtend { by, at } => {
                    self.extend(&mut out, by, at)?;
                    Ok(out)
                }
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
                artifacts,
                at,
            } => {
                self.reply(&mut out, round, &part, &content, artifacts, at)?;
                self.advance(&mut out)?;
            }
            Input::UserIntervention { to, body, at } => {
                self.intervention(&mut out, to, body, at)?;
            }
            Input::UserSelectWinner { winner } => {
                if !self.session.is_contestant(&winner) {
                    return rejected(format!("不明な参加者です: {winner}"));
                }
                self.commit(
                    &mut out,
                    vec![Event::WinnerSelected { winner }, Event::SessionClosed],
                )?;
                out.effects.push(Effect::RefreshViewer);
            }
            Input::UserRetry => self.user_retry(&mut out)?,
            Input::UserSetHold { hold, at } => {
                if hold != self.session.hold {
                    let waiting = self.session.waits_to_continue();
                    self.commit(&mut out, vec![Event::HoldSet { hold }])?;
                    // Let go while a round waits: nothing holds it now.
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
                out.effects.push(Effect::RefreshViewer);
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

    fn is_judge(&self, part: &PartId) -> bool {
        *part == self.session.judge.id
    }

    fn current_no(&self) -> Option<RoundNo> {
        self.session.current_round().map(|r| r.no)
    }

    fn open_brief(&self, round: RoundNo, part: &PartId) -> Option<&Brief> {
        let current = self.session.current_round()?;
        if current.no != round {
            return None;
        }
        let in_play = if self.is_judge(part) {
            current.phase == Phase::Judging
        } else {
            matches!(current.phase, Phase::Briefing | Phase::Competing)
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
        if self.session.closed {
            return Ok(());
        }
        let Some(round) = self.session.current_round() else {
            return self.next_round(out, now);
        };
        // A log cut between a round's start and its briefs (written before
        // the two went into one batch): the round gets its briefs first.
        if round.phase == Phase::Briefing {
            let no = round.no;
            let missing: Vec<PartId> = self
                .session
                .contestant_ids()
                .filter(|part| round.brief(part).is_none())
                .cloned()
                .collect();
            if !missing.is_empty() {
                let batch = self.brief_events(&self.session, no, &missing);
                self.commit(out, batch)?;
            }
        }
        let Some(round) = self.session.current_round() else {
            return Ok(());
        };
        // A resumed session: briefs out before it stopped get their full
        // time again from now, as the agents may have worked meanwhile.
        for brief in round.briefs.iter().chain(&round.judge_brief) {
            if brief.receipt.sent.is_some() && brief.receipt.replied.is_none() {
                self.waiting.insert(brief.to.clone(), Some(now));
            }
        }
        for brief in &round.briefs {
            if brief.receipt.replied.is_some() {
                continue;
            }
            if round.phase != Phase::Closed {
                Self::watch(out, round.no, brief);
            }
            if matches!(round.phase, Phase::Briefing | Phase::Competing)
                && brief.receipt.failed.is_none()
                && brief.receipt.sent.is_none()
            {
                Self::send(out, round.no, brief);
            }
        }
        match (round.phase, &round.judge_brief) {
            (Phase::Judging, Some(brief)) if brief.receipt.failed.is_none() => {
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

    /// Gives the competition `by` more rounds past the limit (or the rounds
    /// it has run). One that ended at its limit is open again, and its next
    /// round starts unless it is held.
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
            .unwrap_or_default()
            .max(self.session.rounds.len() as RoundNo);
        let max_rounds = base.saturating_add(by);
        self.commit(out, vec![Event::RoundsExtended { max_rounds, at }])?;
        out.effects.push(Effect::RefreshViewer);
        self.next_round(out, at)
    }

    /// Starts the round after a judged one (or the first), unless the
    /// competition is over or the user holds it after each round.
    fn next_round(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
        self.start_next(out, now, false)
    }

    /// [`Self::next_round`]; with `go`, past the user's hold.
    fn start_next(&mut self, out: &mut Output, now: Millis, go: bool) -> Result<(), EngineError> {
        if self.session.closed {
            return Ok(());
        }
        if let Some(round) = self.session.current_round()
            && round.phase != Phase::Closed
        {
            return Ok(());
        }
        if self
            .session
            .max_rounds
            .is_some_and(|max| self.session.rounds.len() as RoundNo >= max)
        {
            self.commit(out, vec![Event::SessionClosed])?;
            out.effects.push(Effect::RefreshViewer);
            return Ok(());
        }
        // Held: the judged round waits for the user (the limit above still
        // ends the competition).
        if self.session.hold && !go && !self.session.rounds.is_empty() {
            return Ok(());
        }
        let no = self.session.rounds.len() as RoundNo + 1;
        let kind = RoundKind::for_round(no, self.session.reveal_interval);
        // The round and its briefs go into one batch: a log cut after the
        // round's start alone would show a round with no one in it.
        let started = Event::RoundStarted { no, kind, at: now };
        let mut with_round = self.session.clone();
        reduce::apply(&mut with_round, std::slice::from_ref(&started))?;
        let parts: Vec<PartId> = self.session.contestant_ids().cloned().collect();
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

    /// `BriefIssued` for each of `parts` in round `no`, written from
    /// `session` (which already holds the round).
    fn brief_events(&self, session: &Session, no: RoundNo, parts: &[PartId]) -> Vec<Event> {
        let reply_dir = self.reply_dir();
        parts
            .iter()
            .map(|part| {
                let entry_dir = store::entry_dir(&reply_dir, part);
                let path = store::reply_path(&reply_dir, no, part);
                let brief =
                    brief::contestant_brief(session, part, no, &entry_dir, &reply_dir, &path);
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
                        phase: Phase::Competing,
                    }],
                )?;
                self.advance(out)
            }
            Phase::Competing
                if settled(|b| b.receipt.started.is_some())
                    && settled(|b| b.receipt.replied.is_some()) =>
            {
                self.judge_round(out, no)
            }
            _ => Ok(()),
        }
    }

    fn judge_round(&mut self, out: &mut Output, no: RoundNo) -> Result<(), EngineError> {
        let reply_dir = self.reply_dir();
        let path = store::judge_reply_path(&reply_dir, no);
        let brief = brief::judge_brief(&self.session, no, &reply_dir, &path);
        self.commit(
            out,
            vec![
                Event::PhaseChanged {
                    round: no,
                    phase: Phase::Judging,
                },
                Event::BriefIssued { round: no, brief },
            ],
        )?;
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
        for brief in round.briefs.iter().chain(&round.judge_brief) {
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
        artifacts: Vec<ArtifactFile>,
        at: Millis,
    ) -> Result<(), EngineError> {
        let Some(brief) = self.open_brief(round, part).cloned() else {
            return Ok(());
        };
        // A reply file written before Running was observed still proves the agent started.
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
        if self.is_judge(part) {
            match validate::parse_judge(content, &self.session, round) {
                Ok(output) => {
                    let mut batch = vec![
                        Event::ReplyAccepted {
                            round,
                            to: part.clone(),
                            at,
                        },
                        Event::JudgeAccepted {
                            round,
                            output: output.clone(),
                        },
                        Event::MessagePosted {
                            message: Message {
                                id: self.session.next_message_id(0),
                                round,
                                author: Author::Part(part.clone()),
                                kind: MsgKind::Judgment,
                                to: Vec::new(),
                                body: output.overview.clone(),
                                at,
                            },
                        },
                    ];
                    if let (Some(winner), Some(verdict)) = (&output.winner, &output.final_verdict) {
                        batch.push(Event::MessagePosted {
                            message: Message {
                                id: self.session.next_message_id(1),
                                round,
                                author: Author::Part(part.clone()),
                                kind: MsgKind::FinalVerdict,
                                to: Vec::new(),
                                body: format!("優勝: {winner}\n\n{verdict}"),
                                at,
                            },
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
                    out.effects.push(Effect::RefreshViewer);
                    if !self.session.closed {
                        self.next_round(out, at)?;
                    }
                }
                Err(error) => {
                    self.reject_reply(out, round, &brief, content, error.to_string())?;
                }
            }
        } else {
            match validate::parse_submission(content) {
                Ok(parsed) => {
                    let submission = Submission {
                        part: part.clone(),
                        summary: parsed.summary,
                        body: parsed.body.clone(),
                        artifacts,
                        at,
                    };
                    self.commit(
                        out,
                        vec![
                            Event::ReplyAccepted {
                                round,
                                to: part.clone(),
                                at,
                            },
                            Event::SubmissionRecorded { round, submission },
                            Event::MessagePosted {
                                message: Message {
                                    id: self.session.next_message_id(0),
                                    round,
                                    author: Author::Part(part.clone()),
                                    kind: MsgKind::Submission,
                                    to: Vec::new(),
                                    body: parsed.body,
                                    at,
                                },
                            },
                        ],
                    )?;
                    out.effects.push(Effect::RefreshViewer);
                }
                Err(error) => {
                    self.reject_reply(out, round, &brief, content, error.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn reject_reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        brief: &Brief,
        content: &str,
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
                    digest: Some(store::digest(content)),
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
mod tests {
    use super::*;
    use crate::model::types::{AgentKind, ArtifactKind};

    fn run_one_round(engine: &mut Engine, round: RoundNo, close: bool) {
        for part in [PartId::new("claude"), PartId::new("codex")] {
            engine
                .handle(Input::Sent {
                    round,
                    part: part.clone(),
                    at: round as u64 * 100,
                })
                .unwrap();
            engine
                .handle(Input::ReplyFileWritten {
                    round,
                    part: part.clone(),
                    content: format!(
                        "# Summary: {part} R{round} work\n\nDetails from {part} R{round}."
                    ),
                    artifacts: vec![ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 512,
                    }],
                    at: round as u64 * 100 + 10,
                })
                .unwrap();
        }
        let judge = PartId::new(JUDGE_ID);
        engine
            .handle(Input::Sent {
                round,
                part: judge.clone(),
                at: round as u64 * 100 + 20,
            })
            .unwrap();
        let winner_json = if close {
            r#""winner": "codex", "final_verdict": "Codex delivered a sharper UI.", "close": true"#
        } else {
            r#""winner": null, "final_verdict": null, "close": false"#
        };
        let json = format!(
            r#"{{
  "overview": "Round {round} evaluated.",
  "evaluations": {{
    "claude": {{ "score": {}, "strengths": "Solid", "feedback": "Iterate more" }},
    "codex": {{ "score": {}, "strengths": "Bold", "feedback": "Refine details" }}
  }},
  "ranking": ["codex", "claude"],
  {winner_json}
}}"#,
            70 + round,
            75 + round
        );
        engine
            .handle(Input::ReplyFileWritten {
                round,
                part: judge,
                content: json,
                artifacts: vec![],
                at: round as u64 * 100 + 30,
            })
            .unwrap();
    }

    fn two_contestants() -> Engine {
        let setup = Setup {
            request: "Design a music player UI",
            max_rounds: Some(2),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (mut engine, _) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        engine.handle(Input::Start { now: 2 }).unwrap();
        engine
    }

    fn submission(engine: &mut Engine, part: &str, content: &str, at: Millis) {
        engine
            .handle(Input::ReplyFileWritten {
                round: 1,
                part: PartId::new(part),
                content: content.into(),
                artifacts: vec![],
                at,
            })
            .unwrap();
    }

    #[test]
    fn a_pane_reopened_before_the_correction_gets_the_whole_brief() {
        let mut engine = two_contestants();
        let claude = PartId::new("claude");
        engine
            .handle(Input::Sent {
                round: 1,
                part: claude.clone(),
                at: 10,
            })
            .unwrap();
        // An empty report is turned down; the correction is not sent yet when
        // the agent's pane is started again.
        submission(&mut engine, "claude", "  ", 20);
        let out = engine
            .handle(Input::PaneSpawned {
                part: claude.clone(),
                pane_id: 7,
            })
            .unwrap();
        let body = out
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::SendBrief { part, body, .. } if *part == claude => Some(body),
                _ => None,
            })
            .expect("the brief is sent again");
        let brief = engine.session().round(1).unwrap().brief(&claude).unwrap();
        assert_eq!(*body, brief.body);
    }

    #[test]
    fn a_reply_taken_without_a_recorded_send_still_lets_the_round_be_judged() {
        let mut engine = two_contestants();
        engine
            .handle(Input::Sent {
                round: 1,
                part: PartId::new("codex"),
                at: 10,
            })
            .unwrap();
        // claude's `Sent` was lost with the link; its report arrives anyway.
        submission(&mut engine, "claude", "# Summary: done", 20);
        submission(&mut engine, "codex", "# Summary: done", 21);
        assert_eq!(engine.session().round(1).unwrap().phase, Phase::Judging);
    }

    #[test]
    fn five_blind_rounds_then_sixth_round_reveals_peer_work() {
        let setup = Setup {
            request: "Design a music player UI",
            max_rounds: Some(10),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (mut engine, _) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        engine.handle(Input::Start { now: 2 }).unwrap();

        for r in 1..=5 {
            assert_eq!(engine.session().round(r).unwrap().kind, RoundKind::Blind);
            let claude_brief = &engine
                .session()
                .round(r)
                .unwrap()
                .brief(&PartId::new("claude"))
                .unwrap()
                .body;
            assert!(!claude_brief.contains("Details from codex"));
            run_one_round(&mut engine, r, false);
        }

        let r6 = engine.session().round(6).unwrap();
        assert_eq!(r6.kind, RoundKind::Reveal);
        let claude_r6 = &r6.brief(&PartId::new("claude")).unwrap().body;
        assert!(claude_r6.contains("Peer showcase (round 5"));
        assert!(claude_r6.contains("r5/report-codex.md"));
    }

    #[test]
    fn a_resumed_session_restarts_the_time_limits_of_briefs_already_sent() {
        let mut engine = two_contestants();
        for part in ["claude", "codex"] {
            engine
                .handle(Input::Sent {
                    round: 1,
                    part: PartId::new(part),
                    at: 10,
                })
                .unwrap();
        }
        let config = Config::default();
        let later = 10 + 10 * config.reply_timeout;
        let mut resumed = Engine::resume(
            engine.session().clone(),
            Path::new("/tmp/comp-test"),
            config,
        );
        let out = resumed.handle(Input::Start { now: later }).unwrap();
        // Sent briefs are watched, not pasted again.
        assert!(
            out.effects
                .iter()
                .all(|e| matches!(e, Effect::WatchReply { .. }))
        );
        assert_eq!(out.effects.len(), 2);
        let out = resumed.handle(Input::Tick { now: later + 1 }).unwrap();
        assert!(out.batches.is_empty(), "{out:?}");
        let out = resumed
            .handle(Input::Tick {
                now: later + config.start_timeout,
            })
            .unwrap();
        assert_eq!(
            out.effects
                .iter()
                .filter(|e| matches!(e, Effect::SendBrief { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn a_panel_larger_than_the_cap_is_turned_down() {
        let panel = vec![AgentChoice::from(AgentKind::Claude); MAX_PANEL + 1];
        let setup = Setup {
            request: "x",
            max_rounds: Some(2),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &panel,
        };
        let err = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap_err();
        assert!(err.to_string().contains("6体まで"), "{err}");
        let (judge, contestants) = participants(&AgentKind::Agy.into(), &panel[..MAX_PANEL]);
        assert_eq!(judge.id.as_str(), JUDGE_ID);
        let ids: Vec<&str> = contestants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude", "claude-2", "claude-3", "claude-4", "claude-5", "claude-6"
            ]
        );
    }

    #[test]
    fn a_round_starts_in_one_batch_with_its_briefs() {
        let setup = Setup {
            request: "x",
            max_rounds: Some(2),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (mut engine, _) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        let out = engine.handle(Input::Start { now: 2 }).unwrap();
        assert_eq!(out.batches.len(), 1, "{:?}", out.batches);
        assert!(matches!(out.batches[0][0], Event::RoundStarted { .. }));
        assert_eq!(out.batches[0].len(), 3);
    }

    #[test]
    fn a_log_cut_after_a_rounds_start_briefs_the_round_before_judging_it() {
        let setup = Setup {
            request: "x",
            max_rounds: Some(2),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (_, created) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        // What an older version left when it stopped between the two lines.
        let cut = vec![
            created.batches[0].clone(),
            vec![Event::RoundStarted {
                no: 1,
                kind: RoundKind::Blind,
                at: 2,
            }],
        ];
        let session = reduce::replay(&cut).unwrap();
        let mut engine = Engine::resume(session, Path::new("/tmp/comp-test"), Config::default());
        let out = engine.handle(Input::Start { now: 3 }).unwrap();
        let round = engine.session().round(1).unwrap();
        assert_eq!(round.phase, Phase::Briefing);
        assert_eq!(round.briefs.len(), 2);
        assert!(round.judge_brief.is_none());
        let sends = out
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::SendBrief { .. }))
            .count();
        assert_eq!(sends, 2);
    }

    #[test]
    fn a_brief_lost_with_its_pane_goes_out_again_without_a_retry() {
        let mut engine = two_contestants();
        let claude = PartId::new("claude");
        // claude is on its last try.
        for _ in 0..Config::default().max_retries {
            engine
                .handle(Input::SendFailed {
                    round: 1,
                    part: claude.clone(),
                    error: "x".into(),
                })
                .unwrap();
        }
        engine
            .handle(Input::Sent {
                round: 1,
                part: claude.clone(),
                at: 10,
            })
            .unwrap();
        let out = engine
            .handle(Input::Redeliver {
                round: 1,
                part: claude.clone(),
            })
            .unwrap();
        assert!(out.batches.is_empty());
        assert!(matches!(&out.effects[..], [Effect::SendBrief { part, .. }] if *part == claude));
        let brief = engine.session().round(1).unwrap().brief(&claude).unwrap();
        assert!(brief.receipt.failed.is_none());
    }

    #[test]
    fn a_turned_down_reply_is_remembered_by_its_digest() {
        let mut engine = two_contestants();
        submission(&mut engine, "claude", "  ", 20);
        let brief = engine
            .session()
            .round(1)
            .unwrap()
            .brief(&PartId::new("claude"))
            .unwrap();
        assert!(brief.receipt.rejected.is_some());
        assert_eq!(brief.receipt.rejected_digest, Some(store::digest("  ")));
    }

    /// An engine and every batch it committed, from its creation on.
    struct Logged {
        engine: Engine,
        log: Vec<Vec<Event>>,
    }

    impl Logged {
        fn new(max_rounds: Option<RoundNo>) -> Self {
            let setup = Setup {
                request: "Design a music player UI",
                max_rounds,
                reveal_interval: 5,
                workdir: None,
                judge: AgentKind::Agy.into(),
                panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
            };
            let (engine, out) = Engine::create_in(
                SessionId::new("comp-test"),
                setup,
                Path::new("/tmp/comp-test"),
                Config::default(),
                1,
            )
            .unwrap();
            let mut logged = Logged {
                engine,
                log: out.batches,
            };
            logged.feed(Input::Start { now: 2 });
            logged
        }

        fn feed(&mut self, input: Input) -> Output {
            let out = self.engine.handle(input).unwrap();
            self.log.extend(out.batches.iter().cloned());
            out
        }

        fn try_feed(&mut self, input: Input) -> Result<Output, String> {
            let out = self.engine.handle(input).map_err(|e| e.to_string())?;
            self.log.extend(out.batches.iter().cloned());
            Ok(out)
        }

        fn round(&mut self, no: RoundNo, close: bool) {
            let before = self.engine.session().clone();
            run_one_round(&mut self.engine, no, close);
            // `run_one_round` drives the engine directly: log what it did.
            let after = self.engine.session().clone();
            assert!(after.rev > before.rev);
            self.log = replay_log(&self.log, &before, &after);
        }

        fn rounds(&self) -> usize {
            self.engine.session().rounds.len()
        }

        fn folds(&self) {
            assert_eq!(reduce::replay(&self.log).unwrap(), *self.engine.session());
        }
    }

    /// The log with the batches that took `before` to `after` appended,
    /// read back off a second engine driven the same way.
    fn replay_log(log: &[Vec<Event>], before: &Session, after: &Session) -> Vec<Vec<Event>> {
        let mut twin = Engine::resume(
            before.clone(),
            Path::new("/tmp/comp-test"),
            Config::default(),
        );
        let no = after.rounds.len().max(before.rounds.len()) as RoundNo;
        let mut log = log.to_vec();
        let round = before
            .current_round()
            .filter(|r| r.phase != Phase::Closed)
            .map_or(no, |r| r.no);
        let mut feed = |input| log.extend(twin.handle(input).unwrap().batches);
        for part in ["claude", "codex"] {
            let part = PartId::new(part);
            feed(Input::Sent {
                round,
                part: part.clone(),
                at: round as u64 * 100,
            });
            feed(Input::ReplyFileWritten {
                round,
                part: part.clone(),
                content: format!(
                    "# Summary: {part} R{round} work\n\nDetails from {part} R{round}."
                ),
                artifacts: vec![ArtifactFile {
                    rel_path: "index.html".into(),
                    kind: ArtifactKind::Html,
                    bytes: 512,
                }],
                at: round as u64 * 100 + 10,
            });
        }
        let judge = PartId::new(JUDGE_ID);
        feed(Input::Sent {
            round,
            part: judge.clone(),
            at: round as u64 * 100 + 20,
        });
        let winner = after.rounds[round as usize - 1]
            .outcome
            .as_ref()
            .and_then(|o| o.winner.clone());
        let winner_json = match winner {
            Some(w) => format!(
                r#""winner": "{w}", "final_verdict": "Codex delivered a sharper UI.", "close": true"#
            ),
            None => r#""winner": null, "final_verdict": null, "close": false"#.to_owned(),
        };
        let json = format!(
            r#"{{
  "overview": "Round {round} evaluated.",
  "evaluations": {{
    "claude": {{ "score": {}, "strengths": "Solid", "feedback": "Iterate more" }},
    "codex": {{ "score": {}, "strengths": "Bold", "feedback": "Refine details" }}
  }},
  "ranking": ["codex", "claude"],
  {winner_json}
}}"#,
            70 + round,
            75 + round
        );
        feed(Input::ReplyFileWritten {
            round,
            part: judge,
            content: json,
            artifacts: vec![],
            at: round as u64 * 100 + 30,
        });
        assert_eq!(twin.session(), after, "the twin drifted");
        log
    }

    fn sends(out: &Output) -> usize {
        out.effects
            .iter()
            .filter(|e| matches!(e, Effect::SendBrief { .. }))
            .count()
    }

    #[test]
    fn a_held_competition_waits_after_judging_and_takes_interventions_into_the_next_brief() {
        let mut l = Logged::new(Some(3));
        let err = l.try_feed(Input::UserContinue { at: 3 }).unwrap_err();
        assert!(err.contains("停止していません"), "{err}");
        let out = l.feed(Input::UserSetHold { hold: true, at: 3 });
        assert_eq!(out.batches, vec![vec![Event::HoldSet { hold: true }]]);
        assert!(
            l.feed(Input::UserSetHold { hold: true, at: 3 })
                .batches
                .is_empty()
        );
        let err = l.try_feed(Input::UserContinue { at: 3 }).unwrap_err();
        assert!(err.contains("進行中"), "{err}");

        l.round(1, false);
        assert_eq!(l.rounds(), 1, "held: no round 2 yet");
        assert!(l.engine.session().waits_to_continue());
        l.feed(Input::UserIntervention {
            to: vec![],
            body: "モバイル表示を優先して".into(),
            at: 200,
        });
        let out = l.feed(Input::UserContinue { at: 201 });
        assert_eq!(l.rounds(), 2);
        assert_eq!(sends(&out), 2);
        let brief = &l.engine.session().round(2).unwrap().briefs[0].body;
        assert!(brief.contains("モバイル表示を優先して"), "{brief}");

        // Still held after round 2; let go, and round 3 follows at once.
        l.round(2, false);
        assert_eq!(l.rounds(), 2);
        let out = l.feed(Input::UserSetHold {
            hold: false,
            at: 300,
        });
        assert_eq!(l.rounds(), 3);
        assert_eq!(sends(&out), 2);
        let err = l.try_feed(Input::UserContinue { at: 301 }).unwrap_err();
        assert!(err.contains("停止していません"), "{err}");
        l.folds();
    }

    #[test]
    fn a_held_competition_still_ends_at_its_round_limit() {
        let mut l = Logged::new(Some(1));
        l.feed(Input::UserSetHold { hold: true, at: 3 });
        // The final round's verdict closes it (the judge must close there).
        l.round(1, true);
        assert!(l.engine.session().closed);
        assert!(!l.engine.session().waits_to_continue());
        let err = l.try_feed(Input::UserContinue { at: 200 }).unwrap_err();
        assert!(err.contains("終了"), "{err}");
        l.folds();
    }

    #[test]
    fn a_competition_that_ran_out_of_rounds_goes_on_without_its_winner() {
        let mut l = Logged::new(Some(2));
        l.round(1, false);
        l.round(2, true);
        let session = l.engine.session();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("codex")));
        assert!(session.extendable().is_ok());
        let err = l
            .try_feed(Input::UserExtend { by: 0, at: 300 })
            .unwrap_err();
        assert!(err.contains("1ラウンド以上"), "{err}");

        let out = l.feed(Input::UserExtend { by: 3, at: 300 });
        assert_eq!(
            out.batches[0],
            vec![Event::RoundsExtended {
                max_rounds: 5,
                at: 300
            }]
        );
        let session = l.engine.session();
        assert!(!session.closed);
        assert_eq!(
            session.winner, None,
            "the winner waits for the new final round"
        );
        assert_eq!(session.max_rounds, Some(5));
        assert_eq!(session.rounds.len(), 3);
        assert_eq!(sends(&out), 2);
        // The earlier verdict stays where the judge gave it.
        let earlier = session.round(2).unwrap().outcome.as_ref().unwrap();
        assert_eq!(earlier.winner, Some(PartId::new("codex")));
        assert!(earlier.final_verdict.is_some());
        // Round 3 is no longer the final one.
        let brief = &session.round(3).unwrap().briefs[0].body;
        assert!(!brief.contains("FINAL"), "{brief}");
        l.folds();
    }

    #[test]
    fn a_running_competition_gets_a_higher_limit_without_a_new_round() {
        let mut l = Logged::new(Some(2));
        let out = l.feed(Input::UserExtend { by: 1, at: 3 });
        assert_eq!(l.engine.session().max_rounds, Some(3));
        assert_eq!(l.rounds(), 1);
        assert_eq!(sends(&out), 0);
        l.folds();
    }

    #[test]
    fn a_held_competition_extended_after_its_end_waits_for_the_user() {
        let mut l = Logged::new(Some(1));
        l.feed(Input::UserSetHold { hold: true, at: 3 });
        l.round(1, true);
        assert!(l.engine.session().closed);
        let out = l.feed(Input::UserExtend { by: 1, at: 200 });
        assert_eq!(sends(&out), 0);
        assert_eq!(l.rounds(), 1);
        assert!(l.engine.session().waits_to_continue());
        l.feed(Input::UserContinue { at: 201 });
        assert_eq!(l.rounds(), 2);
        l.folds();
    }

    #[test]
    fn a_competition_ended_early_or_without_a_limit_or_on_its_final_verdict_is_not_extended() {
        let refused = |l: &mut Logged| {
            l.try_feed(Input::UserExtend { by: 1, at: 900 })
                .unwrap_err()
        };
        // The judge closed it before its limit.
        let mut l = Logged::new(Some(3));
        l.round(1, true);
        assert!(refused(&mut l).contains("上限の前に終了"));
        // The user ended it.
        let mut l = Logged::new(Some(3));
        l.feed(Input::UserClose);
        assert!(refused(&mut l).contains("上限の前に終了"));
        // No limit.
        let mut l = Logged::new(None);
        assert!(refused(&mut l).contains("上限のない"));
        // The judge scores the round its brief called FINAL.
        let mut l = Logged::new(Some(1));
        for part in ["claude", "codex"] {
            submission(&mut l.engine, part, "# Summary: done", 10);
        }
        assert_eq!(l.engine.session().round(1).unwrap().phase, Phase::Judging);
        let judge = &l.engine.session().round(1).unwrap().judge_brief;
        assert!(judge.as_ref().unwrap().body.contains("FINAL"));
        assert!(refused(&mut l).contains("採点中"));
    }
}
