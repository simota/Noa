//! The live feed: a thread that holds the control connection to the running
//! Noa (`noa-ipc`) and sends the dashboard snapshots of its agent panes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use noa_ipc::{Client, ClientError, EventKind, Notification, Panel, Scope, ScopeSet, TextSource};

use crate::fleet::{Fleet, Orchestrator, Phase, State, Worker, minutes_text, one_line};

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(4);
const NOTIFY_WAIT: Duration = Duration::from_secs(1);
const RELIST_EVERY: Duration = Duration::from_secs(2);
const SCREEN_EVERY: Duration = Duration::from_secs(2);
const PULSE_EVERY: Duration = Duration::from_secs(5);
/// The window is woken at least this often while the feed runs, so the
/// header's time since the last sync ticks between snapshots.
const HEARTBEAT: Duration = Duration::from_secs(1);
const SCREEN_MAX_BYTES: usize = 4000;
/// The drawer shows this many; a queue card the last of them
/// (`Worker::card_tail`).
const SCREEN_TAIL_LINES: usize = 40;
/// A running pane whose preview has not changed for this long is quiet, as
/// on the TUI board.
const QUIET_SECS: f64 = 120.0;
const BUCKET_SECS: f64 = 5.0;
const BUCKETS: usize = 12;
pub(crate) const ALONE: &str = "単独";

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
    /// The server instance the fleet's pane ids belong to
    /// (`Client::server_instance_id`); `None` while the link is down. A
    /// restarted Noa gives its ids out again, to other panes.
    pub instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The panes whose screen tails the feed fetches: the expanded queue
    /// card and the worker in the details drawer. Pane ids belong to one
    /// server instance: the feed takes them as the panes of the instance of
    /// the newest snapshot it had sent (the dashboard can know no newer
    /// one), and fetches them only while it follows that instance. The
    /// dashboard sends its list whenever it changes and again with each
    /// live snapshot it takes in, so a list the feed named after an
    /// instance the dashboard never showed is named again by the next one;
    /// the same list for the same instance changes nothing.
    Watch(Vec<u64>),
    Focus(u64),
}

/// The dashboard's end of the feed.
pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    /// Why a command failed, for the notice line to show.
    pub notices: Receiver<String>,
    commands: Sender<Command>,
    /// Dropped with this end, which ends the heartbeat thread.
    _heartbeat: Sender<()>,
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
                _heartbeat: mpsc::channel().0,
            },
            snapshot_tx,
            notice_tx,
            command_rx,
        )
    }
}

/// Starts the feed thread. `wake` is called after each snapshot it sends,
/// and every `HEARTBEAT` until the channel is dropped.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    start_with(wake, HEARTBEAT, connect)
}

/// `start` with the heartbeat's period and the way the feed reaches Noa
/// given (the tests give a Noa of their own).
fn start_with<C: Noa + 'static>(
    wake: impl Fn() + Send + Sync + 'static,
    every: Duration,
    connect: impl FnMut() -> Result<C, String> + Send + 'static,
) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let (heartbeat, _thread) = heartbeat(every, Arc::clone(&wake));
    let feed = Feed::new(
        snapshot_tx,
        notice_tx,
        command_rx,
        wake,
        Box::new(Instant::now),
    );
    std::thread::Builder::new()
        .name("live-feed".into())
        .spawn(move || feed.run(connect))
        .expect("the live feed thread starts");
    Channel {
        snapshots,
        notices,
        commands,
        _heartbeat: heartbeat,
    }
}

/// Calls `wake` every `every` on a thread of its own: the feed thread can
/// wait seconds on Noa (connecting, or a request near its 5 s timeout), and
/// the header's time since the last sync must tick meanwhile. The thread
/// ends once the returned sender is dropped.
fn heartbeat(every: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> (Sender<()>, JoinHandle<()>) {
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("live-heartbeat".into())
        .spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                wake();
            }
        })
        .expect("the heartbeat thread starts");
    (stop, thread)
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

/// What the feed asks of the Noa it follows: a control connection
/// (`noa_ipc::Client`), or a scripted one in the tests.
trait Noa {
    /// The server's per-process identity (`Client::server_instance_id`).
    fn instance(&self) -> &str;
    fn can_focus(&self) -> bool;
    fn list_panels(&mut self) -> Result<Vec<Panel>, ClientError>;
    /// Subscribes to every pane's state changes.
    fn subscribe(&mut self) -> Result<(), ClientError>;
    fn next_notification(&mut self, wait: Duration) -> Result<Option<Notification>, ClientError>;
    fn focus_pane(&mut self, pane: u64) -> Result<(), ClientError>;
    /// The pane's screen text, at most `SCREEN_MAX_BYTES` of it.
    fn screen(&mut self, pane: u64) -> Result<String, ClientError>;
}

impl Noa for Client {
    fn instance(&self) -> &str {
        self.server_instance_id()
    }

    fn can_focus(&self) -> bool {
        self.granted_scopes().contains(Scope::Control)
    }

