//! Runs one competition against a live Noa, on a thread of its own.
//!
//! Opens a Noa tab for the competition, spawns every judge and contestant
//! as interactive agents in splits, creates each contestant's private entry
//! directory (`entries/<part>/`), snapshots deliverables on every submission
//! (`r<N>/artifacts/<part>/`), regenerates `compare.html`, and feeds back
//! deliveries, agent states, reply files, and user actions to the [`Engine`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use competition::engine::{AgentState, Config, Effect, Engine, EngineError, Input, Setup};
use competition::model::export;
use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, Millis, PartId, Phase, RoundNo, Session, SessionId,
};
use competition::model::viewer::ViewerCache;
use competition::store::{self, Store};
use competition::{check, preview, shots};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    New {
        request: String,
        workdir: Option<PathBuf>,
        max_rounds: Option<RoundNo>,
        reveal_interval: RoundNo,
        /// The judge panel in seat order, lead first.
        judges: Vec<AgentChoice>,
        panel: Vec<AgentChoice>,
        root: PathBuf,
    },
    /// Continues the competition logged in `dir` (`Store::open` +
    /// `Engine::resume`): agents still in their panes are taken up again,
    /// the others are started anew.
    Resume { dir: PathBuf },
    /// Gives the competition logged in `dir` `by` more rounds, then goes on
    /// as [`Start::Resume`]: a finished competition that ran out of rounds
    /// continues. The extension is stored before any pane is started, and
    /// a refused one starts none.
    Extend { dir: PathBuf, by: RoundNo },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    SelectWinner {
        winner: PartId,
    },
    Retry,
    Close,
    Adopt {
        winner: PartId,
        agent: AgentKind,
    },
    /// Brings `part`'s pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Holds the competition after each judged round, or lets it go on.
    SetHold {
        hold: bool,
    },
    /// Starts the next round of a held competition.
    Continue,
    /// Raises the round limit by `by`.
    Extend {
        by: RoundNo,
    },
    /// Closes every judge's and contestant's pane in Noa. Refused until
    /// the competition has ended, so no brief is cut off.
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
        artifact_root: PathBuf,
    },
    Link(Link),
    Notice(String),
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
        .name("competition-live".into())
        .spawn(move || {
            run_session(
                start,
                IpcConnect,
                SystemClock,
                Config::default(),
                action_rx,
                update_tx,
                wake,
            )
        })
        .expect("spawn competition-live thread");
    SessionHandle { updates, actions }
}

