//! The live feed: a thread that holds the control connection to the running
//! Noa (`noa-ipc`) and sends the dashboard snapshots of its agent panes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use noa_ipc::{Client, ClientError, EventKind, Notification, Panel, Scope, ScopeSet, TextSource};

use crate::fleet::{Fleet, Orchestrator, Phase, State, Worker, minutes_text};

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(4);
const NOTIFY_WAIT: Duration = Duration::from_secs(1);
const RELIST_EVERY: Duration = Duration::from_secs(2);
const SCREEN_EVERY: Duration = Duration::from_secs(2);
const PULSE_EVERY: Duration = Duration::from_secs(5);
/// The feed wakes the window at least this often while it runs, so the
/// header's time since the last sync ticks between snapshots.
const HEARTBEAT: Duration = Duration::from_secs(1);
const SCREEN_MAX_BYTES: usize = 4000;
const SCREEN_TAIL_LINES: usize = 6;
/// A running pane whose preview has not changed for this long is quiet, as
/// on the TUI board.
const QUIET_SECS: f64 = 120.0;
const BUCKET_SECS: f64 = 5.0;
const BUCKETS: usize = 12;
const ALONE: &str = "単独";

#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub fleet: Fleet,
    pub link: Link,
    /// Seconds since the server last answered.
    pub synced_secs: u64,
    /// Whether Noa granted the `control` scope `noa.focusPane` needs. Its
    /// default `server-scopes` is `read` only.
    pub can_focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The panes whose screen tails the feed fetches: the expanded queue
    /// card and the worker in the details drawer.
    Watch(Vec<u64>),
    Focus(u64),
}

/// The dashboard's end of the feed.
pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    /// Why a command failed, for the header to show.
    pub notices: Receiver<String>,
    commands: Sender<Command>,
}

impl Channel {
    pub fn send(&self, command: Command) {
        // Fails only once the feed thread has ended, which it does only
        // after this end is dropped: there is no one left to tell.
        let _ = self.commands.send(command);
    }

    #[cfg(test)]
    pub fn detached() -> (Self, Sender<Snapshot>, Sender<String>, Receiver<Command>) {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        (
            Self {
                snapshots,
                notices,
                commands,
            },
            snapshot_tx,
            notice_tx,
            command_rx,
        )
    }
}

/// Starts the feed thread. `wake` is called after each snapshot it sends,
/// and at least every `HEARTBEAT` while the thread runs.
pub fn start(wake: impl Fn() + Send + 'static) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let feed = Feed {
        snapshots: snapshot_tx,
        notices: notice_tx,
        commands: command_rx,
        wake: Box::new(wake),
        tracker: Tracker::default(),
        watched: Vec::new(),
        screens: HashMap::new(),
        last_wake: Instant::now(),
        last_list: Instant::now(),
        last_contact: Instant::now(),
    };
    std::thread::Builder::new()
        .name("live-feed".into())
        .spawn(move || feed.run())
        .expect("the live feed thread starts");
    Channel {
        snapshots,
        notices,
        commands,
    }
}

enum Stop {
    /// The dashboard dropped its end.
    Gone,
    Lost(String),
}

impl From<ClientError> for Stop {
    fn from(err: ClientError) -> Self {
        Stop::Lost(rpc_reason(&err))
    }
}

struct Feed {
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    commands: Receiver<Command>,
    wake: Box<dyn Fn() + Send>,
    tracker: Tracker,
    watched: Vec<u64>,
    /// Each watched pane's screen tail and when it was fetched.
    screens: HashMap<u64, (Instant, Vec<String>)>,
    last_wake: Instant,
    last_list: Instant,
    last_contact: Instant,
}

impl Feed {
    fn run(mut self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            let reason = match connect() {
                Ok(mut client) => {
                    backoff = BACKOFF_MIN;
                    match self.follow(&mut client) {
                        Err(Stop::Gone) => return,
                        Err(Stop::Lost(reason)) => reason,
                        Ok(()) => return,
                    }
                }
                Err(reason) => reason,
            };
            let down = Snapshot {
                fleet: Fleet::default(),
                link: Link::Down(reason),
                synced_secs: 0,
                can_focus: false,
            };
            if self.publish(down).is_err() || !self.pause(backoff) {
                return;
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Waits `backoff` while keeping up with the dashboard's commands and
    /// the heartbeat; false once the dashboard is gone.
    fn pause(&mut self, backoff: Duration) -> bool {
        let until = Instant::now() + backoff;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(left.min(HEARTBEAT)) {
                Ok(Command::Watch(panes)) => self.watch(panes),
                Ok(Command::Focus(_)) => {}
                Err(RecvTimeoutError::Timeout) if left <= HEARTBEAT => return true,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
            self.heartbeat();
        }
    }

    /// Wakes the window when it has not been woken for `HEARTBEAT`.
    fn heartbeat(&mut self) {
        if self.last_wake.elapsed() >= HEARTBEAT {
            (self.wake)();
            self.last_wake = Instant::now();
        }
    }

    fn follow(&mut self, client: &mut Client) -> Result<(), Stop> {
        self.tracker.serve(client.server_instance_id());
        self.relist(client)?;
        client.subscribe(&[EventKind::StateChanged], None)?;
        let mut sent: Option<(Fleet, Instant)> = None;
        loop {
            self.take_commands(client)?;
            if self.last_list.elapsed() >= RELIST_EVERY {
                self.relist(client)?;
            }
            if let Some(notification) = client.next_notification(NOTIFY_WAIT)? {
                self.last_contact = Instant::now();
                let dropped = notification.dropped();
                if let Notification::StateChanged { panels, .. } = notification {
                    self.tracker.merge(panels, unix_now());
                }
                if dropped {
                    self.relist(client)?;
                }
            }
            self.tracker.advance(unix_now());
            self.fetch_screens(client)?;
            self.send_if_due(client, &mut sent)?;
            self.heartbeat();
        }
    }

    fn relist(&mut self, client: &mut Client) -> Result<(), Stop> {
        let panels = client.list_panels()?;
        self.last_list = Instant::now();
        self.last_contact = self.last_list;
        self.tracker.replace(panels, unix_now());
        self.tracker.read_journals();
        self.tracker.read_results();
        Ok(())
    }

    fn take_commands(&mut self, client: &mut Client) -> Result<(), Stop> {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Watch(panes)) => self.watch(panes),
                Ok(Command::Focus(pane)) => match client.focus_pane(pane) {
                    Err(ClientError::Rpc { message, .. }) => {
                        self.notice(format!("ペインへ移動できません: {message}"));
                    }
                    other => other?,
                },
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(Stop::Gone),
            }
        }
    }

    fn watch(&mut self, panes: Vec<u64>) {
        self.screens.retain(|pane, _| panes.contains(pane));
        self.watched = panes;
    }

    /// Fetches each watched worker's screen tail at most every `SCREEN_EVERY`.
    fn fetch_screens(&mut self, client: &mut Client) -> Result<(), Stop> {
        for &pane in &self.watched {
            if !self.tracker.is_worker(pane)
                || self
                    .screens
                    .get(&pane)
                    .is_some_and(|(at, _)| at.elapsed() < SCREEN_EVERY)
            {
                continue;
            }
            let tail = match client.get_text(pane, TextSource::Screen, Some(SCREEN_MAX_BYTES)) {
                Ok(result) => screen_tail(&result.text),
                // The pane closed since the last list: the next list drops it.
                Err(ClientError::Rpc { .. }) => Vec::new(),
                Err(err) => return Err(err.into()),
            };
            self.screens.insert(pane, (Instant::now(), tail));
        }
        Ok(())
    }

    fn send_if_due(
        &mut self,
        client: &Client,
        sent: &mut Option<(Fleet, Instant)>,
    ) -> Result<(), Stop> {
        let tails: HashMap<u64, Vec<String>> = self
            .screens
            .iter()
            .map(|(pane, (_, tail))| (*pane, tail.clone()))
            .collect();
        let fleet = self.tracker.fleet(unix_now(), Some(&tails));
        let due = sent
            .as_ref()
            .is_none_or(|(last, at)| !last.same_structure(&fleet) || at.elapsed() >= PULSE_EVERY);
        if !due {
            return Ok(());
        }
        self.publish(Snapshot {
            fleet: fleet.clone(),
            link: Link::Live,
            synced_secs: self.last_contact.elapsed().as_secs(),
            can_focus: client.granted_scopes().contains(Scope::Control),
        })?;
        *sent = Some((fleet, Instant::now()));
        Ok(())
    }

    fn notice(&self, text: String) {
        // Only the dashboard's end can be gone, and `publish` ends the feed then.
        let _ = self.notices.send(text);
        (self.wake)();
    }

    fn publish(&self, snapshot: Snapshot) -> Result<(), Stop> {
        self.snapshots.send(snapshot).map_err(|_| Stop::Gone)?;
        (self.wake)();
        Ok(())
    }
}