    fn list_panels(&mut self) -> Result<Vec<Panel>, ClientError> {
        Client::list_panels(self)
    }

    fn subscribe(&mut self) -> Result<(), ClientError> {
        Client::subscribe(self, &[EventKind::StateChanged], None).map(drop)
    }

    fn next_notification(&mut self, wait: Duration) -> Result<Option<Notification>, ClientError> {
        Client::next_notification(self, wait)
    }

    fn focus_pane(&mut self, pane: u64) -> Result<(), ClientError> {
        Client::focus_pane(self, pane)
    }

    fn screen(&mut self, pane: u64) -> Result<String, ClientError> {
        let result = self.get_text(pane, TextSource::Screen, Some(SCREEN_MAX_BYTES))?;
        Ok(result.text)
    }
}

/// Where the feed reads the time: `Instant::now`, or a test's own clock.
type Clock = Box<dyn Fn() -> Instant + Send>;

struct Feed {
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    commands: Receiver<Command>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The time the feed's intervals and ages are measured in (relists,
    /// screen fetches, pulses, `synced_secs`).
    clock: Clock,
    tracker: Tracker,
    /// The panes the dashboard last asked for (`Command::Watch`).
    watched: Vec<u64>,
    /// The server instance whose panes `watched` names: the one of the
    /// newest snapshot published when the `Watch` came (`shown`). The
    /// panes are fetched only while the feed follows that instance: the
    /// same ids on another one are other panes, which nobody chose. The
    /// list itself outlives a detour to another instance the dashboard
    /// never shows: back on the instance it shows, the feed fetches what
    /// it showed there. The dashboard sends its list again with each live
    /// snapshot it takes in, which names it afresh after the instance
    /// followed then.
    watched_on: Option<String>,
    /// The server instance of the newest snapshot published, `None`
    /// before the first.
    shown: Option<String>,
    /// Each watched pane's screen tail and when it was fetched.
    screens: HashMap<u64, (Instant, Vec<String>)>,
    last_list: Instant,
    last_contact: Instant,
}

