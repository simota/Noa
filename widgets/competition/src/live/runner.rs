//! Runs one competition against a live Noa, on a thread of its own.
//!
//! Opens a Noa tab for the competition, spawns the judge and every contestant
//! as interactive agents in splits, creates each contestant's private entry
//! directory (`entries/<part>/`), snapshots deliverables on every submission
//! (`r<N>/artifacts/<part>/`), regenerates `compare.html`, and feeds back
//! deliveries, agent states, reply files, and user actions to the [`Engine`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use competition::check;
use competition::engine::{AgentState, Config, Effect, Engine, EngineError, Input, Setup};
use competition::model::export;
use competition::model::types::{AgentKind, Millis, PartId, Phase, RoundNo, Session, SessionId};
use competition::store::{self, Store};

use super::ipc::{Changes, Connect, IpcConnect, Noa, NoaError, PaneSpec, PaneView, Placement};

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
        judge: AgentKind,
        panel: Vec<AgentKind>,
        root: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Intervene { to: Vec<PartId>, body: String },
    SelectWinner { winner: PartId },
    Retry,
    Close,
    Adopt { winner: PartId, agent: AgentKind },
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
        session: Box<Session>,
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

fn agent_argv(kind: AgentKind) -> Vec<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_owned());
    vec![
        shell,
        "-l".into(),
        "-i".into(),
        "-c".into(),
        format!("exec {}", kind.command()),
    ]
}

