//! Runs one review against a live Noa, on a thread of its own.
//!
//! Captures the target into its snapshot, opens a Noa tab for the review,
//! spawns the lead and every reviewer as interactive agents in splits, feeds
//! deliveries, agent states, reply files and user actions to the [`Engine`],
//! and rewrites `report.md` after each consolidated round.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review::engine::{AgentState, Config, Effect, Engine, EngineError, Input, Setup};
use review::model::export;
use review::model::types::{
    AgentChoice, AgentKind, AgentSetup, Millis, PartId, Phase, ReviewerChoice, RoundNo, Session,
    SessionId, Target, TargetKind,
};
use review::store::{self, Store};
use review::task_links::{self, LinkEvent, Origin, SessionKind};
use review::timeline::{self, Publisher, RunSpec, Status, TimelineApi};

use super::ipc::{
    Changes, Connect, IpcConnect, Noa, NoaError, PaneSpec, PaneView, Placement, no_board_tab,
};

const POLL: Duration = Duration::from_millis(200);
const TICK_EVERY: Millis = 1_000;
const BACKOFF_MIN: Millis = 500;
const BACKOFF_MAX: Millis = 8_000;
const SUBMIT_DELAY: Duration = Duration::from_millis(150);
const QUIET_FOR: Millis = 1_500;
const READY_LIMIT: Millis = 30_000;
const CLAUDE_TRUST_DETAIL: &str = "trust this folder";
const CLAUDE_TRUST_YES: &str = "Yes, I trust this folder";
const TRUST_KEYS_MAX: u8 = 4;
const TRUST_KEY_SHOWS_IN: Millis = 2_000;
const REPLY_STABLE_FOR: Millis = 600;
const MAX_SPAWNS: u8 = 2;
/// The largest brief pasted as it is, measured JSON-encoded as it goes in
/// a `noa.sendText` frame: Noa's control server takes frames of at most
/// 256 KiB (`crates/noa-ipc/src/server.rs`, `MAX_WS_FRAME_SIZE`), so a
/// longer brief is written to a file in the round's folder and the agent
/// is sent where to read it.
const BRIEF_PASTE_MAX: usize = 128 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    New(Box<NewReview>),
    /// Continues the review logged in `dir` (`Store::open` +
    /// `Engine::resume`): agents still in their panes are taken up again,
    /// the others are started anew.
    Resume {
        dir: PathBuf,
    },
    /// Gives the review logged in `dir` `by` more rounds, then goes on
    /// as [`Start::Resume`]: a finished review that ran out of rounds
    /// continues. The extension is stored before any pane is started, and
    /// a refused one starts none.
    Extend {
        dir: PathBuf,
        by: RoundNo,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    Retry,
    Close,
    /// Brings `part`'s pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Sends the review's timeline posts that are not published again.
    RetryTimeline,
    /// Holds the review after each consolidated round, or lets it go on.
    SetHold {
        hold: bool,
    },
    /// Starts the next round of a held review.
    Continue,
    /// Raises the round limit by `by`.
    Extend {
        by: RoundNo,
    },
    /// Closes every reviewer's and the lead's pane in Noa. Refused until
    /// the review has ended, so no brief is cut off. A pane opened for a
    /// fix request is the user's and stays.
    ClosePanes,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Session {
        session: Arc<Session>,
        reply_root: PathBuf,
        session_dir: PathBuf,
        origin: Option<Origin>,
    },
    Link(Link),
    Notice(String),
    /// The version of the Noa this runner just connected to (each
    /// connection, so a reconnect to another Noa shows), with the number
    /// of that connection's hello.
    Version(u64, String),
    /// Where the review's Board Timeline posts stand.
    Timeline(Status),
    Ended(Result<(), String>),
}

pub struct SessionHandle {
    pub updates: Receiver<Update>,
    actions: Sender<Action>,
}

impl SessionHandle {
    pub fn actions(&self) -> Sender<Action> {
        self.actions.clone()
    }
}

