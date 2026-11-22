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
    AgentKind, ArtifactFile, Author, Brief, JUDGE_ID, Message, Millis, MsgKind, PartId,
    Participant, Phase, Role, RoundKind, RoundNo, Session, SessionId, Submission,
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

pub fn participants(judge: AgentKind, panel: &[AgentKind]) -> (Participant, Vec<Participant>) {
    let mut contestants: Vec<Participant> = Vec::with_capacity(panel.len());
    for &kind in panel {
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
        });
    }
    let judge = Participant {
        id: PartId::new(JUDGE_ID),
        kind: judge,
        role: Role::Judge,
        pane_id: None,
    };
    (judge, contestants)
}

#[derive(Debug, Clone)]
pub struct Setup<'a> {
    pub request: &'a str,
    pub max_rounds: Option<RoundNo>,
    pub reveal_interval: RoundNo,
    pub workdir: Option<PathBuf>,
    pub judge: AgentKind,
    pub panel: &'a [AgentKind],
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
        let (judge, contestants) = participants(setup.judge, setup.panel);
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
        match &self.session.workdir {
            Some(workdir) => store::workdir_reply_dir(workdir, &self.session.id),
            None => self.dir.clone(),
        }
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
                Input::UserIntervention { .. } | Input::UserRetry | Input::UserClose => {
                    rejected("コンペティションは終了しています")
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
        let Some(round) = self.session.current_round().cloned() else {
            return self.next_round(out, now);
        };
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

    fn next_round(&mut self, out: &mut Output, now: Millis) -> Result<(), EngineError> {
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
        let no = self.session.rounds.len() as RoundNo + 1;
        let kind = RoundKind::for_round(no, self.session.reveal_interval);
        self.commit(out, vec![Event::RoundStarted { no, kind, at: now }])?;

        let reply_dir = self.reply_dir();
        let briefs: Vec<Brief> = self
            .session
            .contestant_ids()
            .map(|part| {
                let entry_dir = store::entry_dir(&reply_dir, part);
                let path = store::reply_path(&reply_dir, no, part);
                brief::contestant_brief(&self.session, part, no, &entry_dir, &reply_dir, &path)
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
            if let Some(brief) = self.open_brief(round, part).cloned() {
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
                    self.reject_reply(out, round, &brief, error.to_string())?;
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
                    self.reject_reply(out, round, &brief, error.to_string())?;
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
    use crate::model::types::ArtifactKind;

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

    #[test]
    fn five_blind_rounds_then_sixth_round_reveals_peer_work() {
        let setup = Setup {
            request: "Design a music player UI",
            max_rounds: Some(10),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy,
            panel: &[AgentKind::Claude, AgentKind::Codex],
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
        assert!(claude_r6.contains("Details from codex R5."));
    }
}