pub fn run_session<C: Connect, K: Clock>(
    start: Start,
    connector: C,
    clock: K,
    config: Config,
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

/// `exec KIND "$@"` through the login shell, with `args` as the script's
/// positional parameters (`$0` is `name`), so a prompt is never spliced
/// into the script. Takes a POSIX login shell, as the first prompt always has.
fn launch_argv(kind: AgentKind, name: &str, args: Vec<String>) -> Vec<String> {
    let mut argv = vec![
        login_shell(),
        "-l".into(),
        "-i".into(),
        "-c".into(),
        format!("exec {} \"$@\"", kind.command()),
        name.to_owned(),
    ];
    argv.extend(args);
    argv
}

fn adopt_argv(kind: AgentKind, prompt: &str) -> Vec<String> {
    let mut args = Vec::new();
    if kind == AgentKind::Agy {
        args.push("-i".to_owned());
    }
    args.push(prompt.to_owned());
    launch_argv(kind, "noa-adopt", args)
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
    // Every judge (and the lead's verdict brief, which `brief` finds in
    // its scorecard's place) is in play while the round is judged.
    let phase_ok = if session.is_judge(part) {
        current.phase == Phase::Judging
    } else {
        matches!(current.phase, Phase::Briefing | Phase::Competing)
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
    seen: Option<(String, Millis)>,
}

struct Runner<C: Connect, K: Clock> {
    engine: Engine,
    store: Store,
    connector: C,
    clock: K,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: Rc<dyn Fn()>,
    panes: BTreeMap<PartId, Pane>,
    /// Panes asked to close whose outcome Noa could not tell (a timeout,
    /// a lost link): when one closes after all, it goes without a notice.
    closing: BTreeSet<u64>,
    /// The board tab the agents' floats are stowed on: opened with the first
    /// agent, taken back from an agent's `parent_pane_id` on a resume, and
    /// forgotten once it is gone.
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
    /// Problems reported by snapshot checks running on their own threads.
    check_notes: (Sender<String>, Receiver<String>),
    /// Cached per-snapshot file previews reused across `Effect::RefreshViewer`.
    viewer_cache: ViewerCache,
}

impl<C: Connect, K: Clock> Runner<C, K> {
    fn open(
        start: Start,
        connector: C,
        clock: K,
        config: Config,
        actions: Receiver<Action>,
        updates: Sender<Update>,
        wake: Rc<dyn Fn()>,
    ) -> Result<Self, String> {
        let now = clock.now();
        let resumed = matches!(start, Start::Resume { .. } | Start::Extend { .. });
        let (engine, store) = match start {
            Start::New {
                request,
                workdir,
                max_rounds,
                reveal_interval,
                judges,
                panel,
                root,
            } => {
                if let Some(workdir) = &workdir {
                    store::prepare_workdir(workdir)
                        .map_err(|e| format!("prepare {}: {e}", workdir.display()))?;
                }
                let id = unique_session_id(&root, now);
                let dir = store::session_dir(&root, &id);
                let setup = Setup {
                    request: &request,
                    workdir,
                    max_rounds,
                    reveal_interval,
                    judges: &judges,
                    panel: &panel,
                };
                let (engine, output) =
                    Engine::create_in(id, setup, &dir, config, now).map_err(|e| e.to_string())?;
                let store = Store::create(&dir, &output.batches[0])
                    .map_err(|e| format!("create {}: {e}", dir.display()))?;
                // Prepare each contestant's private output directory upfront.
                let reply_dir = engine.reply_dir();
                for part in engine.session().contestant_ids() {
                    let _ = fs::create_dir_all(store::entry_dir(&reply_dir, part));
                }
                let _ = store::write_viewer(&reply_dir, engine.session());
                (engine, store)
            }
            Start::Resume { dir } => open_log(dir, None, config, now)?,
            Start::Extend { dir, by } => open_log(dir, Some(by), config, now)?,
        };
        let runner = Self {
            engine,
            store,
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
            check_notes: mpsc::channel(),
            viewer_cache: ViewerCache::new(),
        };
        runner.emit_session();
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
            artifact_root: self.engine.reply_dir(),
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
            match conn.changes(POLL).map_err(link)? {
                Some(changes) => self.apply_changes(conn, changes)?,
                None => {}
            }
        }
        Ok(())
    }

    /// On each connection: takes up the panes; on the first, starts (or
    /// resumes) the competition.
    fn begin(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
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
        Ok(())
    }

    fn sync_panes(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let views = conn.panes().map_err(link)?;
        let by_id: BTreeMap<u64, &PaneView> = views.iter().map(|v| (v.pane_id, v)).collect();
        let mut alive = BTreeMap::new();
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
                if self.board.is_none() {
                    self.board = view.parent_pane_id.filter(|b| by_id.contains_key(b));
                }
                let mut pane = self
                    .panes
                    .get(&part)
                    .filter(|p| p.id == view.pane_id)
                    .cloned()
                    .unwrap_or_else(|| Pane::from_view(view));
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
        if self.board.is_some_and(|b| !by_id.contains_key(&b)) {
            self.board = None;
        }
        // Panes gone while the link was down need no quiet closing.
        let live: BTreeSet<u64> = self.panes.values().map(|p| p.id).collect();
        self.closing.retain(|id| live.contains(id));
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
                Ok(Action::SelectWinner { winner }) => {
                    self.feed_user(Input::UserSelectWinner { winner })?;
                }
                Ok(Action::Retry) => {
                    self.feed_user(Input::UserRetry)?;
                }
                Ok(Action::Close) => {
                    self.feed_user(Input::UserClose)?;
                }
                Ok(Action::Adopt { winner, agent }) => {
                    self.adopt_winner(conn.as_deref_mut(), &winner, agent);
                }
                Ok(Action::Focus { part }) => {
                    self.focus_pane(conn.as_deref_mut(), &part);
                }
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

    /// Closes the judges' and contestants' panes of an ended competition.
    /// A pane is forgotten before Noa reports it gone, so its closing is
    /// not told as an agent's pane closing. The adopt tab is the user's and
    /// stays open; more rounds added later start the agents in new panes.
    fn close_panes(&mut self, conn: Option<&mut C::Conn>) {
        if !self.engine.session().closed {
            self.notice("ペインはコンペが終わってから閉じられます");
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
        // The board goes once its agents are closed. Closing it closes any
        // float left on it, so it stays while one could not be closed.
        if failed.is_empty()
            && let Some(board) = self.board
        {
            match conn.close_pane(board) {
                Ok(()) | Err(NoaError::PaneGone) => self.board = None,
                Err(err) => failed.push(format!("ボード: {err}")),
            }
        }
        self.notice(if failed.is_empty() {
            format!("{closed} 個のペインを閉じました")
        } else {
            format!(
                "{closed} 個のペインを閉じました。閉じたか確かめられなかったもの: {}",
                failed.join(", ")
            )
        });
    }

    /// After a resume: what the agents taken up are doing now reaches the
    /// engine, so a turn started while the competition was stopped counts
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
        for brief in round.briefs.iter().chain(round.judge_briefs()) {
            let Some(digest) = brief.receipt.rejected_digest else {
                continue;
            };
            if brief.receipt.sent.is_none()
                && let Ok(Some(text)) = store::read_reply(&brief.reply_path)
                && store::digest(&text) == digest
            {
                let _ = store::clear_reply(&brief.reply_path);
            }
        }
    }

    /// After a resume: a brief that went out before the competition
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
            .chain(round.judge_briefs())
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

    fn adopt_winner(&mut self, conn: Option<&mut C::Conn>, winner: &PartId, agent: AgentKind) {
        // The button was drawn for the winner then; rounds added since
        // leave the winner undecided.
        if self.engine.session().winner.as_ref() != Some(winner) {
            self.notice(format!(
                "{winner} は現在の優勝者ではありません（新しい最終判定を待ってください）"
            ));
            return;
        }
        let Some(workdir) = self.engine.session().workdir.clone() else {
            self.notice("採用して実装するには作業ディレクトリを指定して開始してください");
            return;
        };
        let Some(conn) = conn else {
            self.notice("Noa に接続されていません");
            return;
        };
        let reply_dir = self.engine.reply_dir();
        let entry_dir = store::entry_dir(&reply_dir, winner);
        let task_path = reply_dir.join(format!("adopt-{winner}.md"));
        let task = export::adopt_task(self.engine.session(), winner, &entry_dir, &task_path);
        if let Err(e) = fs::create_dir_all(&reply_dir).and_then(|()| fs::write(&task_path, task)) {
            self.notice(format!("採用タスクの書き出しに失敗しました: {e}"));
            return;
        }
        let prompt = format!(
            "Read {} and integrate the winning competition entry from {} into this repository.",
            task_path.display(),
            entry_dir.display()
        );
        let spec = PaneSpec {
            argv: adopt_argv(agent, &prompt),
            cwd: workdir,
            label: format!("adopt:{}", self.engine.session().id),
        };
        match conn.spawn(Placement::Tab, &spec) {
            Ok(_) => self.notice(format!(
                "{} で優勝案 ({winner}) の採用実装タブを開きました",
                agent.command()
            )),
            Err(e) => self.notice(format!("実装タブの起動に失敗しました: {e}")),
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

    fn apply_output(&mut self, output: competition::engine::Output) -> Result<(), String> {
        if !output.batches.is_empty() {
            self.store
                .append_all(&output.batches)
                .map_err(|e| format!("write log: {e}"))?;
            self.emit_session();
        }
        let now = self.clock.now();
        for effect in output.effects {
            match effect {
                Effect::SendBrief { round, part, body } => {
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
                    if let Err(e) = store::prepare_reply(&path) {
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
                    let _ = store::clear_reply(&path);
                    self.watches.retain(|w| w.path != path);
                }
                Effect::RefreshViewer => {
                    let reply_dir = self.engine.reply_dir();
                    if let Err(e) = store::write_viewer_cached(
                        &reply_dir,
                        self.engine.session(),
                        &mut self.viewer_cache,
                    ) {
                        self.notice(format!("比較ビューアーの更新に失敗しました: {e}"));
                    }
                }
            }
        }
        Ok(())
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
        // competitions directory (never in a user's own repository).
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
        };
        // A board still listed is tried first; one gone is forgotten and the
        // next agent opens a new board.
        let mut placement = self.board.map_or(Placement::Board, Placement::Float);
        loop {
            if placement == Placement::Board
                && let Err(version) = conn.board_tab_support()
            {
                return Err(Fail::Fatal(no_board_tab(&version)));
            }
            match conn.spawn(placement, &spec) {
                Ok(spawned) => {
                    self.board = spawned.board;
                    self.panes.insert(part.clone(), Pane::new(spawned.pane));
                    return self.feed(Input::PaneSpawned {
                        part: part.clone(),
                        pane_id: spawned.pane,
                    });
                }
                Err(NoaError::PaneGone) if placement != Placement::Board => {
                    self.board = None;
                    placement = Placement::Board;
                }
                Err(NoaError::Refused(why)) => {
                    return Err(Fail::Link(format!("{part} のペインを開けません: {why}")));
                }
                Err(other) => return Err(link(other)),
            }
        }
    }

    fn poll_replies(&mut self) -> Result<(), Fail> {
        while let Ok(note) = self.check_notes.1.try_recv() {
            self.notice(note);
        }
        let now = self.clock.now();
        let mut ready = Vec::new();
        let mut idx = 0;
        while idx < self.watches.len() {
            let watch = &mut self.watches[idx];
            if !in_play(self.engine.session(), watch.round, &watch.part) {
                self.watches.remove(idx);
                continue;
            }
            match store::read_reply(&watch.path) {
                Ok(Some(text)) => match &watch.seen {
                    Some((prev, since)) if *prev == text => {
                        if now.saturating_sub(*since) >= REPLY_STABLE_FOR {
                            let w = self.watches.remove(idx);
                            ready.push((w.round, w.part, text));
                            continue;
                        }
                    }
                    _ => {
                        watch.seen = Some((text, now));
                    }
                },
                Ok(None) => {
                    watch.seen = None;
                }
                Err(_) => {}
            }
            idx += 1;
        }
        for (round, part, content) in ready {
            let artifacts = if self.engine.session().is_contestant(&part) {
                let reply_dir = self.engine.reply_dir();
                let src = store::entry_dir(&reply_dir, &part);
                let dst = store::artifact_snapshot_dir(&reply_dir, round, &part);
                match store::snapshot_artifacts(&src, &dst) {
                    Ok(files) => {
                        self.viewer_cache.invalidate(round, &part);
                        let checks = store::checks_dir(&reply_dir, round, &part);
                        self.check_snapshot(&part, &dst, &checks);
                        files
                    }
                    Err(e) => {
                        self.notice(format!(
                            "{part} の成果物スナップショットに失敗しました: {e}"
                        ));
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            self.feed(Input::ReplyFileWritten {
                round,
                part,
                content,
                artifacts,
                at: now,
            })?;
        }
        Ok(())
    }

    /// Starts the snapshot check on a submitted entry; see [`start_check`].
    fn check_snapshot(&self, part: &PartId, snapshot: &Path, out: &Path) {
        let notes = self.check_notes.0.clone();
        if let Err(e) = start_check(part, snapshot, out, shots::find_chrome(), notes) {
            self.notice(format!("{part} のスナップショット確認に失敗しました: {e}"));
        }
    }

    fn all_parts(&self) -> Vec<PartId> {
        let s = self.engine.session();
        s.judges()
            .map(|j| j.id.clone())
            .chain(s.contestant_ids().cloned())
            .collect()
    }

    fn pane_label(&self, part: &PartId) -> String {
        format!("comp:{}:{part}", self.engine.session().id)
    }
}

/// Opens the competition logged in `dir` to go on with it, after the
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
    competition::engine::check_resumable(&session, &dir)
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
    let reply_dir = engine.reply_dir();
    if let Some(out) = extension {
        // Stored before anything starts; the round it opens is briefed by
        // the start that follows the first connection. The viewer shows
        // the reopened competition (no winner) at once.
        store
            .append_all(&out.batches)
            .map_err(|e| format!("write log: {e}"))?;
        let _ = store::write_viewer(&reply_dir, engine.session());
    }
    for part in engine.session().contestant_ids() {
        let _ = fs::create_dir_all(store::entry_dir(&reply_dir, part));
    }
    Ok((engine, store))
}

fn unique_session_id(root: &Path, now: Millis) -> SessionId {
    let mut candidate = SessionId::at(now);
    let mut suffix = 1u32;
    while store::session_dir(root, &candidate).exists() {
        candidate = SessionId::new(format!("comp-{now}-{suffix}"));
        suffix += 1;
    }
    candidate
}

/// Runs the snapshot check on a submitted entry. The viewer copy is written
/// before this returns, so the viewer refresh that follows the accepted
/// submission frames it; the screenshots and manifest run on a thread of
/// their own, since they start a browser several times, which must not hold
/// up the runner. Problems come back on `notes`.
fn start_check(
    part: &PartId,
    snapshot: &Path,
    out: &Path,
    chrome: Option<PathBuf>,
    notes: Sender<String>,
) -> std::io::Result<()> {
    let preview = preview::write(snapshot)?;
    let (part, snapshot, out) = (part.clone(), snapshot.to_owned(), out.to_owned());
    std::thread::Builder::new()
        .name(format!("competition-check-{part}"))
        .spawn(move || {
            let note = match check::inspect_with(&snapshot, &out, preview, chrome) {
                Ok(inspection) => {
                    let problems = inspection.problems();
                    (!problems.is_empty()).then(|| {
                        format!(
                            "{part} のスナップショット確認で問題: {}",
                            problems.join(" / ")
                        )
                    })
                }
                Err(e) => Some(format!("{part} のスナップショット確認に失敗しました: {e}")),
            };
            if let Some(note) = note {
                let _ = notes.send(note);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The comparison viewer is refreshed as soon as a submission is
    /// accepted, and frames the viewer copy only if it exists by then.
    #[test]
    fn the_viewer_copy_exists_once_the_check_has_started() {
        let dir = std::env::temp_dir().join(format!(
            "noa-competition-start-check-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let snapshot = dir.join("snapshot");
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(snapshot.join("index.html"), "<p>hi</p>").unwrap();
        let (tx, _rx) = mpsc::channel();
        let part = PartId::new("claude");
        start_check(&part, &snapshot, &dir.join("checks"), None, tx).unwrap();
        assert!(preview::preview_path(&snapshot).is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    use super::super::ipc::Spawned;
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakePane {
        label: Option<String>,
        state: Option<String>,
        blocked: Option<String>,
        seq: u64,
        /// The board a float is stowed on.
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
        /// Board tabs opened, in order (each is also in `panes`).
        boards: Vec<u64>,
        /// Plain tabs opened (the adopt tab).
        plain_tabs: Vec<u64>,
        /// Every placement `spawn` was asked for.
        spawn_log: Vec<Placement>,
        /// A Noa that predates board tabs.
        old_noa: bool,
        /// Panes that are gone by the time they are asked for a paste.
        gone_on_paste: bool,
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
            Ok(w.panes.iter().map(|(id, p)| Fake::view(*id, p)).collect())
        }

        fn changes(&mut self, _wait: Duration) -> Result<Option<Changes>, NoaError> {
            let mut w = self.0.borrow_mut();
            w.rpc_changes += 1;
            Ok(w.events.pop_front())
        }

        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<Spawned, NoaError> {
            let mut w = self.0.borrow_mut();
            w.spawn_log.push(placement);
            let next = |w: &mut World| {
                let id = w.next_pane;
                w.next_pane += 1;
                id
            };
            let (board, parent) = match placement {
                Placement::Tab => (None, None),
                Placement::Float(board) => {
                    if !w.panes.contains_key(&board) {
                        return Err(NoaError::PaneGone);
                    }
                    (Some(board), Some(board))
                }
                Placement::Board => {
                    let board = next(&mut w);
                    w.boards.push(board);
                    w.panes.insert(
                        board,
                        FakePane {
                            label: Some(format!("board:{board}")),
                            state: None,
                            blocked: None,
                            seq: 0,
                            parent: None,
                        },
                    );
                    (Some(board), Some(board))
                }
            };
            let id = next(&mut w);
            if board.is_none() {
                w.plain_tabs.push(id);
            }
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
            Ok(Spawned { pane: id, board })
        }

        fn board_tab_support(&self) -> Result<(), String> {
            if self.0.borrow().old_noa {
                Err("0.9.15".to_owned())
            } else {
                Ok(())
            }
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
            self.0.borrow_mut().typed.push((pane, text.to_owned()));
            Ok(())
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
            if w.panes.remove(&pane).is_none() {
                return Err(NoaError::PaneGone);
            }
            // Closing a board closes the floats stowed on it.
            let floats: Vec<u64> = w
                .panes
                .iter()
                .filter(|(_, p)| p.parent == Some(pane))
                .map(|(id, _)| *id)
                .collect();
            for float in &floats {
                w.panes.remove(float);
            }
            w.closed.push(pane);
            w.events.push_back(Changes {
                removed: std::iter::once(pane).chain(floats).collect(),
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
            "noa-competition-live-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
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

    /// A runner opened on `start`, with its action and update channels.
    fn open_wired(fake: &Fake, start: Start) -> Result<Wired, String> {
        let (actions_tx, actions) = mpsc::channel();
        let (updates, updates_rx) = mpsc::channel();
        let runner = Runner::open(
            start,
            fake.clone(),
            fake.clone(),
            CONFIG,
            actions,
            updates,
            Rc::new(|| {}),
        )?;
        Ok((runner, actions_tx, updates_rx))
    }

    fn new_start(root: &Path, panel: Vec<AgentChoice>) -> Start {
        Start::New {
            request: "Design a music player UI".into(),
            workdir: None,
            max_rounds: Some(2),
            reveal_interval: 5,
            judges: vec![AgentKind::Claude.into()],
            panel,
            root: root.to_owned(),
        }
    }

    /// Opens a runner on `start` and runs its first connection.
    fn rig_on(fake: &Fake, start: Start) -> Wired {
        let (mut runner, actions, updates) =
            open_wired(fake, start).unwrap_or_else(|e| panic!("{e}"));
        let mut conn = fake.clone();
        assert!(runner.begin(&mut conn).is_ok());
        (runner, actions, updates)
    }

    fn rig(fake: &Fake, root: &Path) -> Runner<Fake, Fake> {
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        rig_on(fake, new_start(root, panel)).0
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

    /// Waits for `dir`'s log to be unlocked. A child process another test
    /// starts just then holds a copy of the closed log (and its lock) until
    /// it execs; `Store::open` waits that out too (`store::LOCK_GRACE`).
    /// Fails the test when it is still locked after 10 s: then something
    /// really holds it, which no test here should.
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

    /// A competition whose two contestants took their first brief, then
    /// stopped (the runner is dropped); returns its directory.
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

    #[test]
    fn a_resumed_competition_takes_up_the_agents_still_in_their_panes() {
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
        assert_eq!(runner.panes.len(), 2);

        // Its reply is taken in as it would have been without the stop.
        let claude = PartId::new("claude");
        let reply = store::reply_path(&runner.engine.reply_dir(), 1, &claude);
        fs::write(&reply, "# Summary: done").unwrap();
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(&mut runner, &mut conn);
        let round = runner.engine.session().round(1).unwrap();
        assert!(round.submissions.contains_key(&claude));
        drop(runner);
        assert!(
            store::read_session(&dir)
                .unwrap()
                .round(1)
                .unwrap()
                .submissions
                .contains_key(&claude)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resumed_competition_starts_agents_whose_panes_are_gone_again() {
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
        // The stop lost the brief, not the agent: no retry is counted.
        let round = runner.engine.session().round(1).unwrap();
        let brief = round.brief(&PartId::new("claude")).unwrap();
        assert_eq!(brief.receipt.retries, 0);
        assert!(brief.receipt.failed.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_id_reused_by_another_pane_is_not_taken_for_the_agent() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        // Noa restarted: the ids now name panes that are not the agents'.
        for pane in fake.0.borrow_mut().panes.values_mut() {
            pane.label = Some("zsh".into());
        }
        let (runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert!(runner.panes.is_empty(), "{:?}", runner.panes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resume_checks_the_setup_of_a_log_edited_by_hand() {
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
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("再開できません"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_competition_running_elsewhere_is_not_resumed_twice() {
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
    fn panes_close_together_only_after_the_competition_ends() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, actions, updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let contestants = [fake.pane_of(":claude"), fake.pane_of(":codex")];
        let _ = notices(&updates);

        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(fake.0.borrow().closed.is_empty(), "refused while running");
        assert!(notices(&updates)[0].contains("コンペが終わってから"));

        actions.send(Action::Close).unwrap();
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        let board = fake.0.borrow().boards[0];
        let mut closed = fake.0.borrow().closed.clone();
        closed.sort_unstable();
        let mut want = contestants.to_vec();
        want.push(board);
        want.sort_unstable();
        assert_eq!(closed, want, "the agents and then the board");
        assert_eq!(fake.0.borrow().closed.last(), Some(&board));
        assert!(runner.panes.is_empty());
        assert_eq!(runner.board, None);
        let spawned = fake.0.borrow().spawned;
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        let told = notices(&updates);
        assert!(
            told.contains(&"2 個のペインを閉じました".to_owned()),
            "{told:?}"
        );
        assert!(
            !told.iter().any(|n| n.contains("のペインが閉じられました")),
            "closing them is no agent's pane closing: {told:?}"
        );
        assert_eq!(fake.0.borrow().spawned, spawned, "nothing is started again");

        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(notices(&updates), ["閉じるペインはありません"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_whose_close_timed_out_closes_later_without_a_notice() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, actions, updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
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
    fn focusing_an_agent_brings_its_pane_forward_or_says_why_not() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, actions, updates) = rig_on(&fake, new_start(&root, panel));
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

        // The judge has no pane before its round is judged.
        actions
            .send(Action::Focus {
                part: PartId::new("judge"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(fake.0.borrow().focused.len(), 1);
        assert!(notices(&updates)[0].contains("judge のペインがありません"));

        // A pane closed behind the runner's back.
        fake.0.borrow_mut().panes.remove(&claude);
        actions
            .send(Action::Focus {
                part: PartId::new("claude"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("閉じられています"));
        assert!(!runner.panes.contains_key(&PartId::new("claude")));

        actions
            .send(Action::Focus {
                part: PartId::new("codex"),
            })
            .unwrap();
        runner.drain_actions(None).unwrap();
        assert!(notices(&updates)[0].contains("接続されていません"));
        let _ = fs::remove_dir_all(root);
    }

    fn fake() -> Fake {
        Fake(Rc::new(RefCell::new(World {
            now: 1_000,
            next_pane: 100,
            ..World::default()
        })))
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
    fn a_resend_held_through_a_turn_is_dropped_when_the_turn_ends_in_a_reply() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        fake.report(claude, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 1);

        // The turn outlasts the reply limit, so a resend waits for it to end.
        fake.report(claude, "running", None);
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += CONFIG.reply_timeout + 1;
        step(&mut runner, &mut conn);

        // The turn ends with the report written: it is taken in, and the
        // brief is not pasted into the agent again.
        let reply = store::reply_path(&runner.engine.reply_dir(), 1, &PartId::new("claude"));
        fs::write(&reply, "# Summary: done").unwrap();
        fake.report(claude, "input", None);
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 1, "pasted after the agent replied");
        let brief = runner
            .engine
            .session()
            .round(1)
            .unwrap()
            .brief(&PartId::new("claude"))
            .unwrap()
            .clone();
        assert!(brief.receipt.replied.is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resent_brief_waits_while_the_agent_works_or_sits_on_a_prompt() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();

        // The panes are spawned, report that they wait for input, and take
        // their first brief.
        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        fake.report(claude, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 1);

        // The agent works past the reply limit: the engine resends, and the
        // brief waits for the turn to end instead of landing mid-turn.
        fake.report(claude, "running", None);
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += CONFIG.reply_timeout + 1;
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += READY_LIMIT + 1;
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 1, "pasted into a working agent");

        // A prompt shown later in the turn holds it too.
        fake.report(claude, "permission", Some("permission"));
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 1, "pasted into a prompt");

        // Once the agent is back at its input box, the brief goes out.
        fake.report(claude, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(claude), 2);
        let brief = runner
            .engine
            .session()
            .current_round()
            .unwrap()
            .brief(&PartId::new("claude"))
            .unwrap();
        assert!(brief.receipt.failed.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agents_start_on_their_model_and_effort() {
        let plain = agent_argv(AgentKind::Claude, &AgentSetup::default());
        assert!(plain[0].starts_with('/'));
        assert_eq!(&plain[1..], ["-l", "-i", "-c", "exec claude"]);
        let setup = AgentSetup {
            model: Some("gemini-4-neon-high".into()),
            effort: Some("xhigh".into()),
        };
        assert_eq!(
            agent_argv(AgentKind::Agy, &setup)[4],
            "exec agy --model gemini-4-neon-high --effort xhigh"
        );
        let effort_only = AgentSetup {
            model: None,
            effort: Some("ultra".into()),
        };
        assert_eq!(
            agent_argv(AgentKind::Codex, &effort_only)[4],
            "exec codex -c model_reasoning_effort=ultra"
        );
        assert_eq!(
            &adopt_argv(AgentKind::Agy, "go")[4..],
            ["exec agy \"$@\"", "noa-adopt", "-i", "go"]
        );
    }

    /// The script an agent is started with, run by a shell that is not
    /// POSIX (csh here; fish has no `$@` either), with `echo` standing in
    /// for the agent: it must still start, with every flag.
    fn run_under_csh(argv: &[String], kind: AgentKind) -> Option<String> {
        let csh = std::path::Path::new("/bin/csh");
        if !csh.exists() {
            return None;
        }
        let script = argv[4].replacen(&format!("exec {}", kind.command()), "exec /bin/echo", 1);
        let out = std::process::Command::new(csh)
            .arg("-f")
            .arg("-c")
            .arg(&script)
            .args(&argv[5..])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }

    #[test]
    fn agents_start_under_a_login_shell_that_is_not_posix() {
        let setup = AgentSetup {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        let Some(out) = run_under_csh(&agent_argv(AgentKind::Claude, &setup), AgentKind::Claude)
        else {
            return;
        };
        assert_eq!(out, "--model opus --effort high");
        let codex = run_under_csh(&agent_argv(AgentKind::Codex, &setup), AgentKind::Codex);
        assert_eq!(codex.unwrap(), "-m opus -c model_reasoning_effort=high");
        let plain = agent_argv(AgentKind::Agy, &AgentSetup::default());
        assert_eq!(run_under_csh(&plain, AgentKind::Agy).unwrap(), "");
    }

    /// Runs round 1 of `n` claude entries until the judge is briefed: every
    /// pane (the judge's too) must float on the session's one board.
    fn judge_is_briefed_after(n: usize) {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentChoice::from(AgentKind::Claude); n];
        let (mut runner, _actions, _updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, n);
        let contestants: Vec<PartId> = runner.engine.session().contestant_ids().cloned().collect();
        for part in &contestants {
            fake.report(runner.panes[part].id, "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let reply_dir = runner.engine.reply_dir();
        for part in &contestants {
            assert_eq!(fake.pasted(runner.panes[part].id), 1, "{part} not briefed");
            fs::write(store::reply_path(&reply_dir, 1, part), "# Summary: done").unwrap();
        }
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(&mut runner, &mut conn);
        assert_eq!(
            runner.engine.session().round(1).unwrap().phase,
            Phase::Judging
        );
        step(&mut runner, &mut conn);
        let judge = PartId::new(JUDGE_ID_FOR_TESTS);
        let judge_pane = runner.panes[&judge].id;
        fake.report(judge_pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(judge_pane), 1, "the judge was not briefed");
        let w = fake.0.borrow();
        assert_eq!(w.spawned, n + 1);
        assert_eq!(w.boards.len(), 1, "one board for the whole session");
        assert!(w.plain_tabs.is_empty());
        assert!(
            w.panes
                .iter()
                .filter(|(id, _)| !w.boards.contains(id))
                .all(|(_, p)| p.parent == Some(w.boards[0]))
        );
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    const JUDGE_ID_FOR_TESTS: &str = competition::model::types::JUDGE_ID;

    #[test]
    fn the_first_agent_opens_a_board_and_the_rest_float_on_it() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, _actions, _updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let w = fake.0.borrow();
        assert_eq!(w.boards.len(), 1);
        let board = w.boards[0];
        assert_eq!(w.spawn_log, [Placement::Board, Placement::Float(board)]);
        for pane in runner.panes.values() {
            assert_eq!(w.panes[&pane.id].parent, Some(board));
        }
        assert_eq!(runner.board, Some(board));
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_board_that_is_gone_is_replaced_by_a_new_one() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, _actions, _updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let old = fake.0.borrow().boards[0];
        // The user closes the board tab: it and its floats are gone, but the
        // runner has not heard of it yet.
        conn.close_pane(old).unwrap();
        runner.panes.clear();
        let part = PartId::new("claude");
        assert_eq!(runner.board, Some(old));
        assert!(runner.spawn_pane(&mut conn, &part).is_ok());
        let w = fake.0.borrow();
        assert_eq!(w.boards.len(), 2);
        let new = w.boards[1];
        assert_eq!(
            w.spawn_log[2..],
            [Placement::Float(old), Placement::Board],
            "the dead board is tried once, then replaced"
        );
        assert_eq!(runner.board, Some(new));
        assert_eq!(w.panes[&runner.panes[&part].id].parent, Some(new));
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resume_recovers_the_board_from_the_agents_it_takes_back() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        let board = fake.0.borrow().boards[0];
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert_eq!(runner.board, Some(board));
        // An agent started after the resume joins that board.
        let mut conn = fake.clone();
        let part = PartId::new("claude");
        runner.panes.remove(&part);
        assert!(runner.spawn_pane(&mut conn, &part).is_ok());
        let w = fake.0.borrow();
        assert_eq!(w.boards, [board], "no second board");
        assert_eq!(w.spawn_log.last(), Some(&Placement::Float(board)));
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_older_noa_starts_no_agents() {
        let fake = fake();
        fake.0.borrow_mut().old_noa = true;
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, _actions, _updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        let Err(Fail::Fatal(why)) = runner.deliver_sends(&mut conn) else {
            panic!("the agents were started");
        };
        assert!(why.contains("0.9.15") && why.contains("0.9.16"), "{why}");
        let w = fake.0.borrow();
        assert!(w.spawn_log.is_empty() && w.panes.is_empty());
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn four_entries_and_the_judge_fit_on_one_board() {
        judge_is_briefed_after(4);
    }

    #[test]
    fn six_entries_and_the_judge_fit_on_one_board() {
        judge_is_briefed_after(6);
    }

    /// A new competition of `n` claude entries judged by `judges` claude
    /// judges (`judge`, `judge-2`, …).
    fn panel_start(root: &Path, n: usize, judges: usize) -> Start {
        Start::New {
            request: "Design a music player UI".into(),
            workdir: None,
            max_rounds: Some(2),
            reveal_interval: 5,
            judges: vec![AgentKind::Claude.into(); judges],
            panel: vec![AgentKind::Claude.into(); n],
            root: root.to_owned(),
        }
    }

    /// Runs round 1 until every contestant has submitted and every judge's
    /// pane is up (not yet briefed).
    fn run_to_judging(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        step(runner, conn);
        let contestants: Vec<PartId> = runner.engine.session().contestant_ids().cloned().collect();
        for part in &contestants {
            fake.report(runner.panes[part].id, "input", None);
        }
        step(runner, conn);
        step(runner, conn);
        let reply_dir = runner.engine.reply_dir();
        for part in &contestants {
            assert_eq!(fake.pasted(runner.panes[part].id), 1, "{part} not briefed");
            fs::write(store::reply_path(&reply_dir, 1, part), "# Summary: done").unwrap();
        }
        step(runner, conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(runner, conn);
        assert_eq!(
            runner.engine.session().round(1).unwrap().phase,
            Phase::Judging
        );
        step(runner, conn);
    }

    fn judge_ids(runner: &Runner<Fake, Fake>) -> Vec<PartId> {
        runner
            .engine
            .session()
            .judges()
            .map(|j| j.id.clone())
            .collect()
    }

    /// Reports every judge's pane at its prompt and lets the briefs go out.
    fn brief_judges(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        for judge in judge_ids(runner) {
            fake.report(runner.panes[&judge].id, "input", None);
        }
        step(runner, conn);
        step(runner, conn);
    }

    #[test]
    fn six_entries_and_three_judges_fit_on_one_board() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(&fake, panel_start(&root, 6, 3));
        let mut conn = fake.clone();
        run_to_judging(&fake, &mut runner, &mut conn);
        brief_judges(&fake, &mut runner, &mut conn);
        let id = runner.engine.session().id.clone();
        for judge in judge_ids(&runner) {
            let pane = runner.panes[&judge].id;
            assert_eq!(fake.pasted(pane), 1, "{judge} was not briefed");
            let label = fake.0.borrow().panes[&pane].label.clone();
            assert_eq!(label, Some(format!("comp:{id}:{judge}")));
        }
        let w = fake.0.borrow();
        assert_eq!(w.spawned, 9);
        assert_eq!(w.boards.len(), 1, "one board holds all nine");
        assert_eq!(
            w.panes
                .values()
                .filter(|p| p.parent == Some(w.boards[0]))
                .count(),
            9
        );
        drop(w);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resume_takes_up_every_judges_pane() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(&fake, panel_start(&root, 2, 3));
        let mut conn = fake.clone();
        run_to_judging(&fake, &mut runner, &mut conn);
        brief_judges(&fake, &mut runner, &mut conn);
        let judges = judge_ids(&runner);
        let panes: BTreeMap<PartId, u64> = judges
            .iter()
            .map(|j| (j.clone(), runner.panes[j].id))
            .collect();
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        // judge-3's pane closed while the competition was stopped.
        let lost = judges[2].clone();
        fake.0.borrow_mut().panes.remove(&panes[&lost]);
        let spawned = fake.0.borrow().spawned;

        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        for judge in &judges[..2] {
            assert_eq!(runner.panes[judge].id, panes[judge], "{judge} not taken up");
        }
        step(&mut runner, &mut conn);
        assert_eq!(
            fake.0.borrow().spawned,
            spawned + 1,
            "only judge-3 starts again"
        );
        let pane = runner.panes[&lost].id;
        assert_ne!(pane, panes[&lost]);
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        // Briefs out before the stop are not pasted again; the lost one is,
        // without counting a retry.
        for judge in &judges[..2] {
            assert_eq!(fake.pasted(panes[judge]), 1, "{judge} briefed again");
        }
        assert_eq!(fake.pasted(pane), 1, "judge-3 was not briefed again");
        let round = runner.engine.session().round(1).unwrap();
        for judge in &judges {
            let receipt = &round.brief(judge).unwrap().receipt;
            assert_eq!(receipt.retries, 0, "{judge}");
            assert!(receipt.failed.is_none(), "{judge}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_dead_judge_pane_is_restarted_for_its_brief() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig_on(&fake, panel_start(&root, 2, 3));
        let mut conn = fake.clone();
        run_to_judging(&fake, &mut runner, &mut conn);
        let judges = judge_ids(&runner);
        assert_eq!(fake.0.borrow().spawned, 5);
        // judge-2's agent exits before it is briefed.
        let dead = runner.panes[&judges[1]].id;
        fake.0.borrow_mut().panes.remove(&dead);
        fake.0.borrow_mut().events.push_back(Changes {
            removed: vec![dead],
            ..Changes::default()
        });
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, 6);
        let again = runner.panes[&judges[1]].id;
        assert_ne!(again, dead);
        brief_judges(&fake, &mut runner, &mut conn);
        for judge in &judges {
            assert_eq!(
                fake.pasted(runner.panes[judge].id),
                1,
                "{judge} not briefed"
            );
        }
        assert_eq!(
            fake.0.borrow().spawned,
            6,
            "another judge was started again"
        );
        let round = runner.engine.session().round(1).unwrap();
        for judge in &judges {
            assert!(
                round.brief(judge).unwrap().receipt.sent.is_some(),
                "{judge}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_without_the_label_is_not_taken_for_the_agent_by_its_id() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        let claude = fake.pane_of(":claude");
        // Noa restarted: claude's old id now names an unlabelled shell, and
        // claude's own pane has another id.
        let mut w = fake.0.borrow_mut();
        let mut own = w.panes.remove(&claude).unwrap();
        w.panes.insert(
            claude,
            FakePane {
                label: None,
                state: Some("input".into()),
                blocked: None,
                seq: 0,
                parent: None,
            },
        );
        own.seq = 0;
        w.panes.insert(500, own);
        drop(w);
        let (runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert_eq!(runner.panes[&PartId::new("claude")].id, 500);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_resumed_agent_already_at_work_is_not_retried_for_not_starting() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_first_briefs(&fake, &root);
        // While stopped, claude started its turn (and codex sits on a prompt).
        fake.report(fake.pane_of(":claude"), "running", None);
        fake.report(fake.pane_of(":codex"), "permission", Some("permission"));
        fake.0.borrow_mut().events.clear();

        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        let mut conn = fake.clone();
        fake.0.borrow_mut().now += CONFIG.start_timeout + 1;
        step(&mut runner, &mut conn);
        let brief = |runner: &Runner<Fake, Fake>, part: &str| {
            let round = runner.engine.session().round(1).unwrap();
            round.brief(&PartId::new(part)).unwrap().receipt.clone()
        };
        let claude = brief(&runner, "claude");
        assert!(claude.started.is_some());
        assert_eq!(claude.retries, 0, "retried an agent at work");
        // A prompt holds the time limit however long it waits.
        fake.0.borrow_mut().now += 2 * CONFIG.reply_timeout;
        step(&mut runner, &mut conn);
        assert_eq!(
            brief(&runner, "codex").retries,
            0,
            "retried an agent on a prompt"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_last_try_lost_with_its_pane_is_sent_again_not_failed() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        let claude = PartId::new("claude");
        for _ in 0..CONFIG.max_retries {
            assert!(
                runner
                    .feed(Input::SendFailed {
                        round: 1,
                        part: claude.clone(),
                        error: "x".into(),
                    })
                    .is_ok()
            );
        }
        step(&mut runner, &mut conn);
        fake.report(fake.pane_of(":claude"), "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(fake.pane_of(":claude")), 1);
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        fake.0.borrow_mut().panes.clear();

        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":claude");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pasted(pane), 1, "the last try was not sent again");
        let brief = runner
            .engine
            .session()
            .round(1)
            .unwrap()
            .brief(&claude)
            .unwrap()
            .clone();
        assert!(brief.receipt.failed.is_none(), "{:?}", brief.receipt);
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
        let reply = store::reply_path(&runner.engine.reply_dir(), 1, &claude);
        fs::write(&reply, "  ").unwrap();
        step(&mut runner, &mut conn);
        fake.0.borrow_mut().now += REPLY_STABLE_FOR + 1;
        step(&mut runner, &mut conn);
        let retries = |runner: &Runner<Fake, Fake>| {
            let round = runner.engine.session().round(1).unwrap();
            round.brief(&claude).unwrap().receipt.retries
        };
        assert_eq!(retries(&runner), 1);
        // The stop came after the rejection was logged, before the file
        // was cleared.
        fs::write(&reply, "  ").unwrap();
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);

        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        assert!(!reply.exists(), "the turned-down reply is still there");
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
    fn a_finished_competition_is_not_resumed() {
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
        let err = open_wired(&fake, Start::Resume { dir }).err().unwrap();
        assert!(err.contains("終了しています"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_log_whose_reply_paths_lead_elsewhere_is_not_resumed() {
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

    /// A one-round competition of claude and codex, judged by claude, logged
    /// under `root` and run to its end by the engine: at its limit when
    /// `max_rounds` is 1, closed early by the judge otherwise.
    fn finished(root: &Path, max_rounds: RoundNo) -> PathBuf {
        finished_with(root, max_rounds, None, false)
    }

    /// [`finished`] in `workdir` (when given), held after each round when
    /// `hold`.
    fn finished_with(
        root: &Path,
        max_rounds: RoundNo,
        workdir: Option<PathBuf>,
        hold: bool,
    ) -> PathBuf {
        let id = SessionId::new("comp-1000");
        let dir = store::session_dir(root, &id);
        let setup = competition::engine::Setup {
            request: "x",
            max_rounds: Some(max_rounds),
            reveal_interval: 5,
            workdir,
            judges: &[AgentKind::Claude.into()],
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (mut engine, out) = Engine::create_in(id, setup, &dir, CONFIG, 1_000).unwrap();
        let mut log = Store::create(&dir, &out.batches[0]).unwrap();
        let mut feed = |engine: &mut Engine, input| {
            log.append_all(&engine.handle(input).unwrap().batches)
                .unwrap();
        };
        if hold {
            feed(&mut engine, Input::UserSetHold { hold, at: 1_000 });
        }
        feed(&mut engine, Input::Start { now: 1_000 });
        for part in ["claude", "codex"] {
            let part = PartId::new(part);
            feed(
                &mut engine,
                Input::ReplyFileWritten {
                    round: 1,
                    part,
                    content: "# Summary: done".into(),
                    artifacts: vec![],
                    at: 2_000,
                },
            );
        }
        let json = r#"{
  "overview": "ok",
  "evaluations": {
    "claude": { "score": 80, "strengths": "a", "feedback": "b" },
    "codex": { "score": 70, "strengths": "a", "feedback": "b" }
  },
  "ranking": ["claude", "codex"],
  "winner": "claude",
  "final_verdict": "claude wins",
  "close": true
}"#;
        feed(
            &mut engine,
            Input::ReplyFileWritten {
                round: 1,
                part: PartId::new(competition::model::types::JUDGE_ID),
                content: json.into(),
                artifacts: vec![],
                at: 3_000,
            },
        );
        assert!(engine.session().closed);
        // What the runner leaves: the viewer of the finished competition.
        store::write_viewer(&engine.reply_dir(), engine.session()).unwrap();
        dir
    }

    #[test]
    fn a_finished_competition_is_extended_before_any_pane_starts() {
        let fake = fake();
        let root = temp_root();
        let dir = finished(&root, 1);
        let (mut runner, _actions, _updates) = open_wired(
            &fake,
            Start::Extend {
                dir: dir.clone(),
                by: 2,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Stored, and nothing started yet.
        let stored = store::read_session(&dir).unwrap();
        assert!(!stored.closed);
        assert_eq!(stored.max_rounds, Some(3));
        assert_eq!(stored.winner, None);
        assert_eq!(stored.rounds.len(), 2);
        assert_eq!(fake.0.borrow().spawned, 0);

        let mut conn = fake.clone();
        assert!(runner.begin(&mut conn).is_ok());
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, 2);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} not briefed");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_finished_log_a_widget_from_before_the_lock_may_still_hold_is_not_extended() {
        let fake = fake();
        let root = temp_root();
        let dir = finished(&root, 1);
        // No marker, written just now: an older widget's finished screen
        // may still take a manual winner on it.
        fs::remove_file(dir.join(store::LOCKS_FILE)).unwrap();
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
        assert!(err.contains("旧版"), "{err}");
        assert_eq!(fs::read(dir.join(store::EVENTS_FILE)).unwrap(), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extending_a_held_stored_competition_shows_it_without_its_winner_at_once() {
        let fake = fake();
        let root = temp_root();
        let dir = finished_with(&root, 1, None, true);
        let page = store::viewer_path(&dir);
        let had = fs::read_to_string(&page).unwrap();
        assert!(had.contains(r#""winner":"claude""#));
        let (_runner, _actions, _updates) = open_wired(
            &fake,
            Start::Extend {
                dir: dir.clone(),
                by: 1,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Held: no round starts, so nothing else would rewrite the viewer.
        assert_eq!(store::read_session(&dir).unwrap().rounds.len(), 1);
        let now = fs::read_to_string(&page).unwrap();
        assert!(
            now.contains(r#""winner":null"#),
            "the old winner is still shown"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_refused_extension_writes_nothing_into_the_workdir() {
        let fake = fake();
        let root = temp_root();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        // Closed early by the judge (limit 3): not extendable.
        let dir = finished_with(&root, 3, Some(work.clone()), false);
        fs::remove_dir_all(work.join(".noa")).unwrap();
        let err = open_wired(&fake, Start::Extend { dir, by: 1 })
            .err()
            .unwrap();
        assert!(err.contains("ラウンドを追加できません"), "{err}");
        assert!(!work.join(".noa").exists(), "the workdir was written to");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_winner_of_before_an_extension_is_not_adopted() {
        let fake = fake();
        let root = temp_root();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        let start = Start::New {
            request: "x".into(),
            workdir: Some(work.clone()),
            max_rounds: Some(1),
            reveal_interval: 5,
            judges: vec![AgentKind::Claude.into()],
            panel: vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            root: root.clone(),
        };
        let (mut runner, actions, updates) = rig_on(&fake, start);
        let mut conn = fake.clone();
        for part in ["claude", "codex"] {
            assert!(
                runner
                    .feed(Input::ReplyFileWritten {
                        round: 1,
                        part: PartId::new(part),
                        content: "# Summary: done".into(),
                        artifacts: vec![],
                        at: 2_000,
                    })
                    .is_ok()
            );
        }
        let json = r#"{"overview":"ok","evaluations":{"claude":{"score":80,"feedback":"a"},"codex":{"score":70,"feedback":"b"}},"ranking":["claude","codex"],"winner":"claude","final_verdict":"claude wins","close":true}"#;
        assert!(
            runner
                .feed(Input::ReplyFileWritten {
                    round: 1,
                    part: PartId::new(competition::model::types::JUDGE_ID),
                    content: json.into(),
                    artifacts: vec![],
                    at: 3_000,
                })
                .is_ok()
        );
        assert!(runner.engine.session().closed);
        let spawned = fake.0.borrow().spawned;
        // "+1", then the adopt button still showing claude, before the
        // screen catches up.
        actions.send(Action::Extend { by: 1 }).unwrap();
        actions
            .send(Action::Adopt {
                winner: PartId::new("claude"),
                agent: AgentKind::Codex,
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(runner.engine.session().winner, None);
        assert_eq!(fake.0.borrow().spawned, spawned, "an adopt pane started");
        let task = runner.engine.reply_dir().join("adopt-claude.md");
        assert!(!task.exists(), "the adopt task was written");
        assert!(
            notices(&updates)
                .iter()
                .any(|n| n.contains("現在の優勝者ではありません"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_competition_the_judge_closed_early_is_not_extended() {
        let fake = fake();
        let root = temp_root();
        let dir = finished(&root, 3);
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
        assert_eq!(fake.0.borrow().spawned, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn round_control_reaches_the_engine_from_the_screen() {
        let fake = fake();
        let root = temp_root();
        let panel = vec![AgentKind::Claude.into(), AgentKind::Codex.into()];
        let (mut runner, actions, updates) = rig_on(&fake, new_start(&root, panel));
        let mut conn = fake.clone();
        actions.send(Action::SetHold { hold: true }).unwrap();
        actions.send(Action::Extend { by: 3 }).unwrap();
        actions.send(Action::Continue).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        let session = runner.engine.session();
        assert!(session.hold);
        assert_eq!(session.max_rounds, Some(5));
        // Round 1 still runs: there is nothing to continue yet.
        assert!(notices(&updates)[0].contains("進行中"));
        let stored = store::read_session(runner.engine.dir()).unwrap();
        assert!(stored.hold);
        assert_eq!(stored.max_rounds, Some(5));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "perf probe: counts IPC calls per tick"]
    fn probe_ipc_counts_per_tick() {
        let fake = fake();
        let root = temp_root();
        let mut runner = rig(&fake, &root);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        let codex = fake.pane_of(":codex");
        fake.report(claude, "input", None);
        fake.report(codex, "input", None);
        for _ in 0..5 {
            step(&mut runner, &mut conn);
        }
        assert_eq!(fake.pasted(claude), 1);
        assert_eq!(fake.pasted(codex), 1);
        let (startup_panes, startup_paste, startup_screen, startup_changes) = {
            let w = fake.0.borrow();
            (w.rpc_panes, w.rpc_paste_ready, w.rpc_screen, w.rpc_changes)
        };
        for _ in 0..100 {
            step(&mut runner, &mut conn);
        }
        let (steady_panes, steady_paste, steady_screen, steady_changes) = {
            let w = fake.0.borrow();
            (
                w.rpc_panes - startup_panes,
                w.rpc_paste_ready - startup_paste,
                w.rpc_screen - startup_screen,
                w.rpc_changes - startup_changes,
            )
        };
        println!(
            "competition runner IPC counts:\n  \
             startup (2 idle contestants): panes={startup_panes} paste_ready={startup_paste} screen={startup_screen} changes={startup_changes}\n  \
             steady-state (100 ticks):     panes={steady_panes} paste_ready={steady_paste} screen={steady_screen} changes={steady_changes}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