/// A control connection to the Noa this user's config names, with the
/// `read` and `control` scopes only.
fn connect() -> Result<Client, String> {
    let (config, _diagnostics) =
        noa_config::load_startup_config(noa_config::ConfigOverrides::default())
            .map_err(|err| format!("設定を読めません: {err:#}"))?;
    if !config.server_enable {
        return Err("設定でサーバーが無効です（server-enable = true が必要）".into());
    }
    let token = match config.server_token.filter(|token| !token.trim().is_empty()) {
        Some(token) => token,
        None => {
            let path = noa_config::server_token_path()
                .ok_or_else(|| "トークンの場所がわかりません".to_owned())?;
            std::fs::read_to_string(&path)
                .map(|token| token.trim().to_owned())
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| format!("トークンがありません: {}", path.display()))?
        }
    };
    let url = control_url(&config.server_bind, config.server_port);
    Client::connect(&url, &token, ScopeSet::from_strings(["read", "control"])).map_err(|err| {
        match err {
            ClientError::Rpc { .. } => rpc_reason(&err),
            other => format!("{url} に接続できません: {other}"),
        }
    })
}

fn rpc_reason(err: &ClientError) -> String {
    match err {
        ClientError::Rpc { .. } => format!("RPC エラー: {err}"),
        other => format!("接続が切れました: {other}"),
    }
}

/// The control URL for the configured bind address, as `Noa +ctl` builds it:
/// an unspecified bind address is reached over the loopback of its family.
fn control_url(bind: &str, port: u16) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let ip = match bind.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("ws://{}/", SocketAddr::new(ip, port))
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// What the feed saw of one pane since the dashboard started.
#[derive(Debug, Clone)]
struct Observed {
    state: State,
    state_since: f64,
    phase: Phase,
    phase_since: f64,
    /// Closed phases, oldest first, in seconds.
    history: Vec<(Phase, f64)>,
    /// When `preview` changed, within the sparkline's window.
    changes: VecDeque<f64>,
    /// When `preview` last changed; first sight counts as a change.
    last_change: f64,
    /// Whether the feed saw the pane enter its state (not already in it at
    /// first sight), so the time in it is known.
    state_seen: bool,
    /// Whether the feed saw `preview` change at all.
    change_seen: bool,
}

impl Observed {
    fn first(panel: &Panel, now: f64) -> Self {
        let state = state_of(panel);
        Self {
            state,
            state_since: now,
            phase: phase_of(state, false),
            phase_since: now,
            history: Vec::new(),
            changes: VecDeque::new(),
            last_change: now,
            state_seen: false,
            change_seen: false,
        }
    }

    fn saw(&mut self, old: &Panel, new: &Panel, now: f64) {
        if old.preview != new.preview {
            self.changes.push_back(now);
            self.last_change = now;
            self.change_seen = true;
        }
        let state = state_of(new);
        if state != self.state {
            self.state = state;
            self.state_since = now;
            self.state_seen = true;
        }
        self.advance(now);
    }

    fn quiet(&self, now: f64) -> bool {
        self.state == State::Running && now - self.last_change >= QUIET_SECS
    }

    fn advance(&mut self, now: f64) {
        let phase = phase_of(self.state, self.quiet(now));
        if phase != self.phase {
            self.history.push((self.phase, now - self.phase_since));
            self.phase = phase;
            self.phase_since = now;
        }
        let window = BUCKET_SECS * BUCKETS as f64;
        while self.changes.front().is_some_and(|at| now - at >= window) {
            self.changes.pop_front();
        }
    }

    /// The phases in minutes, the current one up to `now` included.
    fn phases(&self, now: f64) -> Vec<(Phase, f64)> {
        self.history
            .iter()
            .copied()
            .chain([(self.phase, now - self.phase_since)])
            .map(|(phase, secs)| (phase, secs / 60.0))
            .collect()
    }
}

#[derive(Default)]
struct Tracker {
    instance: String,
    panels: HashMap<u64, Panel>,
    seen: HashMap<u64, Observed>,
    /// Tasks by pane, from each orchestrator's journal (by its surface id).
    journals: HashMap<String, Journal>,
    /// The done panes known to have left a result file.
    results: HashSet<u64>,
}

struct Journal {
    modified: Option<SystemTime>,
    spawns: HashMap<u64, Spawned>,
}

/// What a spawner's journal says of the latest worker it spawned in a pane.
#[derive(Debug, Clone, Default, PartialEq)]
struct Spawned {
    /// Empty until a task is sent.
    task: String,
    /// The worker's own result file (`$NOA_RESULT`).
    result: Option<PathBuf>,
}

impl Tracker {
    /// Pane ids mean something only within one server instance: another
    /// one (Noa restarted) starts the observations over.
    fn serve(&mut self, instance: &str) {
        if self.instance != instance {
            *self = Tracker {
                instance: instance.to_owned(),
                ..Tracker::default()
            };
        }
    }

    fn is_worker(&self, pane: u64) -> bool {
        self.panels.get(&pane).is_some_and(is_worker)
    }

    fn replace(&mut self, panels: Vec<Panel>, now: f64) {
        let live: Vec<u64> = panels.iter().map(|p| p.pane_id.0).collect();
        self.panels.retain(|pane, _| live.contains(pane));
        self.seen.retain(|pane, _| live.contains(pane));
        self.merge(panels, now);
    }

