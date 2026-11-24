//! Runs one verification against a live Noa, on a thread of its own.
//!
//! Captures the target into the judge's reply dir, opens one Noa tab for
//! the verification, spawns the judge (in its reply dir) and every verifier
//! (in its worktree, or the target's checkout in 共有 mode) as interactive
//! agents in splits, and feeds deliveries, agent states, reply files and
//! user actions to the [`Engine`]. It carries out every [`Effect`] the
//! engine asks for: the worktrees, evidence ingest, the re-run scripts and
//! their watch, and `report.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use verify::engine::{AgentState, Config, Effect, Engine, EngineError, Input, Output, Setup};
use verify::evidence::{self, IngestItem};
use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, BriefKind, CRITERIA_ROUND, CriteriaEdit, EvidenceBody,
    EvidenceKey, Head, Isolation, JUDGE_TAIL_LINES, MAX_LOG_BYTES, Millis, PartId, Phase,
    RERUN_TIMEOUT, Rerun, RerunRequest, RoundNo, Session, SessionId, Target, VerifierChoice,
    WorktreeState, short_id,
};
use verify::model::validate;
use verify::store::{self, Store};
use verify::{media, rerun, worktree};

use super::ipc::{Changes, Connect, IpcConnect, Noa, NoaError, PaneSpec, PaneView, Placement};
use crate::live::Attachment;

const POLL: Duration = Duration::from_millis(200);
const TICK_EVERY: Millis = 1_000;
const BACKOFF_MIN: Millis = 500;
const BACKOFF_MAX: Millis = 8_000;
const SUBMIT_DELAY: Duration = Duration::from_millis(150);
const QUIET_FOR: Millis = 1_500;
const READY_LIMIT: Millis = 30_000;
const REPLY_STABLE_FOR: Millis = 600;
const MAX_SPAWNS: u8 = 2;
/// The largest brief pasted as it is, measured JSON-encoded as it goes in
/// a `noa.sendText` frame: Noa's control server takes frames of at most
/// 256 KiB (`crates/noa-ipc/src/server.rs`, `MAX_WS_FRAME_SIZE`), so a
/// longer brief is written to a file beside the agent's reply and the
/// agent is sent where to read it.
const BRIEF_PASTE_MAX: usize = 128 * 1024;
/// Noa's most panes in one row or column of a tab (`MAX_PANES_PER_AXIS`,
/// crates/noa-app/src/split_tree/tree/types.rs): a split along a full one
/// is refused. A tab of three columns of three holds the judge and the
/// largest panel.
const PANES_PER_AXIS: usize = 3;
/// What every closing of panes is told with: `n` closed, and those whose
/// closing Noa could not confirm.
fn closed_notice(closed: usize, failed: &[String]) -> String {
    if failed.is_empty() {
        format!("{closed} 個のペインを閉じました")
    } else {
        format!(
            "{closed} 個のペインを閉じました。閉じたか確かめられなかったもの: {}",
            failed.join(", ")
        )
    }
}
const NO_PANES: &str = "閉じるペインはありません";
const PANES_AFTER_END: &str = "ペインは検証が終わってから閉じられます";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    New(Box<NewVerification>),
    /// Continues the verification logged in `dir` (`Store::open` +
    /// `Engine::resume`): agents still in their panes are taken up again,
    /// the others are started anew, and a recorded worktree that is gone
    /// is rebuilt from the recipe (L1-10).
    Resume {
        dir: PathBuf,
    },
    /// Gives the verification logged in `dir` `by` more rounds, then goes
    /// on as [`Start::Resume`]: a finished one that ran out of rounds
    /// continues. The extension is stored before any pane is started, and
    /// a refused one starts none.
    Extend {
        dir: PathBuf,
        by: RoundNo,
    },
}

/// What a new verification starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVerification {
    pub request: String,
    pub target: Target,
    pub isolation: Isolation,
    pub user_criteria: Vec<String>,
    pub confirm: bool,
    pub max_rounds: RoundNo,
    pub round_cap: RoundNo,
    pub judge: AgentChoice,
    pub panel: Vec<VerifierChoice>,
    /// The store root (`<data dir>/noa/verifications`).
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    EditCriteria {
        edit: CriteriaEdit,
    },
    StartVerify,
    Attach(Box<Attachment>),
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    Retry,
    Close,
    Focus {
        part: PartId,
    },
    SetHold {
        hold: bool,
    },
    Continue,
    Extend {
        by: RoundNo,
    },
    /// ペインをすべて閉じる: refused until the verification has ended.
    ClosePanes,
    /// ワークツリーを削除 on the ended verification this runner holds.
    RemoveWorktrees,
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
        /// The judge's reply dir.
        reply_root: PathBuf,
        session_dir: PathBuf,
    },
    Link(Link),
    Notice(String),
    /// The version of the Noa this runner just connected to (each
    /// connection, so a reconnect to another Noa shows), with the number
    /// of that connection's hello.
    Version(u64, String),
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
        .name("verify-live".into())
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
        .expect("spawn verify-live thread");
    SessionHandle { updates, actions }
}

