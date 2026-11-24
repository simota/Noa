//! The competition round state machine. Pure: takes inputs with timestamps,
//! commits event batches to its session and returns them with effects to carry
//! out. Never does I/O or reads a clock.
//!
//! A round runs Briefing -> Competing (each contestant works in isolation in
//! `entries/<part>/` and writes `r<N>/<part>.md`) -> Judging (judge evaluates
//! all submissions and snapshots into `r<N>/judge.json`) -> Closed, then the
//! next round. Every `reveal_interval` completed rounds (default 5), the next
//! round is `RoundKind::Reveal` so contestants can inspect peers' work.
//!
//! A judge panel (2–3 judges) scores in `Judging` in parallel, each judge
//! alone into `r<N>/<judge>.json`; the round settles on the mean once every
//! scorecard is in. In a deciding round (the last, or one the lead closes)
//! the lead then names the winner in `r<N>/verdict.json`, still in `Judging`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{
    AgentChoice, ArtifactFile, Author, Brief, MAX_JUDGES, MAX_PANEL, Message, Millis, MsgKind,
    PartId, Participant, Phase, Role, RoundKind, RoundNo, Session, SessionId, Submission, judge_id,
};
use crate::model::validate;
use crate::model::{aggregate, brief};
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

/// A competition's seats: the lead judge, the co-judges, the contestants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seats {
    pub judge: Participant,
    pub co_judges: Vec<Participant>,
    pub contestants: Vec<Participant>,
}