    fn merge(&mut self, panels: Vec<Panel>, now: f64) {
        for panel in panels {
            let pane = panel.pane_id.0;
            match (self.panels.get(&pane), self.seen.get_mut(&pane)) {
                (Some(old), Some(seen)) => seen.saw(old, &panel, now),
                _ => {
                    self.seen.insert(pane, Observed::first(&panel, now));
                }
            }
            self.panels.insert(pane, panel);
        }
    }

    fn advance(&mut self, now: f64) {
        for seen in self.seen.values_mut() {
            seen.advance(now);
        }
    }

    /// Reads again each spawner's journal that changed on disk. A missing
    /// or unreadable journal gives no tasks.
    fn read_journals(&mut self) {
        let surfaces: Vec<String> = self
            .panels
            .values()
            .filter(|panel| is_worker(panel))
            .filter_map(|panel| {
                self.panels
                    .get(&panel.parent_pane_id?.0)?
                    .surface_id
                    .clone()
            })
            .collect();
        self.journals
            .retain(|surface, _| surfaces.contains(surface));
        for surface in surfaces {
            let Some(path) = noa_config::ctl_journal_path(&surface) else {
                continue;
            };
            let modified = modified(&path);
            if self
                .journals
                .get(&surface)
                .is_some_and(|journal| journal.modified == modified)
            {
                continue;
            }
            let spawns = std::fs::read_to_string(&path)
                .map(|text| journal_spawns(&text))
                .unwrap_or_default();
            self.journals.insert(surface, Journal { modified, spawns });
        }
    }

    /// Looks again at each done worker's own result file, the one its
    /// spawner's journal names. The file is there before the worker writes
    /// it, so an empty one is no result; no path in the journal, no result.
    fn read_results(&mut self) {
        self.results = self
            .panels
            .values()
            .filter(|panel| is_worker(panel) && state_of(panel) == State::Finished)
            .filter(|panel| {
                let result = self
                    .spawned(panel)
                    .and_then(|spawned| spawned.result.as_ref());
                result.is_some_and(|path| {
                    let path = Path::new(&panel.cwd).join(path);
                    std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
                })
            })
            .map(|panel| panel.pane_id.0)
            .collect();
    }

    fn spawned(&self, panel: &Panel) -> Option<&Spawned> {
        let parent = self.panels.get(&panel.parent_pane_id?.0)?;
        let journal = self.journals.get(parent.surface_id.as_ref()?)?;
        journal.spawns.get(&panel.pane_id.0)
    }

    fn task(&self, panel: &Panel) -> String {
        self.spawned(panel)
            .map(|spawned| spawned.task.clone())
            .unwrap_or_default()
    }