pub fn spawn_session(start: Start, wake: impl Fn() + Send + 'static) -> SessionHandle {
    let (update_tx, updates) = mpsc::channel();
    let (actions, action_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("review-live".into())
        .spawn(move || {
            run_session(
                start,
                IpcConnect,
                SystemClock,
                RunnerConfig {
                    engine: Config::default(),
                    tasks_root: task_links::default_root(),
                    timeline: noa_config::timeline_outbox_dir().map(TimelineLink::system),
                },
                action_rx,
                update_tx,
                wake,
            )
        })
        .expect("spawn review-live thread");
    SessionHandle { updates, actions }
}

pub struct RunnerConfig {
    pub engine: Config,
    pub tasks_root: Option<PathBuf>,
    /// Where the review posts its verdicts on its board's timeline; `None`
    /// posts nothing.
    pub timeline: Option<TimelineLink>,
}

/// How the runner reaches the Board Timeline: the outbox root
/// (`noa_config::timeline_outbox_dir()`) and a connection of its own (the
/// agents' [`Noa`] link knows nothing of the timeline).
pub struct TimelineLink {
    pub outbox_root: PathBuf,
    pub connect: Box<dyn FnMut() -> Result<Box<dyn TimelineApi>, String>>,
}

impl TimelineLink {
    /// The running Noa's control server.
    pub fn system(outbox_root: PathBuf) -> Self {
        Self {
            outbox_root,
            connect: Box::new(|| {
                timeline::connect().map(|client| Box::new(client) as Box<dyn TimelineApi>)
            }),
        }
    }
}

/// The runner's side of the timeline: the publisher, opened once the
/// server is known to have the timeline (or its outbox is there from
/// before), and its connection, dropped after a failure.
struct Timeline {
    link: TimelineLink,
    publisher: Option<Publisher>,
    api: Option<Box<dyn TimelineApi>>,
    /// How many confirmed points the last attempt covered.
    tried: usize,
    /// The board pane the run was last registered (or tried) from.
    offered: Option<u64>,
    status: Status,
}

impl Timeline {
    fn new(link: TimelineLink) -> Self {
        Self {
            link,
            publisher: None,
            api: None,
            tried: 0,
            offered: None,
            status: Status::Idle,
        }
    }

    /// Whether some of `candidates` are not published, as far as the
    /// outbox says (an outbox not made yet has nothing published).
    fn unsent(&mut self, session: &str, candidates: &[timeline::Candidate]) -> bool {
        if self.publisher.is_none()
            && let Ok(existing) = Publisher::existing(&self.link.outbox_root, session)
        {
            self.publisher = existing;
        }
        !candidates.is_empty()
            && self
                .publisher
                .as_ref()
                .is_none_or(|p| !matches!(p.status(candidates), Status::Published { .. }))
    }

    /// Connects when not connected and opens the publisher once the server
    /// has the timeline, then sends `candidates`, or with none registers
    /// the run from `board`.
    fn attempt(
        &mut self,
        session: &str,
        board: Option<u64>,
        candidates: &[timeline::Candidate],
    ) -> Result<Status, String> {
        if self.api.is_none() {
            self.api = Some((self.link.connect)()?);
        }
        let api = self.api.as_mut().expect("connected above");
        if !api.supports() {
            return Ok(Status::Idle);
        }
        if self.publisher.is_none() {
            let publisher = Publisher::open(&self.link.outbox_root, session, RunSpec::default())
                .map_err(|e| format!("outbox: {e}"))?;
            self.publisher = Some(publisher);
        }
        let publisher = self.publisher.as_mut().expect("opened above");
        Ok(if candidates.is_empty() {
            publisher.register(api.as_mut(), board)
        } else {
            publisher.sync(api.as_mut(), board, candidates)
        })
    }
}

pub fn run_session<C: Connect, K: Clock>(
    start: Start,
    connector: C,
    clock: K,
    config: RunnerConfig,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: impl Fn() + 'static,
) {
    let wake: Rc<dyn Fn()> = Rc::new(wake);
    let opened = Runner::open(
        start,
        connector,
        clock,
        config,
        actions,
        updates.clone(),
        wake.clone(),
    );
    let result = opened.and_then(|mut runner| runner.run());
    let _ = updates.send(Update::Ended(result));
    wake();
}

pub trait Clock {
    fn now(&self) -> Millis;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Millis {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as Millis)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn agent_argv(kind: AgentKind, setup: &AgentSetup) -> Vec<String> {
    // Plain words only (`AgentSetup::check`, enforced when the session is
    // created), written into the script itself: `"$@"` is not valid in
    // every login shell.
    let script = std::iter::once(format!("exec {}", kind.command()))
        .chain(kind.flags(setup))
        .collect::<Vec<_>>()
        .join(" ");
    vec![login_shell(), "-l".into(), "-i".into(), "-c".into(), script]
}

pub(crate) fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

fn agent_state(state: Option<&str>) -> Option<AgentState> {
    Some(match state? {
        "running" => AgentState::Running,
        "permission" => AgentState::Permission,
        "input" => AgentState::Input,
        "finished" => AgentState::Finished,
        "error" => AgentState::Error,
        _ => return None,
    })
}

fn in_play(session: &Session, round: RoundNo, part: &PartId) -> bool {
    if session.closed {
        return false;
    }
    let Some(current) = session.current_round() else {
        return false;
    };
    if current.no != round {
        return false;
    }
    let phase_ok = if *part == session.lead.id {
        current.phase == Phase::Consolidating
    } else {
        matches!(current.phase, Phase::Briefing | Phase::Reviewing)
    };
    phase_ok
        && current
            .brief(part)
            .is_some_and(|b| b.receipt.replied.is_none() && b.receipt.failed.is_none())
}

enum Fail {
    Link(String),
    Fatal(String),
}

impl From<String> for Fail {
    fn from(why: String) -> Self {
        Fail::Fatal(why)
    }
}

fn link(err: NoaError) -> Fail {
    match err {
        NoaError::Link(why) => Fail::Link(why),
        other => Fail::Link(other.to_string()),
    }
}

#[derive(Debug, Clone)]
struct Pane {
    id: u64,
    ready: bool,
    seq: u64,
    state: Option<String>,
    blocked: Option<String>,
    detail: Option<String>,
    quiet: Option<(String, Millis)>,
    told_held: bool,
    briefed: bool,
    trust_keys: u8,
    trust_screen: Option<(String, Millis)>,
}

impl Pane {
    fn new(id: u64) -> Self {
        Self {
            id,
            ready: false,
            seq: 0,
            state: None,
            blocked: None,
            detail: None,
            quiet: None,
            told_held: false,
            briefed: false,
            trust_keys: 0,
            trust_screen: None,
        }
    }

    fn from_view(view: &PaneView) -> Self {
        let mut pane = Self::new(view.pane_id);
        pane.seq = view.agent_seq;
        pane.state.clone_from(&view.agent_state);
        pane.blocked.clone_from(&view.agent_blocked);
        pane.detail.clone_from(&view.agent_detail);
        pane.ready = pane.reports_ready();
        pane
    }

    fn reports_ready(&self) -> bool {
        matches!(
            (self.state.as_deref(), self.blocked.as_deref()),
            (Some("input"), None) | (Some("finished"), _)
        )
    }

    fn held(&self) -> bool {
        matches!(self.state.as_deref(), Some("permission"))
            || self.blocked.is_some()
            || (!self.briefed && self.state.as_deref() == Some("error"))
    }

    /// Whether the agent's last report says it is working on a turn.
    fn busy(&self) -> bool {
        self.state.as_deref() == Some("running")
    }

    fn is_claude_trust(&self) -> bool {
        self.blocked.as_deref() == Some("permission")
            && self
                .detail
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(CLAUDE_TRUST_DETAIL))
    }
}

#[derive(Debug, Clone)]
struct PendingSend {
    round: RoundNo,
    part: PartId,
    body: String,
    queued_at: Millis,
    spawns: u8,
}

#[derive(Debug, Clone)]
struct WatchedReply {
    round: RoundNo,
    part: PartId,
    path: PathBuf,
    /// What the file held when last read (its bytes, or why it could not
    /// be read) and since when.
    seen: Option<(Result<Vec<u8>, String>, Millis)>,
}

struct Runner<C: Connect, K: Clock> {
    engine: Engine,
    store: Store,
    origin: Option<Origin>,
    last_outcome: Option<(Option<&'static str>, bool)>,
    link_notice_shown: bool,
    connector: C,
    clock: K,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: Rc<dyn Fn()>,
    panes: BTreeMap<PartId, Pane>,
    /// Panes asked to close whose outcome Noa could not tell (a timeout,
    /// a lost link): when one closes after all, it goes without a notice.
    closing: BTreeSet<u64>,
    /// The board tab's pane, where every agent of this review floats: opened
    /// with the first agent, taken from an agent's `parent_pane_id` on a
    /// resume, forgotten once it is gone.
    board: Option<u64>,
    sends: Vec<PendingSend>,
    watches: Vec<WatchedReply>,
    last_tick: Millis,
    stop: bool,
    started: bool,
    /// Opened from a log: agents whose panes are gone are briefed again
    /// once the first connection shows which are.
    resumed: bool,
    link_state: Link,
    timeline: Option<Timeline>,
}

impl<C: Connect, K: Clock> Runner<C, K> {
    fn open(
        start: Start,
        connector: C,
        clock: K,
        config: RunnerConfig,
        actions: Receiver<Action>,
        updates: Sender<Update>,
        wake: Rc<dyn Fn()>,
    ) -> Result<Self, String> {
        let now = clock.now();
        let resumed = matches!(start, Start::Resume { .. } | Start::Extend { .. });
        let (engine, store) = match start {
            Start::New(new) => new_review(*new, config.engine, now)?,
            Start::Resume { dir } => open_log(dir, None, config.engine, now)?,
            Start::Extend { dir, by } => open_log(dir, Some(by), config.engine, now)?,
        };
        let (origin, link_error) = match config.tasks_root {
            Some(root) => match session_origin(&root, engine.session(), resumed, now) {
                Ok(origin) => (origin, None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let mut runner = Self {
            engine,
            store,
            origin,
            last_outcome: None,
            link_notice_shown: false,
            connector,
            clock,
            actions,
            updates,
            wake,
            panes: BTreeMap::new(),
            closing: BTreeSet::new(),
            board: None,
            sends: Vec::new(),
            watches: Vec::new(),
            last_tick: now,
            stop: false,
            started: false,
            resumed,
            link_state: Link::Connecting,
            timeline: config.timeline.map(Timeline::new),
        };
        runner.emit_session();
        if let Some(error) = link_error {
            runner.link_error(error);
        }
        runner.record_outcome();
        runner.emit_link(Link::Connecting);
        Ok(runner)
    }

    fn emit(&self, update: Update) {
        let _ = self.updates.send(update);
        (self.wake)();
    }

    fn emit_session(&self) {
        self.emit(Update::Session {
            session: Arc::new(self.engine.session().clone()),
            reply_root: self.engine.reply_dir(),
            session_dir: self.engine.dir().to_owned(),
            origin: self.origin.clone(),
        });
    }

    fn emit_link(&self, state: Link) {
        self.emit(Update::Link(state));
    }

    fn set_link(&mut self, state: Link) {
        if self.link_state != state {
            self.link_state = state.clone();
            self.emit_link(state);
        }
    }

    fn notice(&self, text: impl Into<String>) {
        self.emit(Update::Notice(text.into()));
    }

    fn link_error(&mut self, error: String) {
        let message = format!("タスクとの関連を記録できません: {error}");
        eprintln!("{message}");
        if !self.link_notice_shown {
            self.link_notice_shown = true;
            self.notice(message);
        }
    }

    fn record_outcome(&mut self) {
        let Some(origin) = &self.origin else { return };
        let session = self.engine.session();
        let decision = session.verdict().map(|v| v.decision.name());
        let outcome = (decision, session.closed);
        if self.last_outcome == Some(outcome) {
            return;
        }
        match task_links::record_outcome(
            origin,
            SessionKind::Review,
            session.id.as_str(),
            decision,
            session.closed,
            self.clock.now(),
        ) {
            Ok(()) => self.last_outcome = Some(outcome),
            Err(error) => self.link_error(error.to_string()),
        }
    }

    fn run(&mut self) -> Result<(), String> {
        let mut backoff = BACKOFF_MIN;
        while !self.stop {
            self.drain_actions(None)?;
            if self.stop {
                break;
            }
            match self.connector.connect() {
                Ok(mut conn) => {
                    backoff = BACKOFF_MIN;
                    self.set_link(Link::Live);
                    match self.serve(&mut conn) {
                        Ok(()) => break,
                        Err(Fail::Link(why)) => {
                            self.set_link(Link::Down(why));
                        }
                        Err(Fail::Fatal(why)) => return Err(why),
                    }
                }
                Err(why) => {
                    self.set_link(Link::Down(why));
                    self.sleep_checking_actions(backoff)?;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
        Ok(())
    }

    fn sleep_checking_actions(&mut self, mut ms: Millis) -> Result<(), String> {
        while ms > 0 && !self.stop {
            let step = ms.min(POLL.as_millis() as Millis);
            self.clock.sleep(Duration::from_millis(step));
            ms = ms.saturating_sub(step);
            self.drain_actions(None)?;
        }
        Ok(())
    }

    fn serve(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        self.begin(conn)?;
        while !self.stop {
            self.drain_actions(Some(conn))?;
            if self.stop {
                break;
            }
            self.deliver_sends(conn)?;
            self.poll_replies()?;
            let now = self.clock.now();
            if now.saturating_sub(self.last_tick) >= TICK_EVERY {
                self.last_tick = now;
                self.feed(Input::Tick { now })?;
            }
            if let Some(changes) = conn.changes(POLL).map_err(link)? {
                self.apply_changes(conn, changes)?;
            }
            // A board tab opened since registers the run from it.
            self.publish_timeline(false);
        }
        Ok(())
    }

    /// On each connection: takes up the panes; on the first, starts (or
    /// resumes) the review.
    fn begin(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        self.emit(Update::Version(conn.hello_seq(), conn.server_version()));
        self.sync_panes(conn)?;
        if !self.started {
            self.started = true;
            let now = self.clock.now();
            self.feed(Input::Start { now })?;
            if self.resumed {
                self.take_up_states(now)?;
                self.clear_rejected_leftovers();
                self.redeliver_lost()?;
            }
        }
        // A post staged before a restart or a lost link goes out now.
        self.publish_timeline(true);
        Ok(())
    }

    /// Registers the run on the board's timeline once a board pane names
    /// the board (the first connection, a new board tab), and posts the
    /// confirmed verdicts not there yet: on a new one (or with `again`,
    /// every one not published). After the source event is in the log; a
    /// failure only changes what the header says.
    fn publish_timeline(&mut self, again: bool) {
        let Some(mut tl) = self.timeline.take() else {
            return;
        };
        let session = self.engine.session();
        let confirmed = timeline::confirmed(session);
        let new_board = self.board.is_some() && self.board != tl.offered;
        if again || new_board || confirmed != tl.tried {
            tl.tried = confirmed;
            if self.board.is_some() {
                tl.offered = self.board;
            }
            let id = session.id.as_str().to_owned();
            let candidates = if confirmed == 0 {
                Vec::new()
            } else {
                timeline::candidates(session)
            };
            // The header says pending while the post is on its way.
            if tl.unsent(&id, &candidates) && tl.status != Status::Pending {
                tl.status = Status::Pending;
                self.emit(Update::Timeline(Status::Pending));
            }
            let status = tl
                .attempt(&id, self.board, &candidates)
                .unwrap_or_else(|message| Status::Failed { message });
            if status.failed() {
                // Reconnects on the next attempt.
                tl.api = None;
            }
            // A registration alone shows nothing: the next post tries again.
            let status = if candidates.is_empty() {
                Status::Idle
            } else {
                status
            };
            if status != tl.status {
                tl.status = status.clone();
                self.emit(Update::Timeline(status));
            }
        }
        self.timeline = Some(tl);
    }

    fn sync_panes(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let views = conn.panes().map_err(link)?;
        let by_id: BTreeMap<u64, &PaneView> = views.iter().map(|v| (v.pane_id, v)).collect();
        let mut alive = BTreeMap::new();
        // States the engine has not seen: a full sync follows lost or
        // dropped notifications, and its timers must hear what they said.
        let mut states = Vec::new();
        let mut adopted_board = None;
        for part in self.all_parts() {
            let label = self.pane_label(&part);
            let known_id = self.panes.get(&part).map(|p| p.id).or_else(|| {
                self.engine
                    .session()
                    .participant(&part)
                    .and_then(|p| p.pane_id)
            });
            // A pane id from the log names this agent's pane only while it
            // still carries the label it was started with: ids are reused.
            let found = known_id
                .and_then(|id| by_id.get(&id).copied())
                .filter(|v| v.label.as_deref() == Some(label.as_str()))
                .or_else(|| {
                    views
                        .iter()
                        .find(|v| v.label.as_deref() == Some(label.as_str()))
                });
            if let Some(view) = found {
                if let Some(parent) = view.parent_pane_id.filter(|p| by_id.contains_key(p)) {
                    adopted_board.get_or_insert(parent);
                }
                let known = self.panes.get(&part).filter(|p| p.id == view.pane_id);
                let changed =
                    known.is_none_or(|p| p.seq != view.agent_seq || p.state != view.agent_state);
                if changed && let Some(state) = agent_state(view.agent_state.as_deref()) {
                    states.push((part.clone(), state));
                }
                let mut pane = known.cloned().unwrap_or_else(|| Pane::from_view(view));
                pane.seq = view.agent_seq;
                pane.state.clone_from(&view.agent_state);
                pane.blocked.clone_from(&view.agent_blocked);
                pane.detail.clone_from(&view.agent_detail);
                if pane.reports_ready() {
                    pane.ready = true;
                }
                alive.insert(part, pane);
            }
        }
        self.panes = alive;
        // The board is the one a pane taken up floats on, else the one known
        // before, while it is still listed.
        self.board = adopted_board.or(self.board.filter(|b| by_id.contains_key(b)));
        // Panes gone while the link was down need no quiet closing.
        let live: BTreeSet<u64> = self.panes.values().map(|p| p.id).collect();
        self.closing.retain(|id| live.contains(id));
        let at = self.clock.now();
        for (part, state) in states {
            self.feed(Input::AgentStateChanged { part, state, at })?;
        }
        Ok(())
    }

    fn apply_changes(&mut self, conn: &mut C::Conn, changes: Changes) -> Result<(), Fail> {
        if changes.dropped {
            self.sync_panes(conn)?;
            return Ok(());
        }
        let now = self.clock.now();
        for removed in changes.removed {
            if self.board == Some(removed) {
                self.board = None;
            }
            let part = self
                .panes
                .iter()
                .find(|(_, p)| p.id == removed)
                .map(|(k, _)| k.clone());
            if let Some(part) = part {
                self.panes.remove(&part);
                if !self.closing.remove(&removed) {
                    self.notice(format!("{part} のペインが閉じられました"));
                }
            }
        }
        for view in changes.panes {
            let part = self
                .panes
                .iter()
                .find(|(_, p)| p.id == view.pane_id)
                .map(|(k, _)| k.clone());
            let Some(part) = part else {
                continue;
            };
            let mut changed_state = None;
            if let Some(pane) = self.panes.get_mut(&part) {
                let state_changed = pane.seq != view.agent_seq || pane.state != view.agent_state;
                pane.seq = view.agent_seq;
                pane.state.clone_from(&view.agent_state);
                pane.blocked.clone_from(&view.agent_blocked);
                pane.detail.clone_from(&view.agent_detail);
                if pane.reports_ready() {
                    pane.ready = true;
                    pane.told_held = false;
                }
                if state_changed {
                    changed_state = agent_state(view.agent_state.as_deref());
                }
            }
            if let Some(state) = changed_state {
                self.feed(Input::AgentStateChanged {
                    part,
                    state,
                    at: now,
                })?;
            }
        }
        Ok(())
    }

    fn drain_actions(&mut self, mut conn: Option<&mut C::Conn>) -> Result<(), String> {
        loop {
            match self.actions.try_recv() {
                Ok(Action::Stop) => {
                    self.stop = true;
                    return Ok(());
                }
                Ok(Action::Intervene { to, body }) => {
                    let at = self.clock.now();
                    self.feed_user(Input::UserIntervention { to, body, at })?;
                }
                Ok(Action::Retry) => {
                    self.feed_user(Input::UserRetry)?;
                }
                Ok(Action::Close) => {
                    self.feed_user(Input::UserClose)?;
                }
                Ok(Action::Focus { part }) => {
                    self.focus_pane(conn.as_deref_mut(), &part);
                }
                Ok(Action::RetryTimeline) => self.publish_timeline(true),
                Ok(Action::ClosePanes) => {
                    self.close_panes(conn.as_deref_mut());
                }
                Ok(Action::SetHold { hold }) => {
                    let at = self.clock.now();
                    self.feed_user(Input::UserSetHold { hold, at })?;
                }
                Ok(Action::Continue) => {
                    let at = self.clock.now();
                    self.feed_user(Input::UserContinue { at })?;
                }
                Ok(Action::Extend { by }) => {
                    let at = self.clock.now();
                    self.feed_user(Input::UserExtend { by, at })?;
                }
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.stop = true;
                    return Ok(());
                }
            }
        }
    }

    fn focus_pane(&mut self, conn: Option<&mut C::Conn>, part: &PartId) {
        let Some(conn) = conn else {
            self.notice("Noa に接続されていません");
            return;
        };
        let Some(pane) = self.panes.get(part).map(|p| p.id) else {
            self.notice(format!(
                "{part} のペインがありません（まだ起動していないか、閉じられています）"
            ));
            return;
        };
        match conn.focus(pane) {
            Ok(()) => {}
            Err(NoaError::PaneGone) => {
                self.panes.remove(part);
                self.notice(format!("{part} のペインは閉じられています"));
            }
            Err(e) => self.notice(format!("{part} のペインに移動できません: {e}")),
        }
    }

    /// Closes the agents' panes of an ended review. A pane is forgotten
    /// before Noa reports it gone, so its closing is not told as a pane the
    /// user closed. Should the review go on (+N), its agents start again in
    /// a new board tab ([`Self::spawn_pane`]). The board goes with them.
    fn close_panes(&mut self, conn: Option<&mut C::Conn>) {
        if !self.engine.session().closed {
            self.notice("ペインはレビューが終わってから閉じられます");
            return;
        }
        if self.panes.is_empty() && self.board.is_none() {
            self.notice("閉じるペインはありません");
            return;
        }
        let Some(conn) = conn else {
            self.notice("Noa に接続されていません");
            return;
        };
        let mut failed = Vec::new();
        let mut closed = 0;
        for (part, pane) in std::mem::take(&mut self.panes) {
            match conn.close_pane(pane.id) {
                Ok(()) | Err(NoaError::PaneGone) => closed += 1,
                // Noa may still close it (a timeout says nothing of the
                // outcome): kept until it is reported gone.
                Err(err) => {
                    failed.push(format!("{part}: {err}"));
                    self.closing.insert(pane.id);
                    self.panes.insert(part, pane);
                }
            }
        }
        // The board only once its agents are closed: a float it still holds
        // would go with it unreported.
        let mut board_failed = false;
        if failed.is_empty()
            && let Some(board) = self.board
        {
            match conn.close_pane(board) {
                Ok(()) | Err(NoaError::PaneGone) => self.board = None,
                Err(err) => {
                    board_failed = true;
                    self.notice(format!("ボードタブを閉じられません: {err}"));
                }
            }
        }
        if failed.is_empty() {
            if !board_failed {
                self.notice(format!("{closed} 個のペインとボードタブを閉じました"));
            }
        } else {
            self.notice(format!(
                "{closed} 個のペインを閉じました。閉じたか確かめられなかったもの: {}",
                failed.join(", ")
            ));
        }
    }

    /// After a resume: what the agents taken up are doing now reaches the
    /// engine, so a turn started while the review was stopped counts
    /// as started, and a prompt waiting for the user holds the time limit.
    fn take_up_states(&mut self, now: Millis) -> Result<(), Fail> {
        let states: Vec<(PartId, AgentState)> = self
            .panes
            .iter()
            .filter_map(|(part, pane)| Some((part.clone(), agent_state(pane.state.as_deref())?)))
            .filter(|(_, state)| matches!(state, AgentState::Running | AgentState::Permission))
            .collect();
        for (part, state) in states {
            self.feed(Input::AgentStateChanged {
                part,
                state,
                at: now,
            })?;
        }
        Ok(())
    }

    /// After a resume: a reply turned down just before the stop may still
    /// be on disk (the log took the rejection, the file was not cleared
    /// yet). Taking it in again would turn it down twice, so it goes now.
    fn clear_rejected_leftovers(&self) {
        let Some(round) = self.engine.session().current_round() else {
            return;
        };
        let base = self.engine.reply_base();
        for brief in round.briefs.iter().chain(&round.lead_brief) {
            let Some(digest) = brief.receipt.rejected_digest else {
                continue;
            };
            if brief.receipt.sent.is_none()
                && let Ok(Some(bytes)) = store::read_reply(&base, &brief.reply_path)
                && review::capture::digest_bytes(&bytes) == digest
            {
                let _ = store::clear_reply(&base, &brief.reply_path);
            }
        }
    }

    /// After a resume: a brief that went out before the review
    /// stopped, to an agent whose pane is gone and that left no reply, is
    /// sent again as it was (no retry is counted: the stop lost it, not
    /// the agent), which starts the agent in a new pane.
    fn redeliver_lost(&mut self) -> Result<(), Fail> {
        let Some(round) = self.engine.session().current_round() else {
            return Ok(());
        };
        let no = round.no;
        let lost: Vec<PartId> = round
            .briefs
            .iter()
            .chain(&round.lead_brief)
            .filter(|b| b.receipt.sent.is_some() && !b.reply_path.exists())
            .filter(|b| !self.panes.contains_key(&b.to))
            .filter(|b| in_play(self.engine.session(), no, &b.to))
            .map(|b| b.to.clone())
            .collect();
        for part in lost {
            self.feed(Input::Redeliver { round: no, part })?;
        }
        Ok(())
    }

    fn write_report(&self) {
        let text = export::report_markdown(self.engine.session());
        if let Err(e) = store::write_report(self.engine.dir(), &text) {
            self.notice(format!("レポートの書き出しに失敗しました: {e}"));
        }
    }

    fn feed_user(&mut self, input: Input) -> Result<(), String> {
        match self.engine.handle(input) {
            Ok(out) => self.apply_output(out),
            Err(EngineError::Rejected(why)) => {
                self.notice(why);
                Ok(())
            }
            Err(EngineError::Reduce(e)) => Err(e.to_string()),
        }
    }

    fn feed(&mut self, input: Input) -> Result<(), Fail> {
        match self.engine.handle(input) {
            Ok(out) => self.apply_output(out).map_err(Fail::Fatal),
            Err(EngineError::Rejected(why)) => {
                self.notice(why);
                Ok(())
            }
            Err(EngineError::Reduce(e)) => Err(Fail::Fatal(e.to_string())),
        }
    }

    fn apply_output(&mut self, output: review::engine::Output) -> Result<(), String> {
        if !output.batches.is_empty() {
            self.store
                .append_all(&output.batches)
                .map_err(|e| format!("write log: {e}"))?;
            self.record_outcome();
            self.emit_session();
            self.publish_timeline(false);
        }
        let now = self.clock.now();
        let mut failed = Vec::new();
        for effect in output.effects {
            match effect {
                Effect::SendBrief { round, part, body } => {
                    let body = match self.oversized_brief(round, &part, body) {
                        Ok(body) => body,
                        Err(error) => {
                            self.sends.retain(|s| !(s.round == round && s.part == part));
                            // Recorded after this output's effects are applied.
                            failed.push((round, part, error));
                            continue;
                        }
                    };
                    self.sends.retain(|s| !(s.round == round && s.part == part));
                    self.sends.push(PendingSend {
                        round,
                        part,
                        body,
                        queued_at: now,
                        spawns: 0,
                    });
                }
                Effect::WatchReply { round, part, path } => {
                    if let Err(e) = store::prepare_reply(&self.engine.reply_base(), &path) {
                        self.notice(format!("返答フォルダの作成に失敗しました: {e}"));
                    }
                    self.watches
                        .retain(|w| !(w.round == round && w.part == part));
                    self.watches.push(WatchedReply {
                        round,
                        part,
                        path,
                        seen: None,
                    });
                }
                Effect::ClearReply { path } => {
                    let _ = store::clear_reply(&self.engine.reply_base(), &path);
                    self.watches.retain(|w| w.path != path);
                }
                Effect::WriteReport => self.write_report(),
            }
        }
        for (round, part, error) in failed {
            match self.engine.handle(Input::SendFailed { round, part, error }) {
                Ok(out) => self.apply_output(out)?,
                Err(EngineError::Rejected(why)) => self.notice(why),
                Err(EngineError::Reduce(e)) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    /// `body` as it can be pasted: itself when it fits a control frame,
    /// else a short pointer to a file in the round's folder holding it.
    fn oversized_brief(
        &self,
        round: RoundNo,
        part: &PartId,
        body: String,
    ) -> Result<String, String> {
        let encoded = serde_json::to_string(&body).map_or(usize::MAX, |s| s.len());
        if encoded <= BRIEF_PASTE_MAX {
            return Ok(body);
        }
        let base = self.engine.reply_base();
        let path = brief_file(&self.engine.reply_dir(), round, part);
        if let Some(parent) = path.parent() {
            review::fsx::create_dirs(&base, parent)
                .map_err(|e| format!("ブリーフを書けません: {e}"))?;
        }
        review::fsx::write_replace(&base, &path, body.as_bytes())
            .map_err(|e| format!("ブリーフを書けません: {e}"))?;
        Ok(brief_pointer(&path))
    }

    fn deliver_sends(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let mut idx = 0;
        while idx < self.sends.len() {
            let round = self.sends[idx].round;
            let spawns = self.sends[idx].spawns;
            let queued_at = self.sends[idx].queued_at;
            if !in_play(self.engine.session(), round, &self.sends[idx].part) {
                self.sends.remove(idx);
                continue;
            }
            let now = self.clock.now();
            // A reply already on disk is taken in (or turned down) once it
            // settles; pasting the brief again would start a new turn.
            if self
                .watches
                .iter()
                .any(|w| w.round == round && w.part == self.sends[idx].part && w.path.exists())
            {
                self.sends[idx].queued_at = now;
                idx += 1;
                continue;
            }
            if !self.panes.contains_key(&self.sends[idx].part) {
                if spawns >= MAX_SPAWNS {
                    let removed = self.sends.remove(idx);
                    self.feed(Input::SendFailed {
                        round: removed.round,
                        part: removed.part,
                        error: "agent pane exited repeatedly".to_owned(),
                    })?;
                    continue;
                }
                let part = self.sends[idx].part.clone();
                self.sends[idx].spawns += 1;
                self.spawn_pane(conn, &part)?;
                idx += 1;
                continue;
            }
            // Checked before the pane counts as ready from an earlier brief: a
            // retry or correction must wait for the agent's turn to end, and a
            // prompt from a later turn is the user's to answer.
            let ready = {
                let p = self
                    .panes
                    .get_mut(&self.sends[idx].part)
                    .expect("pane exists");
                if p.held() || p.busy() {
                    p.ready = false;
                    p.quiet = None;
                }
                p.ready
            };
            if !ready {
                let part = self.sends[idx].part.clone();
                if self.try_ready(conn, &part, now)? {
                    // Ready now; send on this pass.
                } else {
                    // `try_ready` drops a pane found gone; the next pass starts it again.
                    let Some(pane_now) = self.panes.get_mut(&part) else {
                        idx += 1;
                        continue;
                    };
                    if pane_now.held() {
                        let notify = !pane_now.told_held;
                        pane_now.told_held = true;
                        if notify {
                            self.notice(format!(
                                "{part} が確認プロンプトで待機中です（Noa のペインで許可してください）"
                            ));
                        }
                        self.sends[idx].queued_at = now;
                    } else if pane_now.busy() {
                        // The time limit starts again once the turn ends.
                        self.sends[idx].queued_at = now;
                    } else if now.saturating_sub(queued_at) >= READY_LIMIT {
                        let removed = self.sends.remove(idx);
                        self.feed(Input::SendFailed {
                            round: removed.round,
                            part: removed.part,
                            error: "pane did not become ready in time".to_owned(),
                        })?;
                        continue;
                    }
                    idx += 1;
                    continue;
                }
            }
            let pane_id = self.panes[&self.sends[idx].part].id;
            match conn.type_text(pane_id, &self.sends[idx].body, true) {
                Ok(()) => {
                    self.clock.sleep(SUBMIT_DELAY);
                    if let Err(e) = conn.type_text(pane_id, "\r", false) {
                        match e {
                            NoaError::PaneGone => {
                                self.panes.remove(&self.sends[idx].part);
                                idx += 1;
                                continue;
                            }
                            // The agent left between the paste and the
                            // Enter (its input went with it): the brief
                            // waits for it to come back and goes again.
                            NoaError::NotReady => {
                                let part = self.sends[idx].part.clone();
                                if let Some(p) = self.panes.get_mut(&part) {
                                    p.ready = false;
                                    p.quiet = None;
                                }
                                self.notice(format!(
                                    "{part} への送信の確定（Enter）をやめました: シェルに戻っています"
                                ));
                                idx += 1;
                                continue;
                            }
                            other => return Err(link(other)),
                        }
                    }
                    if let Some(p) = self.panes.get_mut(&self.sends[idx].part) {
                        p.briefed = true;
                    }
                    let removed = self.sends.remove(idx);
                    let at = self.clock.now();
                    self.feed(Input::Sent {
                        round: removed.round,
                        part: removed.part,
                        at,
                    })?;
                }
                Err(NoaError::NotReady) => {
                    if let Some(p) = self.panes.get_mut(&self.sends[idx].part) {
                        p.ready = false;
                    }
                    idx += 1;
                }
                Err(NoaError::PaneGone) => {
                    self.panes.remove(&self.sends[idx].part);
                    idx += 1;
                }
                Err(NoaError::Refused(why)) => {
                    let removed = self.sends.remove(idx);
                    self.feed(Input::SendFailed {
                        round: removed.round,
                        part: removed.part,
                        error: why,
                    })?;
                }
                Err(other) => return Err(link(other)),
            }
        }
        Ok(())
    }

    fn try_ready(&mut self, conn: &mut C::Conn, part: &PartId, now: Millis) -> Result<bool, Fail> {
        let (pane_id, try_trust, skip_ready) = {
            let Some(pane) = self.panes.get_mut(part) else {
                return Ok(false);
            };
            if pane.reports_ready() {
                pane.ready = true;
                return Ok(true);
            }
            let try_trust = self.engine.session().workdir.is_none()
                && pane.is_claude_trust()
                && pane.trust_keys < TRUST_KEYS_MAX;
            (pane.id, try_trust, pane.held() || pane.busy())
        };
        // Auto-trust Claude Code folder prompt only when running in our managed
        // reviews directory (never in a user's own repository).
        if try_trust {
            let screen = conn.screen(pane_id).unwrap_or_default();
            let can_press = self
                .panes
                .get(part)
                .and_then(|p| p.trust_screen.as_ref())
                .is_none_or(|(prev, at)| {
                    *prev != screen || now.saturating_sub(*at) >= TRUST_KEY_SHOWS_IN
                });
            if can_press && screen.contains(CLAUDE_TRUST_YES) {
                let _ = conn.type_text(pane_id, "\r", false);
                if let Some(p) = self.panes.get_mut(part) {
                    p.trust_keys += 1;
                    p.trust_screen = Some((screen, now));
                }
                return Ok(false);
            }
        }
        if skip_ready {
            return Ok(false);
        }
        match conn.paste_ready(pane_id) {
            Ok(true) => {
                let screen = conn.screen(pane_id).unwrap_or_default();
                if let Some(p) = self.panes.get_mut(part) {
                    match &p.quiet {
                        Some((prev, since)) if *prev == screen => {
                            if now.saturating_sub(*since) >= QUIET_FOR {
                                p.ready = true;
                                return Ok(true);
                            }
                        }
                        _ => {
                            p.quiet = Some((screen, now));
                        }
                    }
                }
                Ok(false)
            }
            Ok(false) => Ok(false),
            Err(NoaError::PaneGone) => {
                self.panes.remove(part);
                Ok(false)
            }
            Err(other) => Err(link(other)),
        }
    }

    fn spawn_pane(&mut self, conn: &mut C::Conn, part: &PartId) -> Result<(), Fail> {
        let Some(participant) = self.engine.session().participant(part).cloned() else {
            return Ok(());
        };
        let cwd = self
            .engine
            .session()
            .workdir
            .clone()
            .unwrap_or_else(|| self.engine.dir().to_path_buf());
        let _ = fs::create_dir_all(&cwd);
        let spec = PaneSpec {
            argv: agent_argv(participant.kind, &participant.setup),
            cwd,
            label: self.pane_label(part),
            attended: false,
            env: Vec::new(),
        };
        // A board that closed since it was noted answers PaneGone: forget it
        // and open the next one (once; a second PaneGone is a fresh board
        // closed at once).
        let mut refused = String::new();
        for _ in 0..2 {
            let board = match self.board {
                Some(board) => board,
                None => {
                    if !conn.supports_board_tab() {
                        return Err(Fail::Fatal(no_board_tab(&conn.server_version())));
                    }
                    match conn.spawn(Placement::Board, &spec) {
                        Ok(board) => {
                            self.board = Some(board);
                            board
                        }
                        Err(NoaError::Refused(why)) => {
                            refused = why;
                            break;
                        }
                        Err(NoaError::PaneGone) => continue,
                        Err(other) => return Err(link(other)),
                    }
                }
            };
            match conn.spawn(Placement::Float(board), &spec) {
                Ok(pane_id) => {
                    self.panes.insert(part.clone(), Pane::new(pane_id));
                    return self.feed(Input::PaneSpawned {
                        part: part.clone(),
                        pane_id,
                    });
                }
                Err(NoaError::Refused(why)) => {
                    refused = why;
                    break;
                }
                Err(NoaError::PaneGone) => self.board = None,
                Err(other) => return Err(link(other)),
            }
        }
        Err(Fail::Link(format!(
            "{part} のペインを開けません: {refused}"
        )))
    }

    fn poll_replies(&mut self) -> Result<(), Fail> {
        let now = self.clock.now();
        let base = self.engine.reply_base();
        let mut ready = Vec::new();
        let mut idx = 0;
        while idx < self.watches.len() {
            let watch = &mut self.watches[idx];
            if !in_play(self.engine.session(), watch.round, &watch.part) {
                self.watches.remove(idx);
                continue;
            }
            // A read error that stays is turned down like a bad reply
            // (it counts against the retries), never waited on for ever.
            let read = match store::read_reply(&base, &watch.path) {
                Ok(Some(bytes)) => Ok(bytes),
                Ok(None) => {
                    watch.seen = None;
                    idx += 1;
                    continue;
                }
                Err(e) => Err(e.to_string()),
            };
            match &watch.seen {
                Some((prev, since)) if *prev == read => {
                    if now.saturating_sub(*since) >= REPLY_STABLE_FOR {
                        let w = self.watches.remove(idx);
                        ready.push((w.round, w.part, read));
                        continue;
                    }
                }
                _ => watch.seen = Some((read, now)),
            }
            idx += 1;
        }
        for (round, part, read) in ready {
            let input = match read.map(String::from_utf8) {
                Ok(Ok(content)) => Input::ReplyFileWritten {
                    round,
                    part,
                    content,
                    at: now,
                },
                Ok(Err(e)) => Input::ReplyUnreadable {
                    round,
                    part,
                    error: "返答が UTF-8 のテキストではありません".to_owned(),
                    digest: review::capture::digest_bytes(e.as_bytes()),
                    at: now,
                },
                Err(why) => Input::ReplyUnreadable {
                    round,
                    part,
                    error: format!("返答を読めません: {why}"),
                    digest: review::capture::digest_bytes(why.as_bytes()),
                    at: now,
                },
            };
            self.feed(input)?;
        }
        Ok(())
    }

    fn all_parts(&self) -> Vec<PartId> {
        let s = self.engine.session();
        std::iter::once(s.lead.id.clone())
            .chain(s.reviewer_ids().cloned())
            .collect()
    }

    fn pane_label(&self, part: &PartId) -> String {
        format!("rev:{}:{part}", self.engine.session().id)
    }
}

/// Opens the review logged in `dir` to go on with it, after the
/// checks a log gets (see [`Start::Resume`]); with `extend_by`, extended
/// first (see [`Start::Extend`]).
fn open_log(
    dir: PathBuf,
    extend_by: Option<RoundNo>,
    config: Config,
    now: Millis,
) -> Result<(Engine, Store), String> {
    let (mut store, session) =
        Store::open(&dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
    // Checked under the lock: a list shown earlier may be stale.
    if session.closed && extend_by.is_none() {
        return Err(format!(
            "{} は終了しています（閲覧で開いてください）",
            dir.display()
        ));
    }
    review::engine::check_resumable(&session, &dir)
        .map_err(|why| format!("{} を再開できません: {why}", dir.display()))?;
    let mut engine = Engine::resume(session, &dir, config);
    // Refused before anything is written, in the log or the workdir.
    let extension = match extend_by {
        None => None,
        Some(by) => match engine.handle(Input::UserExtend { by, at: now }) {
            Ok(out) => Some(out),
            Err(EngineError::Rejected(why)) => {
                return Err(format!("ラウンドを追加できません: {why}"));
            }
            Err(EngineError::Reduce(e)) => return Err(e.to_string()),
        },
    };
    if let Some(workdir) = &engine.session().workdir {
        if !workdir.is_dir() {
            return Err(format!(
                "{} はディレクトリではありません",
                workdir.display()
            ));
        }
        store::prepare_workdir(workdir)
            .map_err(|e| format!("prepare {}: {e}", workdir.display()))?;
    }
    if let Some(out) = extension {
        // Stored before anything starts; the round it opens is briefed by
        // the start that follows the first connection.
        store
            .append_all(&out.batches)
            .map_err(|e| format!("write log: {e}"))?;
    }
    // The report follows the log: a stop between the two writes (the log
    // took a lead output, the report did not) is repaired here.
    let session = engine.session();
    if session.closed || session.rounds.iter().any(|r| r.outcome.is_some()) {
        let _ = store::write_report(&dir, &export::report_markdown(session));
    }
    Ok((engine, store))
}

/// Where a brief too long to paste is written: `r<N>/brief-<part>.md`.
fn brief_file(reply_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    store::round_dir(reply_dir, round).join(format!("brief-{part}.md"))
}

/// What is pasted in place of a brief written to `path`.
fn brief_pointer(path: &Path) -> String {
    format!(
        "This round's brief is too long to paste, so it is in a file. Read all of {} \
         now and follow it exactly as your brief for this round: it says what to \
         review and where to write your reply.",
        path.display()
    )
}

/// How many ids of one millisecond a start tries before it gives up.
const ID_TRIES: u32 = 1000;

/// A new review's id with its directories reserved: the store directory
/// and (with a workdir) the reply dir, each created exclusively. An id
/// taken in either place (another window, another data dir) moves on to
/// the next suffix, so what a refused start removes is only its own.
/// Returns the id, the store directory and the reply dir.
fn reserve(
    root: &Path,
    workdir: Option<&Path>,
    now: Millis,
) -> Result<(SessionId, PathBuf, PathBuf), String> {
    fs::create_dir_all(root).map_err(|e| format!("{} を作れません: {e}", root.display()))?;
    if let Some(workdir) = workdir {
        store::prepare_workdir(workdir)
            .map_err(|e| format!("prepare {}: {e}", workdir.display()))?;
    }
    for suffix in 0..ID_TRIES {
        let id = match suffix {
            0 => SessionId::at(now),
            n => SessionId::new(format!("rev-{now}-{n}")),
        };
        let dir = store::session_dir(root, &id);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{} を作れません: {e}", dir.display())),
        }
        let Some(workdir) = workdir else {
            return Ok((id, dir.clone(), dir));
        };
        // `.noa/reviews` was checked free of links by `prepare_workdir`.
        let reply = store::workdir_reply_dir(workdir, &id);
        match fs::create_dir(&reply) {
            Ok(()) => return Ok((id, dir, reply)),
            Err(e) => {
                let _ = fs::remove_dir(&dir);
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(format!("{} を作れません: {e}", reply.display()));
                }
            }
        }
    }
    Err("新しいレビューの ID を確保できません".to_owned())
}

/// The directory the agents of a new review run in: the repository's top
/// level (git), the root (files), or the document's working directory.
fn workdir_of(target: &Target) -> Result<Option<PathBuf>, String> {
    match target.kind {
        TargetKind::Git => {
            let path = target
                .path
                .as_deref()
                .ok_or("リポジトリを指定してください")?;
            review::capture::repo_root(path).map(Some)
        }
        TargetKind::Files => {
            let root = target
                .path
                .clone()
                .ok_or("ルートのフォルダを指定してください")?;
            if !root.is_dir() {
                return Err(format!("{} はフォルダではありません", root.display()));
            }
            Ok(Some(root))
        }
        TargetKind::Document => Ok(target
            .workdir
            .clone()
            .filter(|dir| !dir.as_os_str().is_empty())),
    }
}

/// What a new review starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReview {
    pub auto: Option<review::auto_assign::AutoRecord>,
    pub request: String,
    pub target: Target,
    pub max_rounds: RoundNo,
    /// See [`review::engine::Setup::round_cap`].
    pub round_cap: RoundNo,
    pub lead: AgentChoice,
    pub panel: Vec<ReviewerChoice>,
    /// The store root (`<data dir>/noa/reviews`).
    pub root: PathBuf,
}

/// Captures the target and creates the review's log: nothing is left
/// behind when the capture or the setup is refused.
fn new_review(new: NewReview, config: Config, now: Millis) -> Result<(Engine, Store), String> {
    let NewReview {
        auto,
        request,
        target,
        max_rounds,
        round_cap,
        lead,
        panel,
        root,
    } = new;
    let root = root.as_path();
    review::engine::check_panel(&lead, &panel)?;
    let workdir = workdir_of(&target)?;
    if let Some(dir) = &workdir
        && !dir.is_dir()
    {
        return Err(format!("{} はフォルダではありません", dir.display()));
    }
    let (id, dir, reply_dir) = reserve(root, workdir.as_deref(), now)?;
    // This call made both directories (`reserve`): a refusal removes them
    // whole, and nothing else.
    let undo = || {
        let _ = fs::remove_dir_all(&reply_dir);
        let _ = fs::remove_dir_all(&dir);
    };
    let manifest = match review::capture::capture(&target, &store::target_dir(&reply_dir), now) {
        Ok(manifest) => manifest,
        Err(why) => {
            undo();
            return Err(why);
        }
    };
    let setup = Setup {
        request: &request,
        target,
        manifest,
        workdir,
        max_rounds,
        round_cap,
        lead,
        panel: &panel,
    };
    let (engine, output) = match Engine::create_in_with_auto(id, setup, &dir, config, now, auto) {
        Ok(created) => created,
        Err(e) => {
            undo();
            return Err(e.to_string());
        }
    };
    let store = Store::create(&dir, &output.batches[0]).map_err(|e| {
        undo();
        format!("create {}: {e}", dir.display())
    })?;
    Ok((engine, store))
}
fn session_origin(
    root: &Path,
    session: &Session,
    resumed: bool,
    now: Millis,
) -> Result<Option<Origin>, String> {
    if session.target.kind != TargetKind::Git {
        return Ok(None);
    }
    match fs::read_dir(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    }
    let id = session.id.as_str();
    if resumed {
        return Ok(task_links::find_origin(root, SessionKind::Review, id));
    }
    let manifest = &session.manifest;
    let Some(toplevel) = manifest.repo.clone() else {
        return Ok(None);
    };
    let target = task_links::Target {
        toplevel,
        head_ref: manifest.head_ref.clone(),
        head_sha: manifest.head_sha.clone(),
    };
    let Some(matched) = task_links::match_run(root, &target) else {
        return Ok(None);
    };
    let uncommitted = manifest.uncommitted;
    let log = task_links::links_log(&matched.run.key_dir);
    let events = task_links::read(&log).map_err(|error| format!("{}: {error}", log.display()))?;
    let tasks_repo = events.into_iter().rev().find_map(|event| match event {
        LinkEvent::Repo { path, .. } => Some(path),
        _ => None,
    });
    task_links::link_session(
        &matched,
        SessionKind::Review,
        id,
        target.head_sha.as_deref(),
        uncommitted,
        now,
    )
    .map_err(|error| format!("{}: {error}", log.display()))?;
    Ok(Some(Origin {
        key_dir: matched.run.key_dir,
        tasks_repo,
        task: matched.run.task,
        run: matched.run.run,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::ipc::BOARD_CAPACITY;
    use super::*;
    use review::model::types::{BuiltinLens, Head, LensChoice};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    mod links {
        use super::*;
        use review::task_links::{MatchedBy, RUNS_FILE};

        struct LinkFixture {
            root: PathBuf,
            repo: PathBuf,
            worktree: PathBuf,
            tasks_root: PathBuf,
            log: PathBuf,
            sha: String,
        }

        fn git(dir: &Path, args: &[&str]) -> String {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args([
                    "-c",
                    "user.name=Review Test",
                    "-c",
                    "user.email=review@example.test",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }

        impl LinkFixture {
            fn new() -> Self {
                let root = temp_root();
                let repo = root.join("repo");
                let worktree = root.join("worktree");
                fs::create_dir_all(&repo).unwrap();
                git(&repo, &["init", "-q", "-b", "main"]);
                fs::write(repo.join("a.txt"), "original\n").unwrap();
                git(&repo, &["add", "a.txt"]);
                git(&repo, &["commit", "-qm", "base"]);
                git(
                    &repo,
                    &[
                        "worktree",
                        "add",
                        "-qb",
                        "task-run-5",
                        worktree.to_str().unwrap(),
                    ],
                );
                fs::write(worktree.join("a.txt"), "changed\n").unwrap();
                git(&worktree, &["commit", "-qam", "change"]);
                let sha = git(&worktree, &["rev-parse", "HEAD"]);
                let tasks_root = root.join("tasks");
                let key_dir = tasks_root.join("key");
                fs::create_dir_all(&key_dir).unwrap();
                fs::write(
                    key_dir.join(RUNS_FILE),
                    format!(
                        "{}\n",
                        serde_json::json!({
                            "event": "RunStarted", "run": "run-5", "task": {"local": "t-1"},
                            "workdir": worktree, "branch": "task-run-5", "at": 5,
                        })
                    ),
                )
                .unwrap();
                let log = task_links::links_log(&key_dir);
                task_links::append(
                    &log,
                    &LinkEvent::Repo {
                        path: repo.clone(),
                        at: 1,
                    },
                )
                .unwrap();
                task_links::append(
                    &log,
                    &LinkEvent::ChangeSeen {
                        run: "run-5".into(),
                        commit: sha.clone(),
                        at: 6,
                    },
                )
                .unwrap();
                Self {
                    root,
                    repo,
                    worktree,
                    tasks_root,
                    log,
                    sha,
                }
            }

            fn start(&self, repo: &Path, head: Head, paths: &[&str]) -> Start {
                new_start_on(
                    &self.root.join("reviews"),
                    Target {
                        kind: TargetKind::Git,
                        path: Some(repo.to_owned()),
                        base: Some("main".into()),
                        head,
                        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
                        text: None,
                        workdir: None,
                    },
                    vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                    1,
                )
            }

            fn open(&self, fake: &Fake, start: Start) -> Wired {
                open_wired_at(fake, start, Some(self.tasks_root.clone())).unwrap()
            }

            fn events(&self) -> Vec<LinkEvent> {
                task_links::read(&self.log).unwrap()
            }
        }

        impl Drop for LinkFixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn links_ac3_matches_worktree_branch_and_commit_and_scopes_dirty_status() {
            let fixture = LinkFixture::new();
            fs::write(fixture.worktree.join("dirty.txt"), "untracked\n").unwrap();
            let fake = fake();
            for (repo, head, paths, expected, dirty) in [
                (
                    &fixture.worktree,
                    Head::Worktree,
                    &[][..],
                    MatchedBy::Worktree,
                    true,
                ),
                (
                    &fixture.worktree,
                    Head::Worktree,
                    &["a.txt"][..],
                    MatchedBy::Worktree,
                    false,
                ),
                (
                    &fixture.repo,
                    Head::Ref("task-run-5".into()),
                    &[][..],
                    MatchedBy::Branch,
                    false,
                ),
                (
                    &fixture.repo,
                    Head::Ref(fixture.sha.clone()),
                    &[][..],
                    MatchedBy::Commit,
                    false,
                ),
            ] {
                let (runner, _, _) = fixture.open(&fake, fixture.start(repo, head, paths));
                let id = &runner.engine.session().id;
                let origin = runner.origin.as_ref().unwrap();
                assert_eq!(origin.tasks_repo.as_deref(), Some(fixture.repo.as_path()));
                assert_eq!(origin.run, "run-5");
                let events = fixture.events();
                let links = events.iter().filter(|event| {
                    matches!(event, LinkEvent::SessionLinked { session, .. } if session == id.as_str())
                }).collect::<Vec<_>>();
                assert!(
                    matches!(links.as_slice(), [LinkEvent::SessionLinked {
                    kind: SessionKind::Review, task, run, commit: Some(commit), by, uncommitted, ..
                }] if task == &origin.task && run == "run-5" && commit == &fixture.sha && *by == expected && *uncommitted == dirty),
                    "{links:?}"
                );
            }
        }

        /// A snapshot taken with uncommitted changes stays "+未コミット" when
        /// the run commits them before the session is linked.
        #[test]
        fn links_ac3_dirty_snapshot_stays_uncommitted_when_committed_before_linking() {
            let fixture = LinkFixture::new();
            fs::write(fixture.worktree.join("a.txt"), "more\n").unwrap();
            let fake = fake();
            let start = fixture.start(&fixture.worktree, Head::Worktree, &[]);
            let (runner, _, _) = open_wired_at(&fake, start, None).unwrap();
            let session = runner.engine.session().clone();
            git(&fixture.worktree, &["commit", "-qam", "commit D"]);
            let origin = session_origin(&fixture.tasks_root, &session, false, 9)
                .unwrap()
                .expect("the run matches");
            assert_eq!(origin.run, "run-5");
            let events = fixture.events();
            assert!(
                matches!(events.last(), Some(LinkEvent::SessionLinked {
                    commit: Some(commit), uncommitted: true, ..
                }) if commit == &fixture.sha),
                "{events:?}"
            );
        }

        #[test]
        fn links_ac3_unrelated_and_document_targets_write_nothing() {
            let fixture = LinkFixture::new();
            let unrelated = fixture.root.join("unrelated");
            fs::create_dir(&unrelated).unwrap();
            git(&unrelated, &["init", "-q", "-b", "main"]);
            git(
                &unrelated,
                &["commit", "--allow-empty", "-qm", "independent"],
            );
            fs::write(unrelated.join("other.txt"), "untracked\n").unwrap();
            let before = fixture.events();
            let fake = fake();
            let doc = new_start(
                &fixture.root.join("reviews"),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            );
            for start in [fixture.start(&unrelated, Head::Worktree, &[]), doc] {
                let (runner, _, _) = fixture.open(&fake, start);
                assert!(runner.origin.is_none());
                assert_eq!(fixture.events(), before);
            }
        }

        #[test]
        fn links_ac3_write_failure_leaves_review_created_and_posts_one_notice() {
            let fixture = LinkFixture::new();
            let lock = fixture.log.with_extension("jsonl.lock");
            fs::remove_file(&lock).unwrap();
            fs::create_dir(&lock).unwrap();
            let fake = fake();
            let (mut runner, _, updates) =
                fixture.open(&fake, fixture.start(&fixture.worktree, Head::Worktree, &[]));
            assert!(runner.origin.is_none());
            assert!(store::read_session(runner.engine.dir()).is_ok());
            assert!(runner.begin(&mut fake.clone()).is_ok());
            let notices = notices(&updates);
            assert!(
                matches!(notices.as_slice(), [notice] if notice.starts_with("タスクとの関連を記録できません: ") && notice.contains("links.jsonl")),
                "{notices:?}"
            );
        }

        #[test]
        fn links_ac3_outcomes_follow_verdict_close_resume_and_extension() {
            let fixture = LinkFixture::new();
            let fake = fake();
            let (runner, _, _) =
                fixture.open(&fake, fixture.start(&fixture.worktree, Head::Worktree, &[]));
            let dir = runner.engine.dir().to_owned();
            let id = runner.engine.session().id.to_string();
            let origin = runner.origin.clone();
            let outcomes = || {
                fixture
                    .events()
                    .into_iter()
                    .filter_map(|event| match event {
                        LinkEvent::SessionOutcome {
                            kind: SessionKind::Review,
                            session,
                            decision,
                            closed,
                            ..
                        } if session == id => Some((decision, closed)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(outcomes(), [(None, false)]);
            drop(runner);
            let (mut runner, _, _) = fixture.open(&fake, Start::Resume { dir: dir.clone() });
            assert_eq!(runner.origin, origin);
            let mut conn = fake.clone();
            assert!(runner.begin(&mut conn).is_ok());
            step(&mut runner, &mut conn);
            for part in [":claude", ":codex"] {
                fake.report(fake.pane_of(part), "input", None);
            }
            step(&mut runner, &mut conn);
            step(&mut runner, &mut conn);
            for part in ["claude", "codex"] {
                reply(
                    &fake,
                    &mut runner,
                    &mut conn,
                    &PartId::new(part),
                    r#"{"summary":"fine"}"#,
                );
            }
            step(&mut runner, &mut conn);
            fake.report(fake.pane_of(":lead"), "input", None);
            step(&mut runner, &mut conn);
            step(&mut runner, &mut conn);
            reply(
                &fake,
                &mut runner,
                &mut conn,
                &PartId::new("lead"),
                r#"{"summary":"Fine", "verdict":{"decision":"approve","rationale":"Sound."}}"#,
            );
            step(&mut runner, &mut conn);
            assert_eq!(
                outcomes(),
                [(None, false), (None, false), (Some("approve".into()), true)]
            );
            drop(runner);
            let (mut runner, _, _) = fixture.open(&fake, Start::Extend { dir, by: 1 });
            assert_eq!(runner.origin, origin);
            runner.feed_user(Input::UserClose).unwrap();
            assert_eq!(
                outcomes(),
                [
                    (None, false),
                    (None, false),
                    (Some("approve".into()), true),
                    (None, false),
                    (None, true)
                ]
            );
            assert_eq!(
                fixture
                    .events()
                    .iter()
                    .filter(|event| matches!(event, LinkEvent::SessionLinked { .. }))
                    .count(),
                1
            );
        }
    }

    struct FakePane {
        label: Option<String>,
        state: Option<String>,
        blocked: Option<String>,
        seq: u64,
        /// The board this pane floats on.
        parent: Option<u64>,
    }

    #[derive(Default)]
    struct World {
        /// `close_pane` closes the pane but answers as Noa does when its
        /// UI thread is slow (-32009): the outcome is unknown.
        close_unsure: bool,
        now: Millis,
        next_pane: u64,
        panes: BTreeMap<u64, FakePane>,
        events: VecDeque<Changes>,
        typed: Vec<(u64, String)>,
        focused: Vec<u64>,
        /// Panes closed through `close_pane`.
        closed: Vec<u64>,
        spawned: usize,
        /// Board panes alive (one per board tab opened and not closed).
        boards: BTreeSet<u64>,
        /// Board tabs opened so far.
        boards_opened: usize,
        /// Placements asked, in order.
        placements: Vec<Placement>,
        /// The version the fake's hello gives (0.9.16 when unset).
        version: Option<String>,
        /// Its hello lists no `boardTab` (a Noa that predates board tabs).
        no_board_tab: bool,
        /// Panes that are gone by the time they are asked for a paste.
        gone_on_paste: bool,
        /// Panes back at their shell: Noa refuses every write
        /// (`refuseShell`) and writes nothing.
        shell: BTreeSet<u64>,
        /// The agent exits right after the next paste into this pane.
        exits_after_paste: Option<u64>,
        rpc_panes: usize,
        rpc_changes: usize,
        rpc_paste_ready: usize,
        rpc_screen: usize,
    }

    /// A fake Noa and clock sharing one world.
    #[derive(Clone)]
    struct Fake(Rc<RefCell<World>>);

    impl Fake {
        fn view(id: u64, pane: &FakePane) -> PaneView {
            PaneView {
                pane_id: id,
                label: pane.label.clone(),
                agent_state: pane.state.clone(),
                agent_seq: pane.seq,
                agent_blocked: pane.blocked.clone(),
                agent_detail: None,
                parent_pane_id: pane.parent,
            }
        }

        fn report(&self, pane: u64, state: &str, blocked: Option<&str>) {
            let mut w = self.0.borrow_mut();
            let p = w.panes.get_mut(&pane).unwrap();
            p.seq += 1;
            p.state = Some(state.to_owned());
            p.blocked = blocked.map(str::to_owned);
            let view = Fake::view(pane, p);
            w.events.push_back(Changes {
                panes: vec![view],
                ..Changes::default()
            });
        }

        fn pane_of(&self, label_end: &str) -> u64 {
            let w = self.0.borrow();
            *w.panes
                .iter()
                .find(|(_, p)| p.label.as_deref().is_some_and(|l| l.ends_with(label_end)))
                .unwrap()
                .0
        }

        /// The texts pasted into `pane` (the Enter that submits each is left out).
        fn pasted(&self, pane: u64) -> usize {
            let w = self.0.borrow();
            w.typed
                .iter()
                .filter(|(p, t)| *p == pane && t != "\r")
                .count()
        }
    }

    impl Clock for Fake {
        fn now(&self) -> Millis {
            self.0.borrow().now
        }

        fn sleep(&self, duration: Duration) {
            self.0.borrow_mut().now += duration.as_millis() as Millis;
        }
    }

    impl Connect for Fake {
        type Conn = Fake;

        fn connect(&mut self) -> Result<Fake, String> {
            Ok(self.clone())
        }
    }

    impl Noa for Fake {
        fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
            let mut w = self.0.borrow_mut();
            w.rpc_panes += 1;
            let mut views: Vec<PaneView> =
                w.panes.iter().map(|(id, p)| Fake::view(*id, p)).collect();
            views.extend(w.boards.iter().map(|&id| PaneView {
                pane_id: id,
                label: Some(format!("board:{id}")),
                agent_state: None,
                agent_seq: 0,
                agent_blocked: None,
                agent_detail: None,
                parent_pane_id: None,
            }));
            Ok(views)
        }

        fn changes(&mut self, _wait: Duration) -> Result<Option<Changes>, NoaError> {
            let mut w = self.0.borrow_mut();
            w.rpc_changes += 1;
            Ok(w.events.pop_front())
        }

        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            let mut w = self.0.borrow_mut();
            let id = w.next_pane;
            w.placements.push(placement);
            let mut parent = None;
            match placement {
                Placement::NewTab => {}
                Placement::Board => {
                    w.next_pane += 1;
                    w.boards_opened += 1;
                    w.boards.insert(id);
                    return Ok(id);
                }
                Placement::Float(board) => {
                    if !w.boards.contains(&board) {
                        return Err(NoaError::PaneGone);
                    }
                    let floats = w.panes.values().filter(|p| p.parent == Some(board)).count();
                    if floats >= BOARD_CAPACITY as usize {
                        return Err(NoaError::Refused("limit reached".into()));
                    }
                    parent = Some(board);
                }
            }
            w.next_pane += 1;
            w.spawned += 1;
            w.panes.insert(
                id,
                FakePane {
                    label: Some(spec.label.clone()),
                    state: None,
                    blocked: None,
                    seq: 0,
                    parent,
                },
            );
            Ok(id)
        }

        fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError> {
            let mut w = self.0.borrow_mut();
            w.rpc_paste_ready += 1;
            if w.gone_on_paste {
                w.panes.remove(&pane);
                return Err(NoaError::PaneGone);
            }
            Ok(true)
        }

        fn screen(&mut self, _pane: u64) -> Result<String, NoaError> {
            self.0.borrow_mut().rpc_screen += 1;
            Ok("> ".to_owned())
        }

        fn type_text(&mut self, pane: u64, text: &str, _paste: bool) -> Result<(), NoaError> {
            let mut w = self.0.borrow_mut();
            if w.shell.contains(&pane) {
                return Err(NoaError::NotReady);
            }
            w.typed.push((pane, text.to_owned()));
            if text != "\r" && w.exits_after_paste == Some(pane) {
                w.exits_after_paste = None;
                w.shell.insert(pane);
            }
            Ok(())
        }

        fn server_version(&self) -> String {
            self.0
                .borrow()
                .version
                .clone()
                .unwrap_or_else(|| "0.9.16".into())
        }

        fn supports_board_tab(&self) -> bool {
            !self.0.borrow().no_board_tab
        }

        fn hello_seq(&self) -> u64 {
            1
        }

        fn focus(&mut self, pane: u64) -> Result<(), NoaError> {
            let mut w = self.0.borrow_mut();
            if !w.panes.contains_key(&pane) {
                return Err(NoaError::PaneGone);
            }
            w.focused.push(pane);
            Ok(())
        }

        fn close_pane(&mut self, pane: u64) -> Result<(), NoaError> {
            let mut w = self.0.borrow_mut();
            if w.boards.remove(&pane) {
                // Closing a board closes the floats stowed on it.
                let floats: Vec<u64> = w
                    .panes
                    .iter()
                    .filter(|(_, p)| p.parent == Some(pane))
                    .map(|(id, _)| *id)
                    .collect();
                for id in floats {
                    w.panes.remove(&id);
                    w.closed.push(id);
                    w.events.push_back(Changes {
                        removed: vec![id],
                        ..Changes::default()
                    });
                }
            } else if w.panes.remove(&pane).is_none() {
                return Err(NoaError::PaneGone);
            }
            w.closed.push(pane);
            w.events.push_back(Changes {
                removed: vec![pane],
                ..Changes::default()
            });
            if w.close_unsure {
                return Err(NoaError::Refused("timed out (-32009)".into()));
            }
            Ok(())
        }
    }

    const CONFIG: Config = Config {
        start_timeout: 60_000,
        reply_timeout: 600_000,
        max_retries: 2,
    };

    fn temp_root() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-review-live-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn fake() -> Fake {
        Fake(Rc::new(RefCell::new(World {
            now: 1_000,
            next_pane: 100,
            ..World::default()
        })))
    }

    /// Runs the runner's loop body once: sends, replies, the tick, one change.
    fn step(runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        assert!(runner.deliver_sends(conn).is_ok());
        assert!(runner.poll_replies().is_ok());
        let now = runner.clock.now();
        assert!(runner.feed(Input::Tick { now }).is_ok());
        while let Some(changes) = conn.changes(POLL).unwrap() {
            assert!(runner.apply_changes(conn, changes).is_ok());
        }
    }

    type Wired = (Runner<Fake, Fake>, Sender<Action>, Receiver<Update>);

    fn open_wired(fake: &Fake, start: Start) -> Result<Wired, String> {
        open_wired_at(fake, start, None)
    }

    fn open_wired_at(
        fake: &Fake,
        start: Start,
        tasks_root: Option<PathBuf>,
    ) -> Result<Wired, String> {
        open_wired_with(fake, start, tasks_root, None)
    }

    fn open_wired_with(
        fake: &Fake,
        start: Start,
        tasks_root: Option<PathBuf>,
        timeline: Option<TimelineLink>,
    ) -> Result<Wired, String> {
        let (actions_tx, actions) = mpsc::channel();
        let (updates, updates_rx) = mpsc::channel();
        let runner = Runner::open(
            start,
            fake.clone(),
            fake.clone(),
            RunnerConfig {
                engine: CONFIG,
                tasks_root,
                timeline,
            },
            actions,
            updates,
            Rc::new(|| {}),
        )?;
        Ok((runner, actions_tx, updates_rx))
    }

    fn reviewer(agent: AgentChoice) -> ReviewerChoice {
        ReviewerChoice {
            agent,
            lens: LensChoice::Builtin(BuiltinLens::Correctness),
        }
    }

    fn document(text: &str) -> Target {
        Target {
            kind: TargetKind::Document,
            path: None,
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: Some(text.into()),
            workdir: None,
        }
    }

    fn new_start_on(
        root: &Path,
        target: Target,
        panel: Vec<AgentChoice>,
        max_rounds: RoundNo,
    ) -> Start {
        Start::New(Box::new(NewReview {
            auto: None,
            request: "Review the migration plan".into(),
            target,
            max_rounds,
            round_cap: review::model::types::MAX_MAX_ROUNDS,
            lead: AgentKind::Claude.into(),
            panel: panel.into_iter().map(reviewer).collect(),
            root: root.to_owned(),
        }))
    }

    fn new_start(root: &Path, panel: Vec<AgentChoice>) -> Start {
        new_start_on(root, document("# Plan\n\n1. Move the data\n"), panel, 2)
    }

    #[test]
    fn created_record_survives_store_replay_and_manual_logs_omit_auto() {
        use review::auto_assign::{AutoRecord, Pick};

        let root = temp_root();
        let record = AutoRecord {
            planner: "claude (sonnet · medium)".into(),
            at: 4,
            picks: vec![Pick {
                slot: "lead".into(),
                agent: "claude".into(),
                model: None,
                effort: None,
                reason: "指摘を取りまとめるため".into(),
            }],
            overridden: vec!["reviewer-1".into(), "reviewer-2".into()],
        };
        for auto in [None, Some(record)] {
            let Start::New(mut new) = new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ) else {
                unreachable!()
            };
            let workdir = root.join("workspace");
            fs::create_dir_all(&workdir).unwrap();
            new.target.workdir = Some(workdir.clone());
            new.auto = auto.clone();
            let (engine, store) = new_review(*new, CONFIG, 5).unwrap();
            assert_eq!(engine.session().auto, auto);
            let dir = store.dir().to_owned();
            drop(store);
            let line = fs::read_to_string(dir.join(store::EVENTS_FILE)).unwrap();
            let batch: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(batch[0]["event"], "created");
            assert_eq!(batch[0]["workdir"], serde_json::json!(workdir));
            assert_eq!(
                batch[0].get("auto").cloned(),
                auto.as_ref().map(|a| serde_json::to_value(a).unwrap())
            );
            let loaded = store::read_session(&dir).unwrap();
            assert_eq!(loaded.workdir, Some(workdir));
            assert_eq!(loaded.auto, auto);
            let resumed = Engine::resume(loaded, &dir, CONFIG);
            assert_eq!(resumed.session().auto, auto);
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn rig_on(fake: &Fake, start: Start) -> Wired {
        let (mut runner, actions, updates) =
            open_wired(fake, start).unwrap_or_else(|e| panic!("{e}"));
        let mut conn = fake.clone();
        assert!(runner.begin(&mut conn).is_ok());
        (runner, actions, updates)
    }

    #[test]
    fn each_connection_reports_the_version_of_its_noa() {
        let root = temp_root();
        let fake = fake();
        let (_runner, _actions, updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let versions: Vec<String> = updates
            .try_iter()
            .filter_map(|u| match u {
                Update::Version(_, v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(versions, ["0.9.16"]);
    }

    fn rig(fake: &Fake, root: &Path) -> Runner<Fake, Fake> {
        rig_on(
            fake,
            new_start(
                root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        )
        .0
    }

    fn notices(updates: &Receiver<Update>) -> Vec<String> {
        updates
            .try_iter()
            .filter_map(|u| match u {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Waits for `dir`'s log to be unlocked (see `store::LOCK_GRACE`).
    fn wait_unlocked(dir: &Path) {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while store::is_busy(dir) {
            assert!(
                std::time::Instant::now() < until,
                "{} is still locked 10 s after its runner closed",
                dir.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    const BLIND: &str = r#"{"summary": "done", "findings": [{"id": "1", "location": {"path": "document.md", "line": 1}, "severity": "medium", "category": "correctness", "confidence": "high", "claim": "Step 1 has no rollback", "evidence": "Line 1 names no rollback."}]}"#;

    /// A review whose two reviewers took their first brief, then stopped
    /// (the runner is dropped); returns its directory.
    fn stopped_after_first_briefs(fake: &Fake, root: &Path) -> PathBuf {
        let mut runner = rig(fake, root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1);
        }
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        dir
    }

    /// Writes `text` as `part`'s reply and lets it settle.
    fn reply(
        fake: &Fake,
        runner: &mut Runner<Fake, Fake>,
        conn: &mut Fake,
        part: &PartId,
        text: &str,
    ) {
        let round = runner.engine.session().current_round().unwrap().no;
        let path = if part.as_str() == "lead" {
            store::lead_reply_path(&runner.engine.reply_dir(), round)
        } else {
            store::reply_path(&runner.engine.reply_dir(), round, part)
        };
        fs::write(&path, text).unwrap();
        step(runner, conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(runner, conn);
    }

    #[test]
    fn a_new_review_captures_its_target_before_the_log_and_briefs_from_the_snapshot() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let dir = runner.engine.dir().to_owned();
        let target = store::target_dir(&dir);
        assert_eq!(
            fs::read_to_string(target.join("document.md")).unwrap(),
            "# Plan\n\n1. Move the data\n"
        );
        assert!(target.join("manifest.json").is_file());
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        fake.report(fake.pane_of(":claude"), "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let typed = fake.0.borrow().typed.clone();
        let brief = &typed.iter().find(|(_, t)| t != "\r").unwrap().1;
        assert!(
            brief.contains(&target.join("document.md").display().to_string()),
            "{brief}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_refused_capture_or_setup_leaves_nothing_behind() {
        let fake = fake();
        let root = temp_root();
        let err = open_wired(
            &fake,
            new_start_on(
                &root,
                document("   "),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                2,
            ),
        )
        .err()
        .unwrap();
        assert!(err.contains("文書"), "{err}");
        let err = open_wired(
            &fake,
            new_start_on(&root, document("x"), vec![AgentKind::Claude.into()], 2),
        )
        .err()
        .unwrap();
        assert!(err.contains("2体以上"), "{err}");
        let err = open_wired(
            &fake,
            new_start_on(
                &root,
                document("x"),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                0,
            ),
        )
        .err()
        .unwrap();
        assert!(err.contains("ラウンド上限"), "{err}");
        let left: Vec<_> = fs::read_dir(&root).map(|d| d.collect()).unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac23_the_report_is_rewritten_after_each_consolidation_and_at_the_end() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let report = store::report_path(runner.engine.dir());
        for part in ["claude", "codex"] {
            reply(&fake, &mut runner, &mut conn, &PartId::new(part), BLIND);
        }
        assert!(!report.exists(), "no report before the first consolidation");
        assert_eq!(
            runner.engine.session().current_round().unwrap().phase,
            Phase::Consolidating
        );
        step(&mut runner, &mut conn);
        fake.report(fake.pane_of(":lead"), "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        reply(
            &fake,
            &mut runner,
            &mut conn,
            &PartId::new("lead"),
            r#"{"summary": "One finding.", "ops": [
              {"op": "add", "id": "F1", "from": ["claude/1", "codex/1"], "claim": "No rollback", "severity": "medium", "category": "correctness", "confidence": "high", "status": "open", "rationale": "Both."}]}"#,
        );
        let first = fs::read_to_string(&report).unwrap();
        assert!(first.starts_with("# レビュー結果（暫定）"), "{first}");
        assert!(first.contains("F1"), "{first}");

        // A stop after the log took the lead output but before the report
        // was written: taking the review up again writes it.
        fs::remove_file(&report).unwrap();
        let dir = runner.engine.dir().to_owned();
        drop((runner, actions));
        wait_unlocked(&dir);
        let (mut runner, actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert_eq!(fs::read_to_string(&report).unwrap(), first);

        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        let last = fs::read_to_string(&report).unwrap();
        assert!(last.contains("判定なし（ユーザーが終了）"), "{last}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_resumed_review_takes_up_the_agents_still_in_their_panes() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        let stored = store::read_session(&dir).unwrap();
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir: dir.clone() });
        assert_eq!(*runner.engine.session(), stored);
        let mut conn = fake.clone();
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        assert_eq!(fake.0.borrow().spawned, 2, "an agent was started again");
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} briefed again");
        }
        let claude = PartId::new("claude");
        reply(&fake, &mut runner, &mut conn, &claude, BLIND);
        assert!(
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .reviews
                .contains_key(&claude)
        );
        drop(runner);
        assert!(
            store::read_session(&dir)
                .unwrap()
                .round(1)
                .unwrap()
                .reviews
                .contains_key(&claude)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_resumed_review_starts_agents_whose_panes_are_gone_again() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        fake.0.borrow_mut().panes.clear();
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, 4);
        for part in [":claude", ":codex"] {
            let pane = fake.pane_of(part);
            assert!(pane >= 102, "{part} was not started anew");
            fake.report(pane, "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} not briefed");
        }
        let round = runner.engine.session().round(1).unwrap();
        let brief = round.brief(&PartId::new("claude")).unwrap();
        assert_eq!(brief.receipt.retries, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_first_agent_opens_a_board_and_every_agent_floats_on_it() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let w = fake.0.borrow();
        assert_eq!(w.boards_opened, 1);
        let board = *w.boards.iter().next().unwrap();
        assert_eq!(runner.board, Some(board));
        assert_eq!(w.placements[0], Placement::Board, "the board opens first");
        assert_eq!(
            w.placements[1..],
            [Placement::Float(board), Placement::Float(board)],
            "the agents float on that one board"
        );
        assert_eq!(w.spawned, 2);
        assert!(w.panes.values().all(|p| p.parent == Some(board)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_later_agent_floats_on_the_same_board() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        // Only codex's pane is gone: it starts again, next to claude's.
        let codex = fake.pane_of(":codex");
        fake.0.borrow_mut().panes.remove(&codex);
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let w = fake.0.borrow();
        assert_eq!(w.boards_opened, 1, "no second board");
        let board = *w.boards.iter().next().unwrap();
        assert_eq!(w.placements.last(), Some(&Placement::Float(board)));
        assert_eq!(w.spawned, 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_board_that_is_gone_is_forgotten_and_a_new_one_opens() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let first = runner.board.unwrap();
        // The person closes the board tab before the runner has heard of
        // it: asked to float on it, Noa says the pane is gone, and the
        // runner opens a new board instead.
        conn.close_pane(first).unwrap();
        runner.panes.clear();
        let part = PartId::new("claude");
        runner.spawn_pane(&mut conn, &part).ok().unwrap();
        {
            let w = fake.0.borrow();
            assert_eq!(w.boards_opened, 2);
            let second = *w.boards.iter().next().unwrap();
            assert_ne!(first, second);
            assert_eq!(runner.board, Some(second));
            assert_eq!(w.panes[&runner.panes[&part].id].parent, Some(second));
        }
        // Told of the first board's closing, the runner keeps the second.
        for _ in 0..4 {
            step(&mut runner, &mut conn);
        }
        assert_eq!(runner.board, fake.0.borrow().boards.iter().next().copied());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resume_recovers_the_board_from_the_agents_parent() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        let board = *fake.0.borrow().boards.iter().next().unwrap();
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert_eq!(runner.board, Some(board));
        // An agent started later floats on it: no second board.
        let codex = fake.pane_of(":codex");
        fake.0.borrow_mut().panes.remove(&codex);
        let mut conn = fake.clone();
        runner.sync_panes(&mut conn).ok().unwrap();
        runner
            .spawn_pane(&mut conn, &PartId::new("codex"))
            .ok()
            .unwrap();
        assert_eq!(fake.0.borrow().boards_opened, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_older_noa_is_refused_and_nothing_is_spawned() {
        let fake = fake();
        fake.0.borrow_mut().version = Some("0.9.15".into());
        fake.0.borrow_mut().no_board_tab = true;
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let mut conn = fake.clone();
        let Err(Fail::Fatal(why)) = runner.deliver_sends(&mut conn) else {
            panic!("an older Noa must refuse the session's agents");
        };
        assert!(why.contains("0.9.16 以降が必要"), "{why}");
        let w = fake.0.borrow();
        assert!(w.placements.is_empty(), "{:?}", w.placements);
        assert_eq!((w.spawned, w.boards_opened), (0, 0));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_id_reused_by_another_pane_is_not_taken_for_the_agent() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        for pane in fake.0.borrow_mut().panes.values_mut() {
            pane.label = Some("zsh".into());
        }
        let (runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert!(runner.panes.is_empty(), "{:?}", runner.panes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_resume_checks_the_setup_of_a_log_edited_by_hand() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![
            AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some("opus".into()),
                    effort: None,
                },
            },
            AgentKind::Codex.into(),
        ];
        let (runner, _, _) = rig_on(&fake, new_start(&root, panel));
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        let log = dir.join(store::EVENTS_FILE);
        let text = fs::read_to_string(&log).unwrap();
        fs::write(&log, text.replace("\"opus\"", "\"opus;touch x\"")).unwrap();
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("再開できません"), "{err}");
        // A log moved to another directory name.
        fs::write(&log, text).unwrap();
        let moved = root.join("rev-elsewhere");
        fs::rename(&dir, &moved).unwrap();
        let err = open_wired(&fake, Start::Resume { dir: moved })
            .err()
            .unwrap();
        assert!(err.contains("フォルダ名と一致しません"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_review_running_elsewhere_is_not_resumed_twice() {
        let fake = fake();
        let root = temp_root();
        let runner = rig(&fake, &root);
        let dir = runner.engine.dir().to_owned();
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("別のウィンドウで実行中"), "{err}");
        drop(runner);
        wait_unlocked(&dir);
        assert!(open_wired(&fake, Start::Resume { dir }).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_changed_snapshot_is_not_resumed() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        fs::write(store::target_dir(&dir).join("document.md"), "edited").unwrap();
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("スナップショットが変更されています"), "{err}");
        fs::remove_dir_all(store::target_dir(&dir)).unwrap();
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("スナップショット"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_review_whose_workdir_is_gone_is_not_resumed() {
        let fake = fake();
        let root = temp_root();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        let mut target = document("plan");
        target.workdir = Some(work.clone());
        let (runner, _, _) = rig_on(
            &fake,
            new_start_on(
                &root,
                target,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                2,
            ),
        );
        let dir = runner.engine.dir().to_owned();
        assert!(
            work.join(".noa/reviews")
                .join(dir.file_name().unwrap())
                .join("target/document.md")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(work.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        drop(runner);
        wait_unlocked(&dir);
        fs::remove_dir_all(&work).unwrap();
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("作業ディレクトリ"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_log_whose_reply_paths_lead_elsewhere_is_not_resumed() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        let log = dir.join(store::EVENTS_FILE);
        let text = fs::read_to_string(&log).unwrap();
        let elsewhere = root.join("victim.txt");
        fs::write(&elsewhere, "keep me").unwrap();
        let from = store::reply_path(&dir, 1, &PartId::new("codex"));
        let edited = text.replace(
            &from.display().to_string(),
            &elsewhere.display().to_string(),
        );
        assert_ne!(edited, text);
        fs::write(&log, edited).unwrap();
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("返答パス"), "{err}");
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "keep me");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_finished_review_is_not_resumed_but_a_full_run_is_extended_before_any_pane_starts() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(
            &fake,
            new_start_on(
                &root,
                document("plan"),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                1,
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in ["claude", "codex"] {
            reply(
                &fake,
                &mut runner,
                &mut conn,
                &PartId::new(part),
                r#"{"summary": "fine"}"#,
            );
        }
        step(&mut runner, &mut conn);
        fake.report(fake.pane_of(":lead"), "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        reply(
            &fake,
            &mut runner,
            &mut conn,
            &PartId::new("lead"),
            r#"{"summary": "Nothing found.", "verdict": {"decision": "approve", "rationale": "Sound."}}"#,
        );
        assert!(runner.engine.session().closed);
        let dir = runner.engine.dir().to_owned();
        let spawned = fake.0.borrow().spawned;
        drop(runner);
        wait_unlocked(&dir);
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("終了しています"), "{err}");

        let (mut runner, _actions, _updates) = open_wired(
            &fake,
            Start::Extend {
                dir: dir.clone(),
                by: 2,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let stored = store::read_session(&dir).unwrap();
        assert!(!stored.closed);
        assert_eq!(stored.max_rounds, 3);
        assert!(stored.verdict().is_none());
        assert_eq!(
            fake.0.borrow().spawned,
            spawned,
            "a pane started before the extension was stored"
        );
        assert!(
            fs::read_to_string(store::report_path(&dir))
                .unwrap()
                .contains("（暫定）")
        );
        assert!(runner.begin(&mut conn).is_ok());
        step(&mut runner, &mut conn);
        assert_eq!(runner.engine.session().rounds.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_refused_extension_writes_nothing() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        assert!(runner.feed_user(Input::UserClose).is_ok());
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        let before = fs::read(dir.join(store::EVENTS_FILE)).unwrap();
        let err = open_wired(
            &fake,
            Start::Extend {
                dir: dir.clone(),
                by: 1,
            },
        )
        .err()
        .unwrap();
        assert!(err.contains("ラウンドを追加できません"), "{err}");
        assert_eq!(fs::read(dir.join(store::EVENTS_FILE)).unwrap(), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn focusing_an_agent_brings_its_pane_forward_or_says_why_not() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        actions
            .send(Action::Focus {
                part: PartId::new("claude"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(fake.0.borrow().focused, [claude]);
        assert!(notices(&updates).is_empty());
        // The lead has no pane before its first consolidation.
        actions
            .send(Action::Focus {
                part: PartId::new("lead"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("lead のペインがありません"));
        fake.0.borrow_mut().panes.remove(&claude);
        actions
            .send(Action::Focus {
                part: PartId::new("claude"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("閉じられています"));
        actions
            .send(Action::Focus {
                part: PartId::new("codex"),
            })
            .unwrap();
        runner.drain_actions(None).unwrap();
        assert!(notices(&updates)[0].contains("接続されていません"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_agents_panes_close_together_once_the_review_has_ended() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig_on(
            &fake,
            new_start_on(
                &root,
                document("plan"),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                1,
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let _ = notices(&updates);
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(fake.0.borrow().closed.is_empty(), "refused while running");
        assert!(notices(&updates)[0].contains("レビューが終わってから"));
        for part in ["claude", "codex"] {
            reply(
                &fake,
                &mut runner,
                &mut conn,
                &PartId::new(part),
                r#"{"summary": "fine"}"#,
            );
        }
        step(&mut runner, &mut conn);
        fake.report(fake.pane_of(":lead"), "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        reply(
            &fake,
            &mut runner,
            &mut conn,
            &PartId::new("lead"),
            r#"{"summary": "Nothing found.", "verdict": {"decision": "approve", "rationale": "Sound."}}"#,
        );
        assert!(runner.engine.session().closed);
        let spawned = fake.0.borrow().spawned;
        let _ = notices(&updates);
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(
            fake.0.borrow().closed.len(),
            4,
            "the lead, both reviewers and the board"
        );
        assert!(fake.0.borrow().panes.is_empty());
        assert!(fake.0.borrow().boards.is_empty(), "the board is closed too");
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        let told = notices(&updates);
        assert_eq!(
            told,
            vec!["3 個のペインとボードタブを閉じました".to_owned()]
        );
        assert_eq!(fake.0.borrow().spawned, spawned, "nothing started again");
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(
            notices(&updates),
            vec!["閉じるペインはありません".to_owned()]
        );
        // Given more rounds, the reviewers start again on a new board tab.
        actions.send(Action::Extend { by: 1 }).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(!runner.engine.session().closed);
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        assert!(
            fake.0.borrow().spawned > spawned,
            "the reviewers start again"
        );
        assert_eq!(fake.0.borrow().boards_opened, 2, "on a new board");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_whose_close_timed_out_closes_later_without_a_notice() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig_on(
            &fake,
            new_start_on(
                &root,
                document("plan"),
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                1,
            ),
        );
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(runner.engine.session().closed);
        fake.0.borrow_mut().close_unsure = true;
        let _ = notices(&updates);
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        let told = notices(&updates);
        assert!(
            !told.iter().any(|n| n.contains("のペインが閉じられました")),
            "{told:?}"
        );
        assert!(runner.panes.is_empty(), "the closed panes are forgotten");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_gone_while_waiting_for_ready_is_started_again() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, 2);
        fake.0.borrow_mut().gone_on_paste = true;
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().gone_on_paste = false;
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, 4);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_reply_turned_down_just_before_a_stop_is_not_turned_down_twice() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":claude");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let claude = PartId::new("claude");
        let path = store::reply_path(&runner.engine.reply_dir(), 1, &claude);
        reply(&fake, &mut runner, &mut conn, &claude, "  ");
        let retries = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .brief(&claude)
                .unwrap()
                .receipt
                .retries
        };
        assert_eq!(retries(&runner), 1);
        fs::write(&path, "  ").unwrap();
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert!(!path.exists(), "the turned-down reply is still there");
        for _ in 0..2 {
            step(&mut runner, &mut conn);
            fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        }
        step(&mut runner, &mut conn);
        assert_eq!(retries(&runner), 1, "turned down twice");
        assert_eq!(fake.pasted(pane), 2, "the correction was not sent");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agents_start_on_their_model_and_effort() {
        let setup = AgentSetup {
            model: Some("gpt-6.2-sol".into()),
            effort: Some("xhigh".into()),
        };
        let argv = agent_argv(AgentKind::Codex, &setup);
        assert_eq!(argv[1..4], ["-l", "-i", "-c"]);
        assert_eq!(
            argv[4],
            "exec codex -m gpt-6.2-sol -c model_reasoning_effort=xhigh"
        );
        let claude = agent_argv(AgentKind::Claude, &AgentSetup::default());
        assert_eq!(claude[4], "exec claude");
    }

    /// Runs round 1 of `n` claude reviewers until the lead is briefed:
    /// every pane (the lead's too) must find room on the review's board.
    fn lead_is_briefed_after(n: usize) {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentChoice::from(AgentKind::Claude); n];
        let (mut runner, _actions, _updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, n);
        let reviewers: Vec<PartId> = runner.engine.session().reviewer_ids().cloned().collect();
        for part in &reviewers {
            fake.report(runner.panes[part].id, "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let reply_dir = runner.engine.reply_dir();
        for part in &reviewers {
            assert_eq!(fake.pasted(runner.panes[part].id), 1, "{part} not briefed");
            fs::write(
                store::reply_path(&reply_dir, 1, part),
                r#"{"summary": "fine"}"#,
            )
            .unwrap();
        }
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(&mut runner, &mut conn);
        assert_eq!(
            runner.engine.session().round(1).unwrap().phase,
            Phase::Consolidating
        );
        step(&mut runner, &mut conn);
        let lead = PartId::new("lead");
        let lead_pane = runner.panes[&lead].id;
        fake.report(lead_pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(lead_pane), 1, "the lead was not briefed");
        let w = fake.0.borrow();
        assert_eq!(w.boards_opened, 1, "one board holds every agent");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn six_reviewers_and_the_lead_fit_on_one_board() {
        lead_is_briefed_after(6);
    }

    #[test]
    fn two_reviewers_and_the_lead_fit_on_one_board() {
        lead_is_briefed_after(2);
    }

    #[test]
    fn round_control_reaches_the_engine_from_the_screen() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, _updates) = rig_on(
            &fake,
            new_start(
                &root,
                vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ),
        );
        actions.send(Action::SetHold { hold: true }).unwrap();
        actions.send(Action::Extend { by: 3 }).unwrap();
        actions
            .send(Action::Intervene {
                to: vec![PartId::new("codex")],
                body: "認可も見て".into(),
            })
            .unwrap();
        runner.drain_actions(None).unwrap();
        let s = runner.engine.session();
        assert!(s.hold);
        assert_eq!(s.max_rounds, 5);
        assert_eq!(s.messages.last().unwrap().body, "認可も見て");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac19_an_id_taken_anywhere_moves_on_and_a_refused_start_removes_only_its_own() {
        let root = temp_root();
        let work = root.join("work");
        // Another review holds rev-5 in the store, another data dir holds
        // rev-5-1 in this workdir.
        fs::create_dir_all(root.join("store/rev-5")).unwrap();
        fs::write(root.join("store/rev-5/events.jsonl"), "theirs").unwrap();
        fs::create_dir_all(work.join(".noa/reviews/rev-5-1")).unwrap();
        fs::write(work.join(".noa/reviews/rev-5-1/x"), "theirs").unwrap();
        let (id, dir, reply) = reserve(&root.join("store"), Some(&work), 5).unwrap();
        assert_eq!(id.as_str(), "rev-5-2");
        assert_eq!((dir.is_dir(), reply.is_dir()), (true, true));
        assert!(
            !root.join("store/rev-5-1").exists(),
            "a half-reserved id stayed"
        );

        // A start refused after the reservation (nothing to review) takes
        // away its own directories and leaves the others alone.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let new = NewReview {
            auto: None,
            request: "r".into(),
            target: Target {
                kind: TargetKind::Files,
                path: Some(empty.clone()),
                base: None,
                head: Head::Worktree,
                paths: Vec::new(),
                text: None,
                workdir: None,
            },
            max_rounds: 1,
            round_cap: review::model::types::MAX_MAX_ROUNDS,
            lead: AgentKind::Claude.into(),
            panel: vec![
                reviewer(AgentKind::Claude.into()),
                reviewer(AgentKind::Codex.into()),
            ],
            root: root.join("store"),
        };
        assert!(new_review(new, CONFIG, 5).is_err());
        let mut left: Vec<String> = fs::read_dir(root.join("store"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["rev-5", "rev-5-2"]);
        assert_eq!(
            fs::read_to_string(root.join("store/rev-5/events.jsonl")).unwrap(),
            "theirs"
        );
        assert_eq!(
            fs::read_to_string(work.join(".noa/reviews/rev-5-1/x")).unwrap(),
            "theirs"
        );
        assert!(!empty.join(".noa/reviews").read_dir().unwrap().any(|_| true));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac20_a_round_folder_or_snapshot_swapped_for_a_link_is_never_read_through() {
        use std::os::unix::fs::symlink;
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        // The round folder now leads elsewhere, where a valid reply waits.
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("codex.json"), BLIND).unwrap();
        fs::remove_dir_all(store::round_dir(&dir, 1)).unwrap();
        symlink(&elsewhere, store::round_dir(&dir, 1)).unwrap();
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir: dir.clone() });
        let mut conn = fake.clone();
        for _ in 0..2 {
            step(&mut runner, &mut conn);
            fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        }
        step(&mut runner, &mut conn);
        let codex = runner
            .engine
            .session()
            .round(1)
            .unwrap()
            .brief(&PartId::new("codex"))
            .unwrap()
            .clone();
        assert!(
            codex.receipt.replied.is_none(),
            "a reply behind a link was taken in"
        );
        assert!(
            codex
                .receipt
                .rejected
                .as_deref()
                .is_some_and(|r| r.contains("読めません")),
            "{:?}",
            codex.receipt
        );
        assert_eq!(
            fs::read_to_string(elsewhere.join("codex.json")).unwrap(),
            BLIND
        );
        drop(runner);
        wait_unlocked(&dir);

        // The snapshot folder swapped for a link to a faithful copy: refused.
        fs::remove_file(store::round_dir(&dir, 1)).unwrap();
        let copy = root.join("copy");
        fs::rename(store::target_dir(&dir), &copy).unwrap();
        symlink(&copy, store::target_dir(&dir)).unwrap();
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("場所が変わっています"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_reply_that_is_not_utf8_is_turned_down_and_counts_against_the_retries() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":claude");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let claude = PartId::new("claude");
        let path = store::reply_path(&runner.engine.reply_dir(), 1, &claude);
        let receipt = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .brief(&claude)
                .unwrap()
                .receipt
                .clone()
        };
        for n in 1..=CONFIG.max_retries + 1 {
            fs::write(&path, b"{\"summary\": \"\xff\xfe\"}").unwrap();
            step(&mut runner, &mut conn);
            fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
            step(&mut runner, &mut conn);
            let r = receipt(&runner);
            if n <= CONFIG.max_retries {
                assert_eq!(r.retries, n, "{r:?}");
                assert!(
                    r.rejected.as_deref().is_some_and(|e| e.contains("UTF-8")),
                    "{r:?}"
                );
                assert!(!path.exists(), "the unreadable reply was not cleared");
            } else {
                assert!(r.failed.is_some(), "it never failed: {r:?}");
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_is_typed_into_a_pane_back_at_its_shell_and_a_refused_enter_sends_again() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":claude");
        let claude = PartId::new("claude");
        let sent = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .brief(&claude)
                .unwrap()
                .receipt
                .sent
        };
        // The agent leaves right after the paste: its Enter is refused.
        fake.0.borrow_mut().exits_after_paste = Some(pane);
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(pane), 1);
        assert!(
            sent(&runner).is_none(),
            "a brief without its Enter counted as sent"
        );
        assert!(
            !fake
                .0
                .borrow()
                .typed
                .iter()
                .any(|(p, t)| *p == pane && t == "\r")
        );
        // While the pane is at its shell nothing reaches it.
        for _ in 0..3 {
            fake.report(pane, "input", None);
            step(&mut runner, &mut conn);
        }
        assert_eq!(fake.pasted(pane), 1);
        // The agent is back: the brief goes again, whole, with its Enter.
        fake.0.borrow_mut().shell.clear();
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(pane), 2);
        assert!(sent(&runner).is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_brief_too_long_for_a_control_frame_goes_through_a_file() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let claude = PartId::new("claude");
        // Quotes and newlines double in JSON: the encoded size decides.
        let body = "\"\n".repeat(BRIEF_PASTE_MAX / 3);
        assert!(body.len() < BRIEF_PASTE_MAX);
        let output = review::engine::Output {
            batches: Vec::new(),
            effects: vec![Effect::SendBrief {
                round: 1,
                part: claude.clone(),
                body: body.clone(),
            }],
        };
        assert!(runner.apply_output(output).is_ok());
        let path = brief_file(&runner.engine.reply_dir(), 1, &claude);
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        let pane = fake.pane_of(":claude");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let typed: Vec<String> = fake
            .0
            .borrow()
            .typed
            .iter()
            .filter(|(p, t)| *p == pane && t != "\r")
            .map(|(_, t)| t.clone())
            .collect();
        assert_eq!(typed, [brief_pointer(&path)]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_state_change_whose_notification_was_lost_reaches_the_engine_on_the_full_sync() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":claude");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let claude = PartId::new("claude");
        let started = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .brief(&claude)
                .unwrap()
                .receipt
                .started
        };
        assert!(started(&runner).is_none());
        // The agent starts working; the notification is lost and Noa says
        // only that some were dropped.
        {
            let mut w = fake.0.borrow_mut();
            let p = w.panes.get_mut(&pane).unwrap();
            p.seq += 1;
            p.state = Some("running".into());
            w.events.push_back(Changes {
                dropped: true,
                ..Changes::default()
            });
        }
        step(&mut runner, &mut conn);
        assert!(
            started(&runner).is_some(),
            "the engine never heard the agent start"
        );
        let _ = fs::remove_dir_all(root);
    }

    mod timeline {
        use super::*;
        use noa_ipc::ClientError;
        use noa_ipc::timeline::{
            PostParams, PostResult, RegisterRunParams, RegisterRunResult, Seq,
        };
        use noa_ipc::timeline_outbox::PostSink;

        #[derive(Default)]
        struct World {
            posts: Vec<PostParams>,
            runs: Vec<RegisterRunParams>,
        }

        #[derive(Clone, Default)]
        struct Tl(Rc<RefCell<World>>);

        impl PostSink for Tl {
            fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
                let mut w = self.0.borrow_mut();
                w.posts.push(p.clone());
                Ok(PostResult {
                    created_at: "2026-11-26T00:00:00Z".into(),
                    duplicate: false,
                    post_id: format!("po-{:016x}", w.posts.len()),
                    seq: Seq(w.posts.len() as u64),
                })
            }
        }

        impl TimelineApi for Tl {
            fn supports(&self) -> bool {
                true
            }

            fn register_run(
                &mut self,
                p: &RegisterRunParams,
            ) -> Result<RegisterRunResult, ClientError> {
                self.0.borrow_mut().runs.push(p.clone());
                Ok(RegisterRunResult {
                    board_id: "bd-0000000000000001".into(),
                    created: true,
                    producer_id: p.publish_policy.producer_id.clone().unwrap(),
                    run_id: "rn-0000000000000001".into(),
                    worker_id: None,
                })
            }
        }

        const VERDICT: &str = r#"{"summary": "Ready with one fix.", "ops": [
          {"op": "add", "id": "F1", "from": ["claude/1", "codex/1"], "claim": "No rollback", "severity": "medium", "category": "correctness", "confidence": "high", "status": "agreed", "rationale": "Both."}],
          "verdict": {"decision": "approve_with_fixes", "rationale": "One fix.", "must_fix": ["F1"]}}"#;

        /// Runs a two-reviewer, one-round review to the lead's verdict.
        fn to_verdict(link: TimelineLink) -> (Runner<Fake, Fake>, Receiver<Update>, Fake, PathBuf) {
            let fake = fake();
            let root = temp_root();
            let (mut runner, _actions, updates) = open_wired_with(
                &fake,
                new_start_on(
                    &root,
                    document("# Plan\n\n1. Move the data\n"),
                    vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                    1,
                ),
                None,
                Some(link),
            )
            .unwrap();
            let mut conn = fake.clone();
            assert!(runner.begin(&mut conn).is_ok());
            step(&mut runner, &mut conn);
            for part in [":claude", ":codex"] {
                fake.report(fake.pane_of(part), "input", None);
            }
            step(&mut runner, &mut conn);
            step(&mut runner, &mut conn);
            for part in ["claude", "codex"] {
                reply(&fake, &mut runner, &mut conn, &PartId::new(part), BLIND);
            }
            step(&mut runner, &mut conn);
            fake.report(fake.pane_of(":lead"), "input", None);
            step(&mut runner, &mut conn);
            step(&mut runner, &mut conn);
            reply(&fake, &mut runner, &mut conn, &PartId::new("lead"), VERDICT);
            (runner, updates, fake, root)
        }

        fn statuses(updates: &Receiver<Update>) -> Vec<Status> {
            updates
                .try_iter()
                .filter_map(|u| match u {
                    Update::Timeline(status) => Some(status),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn ac5_the_verdict_is_posted_once_after_it_is_logged_from_the_board() {
            let tl = Tl::default();
            let outbox = temp_root();
            let shared = tl.clone();
            let link = TimelineLink {
                outbox_root: outbox.clone(),
                connect: Box::new(move || Ok(Box::new(shared.clone()) as Box<dyn TimelineApi>)),
            };
            let (runner, updates, _fake, root) = to_verdict(link);
            let session = runner.engine.session();
            assert!(session.verdict().is_some());
            let world = tl.0.borrow();
            assert_eq!(world.posts.len(), 1, "one post, after the verdict only");
            let post = &world.posts[0];
            assert_eq!(post.request_id, format!("review-{}-verdict-r1", session.id));
            assert_eq!(post.post.summary, "Ready with one fix.");
            assert_eq!(world.runs.len(), 1);
            assert_eq!(
                world.runs[0].orchestrator_pane_id.map(|p| p.0),
                runner.board,
                "registered from the review's board tab"
            );
            // Pending while the post is on its way, then where it landed.
            assert_eq!(
                statuses(&updates),
                [
                    Status::Pending,
                    Status::Published {
                        seq: 1,
                        post_id: "po-0000000000000001".into()
                    }
                ]
            );
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outbox);
        }

        #[test]
        fn ac5_a_timeline_that_cannot_be_reached_leaves_the_review_as_it_is() {
            let outbox = temp_root();
            let link = TimelineLink {
                outbox_root: outbox.clone(),
                connect: Box::new(|| Err("Noa に接続できません".into())),
            };
            let (runner, updates, _fake, root) = to_verdict(link);
            let session = runner.engine.session();
            assert!(session.closed && session.verdict().is_some());
            assert!(store::report_path(runner.engine.dir()).exists());
            assert!(matches!(
                statuses(&updates).as_slice(),
                [Status::Pending, Status::Failed { message }] if message == "Noa に接続できません"
            ));
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outbox);
        }

        // Review defect 8: the run exists from the session's start, before
        // any verdict, registered from the review's board tab.
        #[test]
        fn ac5_the_run_is_registered_when_the_review_starts() {
            let tl = Tl::default();
            let outbox = temp_root();
            let shared = tl.clone();
            let link = TimelineLink {
                outbox_root: outbox.clone(),
                connect: Box::new(move || Ok(Box::new(shared.clone()) as Box<dyn TimelineApi>)),
            };
            let fake = fake();
            let root = temp_root();
            let (mut runner, _actions, updates) = open_wired_with(
                &fake,
                new_start_on(
                    &root,
                    document("# Plan\n\n1. Move the data\n"),
                    vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                    1,
                ),
                None,
                Some(link),
            )
            .unwrap();
            let mut conn = fake.clone();
            assert!(runner.begin(&mut conn).is_ok());
            step(&mut runner, &mut conn);
            assert!(runner.board.is_some());
            assert_eq!(review::timeline::confirmed(runner.engine.session()), 0);
            let world = tl.0.borrow();
            assert_eq!(world.runs.len(), 1, "registered before any verdict");
            assert_eq!(
                world.runs[0].orchestrator_pane_id.map(|p| p.0),
                runner.board
            );
            assert!(world.posts.is_empty());
            assert_eq!(
                Publisher::existing(&outbox, runner.engine.session().id.as_str())
                    .unwrap()
                    .and_then(|p| p.binding().cloned())
                    .map(|b| b.run_id),
                Some("rn-0000000000000001".to_owned()),
                "saved for a restart"
            );
            assert!(
                statuses(&updates).is_empty(),
                "a registration alone shows nothing"
            );
            drop(world);
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outbox);
        }
    }
}