fn run_session<C: Connect, K: Clock>(
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

trait Clock {
    fn now(&self) -> Millis;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

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

/// The login shell agents and the fix tab start in.
pub fn login_shell() -> String {
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

/// The label of `part`'s pane in verification `id`: how a resume and the
/// stored verification's ペインをすべて閉じる find it again.
fn pane_label(id: &SessionId, part: &PartId) -> String {
    format!("ver:{id}:{part}")
}

/// Whether `part`'s brief of `kind` in `round` still waits to be taken
/// up, as the engine sees it: the judge's criteria brief before the
/// criteria are accepted, a verifier's work brief while the round is
/// briefed or verified, its re-run brief while the round ingests (and
/// only until the script has started: it is never sent again then, L1-5
/// step 4), the judge's ruling brief while the round is ruled.
fn in_play(session: &Session, round: RoundNo, part: &PartId, kind: BriefKind) -> bool {
    if session.closed {
        return false;
    }
    let open = |b: &verify::model::types::Brief| {
        b.receipt.replied.is_none()
            && b.receipt.failed.is_none()
            && !(b.kind == BriefKind::Rerun && b.receipt.started.is_some())
    };
    if round == CRITERIA_ROUND {
        return kind == BriefKind::Work
            && !session.criteria_accepted
            && session
                .criteria_brief
                .as_ref()
                .is_some_and(|b| &b.to == part && open(b));
    }
    let Some(current) = session.current_round() else {
        return false;
    };
    if current.no != round {
        return false;
    }
    let phase_ok = match (*part == session.judge.id, kind) {
        (true, BriefKind::Work) => current.phase == Phase::Ruling,
        (false, BriefKind::Work) => matches!(current.phase, Phase::Briefing | Phase::Verifying),
        (false, BriefKind::Rerun) => current.phase == Phase::Ingesting,
        (true, BriefKind::Rerun) => false,
    };
    phase_ok && current.brief(part, kind).is_some_and(open)
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
    quiet: Option<(String, Millis)>,
    told_held: bool,
    briefed: bool,
}

impl Pane {
    fn new(id: u64) -> Self {
        Self {
            id,
            ready: false,
            seq: 0,
            state: None,
            blocked: None,
            quiet: None,
            told_held: false,
            briefed: false,
        }
    }

    fn from_view(view: &PaneView) -> Self {
        let mut pane = Self::new(view.pane_id);
        pane.seq = view.agent_seq;
        pane.state.clone_from(&view.agent_state);
        pane.blocked.clone_from(&view.agent_blocked);
        pane.ready = pane.reports_ready();
        pane
    }

    fn reports_ready(&self) -> bool {
        matches!(
            (self.state.as_deref(), self.blocked.as_deref()),
            (Some("input"), None) | (Some("finished"), _)
        )
    }

    /// Whether the agent sits on a prompt only the person can answer.
    /// Claude Code's folder trust prompt is one: every pane of a
    /// verification starts in the person's repository or a copy of it, so
    /// the runner never answers it (review answers it only in its own
    /// data folder).
    fn held(&self) -> bool {
        matches!(self.state.as_deref(), Some("permission"))
            || self.blocked.is_some()
            || (!self.briefed && self.state.as_deref() == Some("error"))
    }

    /// Whether the agent's last report says it is working on a turn.
    fn busy(&self) -> bool {
        self.state.as_deref() == Some("running")
    }
}

#[derive(Debug, Clone)]
struct PendingSend {
    round: RoundNo,
    part: PartId,
    kind: BriefKind,
    body: String,
    queued_at: Millis,
    spawns: u8,
}

#[derive(Debug, Clone)]
struct WatchedReply {
    round: RoundNo,
    part: PartId,
    path: PathBuf,
    /// What the path is checked against: the target's checkout for the
    /// judge, the verifier's root (its worktree) for a verifier.
    base: PathBuf,
    /// What the file held when last read (its bytes, or why it could not
    /// be read) and since when.
    seen: Option<(Result<Vec<u8>, String>, Millis)>,
}

/// One re-run script watched to its items' outcomes (L1-5 step 4).
#[derive(Debug, Clone)]
struct RerunWatch {
    round: RoundNo,
    part: PartId,
    /// `rerun/` in the verifier's reply dir.
    dir: PathBuf,
    /// The verifier's root, which `dir` is read below.
    base: PathBuf,
    request: RerunRequest,
    watch: rerun::Watch,
    /// Whether `RerunStarted` was given (its first `.pid` seen).
    started: bool,
    /// Whether a failed look at `dir` was told.
    told: bool,
}

struct Runner<C: Connect, K: Clock> {
    engine: Engine,
    store: Store,
    connector: C,
    clock: K,
    config: Config,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: Rc<dyn Fn()>,
    panes: BTreeMap<PartId, Pane>,
    /// Panes asked to close whose outcome Noa could not tell (a timeout,
    /// a lost link): when one closes after all, it goes without a notice.
    closing: BTreeSet<u64>,
    /// The verification tab's panes, column by column (each top to
    /// bottom), as this runner placed them.
    layout: Vec<Vec<u64>>,
    sends: Vec<PendingSend>,
    watches: Vec<WatchedReply>,
    reruns: Vec<RerunWatch>,
    last_tick: Millis,
    stop: bool,
    started: bool,
    /// Opened from a log: agents whose panes are gone are briefed again
    /// once the first connection shows which are.
    resumed: bool,
    link_state: Link,
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
            Start::New(new) => new_verification(*new, config, now)?,
            Start::Resume { dir } => open_log(dir, None, config, now)?,
            Start::Extend { dir, by } => open_log(dir, Some(by), config, now)?,
        };
        let runner = Self {
            engine,
            store,
            connector,
            clock,
            config,
            actions,
            updates,
            wake,
            panes: BTreeMap::new(),
            closing: BTreeSet::new(),
            layout: Vec::new(),
            sends: Vec::new(),
            watches: Vec::new(),
            reruns: Vec::new(),
            last_tick: now,
            stop: false,
            started: false,
            resumed,
            link_state: Link::Connecting,
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
            reply_root: self.engine.reply_dir(),
            session_dir: self.engine.dir().to_owned(),
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
            self.poll_reruns()?;
            let now = self.clock.now();
            if now.saturating_sub(self.last_tick) >= TICK_EVERY {
                self.last_tick = now;
                self.feed(Input::Tick { now })?;
            }
            if let Some(changes) = conn.changes(POLL).map_err(link)? {
                self.apply_changes(conn, changes)?;
            }
        }
        Ok(())
    }

    /// On each connection: takes up the panes; on the first, starts (or
    /// resumes) the verification.
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
        Ok(())
    }

    fn sync_panes(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let views = conn.panes().map_err(link)?;
        let by_id: BTreeMap<u64, &PaneView> = views.iter().map(|v| (v.pane_id, v)).collect();
        let mut alive = BTreeMap::new();
        // States the engine has not seen: a full sync follows lost or
        // dropped notifications, and its timers must hear what they said.
        let mut states = Vec::new();
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
                if pane.reports_ready() {
                    pane.ready = true;
                }
                alive.insert(part, pane);
            }
        }
        self.panes = alive;
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
            let action = match self.actions.try_recv() {
                Ok(action) => action,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.stop = true;
                    return Ok(());
                }
            };
            let at = self.clock.now();
            match action {
                Action::Stop => {
                    self.stop = true;
                    return Ok(());
                }
                Action::EditCriteria { edit } => {
                    self.feed_user(Input::CriteriaEdit { edit, at })?;
                }
                Action::StartVerify => self.feed_user(Input::UserStartVerify { at })?,
                Action::Attach(attachment) => self.attach(*attachment, at)?,
                Action::Intervene { to, body } => {
                    self.feed_user(Input::UserIntervention { to, body, at })?;
                }
                Action::Retry => self.feed_user(Input::UserRetry)?,
                Action::Close => self.feed_user(Input::UserClose)?,
                Action::Focus { part } => self.focus_pane(conn.as_deref_mut(), &part),
                Action::ClosePanes => self.close_panes(conn.as_deref_mut()),
                Action::RemoveWorktrees => self.remove_worktrees_now()?,
                Action::SetHold { hold } => self.feed_user(Input::UserSetHold { hold, at })?,
                Action::Continue => self.feed_user(Input::UserContinue { at })?,
                Action::Extend { by } => self.feed_user(Input::UserExtend { by, at })?,
            }
        }
    }

    /// 証跡を追加: copies the person's file into the judge's reply dir
    /// (`evidence::attach`), then has the engine log it. A copy the engine
    /// turns down is taken away again, so nothing unlogged stays.
    fn attach(&mut self, attachment: Attachment, at: Millis) -> Result<(), String> {
        let session = self.engine.session();
        if session.closed {
            self.notice("検証は終了しています");
            return Ok(());
        }
        if session.current_round().is_none() {
            self.notice("証跡は検証の開始後に追加できます");
            return Ok(());
        }
        let reply_dir = self.engine.reply_dir();
        let attached =
            match evidence::attach(&self.engine.reply_base(), &reply_dir, &attachment.source) {
                Ok(attached) => attached,
                Err(why) => {
                    self.notice(format!("証跡を追加できません: {why}"));
                    return Ok(());
                }
            };
        let name = attachment
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let input = Input::AttachEvidence {
            key: attached.key,
            body: EvidenceBody::File {
                path: name,
                caption: Some(attachment.caption),
                media: Some(attached.media),
                marks: attachment.marks,
                shows: attachment.shows.clone(),
            },
            stored: attached.stored.clone(),
            shows: attachment.shows,
            at,
        };
        match self.engine.handle(input) {
            Ok(out) => self.apply_output(out),
            Err(EngineError::Rejected(why)) => {
                // Made by `attach` just now, exclusively.
                let _ = fs::remove_file(reply_dir.join(&attached.stored.path));
                self.notice(why);
                Ok(())
            }
            Err(EngineError::Reduce(e)) => Err(e.to_string()),
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

    /// Closes the panes this runner started for the judge and the
    /// verifiers once the verification has ended (a verdict, or 終了). A
    /// pane is forgotten before Noa reports it gone, so its closing is not
    /// told as a pane the user closed. A 修正を依頼 tab is not this
    /// runner's and stays; the worktrees are not touched. Should the
    /// verification go on (+N), its agents start again in a new tab
    /// ([`Self::placements`]).
    fn close_panes(&mut self, conn: Option<&mut C::Conn>) {
        if !self.engine.session().closed {
            self.notice(PANES_AFTER_END);
            return;
        }
        if self.panes.is_empty() {
            self.notice(NO_PANES);
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
        self.notice(closed_notice(closed, &failed));
    }

    /// ワークツリーを削除 on the verification this runner holds: only once
    /// it has ended (a running one still needs them).
    fn remove_worktrees_now(&mut self) -> Result<(), String> {
        let session = self.engine.session();
        if !session.closed {
            self.notice("ワークツリーは検証が終わってから削除できます");
            return Ok(());
        }
        if !session
            .worktrees
            .values()
            .any(|t| t.state == WorktreeState::Made)
        {
            self.notice(NO_WORKTREES);
            return Ok(());
        }
        self.apply_output(Output {
            batches: Vec::new(),
            effects: vec![Effect::RemoveWorktrees],
        })
    }

    /// After a resume: what the agents taken up are doing now reaches the
    /// engine, so a turn started while the verification was stopped counts
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
        let session = self.engine.session();
        let briefs = session.criteria_brief.iter().chain(
            session
                .current_round()
                .into_iter()
                .flat_map(|r| r.briefs.iter().chain(&r.judge_brief)),
        );
        for brief in briefs {
            let Some(digest) = brief.receipt.rejected_digest else {
                continue;
            };
            let base = self.base_for(&brief.reply_path);
            if brief.receipt.sent.is_none()
                && let Ok(Some(bytes)) = store::read_reply(&base, &brief.reply_path)
                && verify::capture::digest_bytes(&bytes) == digest
            {
                let _ = store::clear_reply(&base, &brief.reply_path);
            }
        }
    }

    /// After a resume: a brief that went out before the verification
    /// stopped, to an agent whose pane is gone and that left no reply, is
    /// sent again as it was (no retry is counted: the stop lost it, not
    /// the agent), which starts the agent in a new pane. A re-run brief
    /// whose script started is never sent again (the engine sees to it).
    fn redeliver_lost(&mut self) -> Result<(), Fail> {
        let session = self.engine.session();
        let mut lost: Vec<(RoundNo, PartId, BriefKind)> = Vec::new();
        let criteria = session.criteria_brief.iter().map(|b| (CRITERIA_ROUND, b));
        let round = session.current_round().into_iter().flat_map(|r| {
            r.briefs
                .iter()
                .chain(&r.rerun_briefs)
                .chain(&r.judge_brief)
                .map(move |b| (r.no, b))
        });
        for (no, brief) in criteria.chain(round) {
            let unanswered = brief.kind == BriefKind::Rerun || !brief.reply_path.exists();
            if brief.receipt.sent.is_some()
                && unanswered
                && !self.panes.contains_key(&brief.to)
                && in_play(session, no, &brief.to, brief.kind)
            {
                lost.push((no, brief.to.clone(), brief.kind));
            }
        }
        for (round, part, kind) in lost {
            self.feed(Input::Redeliver { round, part, kind })?;
        }
        Ok(())
    }

    fn write_report(&self) {
        let text = store::report_text(self.engine.session());
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
        self.feed_user(input).map_err(Fail::Fatal)
    }

    /// Logs `output`'s batches and carries out its effects. What an effect
    /// gives back (a worktree made, evidence ingested, a re-run script
    /// written, a send that cannot go) is fed to the engine after every
    /// effect of `output` has been carried out, in order.
    fn apply_output(&mut self, output: Output) -> Result<(), String> {
        if !output.batches.is_empty() {
            self.store
                .append_all(&output.batches)
                .map_err(|e| format!("write log: {e}"))?;
            self.emit_session();
        }
        let now = self.clock.now();
        let mut follow: Vec<Input> = Vec::new();
        for effect in output.effects {
            match effect {
                Effect::SendBrief {
                    round,
                    part,
                    kind,
                    body,
                } => {
                    self.sends
                        .retain(|s| !(s.round == round && s.part == part && s.kind == kind));
                    match self.oversized_brief(round, &part, kind, body) {
                        Ok(body) => self.sends.push(PendingSend {
                            round,
                            part,
                            kind,
                            body,
                            queued_at: now,
                            spawns: 0,
                        }),
                        Err(error) => follow.push(Input::SendFailed {
                            round,
                            part,
                            kind,
                            error,
                        }),
                    }
                }
                Effect::WatchReply { round, part, path } => {
                    let base = self.base_for(&path);
                    if let Err(e) = store::prepare_reply(&base, &path) {
                        self.notice(format!("返答フォルダの作成に失敗しました: {e}"));
                    }
                    self.watches
                        .retain(|w| !(w.round == round && w.part == part));
                    self.watches.push(WatchedReply {
                        round,
                        part,
                        path,
                        base,
                        seen: None,
                    });
                }
                Effect::ClearReply { path } => {
                    let _ = store::clear_reply(&self.base_for(&path), &path);
                    self.watches.retain(|w| w.path != path);
                }
                Effect::WriteReport => self.write_report(),
                Effect::MakeWorktrees => follow.extend(self.make_worktrees()),
                Effect::Ingest { round } => follow.extend(self.ingest(round)),
                Effect::Rerun { round, part } => follow.extend(self.write_rerun(round, &part, now)),
                Effect::RemoveWorktrees => follow.extend(self.remove_worktrees()),
            }
        }
        let mut manifests = BTreeSet::new();
        for input in follow {
            if let Input::Ingested { round, .. } | Input::RerunReported { round, .. } = &input {
                manifests.insert(*round);
            }
            match self.engine.handle(input) {
                Ok(out) => self.apply_output(out)?,
                Err(EngineError::Rejected(why)) => self.notice(why),
                Err(EngineError::Reduce(e)) => return Err(e.to_string()),
            }
        }
        for round in manifests {
            self.write_manifest(round);
        }
        self.sync_rerun_watches();
        Ok(())
    }

    /// The folder every path of `path`'s owner is checked against: a
    /// verifier's root (its worktree) for what lies in it, else the
    /// target's checkout (the judge's reply dir, and every verifier's in
    /// 共有 mode).
    fn base_for(&self, path: &Path) -> PathBuf {
        let session = self.engine.session();
        if session.isolation == Isolation::Worktree {
            for part in session.verifier_ids() {
                let root = store::verifier_root(session, part);
                if path.starts_with(&root) {
                    return root;
                }
            }
        }
        store::reply_base(session)
    }

    /// Builds and checks every verifier's worktree not yet made (L1-3),
    /// each with its `.noa/.gitignore`. The first that cannot be made
    /// stops the others: the round is refused until 再試行.
    fn make_worktrees(&self) -> Vec<Input> {
        let session = self.engine.session();
        let target = store::target_dir(&self.engine.reply_dir());
        let mut out = Vec::new();
        for part in session.verifier_ids() {
            let path = worktree::path_for(&session.main_worktree, &session.id, part);
            let made = session
                .worktrees
                .get(part)
                .is_some_and(|t| t.state == WorktreeState::Made);
            if made && path.exists() {
                continue;
            }
            match build_worktree(session, &target, &path) {
                Ok(()) => out.push(Input::WorktreeMade {
                    part: part.clone(),
                    path,
                }),
                Err(error) => {
                    self.notice(format!("{part} のワークツリーを作れません: {error}"));
                    out.push(Input::WorktreeFailed {
                        part: part.clone(),
                        path,
                        error,
                    });
                    break;
                }
            }
        }
        out
    }

    /// Copies and hashes the evidence of every verifier that replied in
    /// `round` and is not ingested yet, with the tails of its failing
    /// commands' logs for the judge (L1-5, L2-3). A file that cannot be
    /// stored is left out (it counts as `missing`) and named in a notice.
    fn ingest(&self, round: RoundNo) -> Vec<Input> {
        let session = self.engine.session();
        let Some(current) = session.round(round) else {
            return Vec::new();
        };
        let reply_base = store::reply_base(session);
        let reply_dir = store::reply_dir(session);
        let mut out = Vec::new();
        for part in current.replies.keys() {
            if current.ingested.contains(part) {
                continue;
            }
            let src_base = store::verifier_root(session, part);
            let src_dir = store::part_dir(&store::verifier_reply_dir(session, part), round, part);
            let items: Vec<IngestItem> = session
                .reported_items(round, part)
                .into_iter()
                .filter_map(|e| {
                    let cap = match &e.body {
                        EvidenceBody::Command { .. } => MAX_LOG_BYTES,
                        EvidenceBody::File { media, .. } => media::cap(*media),
                        EvidenceBody::Observation { .. } => return None,
                    };
                    Some(IngestItem {
                        key: e.key.clone(),
                        path: e.file_path()?.to_owned(),
                        cap,
                    })
                })
                .collect();
            let mut stored = Vec::new();
            let mut tails = BTreeMap::new();
            let mut unstored = Vec::new();
            let copied = evidence::ingest(
                &src_base,
                &src_dir,
                &reply_base,
                &reply_dir,
                round,
                part,
                &items,
            );
            for (key, copy) in copied {
                match copy {
                    Ok(copy) => {
                        let failing = matches!(
                            session.evidence_item(&key).map(|e| &e.body),
                            Some(EvidenceBody::Command { exit, .. }) if *exit != 0
                        );
                        if failing {
                            match evidence::tail(&reply_base, &reply_dir, &copy, JUDGE_TAIL_LINES) {
                                Ok(tail) => {
                                    tails.insert(key.clone(), tail);
                                }
                                Err(e) => unstored.push(format!("{key}: {e}")),
                            }
                        }
                        stored.push((key, copy));
                    }
                    Err(why) => unstored.push(format!("{key}: {why}")),
                }
            }
            if !unstored.is_empty() {
                self.notice(format!(
                    "{part} の証跡のうち保存できなかったもの: {}",
                    unstored.join(", ")
                ));
            }
            out.push(Input::Ingested {
                round,
                part: part.clone(),
                items: stored,
                tails,
            });
        }
        out
    }

    /// Rewrites round `round`'s `evidence/r<N>/manifest.md` from what is
    /// stored now.
    fn write_manifest(&self, round: RoundNo) {
        let session = self.engine.session();
        let items: Vec<&verify::model::types::Evidence> = session
            .evidence
            .iter()
            .filter(|e| e.key.split().is_some_and(|(_, r, _)| r == round))
            .collect();
        if items.is_empty() {
            return;
        }
        if let Err(e) = evidence::write_manifest(
            &self.engine.reply_base(),
            &self.engine.reply_dir(),
            round,
            &items,
        ) {
            self.notice(format!("証跡の一覧を書けません: {e}"));
        }
    }

    /// Writes `part`'s re-run script for `round` (L1-5 step 1). One that
    /// cannot be written leaves every item `not_run` with why, so the
    /// round still reaches the judge.
    fn write_rerun(&self, round: RoundNo, part: &PartId, now: Millis) -> Vec<Input> {
        let session = self.engine.session();
        let root = store::verifier_root(session, part);
        let dir = store::rerun_dir(&store::verifier_reply_dir(session, part), round, part);
        let commands: Vec<rerun::Command> = session
            .rerun_items(round, part)
            .into_iter()
            .filter_map(|e| match &e.body {
                EvidenceBody::Command { cmd, cwd, .. } => Some(rerun::Command {
                    key: e.key.clone(),
                    cmd: cmd.clone(),
                    cwd: cwd.clone(),
                }),
                _ => None,
            })
            .collect();
        match rerun::write_request(&root, &dir, &root, &commands) {
            Ok(request) => vec![Input::RerunWritten {
                round,
                part: part.clone(),
                request,
                script: dir.join(rerun::SCRIPT_FILE),
            }],
            Err(e) => {
                let why = format!("the re-run script could not be written: {e}");
                self.notice(format!("{part} の再実行スクリプトを書けません: {e}"));
                commands
                    .into_iter()
                    .map(|c| Input::RerunReported {
                        round,
                        key: c.key,
                        rerun: Rerun::NotRun {
                            why: why.clone(),
                            log: None,
                        },
                        at: now,
                    })
                    .collect()
            }
        }
    }

    /// Removes every worktree the log records as made (L1-3); one that is
    /// not this verification's detached worktree any more is left and
    /// named.
    fn remove_worktrees(&self) -> Vec<Input> {
        let (removed, left) = remove_recorded(self.engine.session());
        if !removed.is_empty() || !left.is_empty() {
            self.notice(removal_notice(removed.len(), &left));
        }
        removed
            .into_iter()
            .map(|part| Input::WorktreeRemoved { part })
            .collect()
    }

    /// A watch for every re-run request of the round being ingested whose
    /// items still lack an outcome: the one just written, or, after a
    /// resume, every one the log holds (a new [`rerun::Watch`], so a
    /// command still running is timed from now).
    fn sync_rerun_watches(&mut self) {
        let session = self.engine.session();
        let Some(round) = session
            .current_round()
            .filter(|r| r.phase == Phase::Ingesting)
        else {
            self.reruns.clear();
            return;
        };
        for (part, request) in &round.reruns {
            let pending = session
                .rerun_items(round.no, part)
                .iter()
                .any(|e| e.rerun.is_none());
            let watched = self
                .reruns
                .iter()
                .any(|w| w.round == round.no && &w.part == part);
            if !pending || watched {
                continue;
            }
            let started = round
                .brief(part, BriefKind::Rerun)
                .is_some_and(|b| b.receipt.started.is_some());
            self.reruns.push(RerunWatch {
                round: round.no,
                part: part.clone(),
                dir: store::rerun_dir(&store::verifier_reply_dir(session, part), round.no, part),
                base: store::verifier_root(session, part),
                request: request.clone(),
                watch: rerun::Watch::new(rerun::Limits {
                    // Every kind runs `run.sh` detached for now
                    // (`rerun::detached`); a foreground kind would get the
                    // smaller of this and its tool's limit (L1-5 step 3).
                    run: RERUN_TIMEOUT,
                    next_start: self.config.start_timeout,
                }),
                started,
                told: false,
            });
        }
    }

    /// Polls every watched re-run script (`rerun::poll`, which also kills a
    /// command past its limit): its first `.pid` starts the re-run brief,
    /// and once every item's turn is over (or `done` holds another nonce)
    /// the files are checked against their digests, the logs ingested and
    /// every outcome reported (L1-5 steps 4–5).
    fn poll_reruns(&mut self) -> Result<(), Fail> {
        let now = self.clock.now();
        let mut inputs = Vec::new();
        let mut idx = 0;
        while idx < self.reruns.len() {
            let session = self.engine.session();
            let w = &self.reruns[idx];
            let open = session
                .current_round()
                .is_some_and(|r| r.no == w.round && r.phase == Phase::Ingesting);
            let pending = session
                .rerun_items(w.round, &w.part)
                .iter()
                .any(|e| e.rerun.is_none());
            if !open || !pending {
                self.reruns.remove(idx);
                continue;
            }
            let w = &mut self.reruns[idx];
            let status = match rerun::poll(&w.dir, &w.request, &mut w.watch, now) {
                Ok(status) => status,
                Err(e) => {
                    if !w.told {
                        w.told = true;
                        let text = format!("{} の再実行を確かめられません: {e}", w.part);
                        self.notice(text);
                    }
                    idx += 1;
                    continue;
                }
            };
            if !w.started && status.items.iter().any(|i| i.pid.is_some()) {
                w.started = true;
                inputs.push(Input::RerunStarted {
                    round: w.round,
                    part: w.part.clone(),
                    at: now,
                });
            }
            let wrong_nonce = status.done.as_ref().is_some_and(|d| *d != w.request.nonce);
            if status.items.iter().all(|i| i.end.is_some()) || wrong_nonce {
                let w = self.reruns.remove(idx);
                inputs.extend(self.finish_rerun(&w, &status, now));
                continue;
            }
            idx += 1;
        }
        let mut manifests = BTreeSet::new();
        for input in inputs {
            if let Input::RerunReported { round, .. } = &input {
                manifests.insert(*round);
            }
            self.feed(input)?;
        }
        for round in manifests {
            self.write_manifest(round);
        }
        Ok(())
    }

    /// The outcome of every item of a finished script: `not_run` for all
    /// when its files are not the ones written (L1-5 step 5), else each
    /// command's end against the exit status its verifier reported; each
    /// keeps its log, ingested beside the verifier's evidence.
    fn finish_rerun(&self, w: &RerunWatch, status: &rerun::Status, now: Millis) -> Vec<Input> {
        let session = self.engine.session();
        let checked = rerun::check_digests(&w.base, &w.dir, &w.request);
        let mut logs: BTreeMap<EvidenceKey, Option<verify::model::types::Stored>> = BTreeMap::new();
        let mut unstored = Vec::new();
        for (key, copy) in rerun::ingest_logs(
            &w.base,
            &w.dir,
            &store::reply_base(session),
            &store::reply_dir(session),
            w.round,
            &w.part,
            &w.request,
        ) {
            match copy {
                Ok(copy) => {
                    logs.insert(key, Some(copy));
                }
                Err(why) => {
                    unstored.push(format!("{key}: {why}"));
                    logs.insert(key, None);
                }
            }
        }
        if !unstored.is_empty() {
            self.notice(format!(
                "{} の再実行ログのうち保存できなかったもの: {}",
                w.part,
                unstored.join(", ")
            ));
        }
        w.request
            .items
            .iter()
            .map(|item| {
                let log = logs.get(&item.key).cloned().flatten();
                let end = status
                    .items
                    .iter()
                    .find(|i| i.n == item.n)
                    .and_then(|i| i.end.clone());
                let reported = match session.evidence_item(&item.key).map(|e| &e.body) {
                    Some(EvidenceBody::Command { exit, .. }) => Some(*exit),
                    _ => None,
                };
                let rerun = match (&checked, end, reported) {
                    (Err(why), _, _) => Rerun::NotRun {
                        why: why.clone(),
                        log,
                    },
                    (Ok(()), Some(end), Some(reported)) => rerun::outcome(reported, end, log),
                    (Ok(()), _, _) => Rerun::NotRun {
                        why: rerun::DID_NOT_FINISH.to_owned(),
                        log,
                    },
                };
                Input::RerunReported {
                    round: w.round,
                    key: item.key.clone(),
                    rerun,
                    at: now,
                }
            })
            .collect()
    }

    /// `body` as it can be pasted: itself when it fits a control frame,
    /// else a short pointer to a file beside the agent's reply holding it.
    fn oversized_brief(
        &self,
        round: RoundNo,
        part: &PartId,
        kind: BriefKind,
        body: String,
    ) -> Result<String, String> {
        let encoded = serde_json::to_string(&body).map_or(usize::MAX, |s| s.len());
        if encoded <= BRIEF_PASTE_MAX {
            return Ok(body);
        }
        let (base, path) = brief_file(self.engine.session(), round, part, kind);
        if let Some(parent) = path.parent() {
            verify::fsx::create_dirs(&base, parent)
                .map_err(|e| format!("ブリーフを書けません: {e}"))?;
        }
        verify::fsx::write_replace(&base, &path, body.as_bytes())
            .map_err(|e| format!("ブリーフを書けません: {e}"))?;
        Ok(brief_pointer(&path))
    }

    fn deliver_sends(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let mut idx = 0;
        while idx < self.sends.len() {
            let round = self.sends[idx].round;
            let kind = self.sends[idx].kind;
            let spawns = self.sends[idx].spawns;
            let queued_at = self.sends[idx].queued_at;
            if !in_play(self.engine.session(), round, &self.sends[idx].part, kind) {
                self.sends.remove(idx);
                continue;
            }
            let now = self.clock.now();
            // A reply already on disk is taken in (or turned down) once it
            // settles; pasting the brief again would start a new turn.
            if kind == BriefKind::Work
                && self
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
                        kind: removed.kind,
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
                            kind: removed.kind,
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
                        kind: removed.kind,
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
                        kind: removed.kind,
                        error: why,
                    })?;
                }
                Err(other) => return Err(link(other)),
            }
        }
        Ok(())
    }

    fn try_ready(&mut self, conn: &mut C::Conn, part: &PartId, now: Millis) -> Result<bool, Fail> {
        let (pane_id, skip_ready) = {
            let Some(pane) = self.panes.get_mut(part) else {
                return Ok(false);
            };
            if pane.reports_ready() {
                pane.ready = true;
                return Ok(true);
            }
            (pane.id, pane.held() || pane.busy())
        };
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

    /// Starts `part`'s agent: the judge in its reply dir, a verifier in
    /// its worktree (共有: the target's checkout), L1-3.
    fn spawn_pane(&mut self, conn: &mut C::Conn, part: &PartId) -> Result<(), Fail> {
        let session = self.engine.session();
        let Some(participant) = session.participant(part).cloned() else {
            return Ok(());
        };
        let cwd = if *part == session.judge.id {
            self.engine.reply_dir()
        } else {
            store::verifier_root(session, part)
        };
        let spec = PaneSpec {
            argv: agent_argv(participant.kind, &participant.setup),
            cwd,
            label: self.pane_label(part),
            attended: false,
            env: Vec::new(),
        };
        let mut refused = String::new();
        for placement in self.placements() {
            match conn.spawn(placement, &spec) {
                Ok(pane_id) => {
                    self.place(placement, pane_id);
                    self.panes.insert(part.clone(), Pane::new(pane_id));
                    return self.feed(Input::PaneSpawned {
                        part: part.clone(),
                        pane_id,
                    });
                }
                // No room there (or the anchor just closed): the next place.
                Err(NoaError::Refused(why)) => refused = why,
                Err(NoaError::PaneGone) => {}
                Err(other) => return Err(link(other)),
            }
        }
        Err(Fail::Link(format!(
            "{part} のペインを開けません: {refused}"
        )))
    }

    /// Where a new pane may go, best first: a new column while the top row
    /// has room, then under the shortest column, then next to any pane of
    /// the verification (those taken up on a resume are not in the
    /// layout), and last a tab of its own.
    fn placements(&mut self) -> Vec<Placement> {
        let alive: BTreeSet<u64> = self.panes.values().map(|p| p.id).collect();
        for column in &mut self.layout {
            column.retain(|id| alive.contains(id));
        }
        self.layout.retain(|column| !column.is_empty());
        let mut out = Vec::new();
        if alive.is_empty() {
            out.push(Placement::NewTab);
            return out;
        }
        if self.layout.len() < PANES_PER_AXIS
            && let Some(column) = self.layout.last()
        {
            out.push(Placement::Right(column[0]));
        }
        let mut columns: Vec<&Vec<u64>> = self
            .layout
            .iter()
            .filter(|c| c.len() < PANES_PER_AXIS)
            .collect();
        columns.sort_by_key(|c| c.len());
        out.extend(
            columns
                .iter()
                .filter_map(|c| c.last())
                .map(|&id| Placement::Below(id)),
        );
        for &id in &alive {
            for placement in [Placement::Below(id), Placement::Right(id)] {
                if !out.contains(&placement) {
                    out.push(placement);
                }
            }
        }
        out.push(Placement::NewTab);
        out
    }

    /// Records where `pane` went.
    fn place(&mut self, placement: Placement, pane: u64) {
        let column_of = |layout: &mut Vec<Vec<u64>>, anchor: u64| {
            layout
                .iter()
                .position(|c| c.contains(&anchor))
                .unwrap_or_else(|| {
                    layout.push(vec![anchor]);
                    layout.len() - 1
                })
        };
        match placement {
            Placement::NewTab => self.layout.push(vec![pane]),
            Placement::Right(anchor) => {
                let at = column_of(&mut self.layout, anchor);
                self.layout.insert(at + 1, vec![pane]);
            }
            Placement::Below(anchor) => {
                let at = column_of(&mut self.layout, anchor);
                let column = &mut self.layout[at];
                let below = column
                    .iter()
                    .position(|&id| id == anchor)
                    .map_or(column.len(), |i| i + 1);
                column.insert(below, pane);
            }
        }
    }

    fn poll_replies(&mut self) -> Result<(), Fail> {
        let now = self.clock.now();
        let mut ready = Vec::new();
        let mut idx = 0;
        while idx < self.watches.len() {
            let watch = &mut self.watches[idx];
            if !in_play(
                self.engine.session(),
                watch.round,
                &watch.part,
                BriefKind::Work,
            ) {
                self.watches.remove(idx);
                continue;
            }
            // A read error that stays is turned down like a bad reply
            // (it counts against the retries), never waited on for ever.
            let read = match store::read_reply(&watch.base, &watch.path) {
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
                        ready.push((w.round, w.part, w.base, read));
                        continue;
                    }
                }
                _ => watch.seen = Some((read, now)),
            }
            idx += 1;
        }
        for (round, part, base, read) in ready {
            let input = match read.map(String::from_utf8) {
                Ok(Ok(content)) => {
                    let files = self.inspect_cited(round, &part, &base, &content);
                    Input::ReplyFileWritten {
                        round,
                        part,
                        content,
                        files,
                        at: now,
                    }
                }
                Ok(Err(e)) => Input::ReplyUnreadable {
                    round,
                    part,
                    error: "返答が UTF-8 のテキストではありません".to_owned(),
                    digest: verify::capture::digest_bytes(e.as_bytes()),
                    at: now,
                },
                Err(why) => Input::ReplyUnreadable {
                    round,
                    part,
                    error: format!("返答を読めません: {why}"),
                    digest: verify::capture::digest_bytes(why.as_bytes()),
                    at: now,
                },
            };
            self.feed(input)?;
        }
        Ok(())
    }

    /// What is at every path a verifier's reply cites (`logs/…` and
    /// `file` paths, `validate::cited_paths`), relative to its
    /// `r<N>/<verifier>/`, so the reply's validation stays pure (L1-5).
    /// The judge's files cite none.
    fn inspect_cited(
        &self,
        round: RoundNo,
        part: &PartId,
        base: &Path,
        content: &str,
    ) -> Vec<verify::model::types::FileFact> {
        let session = self.engine.session();
        if round == CRITERIA_ROUND || !session.is_verifier(part) {
            return Vec::new();
        }
        let dir = store::part_dir(&store::verifier_reply_dir(session, part), round, part);
        evidence::inspect(base, &dir, &validate::cited_paths(content))
    }

    fn all_parts(&self) -> Vec<PartId> {
        let s = self.engine.session();
        std::iter::once(s.judge.id.clone())
            .chain(s.verifier_ids().cloned())
            .collect()
    }

    fn pane_label(&self, part: &PartId) -> String {
        pane_label(&self.engine.session().id, part)
    }
}