fn adopt_argv(kind: AgentKind, prompt: &str) -> Vec<String> {
    let mut argv = agent_argv(kind);
    argv.pop();
    let script = match kind {
        AgentKind::Agy => format!("exec {} -i \"$1\"", kind.command()),
        _ => format!("exec {} \"$1\"", kind.command()),
    };
    argv.extend([script, "noa-adopt".to_owned(), prompt.to_owned()]);
    argv
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
    let phase_ok = if *part == session.judge.id {
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
    sends: Vec<PendingSend>,
    watches: Vec<WatchedReply>,
    last_tick: Millis,
    stop: bool,
    started: bool,
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
        let (engine, store) = match start {
            Start::New {
                request,
                workdir,
                max_rounds,
                reveal_interval,
                judge,
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
                    judge,
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
            sends: Vec::new(),
            watches: Vec::new(),
            last_tick: now,
            stop: false,
            started: false,
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
            session: Box::new(self.engine.session().clone()),
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
        self.sync_panes(conn)?;
        if !self.started {
            self.started = true;
            let now = self.clock.now();
            self.feed(Input::Start { now })?;
        }
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
            let found = known_id.and_then(|id| by_id.get(&id).copied()).or_else(|| {
                views
                    .iter()
                    .find(|v| v.label.as_deref() == Some(label.as_str()))
            });
            if let Some(view) = found {
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
                self.notice(format!("{part} のペインが閉じられました"));
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
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.stop = true;
                    return Ok(());
                }
            }
        }
    }

    fn adopt_winner(&mut self, conn: Option<&mut C::Conn>, winner: &PartId, agent: AgentKind) {
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
        match conn.spawn(Placement::NewTab, &spec) {
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
                    if let Err(e) = store::write_viewer(&reply_dir, self.engine.session()) {
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
            let pending = self.sends[idx].clone();
            if !in_play(self.engine.session(), pending.round, &pending.part) {
                self.sends.remove(idx);
                continue;
            }
            let now = self.clock.now();
            // A reply already on disk is taken in (or turned down) once it
            // settles; pasting the brief again would start a new turn.
            if self
                .watches
                .iter()
                .any(|w| w.round == pending.round && w.part == pending.part && w.path.exists())
            {
                self.sends[idx].queued_at = now;
                idx += 1;
                continue;
            }
            if !self.panes.contains_key(&pending.part) {
                if pending.spawns >= MAX_SPAWNS {
                    self.sends.remove(idx);
                    self.feed(Input::SendFailed {
                        round: pending.round,
                        part: pending.part,
                        error: "agent pane exited repeatedly".to_owned(),
                    })?;
                    continue;
                }
                self.sends[idx].spawns += 1;
                self.spawn_pane(conn, &pending.part)?;
                idx += 1;
                continue;
            }
            // Checked before the pane counts as ready from an earlier brief: a
            // retry or correction must wait for the agent's turn to end, and a
            // prompt from a later turn is the user's to answer.
            if let Some(p) = self.panes.get_mut(&pending.part)
                && (p.held() || p.busy())
            {
                p.ready = false;
                p.quiet = None;
            }
            let pane = self.panes[&pending.part].clone();
            if !pane.ready {
                if self.try_ready(conn, &pending.part, now)? {
                    // Ready now; send on this pass.
                } else {
                    // `try_ready` drops a pane found gone; the next pass starts it again.
                    let Some(pane_now) = self.panes.get(&pending.part) else {
                        idx += 1;
                        continue;
                    };
                    if pane_now.held() {
                        if !pane_now.told_held {
                            if let Some(p) = self.panes.get_mut(&pending.part) {
                                p.told_held = true;
                            }
                            self.notice(format!(
                                "{} が確認プロンプトで待機中です（Noa のペインで許可してください）",
                                pending.part
                            ));
                        }
                        self.sends[idx].queued_at = now;
                    } else if pane_now.busy() {
                        // The time limit starts again once the turn ends.
                        self.sends[idx].queued_at = now;
                    } else if now.saturating_sub(pending.queued_at) >= READY_LIMIT {
                        self.sends.remove(idx);
                        self.feed(Input::SendFailed {
                            round: pending.round,
                            part: pending.part,
                            error: "pane did not become ready in time".to_owned(),
                        })?;
                        continue;
                    }
                    idx += 1;
                    continue;
                }
            }
            let pane_id = self.panes[&pending.part].id;
            match conn.type_text(pane_id, &pending.body, true) {
                Ok(()) => {
                    self.clock.sleep(SUBMIT_DELAY);
                    if let Err(e) = conn.type_text(pane_id, "\r", false) {
                        match e {
                            NoaError::PaneGone => {
                                self.panes.remove(&pending.part);
                                idx += 1;
                                continue;
                            }
                            other => return Err(link(other)),
                        }
                    }
                    if let Some(p) = self.panes.get_mut(&pending.part) {
                        p.briefed = true;
                    }
                    self.sends.remove(idx);
                    let at = self.clock.now();
                    self.feed(Input::Sent {
                        round: pending.round,
                        part: pending.part,
                        at,
                    })?;
                }
                Err(NoaError::NotReady) => {
                    if let Some(p) = self.panes.get_mut(&pending.part) {
                        p.ready = false;
                    }
                    idx += 1;
                }
                Err(NoaError::PaneGone) => {
                    self.panes.remove(&pending.part);
                    idx += 1;
                }
                Err(NoaError::Refused(why)) => {
                    self.sends.remove(idx);
                    self.feed(Input::SendFailed {
                        round: pending.round,
                        part: pending.part,
                        error: why,
                    })?;
                }
                Err(other) => return Err(link(other)),
            }
        }
        Ok(())
    }

    fn try_ready(&mut self, conn: &mut C::Conn, part: &PartId, now: Millis) -> Result<bool, Fail> {
        let Some(pane) = self.panes.get(part).cloned() else {
            return Ok(false);
        };
        if pane.reports_ready() {
            if let Some(p) = self.panes.get_mut(part) {
                p.ready = true;
            }
            return Ok(true);
        }
        // Auto-trust Claude Code folder prompt only when running in our managed
        // competitions directory (never in a user's own repository).
        if self.engine.session().workdir.is_none()
            && pane.is_claude_trust()
            && pane.trust_keys < TRUST_KEYS_MAX
        {
            let screen = conn.screen(pane.id).unwrap_or_default();
            let can_press = match &pane.trust_screen {
                None => true,
                Some((prev, at)) => {
                    *prev != screen || now.saturating_sub(*at) >= TRUST_KEY_SHOWS_IN
                }
            };
            if can_press && screen.contains(CLAUDE_TRUST_YES) {
                let _ = conn.type_text(pane.id, "\r", false);
                if let Some(p) = self.panes.get_mut(part) {
                    p.trust_keys += 1;
                    p.trust_screen = Some((screen, now));
                }
                return Ok(false);
            }
        }
        if pane.held() || pane.busy() {
            return Ok(false);
        }
        match conn.paste_ready(pane.id) {
            Ok(true) => {
                let screen = conn.screen(pane.id).unwrap_or_default();
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
        let placement = match self.panes.values().next() {
            None => Placement::NewTab,
            Some(first) if self.panes.len() == 1 => Placement::Right(first.id),
            Some(_) => {
                let anchor = self.panes.values().last().map(|p| p.id).unwrap();
                Placement::Below(anchor)
            }
        };
        let cwd = self
            .engine
            .session()
            .workdir
            .clone()
            .unwrap_or_else(|| self.engine.dir().to_path_buf());
        let _ = fs::create_dir_all(&cwd);
        let spec = PaneSpec {
            argv: agent_argv(participant.kind),
            cwd,
            label: self.pane_label(part),
        };
        let pane_id = conn.spawn(placement, &spec).map_err(link)?;
        self.panes.insert(part.clone(), Pane::new(pane_id));
        self.feed(Input::PaneSpawned {
            part: part.clone(),
            pane_id,
        })
    }

    fn poll_replies(&mut self) -> Result<(), Fail> {
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

    /// Runs the snapshot check (viewer copy, screenshots, manifest) on a
    /// submitted entry. Problems are reported but never block the submission.
    fn check_snapshot(&self, part: &PartId, snapshot: &Path, out: &Path) {
        match check::inspect(snapshot, out) {
            Ok(inspection) => {
                let problems = inspection.problems();
                if !problems.is_empty() {
                    self.notice(format!(
                        "{part} のスナップショット確認で問題: {}",
                        problems.join(" / ")
                    ));
                }
            }
            Err(e) => self.notice(format!("{part} のスナップショット確認に失敗しました: {e}")),
        }
    }

    fn all_parts(&self) -> Vec<PartId> {
        let s = self.engine.session();
        std::iter::once(s.judge.id.clone())
            .chain(s.contestant_ids().cloned())
            .collect()
    }

    fn pane_label(&self, part: &PartId) -> String {
        format!("comp:{}:{part}", self.engine.session().id)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakePane {
        label: String,
        state: Option<String>,
        blocked: Option<String>,
        seq: u64,
    }

    #[derive(Default)]
    struct World {
        now: Millis,
        next_pane: u64,
        panes: BTreeMap<u64, FakePane>,
        events: VecDeque<Changes>,
        typed: Vec<(u64, String)>,
        spawned: usize,
        /// Panes that are gone by the time they are asked for a paste.
        gone_on_paste: bool,
    }

    /// A fake Noa and clock sharing one world.
    #[derive(Clone)]
    struct Fake(Rc<RefCell<World>>);

    impl Fake {
        fn view(id: u64, pane: &FakePane) -> PaneView {
            PaneView {
                pane_id: id,
                label: Some(pane.label.clone()),
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
                .find(|(_, p)| p.label.ends_with(label_end))
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
            let w = self.0.borrow();
            Ok(w.panes.iter().map(|(id, p)| Fake::view(*id, p)).collect())
        }

        fn changes(&mut self, _wait: Duration) -> Result<Option<Changes>, NoaError> {
            Ok(self.0.borrow_mut().events.pop_front())
        }

        fn spawn(&mut self, _placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            let mut w = self.0.borrow_mut();
            let id = w.next_pane;
            w.next_pane += 1;
            w.spawned += 1;
            w.panes.insert(
                id,
                FakePane {
                    label: spec.label.clone(),
                    state: None,
                    blocked: None,
                    seq: 0,
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
            self.0.borrow_mut().typed.push((pane, text.to_owned()));
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

    fn rig(fake: &Fake, root: &Path) -> Runner<Fake, Fake> {
        let (_actions_tx, actions) = mpsc::channel();
        let (updates, _updates_rx) = mpsc::channel();
        let start = Start::New {
            request: "Design a music player UI".into(),
            workdir: None,
            max_rounds: Some(2),
            reveal_interval: 5,
            judge: AgentKind::Claude,
            panel: vec![AgentKind::Claude, AgentKind::Codex],
            root: root.to_owned(),
        };
        let mut runner = Runner::open(
            start,
            fake.clone(),
            fake.clone(),
            CONFIG,
            actions,
            updates,
            Rc::new(|| {}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut conn = fake.clone();
        assert!(runner.sync_panes(&mut conn).is_ok());
        assert!(runner.feed(Input::Start { now: 1_000 }).is_ok());
        runner
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
}