impl Feed {
    fn new(
        snapshots: Sender<Snapshot>,
        notices: Sender<String>,
        commands: Receiver<Command>,
        wake: Arc<dyn Fn() + Send + Sync>,
        clock: Clock,
    ) -> Self {
        let now = clock();
        Feed {
            snapshots,
            notices,
            commands,
            wake,
            clock,
            tracker: Tracker::default(),
            watched: Vec::new(),
            watched_on: None,
            shown: None,
            screens: HashMap::new(),
            last_list: now,
            last_contact: now,
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    /// How long ago `then` was, by the feed's clock.
    fn since(&self, then: Instant) -> Duration {
        self.now().saturating_duration_since(then)
    }

    fn run<C: Noa>(mut self, mut connect: impl FnMut() -> Result<C, String>) {
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
                // An error's text from the server or the system: on one
                // line, like every text from Noa (`Worker::on_one_line`).
                link: Link::Down(one_line(&reason)),
                synced_secs: 0,
                can_focus: false,
                instance: None,
            };
            if self.publish(down).is_err() || !self.pause(backoff) {
                return;
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Waits `backoff` while keeping up with the dashboard's commands; false
    /// once the dashboard is gone. The wait is real: it reads the system's
    /// clock, not the feed's.
    fn pause(&mut self, backoff: Duration) -> bool {
        let until = Instant::now() + backoff;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(left) {
                Ok(Command::Watch(panes)) => self.watch(panes),
                Ok(Command::Focus(_)) => {}
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Follows the server instance `instance`. Pane ids mean something only
    /// within one instance, so another one drops the screen tails fetched
    /// so far with what the tracker saw: a new pane with an old id must not
    /// show the old pane's screen. The panes watched stay with the instance
    /// they belong to (`watched_on`).
    fn serve(&mut self, instance: &str) {
        if self.tracker.serve(instance) {
            self.screens.clear();
        }
    }

    fn follow(&mut self, client: &mut impl Noa) -> Result<(), Stop> {
        self.serve(client.instance());
        self.relist(client)?;
        client.subscribe()?;
        // The subscription's answer is the server's too.
        self.last_contact = self.now();
        let mut sent: Option<(Fleet, Instant)> = None;
        loop {
            self.take_commands(client)?;
            if self.since(self.last_list) >= RELIST_EVERY {
                self.relist(client)?;
            }
            if let Some(notification) = client.next_notification(NOTIFY_WAIT)? {
                self.last_contact = self.now();
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
        }
    }

    fn relist(&mut self, client: &mut impl Noa) -> Result<(), Stop> {
        let panels = client.list_panels()?;
        self.last_list = self.now();
        self.last_contact = self.last_list;
        self.tracker.replace(panels, unix_now());
        self.tracker.read_journals();
        self.tracker.read_results();
        Ok(())
    }

    fn take_commands(&mut self, client: &mut impl Noa) -> Result<(), Stop> {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Watch(panes)) => self.watch(panes),
                // A pane focused is an answer from the server, as a list
                // or a notification is.
                Ok(Command::Focus(pane)) => match client.focus_pane(pane) {
                    Ok(()) => self.last_contact = self.now(),
                    Err(ClientError::Rpc { message, .. }) => {
                        self.notice(format!("ペインへ移動できません: {message}"));
                    }
                    Err(err) => return Err(err.into()),
                },
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(Stop::Gone),
            }
        }
    }

    /// A `Watch` from the dashboard: panes of the instance it was last
    /// sent (`watched_on`). The same list again keeps the tails fetched
    /// and when they were, so the dashboard can repeat it with every
    /// snapshot without fetching more often.
    fn watch(&mut self, panes: Vec<u64>) {
        self.screens.retain(|pane, _| panes.contains(pane));
        self.watched = panes;
        self.watched_on.clone_from(&self.shown);
    }

    /// Fetches each watched worker's screen tail at most every `SCREEN_EVERY`,
    /// while the feed follows the instance the dashboard watched it on.
    /// A tail that came is an answer from the server, as a list or a
    /// notification is: the time since the last sync starts over.
    fn fetch_screens(&mut self, client: &mut impl Noa) -> Result<(), Stop> {
        if self.watched_on.as_deref() != Some(self.tracker.instance.as_str()) {
            return Ok(());
        }
        for &pane in &self.watched {
            if !self.tracker.is_worker(pane)
                || self
                    .screens
                    .get(&pane)
                    .is_some_and(|(at, _)| self.since(*at) < SCREEN_EVERY)
            {
                continue;
            }
            let answer = client.screen(pane);
            let now = self.now();
            let tail = match answer {
                Ok(text) => {
                    self.last_contact = now;
                    screen_tail(&text)
                }
                // The pane closed since the last list: the next list drops it.
                Err(ClientError::Rpc { .. }) => Vec::new(),
                Err(err) => return Err(err.into()),
            };
            self.screens.insert(pane, (now, tail));
        }
        Ok(())
    }

    fn send_if_due(
        &mut self,
        client: &impl Noa,
        sent: &mut Option<(Fleet, Instant)>,
    ) -> Result<(), Stop> {
        let tails: HashMap<u64, Vec<String>> = self
            .screens
            .iter()
            .map(|(pane, (_, tail))| (*pane, tail.clone()))
            .collect();
        let fleet = self.tracker.fleet(unix_now(), Some(&tails));
        let now = self.now();
        let due = sent.as_ref().is_none_or(|(last, at)| {
            !last.same_structure(&fleet) || now.saturating_duration_since(*at) >= PULSE_EVERY
        });
        if !due {
            return Ok(());
        }
        self.publish(Snapshot {
            fleet: fleet.clone(),
            link: Link::Live,
            synced_secs: now.saturating_duration_since(self.last_contact).as_secs(),
            can_focus: client.can_focus(),
            instance: Some(client.instance().to_owned()),
        })?;
        // From now on the dashboard can have this instance's panes to
        // ask for.
        self.shown = Some(client.instance().to_owned());
        *sent = Some((fleet, now));
        Ok(())
    }

    fn notice(&self, text: String) {
        // Only the dashboard's end can be gone, and `publish` ends the feed then.
        let _ = self.notices.send(one_line(&text));
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
    /// The ended panes known to have left a result file, and that file.
    results: HashMap<u64, PathBuf>,
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
    /// one (Noa restarted) starts the observations over. True when it did.
    fn serve(&mut self, instance: &str) -> bool {
        let other = self.instance != instance;
        if other {
            *self = Tracker {
                instance: instance.to_owned(),
                ..Tracker::default()
            };
        }
        other
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

    /// Looks again at each ended worker's own result file (one that failed
    /// may have written why), the one its
    /// spawner's journal names. The file is there before the worker writes
    /// it, so an empty one is no result; no path in the journal, no result.
    fn read_results(&mut self) {
        self.results = self
            .panels
            .values()
            .filter(|panel| {
                is_worker(panel) && matches!(state_of(panel), State::Finished | State::Error)
            })
            .filter_map(|panel| {
                let result = self.spawned(panel)?.result.as_ref()?;
                let path = Path::new(&panel.cwd).join(result);
                std::fs::metadata(&path)
                    .is_ok_and(|meta| meta.len() > 0)
                    .then_some((panel.pane_id.0, path))
            })
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
                    self.results.get(&pane).cloned(),
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
    result: Option<PathBuf>,
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
        has_result: result.is_some(),
        result,
        screen_tail: screen_tail.to_vec(),
        lines_per_tick: if running {
            buckets(&seen.changes, now)
        } else {
            Vec::new()
        },
        freshness: if running { freshness(change_secs) } else { 0.0 },
        phases: seen.phases(now),
    }
    // Every text above comes from Noa (pane titles, agent details, branch
    // names, journals, screens) and may hold line breaks or a paragraph
    // separator, which the shaper panics on next to right-to-left text.
    // The orchestrators' names are copied from these workers.
    .on_one_line()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use noa_gpui_core::widgets::Label;
    use noa_gpui_core::{Event, Key, Modifiers, UiTree, WidgetId};
    use noa_gpui_platform::Screen;
    use noa_gpui_scene::Size;
    use noa_gpui_text::TextStore;

    use super::*;
    use crate::dashboard::Dashboard;
    use crate::fleet::Grouping;

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
    fn a_failed_worker_shows_its_result() {
        let cwd = noa_dir("failed-result");
        let own = cwd.join(".noa").join("result-0a1b2c3d.md");
        std::fs::write(&own, "why it failed").unwrap();
        let orchestrator = panel(serde_json::json!({ "paneId": "1", "surfaceId": "s-failed" }));
        let worker = panel(serde_json::json!({
            "paneId": "7",
            "parentPaneId": "1",
            "cwd": cwd.to_str().unwrap(),
            "agent": { "seq": 1, "kind": "claude", "state": "running" },
            "exit": { "code": 1 },
        }));
        let mut tracker = Tracker::default();
        tracker.merge(vec![orchestrator, worker], 1000.0);
        with_journal(&mut tracker, "s-failed", &spawn_line(7, Some(&own)));
        tracker.read_results();
        let fleet = tracker.fleet(1001.0, None);
        let worker = fleet.worker(7).unwrap();
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(worker.state, State::Error);
        assert_eq!(worker.result.as_deref(), Some(own.as_path()));
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
    fn the_screen_tail_is_the_last_lines_with_text() {
        let text = "a\nb\n\nc\nd  \n   \ne\nf\ng\n\n\n";
        assert_eq!(screen_tail(text), ["a", "b", "c", "d", "e", "f", "g"]);
        let long: String = (0..SCREEN_TAIL_LINES + 5)
            .map(|n| format!("{n}\n"))
            .collect();
        let tail = screen_tail(&long);
        assert_eq!(tail.len(), SCREEN_TAIL_LINES);
        assert_eq!(tail.first().map(String::as_str), Some("5"));
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

    /// Holds each of the breaks `one_line` takes out, around right-to-left
    /// text: the shaper panics on "A\u{2029}\u{5D0}" in one label.
    const BROKEN: &str = "A\u{2029}\u{5D0}\u{2028}b\nc\td\u{85}e";

    /// Whether `text` would be drawn as more than one paragraph.
    fn breaks(text: &str) -> bool {
        text.chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    }

    #[test]
    fn every_text_from_noa_is_on_one_line() {
        let orchestrator =
            panel(serde_json::json!({ "paneId": "1", "label": BROKEN, "surfaceId": "s1" }));
        // An agent whose model and effort come from its flags, and a command
        // whose kind is its own text.
        let agent = panel(serde_json::json!({
            "paneId": "2",
            "parentPaneId": "1",
            "name": "claude",
            "label": BROKEN,
            "branch": BROKEN,
            "agent": { "seq": 3, "kind": "claude", "state": "running", "detail": BROKEN },
            "spawn": { "argv": ["claude", "--model", BROKEN, "--effort", BROKEN], "startedAt": 410 },
        }));
        let command = panel(serde_json::json!({
            "paneId": "3",
            "parentPaneId": "1",
            "name": BROKEN,
            "agent": { "seq": 1, "kind": BROKEN, "state": "permission", "detail": BROKEN },
            "spawn": { "argv": [BROKEN], "startedAt": 410 },
        }));
        let mut tracker = Tracker::default();
        tracker.merge(vec![orchestrator, agent, command], 1000.0);
        // A task is the first line of what was sent: every other break stays.
        let task = BROKEN.replace('\n', " ");
        let journal = [
            spawn_line(2, None),
            serde_json::json!({ "ev": "send", "pane": 2, "text": task, "ts": 2 }).to_string(),
        ]
        .join("\n");
        with_journal(&mut tracker, "s1", &journal);
        let tails = HashMap::from([(2, vec![BROKEN.to_owned(), BROKEN.to_owned()])]);
        let fleet = tracker.fleet(1010.0, Some(&tails));

        let line = crate::fleet::one_line(BROKEN);
        assert_eq!(line, "A \u{5D0} b c d e");
        let agent = fleet.worker(2).unwrap();
        let command = fleet.worker(3).unwrap();
        assert_eq!(agent.label, line);
        assert_eq!(
            (agent.model.as_str(), agent.effort.as_str()),
            (&*line, &*line)
        );
        assert_eq!(agent.branch, line);
        assert_eq!(agent.detail, line);
        assert_eq!(agent.task, crate::fleet::one_line(&task));
        assert_eq!(agent.screen_tail, [line.clone(), line.clone()]);
        assert_eq!(agent.orchestrator, line);
        assert_eq!(command.kind, line);
        assert_eq!(command.label, line);
        assert_eq!(fleet.orchestrators.len(), 1);
        assert_eq!(fleet.orchestrators[0].name, line);
        for worker in &fleet.workers {
            let texts = [
                &worker.label,
                &worker.kind,
                &worker.branch,
                &worker.model,
                &worker.effort,
                &worker.orchestrator,
                &worker.task,
                &worker.detail,
                &worker.since,
            ];
            for text in texts.into_iter().chain(&worker.screen_tail) {
                assert!(!breaks(text), "pane {}: {text:?}", worker.pane);
            }
        }
    }

    #[test]
    fn a_lost_links_reason_and_a_notice_are_on_one_line() {
        let (snapshots, published) = mpsc::channel();
        let (notices, noticed) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel::<Command>();
        // With the dashboard's commands gone, the feed publishes the lost
        // link once and ends instead of waiting to connect again.
        drop(commands);
        let clock: Clock = Box::new(Instant::now);
        let feed = Feed::new(snapshots, notices, command_rx, Arc::new(|| {}), clock);
        let line = crate::fleet::one_line(BROKEN);
        feed.notice(BROKEN.to_owned());
        assert_eq!(noticed.try_recv().unwrap(), line);
        feed.run(|| Err::<ScriptedNoa, _>(BROKEN.to_owned()));
        assert_eq!(published.try_recv().unwrap().link, Link::Down(line));
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

    /// A clock that moves only when the test moves it: the feed's
    /// intervals and ages then do not depend on how fast the machine runs.
    #[derive(Clone)]
    struct TestClock(Arc<std::sync::Mutex<Instant>>);

    impl TestClock {
        fn new() -> Self {
            TestClock(Arc::new(std::sync::Mutex::new(Instant::now())))
        }

        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }

        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }

        fn boxed(&self) -> Clock {
            let clock = self.clone();
            Box::new(move || clock.now())
        }
    }

    /// A feed on `clock` whose dashboard end the test holds: what it
    /// publishes, and the sender of its commands (kept, or the feed takes
    /// the dashboard for gone).
    fn held_feed(clock: &TestClock) -> (Feed, Receiver<Snapshot>, Sender<Command>) {
        let (snapshots, published) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let wake = Arc::new(|| {});
        let notices = mpsc::channel().0;
        let feed = Feed::new(snapshots, notices, command_rx, wake, clock.boxed());
        (feed, published, commands)
    }

    /// As if the feed followed `instance` and had sent its snapshot: the
    /// dashboard can ask for that instance's panes.
    fn shows(feed: &mut Feed, instance: &str) {
        feed.serve(instance);
        feed.shown = Some(instance.to_owned());
    }

    /// A Noa that answers from a script: the panels it lists and the
    /// screens it shows. Its connection is lost once it has waited
    /// `waits` times for a notification. Its answers take no time on the
    /// test's clock unless a test says how long they take.
    struct ScriptedNoa {
        instance: String,
        panels: Vec<Panel>,
        screens: HashMap<u64, String>,
        waits: usize,
        clock: TestClock,
        /// How long the subscription takes to answer.
        subscribe_takes: Duration,
        /// How long each wait for a notification takes.
        wait_takes: Duration,
        /// How long focusing a pane takes.
        focus_takes: Duration,
        /// The panes asked to be focused, in order.
        focused: Vec<u64>,
        /// The panes whose screens were asked for, in order.
        fetched: Vec<u64>,
        /// Runs during each wait for a notification, as what the window
        /// does meanwhile on its own thread.
        on_wait: Option<Box<dyn FnMut()>>,
    }

    impl ScriptedNoa {
        /// Pane 2 (a running worker under orchestrator 1) showing `screen`.
        /// The orchestrator has no surface id, so following this Noa looks
        /// for no journal: none of the user's own files is read.
        fn showing(instance: &str, screen: &str, clock: &TestClock) -> Self {
            let orchestrator = panel(serde_json::json!({ "paneId": "1", "label": "release" }));
            ScriptedNoa {
                instance: instance.to_owned(),
                panels: vec![orchestrator, running_agent("2", "1", "a")],
                screens: HashMap::from([(2, screen.to_owned())]),
                waits: 1,
                clock: clock.clone(),
                subscribe_takes: Duration::ZERO,
                wait_takes: Duration::ZERO,
                focus_takes: Duration::ZERO,
                focused: Vec::new(),
                fetched: Vec::new(),
                on_wait: None,
            }
        }
    }

    impl Noa for ScriptedNoa {
        fn instance(&self) -> &str {
            &self.instance
        }

        fn can_focus(&self) -> bool {
            true
        }

        fn list_panels(&mut self) -> Result<Vec<Panel>, ClientError> {
            Ok(self.panels.clone())
        }

        fn subscribe(&mut self) -> Result<(), ClientError> {
            self.clock.advance(self.subscribe_takes);
            Ok(())
        }

        fn next_notification(&mut self, _: Duration) -> Result<Option<Notification>, ClientError> {
            if self.waits == 0 {
                return Err(ClientError::Transport("the script ended".into()));
            }
            self.waits -= 1;
            self.clock.advance(self.wait_takes);
            if let Some(on_wait) = &mut self.on_wait {
                on_wait();
            }
            Ok(None)
        }

        fn focus_pane(&mut self, pane: u64) -> Result<(), ClientError> {
            self.focused.push(pane);
            self.clock.advance(self.focus_takes);
            Ok(())
        }

        fn screen(&mut self, pane: u64) -> Result<String, ClientError> {
            self.fetched.push(pane);
            self.screens.get(&pane).cloned().ok_or(ClientError::Rpc {
                code: -32602,
                message: "no such pane".into(),
            })
        }
    }

    fn tail_of(snapshot: &Snapshot, pane: u64) -> Vec<String> {
        let worker = snapshot.fleet.workers.iter().find(|w| w.pane == pane);
        worker.expect("the worker is there").screen_tail.clone()
    }

    #[test]
    fn another_server_instance_drops_the_screen_tails() {
        let clock = TestClock::new();
        let (mut feed, published, _commands) = held_feed(&clock);
        shows(&mut feed, "a");
        feed.watch(vec![2]);
        // Fetched from server "a" just now. The test's clock stands still,
        // so the tail stays within SCREEN_EVERY through both connections:
        // only a dropped tail is fetched again.
        feed.screens.insert(2, (clock.now(), vec!["$ old".into()]));

        // The same server again (a reconnect): the tail is kept.
        let mut same = ScriptedNoa::showing("a", "$ new", &clock);
        assert!(matches!(feed.follow(&mut same), Err(Stop::Lost(_))));
        let snapshot = published.try_iter().last().expect("a snapshot");
        assert_eq!(tail_of(&snapshot, 2), ["$ old"], "the same server");
        assert_eq!(snapshot.instance.as_deref(), Some("a"));

        // Noa restarted: pane 2 is another pane now, which nobody chose on
        // the new server. Neither the old tail nor the old watch carries
        // over to it: the dashboard asks again for what it shows there.
        let mut other = ScriptedNoa::showing("b", "$ new", &clock);
        assert!(matches!(feed.follow(&mut other), Err(Stop::Lost(_))));
        let snapshot = published.try_iter().last().expect("a snapshot");
        assert!(tail_of(&snapshot, 2).is_empty(), "pane 2 of the old server");
        assert_eq!(snapshot.instance.as_deref(), Some("b"));
        assert_eq!(feed.watched_on.as_deref(), Some("a"), "whose pane 2");
        assert!(other.fetched.is_empty(), "a screen nobody chose on b");
        assert!(feed.tracker.journals.is_empty(), "a journal was looked for");
    }

    #[test]
    fn a_watch_from_before_the_new_server_is_not_followed() {
        let clock = TestClock::new();
        let (mut feed, published, commands) = held_feed(&clock);
        shows(&mut feed, "a");
        feed.watch(vec![2]);
        // While the feed reconnects, the dashboard (still showing server a)
        // asks for pane 2's tail: a's pane 2.
        commands.send(Command::Watch(vec![2])).unwrap();
        let mut other = ScriptedNoa::showing("b", "$ new", &clock);
        assert!(matches!(feed.follow(&mut other), Err(Stop::Lost(_))));
        let first = published.try_iter().next().expect("b's first snapshot");
        assert_eq!(first.instance.as_deref(), Some("b"));
        assert!(tail_of(&first, 2).is_empty());
        assert!(other.fetched.is_empty(), "b's pane 2 under a's watch");

        // Asked for once the dashboard has b's snapshot, it is fetched.
        commands.send(Command::Watch(vec![2])).unwrap();
        assert!(feed.take_commands(&mut other).is_ok());
        assert!(feed.fetch_screens(&mut other).is_ok());
        assert_eq!(other.fetched, [2]);
    }

    #[test]
    fn a_watch_while_down_before_the_new_servers_first_snapshot_is_not_followed() {
        let clock = TestClock::new();
        let (mut feed, published, commands) = held_feed(&clock);
        shows(&mut feed, "a");
        // Server b is taken up, and lost before its first snapshot (its
        // list fails): the dashboard still shows a, and asks for a's pane
        // 2 while the feed waits to reconnect.
        feed.serve("b");
        commands.send(Command::Watch(vec![2])).unwrap();
        assert!(feed.pause(Duration::ZERO));
        let mut other = ScriptedNoa::showing("b", "$ new", &clock);
        assert!(matches!(feed.follow(&mut other), Err(Stop::Lost(_))));
        let first = published.try_iter().next().expect("b's first snapshot");
        assert!(tail_of(&first, 2).is_empty());
        assert!(other.fetched.is_empty(), "b's pane 2 under a's watch");

        commands.send(Command::Watch(vec![2])).unwrap();
        assert!(feed.take_commands(&mut other).is_ok());
        assert!(feed.fetch_screens(&mut other).is_ok());
        assert_eq!(other.fetched, [2]);
    }

    #[test]
    fn a_watch_outlives_a_server_the_dashboard_never_showed() {
        let clock = TestClock::new();
        let (mut feed, published, commands) = held_feed(&clock);
        let mut first = ScriptedNoa::showing("a", "$ old", &clock);
        assert!(matches!(feed.follow(&mut first), Err(Stop::Lost(_))));
        commands.send(Command::Watch(vec![2])).unwrap();
        assert!(feed.take_commands(&mut first).is_ok());

        // The feed follows server b for a while: the dashboard (an IME
        // composing in its filter holds structural snapshots back) never
        // shows b, and asks for nothing.
        let mut other = ScriptedNoa::showing("b", "$ other", &clock);
        assert!(matches!(feed.follow(&mut other), Err(Stop::Lost(_))));
        assert!(other.fetched.is_empty(), "b's pane 2 under a's watch");

        // Back on a, the screen the dashboard still shows has its tail.
        let mut again = ScriptedNoa::showing("a", "$ new", &clock);
        assert!(matches!(feed.follow(&mut again), Err(Stop::Lost(_))));
        let snapshot = published.try_iter().last().expect("a snapshot");
        assert_eq!(snapshot.instance.as_deref(), Some("a"));
        assert_eq!(again.fetched, [2]);
        assert_eq!(tail_of(&snapshot, 2), ["$ new"]);
    }

    /// The labels of `id` and everything under it.
    fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
        let own = tree.get::<Label>(id).map(|l| l.text().to_owned());
        let children = tree.children(id).iter();
        own.into_iter()
            .chain(children.flat_map(|child| labels_under(tree, *child)))
            .collect()
    }

    /// A Noa whose workers wait on a person: 2 for an approval, at the top
    /// of the queue, and 3 (検査) for an answer.
    fn asking(instance: &str, clock: &TestClock) -> ScriptedNoa {
        let mut noa = ScriptedNoa::showing(instance, "$ approve?", clock);
        let waiting = |pane: &str, label: &str, state: &str| {
            panel(serde_json::json!({
                "paneId": pane,
                "parentPaneId": "1",
                "label": label,
                "agent": { "seq": 1, "kind": "claude", "state": state },
            }))
        };
        noa.panels.truncate(1);
        noa.panels.push(waiting("2", "alpha", "permission"));
        noa.panels.push(waiting("3", "検査", "input"));
        noa.screens.insert(3, "$ cargo test".to_owned());
        noa
    }

    /// The review's sequence, with the feed and the dashboard as the app
    /// runs them: the screen shows server a; while an IME composes in its
    /// filter, a snapshot of server b is held back; the feed is back on a
    /// when the composition ends and the filter makes the screen watch 3,
    /// so the feed takes that `Watch` before a's first snapshot, as naming
    /// b's pane 3. The next frame passes over b for a, which looks to the
    /// screen like nothing changed. Taking a's snapshot in tells the feed
    /// again, and on the same connection pane 3's tail is fetched and
    /// shown, with nobody choosing anything again.
    #[test]
    fn a_watch_named_after_a_server_never_shown_heals_on_the_next_snapshot() {
        let clock = TestClock::new();
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let mut feed = Feed::new(
            snapshot_tx,
            notice_tx,
            command_rx,
            Arc::new(|| {}),
            clock.boxed(),
        );
        let channel = Channel {
            snapshots,
            notices,
            commands,
            _heartbeat: mpsc::channel().0,
        };
        let window = Size::new(1440.0, 900.0);
        let screen = Rc::new(RefCell::new((
            Dashboard::live(Grouping::Orchestrator, channel),
            TextStore::new(Default::default()),
        )));
        let frame = {
            let screen = Rc::clone(&screen);
            move || {
                let (dashboard, text) = &mut *screen.borrow_mut();
                dashboard.before_frame(text);
                dashboard.prepare(window, text);
            }
        };
        let input = |event: Event| {
            let (dashboard, text) = &mut *screen.borrow_mut();
            dashboard.dispatch(event, text);
            dashboard.prepare(window, text);
        };
        let shown = || {
            let (dashboard, _) = &*screen.borrow();
            let tree = dashboard.tree();
            labels_under(tree, tree.root().expect("a screen"))
        };
        frame();

        let mut first = asking("a", &clock);
        assert!(matches!(feed.follow(&mut first), Err(Stop::Lost(_))));
        frame();
        assert!(shown().iter().any(|l| l == "検査"), "a's workers are shown");

        let key = Key::Char('/');
        let modifiers = Modifiers::default();
        input(Event::KeyDown { key, modifiers });
        input(Event::Text("/".into()));
        let text = "けんさ".into();
        input(Event::Composition { text, cursor: None });
        let mut other = asking("b", &clock);
        assert!(matches!(feed.follow(&mut other), Err(Stop::Lost(_))));
        frame();
        // The composition ends: the filter leaves 3 the only card, expanded.
        input(Event::Text("検査".into()));

        // Back on a. A wait takes 3 s: the first pass sends a's snapshot,
        // the window takes it in during the second, and the third pass,
        // 6 s after the first snapshot, sends the next.
        let mut again = asking("a", &clock);
        again.waits = 3;
        again.wait_takes = Duration::from_secs(3);
        let frame_once = {
            let mut waits = 0;
            let frame = frame.clone();
            move || {
                waits += 1;
                if waits == 2 {
                    frame();
                }
            }
        };
        again.on_wait = Some(Box::new(frame_once));
        assert!(matches!(feed.follow(&mut again), Err(Stop::Lost(_))));
        assert_eq!(again.fetched, [3], "a's pane 3, which the screen shows");
        assert_eq!(feed.watched_on.as_deref(), Some("a"));

        frame();
        assert!(
            shown().iter().any(|l| l == "$ cargo test"),
            "3's tail on its card: {:?}",
            shown()
        );
    }

    #[test]
    fn a_focus_is_an_answer_from_the_server() {
        let clock = TestClock::new();
        let (mut feed, published, commands) = held_feed(&clock);
        // No pane is watched: the list, then a focus answered 1 s later,
        // then 1 s without a notification before the first snapshot.
        commands.send(Command::Focus(2)).unwrap();
        let mut noa = ScriptedNoa::showing("a", "$ make", &clock);
        noa.focus_takes = Duration::from_secs(1);
        noa.wait_takes = Duration::from_secs(1);
        assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
        assert_eq!(noa.focused, [2], "the focus reached the server");
        let first = published.try_iter().next().expect("a snapshot");
        assert_eq!(first.synced_secs, 1, "the focus answered 1 s ago");
    }

    #[test]
    fn a_screen_tail_is_an_answer_from_the_server() {
        let clock = TestClock::new();
        let (mut feed, published, _commands) = held_feed(&clock);
        let mut noa = ScriptedNoa::showing("a", "$ make", &clock);
        shows(&mut feed, "a");
        feed.tracker.replace(noa.panels.clone(), unix_now());
        feed.watch(vec![2]);
        // The last list or notification came 8 s ago (two slow tails).
        feed.last_contact = clock.now();
        clock.advance(Duration::from_secs(8));
        assert!(feed.fetch_screens(&mut noa).is_ok());
        assert!(feed.send_if_due(&noa, &mut None).is_ok());
        let snapshot = published.try_recv().expect("a snapshot");
        assert_eq!(tail_of(&snapshot, 2), ["$ make"]);
        assert_eq!(snapshot.synced_secs, 0);

        // A pane that closed since the list answers with an error: no tail.
        feed.last_contact = clock.now();
        clock.advance(Duration::from_secs(8));
        noa.screens.clear();
        feed.screens.clear();
        assert!(feed.fetch_screens(&mut noa).is_ok());
        assert!(feed.send_if_due(&noa, &mut None).is_ok());
        let snapshot = published.try_recv().expect("a snapshot");
        assert_eq!(snapshot.synced_secs, 8);
    }

    #[test]
    fn a_subscription_is_an_answer_from_the_server() {
        let clock = TestClock::new();
        let (mut feed, published, _commands) = held_feed(&clock);
        // No pane is watched, so no screen tail answers in between: the
        // list, then the subscription 1.5 s later, then 1 s without a
        // notification before the first snapshot goes out.
        let mut noa = ScriptedNoa::showing("a", "$ make", &clock);
        noa.subscribe_takes = Duration::from_millis(1500);
        noa.wait_takes = Duration::from_secs(1);
        assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
        let first = published.try_iter().next().expect("a snapshot");
        assert_eq!(first.synced_secs, 1, "the subscription answered 1 s ago");
    }

    #[test]
    fn the_heartbeat_wakes_on_its_own_until_the_channel_goes() {
        // The waits below end as soon as what they wait for comes; they are
        // long only so that a loaded machine cannot outlast them.
        const PATIENCE: Duration = Duration::from_secs(60);
        let (woken, wakes) = mpsc::channel();
        let wake = move || {
            let _ = woken.send(());
        };
        // Connecting to Noa hangs until the test lets it go, as a connect
        // or a request near its 5 s timeout does.
        let (release, hang) = mpsc::channel::<()>();
        let connect = move || {
            let _ = hang.recv();
            Err::<ScriptedNoa, String>("released".into())
        };
        let channel = start_with(wake, Duration::from_millis(5), connect);
        for n in 0..3 {
            assert!(wakes.recv_timeout(PATIENCE).is_ok(), "wake {n}");
        }
        assert!(
            channel.snapshots.try_recv().is_err(),
            "the feed sent nothing: the wakes were the heartbeat's"
        );
        drop(channel);
        drop(release);
        // Both threads end, and with them the last `wake`.
        let until = Instant::now() + PATIENCE;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match wakes.recv_timeout(left) {
                Ok(()) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the heartbeat outlives its channel"),
            }
        }
    }
}