/// Where a brief too long to paste is written, and the folder that path
/// is checked against: beside the agent's reply, where its sandbox can
/// read it (the judge's: `c/` or `r<N>/` of its reply dir; a verifier's:
/// `r<N>/` of its own reply dir).
fn brief_file(
    session: &Session,
    round: RoundNo,
    part: &PartId,
    kind: BriefKind,
) -> (PathBuf, PathBuf) {
    let name = match kind {
        BriefKind::Work => format!("brief-{part}.md"),
        BriefKind::Rerun => format!("brief-{part}-rerun.md"),
    };
    if *part == session.judge.id {
        let reply_dir = store::reply_dir(session);
        let dir = if round == CRITERIA_ROUND {
            reply_dir.join(store::CRITERIA_DIR)
        } else {
            store::round_dir(&reply_dir, round)
        };
        (store::reply_base(session), dir.join(name))
    } else {
        let dir = store::round_dir(&store::verifier_reply_dir(session, part), round);
        (store::verifier_root(session, part), dir.join(name))
    }
}

/// What is pasted in place of a brief written to `path`.
fn brief_pointer(path: &Path) -> String {
    format!(
        "This round's brief is too long to paste, so it is in a file. Read all of {} \
         now and follow it exactly as your brief for this round: it says what to \
         verify and where to write your reply.",
        path.display()
    )
}