    /// The workers now; each pane in `screens` gets its screen tail.
    fn fleet(&self, now: f64, screens: Option<&HashMap<u64, Vec<String>>>) -> Fleet {
        let mut panes: Vec<u64> = self
            .panels
            .values()
            .filter(|panel| is_worker(panel))
            .map(|panel| panel.pane_id.0)
            .collect();
        panes.sort_unstable();
        let workers: Vec<Worker> = panes
            .into_iter()
            .filter_map(|pane| {
                let panel = self.panels.get(&pane)?;
                let tail = screens
                    .and_then(|screens| screens.get(&pane))
                    .map_or(&[][..], Vec::as_slice);
                Some(worker_of(
                    panel,
                    self.seen.get(&pane)?,
                    &self.panels,
                    self.task(panel),
                    tail,
                    self.results.contains(&pane),
                    now,
                ))
            })
            .collect();
        let mut orchestrators: Vec<Orchestrator> = Vec::new();
        for worker in &workers {
            let known = orchestrators
                .iter()
                .any(|o| o.name == worker.orchestrator && o.pane == worker.orchestrator_pane);
            if !known {
                orchestrators.push(Orchestrator {
                    name: worker.orchestrator.clone(),
                    pane: worker.orchestrator_pane,
                });
            }
        }
        Fleet {
            workers,
            orchestrators,
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// A worker is an agent pane or a pane an orchestrator spawned, but not a
/// board.
fn is_worker(panel: &Panel) -> bool {
    let board = panel
        .label
        .as_deref()
        .is_some_and(|label| label.starts_with("board:"));
    !board && (panel.agent.is_some() || panel.parent_pane_id.is_some())
}

fn state_of(panel: &Panel) -> State {
    if let Some(exit) = &panel.exit {
        return match exit.code {
            Some(code) if code != 0 => State::Error,
            _ => State::Finished,
        };
    }
    let state = panel
        .agent
        .as_ref()
        .and_then(|agent| agent.state.as_deref());
    match state {
        Some("permission") => State::Permission,
        Some("input") => State::Input,
        Some("finished") => State::Finished,
        Some("error") => State::Error,
        _ => State::Running,
    }
}

fn phase_of(state: State, quiet: bool) -> Phase {
    match state {
        State::Running if quiet => Phase::Quiet,
        State::Running => Phase::Running,
        State::Permission => Phase::Permission,
        State::Input => Phase::Input,
        State::Finished | State::Error => Phase::Ended,
    }
}

/// The model and effort flags `argv` passes to `kind`; empty when absent.
fn model_and_effort(kind: &str, argv: &[String]) -> (String, String) {
    let value = |names: &[&str]| -> String {
        argv.iter()
            .enumerate()
            .find_map(|(at, arg)| {
                names.iter().find_map(|name| {
                    if arg == name {
                        argv.get(at + 1).cloned()
                    } else {
                        arg.strip_prefix(&format!("{name}=")).map(str::to_owned)
                    }
                })
            })
            .unwrap_or_default()
    };
    match kind {
        "claude" => (value(&["--model"]), value(&["--effort"])),
        "codex" => {
            let effort = argv
                .iter()
                .filter_map(|arg| arg.strip_prefix("model_reasoning_effort="))
                .next_back()
                .map(|effort| effort.trim_matches(['"', '\'']).to_owned())
                .unwrap_or_default();
            (value(&["-m", "--model"]), effort)
        }
        "agy" => (value(&["--model"]), String::new()),
        _ => (String::new(), String::new()),
    }
}

/// The agent's kind, else the spawned command's name, else the process.
fn kind_of(panel: &Panel) -> String {
    let agent = panel.agent.as_ref().and_then(|agent| agent.kind.clone());
    let command = panel
        .spawn
        .as_ref()
        .and_then(|spawn| spawn.argv.first())
        .map(|argv0| argv0.rsplit('/').next().unwrap_or(argv0).to_owned());
    agent
        .or(command)
        .or_else(|| panel.process.clone())
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

fn name_of(panel: &Panel) -> String {
    [panel.label.as_deref(), Some(panel.name.as_str())]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
        .map_or_else(|| format!("ペイン {}", panel.pane_id.0), str::to_owned)
}

/// Where a worker sits in the relation graph.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placement {
    orchestrator: String,
    /// The orchestrator's pane, when it is one that is open.
    root: Option<u64>,
    /// For a grandchild, the worker that spawned it.
    parent: Option<u64>,
}

fn parent_of(pane: u64, panels: &HashMap<u64, Panel>) -> Option<u64> {
    panels.get(&pane)?.parent_pane_id.map(|id| id.0)
}

/// An orchestrator is the top of a chain of spawners: a pane that is not a
/// worker, or one with no parent that spawned workers. Its own workers hang
/// off it (`parent: None`); only theirs are grandchildren. An agent with no
/// parent and no workers is `単独`.
fn placement(pane: u64, panels: &HashMap<u64, Panel>) -> Placement {
    let alone = || Placement {
        orchestrator: ALONE.to_owned(),
        root: None,
        parent: None,
    };
    let Some(first) = parent_of(pane, panels) else {
        let spawned = panels
            .values()
            .any(|panel| is_worker(panel) && panel.parent_pane_id.map(|id| id.0) == Some(pane));
        return match panels.get(&pane) {
            Some(panel) if spawned => Placement {
                orchestrator: name_of(panel),
                root: Some(pane),
                parent: None,
            },
            _ => alone(),
        };
    };
    let mut current = first;
    // Bounded: a chain of parents cannot be longer than the panes.
    for _ in 0..=panels.len() {
        let (orchestrator, root) = match panels.get(&current) {
            None => (format!("ペイン {current}"), None),
            Some(panel) if !is_worker(panel) || panel.parent_pane_id.is_none() => {
                (name_of(panel), Some(current))
            }
            Some(_) => {
                current = parent_of(current, panels).unwrap_or(current);
                continue;
            }
        };
        return Placement {
            orchestrator,
            root,
            parent: (first != current).then_some(first),
        };
    }
    Placement {
        parent: Some(first),
        ..alone()
    }
}

/// Each pane's latest `spawn` in a journal (pane ids are given out again
/// after Noa restarts): its result path, and the first non-empty line of the
/// latest `send` after it as the task. Answers to prompts are not tasks.
fn journal_spawns(text: &str) -> HashMap<u64, Spawned> {
    let mut spawns: HashMap<u64, Spawned> = HashMap::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(pane) = event["pane"].as_u64() else {
            continue;
        };
        match event["ev"].as_str() {
            Some("spawn") => {
                let result = event["result"].as_str().map(PathBuf::from);
                spawns.insert(
                    pane,
                    Spawned {
                        result,
                        ..Spawned::default()
                    },
                );
            }
            Some("send") if event["answer"].as_bool() != Some(true) => {
                if let (Some(spawned), Some(text)) = (spawns.get_mut(&pane), event["text"].as_str())
                {
                    spawned.task = first_line(text).to_owned();
                }
            }
            _ => {}
        }
    }
    spawns
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// The last lines of a screen with text on them.
fn screen_tail(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(SCREEN_TAIL_LINES);
    lines[from..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

/// How many changes fell in each of the last `BUCKETS` buckets, oldest first.
fn buckets(changes: &VecDeque<f64>, now: f64) -> Vec<f64> {
    let mut counts = vec![0.0; BUCKETS];
    for at in changes {
        let ago = ((now - at) / BUCKET_SECS).floor();
        if (0.0..BUCKETS as f64).contains(&ago) {
            counts[BUCKETS - 1 - ago as usize] += 1.0;
        }
    }
    counts
}

/// 5 for output under 10 s old, down to 0 for output over 5 min old.
fn freshness(age_secs: f64) -> f64 {
    let steps = [10.0, 30.0, 60.0, 120.0, 300.0];
    let older = steps.iter().filter(|step| age_secs >= **step).count();
    (steps.len() - older) as f64
}

/// Time since something; when the feed did not see it happen (it was so at
/// first sight), only a lower bound is known.
#[derive(Debug, Clone, Copy)]
struct Elapsed {
    secs: f64,
    seen: bool,
}

impl Elapsed {
    fn text(self) -> String {
        minutes_text(self.secs / 60.0)
    }

    /// The time, or `N 以上` when only a lower bound is known.
    fn at_least(self) -> String {
        if self.seen {
            self.text()
        } else {
            format!("{}以上", self.text())
        }
    }
}

/// A time whose start the feed did not see is a lower bound, or is not
/// shown when it would claim when something happened.
fn since_text(state: State, quiet: bool, in_state: Elapsed, output: Elapsed) -> String {
    match state {
        State::Running if quiet => format!("出力なし {}", output.at_least()),
        State::Running if output.seen => format!("{}前に出力", output.text()),
        State::Finished if in_state.seen => format!("{}前に完了", in_state.text()),
        State::Running | State::Finished => String::new(),
        State::Permission | State::Input | State::Error => in_state.at_least(),
    }
}

fn detail_of(panel: &Panel) -> String {
    let detail = panel
        .agent
        .as_ref()
        .and_then(|agent| agent.detail.clone())
        .filter(|detail| !detail.trim().is_empty());
    let exit = panel.exit.as_ref().map(|exit| match exit.code {
        Some(code) => format!("exit {code}"),
        None => "終了".to_owned(),
    });
    match (exit, detail) {
        (Some(exit), Some(detail)) => format!("{exit} · {detail}"),
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) => String::new(),
    }
}

fn worker_of(
    panel: &Panel,
    seen: &Observed,
    panels: &HashMap<u64, Panel>,
    task: String,
    screen_tail: &[String],
    has_result: bool,
    now: f64,
) -> Worker {
    let pane = panel.pane_id.0;
    let kind = kind_of(panel);
    let argv = panel.spawn.as_ref().map_or(&[][..], |spawn| &spawn.argv);
    let (model, effort) = model_and_effort(&kind, argv);
    let Placement {
        orchestrator,
        root: orchestrator_pane,
        parent,
    } = placement(pane, panels);
    let state = seen.state;
    let quiet = seen.quiet(now);
    let state_secs = now - seen.state_since;
    let change_secs = now - seen.last_change;
    let wait_secs = match state {
        State::Running if quiet => change_secs,
        State::Permission | State::Input | State::Error => state_secs,
        State::Running | State::Finished => 0.0,
    };
    let in_state = Elapsed {
        secs: state_secs,
        seen: seen.state_seen,
    };
    let output = Elapsed {
        secs: change_secs,
        seen: seen.change_seen,
    };
    let started_secs = panel
        .spawn
        .as_ref()
        .map(|spawn| (now - spawn.started_at as f64).max(0.0));
    let took_minutes = started_secs
        .filter(|_| state == State::Finished && seen.state_seen)
        .map(|started| (started - state_secs).max(0.0) / 60.0);
    let running = state == State::Running;
    Worker {
        pane,
        label: name_of(panel),
        kind,
        branch: panel.branch.clone().unwrap_or_default(),
        model,
        effort,
        orchestrator,
        orchestrator_pane,
        parent,
        state,
        attention: panel.attention,
        task,
        detail: detail_of(panel),
        since: since_text(state, quiet, in_state, output),
        wait_minutes: wait_secs / 60.0,
        quiet,
        reports: panel.agent.as_ref().map_or(0, |agent| agent.seq),
        started_minutes: started_secs.map(|secs| secs / 60.0),
        took_minutes,
        has_result,
        screen_tail: screen_tail.to_vec(),
        lines_per_tick: if running {
            buckets(&seen.changes, now)
        } else {
            Vec::new()
        },
        freshness: if running { freshness(change_secs) } else { 0.0 },
        phases: seen.phases(now),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn panel(value: serde_json::Value) -> Panel {
        let mut base = serde_json::json!({
            "windowGroupId": "1",
            "windowId": "1",
            "paneId": "1",
            "name": "zsh",
            "cwd": "/r",
            "busy": false,
            "attention": false,
            "preview": [],
        });
        for (key, field) in value.as_object().unwrap() {
            base[key] = field.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn panels(list: Vec<Panel>) -> HashMap<u64, Panel> {
        list.into_iter().map(|p| (p.pane_id.0, p)).collect()
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn workers_are_agents_and_spawned_panes_but_not_boards() {
        let agent = serde_json::json!({ "seq": 0 });
        assert!(is_worker(&panel(serde_json::json!({ "agent": agent }))));
        assert!(is_worker(&panel(
            serde_json::json!({ "parentPaneId": "2" })
        )));
        assert!(!is_worker(&panel(serde_json::json!({}))));
        let board = serde_json::json!({ "agent": agent, "label": "board:main" });
        assert!(!is_worker(&panel(board)));
    }

    #[test]
    fn state_follows_exit_then_agent() {
        let with = |agent: Option<&str>, exit: Option<serde_json::Value>| {
            let mut value = serde_json::json!({});
            if let Some(state) = agent {
                value["agent"] = serde_json::json!({ "seq": 1, "state": state });
            }
            if let Some(exit) = exit {
                value["exit"] = exit;
            }
            state_of(&panel(value))
        };
        let code = |code: i32| Some(serde_json::json!({ "code": code }));
        assert_eq!(with(Some("running"), code(1)), State::Error);
        assert_eq!(with(Some("permission"), code(0)), State::Finished);
        assert_eq!(with(None, Some(serde_json::json!({}))), State::Finished);
        assert_eq!(with(Some("running"), None), State::Running);
        assert_eq!(with(Some("permission"), None), State::Permission);
        assert_eq!(with(Some("input"), None), State::Input);
        assert_eq!(with(Some("finished"), None), State::Finished);
        assert_eq!(with(Some("error"), None), State::Error);
        assert_eq!(with(None, None), State::Running);
    }

    #[test]
    fn phases_follow_state_and_quiet() {
        assert_eq!(phase_of(State::Running, false), Phase::Running);
        assert_eq!(phase_of(State::Running, true), Phase::Quiet);
        assert_eq!(phase_of(State::Permission, true), Phase::Permission);
        assert_eq!(phase_of(State::Input, false), Phase::Input);
        assert_eq!(phase_of(State::Finished, false), Phase::Ended);
        assert_eq!(phase_of(State::Error, false), Phase::Ended);
    }

    #[test]
    fn model_and_effort_come_from_each_agents_flags() {
        let claude = argv(&["claude", "--model", "opus", "--effort", "high"]);
        assert_eq!(
            model_and_effort("claude", &claude),
            ("opus".into(), "high".into())
        );
        let codex = argv(&[
            "codex",
            "-m",
            "gpt-6-luna",
            "-c",
            "model_reasoning_effort=low",
        ]);
        assert_eq!(
            model_and_effort("codex", &codex),
            ("gpt-6-luna".into(), "low".into())
        );
        let agy = argv(&["agy", "--model=gemini-4-neon-high"]);
        assert_eq!(
            model_and_effort("agy", &agy),
            ("gemini-4-neon-high".into(), String::new())
        );
        assert_eq!(
            model_and_effort("claude", &argv(&["claude"])),
            (String::new(), String::new())
        );
        // `-m` is codex's flag only.
        assert_eq!(
            model_and_effort("claude", &argv(&["claude", "-m", "x"])),
            (String::new(), String::new())
        );
    }

    #[test]
    fn kind_prefers_the_agent_then_the_command() {
        let spawn = serde_json::json!({ "argv": ["/usr/bin/cargo", "test"], "startedAt": 1 });
        let agent = serde_json::json!({ "seq": 0, "kind": "codex" });
        let both = panel(serde_json::json!({ "agent": agent, "spawn": spawn }));
        assert_eq!(kind_of(&both), "codex");
        assert_eq!(
            kind_of(&panel(serde_json::json!({ "spawn": spawn }))),
            "cargo"
        );
        assert_eq!(
            kind_of(&panel(serde_json::json!({ "process": "vim" }))),
            "vim"
        );
        assert_eq!(kind_of(&panel(serde_json::json!({}))), "shell");
    }

    #[test]
    fn placement_finds_the_orchestrator_and_the_spawning_worker() {
        let agent = serde_json::json!({ "seq": 0 });
        let all = panels(vec![
            panel(serde_json::json!({ "paneId": "1", "name": "zsh", "label": "release" })),
            panel(serde_json::json!({ "paneId": "2", "parentPaneId": "1", "agent": agent })),
            panel(serde_json::json!({ "paneId": "3", "parentPaneId": "2" })),
            panel(serde_json::json!({ "paneId": "4", "parentPaneId": "3" })),
            panel(serde_json::json!({ "paneId": "5", "agent": agent })),
            panel(serde_json::json!({ "paneId": "6", "parentPaneId": "9" })),
            panel(serde_json::json!({ "paneId": "7", "name": "orch" })),
            panel(serde_json::json!({ "paneId": "8", "parentPaneId": "7" })),
        ]);
        let at = |pane: u64| {
            let placed = placement(pane, &all);
            (placed.orchestrator, placed.root, placed.parent)
        };
        assert_eq!(at(2), ("release".into(), Some(1), None));
        assert_eq!(at(3), ("release".into(), Some(1), Some(2)));
        assert_eq!(at(4), ("release".into(), Some(1), Some(3)));
        assert_eq!(at(5), (ALONE.into(), None, None));
        assert_eq!(at(6), ("ペイン 9".into(), None, None));
        assert_eq!(at(8), ("orch".into(), Some(7), None));
    }

    /// The fleet seen on a real Noa: pane 3, an agent with no parent,
    /// spawned 33 (a plain command) and 34; pane 1 is an agent on its own.
    pub(crate) fn reported_fleet() -> Fleet {
        let claude =
            |state: &str| serde_json::json!({ "seq": 1, "kind": "claude", "state": state });
        let orchestrator = panel(serde_json::json!({
            "paneId": "3",
            "name": "noa-gpui ウィジェットによるUI拡張案",
            "agent": claude("running"),
        }));
        let sleeper = panel(serde_json::json!({
            "paneId": "33",
            "name": "sleep",
            "label": "demo-sleep",
            "parentPaneId": "3",
            "spawn": { "argv": ["sleep", "150"], "startedAt": 990 },
        }));
        let agent = panel(serde_json::json!({
            "paneId": "34",
            "name": "claude",
            "label": "demo-agent",
            "parentPaneId": "3",
            "attention": true,
            "agent": claude("finished"),
            "spawn": {
                "argv": ["claude", "--model", "sonnet", "--effort", "low"],
                "startedAt": 990,
            },
        }));
        let alone = panel(serde_json::json!({
            "paneId": "1",
            "name": "Claude Code",
            "agent": claude("finished"),
        }));
        let mut tracker = Tracker::default();
        tracker.merge(vec![orchestrator, sleeper, agent, alone], 1000.0);
        tracker.fleet(1001.0, None)
    }

    /// Two agent roots both named `Claude Code` and two shells both named
    /// `zsh`, each with one worker of its own.
    pub(crate) fn same_named_roots_fleet() -> Fleet {
        let claude = serde_json::json!({ "seq": 1, "kind": "claude", "state": "running" });
        let panes = vec![
            panel(serde_json::json!({ "paneId": "1", "name": "Claude Code", "agent": claude })),
            panel(serde_json::json!({ "paneId": "2", "name": "Claude Code", "agent": claude })),
            panel(serde_json::json!({
                "paneId": "11", "label": "w-a", "parentPaneId": "1", "agent": claude,
            })),
            panel(serde_json::json!({
                "paneId": "12", "label": "w-b", "parentPaneId": "2", "agent": claude,
            })),
            panel(serde_json::json!({ "paneId": "5", "name": "zsh" })),
            panel(serde_json::json!({ "paneId": "6", "name": "zsh" })),
            panel(serde_json::json!({ "paneId": "15", "label": "w-c", "parentPaneId": "5" })),
            panel(serde_json::json!({ "paneId": "16", "label": "w-d", "parentPaneId": "6" })),
        ];
        let mut tracker = Tracker::default();
        tracker.merge(panes, 1000.0);
        tracker.fleet(1001.0, None)
    }

    #[test]
    fn roots_with_one_name_stay_apart() {
        let fleet = same_named_roots_fleet();
        let roots: Vec<(&str, Option<u64>)> = fleet
            .orchestrators
            .iter()
            .map(|o| (o.name.as_str(), o.pane))
            .collect();
        assert_eq!(
            roots,
            [
                ("Claude Code", Some(1)),
                ("Claude Code", Some(2)),
                ("zsh", Some(5)),
                ("zsh", Some(6)),
            ]
        );
        let groups: Vec<(Option<&str>, Vec<u64>)> = crate::fleet::Grouping::Orchestrator
            .split(&fleet.workers)
            .into_iter()
            .map(|(name, members)| (name, members.iter().map(|w| w.pane).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                (Some("Claude Code"), vec![1, 11]),
                (Some("Claude Code"), vec![2, 12]),
                (Some("zsh"), vec![15]),
                (Some("zsh"), vec![16]),
            ]
        );
    }

    #[test]
    fn an_effort_without_a_model_is_kept() {
        let agent = serde_json::json!({ "seq": 0, "kind": "claude" });
        let with = |argv: &[&str]| {
            let spawn = serde_json::json!({ "argv": argv, "startedAt": 1 });
            let mut tracker = Tracker::default();
            tracker.merge(
                vec![panel(
                    serde_json::json!({ "paneId": "2", "agent": agent, "spawn": spawn }),
                )],
                1000.0,
            );
            tracker.fleet(1001.0, None).workers[0].model_line()
        };
        assert_eq!(with(&["claude", "--effort", "high"]), "モデル既定 · high");
        assert_eq!(with(&["claude"]), "モデル既定");
        assert_eq!(with(&["claude", "--model", "opus"]), "opus");
    }

    /// A fresh `.noa` directory for one test.
    fn noa_dir(name: &str) -> PathBuf {
        let cwd =
            std::env::temp_dir().join(format!("agent-dashboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(cwd.join(".noa")).unwrap();
        cwd
    }

    fn spawn_line(pane: u64, result: Option<&Path>) -> String {
        let mut event =
            serde_json::json!({ "ev": "spawn", "pane": pane, "argv": ["claude"], "ts": 1 });
        if let Some(result) = result {
            event["result"] = serde_json::json!(result.to_str().unwrap());
        }
        event.to_string()
    }

    fn with_journal(tracker: &mut Tracker, surface: &str, text: &str) {
        let journal = Journal {
            modified: None,
            spawns: journal_spawns(text),
        };
        tracker.journals.insert(surface.to_owned(), journal);
    }

    /// Whether a done worker (pane 7, spawned by pane 1) in `cwd` shows a
    /// result, given its spawner's journal.
    fn shows_result(cwd: &Path, label: Option<&str>, journal: &str) -> bool {
        let orchestrator = panel(serde_json::json!({ "paneId": "1", "surfaceId": "s-results" }));
        let mut worker = serde_json::json!({
            "paneId": "7",
            "parentPaneId": "1",
            "cwd": cwd.to_str().unwrap(),
            "agent": { "seq": 1, "kind": "claude", "state": "finished" },
        });
        if let Some(label) = label {
            worker["label"] = serde_json::json!(label);
        }
        let mut tracker = Tracker::default();
        tracker.merge(vec![orchestrator, panel(worker)], 1000.0);
        with_journal(&mut tracker, "s-results", journal);
        tracker.read_results();
        tracker.fleet(1001.0, None).worker(7).unwrap().has_result
    }

    #[test]
    fn a_result_is_the_workers_own_file() {
        let cwd = noa_dir("own-result");
        let own = cwd.join(".noa").join("result-review-0a1b2c3d.md");
        // A previous worker with the same label left its report.
        std::fs::write(cwd.join(".noa").join("result-review.md"), "old report").unwrap();
        std::fs::write(&own, "").unwrap();
        let journal = spawn_line(7, Some(&own));
        let before = shows_result(&cwd, Some("review"), &journal);
        std::fs::write(&own, "new report").unwrap();
        let after = shows_result(&cwd, Some("review"), &journal);
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!((before, after), (false, true));
    }

    #[test]
    fn an_unlabelled_worker_finds_its_result() {
        let cwd = noa_dir("unlabelled-result");
        let own = cwd.join(".noa").join("result-0a1b2c3d.md");
        std::fs::write(&own, "report").unwrap();
        let shown = shows_result(&cwd, None, &spawn_line(7, Some(&own)));
        std::fs::remove_dir_all(&cwd).unwrap();
        assert!(shown, "the unlabelled worker's own result is not shown");
    }

    #[test]
    fn no_journal_path_means_no_result() {
        let cwd = noa_dir("no-result-path");
        let file = cwd.join(".noa").join("result-review.md");
        std::fs::write(&file, "report").unwrap();
        let none = shows_result(&cwd, Some("review"), &spawn_line(7, None));
        // A later spawn of the same pane id (Noa restarted) has no path.
        let respawned = [spawn_line(7, Some(&file)), spawn_line(7, None)].join("\n");
        let after_respawn = shows_result(&cwd, Some("review"), &respawned);
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!((none, after_respawn), (false, false));
    }

    #[test]
    fn a_parentless_pane_with_workers_is_their_orchestrator() {
        let fleet = reported_fleet();
        let orchestrator = "noa-gpui ウィジェットによるUI拡張案";
        let roots: Vec<(&str, Option<u64>)> = fleet
            .orchestrators
            .iter()
            .map(|o| (o.name.as_str(), o.pane))
            .collect();
        assert_eq!(roots, [(ALONE, None), (orchestrator, Some(3))]);
        let placed: Vec<(u64, &str, Option<u64>)> = fleet
            .workers
            .iter()
            .map(|w| (w.pane, w.orchestrator.as_str(), w.parent))
            .collect();
        assert_eq!(
            placed,
            [
                (1, ALONE, None),
                (3, orchestrator, None),
                (33, orchestrator, None),
                (34, orchestrator, None),
            ]
        );
        let groups: Vec<(Option<&str>, Vec<u64>)> = crate::fleet::Grouping::Orchestrator
            .split(&fleet.workers)
            .into_iter()
            .map(|(name, members)| (name, members.iter().map(|w| w.pane).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                (Some(ALONE), vec![1]),
                (Some(orchestrator), vec![3, 33, 34]),
            ]
        );
        let sleeper = fleet.worker(33).unwrap();
        assert_eq!(
            (sleeper.kind.as_str(), sleeper.is_agent()),
            ("sleep", false)
        );
        let agent = fleet.worker(34).unwrap();
        assert_eq!(
            (agent.model.as_str(), agent.effort.as_str()),
            ("sonnet", "low")
        );
    }

    #[test]
    fn a_cycle_of_parents_ends() {
        let all = panels(vec![
            panel(serde_json::json!({ "paneId": "1", "parentPaneId": "2" })),
            panel(serde_json::json!({ "paneId": "2", "parentPaneId": "1" })),
        ]);
        let placed = placement(1, &all);
        assert_eq!(
            (placed.orchestrator.as_str(), placed.parent),
            (ALONE, Some(2))
        );
    }

    #[test]
    fn a_journal_gives_the_last_task_since_the_last_spawn() {
        let journal = [
            r#"{"ev":"spawn","pane":4,"label":"fix","argv":["claude"],"ts":1}"#,
            r#"{"ev":"send","pane":4,"text":"first task","ts":2}"#,
            r#"{"ev":"send","pane":4,"text":"\n  second task\nmore detail","ts":3}"#,
            r#"{"ev":"send","pane":4,"text":"1","answer":true,"ts":4}"#,
            r#"{"ev":"send","pane":6,"text":"before its spawn","ts":5}"#,
            r#"{"ev":"spawn","pane":6,"argv":["codex"],"ts":6}"#,
            r#"{"ev":"send","pane":8,"text":"old","ts":7}"#,
            r#"{"ev":"spawn","pane":8,"argv":["codex"],"ts":8}"#,
            "not json",
            r#"{"ev":"spawn","pane":9,"argv":["codex"],"ts":9}"#,
            r#"{"ev":"send","pane":9,"text":"old run","ts":10}"#,
            r#"{"ev":"spawn","pane":9,"argv":["codex"],"ts":11}"#,
            r#"{"ev":"send","pane":9,"text":"new run","ts":12}"#,
        ]
        .join("\n");
        let spawns = journal_spawns(&journal);
        let task = |pane: u64| spawns.get(&pane).map(|spawned| spawned.task.as_str());
        assert_eq!(task(4), Some("second task"));
        assert_eq!(task(6), Some(""));
        assert_eq!(task(8), Some(""));
        assert_eq!(task(9), Some("new run"));
        assert_eq!(task(5), None);
        assert_eq!(spawns.len(), 4);
    }

    #[test]
    fn the_screen_tail_is_the_last_six_lines_with_text() {
        let text = "a\nb\n\nc\nd  \n   \ne\nf\ng\n\n\n";
        assert_eq!(screen_tail(text), ["b", "c", "d", "e", "f", "g"]);
        assert_eq!(screen_tail("x\n"), ["x"]);
        assert!(screen_tail("\n \n").is_empty());
    }

    #[test]
    fn buckets_count_changes_per_five_seconds_oldest_first() {
        let now = 1000.0;
        let changes: VecDeque<f64> = [now - 61.0, now - 59.0, now - 12.0, now - 0.5, now - 4.9]
            .into_iter()
            .collect();
        let counts = buckets(&changes, now);
        assert_eq!(counts.len(), 12);
        assert_eq!(
            counts,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2.0]
        );
    }

    #[test]
    fn freshness_falls_with_age() {
        let cases = [
            (0.0, 5.0),
            (9.9, 5.0),
            (10.0, 4.0),
            (29.0, 4.0),
            (30.0, 3.0),
            (59.0, 3.0),
            (60.0, 2.0),
            (119.0, 2.0),
            (120.0, 1.0),
            (299.0, 1.0),
            (300.0, 0.0),
            (9999.0, 0.0),
        ];
        for (age, level) in cases {
            assert_eq!(freshness(age), level, "at {age} s");
        }
    }

    #[test]
    fn since_says_what_the_time_is_since() {
        let seen = |secs: f64| Elapsed { secs, seen: true };
        let unseen = |secs: f64| Elapsed { secs, seen: false };
        let cases = [
            (
                State::Running,
                false,
                seen(600.0),
                seen(8.0),
                "8 秒前に出力",
            ),
            (State::Running, false, seen(600.0), unseen(8.0), ""),
            (
                State::Running,
                true,
                seen(600.0),
                seen(180.0),
                "出力なし 3 分",
            ),
            (
                State::Running,
                true,
                seen(600.0),
                unseen(180.0),
                "出力なし 3 分以上",
            ),
            (State::Permission, false, seen(40.0), seen(1.0), "40 秒"),
            (
                State::Permission,
                false,
                unseen(40.0),
                seen(1.0),
                "40 秒以上",
            ),
            (State::Input, false, seen(360.0), seen(1.0), "6 分"),
            (State::Error, false, unseen(360.0), seen(1.0), "6 分以上"),
            (
                State::Finished,
                false,
                seen(240.0),
                seen(1.0),
                "4 分前に完了",
            ),
            (State::Finished, false, unseen(240.0), seen(1.0), ""),
        ];
        for (state, quiet, in_state, output, text) in cases {
            assert_eq!(
                since_text(state, quiet, in_state, output),
                text,
                "{state:?} quiet={quiet} {in_state:?} {output:?}"
            );
        }
    }

    #[test]
    fn control_url_reaches_an_unspecified_bind_over_loopback() {
        assert_eq!(control_url("0.0.0.0", 7681), "ws://127.0.0.1:7681/");
        assert_eq!(control_url("::", 7681), "ws://[::1]:7681/");
        assert_eq!(control_url("192.168.1.2", 9), "ws://192.168.1.2:9/");
        assert_eq!(control_url("nonsense", 9), "ws://127.0.0.1:9/");
    }

    fn running_agent(pane: &str, parent: &str, preview: &str) -> Panel {
        panel(serde_json::json!({
            "paneId": pane,
            "parentPaneId": parent,
            "name": "claude",
            "branch": "feat/x",
            "preview": [{ "row": 0, "spans": [{ "text": preview }] }],
            "agent": { "seq": 3, "kind": "claude", "state": "running", "detail": "building" },
            "spawn": { "argv": ["claude", "--model", "opus"], "startedAt": 410 },
        }))
    }

    #[test]
    fn a_panel_becomes_a_worker() {
        let orchestrator =
            panel(serde_json::json!({ "paneId": "1", "label": "release", "surfaceId": "s1" }));
        let mut tracker = Tracker::default();
        tracker.merge(vec![orchestrator, running_agent("2", "1", "a")], 1000.0);
        tracker.merge(vec![running_agent("2", "1", "b")], 1003.0);
        let tail = ["$ cargo test".to_owned()];
        let tails = HashMap::from([(2, tail.to_vec())]);
        let fleet = tracker.fleet(1010.0, Some(&tails));
        assert_eq!(fleet.orchestrators.len(), 1);
        assert_eq!(fleet.orchestrators[0].name, "release");
        let worker = &fleet.workers[0];
        assert_eq!(fleet.workers.len(), 1);
        assert_eq!((worker.pane, worker.label.as_str()), (2, "claude"));
        assert_eq!(
            (worker.kind.as_str(), worker.model.as_str()),
            ("claude", "opus")
        );
        assert_eq!(worker.effort, "");
        assert_eq!(worker.branch, "feat/x");
        assert_eq!(worker.detail, "building");
        assert_eq!(worker.state, State::Running);
        assert_eq!(worker.since, "7 秒前に出力");
        assert_eq!(worker.freshness, 5.0);
        assert_eq!(worker.reports, 3);
        assert_eq!(worker.screen_tail, tail);
        assert_eq!(worker.lines_per_tick.iter().sum::<f64>(), 1.0);
        assert_eq!(worker.started_minutes, Some(10.0));
        assert_eq!(worker.phases, [(Phase::Running, 10.0 / 60.0)]);
        assert_eq!(worker.task, "");
    }

    #[test]
    fn two_watched_panes_get_their_tails() {
        let orchestrator =
            panel(serde_json::json!({ "paneId": "1", "label": "release", "surfaceId": "s1" }));
        let mut tracker = Tracker::default();
        tracker.merge(
            vec![
                orchestrator,
                running_agent("2", "1", "a"),
                running_agent("3", "1", "b"),
                running_agent("4", "1", "c"),
            ],
            1000.0,
        );
        let tails = HashMap::from([
            (2, vec!["$ make".to_owned()]),
            (3, vec!["$ cargo test".to_owned(), "ok".to_owned()]),
        ]);
        let fleet = tracker.fleet(1010.0, Some(&tails));
        assert_eq!(fleet.workers.len(), 3);
        for worker in &fleet.workers {
            let expected = tails.get(&worker.pane).cloned().unwrap_or_default();
            assert_eq!(worker.screen_tail, expected, "pane {}", worker.pane);
        }
    }

    #[test]
    fn observed_phases_start_at_first_sight_and_turn_quiet() {
        let mut tracker = Tracker::default();
        tracker.merge(vec![running_agent("2", "1", "a")], 1000.0);
        tracker.advance(1000.0 + QUIET_SECS);
        let worker = &tracker.fleet(1000.0 + QUIET_SECS + 60.0, None).workers[0];
        assert!(worker.quiet);
        assert_eq!(worker.since, "出力なし 3 分以上");
        assert_eq!(worker.wait_minutes, 3.0);
        assert_eq!(worker.phases, [(Phase::Running, 2.0), (Phase::Quiet, 1.0)]);
        let mut asking = running_agent("2", "1", "a");
        asking.agent.as_mut().unwrap().state = Some("permission".into());
        tracker.merge(vec![asking], 1000.0 + QUIET_SECS + 60.0);
        let worker = &tracker.fleet(1000.0 + QUIET_SECS + 90.0, None).workers[0];
        assert_eq!(worker.state, State::Permission);
        assert_eq!(worker.since, "30 秒");
        assert_eq!(worker.phases.last(), Some(&(Phase::Permission, 0.5)));
    }

    fn finished(pane: &str, label: &str, spawn: bool) -> Panel {
        let mut value = serde_json::json!({
            "paneId": pane,
            "parentPaneId": "1",
            "label": label,
            "cwd": "/nonexistent",
            "agent": { "seq": 3, "kind": "claude", "state": "finished" },
        });
        if spawn {
            value["spawn"] = serde_json::json!({ "argv": ["claude"], "startedAt": 400 });
        }
        panel(value)
    }

    #[test]
    fn a_state_already_there_at_first_sight_has_no_time() {
        let mut tracker = Tracker::default();
        let mut asking = running_agent("3", "1", "a");
        asking.agent.as_mut().unwrap().state = Some("permission".into());
        let mut no_spawn = running_agent("4", "1", "a");
        no_spawn.spawn = None;
        tracker.merge(vec![finished("2", "done", true), asking, no_spawn], 1000.0);
        let fleet = tracker.fleet(1001.0, None);
        let done = fleet.worker(2).unwrap();
        assert_eq!((done.since.as_str(), done.took_minutes), ("", None));
        assert!(!done.has_result);
        assert_eq!(fleet.worker(3).unwrap().since, "1 秒以上");
        let running = fleet.worker(4).unwrap();
        assert_eq!(
            (running.since.as_str(), running.started_minutes),
            ("", None)
        );
    }

    #[test]
    fn a_finish_seen_with_a_known_start_has_its_duration() {
        let mut tracker = Tracker::default();
        let mut working = finished("2", "done", true);
        working.agent.as_mut().unwrap().state = Some("running".into());
        tracker.merge(vec![working], 1000.0);
        tracker.merge(vec![finished("2", "done", true)], 1060.0);
        let worker = &tracker.fleet(1120.0, None).workers[0];
        assert_eq!(worker.since, "1 分前に完了");
        // Started at 400, finished at 1060.
        assert_eq!(worker.took_minutes, Some(11.0));
        let mut tracker = Tracker::default();
        let mut working = finished("2", "done", false);
        working.agent.as_mut().unwrap().state = Some("running".into());
        tracker.merge(vec![working], 1000.0);
        tracker.merge(vec![finished("2", "done", false)], 1060.0);
        let worker = &tracker.fleet(1120.0, None).workers[0];
        assert_eq!(
            (worker.since.as_str(), worker.took_minutes),
            ("1 分前に完了", None)
        );
    }

    #[test]
    fn a_relist_drops_closed_panes() {
        let mut tracker = Tracker::default();
        let both = vec![running_agent("2", "1", "a"), running_agent("3", "1", "a")];
        tracker.replace(both, 1000.0);
        tracker.replace(vec![running_agent("3", "1", "a")], 1002.0);
        let panes: Vec<u64> = tracker
            .fleet(1003.0, None)
            .workers
            .iter()
            .map(|w| w.pane)
            .collect();
        assert_eq!(panes, [3]);
        assert!(!tracker.seen.contains_key(&2));
    }

    #[test]
    fn another_server_instance_forgets_what_was_seen() {
        let mut tracker = Tracker::default();
        tracker.serve("a");
        tracker.merge(vec![running_agent("2", "1", "a")], 1000.0);
        tracker.serve("a");
        assert_eq!(tracker.seen.len(), 1);
        tracker.serve("b");
        assert!(tracker.seen.is_empty() && tracker.panels.is_empty());
    }
}