/// The participants `judges` (seat order, lead first; at least one) and
/// `panel` give. Judge ids follow the seat (`judge`, `judge-2`, …),
/// contestant ids the kind (`claude`, `claude-2`, …).
pub fn participants(judges: &[AgentChoice], panel: &[AgentChoice]) -> Seats {
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
    let mut judges = judges.iter().enumerate().map(|(seat, choice)| Participant {
        id: judge_id(seat),
        kind: choice.kind,
        role: Role::Judge,
        pane_id: None,
        setup: choice.setup.clone(),
    });
    let judge = judges.next().expect("a competition has at least one judge");
    Seats {
        judge,
        co_judges: judges.collect(),
        contestants,
    }
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

/// `path` with its deepest existing ancestor made canonical (the rest, not
/// written yet, joined as it is).
fn resolved(path: &Path) -> Option<std::path::PathBuf> {
    path.ancestors().find_map(|dir| {
        let rest = path.strip_prefix(dir).ok()?;
        Some(std::fs::canonicalize(dir).ok()?.join(rest))
    })
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
    // The ids below are derived from the log's own judge count, so its
    // bound is checked here, as a new session's is.
    let judge_count = session.judges().count();
    if judge_count > MAX_JUDGES {
        return Err(format!(
            "ログの審判が{judge_count}人います（審判は1〜{MAX_JUDGES}人です）"
        ));
    }
    for part in session.judges().chain(&session.contestants) {
        part.setup.check(part.kind)?;
    }
    let judges: Vec<AgentChoice> = session.judges().map(choice).collect();
    let panel: Vec<AgentChoice> = session.contestants.iter().map(choice).collect();
    let seats = participants(&judges, &panel);
    let same = |a: &Participant, b: &Participant| a.id == b.id && a.role == b.role;
    let all_same = |want: &[Participant], logged: &[Participant]| {
        want.len() == logged.len() && want.iter().zip(logged).all(|(a, b)| same(a, b))
    };
    if !same(&seats.judge, &session.judge)
        || !all_same(&seats.co_judges, &session.co_judges)
        || !all_same(&seats.contestants, &session.contestants)
    {
        return Err("ログの参加者 ID がパネルと一致しません".to_owned());
    }
    let reply_dir = store::reply_dir(dir, session);
    for round in &session.rounds {
        for brief in &round.briefs {
            if !session.is_contestant(&brief.to)
                || !same_file(
                    &brief.reply_path,
                    &store::reply_path(&reply_dir, round.no, &brief.to),
                )
            {
                return Err(format!(
                    "R{} の {} の返答パスがこのコンペの保存先と一致しません",
                    round.no, brief.to
                ));
            }
        }
        if let Some(brief) = &round.judge_brief
            && (brief.to != session.judge.id
                || !same_file(
                    &brief.reply_path,
                    &store::judge_reply_path(&reply_dir, round.no, &brief.to),
                ))
        {
            return Err(format!(
                "R{} の審判の返答パスがこのコンペの保存先と一致しません",
                round.no
            ));
        }
        for brief in &round.co_judge_briefs {
            if !session.co_judges.iter().any(|j| j.id == brief.to)
                || !same_file(
                    &brief.reply_path,
                    &store::judge_reply_path(&reply_dir, round.no, &brief.to),
                )
            {
                return Err(format!(
                    "R{} の審判 {} の返答パスがこのコンペの保存先と一致しません",
                    round.no, brief.to
                ));
            }
        }
        if let Some(brief) = &round.verdict_brief
            && (brief.to != session.judge.id
                || !same_file(
                    &brief.reply_path,
                    &store::verdict_reply_path(&reply_dir, round.no),
                ))
        {
            return Err(format!(
                "R{} の最終講評の返答パスがこのコンペの保存先と一致しません",
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
    /// The judge panel in seat order, lead first.
    pub judges: &'a [AgentChoice],
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
        if setup.judges.is_empty() || setup.judges.len() > MAX_JUDGES {
            return rejected("審判は1〜3人です");
        }
        for choice in setup.judges.iter().chain(setup.panel) {
            if let Err(why) = choice.setup.check(choice.kind) {
                return rejected(why);
            }
        }
        let Seats {
            judge,
            co_judges,
            contestants,
        } = participants(setup.judges, setup.panel);
        let batch = vec![Event::Created {
            id,
            request: request.to_owned(),
            workdir: setup.workdir,
            max_rounds: setup.max_rounds,
            reveal_interval: setup.reveal_interval,
            judge,
            co_judges,
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
        self.session.is_judge(part)
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
        for brief in round.briefs.iter().chain(round.judge_briefs()) {
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
        match round.phase {
            // Every judge brief still open: each judge whose scorecard is
            // in is left alone, and a round waiting for its verdict has the
            // verdict brief in the lead's place.
            Phase::Judging => {
                for brief in round.judge_briefs() {
                    if brief.receipt.failed.is_some() || brief.receipt.replied.is_some() {
                        continue;
                    }
                    Self::watch(out, round.no, brief);
                    if brief.receipt.sent.is_none() {
                        Self::send(out, round.no, brief);
                    }
                }
            }
            Phase::Closed => return self.next_round(out, now),
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
        let Some(max_rounds) = base.checked_add(by) else {
            return rejected(format!("ラウンド上限をこれ以上増やせません（{base}）"));
        };
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
        if self.session.is_panel() {
            return self.judge_round_by_panel(out, no);
        }
        let reply_dir = self.reply_dir();
        let path = store::judge_reply_path(&reply_dir, no, &self.session.judge.id);
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

    /// A panel's judging: every judge gets its own brief in one batch, and
    /// all of them go out at once; each scores alone.
    fn judge_round_by_panel(&mut self, out: &mut Output, no: RoundNo) -> Result<(), EngineError> {
        let reply_dir = self.reply_dir();
        let mut batch = vec![Event::PhaseChanged {
            round: no,
            phase: Phase::Judging,
        }];
        for judge in self.session.judges() {
            let path = store::judge_reply_path(&reply_dir, no, &judge.id);
            let brief = brief::scorecard_brief(&self.session, no, &judge.id, &reply_dir, &path);
            batch.push(Event::BriefIssued { round: no, brief });
        }
        self.commit(out, batch)?;
        if let Some(round) = self.session.current_round() {
            for brief in round.judge_briefs() {
                Self::watch(out, no, brief);
                Self::send(out, no, brief);
            }
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
        for brief in round.briefs.iter().chain(round.judge_briefs()) {
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
        if self.is_judge(part) && self.session.is_panel() {
            if self
                .session
                .round(round)
                .is_some_and(|r| r.verdict_brief.is_some())
            {
                self.verdict_reply(out, round, &brief, content, at)?;
            } else {
                self.scorecard_reply(out, round, &brief, content, at)?;
            }
        } else if self.is_judge(part) {
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

    /// A panel judge's scorecard. Accepted alone while other judges are
    /// still scoring; the last one also settles the round: the combined
    /// judgment, the lead's overview as the round's judgment and, in a
    /// deciding round, the request for the lead's verdict, else the round's
    /// close and the next round.
    fn scorecard_reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        brief: &Brief,
        content: &str,
        at: Millis,
    ) -> Result<(), EngineError> {
        let judge = &brief.to;
        let output = match validate::parse_scorecard(content, &self.session, round, judge) {
            Ok(output) => output,
            Err(error) => return self.reject_reply(out, round, brief, content, error.to_string()),
        };
        let mut batch = vec![
            Event::ReplyAccepted {
                round,
                to: judge.clone(),
                at,
            },
            Event::ScorecardAccepted {
                round,
                judge: judge.clone(),
                output,
            },
        ];
        let mut with_card = self.session.clone();
        reduce::apply(&mut with_card, &batch)?;
        // `open_brief` found the brief, so the round exists.
        let cards = &with_card.round(round).expect("the judged round").scorecards;
        if !aggregate::missing_scorecards(&with_card, cards).is_empty() {
            return self.commit(out, batch);
        }
        let combined = aggregate::combine(&with_card, round, cards);
        let deciding = combined.close;
        let lead = self.session.judge.id.clone();
        batch.push(Event::JudgeAccepted {
            round,
            output: combined.clone(),
        });
        batch.push(Event::MessagePosted {
            message: Message {
                id: self.session.next_message_id(0),
                round,
                author: Author::Part(lead.clone()),
                kind: MsgKind::Judgment,
                to: Vec::new(),
                body: combined.overview,
                at,
            },
        });
        if deciding {
            // The verdict brief is written from the round as combined.
            let mut combined_session = self.session.clone();
            reduce::apply(&mut combined_session, &batch)?;
            let path = store::verdict_reply_path(&self.reply_dir(), round);
            let brief = brief::verdict_brief(&combined_session, round, &path);
            batch.push(Event::VerdictRequested { round, brief });
            self.commit(out, batch)?;
            out.effects.push(Effect::RefreshViewer);
            if let Some(brief) = self.open_brief(round, &lead) {
                Self::watch(out, round, brief);
                Self::send(out, round, brief);
            }
        } else {
            batch.push(Event::PhaseChanged {
                round,
                phase: Phase::Closed,
            });
            self.commit(out, batch)?;
            out.effects.push(Effect::RefreshViewer);
            self.next_round(out, at)?;
        }
        Ok(())
    }

    /// The panel lead's verdict: the winner among the highest means and
    /// the final verdict close the competition.
    fn verdict_reply(
        &mut self,
        out: &mut Output,
        round: RoundNo,
        brief: &Brief,
        content: &str,
        at: Millis,
    ) -> Result<(), EngineError> {
        let verdict = match validate::parse_verdict(content, &self.session, round) {
            Ok(verdict) => verdict,
            Err(error) => return self.reject_reply(out, round, brief, content, error.to_string()),
        };
        let lead = brief.to.clone();
        let body = format!("優勝: {}\n\n{}", verdict.winner, verdict.final_verdict);
        self.commit(
            out,
            vec![
                Event::ReplyAccepted {
                    round,
                    to: lead.clone(),
                    at,
                },
                Event::VerdictAccepted {
                    round,
                    winner: verdict.winner,
                    final_verdict: verdict.final_verdict,
                },
                Event::MessagePosted {
                    message: Message {
                        id: self.session.next_message_id(0),
                        round,
                        author: Author::Part(lead),
                        kind: MsgKind::FinalVerdict,
                        to: Vec::new(),
                        body,
                        at,
                    },
                },
                Event::PhaseChanged {
                    round,
                    phase: Phase::Closed,
                },
                Event::SessionClosed,
            ],
        )?;
        out.effects.push(Effect::RefreshViewer);
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
    use crate::model::types::{AgentKind, AgentSetup, ArtifactKind, JUDGE_ID};
    use crate::model::types::{Author, MsgKind};

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
            judges: &[AgentKind::Agy.into()],
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
            judges: &[AgentKind::Agy.into()],
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
            judges: &[AgentKind::Agy.into()],
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
        let seats = participants(&[AgentKind::Agy.into()], &panel[..MAX_PANEL]);
        assert_eq!(seats.judge.id.as_str(), JUDGE_ID);
        let ids: Vec<&str> = seats.contestants.iter().map(|p| p.id.as_str()).collect();
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
            judges: &[AgentKind::Agy.into()],
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
            judges: &[AgentKind::Agy.into()],
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
                judges: &[AgentKind::Agy.into()],
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
        assert!(refused(&mut l).contains("最終ラウンドの進行中"));
        // Nor before it is judged: its briefs, sent or waiting, say FINAL.
        let mut l = Logged::new(Some(1));
        assert_eq!(l.engine.session().round(1).unwrap().phase, Phase::Briefing);
        assert!(
            l.engine.session().round(1).unwrap().briefs[0]
                .body
                .contains("FINAL")
        );
        assert!(refused(&mut l).contains("最終ラウンドの進行中"));
    }

    #[test]
    fn a_verdict_is_the_result_only_while_its_winner_is() {
        let mut l = Logged::new(Some(2));
        l.round(1, false);
        l.round(2, true);
        let verdict = "Codex delivered a sharper UI.";
        let md = crate::model::export::result_markdown(l.engine.session());
        assert!(md.contains("## 最終講評") && md.contains(verdict), "{md}");

        // Going on: no result yet, though round 2 keeps its verdict.
        l.feed(Input::UserExtend { by: 1, at: 300 });
        let md = crate::model::export::result_markdown(l.engine.session());
        assert!(!md.contains("## 最終講評"), "{md}");
        assert_eq!(l.engine.session().final_verdict(), None);

        // The user picks claude: codex's verdict is not claude's.
        l.feed(Input::UserSelectWinner {
            winner: PartId::new("claude"),
        });
        assert_eq!(l.engine.session().winner, Some(PartId::new("claude")));
        assert_eq!(l.engine.session().final_verdict(), None);
        let md = crate::model::export::result_markdown(l.engine.session());
        assert!(!md.contains(verdict), "{md}");
        l.folds();
    }

    /// A held competition whose last judged round is its limit ends rather
    /// than waiting: on resume, and when the user continues. (The judge's
    /// final verdict closes it itself, so this reaches the limit check
    /// only from a log whose final round closed without that.)
    #[test]
    fn the_round_limit_comes_before_the_hold() {
        let mut l = Logged::new(Some(2));
        l.feed(Input::UserSetHold { hold: true, at: 3 });
        l.round(1, false);
        let held = l.engine.session().clone();
        assert!(held.waits_to_continue());
        let at_limit = |session: &Session| {
            let mut session = session.clone();
            session.max_rounds = Some(1);
            Engine::resume(session, Path::new("/tmp/comp-test"), Config::default())
        };

        let mut e = at_limit(&held);
        e.handle(Input::Start { now: 400 }).unwrap();
        assert!(e.session().closed, "held past its limit on resume");

        let mut e = at_limit(&held);
        e.handle(Input::UserContinue { at: 400 }).unwrap();
        assert!(e.session().closed, "a round past the limit started");
        assert_eq!(e.session().rounds.len(), 1);

        // Before the limit, the hold keeps it waiting.
        let mut e = Engine::resume(held, Path::new("/tmp/comp-test"), Config::default());
        e.handle(Input::Start { now: 400 }).unwrap();
        assert!(!e.session().closed);
        assert_eq!(e.session().rounds.len(), 1);
    }

    #[test]
    fn an_extension_past_the_largest_limit_is_refused_whole() {
        let l = Logged::new(Some(2));
        let mut session = l.engine.session().clone();
        session.max_rounds = Some(RoundNo::MAX - 1);
        let mut e = Engine::resume(session, Path::new("/tmp/comp-test"), Config::default());
        let err = e
            .handle(Input::UserExtend { by: 3, at: 5 })
            .unwrap_err()
            .to_string();
        assert!(err.contains("これ以上増やせません"), "{err}");
        assert_eq!(e.session().max_rounds, Some(RoundNo::MAX - 1));
        let out = e.handle(Input::UserExtend { by: 1, at: 6 }).unwrap();
        assert_eq!(out.batches.len(), 1);
        assert_eq!(e.session().max_rounds, Some(RoundNo::MAX));
    }

    #[test]
    fn more_than_three_judges_are_refused() {
        let create = |judges: &[AgentChoice]| {
            let setup = Setup {
                request: "x",
                max_rounds: Some(2),
                reveal_interval: 5,
                workdir: None,
                judges,
                panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
            };
            Engine::create_in(
                SessionId::new("comp-test"),
                setup,
                Path::new("/tmp/comp-test"),
                Config::default(),
                1,
            )
            .map(|(engine, _)| engine)
            .map_err(|e| e.to_string())
        };
        let agy = AgentChoice::from(AgentKind::Agy);
        for judges in [vec![], vec![agy.clone(); MAX_JUDGES + 1]] {
            assert_eq!(
                create(&judges).err().as_deref(),
                Some("審判は1〜3人です"),
                "{} judges",
                judges.len()
            );
        }
        // Every judge's setup is checked, the third one's too.
        let bad = AgentChoice {
            kind: AgentKind::Agy,
            setup: AgentSetup {
                model: None,
                effort: Some("ultra".into()),
            },
        };
        let err = create(&[agy.clone(), agy.clone(), bad]).unwrap_err();
        assert!(err.contains("ultra"), "{err}");
        // One to three judges, ids by seat whatever the kinds.
        for n in 2..=MAX_JUDGES {
            let panel = create(&vec![agy.clone(); n]).unwrap();
            let ids: Vec<&str> = panel.session().judges().map(|j| j.id.as_str()).collect();
            assert_eq!(ids, ["judge", "judge-2", "judge-3"][..n], "{n} judges");
        }
        let one = create(std::slice::from_ref(&agy)).unwrap();
        assert!(!one.session().is_panel());
        assert_eq!(one.session().judge.id.as_str(), JUDGE_ID);
    }

    #[test]
    fn check_resumable_refuses_more_judges_than_a_panel_takes() {
        let dir = Path::new("/tmp/competitions/comp-crowd");
        let seats = participants(
            &vec![AgentKind::Agy.into(); MAX_JUDGES + 1],
            &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        );
        let ids: Vec<&str> = std::iter::once(&seats.judge)
            .chain(&seats.co_judges)
            .map(|j| j.id.as_str())
            .collect();
        assert_eq!(ids, ["judge", "judge-2", "judge-3", "judge-4"]);
        let session = reduce::replay([vec![Event::Created {
            id: SessionId::new("comp-crowd"),
            request: "x".into(),
            workdir: None,
            max_rounds: Some(2),
            reveal_interval: 5,
            judge: seats.judge,
            co_judges: seats.co_judges,
            contestants: seats.contestants,
            at: 1,
        }]])
        .unwrap();
        let err = check_resumable(&session, dir).unwrap_err();
        assert!(err.contains("審判") && err.contains('4'), "{err}");
        // One fewer is a panel it takes.
        let mut three = session.clone();
        three.co_judges.pop();
        assert_eq!(check_resumable(&three, dir), Ok(()));
    }

    #[test]
    fn check_resumable_checks_every_judge() {
        use crate::model::types::{Receipt, judge_id};
        let id = SessionId::new("comp-panel");
        let dir = Path::new("/tmp/competitions/comp-panel");
        let seats = participants(
            &[
                AgentKind::Agy.into(),
                AgentKind::Claude.into(),
                AgentKind::Claude.into(),
            ],
            &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        );
        let created = Event::Created {
            id,
            request: "x".into(),
            workdir: None,
            max_rounds: Some(2),
            reveal_interval: 5,
            judge: seats.judge,
            co_judges: seats.co_judges,
            contestants: seats.contestants,
            at: 1,
        };
        let brief = |to: PartId, path: PathBuf| Event::BriefIssued {
            round: 1,
            brief: Brief {
                to,
                base_rev: 2,
                body: String::new(),
                reply_path: path,
                receipt: Receipt::default(),
            },
        };
        let judge_path = |seat: usize| store::judge_reply_path(dir, 1, &judge_id(seat));
        let session = reduce::replay([
            vec![created],
            vec![
                Event::RoundStarted {
                    no: 1,
                    kind: RoundKind::Blind,
                    at: 2,
                },
                Event::PhaseChanged {
                    round: 1,
                    phase: Phase::Judging,
                },
                brief(judge_id(0), judge_path(0)),
                brief(judge_id(1), judge_path(1)),
                brief(judge_id(2), judge_path(2)),
            ],
        ])
        .unwrap();
        assert_eq!(check_resumable(&session, dir), Ok(()));

        // Ids and roles re-derived from the logged setups.
        let mut renamed = session.clone();
        renamed.co_judges[1].id = PartId::new("judge-9");
        assert!(check_resumable(&renamed, dir).is_err());
        let mut demoted = session.clone();
        demoted.co_judges[0].role = Role::Contestant;
        assert!(check_resumable(&demoted, dir).is_err());
        let mut dropped = session.clone();
        dropped.co_judges.pop();
        assert!(check_resumable(&dropped, dir).is_err());
        // Each judge's setup.
        let mut unsafe_model = session.clone();
        unsafe_model.co_judges[1].setup.model = Some("$(x)".into());
        assert!(check_resumable(&unsafe_model, dir).is_err());
        // Each co-judge brief's reply path is its own file.
        let mut swapped = session.clone();
        swapped.rounds[0].co_judge_briefs[1].reply_path = judge_path(1);
        let err = check_resumable(&swapped, dir).unwrap_err();
        assert!(err.contains("judge-3"), "{err}");
        let mut lead_elsewhere = session.clone();
        lead_elsewhere.rounds[0]
            .judge_brief
            .as_mut()
            .unwrap()
            .reply_path = judge_path(2);
        assert!(check_resumable(&lead_elsewhere, dir).is_err());
        // The verdict brief's is r<N>/verdict.json.
        let mut verdict = session.clone();
        let mut asked = verdict.rounds[0].judge_brief.clone().unwrap();
        asked.reply_path = judge_path(0);
        verdict.rounds[0].verdict_brief = Some(asked);
        assert!(check_resumable(&verdict, dir).is_err());
        verdict.rounds[0].verdict_brief.as_mut().unwrap().reply_path =
            store::verdict_reply_path(dir, 1);
        assert_eq!(check_resumable(&verdict, dir), Ok(()));
    }
    // ---- A judge panel: agy leads, two claude judges score. ----

    const PANEL_DIR: &str = "/tmp/comp-panel";

    fn seat(n: usize) -> PartId {
        judge_id(n)
    }

    impl Logged {
        /// A competition of claude and codex judged by a panel of three
        /// (agy leads; two claude judges, on opus and sonnet), started.
        fn panel(max_rounds: Option<RoundNo>) -> Self {
            let claude = |model: &str| AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some(model.into()),
                    effort: None,
                },
            };
            let setup = Setup {
                request: "Design a music player UI",
                max_rounds,
                reveal_interval: 5,
                workdir: None,
                judges: &[AgentKind::Agy.into(), claude("opus"), claude("sonnet")],
                panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
            };
            let (engine, out) = Engine::create_in(
                SessionId::new("comp-panel"),
                setup,
                Path::new(PANEL_DIR),
                Config::default(),
                1,
            )
            .unwrap();
            let mut logged = Logged {
                engine,
                log: out.batches,
            };
            logged.feed(Input::Start { now: 2 });
            logged.folds();
            logged
        }

        /// Feeds `input` and checks the log still folds to the session.
        fn step(&mut self, input: Input) -> Output {
            let out = self.feed(input);
            self.folds();
            out
        }

        fn current(&self) -> &crate::model::types::Round {
            self.engine.session().current_round().unwrap()
        }

        /// Both contestants take round `no`'s brief and submit.
        fn compete(&mut self, no: RoundNo) -> Output {
            let mut last = Output::default();
            for part in ["claude", "codex"] {
                let at = no as u64 * 1_000;
                self.step(Input::Sent {
                    round: no,
                    part: PartId::new(part),
                    at,
                });
                last = self.step(Input::ReplyFileWritten {
                    round: no,
                    part: PartId::new(part),
                    content: format!("# Summary: {part} R{no}\n\nDetails from {part} R{no}."),
                    artifacts: vec![],
                    at: at + 10,
                });
            }
            last
        }

        /// The judge in seat `n` takes its round-`no` brief.
        fn sent(&mut self, no: RoundNo, n: usize) {
            self.step(Input::Sent {
                round: no,
                part: seat(n),
                at: no as u64 * 1_000 + 20,
            });
        }

        /// The judge in seat `n` writes `content` as its round-`no` reply.
        fn write(&mut self, no: RoundNo, n: usize, content: &str) -> Output {
            self.step(Input::ReplyFileWritten {
                round: no,
                part: seat(n),
                content: content.to_owned(),
                artifacts: vec![],
                at: no as u64 * 1_000 + 30 + n as u64,
            })
        }

        /// Every judge takes its brief and scores (seat, claude, codex).
        fn score_all(&mut self, no: RoundNo, scores: [(u8, u8); 3]) -> Output {
            let mut last = Output::default();
            for (n, (claude, codex)) in scores.into_iter().enumerate() {
                self.sent(no, n);
                last = self.write(no, n, &card_json(n, no, claude, codex, ""));
            }
            last
        }
    }

    /// Seat `n`'s scorecard for round `no`. Every text names its judge
    /// (`[by judge-2]`), so a brief that quotes one is easy to spot.
    fn card_json(n: usize, no: RoundNo, claude: u8, codex: u8, extra: &str) -> String {
        let by = seat(n);
        let (first, second) = if codex >= claude {
            ("codex", "claude")
        } else {
            ("claude", "codex")
        };
        format!(
            r#"{{
  "overview": "[by {by}] R{no} overview",
  "evaluations": {{
    "claude": {{ "score": {claude}, "strengths": "[by {by}] claude strengths", "feedback": "[by {by}] claude feedback" }},
    "codex": {{ "score": {codex}, "strengths": "[by {by}] codex strengths", "feedback": "[by {by}] codex feedback" }}
  }},
  "ranking": ["{first}", "{second}"]{extra}
}}"#
        )
    }

    fn sent_to(out: &Output) -> Vec<PartId> {
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::SendBrief { part, .. } => Some(part.clone()),
                _ => None,
            })
            .collect()
    }

    fn body_to<'a>(out: &'a Output, to: &PartId) -> &'a str {
        out.effects
            .iter()
            .find_map(|e| match e {
                Effect::SendBrief { part, body, .. } if part == to => Some(body.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing sent to {to}: {out:?}"))
    }

    fn event_names(batch: &[Event]) -> Vec<String> {
        batch
            .iter()
            .map(|e| {
                let json = serde_json::to_value(e).unwrap();
                json["event"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn every_judge_is_briefed_in_one_batch() {
        let mut l = Logged::panel(Some(3));
        let out = l.compete(1);
        let judging = out.batches.last().unwrap();
        assert_eq!(
            event_names(judging),
            [
                "phase_changed",
                "brief_issued",
                "brief_issued",
                "brief_issued"
            ]
        );
        assert_eq!(sent_to(&out), [seat(0), seat(1), seat(2)]);
        let dir = Path::new(PANEL_DIR);
        let watched: Vec<PathBuf> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::WatchReply { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let paths: Vec<PathBuf> = (0..3)
            .map(|n| store::judge_reply_path(dir, 1, &seat(n)))
            .collect();
        assert_eq!(watched, paths);
        let round = l.current();
        assert_eq!(round.phase, Phase::Judging);
        for n in 0..3 {
            let brief = round.brief(&seat(n)).unwrap();
            assert_eq!(brief.reply_path, paths[n]);
            // Each brief names its own reply path, and no other judge's.
            for (m, path) in paths.iter().enumerate() {
                let named = brief.body.contains(&path.display().to_string());
                assert_eq!(named, m == n, "{} names {}", seat(n), path.display());
            }
            assert!(brief.body.contains(&format!(
                "Judging panel: judge (lead), judge-2, judge-3. You are {}.",
                seat(n)
            )));
        }
        // Each judge runs in its own pane.
        for n in 0..3 {
            l.step(Input::PaneSpawned {
                part: seat(n),
                pane_id: 40 + n as u64,
            });
        }
        let panes: Vec<Option<u64>> = l.engine.session().judges().map(|j| j.pane_id).collect();
        assert_eq!(panes, [Some(40), Some(41), Some(42)]);
    }

    #[test]
    fn the_round_settles_only_after_every_judge() {
        let mut l = Logged::panel(Some(3));
        l.compete(1);
        for n in 0..3 {
            l.sent(1, n);
        }
        // The lead and judge-3 score; judge-2 has not yet.
        for (n, claude, codex) in [(0, 70, 90), (2, 75, 80)] {
            let out = l.write(1, n, &card_json(n, 1, claude, codex, ""));
            assert_eq!(
                event_names(out.batches.last().unwrap()),
                ["reply_accepted", "scorecard_accepted"]
            );
            assert!(sent_to(&out).is_empty());
        }
        let round = l.current();
        assert_eq!((round.no, round.phase), (1, Phase::Judging));
        assert!(round.outcome.is_none());
        assert_eq!(round.scorecards.len(), 2);

        let out = l.write(1, 1, &card_json(1, 1, 80, 81, ""));
        assert_eq!(event_names(&out.batches[0]), ["brief_started"]);
        assert_eq!(
            event_names(&out.batches[1]),
            [
                "reply_accepted",
                "scorecard_accepted",
                "judge_accepted",
                "message_posted",
                "phase_changed"
            ]
        );
        assert!(matches!(
            out.batches[2][0],
            Event::RoundStarted { no: 2, .. }
        ));
        let session = l.engine.session();
        let r1 = session.round(1).unwrap();
        assert_eq!(r1.phase, Phase::Closed);
        let outcome = r1.outcome.as_ref().unwrap();
        // claude 70/80/75 → 75.0; codex 90/81/80 → 83.7, rounded 84.
        assert_eq!(outcome.evaluations[&PartId::new("codex")].score, 84);
        assert_eq!(outcome.evaluations[&PartId::new("claude")].score, 75);
        assert_eq!(
            outcome.ranking,
            [PartId::new("codex"), PartId::new("claude")]
        );
        assert_eq!(outcome.overview, "[by judge] R1 overview");
        assert_eq!(
            session.score_label(1, &PartId::new("codex")).as_deref(),
            Some("83.7")
        );
        let judgment = session
            .messages
            .iter()
            .find(|m| m.kind == MsgKind::Judgment)
            .unwrap();
        assert_eq!(judgment.author, Author::Part(seat(0)));
        assert_eq!(judgment.body, "[by judge] R1 overview");
        assert_eq!(session.current_round().unwrap().no, 2);
    }

    #[test]
    fn a_rejected_judge_reply_is_corrected_alone() {
        let mut l = Logged::panel(Some(3));
        l.compete(1);
        for n in 0..3 {
            l.sent(1, n);
        }
        let receipts = |l: &Logged| -> Vec<crate::model::types::Receipt> {
            (0..3)
                .map(|n| l.current().brief(&seat(n)).unwrap().receipt.clone())
                .collect()
        };
        let before = receipts(&l);
        let bad = card_json(1, 1, 70, 80, r#", "close": true"#);
        for attempt in 1..=Config::default().max_retries {
            let out = l.write(1, 1, &bad);
            let path = store::judge_reply_path(Path::new(PANEL_DIR), 1, &seat(1));
            assert!(
                out.effects
                    .contains(&Effect::ClearReply { path: path.clone() })
            );
            assert_eq!(sent_to(&out), [seat(1)]);
            let note = body_to(&out, &seat(1));
            assert!(
                note.contains("close: only the lead judge (judge)"),
                "{note}"
            );
            assert!(note.contains(&path.display().to_string()), "{note}");
            let now = receipts(&l);
            assert_eq!(now[1].retries, attempt);
            assert_eq!(now[1].rejected_digest, Some(store::digest(&bad)));
            assert_eq!((&now[0], &now[2]), (&before[0], &before[2]));
            l.sent(1, 1);
        }
        // Out of retries: judge-2's brief alone fails.
        let out = l.write(1, 1, &bad);
        assert!(sent_to(&out).is_empty());
        let now = receipts(&l);
        assert!(now[1].failed.is_some());
        assert!(now[0].failed.is_none() && now[2].failed.is_none());
        assert_eq!((&now[0], &now[2]), (&before[0], &before[2]));
        // The others still score.
        for n in [0, 2] {
            l.write(1, n, &card_json(n, 1, 70, 80, ""));
        }
        assert_eq!(l.current().scorecards.len(), 2);
        assert_eq!(l.current().phase, Phase::Judging);
    }

    #[test]
    fn a_failed_judge_holds_the_round_until_retried() {
        let mut l = Logged::panel(Some(3));
        l.compete(1);
        for n in 0..2 {
            l.sent(1, n);
        }
        // judge-3's pane cannot be reached: every send fails.
        for _ in 0..=Config::default().max_retries {
            l.step(Input::SendFailed {
                round: 1,
                part: seat(2),
                error: "agent pane exited repeatedly".into(),
            });
        }
        assert!(
            l.current()
                .brief(&seat(2))
                .unwrap()
                .receipt
                .failed
                .is_some()
        );
        for n in 0..2 {
            l.write(1, n, &card_json(n, 1, 70, 80, ""));
        }
        // No round is settled without a judge.
        let round = l.current();
        assert_eq!((round.no, round.phase), (1, Phase::Judging));
        assert!(round.outcome.is_none());
        let out = l.step(Input::Tick { now: 10_000_000 });
        assert!(out.batches.is_empty(), "{out:?}");

        let out = l.step(Input::UserRetry);
        assert_eq!(
            out.batches,
            vec![vec![Event::BriefReset {
                round: 1,
                to: seat(2)
            }]]
        );
        assert_eq!(sent_to(&out), [seat(2)]);
        l.sent(1, 2);
        l.write(1, 2, &card_json(2, 1, 72, 78, ""));
        let r1 = l.engine.session().round(1).unwrap();
        assert_eq!(r1.phase, Phase::Closed);
        assert!(r1.outcome.is_some());
        assert_eq!(l.engine.session().rounds.len(), 2);
    }

    #[test]
    fn the_lead_writes_the_verdict_after_the_combined_scores() {
        let mut l = Logged::panel(Some(1));
        l.compete(1);
        let out = l.score_all(1, [(70, 90), (80, 81), (75, 80)]);
        assert_eq!(
            event_names(out.batches.last().unwrap()),
            [
                "reply_accepted",
                "scorecard_accepted",
                "judge_accepted",
                "message_posted",
                "verdict_requested"
            ]
        );
        let verdict_path = store::verdict_reply_path(Path::new(PANEL_DIR), 1);
        assert!(out.effects.contains(&Effect::WatchReply {
            round: 1,
            part: seat(0),
            path: verdict_path.clone(),
        }));
        let body = body_to(&out, &seat(0));
        for line in [
            "## 2. Combined scores (mean of 3 judges)",
            "1. codex — mean 83.7 (judge 90, judge-2 81, judge-3 80; spread 10)",
            "2. claude — mean 75.0 (judge 70, judge-2 80, judge-3 75; spread 10)",
            "Highest mean: codex",
            "[by judge-2] R1 overview",
            "[by judge-3] R1 overview",
            r#"{"winner": "<one of: codex>""#,
        ] {
            assert!(body.contains(line), "missing {line:?} in\n{body}");
        }
        assert!(body.contains(&verdict_path.display().to_string()));
        let session = l.engine.session();
        assert!(!session.closed && session.winner.is_none());
        assert_eq!(session.current_round().unwrap().phase, Phase::Judging);
        let err = l
            .try_feed(Input::UserExtend { by: 1, at: 5_000 })
            .unwrap_err();
        assert!(err.contains("最終講評の待機中です"), "{err}");

        l.step(Input::Sent {
            round: 1,
            part: seat(0),
            at: 5_000,
        });
        // The lead cannot crown an entry the mean did not put first.
        let out = l.write(1, 0, r#"{"winner": "claude", "final_verdict": "claude"}"#);
        assert!(out.effects.contains(&Effect::ClearReply {
            path: verdict_path.clone()
        }));
        assert!(body_to(&out, &seat(0)).contains("does not have the highest mean"));
        assert!(!l.engine.session().closed);

        let verdict = "codex led on every judge's card but judge-2's, by 1 point.";
        let out = l.write(
            1,
            0,
            &format!(r#"{{"winner": "codex", "final_verdict": "{verdict}"}}"#),
        );
        assert_eq!(
            event_names(out.batches.last().unwrap()),
            [
                "reply_accepted",
                "verdict_accepted",
                "message_posted",
                "phase_changed",
                "session_closed"
            ]
        );
        let session = l.engine.session();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("codex")));
        assert_eq!(session.final_verdict(), Some(verdict));
        let message = session.messages.last().unwrap();
        assert_eq!(
            (message.kind, &message.author, message.body.as_str()),
            (
                MsgKind::FinalVerdict,
                &Author::Part(seat(0)),
                format!("優勝: codex\n\n{verdict}").as_str()
            )
        );

        // A tied top: the lead picks either.
        let mut l = Logged::panel(Some(1));
        l.compete(1);
        l.score_all(1, [(80, 81), (81, 80), (80, 80)]);
        let brief = &l.current().verdict_brief.as_ref().unwrap().body;
        assert!(
            brief.contains("Tied for the highest mean: codex, claude"),
            "{brief}"
        );
        l.step(Input::Sent {
            round: 1,
            part: seat(0),
            at: 5_000,
        });
        // The lead ranked codex first, but may name claude as well.
        l.write(
            1,
            0,
            r#"{"winner": "claude", "final_verdict": "claude by a hair"}"#,
        );
        assert_eq!(l.engine.session().winner, Some(PartId::new("claude")));
    }

    #[test]
    fn a_co_judges_close_is_refused_and_the_leads_goes_to_the_verdict() {
        let mut l = Logged::panel(Some(3));
        l.compete(1);
        for n in 0..3 {
            l.sent(1, n);
        }
        let out = l.write(1, 2, &card_json(2, 1, 70, 80, r#", "close": true"#));
        assert!(body_to(&out, &seat(2)).contains("close: only the lead judge"));
        l.sent(1, 2);
        l.write(1, 2, &card_json(2, 1, 70, 80, ""));
        l.write(1, 1, &card_json(1, 1, 72, 79, ""));
        // The lead asks to end early, before the limit.
        let out = l.write(1, 0, &card_json(0, 1, 71, 85, r#", "close": true"#));
        assert!(matches!(
            out.batches.last().unwrap().last(),
            Some(Event::VerdictRequested { round: 1, .. })
        ));
        let body = body_to(&out, &seat(0));
        assert!(
            body.contains("You asked to end the competition early"),
            "{body}"
        );
        assert_eq!(l.engine.session().rounds.len(), 1);
        l.step(Input::Sent {
            round: 1,
            part: seat(0),
            at: 5_000,
        });
        l.write(1, 0, r#"{"winner": "codex", "final_verdict": "codex"}"#);
        let session = l.engine.session();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("codex")));
        assert_eq!(
            session.extendable(),
            Err("ラウンド上限の前に終了したコンペです")
        );
    }

    #[test]
    fn a_round_the_lead_closes_early_is_not_extended_while_judges_still_score() {
        let mut l = Logged::panel(Some(5));
        l.compete(1);
        l.score_all(1, [(70, 80), (72, 79), (74, 78)]);
        l.compete(2);
        for n in 0..3 {
            l.sent(2, n);
        }
        // The lead ends the competition early; judge-2 is still scoring.
        l.write(2, 0, &card_json(0, 2, 71, 85, r#", "close": true"#));
        l.write(2, 2, &card_json(2, 2, 70, 80, ""));
        assert!(l.current().verdict_brief.is_none());
        let err = l
            .try_feed(Input::UserExtend { by: 1, at: 2_100 })
            .unwrap_err();
        assert!(err.contains("リード審判が早期終了を決めています"), "{err}");
        assert_eq!(l.engine.session().max_rounds, Some(5));
        assert!(
            !l.log
                .iter()
                .flatten()
                .any(|e| matches!(e, Event::RoundsExtended { .. }))
        );
        l.write(2, 1, &card_json(1, 2, 72, 79, ""));
        l.step(Input::Sent {
            round: 2,
            part: seat(0),
            at: 2_200,
        });
        l.write(2, 0, r#"{"winner": "codex", "final_verdict": "codex"}"#);
        let session = l.engine.session();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("codex")));

        // The last round's limit message is unchanged by the lead's close.
        let mut l = Logged::panel(Some(1));
        l.compete(1);
        for n in 0..3 {
            l.sent(1, n);
        }
        l.write(1, 0, &card_json(0, 1, 71, 85, r#", "close": true"#));
        let err = l
            .try_feed(Input::UserExtend { by: 1, at: 1_100 })
            .unwrap_err();
        assert!(err.contains("最終ラウンドの進行中です"), "{err}");
    }

    #[test]
    fn a_held_panel_waits_after_the_combined_judgment() {
        let mut l = Logged::panel(Some(3));
        l.step(Input::UserSetHold { hold: true, at: 3 });
        l.compete(1);
        l.sent(1, 0);
        l.write(1, 0, &card_json(0, 1, 70, 80, ""));
        assert!(!l.engine.session().waits_to_continue());
        l.score_all(1, [(70, 80), (70, 80), (70, 80)]);
        let session = l.engine.session();
        assert_eq!(session.rounds.len(), 1);
        assert_eq!(session.round(1).unwrap().phase, Phase::Closed);
        assert!(session.waits_to_continue());
        let out = l.step(Input::UserContinue { at: 9_000 });
        assert_eq!(l.engine.session().rounds.len(), 2);
        assert_eq!(sends(&out), 2);
    }

    #[test]
    fn an_intervention_during_judging_reaches_the_verdict_and_next_briefs() {
        let mut l = Logged::panel(Some(2));
        l.compete(1);
        // Posted after the judge briefs went out: not in them.
        l.step(Input::UserIntervention {
            to: vec![],
            body: "Prefer keyboard control".into(),
            at: 1_500,
        });
        for n in 0..3 {
            let body = &l.current().brief(&seat(n)).unwrap().body;
            assert!(!body.contains("Prefer keyboard control"));
        }
        l.score_all(1, [(70, 80), (72, 79), (74, 78)]);
        let r2 = l.engine.session().round(2).unwrap();
        for brief in &r2.briefs {
            assert!(
                brief.body.contains("Prefer keyboard control"),
                "{}",
                brief.to
            );
        }
        l.compete(2);
        let r2 = l.current();
        for n in 0..3 {
            let body = &r2.brief(&seat(n)).unwrap().body;
            assert!(body.contains("Prefer keyboard control"), "{}", seat(n));
        }
        for n in 0..3 {
            l.sent(2, n);
        }
        l.step(Input::UserIntervention {
            to: vec![seat(0)],
            body: "Weigh accessibility in the verdict".into(),
            at: 2_500,
        });
        for n in 0..3 {
            l.write(2, n, &card_json(n, 2, 70, 80, ""));
        }
        let verdict = &l.current().verdict_brief.as_ref().unwrap().body;
        assert!(
            verdict.contains("to judge] Weigh accessibility in the verdict"),
            "{verdict}"
        );
        assert!(
            verdict.contains("to everyone] Prefer keyboard control"),
            "{verdict}"
        );
    }

    #[test]
    fn a_panel_extended_after_its_verdict_has_a_new_verdict_step() {
        let mut l = Logged::panel(Some(1));
        l.compete(1);
        l.score_all(1, [(70, 80), (72, 79), (74, 78)]);
        l.step(Input::Sent {
            round: 1,
            part: seat(0),
            at: 5_000,
        });
        l.write(1, 0, r#"{"winner": "codex", "final_verdict": "codex"}"#);
        assert!(l.engine.session().closed);
        assert_eq!(l.engine.session().extendable(), Ok(()));

        let out = l.step(Input::UserExtend { by: 1, at: 6_000 });
        assert_eq!(sends(&out), 2);
        let session = l.engine.session();
        assert!(!session.closed);
        assert_eq!(session.winner, None);
        // The verdict stays with its round.
        let r1 = session.round(1).unwrap().outcome.as_ref().unwrap();
        assert_eq!(r1.winner, Some(PartId::new("codex")));

        l.compete(2);
        for n in 0..3 {
            let body = &l.current().brief(&seat(n)).unwrap().body;
            let lead = n == 0;
            assert_eq!(
                body.contains("This is the FINAL round"),
                lead,
                "{}",
                seat(n)
            );
        }
        let out = l.score_all(2, [(85, 80), (84, 79), (80, 81)]);
        assert!(matches!(
            out.batches.last().unwrap().last(),
            Some(Event::VerdictRequested { round: 2, .. })
        ));
        assert!(body_to(&out, &seat(0)).contains("Highest mean: claude"));
        l.step(Input::Sent {
            round: 2,
            part: seat(0),
            at: 9_000,
        });
        l.write(2, 0, r#"{"winner": "claude", "final_verdict": "claude"}"#);
        let session = l.engine.session();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("claude")));
        assert_eq!(session.final_verdict(), Some("claude"));
    }

    #[test]
    fn resume_mid_judging_briefs_only_judges_without_a_scorecard() {
        let mut l = Logged::panel(Some(3));
        l.compete(1);
        // The lead scored; judge-2 took its brief; judge-3's never went out.
        l.sent(1, 0);
        l.write(1, 0, &card_json(0, 1, 70, 80, ""));
        l.sent(1, 1);
        let stored = reduce::replay(&l.log).unwrap();
        let mut resumed = Engine::resume(stored, Path::new(PANEL_DIR), Config::default());
        let out = resumed.handle(Input::Start { now: 50_000 }).unwrap();
        assert!(out.batches.is_empty(), "{out:?}");
        assert_eq!(sent_to(&out), [seat(2)]);
        let watched: Vec<&PartId> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::WatchReply { part, .. } => Some(part),
                _ => None,
            })
            .collect();
        assert_eq!(watched, [&seat(1), &seat(2)]);
        // judge-2's brief was lost with its pane: it goes out again as it
        // was, without counting a retry.
        let out = resumed
            .handle(Input::Redeliver {
                round: 1,
                part: seat(1),
            })
            .unwrap();
        assert!(out.batches.is_empty());
        let brief = resumed.session().round(1).unwrap().brief(&seat(1)).unwrap();
        assert_eq!(body_to(&out, &seat(1)), brief.body);
        assert_eq!(brief.receipt.retries, 0);
        // The lead's accepted card is not asked for again.
        let out = resumed
            .handle(Input::Redeliver {
                round: 1,
                part: seat(0),
            })
            .unwrap();
        assert!(out.effects.is_empty());
        // judge-2 has its full time again from the resume.
        let start = Config::default().start_timeout;
        let out = resumed
            .handle(Input::Tick {
                now: 50_000 + start - 1,
            })
            .unwrap();
        assert!(out.batches.is_empty(), "{out:?}");
    }

    #[test]
    fn resume_while_the_verdict_is_pending_sends_it() {
        let mut l = Logged::panel(Some(1));
        l.compete(1);
        l.score_all(1, [(70, 80), (72, 79), (74, 78)]);
        let verdict_path = store::verdict_reply_path(Path::new(PANEL_DIR), 1);
        // Stopped before the verdict brief went out.
        let stored = reduce::replay(&l.log).unwrap();
        let mut resumed = Engine::resume(stored, Path::new(PANEL_DIR), Config::default());
        let out = resumed.handle(Input::Start { now: 50_000 }).unwrap();
        assert_eq!(sent_to(&out), [seat(0)]);
        let verdict = resumed
            .session()
            .round(1)
            .unwrap()
            .verdict_brief
            .clone()
            .unwrap();
        assert_eq!(body_to(&out, &seat(0)), verdict.body);
        assert_eq!(
            out.effects
                .iter()
                .filter(|e| matches!(e, Effect::WatchReply { .. }))
                .collect::<Vec<_>>(),
            [&Effect::WatchReply {
                round: 1,
                part: seat(0),
                path: verdict_path.clone(),
            }]
        );
        // Stopped after it went out: watched, not pasted again.
        l.step(Input::Sent {
            round: 1,
            part: seat(0),
            at: 5_000,
        });
        let stored = reduce::replay(&l.log).unwrap();
        let mut resumed = Engine::resume(stored, Path::new(PANEL_DIR), Config::default());
        let out = resumed.handle(Input::Start { now: 50_000 }).unwrap();
        assert_eq!(
            out.effects,
            [Effect::WatchReply {
                round: 1,
                part: seat(0),
                path: verdict_path,
            }]
        );
        resumed
            .handle(Input::ReplyFileWritten {
                round: 1,
                part: seat(0),
                content: r#"{"winner": "codex", "final_verdict": "codex"}"#.into(),
                artifacts: vec![],
                at: 60_000,
            })
            .unwrap();
        assert!(resumed.session().closed);
        assert_eq!(resumed.session().winner, Some(PartId::new("codex")));
    }

    /// The v0.9.15 log cut after round 2's judge brief went out resumes as
    /// that version did: the brief is watched, sent again only when its
    /// pane was lost, and the judge's reply writes the very lines the full
    /// log holds.
    #[test]
    fn a_single_judge_log_cut_while_judging_resumes_as_before() {
        const FULL: &str = include_str!("model/testdata/single-judge-v0.9.15.events.jsonl");
        const CUT: &str = include_str!("model/testdata/single-judge-v0.9.15-judging.events.jsonl");
        let dir = Path::new("/tmp/competitions/comp-1790000000000");
        let read = |text: &str| -> Vec<Vec<Event>> {
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        };
        let session = reduce::replay(read(CUT)).unwrap();
        assert!(!session.is_panel());
        let judge = PartId::new(JUDGE_ID);
        let mut engine = Engine::resume(session, dir, Config::default());
        let out = engine
            .handle(Input::Start {
                now: 1_790_000_250_000,
            })
            .unwrap();
        assert!(out.batches.is_empty());
        assert_eq!(
            out.effects,
            [Effect::WatchReply {
                round: 2,
                part: judge.clone(),
                path: dir.join("r2").join(store::JUDGE_FILE),
            }]
        );
        let out = engine
            .handle(Input::Redeliver {
                round: 2,
                part: judge.clone(),
            })
            .unwrap();
        let brief = engine.session().round(2).unwrap().brief(&judge).unwrap();
        assert!(out.batches.is_empty());
        assert_eq!(body_to(&out, &judge), brief.body);

        let reply = r#"{
  "overview": "claude closed the gap and now leads.",
  "evaluations": {
    "claude": { "score": 88, "strengths": "Queue and keyboard", "feedback": "Polish the empty queue state." },
    "codex": { "score": 84, "strengths": "Solid on small screens", "feedback": "The queue drawer hides the scrubber." }
  },
  "ranking": ["claude", "codex"],
  "winner": "claude",
  "final_verdict": "claude met every checklist item and answered the feedback; codex's drawer still hides the scrubber.",
  "close": true,
  "amendments": [ { "round": 1, "contestant": "claude", "score": 76, "reason": "R1 was judged without its stylesheet" } ]
}"#;
        let out = engine
            .handle(Input::ReplyFileWritten {
                round: 2,
                part: judge,
                content: reply.into(),
                artifacts: vec![],
                at: 1_790_000_260_000,
            })
            .unwrap();
        let written: Vec<String> = out
            .batches
            .iter()
            .map(|b| serde_json::to_string(b).unwrap())
            .collect();
        let rest: Vec<&str> = FULL.lines().skip(CUT.lines().count()).collect();
        assert_eq!(written, rest);
        assert_eq!(*engine.session(), reduce::replay(read(FULL)).unwrap());
    }
}