/// Makes verifier worktree `path` from the snapshot in `target` and
/// gives it its `.noa/.gitignore`; one made but left without it is
/// removed again, so a retry builds it afresh.
fn build_worktree(session: &Session, target: &Path, path: &Path) -> Result<(), String> {
    worktree::add(
        &session.main_worktree,
        path,
        target,
        &session.manifest,
        &session.rebuild,
    )?;
    if let Err(e) = store::prepare_workdir(path) {
        let _ = worktree::remove(&session.main_worktree, path);
        return Err(format!("{} の .noa を用意できません: {e}", path.display()));
    }
    Ok(())
}

const NO_WORKTREES: &str = "削除するワークツリーはありません";

/// Removes every worktree `session` records as made: the parts removed,
/// and each one left with why.
fn remove_recorded(session: &Session) -> (Vec<PartId>, Vec<String>) {
    let mut removed = Vec::new();
    let mut left = Vec::new();
    for (part, tree) in &session.worktrees {
        if tree.state != WorktreeState::Made {
            continue;
        }
        match worktree::remove(&session.main_worktree, &tree.path) {
            Ok(()) => removed.push(part.clone()),
            Err(why) => left.push(format!("{part}: {why}")),
        }
    }
    (removed, left)
}

fn removal_notice(removed: usize, left: &[String]) -> String {
    if left.is_empty() {
        format!("{removed} 個のワークツリーを削除しました")
    } else {
        format!(
            "{removed} 個のワークツリーを削除しました。削除しなかったもの: {}",
            left.join(", ")
        )
    }
}

/// Opens the verification logged in `dir` to go on with it, after the
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
    verify::engine::check_resumable(&session, &dir)
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
    let workdir = engine.session().workdir.clone();
    store::prepare_workdir(&workdir).map_err(|e| format!("prepare {}: {e}", workdir.display()))?;
    if let Some(out) = extension {
        // Stored before anything starts; the round it opens is briefed by
        // the start that follows the first connection.
        store
            .append_all(&out.batches)
            .map_err(|e| format!("write log: {e}"))?;
    }
    rebuild_worktrees(&mut engine, &mut store)?;
    // The report follows the log: a stop between the two writes (the log
    // took a judge output, the report did not) is repaired here.
    let session = engine.session();
    if store::report_is_stale(&dir, session) {
        let _ = store::write_report(&dir, &store::report_text(session));
    }
    Ok((engine, store))
}

/// On a resume (L1-10, AC-19): a worktree the log records as made whose
/// folder is gone is rebuilt from the recipe, and so is one removed with
/// ワークツリーを削除 while a round is still in progress (the round's
/// briefs go into it). One removed between rounds is made again by the
/// engine before the next round. A rebuild that fails refuses the resume.
fn rebuild_worktrees(engine: &mut Engine, store: &mut Store) -> Result<(), String> {
    let session = engine.session();
    if session.closed || session.isolation != Isolation::Worktree || session.frozen_at.is_none() {
        return Ok(());
    }
    let running = session
        .current_round()
        .is_some_and(|r| r.phase != Phase::Closed);
    let target = store::target_dir(&engine.reply_dir());
    let mut made = Vec::new();
    for part in session.verifier_ids() {
        let path = worktree::path_for(&session.main_worktree, &session.id, part);
        let needed = match session.worktrees.get(part).map(|t| &t.state) {
            Some(WorktreeState::Made) => fs::symlink_metadata(&path).is_err(),
            Some(WorktreeState::Removed) => running,
            Some(WorktreeState::Failed { .. }) | None => false,
        };
        if needed {
            build_worktree(session, &target, &path)
                .map_err(|e| format!("{part} のワークツリーを作り直せません: {e}"))?;
            made.push((part.clone(), path));
        }
    }
    for (part, path) in made {
        let out = engine
            .handle(Input::WorktreeMade { part, path })
            .map_err(|e| e.to_string())?;
        store
            .append_all(&out.batches)
            .map_err(|e| format!("write log: {e}"))?;
    }
    Ok(())
}

/// How many ids of one millisecond a start tries before it gives up.
const ID_TRIES: u32 = 1000;

/// A new verification's id with its directories reserved: the store
/// directory and the judge's reply dir in the target's checkout, each
/// created exclusively. An id taken in either place (another window,
/// another data dir) moves on to the next suffix, so what a refused start
/// removes is only its own. Returns the id, the store directory and the
/// reply dir.
fn reserve(
    root: &Path,
    workdir: &Path,
    now: Millis,
) -> Result<(SessionId, PathBuf, PathBuf), String> {
    fs::create_dir_all(root).map_err(|e| format!("{} を作れません: {e}", root.display()))?;
    store::prepare_workdir(workdir).map_err(|e| format!("prepare {}: {e}", workdir.display()))?;
    for suffix in 0..ID_TRIES {
        let id = match suffix {
            0 => SessionId::at(now),
            n => SessionId::new(format!("ver-{now}-{n}")),
        };
        let dir = store::session_dir(root, &id);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{} を作れません: {e}", dir.display())),
        }
        // `.noa/verify` was checked free of links by `prepare_workdir`.
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
    Err("新しい検証の ID を確保できません".to_owned())
}

/// Why 共有（読み取りのみ） cannot verify a ref (L1-3).
pub const SHARED_REF_HEAD: &str = "共有（読み取りのみ）で ref を検証するには、作業ツリーの HEAD がそのコミットである必要があります";

/// Git run in the person's checkout `workdir` as the library runs it
/// (`capture::git`, crate-private): no `GIT_*` variable of the widget's
/// environment, and no file-system monitor (a hook command, or a daemon
/// git may start).
fn checkout_git(workdir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Process::new("git");
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
    let out = cmd
        .arg("-C")
        .arg(workdir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} が失敗しました: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// In 共有（読み取りのみ）, every verifier runs in the target's checkout,
/// which nothing makes the target: a ref head is verified there only when
/// the checkout's HEAD is that ref's commit (`head_sha`, as captured) and
/// its work tree is clean (L1-3).
fn check_shared_ref(
    target: &Target,
    isolation: Isolation,
    workdir: &Path,
    head_sha: &str,
) -> Result<(), String> {
    if isolation != Isolation::Shared || !matches!(target.head, Head::Ref(_)) {
        return Ok(());
    }
    let head = checkout_git(workdir, &["rev-parse", "--verify", "HEAD"])?;
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    if head != head_sha {
        return Err(format!(
            "{SHARED_REF_HEAD}（作業ツリーの HEAD: {}、検証する ref: {}）",
            short_id(&head, 10),
            short_id(head_sha, 10)
        ));
    }
    let status = checkout_git(
        workdir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )?;
    if !status.is_empty() {
        return Err(format!(
            "{SHARED_REF_HEAD}。作業ツリーに未コミットの変更があります"
        ));
    }
    Ok(())
}

/// Captures the target and creates the verification's log: nothing is
/// left behind when the capture or the setup is refused.
fn new_verification(
    new: NewVerification,
    config: Config,
    now: Millis,
) -> Result<(Engine, Store), String> {
    let NewVerification {
        request,
        target,
        isolation,
        user_criteria,
        confirm,
        max_rounds,
        round_cap,
        judge,
        panel,
        root,
    } = new;
    verify::engine::check_panel(&judge, &panel)?;
    let workdir = verify::capture::repo_root(&target.repo)?;
    let main_worktree = worktree::main_worktree(&workdir)?;
    let (id, dir, reply_dir) = reserve(&root, &workdir, now)?;
    // This call made both directories (`reserve`): a refusal removes them
    // whole, and nothing else.
    let undo = || {
        let _ = fs::remove_dir_all(&reply_dir);
        let _ = fs::remove_dir_all(&dir);
    };
    let captured = match verify::capture::capture(&target, &store::target_dir(&reply_dir), now) {
        Ok(captured) => captured,
        Err(why) => {
            undo();
            return Err(why);
        }
    };
    if let Err(why) = check_shared_ref(&target, isolation, &workdir, &captured.rebuild.head_sha) {
        undo();
        return Err(why);
    }
    let setup = Setup {
        request: &request,
        target,
        manifest: captured.manifest,
        rebuild: captured.rebuild,
        spec: captured.spec,
        isolation,
        user_criteria,
        confirm,
        workdir,
        main_worktree,
        max_rounds,
        round_cap,
        judge,
        panel: &panel,
    };
    let (engine, output) = match Engine::create_in(id, setup, &dir, config, now) {
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

/// The stored verification in `dir` and the labels of its judge's and
/// verifiers' panes, for ペインをすべて閉じる without a runner: refused
/// while it has not ended (an interrupted one needs its panes to resume)
/// and for a log that is not its folder's.
fn stored_labels(dir: &Path) -> Result<BTreeSet<String>, String> {
    let session =
        store::read_session(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
    if dir.file_name() != Some(OsStr::new(session.id.as_str())) {
        return Err(format!(
            "ログの検証 ID {} がフォルダ名と一致しません",
            session.id
        ));
    }
    if !session.closed {
        return Err(PANES_AFTER_END.to_owned());
    }
    Ok(std::iter::once(&session.judge.id)
        .chain(session.verifier_ids())
        .map(|part| pane_label(&session.id, part))
        .collect())
}

/// The open panes of `views` that carry one of `labels`.
fn labelled(views: Vec<PaneView>, labels: &BTreeSet<String>) -> Vec<PaneView> {
    views
        .into_iter()
        .filter(|v| v.label.as_ref().is_some_and(|l| labels.contains(l)))
        .collect()
}

/// How many of the stored verification's (in `dir`) judge and verifier
/// panes are open in Noa, found by their labels.
pub fn count_stored_panes<C: Connect>(connector: &mut C, dir: &Path) -> Result<usize, String> {
    let labels = stored_labels(dir)?;
    let mut conn = connector.connect()?;
    let views = conn.panes().map_err(|e| e.to_string())?;
    Ok(labelled(views, &labels).len())
}

/// ペインをすべて閉じる for a finished stored verification: closes its
/// judge's and verifiers' panes found by label; the text is the notice.
/// No other pane is touched: a 修正を依頼 tab carries a `fix · …` label.
pub fn close_stored_panes<C: Connect>(connector: &mut C, dir: &Path) -> Result<String, String> {
    let labels = stored_labels(dir)?;
    let mut conn = connector
        .connect()
        .map_err(|why| format!("Noa に接続できません: {why}"))?;
    let views = labelled(conn.panes().map_err(|e| e.to_string())?, &labels);
    if views.is_empty() {
        return Ok(NO_PANES.to_owned());
    }
    let mut closed = 0;
    let mut failed = Vec::new();
    for view in views {
        match conn.close_pane(view.pane_id) {
            Ok(()) | Err(NoaError::PaneGone) => closed += 1,
            Err(err) => failed.push(format!(
                "{}: {err}",
                view.label.as_deref().unwrap_or_default()
            )),
        }
    }
    Ok(closed_notice(closed, &failed))
}

/// ワークツリーを削除 for a stored verification no runner holds (ended, or
/// interrupted: a resume then rebuilds them): each worktree its log
/// records as made is removed and the removal logged, under the log's
/// lock.
pub fn remove_stored_worktrees(dir: &Path) -> Result<String, String> {
    let (mut store, session) =
        Store::open(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
    verify::engine::check_store_paths(&session, dir)
        .map_err(|why| format!("ワークツリーを削除できません: {why}"))?;
    if !session
        .worktrees
        .values()
        .any(|t| t.state == WorktreeState::Made)
    {
        return Ok(NO_WORKTREES.to_owned());
    }
    let (removed, left) = remove_recorded(&session);
    let mut engine = Engine::resume(session, dir, Config::default());
    for part in &removed {
        let out = engine
            .handle(Input::WorktreeRemoved { part: part.clone() })
            .map_err(|e| e.to_string())?;
        store
            .append_all(&out.batches)
            .map_err(|e| format!("write log: {e}"))?;
    }
    Ok(removal_notice(removed.len(), &left))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use verify::model::types::{BuiltinStrategy, MAX_MAX_ROUNDS, StrategyChoice, TargetKind};

    struct FakePane {
        label: Option<String>,
        state: Option<String>,
        blocked: Option<String>,
        seq: u64,
        cwd: PathBuf,
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
        /// Each tab's panes, column by column, split as Noa splits them
        /// (at most [`PANES_PER_AXIS`] along a row or a column).
        tabs: Vec<Vec<Vec<u64>>>,
        /// Panes that are gone by the time they are asked for a paste.
        gone_on_paste: bool,
        /// Panes back at their shell: Noa refuses every write
        /// (`refuseShell`) and writes nothing.
        shell: BTreeSet<u64>,
        /// The agent exits right after the next paste into this pane.
        exits_after_paste: Option<u64>,
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
                .unwrap_or_else(|| panic!("no pane labelled …{label_end}"))
                .0
        }

        /// A pane someone else opened (the person, a fix request).
        fn add_pane(&self, label: &str) -> u64 {
            let mut w = self.0.borrow_mut();
            let id = w.next_pane;
            w.next_pane += 1;
            w.panes.insert(
                id,
                FakePane {
                    label: Some(label.to_owned()),
                    state: None,
                    blocked: None,
                    seq: 0,
                    cwd: PathBuf::new(),
                },
            );
            id
        }

        /// The texts pasted into `pane` (the Enter that submits each is left out).
        fn pasted(&self, pane: u64) -> usize {
            self.pastes(pane).len()
        }

        fn pastes(&self, pane: u64) -> Vec<String> {
            let w = self.0.borrow();
            w.typed
                .iter()
                .filter(|(p, t)| *p == pane && t != "\r")
                .map(|(_, t)| t.clone())
                .collect()
        }

        fn cwd(&self, pane: u64) -> PathBuf {
            self.0.borrow().panes[&pane].cwd.clone()
        }

        fn advance(&self, ms: Millis) {
            self.0.borrow_mut().now += ms;
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
            let w = self.0.borrow();
            Ok(w.panes.iter().map(|(id, p)| Fake::view(*id, p)).collect())
        }

        fn changes(&mut self, _wait: Duration) -> Result<Option<Changes>, NoaError> {
            Ok(self.0.borrow_mut().events.pop_front())
        }

        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            let mut w = self.0.borrow_mut();
            let id = w.next_pane;
            let World { tabs, panes, .. } = &mut *w;
            for tab in tabs.iter_mut() {
                for column in tab.iter_mut() {
                    column.retain(|p| panes.contains_key(p));
                }
                tab.retain(|column| !column.is_empty());
            }
            let find = |tabs: &Vec<Vec<Vec<u64>>>, anchor: u64| {
                tabs.iter().enumerate().find_map(|(t, tab)| {
                    tab.iter().position(|c| c.contains(&anchor)).map(|c| (t, c))
                })
            };
            let full = || NoaError::Refused("split did not create a pane".into());
            match placement {
                Placement::NewTab => tabs.push(vec![vec![id]]),
                Placement::Right(anchor) => {
                    let (t, c) = find(tabs, anchor).ok_or(NoaError::PaneGone)?;
                    if tabs[t].len() >= PANES_PER_AXIS {
                        return Err(full());
                    }
                    tabs[t].insert(c + 1, vec![id]);
                }
                Placement::Below(anchor) => {
                    let (t, c) = find(tabs, anchor).ok_or(NoaError::PaneGone)?;
                    let column = &mut tabs[t][c];
                    if column.len() >= PANES_PER_AXIS {
                        return Err(full());
                    }
                    let at = column.iter().position(|&p| p == anchor).unwrap() + 1;
                    column.insert(at, id);
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
                    cwd: spec.cwd.clone(),
                },
            );
            Ok(id)
        }

        fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError> {
            let mut w = self.0.borrow_mut();
            if w.gone_on_paste {
                w.panes.remove(&pane);
                return Err(NoaError::PaneGone);
            }
            Ok(true)
        }

        fn screen(&mut self, _pane: u64) -> Result<String, NoaError> {
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
            "0.9.13".into()
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
            if w.panes.remove(&pane).is_none() {
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

    /// A fresh folder for one test, canonical (git reports canonical paths).
    pub(crate) fn temp_root() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-verify-live-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    /// Runs git in `dir` for a test's own setup, away from the person's
    /// settings.
    fn git(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Process::new("git");
        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"GIT_") {
                cmd.env_remove(name);
            }
        }
        let out = cmd
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A repository under `root` with one commit on `main`.
    fn repo(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        repo
    }

    fn fake() -> Fake {
        Fake(Rc::new(RefCell::new(World {
            now: 1_000,
            next_pane: 100,
            ..World::default()
        })))
    }

    /// Runs the runner's loop body once: sends, replies, re-runs, the
    /// tick, the changes.
    fn step(runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        assert!(runner.deliver_sends(conn).is_ok());
        assert!(runner.poll_replies().is_ok());
        assert!(runner.poll_reruns().is_ok());
        let now = runner.clock.now();
        assert!(runner.feed(Input::Tick { now }).is_ok());
        while let Some(changes) = conn.changes(POLL).unwrap() {
            assert!(runner.apply_changes(conn, changes).is_ok());
        }
    }

    type Wired = (Runner<Fake, Fake>, Sender<Action>, Receiver<Update>);

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

    fn verifier(kind: AgentKind, strategy: BuiltinStrategy) -> VerifierChoice {
        VerifierChoice {
            agent: kind.into(),
            strategy: StrategyChoice::Builtin(strategy),
        }
    }

    fn panel() -> Vec<VerifierChoice> {
        vec![
            verifier(AgentKind::Claude, BuiltinStrategy::Conformance),
            verifier(AgentKind::Codex, BuiltinStrategy::Falsify),
        ]
    }

    fn state_of(repo: &Path) -> Target {
        Target {
            kind: TargetKind::State,
            repo: repo.to_owned(),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            spec: None,
        }
    }

    fn new_start(root: &Path, target: Target, isolation: Isolation, confirm: bool) -> Start {
        Start::New(Box::new(NewVerification {
            request: "Verify the library".into(),
            target,
            isolation,
            user_criteria: Vec::new(),
            confirm,
            max_rounds: 2,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel: panel(),
            root: root.join("store"),
        }))
    }

    fn rig_on(fake: &Fake, start: Start) -> Wired {
        let (mut runner, actions, updates) =
            open_wired(fake, start).unwrap_or_else(|e| panic!("{e}"));
        let mut conn = fake.clone();
        assert!(runner.begin(&mut conn).is_ok());
        (runner, actions, updates)
    }

    /// A new verification of `root`'s repository in worktree mode.
    fn rig(fake: &Fake, root: &Path, confirm: bool) -> Wired {
        let repo = repo(root);
        rig_on(
            fake,
            new_start(root, state_of(&repo), Isolation::Worktree, confirm),
        )
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

    const CRITERIA: &str = r#"{"summary": "One check.", "criteria": [{"id": "AC-1", "statement": "The library builds.", "how": "Run the build.", "priority": "must", "source": "inferred"}]}"#;

    /// A reply passing AC-1 on command `e1` (`exit 0`, its log at
    /// `logs/1.log`), marked for re-run or not.
    fn passing(rerun: bool) -> String {
        format!(
            r#"{{"summary": "Ran it.", "environment": "test", "results": [{{"criterion": "AC-1", "result": "pass", "note": "Built.", "evidence": ["e1"]}}], "evidence": [{{"id": "e1", "kind": "command", "cmd": "exit 0", "cwd": ".", "exit": 0, "log": "logs/1.log", "rerun": {rerun}}}]}}"#
        )
    }

    /// Lets a file just written settle and be taken in.
    fn settle(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        step(runner, conn);
        fake.advance(REPLY_STABLE_FOR + 1);
        step(runner, conn);
    }

    /// The judge's pane started and briefed on the criteria.
    fn brief_the_judge(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        step(runner, conn);
        fake.report(fake.pane_of(":judge"), "input", None);
        step(runner, conn);
        step(runner, conn);
        assert_eq!(fake.pasted(fake.pane_of(":judge")), 1);
    }

    /// The judge's criteria accepted.
    fn criteria(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        brief_the_judge(fake, runner, conn);
        fs::write(store::criteria_path(&runner.engine.reply_dir()), CRITERIA).unwrap();
        settle(fake, runner, conn);
        assert!(runner.engine.session().criteria_accepted);
    }

    /// Criteria accepted (not held), worktrees made, round 1's briefs
    /// pasted into a pane of each verifier.
    fn round_one_briefed(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        criteria(fake, runner, conn);
        step(runner, conn);
        assert_eq!(runner.engine.session().rounds.len(), 1, "round 1 started");
        step(runner, conn);
        for part in [":claude", ":codex"] {
            fake.report(fake.pane_of(part), "input", None);
        }
        step(runner, conn);
        step(runner, conn);
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} briefed");
        }
    }

    /// `part`'s round-1 reply with its log, written where its brief said.
    fn verifier_reply(runner: &Runner<Fake, Fake>, part: &str, text: &str) {
        let s = runner.engine.session();
        let part = PartId::new(part);
        let reply_dir = store::verifier_reply_dir(s, &part);
        let logs = store::part_dir(&reply_dir, 1, &part).join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("1.log"), "ok\n").unwrap();
        fs::write(store::reply_path(&reply_dir, 1, &part), text).unwrap();
    }

    /// Both verifiers replied: claude with a command marked for re-run,
    /// codex without; the round is ingested and claude's script written.
    fn replied(fake: &Fake, runner: &mut Runner<Fake, Fake>, conn: &mut Fake) {
        verifier_reply(runner, "claude", &passing(true));
        verifier_reply(runner, "codex", &passing(false));
        settle(fake, runner, conn);
        let round = runner.engine.session().round(1).unwrap();
        assert_eq!(round.phase, Phase::Ingesting, "{:?}", round.briefs);
        assert_eq!(round.ingested.len(), 2);
        assert!(round.reruns.contains_key(&PartId::new("claude")));
    }

    /// The rest of a stopped verification's directory, its runner dropped.
    fn stop(runner: Runner<Fake, Fake>) -> PathBuf {
        let dir = runner.engine.dir().to_owned();
        drop(runner);
        wait_unlocked(&dir);
        dir
    }

    fn rerun_pastes(fake: &Fake, pane: u64) -> usize {
        fake.pastes(pane)
            .iter()
            .filter(|t| t.contains(rerun::SCRIPT_FILE))
            .count()
    }

    /// A one-round verification of a repository under `root` that ended in
    /// a verdict: claude's build failed (its log stored, 100 lines), codex
    /// passed, and the judge ruled AC-1 failed and added defect D1 with
    /// its repro steps. Returns its store directory, its runner gone.
    pub(crate) fn finished_with_a_failure(root: &Path) -> PathBuf {
        let fake = fake();
        let repo = repo(root);
        let mut start = new_start(root, state_of(&repo), Isolation::Worktree, false);
        if let Start::New(new) = &mut start {
            new.max_rounds = 1;
        }
        let (mut runner, _actions, _updates) = rig_on(&fake, start);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        verifier_reply(&runner, "codex", &passing(false));
        verifier_reply(&runner, "claude", FAILING);
        let s = runner.engine.session().clone();
        let claude = PartId::new("claude");
        let log =
            store::part_dir(&store::verifier_reply_dir(&s, &claude), 1, &claude).join("logs/1.log");
        let lines: String = (1..=100).map(|n| format!("error line {n}\n")).collect();
        fs::write(log, lines).unwrap();
        settle(&fake, &mut runner, &mut conn);
        assert_eq!(
            runner.engine.session().round(1).unwrap().phase,
            Phase::Ruling
        );
        step(&mut runner, &mut conn);
        fs::write(
            store::judge_reply_path(&runner.engine.reply_dir(), 1),
            JUDGE_FAILED,
        )
        .unwrap();
        settle(&fake, &mut runner, &mut conn);
        let s = runner.engine.session();
        assert!(
            s.closed && s.verdict().is_some(),
            "{:?}",
            s.round(1).unwrap().judge_brief.as_ref().map(|b| &b.receipt)
        );
        stop(runner)
    }

    /// claude's round-1 reply: AC-1 fails on `cargo build` (exit 101),
    /// with a defect.
    const FAILING: &str = r#"{"summary": "The build fails.", "environment": "test",
 "results": [{"criterion": "AC-1", "result": "fail", "note": "cargo build exits 101.", "evidence": ["e1"]}],
 "evidence": [{"id": "e1", "kind": "command", "cmd": "cargo build", "cwd": ".", "exit": 101, "log": "logs/1.log", "rerun": false}],
 "defects": [{"id": "1", "claim": "one() does not build", "severity": "high", "category": "conformance", "confidence": "high",
   "repro": "1. cargo build\n2. read the first error", "evidence": ["e1"]}]}"#;

    const JUDGE_FAILED: &str = r#"{"summary": "The build fails.",
 "rulings": [{"criterion": "AC-1", "ruling": "failed", "rationale": "claude's build exits 101.", "evidence": ["claude/r1/e1"]}],
 "defect_ops": [{"op": "add", "id": "D1", "from": ["claude/1"], "claim": "one() does not build",
   "repro": "1. cargo build\n2. read the first error", "severity": "high", "category": "conformance",
   "confidence": "high", "status": "open", "rationale": "Seen by claude."}],
 "verdict": {"decision": "fail", "rationale": "AC-1 failed.", "blocking": ["AC-1", "D1"]}}"#;

    #[test]
    fn each_connection_reports_the_version_of_its_noa() {
        let root = temp_root();
        let fake = fake();
        let (_runner, _actions, updates) = rig(&fake, &root, true);
        let versions: Vec<String> = updates
            .try_iter()
            .filter_map(|u| match u {
                Update::Version(_, v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(versions, ["0.9.13"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_verification_captures_its_target_and_briefs_the_judge_in_its_reply_dir() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, true);
        let reply_dir = runner.engine.reply_dir();
        assert!(reply_dir.starts_with(root.join("repo/.noa/verify")));
        assert!(
            store::target_dir(&reply_dir)
                .join("manifest.json")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(root.join("repo/.noa/.gitignore")).unwrap(),
            "*\n"
        );
        let mut conn = fake.clone();
        brief_the_judge(&fake, &mut runner, &mut conn);
        let judge = fake.pane_of(":judge");
        assert_eq!(fake.cwd(judge), reply_dir);
        let id = runner.engine.session().id.clone();
        assert_eq!(
            fake.0.borrow().panes[&judge].label.as_deref(),
            Some(format!("ver:{id}:judge").as_str())
        );
        let brief = &fake.pastes(judge)[0];
        assert!(
            brief.contains(&store::criteria_path(&reply_dir).display().to_string()),
            "{brief}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_refused_start_leaves_nothing_behind() {
        let fake = fake();
        let root = temp_root();
        let repo = repo(&root);
        let plain = root.join("plain");
        fs::create_dir_all(&plain).unwrap();
        let err = open_wired(
            &fake,
            new_start(&root, state_of(&plain), Isolation::Worktree, true),
        )
        .err()
        .unwrap();
        assert!(err.contains("git の作業ツリー"), "{err}");
        let mut one = new_start(&root, state_of(&repo), Isolation::Worktree, true);
        if let Start::New(new) = &mut one {
            new.panel.truncate(1);
        }
        let err = open_wired(&fake, one).err().unwrap();
        assert!(err.contains("2体以上"), "{err}");
        let mut none = new_start(&root, state_of(&repo), Isolation::Worktree, true);
        if let Start::New(new) = &mut none {
            new.max_rounds = 0;
        }
        let err = open_wired(&fake, none).err().unwrap();
        assert!(err.contains("ラウンド上限"), "{err}");
        // An empty diff is refused after the capture: both reserved
        // folders go again.
        let mut diff = state_of(&repo);
        diff.kind = TargetKind::Diff;
        diff.base = Some("main".into());
        diff.head = Head::Ref("main".into());
        let err = open_wired(&fake, new_start(&root, diff, Isolation::Worktree, true))
            .err()
            .unwrap();
        assert!(err.contains("差分がありません"), "{err}");
        let left: Vec<_> = fs::read_dir(root.join("store"))
            .map(|d| d.collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(
            fs::read_dir(repo.join(".noa/verify")).unwrap().count(),
            0,
            "a reply dir stayed"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The store holds nothing and the checkout no reply dir.
    fn nothing_left(root: &Path, repo: &Path) {
        let left: Vec<_> = fs::read_dir(root.join("store"))
            .map(|d| d.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        let replies = fs::read_dir(repo.join(".noa/verify"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(replies, 0, "a reply dir stayed");
    }

    #[test]
    fn shared_mode_verifies_a_ref_only_in_a_clean_checkout_at_its_commit() {
        let fake = fake();
        let root = temp_root();
        let repo = repo(&root);
        git(&repo, &["checkout", "-q", "-b", "feat"]);
        fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "two"]);
        let feat = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "main"]);
        let mut target = state_of(&repo);
        target.head = Head::Ref("feat".into());
        let start = || new_start(&root, target.clone(), Isolation::Shared, true);

        // HEAD is elsewhere (main).
        let err = open_wired(&fake, start()).err().unwrap();
        assert!(err.starts_with(SHARED_REF_HEAD), "{err}");
        assert!(err.contains(&feat[..10]), "{err}");
        nothing_left(&root, &repo);

        // At the commit, with a change not committed.
        git(&repo, &["checkout", "-q", "feat"]);
        fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 3 }\n").unwrap();
        let err = open_wired(&fake, start()).err().unwrap();
        assert!(err.starts_with(SHARED_REF_HEAD), "{err}");
        assert!(err.contains("未コミットの変更"), "{err}");
        nothing_left(&root, &repo);

        // An untracked file is a change too.
        git(&repo, &["checkout", "-q", "--", "src/lib.rs"]);
        fs::write(repo.join("notes.txt"), "draft\n").unwrap();
        let err = open_wired(&fake, start()).err().unwrap();
        assert!(err.contains("未コミットの変更"), "{err}");
        nothing_left(&root, &repo);

        // Clean, at the ref's commit: accepted.
        fs::remove_file(repo.join("notes.txt")).unwrap();
        let (runner, _, _) = open_wired(&fake, start()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(runner.engine.session().rebuild.head_sha, feat);
        assert_eq!(runner.engine.session().isolation, Isolation::Shared);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn each_verifier_starts_in_its_own_worktree_with_the_round_one_brief() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        let s = runner.engine.session().clone();
        for part in ["claude", "codex"] {
            let part = PartId::new(part);
            let tree = &s.worktrees[&part];
            assert_eq!(tree.state, WorktreeState::Made);
            assert_eq!(
                tree.path,
                worktree::path_for(&s.main_worktree, &s.id, &part)
            );
            assert!(tree.path.starts_with(root.join("repo-wt")));
            assert!(tree.path.join("src/lib.rs").is_file());
            assert_eq!(
                fs::read_to_string(tree.path.join(".noa/.gitignore")).unwrap(),
                "*\n"
            );
            let pane = fake.pane_of(&format!(":{part}"));
            assert_eq!(fake.cwd(pane), tree.path);
            let brief = &fake.pastes(pane)[0];
            let reply = store::reply_path(&store::verifier_reply_dir(&s, &part), 1, &part);
            assert!(brief.contains(&reply.display().to_string()), "{brief}");
        }
        assert_eq!(fake.0.borrow().tabs.len(), 1, "one tab");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_rerun_the_agent_starts_is_watched_to_its_outcome_before_the_judge_is_briefed() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        replied(&fake, &mut runner, &mut conn);
        // Evidence copied into the judge's reply dir, with a manifest.
        let reply_dir = runner.engine.reply_dir();
        let key = EvidenceKey::reported(&PartId::new("codex"), 1, "e1");
        let item = runner.engine.session().evidence_item(&key).unwrap().clone();
        let stored = item.stored.expect("codex's log stored");
        assert_eq!(
            fs::read_to_string(reply_dir.join(&stored.path)).unwrap(),
            "ok\n"
        );
        assert!(reply_dir.join("evidence/r1/manifest.md").is_file());

        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        assert_eq!(rerun_pastes(&fake, claude), 1, "the re-run brief");
        let judge = fake.pane_of(":judge");
        assert_eq!(fake.pasted(judge), 1, "the judge waits for the re-run");
        // The agent runs the script as its brief says.
        let s = runner.engine.session().clone();
        let part = PartId::new("claude");
        let dir = store::rerun_dir(&store::verifier_reply_dir(&s, &part), 1, &part);
        let ran = Process::new("/bin/sh")
            .arg(dir.join(rerun::SCRIPT_FILE))
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(ran.success());
        for _ in 0..3 {
            fake.advance(1_000);
            step(&mut runner, &mut conn);
        }
        let key = EvidenceKey::reported(&part, 1, "e1");
        let item = runner.engine.session().evidence_item(&key).unwrap().clone();
        let Some(Rerun::Matched { exit: 0, log }) = &item.rerun else {
            panic!("{:?}", item.rerun);
        };
        let log = log.as_ref().expect("the re-run's log stored");
        assert_eq!(log.path, store::rerun_log_path(Path::new(""), 1, &part, 1));
        assert!(reply_dir.join(&log.path).is_file());
        let round = runner.engine.session().round(1).unwrap();
        assert!(
            round
                .brief(&part, BriefKind::Rerun)
                .unwrap()
                .receipt
                .started
                .is_some()
        );
        assert_eq!(round.phase, Phase::Ruling);
        assert_eq!(fake.pasted(judge), 2, "the judge's ruling brief");
        let _ = fs::remove_dir_all(root);
    }

    /// AC-12 end to end: a re-run script nobody starts. Its brief goes out,
    /// is sent again at each start timeout until the retries run out, then
    /// every item ends `not_run` ("re-run did not start") and the round
    /// goes on to the judge.
    #[test]
    fn ac12_a_script_that_never_writes_a_pid_ends_every_item_not_run_after_its_retries() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        replied(&fake, &mut runner, &mut conn);
        step(&mut runner, &mut conn);
        let claude = fake.pane_of(":claude");
        let judge = fake.pane_of(":judge");
        assert_eq!(rerun_pastes(&fake, claude), 1);
        let part = PartId::new("claude");
        let key = EvidenceKey::reported(&part, 1, "e1");
        let receipt = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .brief(&part, BriefKind::Rerun)
                .unwrap()
                .receipt
                .clone()
        };
        for retry in 1..=CONFIG.max_retries {
            fake.advance(CONFIG.start_timeout + 1);
            step(&mut runner, &mut conn);
            step(&mut runner, &mut conn);
            assert_eq!(receipt(&runner).retries, retry);
            assert_eq!(
                rerun_pastes(&fake, claude),
                1 + retry as usize,
                "retry {retry} sent"
            );
            assert!(
                runner
                    .engine
                    .session()
                    .evidence_item(&key)
                    .unwrap()
                    .rerun
                    .is_none()
            );
            assert_eq!(fake.pasted(judge), 1, "the judge was briefed early");
        }
        fake.advance(CONFIG.start_timeout + 1);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert!(receipt(&runner).failed.is_some());
        assert!(receipt(&runner).started.is_none());
        let s = runner.engine.session();
        for item in s.rerun_items(1, &part) {
            match &item.rerun {
                Some(Rerun::NotRun { why, log: None }) => {
                    assert!(why.starts_with(rerun::DID_NOT_START), "{why}")
                }
                other => panic!("{}: {other:?}", item.key),
            }
        }
        assert_eq!(rerun_pastes(&fake, claude), 1 + CONFIG.max_retries as usize);
        assert_eq!(s.round(1).unwrap().phase, Phase::Ruling);
        assert_eq!(fake.pasted(judge), 2, "the judge's ruling brief");
        let _ = fs::remove_dir_all(root);
    }

    /// A verification whose two verifiers took their round-1 brief, then
    /// stopped (the runner is dropped); returns its directory.
    fn stopped_after_round_one_briefs(fake: &Fake, root: &Path) -> PathBuf {
        let (mut runner, _actions, _updates) = rig(fake, root, false);
        let mut conn = fake.clone();
        round_one_briefed(fake, &mut runner, &mut conn);
        stop(runner)
    }

    #[test]
    fn ac19_a_resumed_verification_takes_up_the_agents_still_in_their_panes() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_round_one_briefs(&fake, &root);
        let stored = store::read_session(&dir).unwrap();
        let spawned = fake.0.borrow().spawned;
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir: dir.clone() });
        assert_eq!(*runner.engine.session(), stored);
        let mut conn = fake.clone();
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        assert_eq!(
            fake.0.borrow().spawned,
            spawned,
            "an agent was started again"
        );
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} briefed again");
        }
        verifier_reply(&runner, "claude", &passing(false));
        settle(&fake, &mut runner, &mut conn);
        let claude = PartId::new("claude");
        assert!(
            runner
                .engine
                .session()
                .round(1)
                .unwrap()
                .replies
                .contains_key(&claude)
        );
        let dir = stop(runner);
        assert!(
            store::read_session(&dir)
                .unwrap()
                .round(1)
                .unwrap()
                .replies
                .contains_key(&claude)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac19_a_resumed_verification_starts_agents_whose_panes_are_gone_again() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_round_one_briefs(&fake, &root);
        fake.0.borrow_mut().panes.clear();
        let (mut runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir });
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let s = runner.engine.session().clone();
        for part in ["claude", "codex"] {
            let pane = fake.pane_of(&format!(":{part}"));
            assert!(pane >= 103, "{part} was not started anew");
            let part = PartId::new(part);
            assert_eq!(fake.cwd(pane), store::verifier_root(&s, &part));
            fake.report(pane, "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in [":claude", ":codex"] {
            assert_eq!(fake.pasted(fake.pane_of(part)), 1, "{part} not briefed");
        }
        let round = runner.engine.session().round(1).unwrap();
        let brief = round
            .brief(&PartId::new("claude"), BriefKind::Work)
            .unwrap();
        assert_eq!(brief.receipt.retries, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac19_a_resume_rebuilds_a_missing_worktree_from_the_recipe() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_round_one_briefs(&fake, &root);
        let s = store::read_session(&dir).unwrap();
        let tree = s.worktrees[&PartId::new("codex")].path.clone();
        fs::remove_dir_all(&tree).unwrap();
        let (runner, _actions, _updates) = rig_on(&fake, Start::Resume { dir: dir.clone() });
        assert!(tree.join("src/lib.rs").is_file(), "not rebuilt");
        assert!(tree.join(".noa/.gitignore").is_file());
        assert!(worktree::check(&s.main_worktree, &tree, &s.rebuild.head_sha).is_ok());
        assert_eq!(
            runner.engine.session().worktrees[&PartId::new("codex")].state,
            WorktreeState::Made
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac19_a_resume_refuses_a_worktree_that_is_not_detached_at_the_head() {
        let fake = fake();
        let root = temp_root();
        let dir = stopped_after_round_one_briefs(&fake, &root);
        let s = store::read_session(&dir).unwrap();
        let tree = s.worktrees[&PartId::new("claude")].path.clone();
        git(&tree, &["checkout", "-q", "-b", "elsewhere"]);
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("再開できません"), "{err}");
        assert!(err.contains("claude"), "{err}");
        assert!(err.contains("elsewhere"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ac19_a_verification_running_elsewhere_is_not_resumed_twice() {
        let fake = fake();
        let root = temp_root();
        let (runner, _, _) = rig(&fake, &root, true);
        let dir = runner.engine.dir().to_owned();
        let err = open_wired(&fake, Start::Resume { dir: dir.clone() })
            .err()
            .unwrap();
        assert!(err.contains("を開けません"), "{err}");
        let dir = stop(runner);
        assert!(open_wired(&fake, Start::Resume { dir }).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_agents_panes_close_together_once_the_verification_has_ended() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        // Panes this runner did not start: the person's, a fix request's.
        let fix = fake.add_pane(&format!("fix · {} · fix-1", runner.engine.session().id));
        let theirs = fake.add_pane("zsh");
        let _ = notices(&updates);
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(fake.0.borrow().closed.is_empty(), "refused while running");
        assert_eq!(notices(&updates), [PANES_AFTER_END]);

        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(runner.engine.session().closed);
        let told = notices(&updates);
        assert_eq!(told, ["2 個のワークツリーを削除しました"], "{told:?}");
        let log = runner.engine.dir().join(store::EVENTS_FILE);
        let before = fs::read(&log).unwrap();
        let spawned = fake.0.borrow().spawned;
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(
            fake.0.borrow().closed.len(),
            3,
            "the judge and both verifiers"
        );
        assert!(fake.0.borrow().panes.contains_key(&fix));
        assert!(fake.0.borrow().panes.contains_key(&theirs));
        for _ in 0..3 {
            step(&mut runner, &mut conn);
        }
        assert_eq!(notices(&updates), ["3 個のペインを閉じました"]);
        assert_eq!(fake.0.borrow().spawned, spawned, "nothing started again");
        assert_eq!(fs::read(&log).unwrap(), before, "the log was written");
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(notices(&updates), [NO_PANES]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_whose_close_timed_out_closes_later_without_a_notice() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(runner.engine.session().closed);
        fake.0.borrow_mut().close_unsure = true;
        let _ = notices(&updates);
        actions.send(Action::ClosePanes).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("閉じたか確かめられなかったもの: judge"));
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
    fn a_stored_verification_closes_only_its_own_panes_and_only_once_it_has_ended() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, _updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        let id = runner.engine.session().id.clone();
        let fix = fake.add_pane(&format!("fix · {id} · fix-1"));
        let theirs = fake.add_pane("zsh");
        // Interrupted: its panes are what a resume takes up.
        let dir = stop(runner);
        let mut noa = fake.clone();
        assert_eq!(
            count_stored_panes(&mut noa, &dir),
            Err(PANES_AFTER_END.into())
        );
        assert_eq!(
            close_stored_panes(&mut noa, &dir),
            Err(PANES_AFTER_END.into())
        );
        assert!(fake.0.borrow().closed.is_empty());

        // Ended with 終了.
        let (mut runner, _, _) = rig_on(&fake, Start::Resume { dir });
        drop(actions);
        assert!(runner.feed_user(Input::UserClose).is_ok());
        let dir = stop(runner);
        assert_eq!(count_stored_panes(&mut noa, &dir), Ok(3));
        assert_eq!(
            close_stored_panes(&mut noa, &dir),
            Ok("3 個のペインを閉じました".into())
        );
        let w = fake.0.borrow();
        assert_eq!(w.closed.len(), 3);
        assert!(w.panes.contains_key(&fix) && w.panes.contains_key(&theirs));
        drop(w);
        assert_eq!(count_stored_panes(&mut noa, &dir), Ok(0));
        assert_eq!(close_stored_panes(&mut noa, &dir), Ok(NO_PANES.into()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn worktrees_are_removed_on_demand_and_a_resume_rebuilds_them() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        let _ = notices(&updates);
        // Not while it runs.
        actions.send(Action::RemoveWorktrees).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("検証が終わってから"));
        let trees: Vec<PathBuf> = runner
            .engine
            .session()
            .worktrees
            .values()
            .map(|t| t.path.clone())
            .collect();
        assert!(trees.iter().all(|t| t.is_dir()));

        // Interrupted, no runner: removed and logged.
        let dir = stop(runner);
        assert_eq!(
            remove_stored_worktrees(&dir),
            Ok("2 個のワークツリーを削除しました".into())
        );
        assert!(trees.iter().all(|t| !t.exists()));
        let stored = store::read_session(&dir).unwrap();
        assert!(
            stored
                .worktrees
                .values()
                .all(|t| t.state == WorktreeState::Removed)
        );
        assert_eq!(remove_stored_worktrees(&dir), Ok(NO_WORKTREES.into()));

        // Resumed mid-round: rebuilt before its briefs go anywhere.
        let (runner, _, _) = rig_on(&fake, Start::Resume { dir });
        assert!(trees.iter().all(|t| t.join("src/lib.rs").is_file()));
        assert!(
            runner
                .engine
                .session()
                .worktrees
                .values()
                .all(|t| t.state == WorktreeState::Made)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_worktree_that_is_not_the_logged_one_is_left_and_named() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        round_one_briefed(&fake, &mut runner, &mut conn);
        let s = runner.engine.session().clone();
        let codex = s.worktrees[&PartId::new("codex")].path.clone();
        git(&codex, &["checkout", "-q", "-b", "mine"]);
        let _ = notices(&updates);
        actions.send(Action::Close).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        let told = notices(&updates);
        assert!(
            told[0].starts_with("1 個のワークツリーを削除しました。削除しなかったもの: codex"),
            "{told:?}"
        );
        assert!(codex.is_dir());
        assert!(!s.worktrees[&PartId::new("claude")].path.exists());
        let s = runner.engine.session();
        assert_eq!(
            s.worktrees[&PartId::new("codex")].state,
            WorktreeState::Made
        );
        assert_eq!(
            s.worktrees[&PartId::new("claude")].state,
            WorktreeState::Removed
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn criteria_edits_and_the_start_reach_the_engine_from_the_screen() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        criteria(&fake, &mut runner, &mut conn);
        assert_eq!(runner.engine.session().phase(), Phase::Confirming);
        actions
            .send(Action::EditCriteria {
                edit: CriteriaEdit::Edit {
                    id: "AC-1".into(),
                    statement: Some("The library builds without warnings.".into()),
                    how: None,
                    priority: None,
                },
            })
            .unwrap();
        actions
            .send(Action::EditCriteria {
                edit: CriteriaEdit::Remove { id: "AC-1".into() },
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        let s = runner.engine.session();
        assert_eq!(
            s.criteria[0].statement,
            "The library builds without warnings."
        );
        assert_eq!(s.criteria_edits.len(), 1);
        assert_eq!(notices(&updates), ["基準は1件以上必要です"]);
        actions.send(Action::StartVerify).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(runner.engine.session().frozen_at.is_some());
        // The worktrees are made, then the round starts on the next tick.
        assert_eq!(runner.engine.session().worktrees.len(), 2);
        step(&mut runner, &mut conn);
        assert_eq!(runner.engine.session().rounds.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    /// A PNG's leading bytes.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

    #[test]
    fn the_persons_media_is_copied_and_logged_once_round_one_has_started() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, false);
        let mut conn = fake.clone();
        let shot = root.join("shot.png");
        fs::write(&shot, PNG).unwrap();
        let attach = || {
            Action::Attach(Box::new(Attachment {
                source: shot.clone(),
                caption: "The new tab in /tmp".into(),
                marks: Vec::new(),
                shows: vec!["AC-1".into()],
            }))
        };
        actions.send(attach()).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("開始後"));
        let user = store::user_evidence_dir(&runner.engine.reply_dir());
        assert!(!user.exists(), "copied before round 1");

        round_one_briefed(&fake, &mut runner, &mut conn);
        let _ = notices(&updates);
        actions.send(attach()).unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(notices(&updates), Vec::<String>::new());
        let key = EvidenceKey::user(1);
        let item = runner.engine.session().evidence_item(&key).unwrap().clone();
        let stored = item.stored.unwrap();
        assert_eq!(
            fs::read(runner.engine.reply_dir().join(&stored.path)).unwrap(),
            PNG
        );
        assert_eq!(item.attached.unwrap().round, 1);
        let EvidenceBody::File { caption, shows, .. } = item.body else {
            panic!("{:?}", item.body);
        };
        assert_eq!(caption.as_deref(), Some("The new tab in /tmp"));
        assert_eq!(shows, ["AC-1"]);

        // Turned down by the engine (an unknown criterion): no copy stays.
        actions
            .send(Action::Attach(Box::new(Attachment {
                source: shot.clone(),
                caption: "x".into(),
                marks: Vec::new(),
                shows: vec!["AC-9".into()],
            })))
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("AC-9"));
        assert_eq!(fs::read_dir(&user).unwrap().count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn round_control_reaches_the_engine_from_the_screen() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, _updates) = rig(&fake, &root, true);
        actions.send(Action::SetHold { hold: true }).unwrap();
        actions.send(Action::Extend { by: 3 }).unwrap();
        actions
            .send(Action::Intervene {
                to: vec![PartId::new("codex")],
                body: "境界値も見て".into(),
            })
            .unwrap();
        runner.drain_actions(None).unwrap();
        let s = runner.engine.session();
        assert!(s.hold);
        assert_eq!(s.max_rounds, 5);
        assert_eq!(s.messages.last().unwrap().body, "境界値も見て");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn focusing_an_agent_brings_its_pane_forward_or_says_why_not() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, actions, updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let judge = fake.pane_of(":judge");
        actions
            .send(Action::Focus {
                part: PartId::new("judge"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert_eq!(fake.0.borrow().focused, [judge]);
        assert!(notices(&updates).is_empty());
        // No verifier has a pane before round 1.
        actions
            .send(Action::Focus {
                part: PartId::new("claude"),
            })
            .unwrap();
        runner.drain_actions(Some(&mut conn)).unwrap();
        assert!(notices(&updates)[0].contains("claude のペインがありません"));
        fake.0.borrow_mut().panes.remove(&judge);
        actions
            .send(Action::Focus {
                part: PartId::new("judge"),
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
    fn a_brief_too_long_for_a_control_frame_goes_through_a_file_beside_the_reply() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let judge = PartId::new("judge");
        // Quotes and newlines double in JSON: the encoded size decides.
        let body = "\"\n".repeat(BRIEF_PASTE_MAX / 3);
        assert!(body.len() < BRIEF_PASTE_MAX);
        let output = Output {
            batches: Vec::new(),
            effects: vec![Effect::SendBrief {
                round: CRITERIA_ROUND,
                part: judge.clone(),
                kind: BriefKind::Work,
                body: body.clone(),
            }],
        };
        assert!(runner.apply_output(output).is_ok());
        let (_, path) = brief_file(
            runner.engine.session(),
            CRITERIA_ROUND,
            &judge,
            BriefKind::Work,
        );
        assert!(path.starts_with(runner.engine.reply_dir()));
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        let pane = fake.pane_of(":judge");
        fake.report(pane, "input", None);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.pastes(pane), [brief_pointer(&path)]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_is_typed_into_a_pane_back_at_its_shell_and_a_refused_enter_sends_again() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        step(&mut runner, &mut conn);
        let pane = fake.pane_of(":judge");
        let sent = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .criteria_brief
                .as_ref()
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
    fn a_reply_that_is_not_utf8_is_turned_down_and_counts_against_the_retries() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        brief_the_judge(&fake, &mut runner, &mut conn);
        let path = store::criteria_path(&runner.engine.reply_dir());
        let receipt = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .criteria_brief
                .as_ref()
                .unwrap()
                .receipt
                .clone()
        };
        for n in 1..=CONFIG.max_retries + 1 {
            fs::write(&path, b"{\"summary\": \"\xff\xfe\"}").unwrap();
            settle(&fake, &mut runner, &mut conn);
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
    fn a_state_change_whose_notification_was_lost_reaches_the_engine_on_the_full_sync() {
        let fake = fake();
        let root = temp_root();
        let (mut runner, _actions, _updates) = rig(&fake, &root, true);
        let mut conn = fake.clone();
        brief_the_judge(&fake, &mut runner, &mut conn);
        let pane = fake.pane_of(":judge");
        let started = |runner: &Runner<Fake, Fake>| {
            runner
                .engine
                .session()
                .criteria_brief
                .as_ref()
                .unwrap()
                .receipt
                .started
        };
        assert!(started(&runner).is_none());
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

    #[test]
    fn an_id_taken_anywhere_moves_on_and_a_refused_start_removes_only_its_own() {
        let root = temp_root();
        let work = repo(&root);
        // Another verification holds ver-5 in the store, another data dir
        // holds ver-5-1 in this checkout.
        fs::create_dir_all(root.join("store/ver-5")).unwrap();
        fs::write(root.join("store/ver-5/events.jsonl"), "theirs").unwrap();
        fs::create_dir_all(work.join(".noa/verify/ver-5-1")).unwrap();
        fs::write(work.join(".noa/verify/ver-5-1/x"), "theirs").unwrap();
        let (id, dir, reply) = reserve(&root.join("store"), &work, 5).unwrap();
        assert_eq!(id.as_str(), "ver-5-2");
        assert_eq!((dir.is_dir(), reply.is_dir()), (true, true));
        assert!(
            !root.join("store/ver-5-1").exists(),
            "a half-reserved id stayed"
        );

        // A start refused after the reservation (an empty diff) takes away
        // its own directories and leaves the others alone.
        let mut target = state_of(&work);
        target.kind = TargetKind::Diff;
        target.base = Some("main".into());
        target.head = Head::Ref("main".into());
        let new = NewVerification {
            request: "r".into(),
            target,
            isolation: Isolation::Worktree,
            user_criteria: Vec::new(),
            confirm: true,
            max_rounds: 1,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel: panel(),
            root: root.join("store"),
        };
        assert!(new_verification(new, CONFIG, 5).is_err());
        let mut left: Vec<String> = fs::read_dir(root.join("store"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["ver-5", "ver-5-2"]);
        assert_eq!(
            fs::read_to_string(root.join("store/ver-5/events.jsonl")).unwrap(),
            "theirs"
        );
        assert_eq!(
            fs::read_to_string(work.join(".noa/verify/ver-5-1/x")).unwrap(),
            "theirs"
        );
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

    /// Runs `n` claude verifiers through round 1 until the judge is
    /// briefed: every pane (the judge's too) must find room in the
    /// verification's tab.
    fn judge_is_briefed_after(n: usize) {
        let fake = fake();
        let root = temp_root();
        let repo = repo(&root);
        let mut start = new_start(&root, state_of(&repo), Isolation::Worktree, false);
        if let Start::New(new) = &mut start {
            new.panel = vec![verifier(AgentKind::Claude, BuiltinStrategy::Regression); n];
        }
        let (mut runner, _actions, _updates) = rig_on(&fake, start);
        let mut conn = fake.clone();
        criteria(&fake, &mut runner, &mut conn);
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        assert_eq!(fake.0.borrow().spawned, n + 1);
        let verifiers: Vec<PartId> = runner.engine.session().verifier_ids().cloned().collect();
        for part in &verifiers {
            fake.report(runner.panes[part].id, "input", None);
        }
        step(&mut runner, &mut conn);
        step(&mut runner, &mut conn);
        for part in &verifiers {
            assert_eq!(fake.pasted(runner.panes[part].id), 1, "{part} not briefed");
            verifier_reply(&runner, part.as_str(), &passing(false));
        }
        settle(&fake, &mut runner, &mut conn);
        assert_eq!(
            runner.engine.session().round(1).unwrap().phase,
            Phase::Ruling
        );
        step(&mut runner, &mut conn);
        let judge = runner.panes[&PartId::new("judge")].id;
        assert_eq!(fake.pasted(judge), 2, "the judge was not briefed");
        let w = fake.0.borrow();
        assert_eq!(
            w.tabs.len(),
            1,
            "the panes spilled into another tab: {:?}",
            w.tabs
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn six_verifiers_and_the_judge_fit_in_one_tab() {
        judge_is_briefed_after(6);
    }

    #[test]
    fn two_verifiers_and_the_judge_fit_in_one_tab() {
        judge_is_briefed_after(2);
    }
}
