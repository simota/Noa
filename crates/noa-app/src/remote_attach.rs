//! Pure Client Mode connection-state policy.
//!
//! Socket ownership lives at the transport boundary; this module keeps the
//! retry and input-gating rules deterministic so tests never sleep or depend
//! on a network. In particular, there is intentionally no disconnected-input
//! buffer: bytes produced while disconnected are rejected and can never be
//! replayed into a later attach generation.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError};
use noa_core::GridSize;
use noa_grid::Terminal;
use parking_lot::Mutex;
use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use crate::UserEvent;
use crate::session_store::{SessionCardId, SessionDelta, SessionWindowId};
use crate::split_tree::PaneId;

pub(crate) const MAX_RECONNECT_ATTEMPTS: u8 = 10;
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const COMMAND_CHANNEL_CAPACITY: usize = 128;
const WAKE_CHANNEL_CAPACITY: usize = 1;
const COMPLETION_CHANNEL_CAPACITY: usize = 2;
/// Bounds the connection manager's idle raw read so queued input is serviced
/// at the same cadence as the server-side attach reader.
const REMOTE_ATTACH_POLL_TIMEOUT: Duration = Duration::from_millis(1);
const SCROLLBACK_RETRY_DELAY: Duration = Duration::from_secs(1);
const DISCONNECTED_GENERATION: u64 = 0;
const MAX_COMMANDS_PER_POLL: usize = 64;
/// How long an attach may go without any raw output before the heartbeat
/// probes it with a `Ping`. Chosen well above ordinary shell idle gaps so a
/// merely quiet remote pane is never mistaken for a stalled one.
const REMOTE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
/// Grace period after a `Ping` is sent before a still-silent attach is
/// treated as dead and routed into the reconnect path. Kept short relative to
/// `REMOTE_HEARTBEAT_INTERVAL` so a genuinely vanished peer (e.g. a dropped
/// Wi-Fi link, which produces no TCP reset) is discovered promptly instead of
/// leaving the pane parked on `Connected` indefinitely.
const REMOTE_HEARTBEAT_PONG_TIMEOUT: Duration = Duration::from_secs(10);
/// Ceiling on bytes queued for one remote attach generation across the
/// command channel, mirroring `io_thread::input_queue`'s
/// `PTY_INPUT_PENDING_BYTE_CAP`: a message-count-only limit
/// (`COMMAND_CHANNEL_CAPACITY`) is insufficient, since a stalled or slow
/// remote socket could otherwise let repeated huge pastes pin unbounded
/// memory behind that fixed slot count.
const REMOTE_INPUT_PENDING_BYTE_CAP: usize = 8 * 1024 * 1024;
/// Small frames are charged at least this much so container/allocation
/// overhead is bounded along with payload bytes, mirroring
/// `io_thread::input_queue`'s `PTY_INPUT_PENDING_MIN_CHARGE`.
const REMOTE_INPUT_PENDING_MIN_CHARGE: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemotePaneIdentity {
    pub(crate) endpoint: String,
    pub(crate) pane_id: u64,
    pub(crate) cached_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteEndpoint {
    authority: String,
    loopback: bool,
}

impl RemoteEndpoint {
    pub(crate) fn parse(value: &str) -> Result<Self, &'static str> {
        let authority = value.trim();
        if authority.is_empty()
            || authority != value
            || authority
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err("endpoint must be a host:port pair");
        }

        let loopback = if let Ok(address) = authority.parse::<SocketAddr>() {
            if address.port() == 0 {
                return Err("endpoint port must be non-zero");
            }
            address.ip().is_loopback()
        } else {
            let Some((host, port)) = authority.rsplit_once(':') else {
                return Err("endpoint must include a port");
            };
            if !valid_endpoint_host(host)
                || port.parse::<u16>().ok().filter(|port| *port != 0).is_none()
            {
                return Err("endpoint has an invalid host or port");
            }
            host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        };

        Ok(Self {
            authority: authority.to_string(),
            loopback,
        })
    }

    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    pub(crate) fn control_url(&self) -> String {
        format!("ws://{}/", self.authority)
    }

    pub(crate) fn requires_unencrypted_warning(&self) -> bool {
        !self.loopback
    }
}

fn valid_endpoint_host(host: &str) -> bool {
    // Bracketed IPv6 is accepted by the `SocketAddr` branch above. Accepting
    // an unbracketed IPv6 literal here would make `ws://{authority}/`
    // structurally ambiguous with its port.
    if matches!(host.parse::<IpAddr>(), Ok(IpAddr::V4(_))) {
        return true;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let mut all_labels_numeric = true;
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return false;
        }
        all_labels_numeric &= label.bytes().all(|byte| byte.is_ascii_digit());
    }
    // Numeric dotted authorities must be valid IPv4, not DNS-looking fallbacks
    // such as `999.1.1.1` after `SocketAddr` parsing rejects them.
    !all_labels_numeric
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoteAttachState {
    Connected,
    Reconnecting {
        /// One-based attempt that will run after `delay`.
        attempt: u8,
        delay: Duration,
    },
    Detached,
}

impl RemoteAttachState {
    pub(crate) fn connected() -> Self {
        Self::Connected
    }

    pub(crate) fn disconnected() -> Self {
        Self::Reconnecting {
            attempt: 1,
            delay: INITIAL_RECONNECT_DELAY,
        }
    }

    /// Records failure of the currently scheduled attempt.
    pub(crate) fn retry_failed(&mut self) {
        let Self::Reconnecting { attempt, .. } = *self else {
            return;
        };
        if attempt >= MAX_RECONNECT_ATTEMPTS {
            *self = Self::Detached;
            return;
        }
        let next_attempt = attempt + 1;
        *self = Self::Reconnecting {
            attempt: next_attempt,
            delay: reconnect_delay(next_attempt),
        };
    }

    pub(crate) fn retry_succeeded(&mut self) {
        *self = Self::Connected;
    }

    /// Starts a fresh bounded sequence only from the explicit manual state.
    pub(crate) fn manual_retry(&mut self) -> bool {
        if !matches!(self, Self::Detached) {
            return false;
        }
        *self = Self::disconnected();
        true
    }

    /// Remote input is accepted only by the live attach generation. Callers
    /// must reject it in both retry states rather than buffering it.
    pub(crate) fn accepts_input(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

pub(crate) fn tab_title(
    identity: &RemotePaneIdentity,
    state: &RemoteAttachState,
    live_title: &str,
) -> String {
    let fallback = identity
        .cached_title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(&identity.endpoint);
    let base = if matches!(state, RemoteAttachState::Connected) {
        Some(live_title)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(fallback)
    } else {
        fallback
    };
    match state {
        RemoteAttachState::Connected => format!("{base} · Remote"),
        RemoteAttachState::Reconnecting { attempt, .. } => {
            format!("{base} · Reconnecting {attempt}/{MAX_RECONNECT_ATTEMPTS}")
        }
        RemoteAttachState::Detached => format!("{base} · Detached"),
    }
}

fn reconnect_delay(attempt: u8) -> Duration {
    debug_assert!(attempt >= 1);
    let exponent = u32::from(attempt.saturating_sub(1));
    INITIAL_RECONNECT_DELAY
        .checked_mul(2_u32.saturating_pow(exponent))
        .unwrap_or(MAX_RECONNECT_DELAY)
        .min(MAX_RECONNECT_DELAY)
}

/// Bearer token kept deliberately opaque to `Debug` and logging call sites.
#[derive(Clone)]
pub(crate) struct RemoteToken(String);

impl RemoteToken {
    pub(crate) fn new(value: String) -> Result<Self, &'static str> {
        if value.trim().is_empty() {
            return Err("remote token must not be empty");
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

/// Connection inputs shared only with the dedicated workers. Manual retry
/// replaces the whole value so the next attempt cannot retain stale auth.
/// This type intentionally has no `Debug` implementation because it owns the
/// credential used by [`noa_ipc::Client::connect`].
#[derive(Clone)]
pub(crate) struct RemoteConnectionCredentials {
    endpoint: RemoteEndpoint,
    token: RemoteToken,
    pane_id: u64,
}

impl RemoteConnectionCredentials {
    pub(crate) fn new(endpoint: RemoteEndpoint, token: RemoteToken, pane_id: u64) -> Self {
        Self {
            endpoint,
            token,
            pane_id,
        }
    }
}

/// Injectable backoff seam. `true` means the full delay elapsed; `false`
/// means the wake channel fired and the manager must re-check shutdown.
pub(crate) trait Clock: Send + Sync + 'static {
    fn wait(&self, delay: Duration, wake_rx: &Receiver<()>) -> bool;
}

#[derive(Clone, Copy)]
struct SystemClock;

impl Clock for SystemClock {
    fn wait(&self, delay: Duration, wake_rx: &Receiver<()>) -> bool {
        match wake_rx.recv_timeout(delay) {
            Err(RecvTimeoutError::Timeout) => true,
            Ok(()) | Err(RecvTimeoutError::Disconnected) => false,
        }
    }
}

trait ConnectionNotifier: Send + Sync + 'static {
    fn redraw(&self);

    fn publish_overview(&self, _terminal: &Terminal) {}

    fn flush_overview_if_due(&self, _terminal: &Arc<Mutex<Terminal>>) -> bool {
        false
    }

    /// A `BEL` (`0x07`) was drained from the attached pane's raw output.
    /// Mirrors the local pty path's sidebar-bell delta (`io_thread::feed`) so
    /// a remote pane's bell escalates to an attention request / sidebar
    /// unread flag the same way a local one does.
    fn bell(&self) {}
}

#[derive(Default)]
struct RemoteOverviewTiming {
    last_publish: Option<Instant>,
    pending_at: Option<Instant>,
}

#[derive(Clone)]
struct RemoteOverviewPublisher {
    target: crate::io_thread::OverviewPublish,
    timing: Arc<Mutex<RemoteOverviewTiming>>,
}

impl RemoteOverviewPublisher {
    fn new(target: crate::io_thread::OverviewPublish) -> Self {
        Self {
            target,
            timing: Arc::new(Mutex::new(RemoteOverviewTiming::default())),
        }
    }

    fn publish(&self, terminal: &Terminal) {
        let mut timing = self.timing.lock();
        let pending_at = crate::io_thread::publish_overview_snapshot(
            terminal,
            &self.target,
            &mut timing.last_publish,
        );
        timing.pending_at = pending_at;
    }

    fn flush_if_due(&self, terminal: &Arc<Mutex<Terminal>>) -> bool {
        let now = Instant::now();
        {
            let mut timing = self.timing.lock();
            if !self.target.visible.load(Ordering::Relaxed) {
                timing.pending_at = None;
                return false;
            }
            if timing.pending_at.is_none_or(|deadline| now < deadline) {
                return false;
            }
            timing.pending_at = None;
        }

        let terminal = terminal.lock();
        let mut timing = self.timing.lock();
        let pending_at = crate::io_thread::publish_overview_snapshot(
            &terminal,
            &self.target,
            &mut timing.last_publish,
        );
        timing.pending_at = pending_at;
        true
    }
}

#[derive(Clone)]
struct WinitConnectionNotifier {
    proxy: EventLoopProxy<UserEvent>,
    window_id: WindowId,
    pane_id: PaneId,
    overview: RemoteOverviewPublisher,
}

impl ConnectionNotifier for WinitConnectionNotifier {
    fn redraw(&self) {
        let _ = self
            .proxy
            .send_event(UserEvent::Redraw(self.window_id, self.pane_id));
    }

    fn publish_overview(&self, terminal: &Terminal) {
        self.overview.publish(terminal);
    }

    fn flush_overview_if_due(&self, terminal: &Arc<Mutex<Terminal>>) -> bool {
        self.overview.flush_if_due(terminal)
    }

    fn bell(&self) {
        let id = SessionCardId::new(SessionWindowId(u64::from(self.window_id)), self.pane_id);
        let _ = self
            .proxy
            .send_event(UserEvent::SessionDelta(SessionDelta::Bell { id }));
    }
}

struct TransportFailure;

enum ScrollbackFetchFailure {
    Transient,
    ReadScopeDenied,
}

struct EstablishedConnection<C: ControlTransport> {
    control: C,
    attach: C::Attach,
    size: GridSize,
}

/// Lazy scrollback fetched over a separate read-only control connection.
/// The generation-matched portion older than the local terminal is merged
/// into paged history while this snapshot remains available for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteScrollbackSnapshot {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

struct CachedRemoteScrollback {
    generation: u64,
    snapshot: RemoteScrollbackSnapshot,
    merged: bool,
}

trait ConnectionFactory: Clone + Send + 'static {
    type Control: ControlTransport;

    fn connect(
        &mut self,
        control_url: &str,
        token: &str,
        requested_scopes: noa_ipc::ScopeSet,
    ) -> Result<Self::Control, TransportFailure>;
}

trait ControlTransport {
    type Attach: AttachTransport;
    type Reservation;

    fn granted_scopes(&self) -> noa_ipc::ScopeSet;
    fn reserve_attach(&mut self, pane_id: u64) -> Result<Self::Reservation, TransportFailure>;
    fn open_reserved_attach(
        &mut self,
        pane_id: u64,
        reservation: &Self::Reservation,
    ) -> Result<Self::Attach, TransportFailure>;
    fn detach(&mut self, pane_id: u64) -> Result<(), TransportFailure>;
    fn resize_pane(&mut self, pane_id: u64, size: GridSize) -> Result<(), TransportFailure>;
    fn get_scrollback(
        &mut self,
        pane_id: u64,
    ) -> Result<RemoteScrollbackSnapshot, TransportFailure>;
}

trait AttachTransport {
    fn take_seed(&mut self) -> Vec<u8>;
    fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportFailure>;
    fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, TransportFailure>;
    fn set_poll_timeout(&mut self, timeout: Duration) -> Result<(), TransportFailure>;
    fn close(&mut self) -> Result<(), TransportFailure>;
    /// Sends a WebSocket-level `Ping` so an idle attach can be probed for
    /// liveness. Only `run_connected`'s heartbeat calls this.
    fn send_ping(&mut self) -> Result<(), TransportFailure>;
    /// Drains whether a `Pong` arrived since the last call. One-shot, mirrors
    /// [`noa_ipc::AttachClient::take_pong`].
    fn take_pong(&mut self) -> bool;
}

#[derive(Clone, Copy)]
struct IpcConnectionFactory;

impl ConnectionFactory for IpcConnectionFactory {
    type Control = noa_ipc::Client;

    fn connect(
        &mut self,
        control_url: &str,
        token: &str,
        requested_scopes: noa_ipc::ScopeSet,
    ) -> Result<Self::Control, TransportFailure> {
        noa_ipc::Client::connect(control_url, token, requested_scopes).map_err(|_| TransportFailure)
    }
}

impl ControlTransport for noa_ipc::Client {
    type Attach = noa_ipc::AttachClient;
    type Reservation = noa_ipc::AttachResult;

    fn granted_scopes(&self) -> noa_ipc::ScopeSet {
        noa_ipc::Client::granted_scopes(self)
    }

    fn reserve_attach(&mut self, pane_id: u64) -> Result<Self::Reservation, TransportFailure> {
        noa_ipc::Client::reserve_attach(self, pane_id).map_err(|_| TransportFailure)
    }

    fn open_reserved_attach(
        &mut self,
        pane_id: u64,
        reservation: &Self::Reservation,
    ) -> Result<Self::Attach, TransportFailure> {
        noa_ipc::Client::open_reserved_attach(self, pane_id, reservation)
            .map_err(|_| TransportFailure)
    }

    fn detach(&mut self, pane_id: u64) -> Result<(), TransportFailure> {
        noa_ipc::Client::detach(self, pane_id).map_err(|_| TransportFailure)
    }

    fn resize_pane(&mut self, pane_id: u64, size: GridSize) -> Result<(), TransportFailure> {
        noa_ipc::Client::resize_pane(self, pane_id, size.cols, size.rows)
            .map_err(|_| TransportFailure)
    }

    fn get_scrollback(
        &mut self,
        pane_id: u64,
    ) -> Result<RemoteScrollbackSnapshot, TransportFailure> {
        let result =
            noa_ipc::Client::get_text(self, pane_id, noa_ipc::TextSource::Scrollback, None)
                .map_err(|_| TransportFailure)?;
        Ok(RemoteScrollbackSnapshot {
            text: result.text,
            truncated: result.truncated,
        })
    }
}

impl AttachTransport for noa_ipc::AttachClient {
    fn take_seed(&mut self) -> Vec<u8> {
        noa_ipc::AttachClient::take_seed(self)
    }

    fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportFailure> {
        noa_ipc::AttachClient::send_raw(self, bytes).map_err(|_| TransportFailure)
    }

    fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, TransportFailure> {
        noa_ipc::AttachClient::poll_raw(self).map_err(|_| TransportFailure)
    }

    fn set_poll_timeout(&mut self, timeout: Duration) -> Result<(), TransportFailure> {
        noa_ipc::AttachClient::set_poll_timeout(self, timeout).map_err(|_| TransportFailure)
    }

    fn close(&mut self) -> Result<(), TransportFailure> {
        noa_ipc::AttachClient::close(self).map_err(|_| TransportFailure)
    }

    fn send_ping(&mut self) -> Result<(), TransportFailure> {
        noa_ipc::AttachClient::send_ping(self).map_err(|_| TransportFailure)
    }

    fn take_pong(&mut self) -> bool {
        noa_ipc::AttachClient::take_pong(self)
    }
}

/// Input bytes plus a shared byte-budget reservation, mirroring
/// `io_thread::input_queue::QueuedPtyInput`: the reservation follows the
/// bytes through the command channel and is released — by `Drop`, so it
/// covers every discard path (generation switch, shutdown drain, a
/// successful or failed `send_raw`) — the moment the command is consumed or
/// dropped.
struct BudgetedInput {
    bytes: Vec<u8>,
    pending_bytes: Arc<AtomicUsize>,
    charge: usize,
}

impl BudgetedInput {
    fn reserve(bytes: Vec<u8>, pending_bytes: Arc<AtomicUsize>) -> Result<Self, Vec<u8>> {
        let charge = bytes.len().max(REMOTE_INPUT_PENDING_MIN_CHARGE);
        if pending_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(charge)
                    .filter(|next| *next <= REMOTE_INPUT_PENDING_BYTE_CAP)
            })
            .is_err()
        {
            return Err(bytes);
        }
        Ok(Self {
            bytes,
            pending_bytes,
            charge,
        })
    }
}

impl AsRef<[u8]> for BudgetedInput {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for BudgetedInput {
    fn drop(&mut self) {
        self.pending_bytes.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

enum RemoteCommand {
    Input {
        generation: u64,
        size: GridSize,
        bytes: BudgetedInput,
    },
    Resize {
        generation: u64,
        size: GridSize,
    },
    ManualRetry,
    Shutdown,
}

/// Main-thread handle for a remote connection manager. Input is rejected
/// while disconnected; resize always updates the latest desired geometry and
/// is tagged when a live generation exists, so stale queued work can never
/// reach a later socket.
pub(crate) struct RemoteConnectionHandle {
    command_tx: Sender<RemoteCommand>,
    wake_tx: Sender<()>,
    scrollback_wake_tx: Sender<()>,
    credentials: Arc<Mutex<RemoteConnectionCredentials>>,
    desired_size: Arc<Mutex<GridSize>>,
    generation: Arc<AtomicU64>,
    generation_guard: Arc<Mutex<()>>,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<RemoteAttachState>>,
    #[allow(
        dead_code,
        reason = "the Surface scrollback presentation consumes this cache in its integration phase"
    )]
    scrollback: Arc<Mutex<Option<CachedRemoteScrollback>>>,
    pending_input_bytes: Arc<AtomicUsize>,
    done_rx: Receiver<()>,
    joins: Vec<std::thread::JoinHandle<()>>,
}

impl RemoteConnectionHandle {
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

    pub(crate) fn send_input(&self, bytes: Vec<u8>) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let generation = self.generation.load(Ordering::Acquire);
        if generation == DISCONNECTED_GENERATION
            || !self.state.lock().accepts_input()
            || self.shutdown.load(Ordering::Acquire)
        {
            return false;
        }
        let Ok(bytes) = BudgetedInput::reserve(bytes, Arc::clone(&self.pending_input_bytes)) else {
            // The pane's queued-but-unsent bytes already hit the cap: a slow
            // or stalled remote socket combined with repeated huge pastes
            // could otherwise pin unbounded memory. Same remedy as the
            // channel-full case below — invalidate the generation so the
            // manager closes it and all later input is rejected until a
            // fresh attach completes.
            let _guard = self.generation_guard.lock();
            if self.generation.load(Ordering::Acquire) == generation {
                self.generation
                    .store(DISCONNECTED_GENERATION, Ordering::Release);
            }
            return false;
        };
        match self.command_tx.try_send(RemoteCommand::Input {
            generation,
            size: *self.desired_size.lock(),
            bytes,
        }) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                // Once any byte sequence is dropped, continuing the same raw
                // stream would hide corruption from both ends. Invalidate the
                // generation so the manager closes it and all later input is
                // rejected until a fresh attach completes.
                let _guard = self.generation_guard.lock();
                if self.generation.load(Ordering::Acquire) == generation {
                    self.generation
                        .store(DISCONNECTED_GENERATION, Ordering::Release);
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn resize(&self, size: GridSize) -> bool {
        if size.cols == 0 || size.rows == 0 || self.shutdown.load(Ordering::Acquire) {
            return false;
        }
        *self.desired_size.lock() = size;
        let generation = self.generation.load(Ordering::Acquire);
        if generation == DISCONNECTED_GENERATION {
            return true;
        }
        match self
            .command_tx
            .try_send(RemoteCommand::Resize { generation, size })
        {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn manual_retry(&self, credentials: RemoteConnectionCredentials) -> bool {
        if !matches!(self.state(), RemoteAttachState::Detached)
            || self.shutdown.load(Ordering::Acquire)
        {
            return false;
        }
        *self.credentials.lock() = credentials;
        if self
            .command_tx
            .try_send(RemoteCommand::ManualRetry)
            .is_err()
        {
            return false;
        }
        let _ = self.wake_tx.try_send(());
        true
    }

    pub(crate) fn state(&self) -> RemoteAttachState {
        self.state.lock().clone()
    }

    /// Returns the current generation's completed lazy-backfill result.
    /// Incomplete responses and results belonging to a disconnected attach
    /// generation are never published to the Surface.
    #[allow(
        dead_code,
        reason = "the Surface scrollback presentation consumes this API in its integration phase"
    )]
    pub(crate) fn scrollback_snapshot(&self) -> Option<RemoteScrollbackSnapshot> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == DISCONNECTED_GENERATION {
            return None;
        }
        let snapshot = self
            .scrollback
            .lock()
            .as_ref()
            .filter(|cached| cached.generation == generation)
            .map(|cached| cached.snapshot.clone());
        (self.generation.load(Ordering::Acquire) == generation)
            .then_some(snapshot)
            .flatten()
    }

    /// Signal every remote worker immediately, then reap them away from the
    /// winit event-loop thread. Synchronous control/DNS calls honor their own
    /// deadlines but cannot be interrupted by the shutdown command, so even a
    /// bounded join must not run inline while closing a pane or window.
    pub(crate) fn shutdown_and_join(self) {
        self.request_shutdown();
        std::thread::spawn(move || {
            if !self.shutdown_and_join_timeout(Self::JOIN_TIMEOUT) {
                log::warn!(
                    "remote connection workers did not stop within {:?}",
                    Self::JOIN_TIMEOUT
                );
            }
        });
    }

    pub(crate) fn shutdown_and_join_timeout(mut self, timeout: Duration) -> bool {
        self.request_shutdown();
        let started = Instant::now();
        let mut remaining_workers = self.joins.len();
        while remaining_workers > 0 {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return false;
            };
            match self.done_rx.recv_timeout(remaining) {
                Ok(()) => remaining_workers -= 1,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
        // Workers send their completion notification as the final statement
        // in their thread closure. Receiving every notification therefore
        // means they can only be in the tiny interval between that send and
        // the closure returning. Do not sample `is_finished()` just once in
        // that interval: it makes an otherwise successful bounded shutdown
        // fail nondeterministically.
        while self.joins.iter().any(|join| !join.is_finished()) {
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::yield_now();
        }
        self.joins.drain(..).all(|join| join.join().is_ok())
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.command_tx.try_send(RemoteCommand::Shutdown);
        let _ = self.wake_tx.try_send(());
        let _ = self.scrollback_wake_tx.try_send(());
    }
}

impl Drop for RemoteConnectionHandle {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

/// Starts the production sync `noa-ipc` manager. The caller supplies the same
/// shared terminal/state/overview objects stored on its remote surface.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_remote_connection(
    credentials: RemoteConnectionCredentials,
    terminal: Arc<Mutex<Terminal>>,
    state: Arc<Mutex<RemoteAttachState>>,
    overview: crate::io_thread::OverviewPublish,
    proxy: EventLoopProxy<UserEvent>,
    window_id: WindowId,
    pane_id: PaneId,
) -> std::io::Result<RemoteConnectionHandle> {
    spawn_remote_connection_with_clock(
        credentials,
        terminal,
        state,
        overview,
        proxy,
        window_id,
        pane_id,
        SystemClock,
    )
}

/// Clock-injectable production seam used by deterministic reconnect tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_remote_connection_with_clock<C: Clock>(
    credentials: RemoteConnectionCredentials,
    terminal: Arc<Mutex<Terminal>>,
    state: Arc<Mutex<RemoteAttachState>>,
    overview: crate::io_thread::OverviewPublish,
    proxy: EventLoopProxy<UserEvent>,
    window_id: WindowId,
    pane_id: PaneId,
    clock: C,
) -> std::io::Result<RemoteConnectionHandle> {
    spawn_connection_manager(
        credentials,
        terminal,
        state,
        IpcConnectionFactory,
        clock,
        WinitConnectionNotifier {
            proxy,
            window_id,
            pane_id,
            overview: RemoteOverviewPublisher::new(overview),
        },
    )
}

fn spawn_connection_manager<F, C, N>(
    credentials: RemoteConnectionCredentials,
    terminal: Arc<Mutex<Terminal>>,
    state: Arc<Mutex<RemoteAttachState>>,
    factory: F,
    clock: C,
    notifier: N,
) -> std::io::Result<RemoteConnectionHandle>
where
    F: ConnectionFactory,
    C: Clock,
    N: Clone + ConnectionNotifier,
{
    let (command_tx, command_rx) = crossbeam_channel::bounded(COMMAND_CHANNEL_CAPACITY);
    let (wake_tx, wake_rx) = crossbeam_channel::bounded(WAKE_CHANNEL_CAPACITY);
    let (scrollback_wake_tx, scrollback_wake_rx) =
        crossbeam_channel::bounded(WAKE_CHANNEL_CAPACITY);
    let (done_tx, done_rx) = crossbeam_channel::bounded(COMPLETION_CHANNEL_CAPACITY);
    let generation = Arc::new(AtomicU64::new(DISCONNECTED_GENERATION));
    let generation_guard = Arc::new(Mutex::new(()));
    let credentials = Arc::new(Mutex::new(credentials));
    let desired_size = Arc::new(Mutex::new(terminal.lock().size));
    let scrollback_requested_generation = Arc::new(AtomicU64::new(DISCONNECTED_GENERATION));
    let shutdown = Arc::new(AtomicBool::new(false));
    let scrollback = Arc::new(Mutex::new(None));
    let pending_input_bytes = Arc::new(AtomicUsize::new(0));
    let thread_generation = Arc::clone(&generation);
    let thread_shutdown = Arc::clone(&shutdown);
    let handle_state = Arc::clone(&state);
    let scrollback_credentials = Arc::clone(&credentials);
    let scrollback_factory = factory.clone();
    let scrollback_generation = Arc::clone(&generation);
    let scrollback_generation_guard = Arc::clone(&generation_guard);
    let worker_requested_generation = Arc::clone(&scrollback_requested_generation);
    let scrollback_shutdown = Arc::clone(&shutdown);
    let worker_cache = Arc::clone(&scrollback);
    let scrollback_notifier = notifier.clone();
    let scrollback_terminal = Arc::clone(&terminal);
    let scrollback_done_tx = done_tx.clone();
    let scrollback_join = std::thread::Builder::new()
        .name("noa-remote-scrollback".to_string())
        .spawn(move || {
            run_scrollback_worker(
                scrollback_credentials,
                scrollback_factory,
                scrollback_generation,
                scrollback_generation_guard,
                worker_requested_generation,
                scrollback_shutdown,
                scrollback_wake_rx,
                worker_cache,
                scrollback_terminal,
                scrollback_notifier,
            );
            let _ = scrollback_done_tx.try_send(());
        })?;

    let manager_requested_generation = Arc::clone(&scrollback_requested_generation);
    let manager_scrollback_wake_tx = scrollback_wake_tx.clone();
    let manager_done_tx = done_tx.clone();
    let manager_desired_size = Arc::clone(&desired_size);
    let manager_credentials = Arc::clone(&credentials);
    let manager_generation_guard = Arc::clone(&generation_guard);
    let manager_scrollback = Arc::clone(&scrollback);
    let manager_join = match std::thread::Builder::new()
        .name("noa-remote-attach".to_string())
        .spawn(move || {
            run_connection_manager(
                manager_credentials,
                terminal,
                state,
                factory,
                clock,
                notifier,
                command_rx,
                wake_rx,
                manager_requested_generation,
                manager_scrollback_wake_tx,
                thread_generation,
                manager_generation_guard,
                thread_shutdown,
                manager_desired_size,
                manager_scrollback,
            );
            let _ = manager_done_tx.try_send(());
        }) {
        Ok(join) => join,
        Err(error) => {
            shutdown.store(true, Ordering::Release);
            let _ = scrollback_wake_tx.try_send(());
            let _ = scrollback_join.join();
            return Err(error);
        }
    };
    drop(done_tx);

    Ok(RemoteConnectionHandle {
        command_tx,
        wake_tx,
        scrollback_wake_tx,
        credentials,
        desired_size,
        generation,
        generation_guard,
        shutdown,
        state: handle_state,
        scrollback,
        pending_input_bytes,
        done_rx,
        joins: vec![manager_join, scrollback_join],
    })
}

#[allow(clippy::too_many_arguments)]
fn run_connection_manager<F, C, N>(
    credentials: Arc<Mutex<RemoteConnectionCredentials>>,
    terminal: Arc<Mutex<Terminal>>,
    state: Arc<Mutex<RemoteAttachState>>,
    mut factory: F,
    clock: C,
    notifier: N,
    command_rx: Receiver<RemoteCommand>,
    wake_rx: Receiver<()>,
    scrollback_requested_generation: Arc<AtomicU64>,
    scrollback_wake_tx: Sender<()>,
    generation: Arc<AtomicU64>,
    generation_guard: Arc<Mutex<()>>,
    shutdown: Arc<AtomicBool>,
    desired_size: Arc<Mutex<GridSize>>,
    scrollback: Arc<Mutex<Option<CachedRemoteScrollback>>>,
) where
    F: ConnectionFactory,
    C: Clock,
    N: ConnectionNotifier,
{
    let mut stream = noa_vt::Stream::new();
    let mut next_generation = DISCONNECTED_GENERATION;
    let mut scheduled_retry = false;
    publish_state(&state, RemoteAttachState::disconnected(), &notifier);

    loop {
        store_generation(&generation, &generation_guard, DISCONNECTED_GENERATION);
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        if matches!(*state.lock(), RemoteAttachState::Detached) {
            if !wait_for_manual_retry(&state, &command_rx, &shutdown, &notifier) {
                break;
            }
            scheduled_retry = true;
        }

        if scheduled_retry {
            let delay = match *state.lock() {
                RemoteAttachState::Reconnecting { delay, .. } => delay,
                RemoteAttachState::Connected | RemoteAttachState::Detached => continue,
            };
            if discard_inactive_commands(&command_rx, &shutdown) {
                break;
            }
            if !clock.wait(delay, &wake_rx) && shutdown.load(Ordering::Acquire) {
                break;
            }
            if shutdown.load(Ordering::Acquire) {
                break;
            }
        }

        let current_credentials = credentials.lock().clone();
        let connection = establish_connection(
            &mut factory,
            &current_credentials,
            &terminal,
            &desired_size,
            &mut stream,
            &notifier,
        );
        let Ok(EstablishedConnection {
            mut control,
            mut attach,
            size: established_size,
        }) = connection
        else {
            if scheduled_retry {
                retry_failed(&state, &notifier);
            } else {
                publish_state(&state, RemoteAttachState::disconnected(), &notifier);
            }
            scheduled_retry = true;
            continue;
        };

        next_generation = next_nonzero_generation(next_generation);
        store_generation(&generation, &generation_guard, next_generation);
        let latest_size = *desired_size.lock();
        if latest_size != established_size
            && control
                .resize_pane(current_credentials.pane_id, latest_size)
                .is_err()
        {
            store_generation(&generation, &generation_guard, DISCONNECTED_GENERATION);
            close_attached_connection(current_credentials.pane_id, &mut control, &mut attach);
            if scheduled_retry {
                retry_failed(&state, &notifier);
            } else {
                publish_state(&state, RemoteAttachState::disconnected(), &notifier);
            }
            scheduled_retry = true;
            continue;
        }
        publish_state(&state, RemoteAttachState::connected(), &notifier);
        schedule_scrollback_backfill(
            next_generation,
            &scrollback_requested_generation,
            &scrollback_wake_tx,
        );
        let outcome = run_connected(
            current_credentials.pane_id,
            next_generation,
            &mut control,
            &mut attach,
            &terminal,
            &mut stream,
            &notifier,
            &command_rx,
            &generation,
            &generation_guard,
            &shutdown,
            &scrollback,
            &scrollback_requested_generation,
            &scrollback_wake_tx,
            &desired_size,
            latest_size,
        );
        store_generation(&generation, &generation_guard, DISCONNECTED_GENERATION);
        close_attached_connection(current_credentials.pane_id, &mut control, &mut attach);
        if matches!(outcome, ConnectedOutcome::Shutdown) || shutdown.load(Ordering::Acquire) {
            break;
        }
        publish_state(&state, RemoteAttachState::disconnected(), &notifier);
        scheduled_retry = true;
    }
}

fn requested_scopes() -> noa_ipc::ScopeSet {
    let mut scopes = noa_ipc::ScopeSet::empty();
    scopes.insert(noa_ipc::Scope::Read);
    scopes.insert(noa_ipc::Scope::Attach);
    scopes
}

fn scrollback_scopes() -> noa_ipc::ScopeSet {
    let mut scopes = noa_ipc::ScopeSet::empty();
    scopes.insert(noa_ipc::Scope::Read);
    scopes
}

fn schedule_scrollback_backfill(
    generation: u64,
    requested_generation: &AtomicU64,
    wake_tx: &Sender<()>,
) {
    requested_generation.store(generation, Ordering::Release);
    let _ = wake_tx.try_send(());
}

#[allow(clippy::too_many_arguments)]
fn run_scrollback_worker<F, N>(
    credentials: Arc<Mutex<RemoteConnectionCredentials>>,
    mut factory: F,
    generation: Arc<AtomicU64>,
    generation_guard: Arc<Mutex<()>>,
    requested_generation: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    wake_rx: Receiver<()>,
    cache: Arc<Mutex<Option<CachedRemoteScrollback>>>,
    terminal: Arc<Mutex<Terminal>>,
    notifier: N,
) where
    F: ConnectionFactory,
    N: ConnectionNotifier,
{
    let mut read_denied_generation = DISCONNECTED_GENERATION;
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if requested_generation.load(Ordering::Acquire) == DISCONNECTED_GENERATION {
            if wake_rx.recv().is_err() {
                break;
            }
            if shutdown.load(Ordering::Acquire) {
                break;
            }
        }

        let requested = requested_generation.swap(DISCONNECTED_GENERATION, Ordering::AcqRel);
        if requested == DISCONNECTED_GENERATION
            || generation.load(Ordering::Acquire) != requested
            || requested == read_denied_generation
        {
            continue;
        }

        let current_credentials = credentials.lock().clone();
        let snapshot = match fetch_scrollback(&mut factory, &current_credentials) {
            Ok(snapshot) => snapshot,
            Err(ScrollbackFetchFailure::ReadScopeDenied) => {
                read_denied_generation = requested;
                continue;
            }
            Err(ScrollbackFetchFailure::Transient) => {
                let should_retry = {
                    let _guard = generation_guard.lock();
                    if shutdown.load(Ordering::Acquire)
                        || generation.load(Ordering::Acquire) != requested
                    {
                        false
                    } else {
                        requested_generation.store(requested, Ordering::Release);
                        true
                    }
                };
                if should_retry
                    && matches!(
                        wake_rx.recv_timeout(SCROLLBACK_RETRY_DELAY),
                        Err(RecvTimeoutError::Disconnected)
                    )
                {
                    break;
                }
                continue;
            }
        };
        let _guard = generation_guard.lock();
        if shutdown.load(Ordering::Acquire) || generation.load(Ordering::Acquire) != requested {
            continue;
        }
        let merged = merge_scrollback_snapshot(&terminal, &snapshot);
        // A failed merge whose snapshot is already overtaken (see
        // `scrollback_snapshot_overtaken`) can never succeed on a later
        // retry of this same cached snapshot — request a fresh fetch under
        // the same generation now, instead of leaving the pane's scrollback
        // permanently short. This does not touch the read-scope-denied path
        // above: that failure mode is a permission rejection, not a stale
        // snapshot, and must stay un-retried for this generation.
        let overtaken = !merged && scrollback_snapshot_overtaken(&terminal, &snapshot);
        *cache.lock() = Some(CachedRemoteScrollback {
            generation: requested,
            snapshot,
            merged,
        });
        if overtaken {
            // No wake signal needed: `requested_generation` is only ever
            // *read* blockingly (`wake_rx.recv()`) when it reads
            // `DISCONNECTED_GENERATION` at the top of this same loop, which
            // this store preempts before that check runs again next
            // iteration.
            requested_generation.store(requested, Ordering::Release);
        }
        notifier.redraw();
    }
}

fn store_generation(generation: &AtomicU64, guard: &Mutex<()>, value: u64) {
    let _guard = guard.lock();
    generation.store(value, Ordering::Release);
}

fn fetch_scrollback<F: ConnectionFactory>(
    factory: &mut F,
    credentials: &RemoteConnectionCredentials,
) -> Result<RemoteScrollbackSnapshot, ScrollbackFetchFailure> {
    let mut control = factory
        .connect(
            &credentials.endpoint.control_url(),
            credentials.token.expose(),
            scrollback_scopes(),
        )
        .map_err(|_| ScrollbackFetchFailure::Transient)?;
    if !control.granted_scopes().contains(noa_ipc::Scope::Read) {
        return Err(ScrollbackFetchFailure::ReadScopeDenied);
    }
    control
        .get_scrollback(credentials.pane_id)
        .map_err(|_| ScrollbackFetchFailure::Transient)
}

/// The primary-screen text `merge_scrollback_snapshot` compares a fetched
/// snapshot against. Factored out so [`scrollback_snapshot_overtaken`] can
/// apply the exact same blank-viewport reconstruction without duplicating it.
fn remote_local_text_for_merge(terminal: &mut Terminal) -> String {
    terminal.scrollback_text().unwrap_or_else(|| {
        // `scrollback_text` omits a wholly blank viewport, but a remote
        // snapshot with older history serializes those blank live rows as a
        // trailing newline run. Reconstruct that non-empty suffix so the same
        // overlap rule still establishes the cross-WebSocket boundary rather
        // than treating an arbitrary unmatched snapshot as history.
        "\n".repeat(usize::from(terminal.size.rows.saturating_sub(1).max(1)))
    })
}

fn merge_scrollback_snapshot(
    terminal: &Arc<Mutex<Terminal>>,
    snapshot: &RemoteScrollbackSnapshot,
) -> bool {
    let mut terminal = terminal.lock();
    if terminal.active_is_alt {
        // Alternate-screen snapshots never belong in primary scrollback;
        // wait for the generation-matched re-fetch after returning to primary.
        return false;
    }
    let local = remote_local_text_for_merge(&mut terminal);
    let overlap = suffix_prefix_overlap(snapshot.text.as_bytes(), local.as_bytes());
    // A short matching prefix can occur by chance in ordinary terminal text.
    // Treat the boundary as synchronized only when the entire local state is
    // present in the snapshot, or the overlap contains at least one complete
    // row boundary. Ambiguous single-line partial matches remain cached.
    if overlap == 0 || (overlap != local.len() && !local.as_bytes()[..overlap].contains(&b'\n')) {
        return false;
    }
    let unique_end = snapshot.text.len().saturating_sub(overlap);
    // `push_full_row_text` (the source-side text builder) only inserts `\n`
    // between rows when the earlier row is *not* soft-wrapped, and terminal
    // cell text never contains a literal control character (see
    // `Screen::plain_text_rows`'s `!character.is_control()` filter). So the
    // byte immediately before the merge boundary unambiguously tells us
    // whether the last unique row continues into the already-local content:
    // a `\n` means a hard break, anything else means a soft wrap.
    let trailing_wrapped = unique_end > 0 && snapshot.text.as_bytes()[unique_end - 1] != b'\n';
    terminal.prepend_scrollback_text(&snapshot.text[..unique_end], trailing_wrapped);
    true
}

/// Whether a snapshot that just failed to merge can *ever* merge later, or is
/// permanently stuck because raw output already grew the local terminal past
/// everything the snapshot captured.
///
/// `merge_scrollback_snapshot`'s overlap is computed against `local` capped
/// to the snapshot's own byte length (`suffix_prefix_overlap`'s `pattern` is
/// `local[..min(local.len(), remote.len())]`), so once `local.len()` reaches
/// `snapshot.text.len()` that comparison stops changing no matter how much
/// more `local` grows — the result of this exact snapshot is decided for
/// good. A worker that keeps re-checking this same cached (immutable)
/// snapshot after that point (`retry_pending_scrollback_merge`, called on
/// every subsequent raw batch) can therefore never recover: new raw output
/// that raced ahead of the fetch permanently outran it. The only way forward
/// is a fresh fetch, which reflects the pane's current state instead of a
/// snapshot already left behind.
fn scrollback_snapshot_overtaken(
    terminal: &Arc<Mutex<Terminal>>,
    snapshot: &RemoteScrollbackSnapshot,
) -> bool {
    let mut terminal = terminal.lock();
    if terminal.active_is_alt {
        return false;
    }
    remote_local_text_for_merge(&mut terminal).len() >= snapshot.text.len()
}

fn retry_pending_scrollback_merge(
    terminal: &Arc<Mutex<Terminal>>,
    generation: u64,
    cache: &Arc<Mutex<Option<CachedRemoteScrollback>>>,
) {
    let snapshot = cache
        .lock()
        .as_ref()
        .filter(|cached| cached.generation == generation && !cached.merged)
        .map(|cached| cached.snapshot.clone());
    let Some(snapshot) = snapshot else {
        return;
    };
    if !merge_scrollback_snapshot(terminal, &snapshot) {
        return;
    }
    if let Some(cached) = cache
        .lock()
        .as_mut()
        .filter(|cached| cached.generation == generation && cached.snapshot == snapshot)
    {
        cached.merged = true;
    }
}

/// Length of the longest suffix of `remote` equal to a prefix of `local`.
/// Both inputs are UTF-8 text, so a matched suffix starts and ends at valid
/// character boundaries even though the linear-time scan operates on bytes.
fn suffix_prefix_overlap(remote: &[u8], local: &[u8]) -> usize {
    let pattern = &local[..local.len().min(remote.len())];
    if pattern.is_empty() {
        return 0;
    }

    let mut prefix = vec![0; pattern.len()];
    for index in 1..pattern.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }

    let mut matched = 0;
    for &byte in remote {
        while matched > 0 && (matched == pattern.len() || byte != pattern[matched]) {
            matched = prefix[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
        }
    }
    matched
}

fn establish_connection<F, N>(
    factory: &mut F,
    credentials: &RemoteConnectionCredentials,
    terminal: &Arc<Mutex<Terminal>>,
    desired_size: &Arc<Mutex<GridSize>>,
    stream: &mut noa_vt::Stream,
    notifier: &N,
) -> Result<EstablishedConnection<F::Control>, TransportFailure>
where
    F: ConnectionFactory,
    N: ConnectionNotifier,
{
    // A disconnect can leave the parser inside a partial OSC/CSI sequence.
    // Connection generations are independent byte streams, so never let
    // parser residue from the previous server consume the new seed.
    *stream = noa_vt::Stream::new();
    let mut control = factory.connect(
        &credentials.endpoint.control_url(),
        credentials.token.expose(),
        requested_scopes(),
    )?;
    if !control.granted_scopes().contains(noa_ipc::Scope::Attach) {
        return Err(TransportFailure);
    }
    // Claim the single-attach lease before mutating the authoritative remote
    // PTY. A losing client must not resize the pane currently owned by the
    // winning attach generation.
    let reservation = control.reserve_attach(credentials.pane_id)?;
    let initial_size = *desired_size.lock();
    if control
        .resize_pane(credentials.pane_id, initial_size)
        .is_err()
    {
        let _ = control.detach(credentials.pane_id);
        return Err(TransportFailure);
    }
    let mut attach = match control.open_reserved_attach(credentials.pane_id, &reservation) {
        Ok(attach) => attach,
        Err(error) => {
            let _ = control.detach(credentials.pane_id);
            return Err(error);
        }
    };
    if attach.set_poll_timeout(REMOTE_ATTACH_POLL_TIMEOUT).is_err() {
        close_attached_connection(credentials.pane_id, &mut control, &mut attach);
        return Err(TransportFailure);
    }
    let seed = attach.take_seed();
    feed_remote_seed(stream, terminal, &seed, initial_size, notifier);
    Ok(EstablishedConnection {
        control,
        attach,
        size: initial_size,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_connected<C, N>(
    pane_id: u64,
    generation: u64,
    control: &mut C,
    attach: &mut C::Attach,
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    notifier: &N,
    command_rx: &Receiver<RemoteCommand>,
    active_generation: &AtomicU64,
    generation_guard: &Mutex<()>,
    shutdown: &AtomicBool,
    scrollback: &Arc<Mutex<Option<CachedRemoteScrollback>>>,
    scrollback_requested_generation: &AtomicU64,
    scrollback_wake_tx: &Sender<()>,
    desired_size: &Mutex<GridSize>,
    mut applied_size: GridSize,
) -> ConnectedOutcome
where
    C: ControlTransport,
    N: ConnectionNotifier,
{
    // A batch left inside synchronized output (DECSET 2026) with no
    // follow-up bytes must still wake the render side once its held-snapshot
    // reuse window elapses (`app::render::sync_output_snapshot_decision`
    // shares this exact cap) — otherwise a remote pane that goes quiet
    // mid-sync freezes on the stale held frame forever, since nothing else
    // in this loop would ever ask for another redraw. Mirrors the local pty
    // path's `RedrawFloor`-armed trailing deadline (`io_thread::spawn`).
    let mut sync_redraw_deadline: Option<Instant> = None;
    let mut heartbeat = Heartbeat::new(Instant::now());
    loop {
        if shutdown.load(Ordering::Acquire) {
            return ConnectedOutcome::Shutdown;
        }
        if active_generation.load(Ordering::Acquire) != generation {
            return ConnectedOutcome::Disconnected;
        }
        if notifier.flush_overview_if_due(terminal) {
            notifier.redraw();
        }
        if sync_redraw_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            sync_redraw_deadline = None;
            notifier.redraw();
        }
        for _ in 0..MAX_COMMANDS_PER_POLL {
            match command_rx.try_recv() {
                Ok(command) => {
                    if let RemoteCommand::Input {
                        generation: command_generation,
                        size,
                        ..
                    } = &command
                        && *command_generation == generation
                        && *size != applied_size
                    {
                        if control.resize_pane(pane_id, *size).is_err() {
                            return ConnectedOutcome::Disconnected;
                        }
                        applied_size = *size;
                    }
                    match service_connected_command(command, generation, pane_id, control, attach) {
                        CommandOutcome::Continue => {}
                        CommandOutcome::Disconnected => return ConnectedOutcome::Disconnected,
                        CommandOutcome::Shutdown => return ConnectedOutcome::Shutdown,
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return ConnectedOutcome::Shutdown,
            }
        }
        let latest_size = *desired_size.lock();
        if latest_size != applied_size {
            if control.resize_pane(pane_id, latest_size).is_err() {
                return ConnectedOutcome::Disconnected;
            }
            applied_size = latest_size;
        }
        match attach.poll_raw() {
            Ok(Some(bytes)) => {
                heartbeat.record_activity(Instant::now());
                let _guard = generation_guard.lock();
                if active_generation.load(Ordering::Acquire) != generation {
                    return ConnectedOutcome::Disconnected;
                }
                let outcome = feed_remote_bytes(stream, terminal, &bytes, notifier);
                if outcome.left_alternate_screen {
                    schedule_scrollback_backfill(
                        generation,
                        scrollback_requested_generation,
                        scrollback_wake_tx,
                    );
                }
                retry_pending_scrollback_merge(terminal, generation, scrollback);
                sync_redraw_deadline = outcome.synchronized_output.then(|| {
                    Instant::now() + crate::io_thread::SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION
                });
            }
            Ok(None) => {
                let now = Instant::now();
                if attach.take_pong() {
                    heartbeat.record_activity(now);
                } else {
                    match heartbeat.poll(now) {
                        HeartbeatAction::Wait => {}
                        HeartbeatAction::SendPing => {
                            if attach.send_ping().is_err() {
                                return ConnectedOutcome::Disconnected;
                            }
                        }
                        HeartbeatAction::TimedOut => return ConnectedOutcome::Disconnected,
                    }
                }
            }
            Err(_) => return ConnectedOutcome::Disconnected,
        }
    }
}

enum ConnectedOutcome {
    Disconnected,
    Shutdown,
}

/// Idle-attach liveness probe for [`run_connected`]. Pure `Instant` arithmetic
/// so tests drive it with synthetic timestamps instead of real sleeps, the
/// same way `sync_redraw_deadline` above is tested without a real clock.
struct Heartbeat {
    last_activity: Instant,
    ping_sent_at: Option<Instant>,
}

enum HeartbeatAction {
    Wait,
    SendPing,
    TimedOut,
}

impl Heartbeat {
    fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            ping_sent_at: None,
        }
    }

    /// Any raw output or `Pong` counts as activity and cancels an outstanding
    /// probe — the peer is provably alive either way.
    fn record_activity(&mut self, now: Instant) {
        self.last_activity = now;
        self.ping_sent_at = None;
    }

    fn poll(&mut self, now: Instant) -> HeartbeatAction {
        if let Some(sent_at) = self.ping_sent_at {
            if now.saturating_duration_since(sent_at) >= REMOTE_HEARTBEAT_PONG_TIMEOUT {
                return HeartbeatAction::TimedOut;
            }
            return HeartbeatAction::Wait;
        }
        if now.saturating_duration_since(self.last_activity) >= REMOTE_HEARTBEAT_INTERVAL {
            self.ping_sent_at = Some(now);
            return HeartbeatAction::SendPing;
        }
        HeartbeatAction::Wait
    }
}

enum CommandOutcome {
    Continue,
    Disconnected,
    Shutdown,
}

fn service_connected_command<C: ControlTransport>(
    command: RemoteCommand,
    current_generation: u64,
    _pane_id: u64,
    _control: &mut C,
    attach: &mut C::Attach,
) -> CommandOutcome {
    match command {
        RemoteCommand::Input {
            generation, bytes, ..
        } => {
            if generation != current_generation {
                return CommandOutcome::Continue;
            }
            match attach.send_raw(bytes.as_ref()) {
                Ok(()) => CommandOutcome::Continue,
                Err(_) => CommandOutcome::Disconnected,
            }
        }
        // Resize commands wake the connected loop. Inputs carry the geometry
        // they were queued under and act as ordering barriers; the loop also
        // converges to `desired_size` after draining the current command batch.
        RemoteCommand::Resize { generation, size } => {
            let _ = (generation, size);
            CommandOutcome::Continue
        }
        RemoteCommand::ManualRetry => CommandOutcome::Continue,
        RemoteCommand::Shutdown => CommandOutcome::Shutdown,
    }
}

fn feed_remote_seed<N: ConnectionNotifier>(
    stream: &mut noa_vt::Stream,
    terminal: &Arc<Mutex<Terminal>>,
    seed: &[u8],
    seed_size: GridSize,
    notifier: &N,
) {
    let mut terminal = terminal.lock();
    let latest_size = terminal.size;
    if latest_size != seed_size {
        terminal.resize(seed_size);
    }
    stream.feed(seed, &mut *terminal);
    if latest_size != seed_size {
        terminal.resize(latest_size);
    }
    let _ = terminal.take_pending_writes();
    let _ = terminal.take_pending_clipboard_reads();
    let _ = terminal.take_pending_clipboard_writes();
    notifier.publish_overview(&terminal);
    drop(terminal);
    notifier.redraw();
}

/// Outcome of feeding one raw batch from the attached remote pane, reported
/// back to [`run_connected`] so it can request a scrollback backfill and arm
/// the synchronized-output trailing redraw deadline.
struct RemoteFeedOutcome {
    left_alternate_screen: bool,
    /// The terminal's DECSET 2026 state at the end of this batch — mirrors
    /// `feed::TerminalOutput::synchronized_output` on the local pty path.
    synchronized_output: bool,
}

fn feed_remote_bytes<N: ConnectionNotifier>(
    stream: &mut noa_vt::Stream,
    terminal: &Arc<Mutex<Terminal>>,
    bytes: &[u8],
    notifier: &N,
) -> RemoteFeedOutcome {
    let mut terminal = terminal.lock();
    let was_alternate = terminal.active_is_alt;
    stream.feed(bytes, &mut *terminal);
    let left_alternate_screen = was_alternate && !terminal.active_is_alt;
    let synchronized_output = terminal.modes.synchronized_output();
    // Drained unconditionally, same as the local pty path's sidebar-bell
    // extraction (`io_thread::feed::feed_terminal_batch`) — a remote pane's
    // bell must escalate/flag even while the sidebar or window is hidden.
    let bell = terminal.take_pending_bell();
    let _ = terminal.take_pending_writes();
    let _ = terminal.take_pending_clipboard_reads();
    let _ = terminal.take_pending_clipboard_writes();
    notifier.publish_overview(&terminal);
    drop(terminal);
    if bell {
        notifier.bell();
    }
    notifier.redraw();
    RemoteFeedOutcome {
        left_alternate_screen,
        synchronized_output,
    }
}

fn close_attached_connection<C: ControlTransport>(
    pane_id: u64,
    control: &mut C,
    attach: &mut C::Attach,
) {
    let _ = attach.close();
    let _ = control.detach(pane_id);
}

fn retry_failed<N: ConnectionNotifier>(state: &Arc<Mutex<RemoteAttachState>>, notifier: &N) {
    state.lock().retry_failed();
    notifier.redraw();
}

fn publish_state<N: ConnectionNotifier>(
    state: &Arc<Mutex<RemoteAttachState>>,
    next: RemoteAttachState,
    notifier: &N,
) {
    if matches!(next, RemoteAttachState::Connected) {
        state.lock().retry_succeeded();
    } else {
        *state.lock() = next;
    }
    notifier.redraw();
}

fn wait_for_manual_retry<N: ConnectionNotifier>(
    state: &Arc<Mutex<RemoteAttachState>>,
    command_rx: &Receiver<RemoteCommand>,
    shutdown: &AtomicBool,
    notifier: &N,
) -> bool {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return false;
        }
        match command_rx.recv() {
            Ok(RemoteCommand::ManualRetry) => {
                if state.lock().manual_retry() {
                    notifier.redraw();
                    return true;
                }
            }
            Ok(RemoteCommand::Shutdown) | Err(_) => return false,
            Ok(RemoteCommand::Input { .. } | RemoteCommand::Resize { .. }) => {}
        }
    }
}

/// Drops all inactive-generation work before any reconnect wait/attempt. The
/// main handle already rejects new input while generation zero is published;
/// this drain closes the race for commands queued just before disconnect.
fn discard_inactive_commands(command_rx: &Receiver<RemoteCommand>, shutdown: &AtomicBool) -> bool {
    loop {
        match command_rx.try_recv() {
            Ok(RemoteCommand::Shutdown) => return true,
            Ok(
                RemoteCommand::Input { .. }
                | RemoteCommand::Resize { .. }
                | RemoteCommand::ManualRetry,
            ) => {}
            Err(TryRecvError::Empty) => return shutdown.load(Ordering::Acquire),
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

fn next_nonzero_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Wraps `bytes` in a throwaway budget reservation for tests that only
    /// care about the byte content of a `RemoteCommand::Input`, not the
    /// budget bookkeeping itself.
    fn test_budgeted(bytes: &[u8]) -> BudgetedInput {
        BudgetedInput::reserve(bytes.to_vec(), Arc::new(AtomicUsize::new(0)))
            .unwrap_or_else(|_| panic!("test input is far under the byte cap"))
    }

    #[derive(Default)]
    struct FakeAttach {
        seed: Vec<u8>,
        sent: Vec<Vec<u8>>,
        poll_timeout: Option<Duration>,
        raw: VecDeque<Vec<u8>>,
        fail_poll: bool,
        closed: bool,
        events: Option<Arc<Mutex<Vec<&'static str>>>>,
        ping_count: usize,
        fail_ping: bool,
        /// When set, `send_ping` immediately arms a `Pong` for the next
        /// `take_pong` call, simulating a live, responsive peer.
        auto_pong: bool,
        pong_pending: bool,
    }

    impl AttachTransport for FakeAttach {
        fn take_seed(&mut self) -> Vec<u8> {
            if let Some(events) = &self.events {
                events.lock().push("seed_delivered");
            }
            std::mem::take(&mut self.seed)
        }

        fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportFailure> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }

        fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, TransportFailure> {
            if let Some(bytes) = self.raw.pop_front() {
                Ok(Some(bytes))
            } else if self.fail_poll {
                Err(TransportFailure)
            } else {
                Ok(None)
            }
        }

        fn set_poll_timeout(&mut self, timeout: Duration) -> Result<(), TransportFailure> {
            self.poll_timeout = Some(timeout);
            Ok(())
        }

        fn close(&mut self) -> Result<(), TransportFailure> {
            self.closed = true;
            Ok(())
        }

        fn send_ping(&mut self) -> Result<(), TransportFailure> {
            self.ping_count += 1;
            if self.fail_ping {
                return Err(TransportFailure);
            }
            if self.auto_pong {
                self.pong_pending = true;
            }
            Ok(())
        }

        fn take_pong(&mut self) -> bool {
            std::mem::take(&mut self.pong_pending)
        }
    }

    struct FakeControl {
        attach: Option<FakeAttach>,
        granted_scopes: noa_ipc::ScopeSet,
        resizes: Vec<(u64, GridSize)>,
        detached: Vec<u64>,
        scrollback: Option<FakeScrollback>,
    }

    struct FakeScrollback {
        events: Arc<Mutex<Vec<&'static str>>>,
        started_tx: Sender<()>,
        release_rx: Receiver<()>,
        snapshot: RemoteScrollbackSnapshot,
    }

    impl Default for FakeControl {
        fn default() -> Self {
            Self {
                attach: Some(FakeAttach::default()),
                granted_scopes: requested_scopes(),
                resizes: Vec::new(),
                detached: Vec::new(),
                scrollback: None,
            }
        }
    }

    impl ControlTransport for FakeControl {
        type Attach = FakeAttach;
        type Reservation = ();

        fn granted_scopes(&self) -> noa_ipc::ScopeSet {
            self.granted_scopes
        }

        fn reserve_attach(&mut self, _pane_id: u64) -> Result<Self::Reservation, TransportFailure> {
            Ok(())
        }

        fn open_reserved_attach(
            &mut self,
            _pane_id: u64,
            _reservation: &Self::Reservation,
        ) -> Result<Self::Attach, TransportFailure> {
            self.attach.take().ok_or(TransportFailure)
        }

        fn detach(&mut self, pane_id: u64) -> Result<(), TransportFailure> {
            self.detached.push(pane_id);
            Ok(())
        }

        fn resize_pane(&mut self, pane_id: u64, size: GridSize) -> Result<(), TransportFailure> {
            self.resizes.push((pane_id, size));
            Ok(())
        }

        fn get_scrollback(
            &mut self,
            pane_id: u64,
        ) -> Result<RemoteScrollbackSnapshot, TransportFailure> {
            if pane_id != 7 {
                return Err(TransportFailure);
            }
            let Some(scrollback) = self.scrollback.take() else {
                return Err(TransportFailure);
            };
            scrollback.events.lock().push("get_text_started");
            let _ = scrollback.started_tx.try_send(());
            if scrollback.release_rx.recv().is_err() {
                return Err(TransportFailure);
            }
            scrollback.events.lock().push("get_text_completed");
            Ok(scrollback.snapshot)
        }
    }

    #[derive(Clone)]
    struct NeverConnectFactory {
        attempts: Arc<AtomicUsize>,
    }

    impl ConnectionFactory for NeverConnectFactory {
        type Control = FakeControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            _requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            self.attempts.fetch_add(1, Ordering::AcqRel);
            Err(TransportFailure)
        }
    }

    #[derive(Clone)]
    struct CredentialRecordingFactory {
        tokens: Arc<Mutex<Vec<String>>>,
    }

    impl ConnectionFactory for CredentialRecordingFactory {
        type Control = FakeControl;

        fn connect(
            &mut self,
            _control_url: &str,
            token: &str,
            _requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            self.tokens.lock().push(token.to_string());
            Err(TransportFailure)
        }
    }

    #[derive(Clone)]
    struct BlockingSeedFactory {
        resizes: Arc<Mutex<Vec<GridSize>>>,
        seed_started_tx: Sender<()>,
        seed_release_rx: Receiver<()>,
        seed: Vec<u8>,
        fail_followup_resize: bool,
    }

    struct BlockingSeedControl {
        granted_scopes: noa_ipc::ScopeSet,
        resizes: Arc<Mutex<Vec<GridSize>>>,
        attach: Option<BlockingSeedAttach>,
        fail_followup_resize: bool,
        resize_calls: usize,
    }

    struct BlockingSeedAttach {
        seed_started_tx: Sender<()>,
        seed_release_rx: Receiver<()>,
        seed: Vec<u8>,
    }

    impl ConnectionFactory for BlockingSeedFactory {
        type Control = BlockingSeedControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            let attach =
                requested_scopes
                    .contains(noa_ipc::Scope::Attach)
                    .then(|| BlockingSeedAttach {
                        seed_started_tx: self.seed_started_tx.clone(),
                        seed_release_rx: self.seed_release_rx.clone(),
                        seed: self.seed.clone(),
                    });
            Ok(BlockingSeedControl {
                granted_scopes: requested_scopes,
                resizes: Arc::clone(&self.resizes),
                attach,
                fail_followup_resize: self.fail_followup_resize,
                resize_calls: 0,
            })
        }
    }

    impl ControlTransport for BlockingSeedControl {
        type Attach = BlockingSeedAttach;
        type Reservation = ();

        fn granted_scopes(&self) -> noa_ipc::ScopeSet {
            self.granted_scopes
        }

        fn reserve_attach(&mut self, _pane_id: u64) -> Result<Self::Reservation, TransportFailure> {
            Ok(())
        }

        fn open_reserved_attach(
            &mut self,
            _pane_id: u64,
            _reservation: &Self::Reservation,
        ) -> Result<Self::Attach, TransportFailure> {
            self.attach.take().ok_or(TransportFailure)
        }

        fn detach(&mut self, _pane_id: u64) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn resize_pane(&mut self, _pane_id: u64, size: GridSize) -> Result<(), TransportFailure> {
            self.resizes.lock().push(size);
            self.resize_calls += 1;
            if self.fail_followup_resize && self.resize_calls > 1 {
                Err(TransportFailure)
            } else {
                Ok(())
            }
        }

        fn get_scrollback(
            &mut self,
            _pane_id: u64,
        ) -> Result<RemoteScrollbackSnapshot, TransportFailure> {
            Err(TransportFailure)
        }
    }

    impl AttachTransport for BlockingSeedAttach {
        fn take_seed(&mut self) -> Vec<u8> {
            let _ = self.seed_started_tx.try_send(());
            let _ = self.seed_release_rx.recv();
            std::mem::take(&mut self.seed)
        }

        fn send_raw(&mut self, _bytes: &[u8]) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, TransportFailure> {
            Ok(None)
        }

        fn set_poll_timeout(&mut self, _timeout: Duration) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn send_ping(&mut self) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn take_pong(&mut self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct ConnectedRecordingFactory {
        events: Arc<Mutex<Vec<&'static str>>>,
        reserve_fails: bool,
    }

    struct ConnectedRecordingControl {
        events: Arc<Mutex<Vec<&'static str>>>,
        granted_scopes: noa_ipc::ScopeSet,
        reserve_fails: bool,
    }

    struct ConnectedRecordingAttach {
        events: Arc<Mutex<Vec<&'static str>>>,
        disconnect_on_poll: bool,
    }

    impl ConnectionFactory for ConnectedRecordingFactory {
        type Control = ConnectedRecordingControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            if requested_scopes.contains(noa_ipc::Scope::Attach) {
                self.events.lock().push("control_connect");
            }
            Ok(ConnectedRecordingControl {
                events: Arc::clone(&self.events),
                granted_scopes: requested_scopes,
                reserve_fails: self.reserve_fails,
            })
        }
    }

    impl ControlTransport for ConnectedRecordingControl {
        type Attach = ConnectedRecordingAttach;
        type Reservation = ();

        fn granted_scopes(&self) -> noa_ipc::ScopeSet {
            self.granted_scopes
        }

        fn reserve_attach(&mut self, _pane_id: u64) -> Result<Self::Reservation, TransportFailure> {
            self.events.lock().push("reserve");
            if self.reserve_fails {
                Err(TransportFailure)
            } else {
                Ok(())
            }
        }

        fn open_reserved_attach(
            &mut self,
            _pane_id: u64,
            _reservation: &Self::Reservation,
        ) -> Result<Self::Attach, TransportFailure> {
            self.events.lock().push("raw_open");
            Ok(ConnectedRecordingAttach {
                events: Arc::clone(&self.events),
                disconnect_on_poll: false,
            })
        }

        fn detach(&mut self, _pane_id: u64) -> Result<(), TransportFailure> {
            self.events.lock().push("detach");
            Ok(())
        }

        fn resize_pane(&mut self, _pane_id: u64, _size: GridSize) -> Result<(), TransportFailure> {
            self.events.lock().push("resize");
            Ok(())
        }

        fn get_scrollback(
            &mut self,
            _pane_id: u64,
        ) -> Result<RemoteScrollbackSnapshot, TransportFailure> {
            Err(TransportFailure)
        }
    }

    impl AttachTransport for ConnectedRecordingAttach {
        fn take_seed(&mut self) -> Vec<u8> {
            self.events.lock().push("seed");
            Vec::new()
        }

        fn send_raw(&mut self, _bytes: &[u8]) -> Result<(), TransportFailure> {
            self.events.lock().push("input");
            Ok(())
        }

        fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, TransportFailure> {
            if self.disconnect_on_poll {
                Err(TransportFailure)
            } else {
                Ok(None)
            }
        }

        fn set_poll_timeout(&mut self, _timeout: Duration) -> Result<(), TransportFailure> {
            self.events.lock().push("poll_timeout");
            Ok(())
        }

        fn close(&mut self) -> Result<(), TransportFailure> {
            self.events.lock().push("raw_close");
            Ok(())
        }

        fn send_ping(&mut self) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn take_pong(&mut self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct LazyBackfillFactory {
        connects: Arc<AtomicUsize>,
        scopes: Arc<Mutex<Vec<noa_ipc::ScopeSet>>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        started_tx: Sender<()>,
        release_rx: Receiver<()>,
        fail_first_scrollback: bool,
    }

    impl ConnectionFactory for LazyBackfillFactory {
        type Control = FakeControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            self.scopes.lock().push(requested_scopes);
            let connection = self.connects.fetch_add(1, Ordering::AcqRel);
            if connection == 0 {
                return Ok(FakeControl {
                    attach: Some(FakeAttach {
                        seed: b"seed".to_vec(),
                        events: Some(Arc::clone(&self.events)),
                        ..FakeAttach::default()
                    }),
                    ..FakeControl::default()
                });
            }
            if self.fail_first_scrollback && connection == 1 {
                return Err(TransportFailure);
            }

            Ok(FakeControl {
                attach: None,
                granted_scopes: scrollback_scopes(),
                resizes: Vec::new(),
                detached: Vec::new(),
                scrollback: Some(FakeScrollback {
                    events: Arc::clone(&self.events),
                    started_tx: self.started_tx.clone(),
                    release_rx: self.release_rx.clone(),
                    snapshot: RemoteScrollbackSnapshot {
                        text: format!("older output\nseed{}", "\n".repeat(23)),
                        truncated: false,
                    },
                }),
            })
        }
    }

    #[derive(Clone)]
    struct ReadDeniedBackfillFactory {
        connects: Arc<AtomicUsize>,
    }

    impl ConnectionFactory for ReadDeniedBackfillFactory {
        type Control = FakeControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            _requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            self.connects.fetch_add(1, Ordering::AcqRel);
            Ok(FakeControl {
                attach: None,
                granted_scopes: noa_ipc::ScopeSet::empty(),
                resizes: Vec::new(),
                detached: Vec::new(),
                scrollback: None,
            })
        }
    }

    #[derive(Clone)]
    struct RecordingClock {
        waits: Arc<Mutex<Vec<Duration>>>,
    }

    impl Clock for RecordingClock {
        fn wait(&self, delay: Duration, _wake_rx: &Receiver<()>) -> bool {
            self.waits.lock().push(delay);
            true
        }
    }

    #[derive(Clone, Default)]
    struct FakeNotifier {
        redraws: Arc<AtomicUsize>,
        overview: Option<RemoteOverviewPublisher>,
        redraws_with_snapshot: Arc<AtomicUsize>,
        bells: Arc<AtomicUsize>,
    }

    impl ConnectionNotifier for FakeNotifier {
        fn redraw(&self) {
            self.redraws.fetch_add(1, Ordering::AcqRel);
            if self
                .overview
                .as_ref()
                .is_some_and(|overview| overview.target.slot.lock().is_some())
            {
                self.redraws_with_snapshot.fetch_add(1, Ordering::AcqRel);
            }
        }

        fn publish_overview(&self, terminal: &Terminal) {
            if let Some(overview) = &self.overview {
                overview.publish(terminal);
            }
        }

        fn flush_overview_if_due(&self, terminal: &Arc<Mutex<Terminal>>) -> bool {
            self.overview
                .as_ref()
                .is_some_and(|overview| overview.flush_if_due(terminal))
        }

        fn bell(&self) {
            self.bells.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn credentials() -> RemoteConnectionCredentials {
        credentials_with_token("test-token")
    }

    fn credentials_with_token(token: &str) -> RemoteConnectionCredentials {
        RemoteConnectionCredentials::new(
            RemoteEndpoint::parse("localhost:61771").unwrap(),
            RemoteToken::new(token.to_string()).unwrap(),
            7,
        )
    }

    fn terminal_with_size(size: GridSize) -> Arc<Mutex<Terminal>> {
        let mut terminal = Terminal::new(size);
        terminal.set_reply_writes_enabled(false);
        Arc::new(Mutex::new(terminal))
    }

    fn terminal() -> Arc<Mutex<Terminal>> {
        terminal_with_size(GridSize::new(80, 24))
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !predicate() {
            assert!(Instant::now() < deadline, "condition did not become true");
            std::thread::yield_now();
        }
    }

    #[test]
    fn reconnect_policy_is_bounded_exponential_and_caps_at_thirty_seconds() {
        let mut state = RemoteAttachState::disconnected();
        let mut observed = Vec::new();
        loop {
            match state {
                RemoteAttachState::Reconnecting { attempt, delay } => {
                    observed.push((attempt, delay));
                    state.retry_failed();
                }
                RemoteAttachState::Detached => break,
                RemoteAttachState::Connected => panic!("failure path became connected"),
            }
        }

        assert_eq!(observed.len(), usize::from(MAX_RECONNECT_ATTEMPTS));
        assert_eq!(
            observed,
            [1, 2, 4, 8, 16, 30, 30, 30, 30, 30]
                .into_iter()
                .enumerate()
                .map(|(index, seconds)| ((index + 1) as u8, Duration::from_secs(seconds)))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn input_is_never_accepted_or_queued_while_not_connected() {
        let mut state = RemoteAttachState::disconnected();
        assert!(!state.accepts_input());
        for _ in 0..MAX_RECONNECT_ATTEMPTS {
            state.retry_failed();
            assert!(!state.accepts_input());
        }
        assert_eq!(state, RemoteAttachState::Detached);
        assert!(state.manual_retry());
        assert!(!state.accepts_input());
        state.retry_succeeded();
        assert!(state.accepts_input());
    }

    #[test]
    fn manual_retry_is_available_only_after_auto_retry_exhaustion() {
        let mut connected = RemoteAttachState::connected();
        assert!(!connected.manual_retry());

        let mut reconnecting = RemoteAttachState::disconnected();
        assert!(!reconnecting.manual_retry());

        for _ in 0..MAX_RECONNECT_ATTEMPTS {
            reconnecting.retry_failed();
        }
        assert!(reconnecting.manual_retry());
        assert_eq!(reconnecting, RemoteAttachState::disconnected());
    }

    #[test]
    fn tab_title_always_names_the_remote_state_in_text() {
        let identity = RemotePaneIdentity {
            endpoint: "server.local:61771".to_string(),
            pane_id: 7,
            cached_title: Some("editor".to_string()),
        };
        assert_eq!(
            tab_title(&identity, &RemoteAttachState::Connected, "vim"),
            "vim · Remote"
        );
        assert_eq!(
            tab_title(&identity, &RemoteAttachState::Connected, ""),
            "editor · Remote"
        );
        assert_eq!(
            tab_title(
                &identity,
                &RemoteAttachState::Reconnecting {
                    attempt: 3,
                    delay: Duration::from_secs(4),
                },
                "stale-vim",
            ),
            "editor · Reconnecting 3/10"
        );
        assert_eq!(
            tab_title(&identity, &RemoteAttachState::Detached, "stale-vim"),
            "editor · Detached"
        );

        let uncached = RemotePaneIdentity {
            cached_title: None,
            ..identity
        };
        assert_eq!(
            tab_title(&uncached, &RemoteAttachState::Connected, "shell"),
            "shell · Remote"
        );
        assert_eq!(
            tab_title(&uncached, &RemoteAttachState::Connected, ""),
            "server.local:61771 · Remote"
        );
    }

    #[test]
    fn endpoint_parser_is_conservative_before_any_socket_is_opened() {
        for endpoint in [
            "localhost:61771",
            "localhost.:61771",
            "127.0.0.2:61771",
            "[::1]:61771",
        ] {
            let endpoint = RemoteEndpoint::parse(endpoint).expect("loopback endpoint");
            assert!(!endpoint.requires_unencrypted_warning());
            assert_eq!(
                endpoint.control_url(),
                format!("ws://{}/", endpoint.authority())
            );
        }
        for authority in [
            "server.local:61771",
            "xn--bcher-kva.example:443",
            "192.168.1.10:61771",
            "[::2]:61771",
        ] {
            let endpoint = RemoteEndpoint::parse(authority).expect("remote endpoint");
            assert!(endpoint.requires_unencrypted_warning());
            assert_eq!(endpoint.authority(), authority);
            assert_eq!(endpoint.control_url(), format!("ws://{authority}/"));
        }
        for invalid in [
            "",
            ":61771",
            "server.local",
            "server.local:0",
            " server.local:61771",
            "server.local:61771 ",
            "server local:61771",
            "server\n.local:61771",
            "server\0.local:61771",
            "user@server.local:61771",
            "server.local:61771?query",
            "server.local:61771#fragment",
            "server\\name:61771",
            "ws://server:61771",
            "server..local:61771",
            "-server.local:61771",
            "server-.local:61771",
            "999.1.1.1:61771",
            "::1:61771",
        ] {
            assert!(RemoteEndpoint::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn manager_uses_injected_bounded_backoff_before_detaching() {
        let waits = Arc::new(Mutex::new(Vec::new()));
        let attempts = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials(),
            terminal(),
            Arc::clone(&state),
            NeverConnectFactory {
                attempts: Arc::clone(&attempts),
            },
            RecordingClock {
                waits: Arc::clone(&waits),
            },
            FakeNotifier::default(),
        )
        .unwrap();

        wait_until(|| {
            attempts.load(Ordering::Acquire) == 11
                && matches!(*state.lock(), RemoteAttachState::Detached)
        });

        assert_eq!(attempts.load(Ordering::Acquire), 11);
        assert_eq!(
            *waits.lock(),
            [1, 2, 4, 8, 16, 30, 30, 30, 30, 30]
                .map(Duration::from_secs)
                .to_vec()
        );
        assert_eq!(handle.state(), RemoteAttachState::Detached);
        assert!(!handle.send_input(b"discarded".to_vec()));
        assert!(
            handle.resize(GridSize::new(100, 30)),
            "the latest desired size must survive a disconnected generation"
        );

        assert!(handle.manual_retry(credentials()));
        wait_until(|| {
            attempts.load(Ordering::Acquire) == 21
                && matches!(*state.lock(), RemoteAttachState::Detached)
        });
        assert_eq!(waits.lock().len(), 20);
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn resize_during_seed_application_is_reapplied_after_connection_establishes() {
        let initial_size = GridSize::new(4, 3);
        let latest_size = GridSize::new(3, 3);
        let mut source = Terminal::new(initial_size);
        let mut source_stream = noa_vt::Stream::new();
        source_stream.feed(b"abcdef", &mut source);
        let seed = source.synthetic_seed();
        source.resize(latest_size);
        let expected_text = source.scrollback_text();
        let resizes = Arc::new(Mutex::new(Vec::new()));
        let (seed_started_tx, seed_started_rx) = crossbeam_channel::bounded(1);
        let (seed_release_tx, seed_release_rx) = crossbeam_channel::bounded(1);
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let remote_terminal = terminal_with_size(initial_size);
        let handle = spawn_connection_manager(
            credentials(),
            Arc::clone(&remote_terminal),
            Arc::clone(&state),
            BlockingSeedFactory {
                resizes: Arc::clone(&resizes),
                seed_started_tx,
                seed_release_rx,
                seed,
                fail_followup_resize: false,
            },
            SystemClock,
            FakeNotifier::default(),
        )
        .unwrap();

        seed_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("seed application did not start");
        remote_terminal.lock().resize(latest_size);
        assert!(handle.resize(latest_size));
        seed_release_tx.send(()).unwrap();
        wait_until(|| matches!(*state.lock(), RemoteAttachState::Connected));
        wait_until(|| resizes.lock().len() >= 2);
        assert_eq!(*resizes.lock(), [initial_size, latest_size]);
        let mut remote_terminal = remote_terminal.lock();
        assert_eq!(remote_terminal.size, latest_size);
        assert_eq!(remote_terminal.scrollback_text(), expected_text);
        drop(remote_terminal);
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn resize_failure_after_seed_advances_the_current_retry_attempt() {
        let resizes = Arc::new(Mutex::new(Vec::new()));
        let (seed_started_tx, seed_started_rx) = crossbeam_channel::bounded(1);
        let (seed_release_tx, seed_release_rx) = crossbeam_channel::bounded(1);
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials(),
            terminal(),
            Arc::clone(&state),
            BlockingSeedFactory {
                resizes,
                seed_started_tx,
                seed_release_rx,
                seed: Vec::new(),
                fail_followup_resize: true,
            },
            RecordingClock {
                waits: Arc::new(Mutex::new(Vec::new())),
            },
            FakeNotifier::default(),
        )
        .unwrap();

        for size in [GridSize::new(100, 30), GridSize::new(101, 31)] {
            seed_started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("seed application did not start");
            assert!(handle.resize(size));
            seed_release_tx.send(()).unwrap();
        }
        seed_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the second retry did not start");

        assert_eq!(
            *state.lock(),
            RemoteAttachState::Reconnecting {
                attempt: 2,
                delay: Duration::from_secs(2),
            }
        );

        handle.request_shutdown();
        seed_release_tx.send(()).unwrap();
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn manual_retry_replaces_credentials_before_the_next_attempt() {
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials_with_token("old-token"),
            terminal(),
            Arc::clone(&state),
            CredentialRecordingFactory {
                tokens: Arc::clone(&tokens),
            },
            RecordingClock {
                waits: Arc::new(Mutex::new(Vec::new())),
            },
            FakeNotifier::default(),
        )
        .unwrap();

        wait_until(|| {
            tokens.lock().len() == 11 && matches!(*state.lock(), RemoteAttachState::Detached)
        });
        assert!(handle.manual_retry(credentials_with_token("new-token")));
        wait_until(|| {
            tokens.lock().len() == 21 && matches!(*state.lock(), RemoteAttachState::Detached)
        });
        let observed = tokens.lock();
        assert!(observed[..11].iter().all(|token| token == "old-token"));
        assert!(observed[11..].iter().all(|token| token == "new-token"));
        drop(observed);
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn scrollback_overlap_keeps_only_history_older_than_the_local_terminal() {
        assert_eq!(
            suffix_prefix_overlap(b"older\nshared", b"shared\nlater"),
            b"shared".len()
        );
        assert_eq!(
            suffix_prefix_overlap("古い\n共有".as_bytes(), "共有\n新しい".as_bytes()),
            "共有".len()
        );
        assert_eq!(suffix_prefix_overlap(b"same", b"same"), 4);
        assert_eq!(suffix_prefix_overlap(b"remote", b"local"), 0);
    }

    #[test]
    fn scrollback_without_a_raw_stream_overlap_is_not_merged() {
        let remote_terminal = terminal();
        let snapshot = RemoteScrollbackSnapshot {
            text: "future output not present locally".to_string(),
            truncated: false,
        };

        assert!(!merge_scrollback_snapshot(&remote_terminal, &snapshot));

        assert_eq!(remote_terminal.lock().scrollback_len(), 0);
    }

    #[test]
    fn scrollback_snapshot_merges_when_the_primary_local_state_is_empty() {
        let remote_terminal = terminal();
        let rows = usize::from(remote_terminal.lock().size.rows);
        let snapshot = RemoteScrollbackSnapshot {
            text: format!("older output{}", "\n".repeat(rows)),
            truncated: false,
        };

        assert!(merge_scrollback_snapshot(&remote_terminal, &snapshot));

        let mut terminal = remote_terminal.lock();
        assert!(terminal.scrollback_len() > 0);
        assert!(
            terminal
                .scrollback_text()
                .is_some_and(|text| text.starts_with("older output"))
        );
    }

    #[test]
    fn empty_alternate_screen_is_not_used_as_a_primary_scrollback_boundary() {
        let remote_terminal = terminal();
        noa_vt::Stream::new().feed(b"\x1b[?1049h", &mut *remote_terminal.lock());
        let snapshot = RemoteScrollbackSnapshot {
            text: "primary history".to_string(),
            truncated: false,
        };

        assert!(!merge_scrollback_snapshot(&remote_terminal, &snapshot));
        assert_eq!(remote_terminal.lock().scrollback_len(), 0);
    }

    #[test]
    fn scrollback_merge_retries_after_raw_output_reaches_the_snapshot_boundary() {
        let mut one_line_terminal = Terminal::new(GridSize::new(80, 1));
        one_line_terminal.set_reply_writes_enabled(false);
        let remote_terminal = Arc::new(Mutex::new(one_line_terminal));
        let snapshot = RemoteScrollbackSnapshot {
            text: "older output\nseedfuture".to_string(),
            truncated: false,
        };
        let cache = Arc::new(Mutex::new(Some(CachedRemoteScrollback {
            generation: 3,
            snapshot,
            merged: false,
        })));
        let notifier = FakeNotifier::default();
        let mut stream = noa_vt::Stream::new();

        feed_remote_bytes(&mut stream, &remote_terminal, b"seed", &notifier);
        retry_pending_scrollback_merge(&remote_terminal, 3, &cache);
        assert_eq!(remote_terminal.lock().scrollback_len(), 0);

        feed_remote_bytes(&mut stream, &remote_terminal, b"future", &notifier);
        let local = remote_terminal.lock().scrollback_text().unwrap();
        assert_eq!(local, "seedfuture");
        assert_eq!(
            suffix_prefix_overlap(b"older output\nseedfuture", local.as_bytes()),
            local.len()
        );
        retry_pending_scrollback_merge(&remote_terminal, 3, &cache);

        let mut terminal = remote_terminal.lock();
        assert!(cache.lock().as_ref().is_some_and(|cached| cached.merged));
        assert!(
            terminal
                .scrollback_text()
                .is_some_and(|text| text.starts_with("older output\nseedfuture"))
        );
    }

    #[test]
    fn scrollback_snapshot_overtaken_by_local_growth_can_never_merge() {
        // Pure counterpart of the integration test below: once `local` has
        // grown to at least the snapshot's own length, `suffix_prefix_overlap`
        // compares against a `local` prefix capped at that same fixed length
        // forever, so the merge/no-merge outcome for this exact snapshot is
        // already decided and cannot change.
        let mut one_line_terminal = Terminal::new(GridSize::new(80, 1));
        one_line_terminal.set_reply_writes_enabled(false);
        let terminal = Arc::new(Mutex::new(one_line_terminal));
        let mut stream = noa_vt::Stream::new();
        let notifier = FakeNotifier::default();
        feed_remote_bytes(&mut stream, &terminal, b"seedfuture", &notifier);

        let stale_snapshot = RemoteScrollbackSnapshot {
            text: "seed".to_string(),
            truncated: false,
        };
        assert!(scrollback_snapshot_overtaken(&terminal, &stale_snapshot));

        let current_snapshot = RemoteScrollbackSnapshot {
            text: "older output\nseedfuture-and-more".to_string(),
            truncated: false,
        };
        assert!(!scrollback_snapshot_overtaken(&terminal, &current_snapshot));
    }

    #[derive(Clone)]
    struct SequentialScrollbackFactory {
        snapshots: Arc<Mutex<VecDeque<RemoteScrollbackSnapshot>>>,
        connects: Arc<AtomicUsize>,
    }

    struct SequentialScrollbackControl {
        snapshot: RemoteScrollbackSnapshot,
    }

    impl ControlTransport for SequentialScrollbackControl {
        type Attach = FakeAttach;
        type Reservation = ();

        fn granted_scopes(&self) -> noa_ipc::ScopeSet {
            scrollback_scopes()
        }

        fn reserve_attach(&mut self, _pane_id: u64) -> Result<Self::Reservation, TransportFailure> {
            Ok(())
        }

        fn open_reserved_attach(
            &mut self,
            _pane_id: u64,
            _reservation: &Self::Reservation,
        ) -> Result<Self::Attach, TransportFailure> {
            Err(TransportFailure)
        }

        fn detach(&mut self, _pane_id: u64) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn resize_pane(&mut self, _pane_id: u64, _size: GridSize) -> Result<(), TransportFailure> {
            Ok(())
        }

        fn get_scrollback(
            &mut self,
            _pane_id: u64,
        ) -> Result<RemoteScrollbackSnapshot, TransportFailure> {
            Ok(self.snapshot.clone())
        }
    }

    impl ConnectionFactory for SequentialScrollbackFactory {
        type Control = SequentialScrollbackControl;

        fn connect(
            &mut self,
            _control_url: &str,
            _token: &str,
            _requested_scopes: noa_ipc::ScopeSet,
        ) -> Result<Self::Control, TransportFailure> {
            self.connects.fetch_add(1, Ordering::AcqRel);
            let snapshot = self
                .snapshots
                .lock()
                .pop_front()
                .expect("test queued fewer snapshots than fetches performed");
            Ok(SequentialScrollbackControl { snapshot })
        }
    }

    #[test]
    fn overtaken_scrollback_snapshot_is_refetched_for_the_same_generation() {
        // Regression test for the permanently-missing-history bug this fixes:
        // raw output reached the replica ("seedfuture") before the worker
        // could merge a snapshot fetched while the pane still only had
        // "seed". That snapshot can never merge (see the pure test above),
        // so leaving it cached would strand the pane's scrollback short for
        // the rest of the connection. The worker must instead fetch again —
        // still under generation 3 — and merge the fresh snapshot.
        let mut one_line_terminal = Terminal::new(GridSize::new(80, 1));
        one_line_terminal.set_reply_writes_enabled(false);
        let terminal = Arc::new(Mutex::new(one_line_terminal));
        let mut stream = noa_vt::Stream::new();
        feed_remote_bytes(
            &mut stream,
            &terminal,
            b"seedfuture",
            &FakeNotifier::default(),
        );

        let connects = Arc::new(AtomicUsize::new(0));
        let factory = SequentialScrollbackFactory {
            snapshots: Arc::new(Mutex::new(VecDeque::from([
                RemoteScrollbackSnapshot {
                    text: "seed".to_string(),
                    truncated: false,
                },
                RemoteScrollbackSnapshot {
                    text: "older output\nseedfuture".to_string(),
                    truncated: false,
                },
            ]))),
            connects: Arc::clone(&connects),
        };
        let generation = Arc::new(AtomicU64::new(3));
        let generation_guard = Arc::new(Mutex::new(()));
        let requested_generation = Arc::new(AtomicU64::new(3));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        let cache = Arc::new(Mutex::new(None));

        let worker_cache = Arc::clone(&cache);
        let worker_shutdown = Arc::clone(&shutdown);
        let join = std::thread::spawn(move || {
            run_scrollback_worker(
                Arc::new(Mutex::new(credentials())),
                factory,
                generation,
                generation_guard,
                requested_generation,
                worker_shutdown,
                wake_rx,
                worker_cache,
                terminal,
                FakeNotifier::default(),
            );
        });

        wait_until(|| cache.lock().as_ref().is_some_and(|cached| cached.merged));
        shutdown.store(true, Ordering::Release);
        let _ = wake_tx.try_send(());
        join.join().unwrap();

        assert_eq!(
            connects.load(Ordering::Acquire),
            2,
            "an overtaken snapshot must trigger exactly one same-generation refetch"
        );
        assert!(
            cache
                .lock()
                .as_ref()
                .is_some_and(|cached| cached.generation == 3)
        );
    }

    #[test]
    fn saturated_input_queue_disconnects_the_generation_and_rejects_followups() {
        let (command_tx, command_rx) = crossbeam_channel::bounded(1);
        command_tx
            .try_send(RemoteCommand::Input {
                generation: 1,
                size: GridSize::new(80, 24),
                bytes: test_budgeted(b"queued"),
            })
            .unwrap();
        let (wake_tx, _wake_rx) = crossbeam_channel::bounded(1);
        let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
        let (_done_tx, done_rx) = crossbeam_channel::bounded(1);
        let generation = Arc::new(AtomicU64::new(1));
        let handle = RemoteConnectionHandle {
            command_tx,
            wake_tx,
            scrollback_wake_tx,
            credentials: Arc::new(Mutex::new(credentials())),
            desired_size: Arc::new(Mutex::new(GridSize::new(80, 24))),
            generation: Arc::clone(&generation),
            generation_guard: Arc::new(Mutex::new(())),
            shutdown: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(RemoteAttachState::Connected)),
            scrollback: Arc::new(Mutex::new(None)),
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            done_rx,
            joins: Vec::new(),
        };

        assert!(!handle.send_input(b"overflow".to_vec()));
        assert_eq!(generation.load(Ordering::Acquire), DISCONNECTED_GENERATION);
        assert!(!handle.send_input(b"followup".to_vec()));
        assert!(matches!(
            command_rx.try_recv(),
            Ok(RemoteCommand::Input { bytes, .. }) if bytes.as_ref() == b"queued"
        ));
    }

    #[test]
    fn oversized_pending_input_disconnects_the_generation_and_releases_the_budget() {
        let (command_tx, command_rx) = crossbeam_channel::bounded(COMMAND_CHANNEL_CAPACITY);
        let (wake_tx, _wake_rx) = crossbeam_channel::bounded(1);
        let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
        let (_done_tx, done_rx) = crossbeam_channel::bounded(1);
        let generation = Arc::new(AtomicU64::new(1));
        let handle = RemoteConnectionHandle {
            command_tx,
            wake_tx,
            scrollback_wake_tx,
            credentials: Arc::new(Mutex::new(credentials())),
            desired_size: Arc::new(Mutex::new(GridSize::new(80, 24))),
            generation: Arc::clone(&generation),
            generation_guard: Arc::new(Mutex::new(())),
            shutdown: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(RemoteAttachState::Connected)),
            scrollback: Arc::new(Mutex::new(None)),
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            done_rx,
            joins: Vec::new(),
        };

        // Each paste is charged at least its own length, so this many
        // maximum-size frames overruns the cap well before the channel's own
        // (much larger) capacity would.
        let paste = vec![b'x'; REMOTE_INPUT_PENDING_BYTE_CAP / 4];
        let mut accepted = 0;
        loop {
            if !handle.send_input(paste.clone()) {
                break;
            }
            accepted += 1;
            assert!(
                accepted <= 8,
                "budget must reject a paste well before this many iterations"
            );
        }
        assert_eq!(
            generation.load(Ordering::Acquire),
            DISCONNECTED_GENERATION,
            "exceeding the pending-byte budget must invalidate the generation \
             the same way a full command channel does"
        );
        assert!(
            !handle.send_input(b"followup".to_vec()),
            "input must stay rejected once the generation is disconnected"
        );

        // Draining the accepted commands releases their reservations via
        // `BudgetedInput::drop`, so the budget must return to zero rather
        // than staying pinned by commands that were already delivered.
        let mut drained = 0;
        while command_rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(drained, accepted);
        assert_eq!(
            handle.pending_input_bytes.load(Ordering::Acquire),
            0,
            "the budget must fully release once every accepted command is dropped"
        );
    }

    #[test]
    fn saturated_command_queue_still_coalesces_and_applies_the_latest_resize() {
        let (command_tx, command_rx) = crossbeam_channel::bounded(1);
        command_tx
            .try_send(RemoteCommand::Input {
                generation: 1,
                size: GridSize::new(80, 24),
                bytes: test_budgeted(b"queued"),
            })
            .unwrap();
        let (wake_tx, _wake_rx) = crossbeam_channel::bounded(1);
        let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
        let (_done_tx, done_rx) = crossbeam_channel::bounded(1);
        let desired_size = Arc::new(Mutex::new(GridSize::new(80, 24)));
        let handle = RemoteConnectionHandle {
            command_tx,
            wake_tx,
            scrollback_wake_tx,
            credentials: Arc::new(Mutex::new(credentials())),
            desired_size: Arc::clone(&desired_size),
            generation: Arc::new(AtomicU64::new(1)),
            generation_guard: Arc::new(Mutex::new(())),
            shutdown: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(RemoteAttachState::Connected)),
            scrollback: Arc::new(Mutex::new(None)),
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            done_rx,
            joins: Vec::new(),
        };

        assert!(
            handle.resize(GridSize::new(120, 40)),
            "a full command queue must retain the coalesced desired size"
        );
        let mut control = FakeControl::default();
        let mut attach = FakeAttach {
            fail_poll: true,
            ..FakeAttach::default()
        };
        let active_generation = AtomicU64::new(1);
        let generation_guard = Mutex::new(());
        let shutdown = AtomicBool::new(false);
        let scrollback = Arc::new(Mutex::new(None));
        let scrollback_requested_generation = AtomicU64::new(DISCONNECTED_GENERATION);
        let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
        assert!(matches!(
            run_connected(
                7,
                1,
                &mut control,
                &mut attach,
                &terminal(),
                &mut noa_vt::Stream::new(),
                &FakeNotifier::default(),
                &command_rx,
                &active_generation,
                &generation_guard,
                &shutdown,
                &scrollback,
                &scrollback_requested_generation,
                &scrollback_wake_tx,
                &desired_size,
                GridSize::new(80, 24),
            ),
            ConnectedOutcome::Disconnected
        ));
        assert_eq!(
            control.resizes,
            vec![(7, GridSize::new(120, 40))],
            "the connected loop must converge to the latest shared size"
        );
    }

    #[test]
    fn resize_is_sent_before_later_input_in_the_same_poll_batch() {
        let resized = GridSize::new(100, 30);
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut control = ConnectedRecordingControl {
            events: Arc::clone(&events),
            granted_scopes: requested_scopes(),
            reserve_fails: false,
        };
        let mut attach = ConnectedRecordingAttach {
            events: Arc::clone(&events),
            disconnect_on_poll: true,
        };
        let (command_tx, command_rx) = crossbeam_channel::bounded(2);
        for command in [
            RemoteCommand::Resize {
                generation: 1,
                size: resized,
            },
            RemoteCommand::Input {
                generation: 1,
                size: resized,
                bytes: test_budgeted(b"mouse-report"),
            },
        ] {
            command_tx.send(command).unwrap();
        }

        assert!(matches!(
            run_connected(
                7,
                1,
                &mut control,
                &mut attach,
                &terminal(),
                &mut noa_vt::Stream::new(),
                &FakeNotifier::default(),
                &command_rx,
                &AtomicU64::new(1),
                &Mutex::new(()),
                &AtomicBool::new(false),
                &Arc::new(Mutex::new(None)),
                &AtomicU64::new(DISCONNECTED_GENERATION),
                &crossbeam_channel::bounded(1).0,
                &Mutex::new(resized),
                GridSize::new(80, 24),
            ),
            ConnectedOutcome::Disconnected
        ));
        assert_eq!(*events.lock(), ["resize", "input"]);
    }

    #[test]
    fn leaving_alternate_screen_requests_primary_scrollback_backfill() {
        let terminal = terminal();
        let mut stream = noa_vt::Stream::new();
        stream.feed(b"\x1b[?1049h", &mut *terminal.lock());
        assert!(terminal.lock().active_is_alt);

        let mut control = FakeControl::default();
        let mut attach = FakeAttach {
            raw: VecDeque::from([b"\x1b[?1049l".to_vec()]),
            fail_poll: true,
            ..FakeAttach::default()
        };
        let (_command_tx, command_rx) = crossbeam_channel::bounded(1);
        let active_generation = AtomicU64::new(7);
        let generation_guard = Mutex::new(());
        let shutdown = AtomicBool::new(false);
        let scrollback = Arc::new(Mutex::new(None));
        let scrollback_requested_generation = AtomicU64::new(DISCONNECTED_GENERATION);
        let (scrollback_wake_tx, scrollback_wake_rx) = crossbeam_channel::bounded(1);
        let desired_size = Mutex::new(GridSize::new(80, 24));

        assert!(matches!(
            run_connected(
                7,
                7,
                &mut control,
                &mut attach,
                &terminal,
                &mut stream,
                &FakeNotifier::default(),
                &command_rx,
                &active_generation,
                &generation_guard,
                &shutdown,
                &scrollback,
                &scrollback_requested_generation,
                &scrollback_wake_tx,
                &desired_size,
                GridSize::new(80, 24),
            ),
            ConnectedOutcome::Disconnected
        ));
        assert!(!terminal.lock().active_is_alt);
        assert_eq!(scrollback_requested_generation.load(Ordering::Acquire), 7);
        assert_eq!(scrollback_wake_rx.try_recv(), Ok(()));
    }

    #[test]
    fn shutdown_and_join_does_not_block_the_event_loop_caller() {
        let (command_tx, _command_rx) = crossbeam_channel::bounded(1);
        let (wake_tx, _wake_rx) = crossbeam_channel::bounded(1);
        let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let (worker_exited_tx, worker_exited_rx) = crossbeam_channel::bounded(1);
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        let join = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            worker_exited_tx.send(()).unwrap();
            done_tx.send(()).unwrap();
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = RemoteConnectionHandle {
            command_tx,
            wake_tx,
            scrollback_wake_tx,
            credentials: Arc::new(Mutex::new(credentials())),
            desired_size: Arc::new(Mutex::new(GridSize::new(80, 24))),
            generation: Arc::new(AtomicU64::new(1)),
            generation_guard: Arc::new(Mutex::new(())),
            shutdown: Arc::clone(&shutdown),
            state: Arc::new(Mutex::new(RemoteAttachState::Connected)),
            scrollback: Arc::new(Mutex::new(None)),
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            done_rx,
            joins: vec![join],
        };

        let started = Instant::now();
        handle.shutdown_and_join();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "remote shutdown must hand joins to a background reaper"
        );
        assert!(
            shutdown.load(Ordering::Acquire),
            "workers must be notified before the caller returns"
        );

        release_tx.send(()).unwrap();
        worker_exited_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn stale_generation_commands_are_discarded_not_replayed() {
        let mut control = FakeControl::default();
        let mut attach = FakeAttach::default();
        let current_generation = 2;

        assert!(matches!(
            service_connected_command(
                RemoteCommand::Input {
                    generation: 1,
                    size: GridSize::new(80, 24),
                    bytes: test_budgeted(b"stale"),
                },
                current_generation,
                7,
                &mut control,
                &mut attach,
            ),
            CommandOutcome::Continue
        ));
        assert!(matches!(
            service_connected_command(
                RemoteCommand::Resize {
                    generation: 1,
                    size: GridSize::new(100, 30),
                },
                current_generation,
                7,
                &mut control,
                &mut attach,
            ),
            CommandOutcome::Continue
        ));
        assert!(attach.sent.is_empty());
        assert!(control.resizes.is_empty());

        assert!(matches!(
            service_connected_command(
                RemoteCommand::Input {
                    generation: current_generation,
                    size: GridSize::new(80, 24),
                    bytes: test_budgeted(b"current"),
                },
                current_generation,
                7,
                &mut control,
                &mut attach,
            ),
            CommandOutcome::Continue
        ));
        assert_eq!(attach.sent, vec![b"current".to_vec()]);
    }

    #[test]
    fn active_generation_forwards_key_ctrl_and_mouse_bytes_exactly() {
        let mut control = FakeControl::default();
        let mut attach = FakeAttach::default();
        let generation = 7;
        let encoded_inputs: [&[u8]; 3] = [b"\x1b[A", b"\x03", b"\x1b[<0;10;5M"];

        for bytes in encoded_inputs {
            assert!(matches!(
                service_connected_command(
                    RemoteCommand::Input {
                        generation,
                        size: GridSize::new(80, 24),
                        bytes: test_budgeted(bytes),
                    },
                    generation,
                    11,
                    &mut control,
                    &mut attach,
                ),
                CommandOutcome::Continue
            ));
        }

        assert_eq!(
            attach.sent,
            encoded_inputs.map(<[u8]>::to_vec).to_vec(),
            "the remote writer must receive the already-encoded local input bytes unchanged"
        );
    }

    #[test]
    fn lazy_scrollback_does_not_delay_seed_or_connected_state() {
        let connects = Arc::new(AtomicUsize::new(0));
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let remote_terminal = terminal();
        let handle = spawn_connection_manager(
            credentials(),
            Arc::clone(&remote_terminal),
            Arc::clone(&state),
            LazyBackfillFactory {
                connects: Arc::clone(&connects),
                scopes: Arc::clone(&scopes),
                events: Arc::clone(&events),
                started_tx,
                release_rx,
                fail_first_scrollback: false,
            },
            SystemClock,
            FakeNotifier::default(),
        )
        .unwrap();

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("lazy getText did not start");
        assert_eq!(*state.lock(), RemoteAttachState::Connected);
        assert_eq!(handle.scrollback_snapshot(), None);
        let events = events.lock().clone();
        assert_eq!(events, ["seed_delivered", "get_text_started"]);

        let requested = scopes.lock().clone();
        assert_eq!(requested.len(), 2);
        assert!(requested[0].contains(noa_ipc::Scope::Attach));
        assert!(requested[0].contains(noa_ipc::Scope::Read));
        assert!(requested[1].contains(noa_ipc::Scope::Read));
        assert!(!requested[1].contains(noa_ipc::Scope::Attach));

        release_tx.send(()).unwrap();
        wait_until(|| handle.scrollback_snapshot().is_some());
        assert_eq!(
            handle.scrollback_snapshot(),
            Some(RemoteScrollbackSnapshot {
                text: format!("older output\nseed{}", "\n".repeat(23)),
                truncated: false,
            })
        );
        assert_eq!(connects.load(Ordering::Acquire), 2);
        assert!(
            remote_terminal.lock().scrollback_len() > 0,
            "the completed generation-matched backfill must enter display history"
        );
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn transient_scrollback_failure_is_retried_for_the_active_generation() {
        let connects = Arc::new(AtomicUsize::new(0));
        let (started_tx, _started_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials(),
            terminal(),
            Arc::clone(&state),
            LazyBackfillFactory {
                connects: Arc::clone(&connects),
                scopes: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(Vec::new())),
                started_tx,
                release_rx,
                fail_first_scrollback: true,
            },
            SystemClock,
            FakeNotifier::default(),
        )
        .unwrap();

        wait_until(|| {
            connects.load(Ordering::Acquire) >= 2
                && matches!(*state.lock(), RemoteAttachState::Connected)
        });
        handle.scrollback_wake_tx.try_send(()).unwrap();
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut recovered = false;
        while Instant::now() < deadline {
            if handle.scrollback_snapshot().is_some() {
                recovered = true;
                break;
            }
            std::thread::yield_now();
        }

        handle.request_shutdown();
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
        assert!(recovered, "scrollback fetch was not retried");
        assert_eq!(connects.load(Ordering::Acquire), 3);
    }

    #[test]
    fn read_denied_scrollback_is_not_retried_for_the_same_generation() {
        let connects = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(7));
        let requested_generation = Arc::new(AtomicU64::new(7));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        let worker_generation = Arc::clone(&generation);
        let worker_requested_generation = Arc::clone(&requested_generation);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_connects = Arc::clone(&connects);
        let join = std::thread::spawn(move || {
            run_scrollback_worker(
                Arc::new(Mutex::new(credentials())),
                ReadDeniedBackfillFactory {
                    connects: worker_connects,
                },
                worker_generation,
                Arc::new(Mutex::new(())),
                worker_requested_generation,
                worker_shutdown,
                wake_rx,
                Arc::new(Mutex::new(None)),
                terminal(),
                FakeNotifier::default(),
            );
        });

        wait_until(|| connects.load(Ordering::Acquire) == 1);
        requested_generation.store(7, Ordering::Release);
        wake_tx.try_send(()).unwrap();
        wait_until(|| requested_generation.load(Ordering::Acquire) == 0);

        shutdown.store(true, Ordering::Release);
        // The worker may not have drained the previous wake token yet; a full
        // channel already guarantees it wakes up and observes `shutdown`, so
        // `Full` is fine here (matches `schedule_scrollback_backfill`).
        let _ = wake_tx.try_send(());
        join.join().unwrap();
        assert_eq!(connects.load(Ordering::Acquire), 1);
    }

    #[test]
    fn input_dispatch_uses_one_millisecond_poll_and_submillisecond_processing_budget() {
        let connects = Arc::new(AtomicUsize::new(0));
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, _started_rx) = crossbeam_channel::bounded(1);
        let (_release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut factory = LazyBackfillFactory {
            connects,
            scopes,
            events,
            started_tx,
            release_rx,
            fail_first_scrollback: false,
        };
        let terminal = terminal();
        let mut stream = noa_vt::Stream::new();
        // Simulate a disconnect in the middle of OSC. The new connection's
        // seed must start with a fresh parser generation rather than being
        // swallowed as part of the stale sequence.
        stream.feed(b"\x1b]", &mut *terminal.lock());
        let connection = establish_connection(
            &mut factory,
            &credentials(),
            &terminal,
            &Arc::new(Mutex::new(GridSize::new(80, 24))),
            &mut stream,
            &FakeNotifier::default(),
        );
        let Ok(EstablishedConnection {
            control,
            attach,
            size: established_size,
        }) = connection
        else {
            panic!("fake attach connection failed");
        };
        assert_eq!(established_size, GridSize::new(80, 24));
        assert_eq!(attach.poll_timeout, Some(REMOTE_ATTACH_POLL_TIMEOUT));
        assert_eq!(control.resizes, vec![(7, GridSize::new(80, 24))]);
        assert_eq!(
            terminal.lock().active().grid[0].cells[..4]
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "seed"
        );

        let mut control = FakeControl::default();
        let mut attach = FakeAttach::default();
        let (command_tx, command_rx) = crossbeam_channel::bounded(1);
        let samples = 1_000_u32;
        let started = Instant::now();
        for _ in 0..samples {
            assert!(
                command_tx
                    .try_send(RemoteCommand::Input {
                        generation: 1,
                        size: GridSize::new(80, 24),
                        bytes: test_budgeted(b"x"),
                    })
                    .is_ok(),
                "input command queue unexpectedly full"
            );
            let command = command_rx.try_recv();
            let Ok(command) = command else {
                panic!("queued input command was unavailable");
            };
            assert!(matches!(
                service_connected_command(command, 1, 7, &mut control, &mut attach,),
                CommandOutcome::Continue
            ));
        }
        let average = started.elapsed() / samples;
        assert!(
            average < Duration::from_millis(1),
            "average input command processing was {average:?}"
        );
        assert_eq!(attach.sent.len(), samples as usize);
    }

    #[test]
    fn remote_stream_feed_stays_under_submillisecond_processing_budget() {
        const WARMUP_SAMPLES: usize = 64;
        const MEASURED_SAMPLES: usize = 1_000;
        const OUTPUT: &[u8] = b"\x1b[38;5;42mremote-output\x1b[0m\r\n";

        let terminal = terminal();
        let notifier = FakeNotifier::default();
        let mut stream = noa_vt::Stream::new();
        let mut samples = Vec::with_capacity(MEASURED_SAMPLES);

        for sample in 0..WARMUP_SAMPLES + MEASURED_SAMPLES {
            let started = Instant::now();
            feed_remote_bytes(&mut stream, &terminal, OUTPUT, &notifier);
            if sample >= WARMUP_SAMPLES {
                samples.push(started.elapsed());
            }
        }

        samples.sort_unstable();
        let mean = samples.iter().map(Duration::as_nanos).sum::<u128>() / samples.len() as u128;
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        let budget = Duration::from_millis(1);
        assert!(
            mean < budget.as_nanos() && p95 < budget,
            "remote Stream feed exceeded budget: mean={mean}ns, p95={p95:?}"
        );
        assert_eq!(
            notifier.redraws.load(Ordering::Acquire),
            WARMUP_SAMPLES + MEASURED_SAMPLES
        );
    }

    #[test]
    fn remote_stream_feed_publishes_overview_snapshot_before_redraw() {
        let terminal = terminal();
        let slot = Arc::new(Mutex::new(None));
        let notifier = FakeNotifier {
            overview: Some(RemoteOverviewPublisher::new(
                crate::io_thread::OverviewPublish {
                    slot: Arc::clone(&slot),
                    visible: Arc::new(AtomicBool::new(true)),
                },
            )),
            ..FakeNotifier::default()
        };
        let mut stream = noa_vt::Stream::new();

        feed_remote_bytes(&mut stream, &terminal, b"remote", &notifier);

        let snapshot = slot
            .lock()
            .clone()
            .expect("overview snapshot was not published");
        assert_eq!(snapshot.rows[0].cells[0].ch, 'r');
        assert_eq!(notifier.redraws.load(Ordering::Acquire), 1);
        assert_eq!(notifier.redraws_with_snapshot.load(Ordering::Acquire), 1);
    }

    #[test]
    fn remote_bel_forwards_a_bell_delta_like_the_local_pty_path() {
        let terminal = terminal();
        let notifier = FakeNotifier::default();
        let mut stream = noa_vt::Stream::new();

        feed_remote_bytes(&mut stream, &terminal, b"before bell\x07after", &notifier);

        assert_eq!(notifier.bells.load(Ordering::Acquire), 1);
        // Draining is unconditional (mirrors `io_thread::feed`): a batch with
        // no BEL must not report one.
        feed_remote_bytes(&mut stream, &terminal, b"no bell here", &notifier);
        assert_eq!(notifier.bells.load(Ordering::Acquire), 1);
    }

    #[test]
    fn remote_synchronized_output_left_open_arms_a_trailing_redraw_deadline() {
        // A batch that leaves DECSET 2026 set must still report
        // `synchronized_output: true` so `run_connected` can arm a trailing
        // redraw once the render side's held-snapshot reuse window
        // (`SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`) elapses, even if no further
        // remote output ever arrives.
        let terminal = terminal();
        let notifier = FakeNotifier::default();
        let mut stream = noa_vt::Stream::new();

        let outcome = feed_remote_bytes(&mut stream, &terminal, b"\x1b[?2026h", &notifier);
        assert!(outcome.synchronized_output);

        let outcome = feed_remote_bytes(&mut stream, &terminal, b"\x1b[?2026l", &notifier);
        assert!(!outcome.synchronized_output);
    }

    #[test]
    fn remote_connected_loop_fires_a_trailing_redraw_after_synchronized_output_stalls() {
        // Regression test for the freeze this fixes: a batch enables mode
        // 2026 and then the attached pane goes quiet (no further raw bytes),
        // yet the render side can only reuse its held snapshot for
        // `SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`. Without a trailing redraw
        // scheduled from here, nothing would ever wake the render again and
        // the screen would stay stuck on the stale frame forever.
        let active_generation = Arc::new(AtomicU64::new(1));
        let shutdown = Arc::new(AtomicBool::new(false));
        let notifier = FakeNotifier::default();
        let redraws = Arc::clone(&notifier.redraws);

        let thread_generation = Arc::clone(&active_generation);
        let thread_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            let mut control = FakeControl::default();
            let mut attach = FakeAttach {
                raw: VecDeque::from([b"\x1b[?2026h".to_vec()]),
                ..FakeAttach::default()
            };
            let (_command_tx, command_rx) = crossbeam_channel::bounded(1);
            let generation_guard = Mutex::new(());
            let scrollback = Arc::new(Mutex::new(None));
            let scrollback_requested_generation = AtomicU64::new(DISCONNECTED_GENERATION);
            let (scrollback_wake_tx, _scrollback_wake_rx) = crossbeam_channel::bounded(1);
            let desired_size = Mutex::new(GridSize::new(80, 24));
            run_connected(
                7,
                1,
                &mut control,
                &mut attach,
                &terminal(),
                &mut noa_vt::Stream::new(),
                &notifier,
                &command_rx,
                &thread_generation,
                &generation_guard,
                &thread_shutdown,
                &scrollback,
                &scrollback_requested_generation,
                &scrollback_wake_tx,
                &desired_size,
                GridSize::new(80, 24),
            )
        });

        // The lone batch itself already earns one immediate redraw; wait for
        // the *second* one, which only the trailing deadline can produce
        // since no further bytes ever arrive.
        wait_until(|| redraws.load(Ordering::Acquire) >= 2);

        shutdown.store(true, Ordering::Release);
        assert!(matches!(handle.join().unwrap(), ConnectedOutcome::Shutdown));
    }

    #[test]
    fn remote_overview_trailing_flush_publishes_the_latest_terminal_state() {
        let terminal = terminal();
        let slot = Arc::new(Mutex::new(None));
        let publisher = RemoteOverviewPublisher::new(crate::io_thread::OverviewPublish {
            slot: Arc::clone(&slot),
            visible: Arc::new(AtomicBool::new(true)),
        });
        let mut stream = noa_vt::Stream::new();
        feed_remote_bytes(&mut stream, &terminal, b"old", &FakeNotifier::default());
        publisher.publish(&terminal.lock());
        feed_remote_bytes(&mut stream, &terminal, b"\rnew", &FakeNotifier::default());
        let due = Instant::now();
        *publisher.timing.lock() = RemoteOverviewTiming {
            last_publish: Some(due - crate::session_overview::OVERVIEW_TILE_MIN_RENDER_INTERVAL),
            pending_at: Some(due),
        };

        assert!(publisher.flush_if_due(&terminal));

        let snapshot = slot
            .lock()
            .clone()
            .expect("trailing snapshot was not published");
        assert_eq!(snapshot.rows[0].cells[0].ch, 'n');
    }

    #[test]
    fn shutdown_wakes_a_real_clock_backoff_and_joins_within_bound() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials(),
            terminal(),
            state,
            NeverConnectFactory {
                attempts: Arc::clone(&attempts),
            },
            SystemClock,
            FakeNotifier::default(),
        )
        .unwrap();
        wait_until(|| attempts.load(Ordering::Acquire) >= 1);

        let started = Instant::now();
        assert!(handle.shutdown_and_join_timeout(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn connected_shutdown_closes_raw_channel_then_detaches() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(RemoteAttachState::Detached));
        let handle = spawn_connection_manager(
            credentials(),
            terminal(),
            Arc::clone(&state),
            ConnectedRecordingFactory {
                events: Arc::clone(&events),
                reserve_fails: false,
            },
            SystemClock,
            FakeNotifier::default(),
        )
        .unwrap();

        wait_until(|| matches!(*state.lock(), RemoteAttachState::Connected));
        assert!(handle.shutdown_and_join_timeout(Duration::from_secs(2)));
        assert_eq!(
            *events.lock(),
            [
                "control_connect",
                "reserve",
                "resize",
                "raw_open",
                "poll_timeout",
                "seed",
                "raw_close",
                "detach",
            ]
        );
    }

    #[test]
    fn attach_reservation_conflict_never_resizes_the_remote_pane() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut factory = ConnectedRecordingFactory {
            events: Arc::clone(&events),
            reserve_fails: true,
        };
        let result = establish_connection(
            &mut factory,
            &credentials(),
            &terminal(),
            &Arc::new(Mutex::new(GridSize::new(100, 30))),
            &mut noa_vt::Stream::new(),
            &FakeNotifier::default(),
        );

        assert!(result.is_err());
        assert_eq!(*events.lock(), ["control_connect", "reserve"]);
    }

    #[test]
    fn remote_feed_drains_generated_replies_and_clipboard_requests() {
        let terminal = terminal();
        terminal.lock().osc52_policy.allow_read = true;
        let notifier = FakeNotifier::default();
        let mut stream = noa_vt::Stream::new();

        feed_remote_bytes(
            &mut stream,
            &terminal,
            b"\x1b[5n\x1b]52;c;?\x07\x1b]52;c;aGVsbG8=\x07",
            &notifier,
        );

        let mut terminal = terminal.lock();
        assert!(terminal.take_pending_writes().is_empty());
        assert!(terminal.take_pending_clipboard_reads().is_empty());
        assert!(terminal.take_pending_clipboard_writes().is_empty());
        assert_eq!(notifier.redraws.load(Ordering::Acquire), 1);
    }

    // ---- Heartbeat: pure `Instant` arithmetic, so these run instantly with
    // no real sleeping (`Instant` supports offsetting by a `Duration` without
    // the clock actually advancing). ----

    #[test]
    fn heartbeat_stays_quiet_before_the_idle_interval_elapses() {
        let start = Instant::now();
        let mut heartbeat = Heartbeat::new(start);

        assert!(matches!(
            heartbeat.poll(start + REMOTE_HEARTBEAT_INTERVAL - Duration::from_millis(1)),
            HeartbeatAction::Wait
        ));
    }

    #[test]
    fn heartbeat_sends_a_ping_once_the_idle_interval_elapses() {
        let start = Instant::now();
        let mut heartbeat = Heartbeat::new(start);
        let due = start + REMOTE_HEARTBEAT_INTERVAL;

        assert!(matches!(heartbeat.poll(due), HeartbeatAction::SendPing));
        // A probe is already outstanding: polling again immediately must not
        // send a second one on top of it.
        assert!(matches!(
            heartbeat.poll(due + Duration::from_millis(1)),
            HeartbeatAction::Wait
        ));
    }

    #[test]
    fn heartbeat_activity_cancels_a_pending_probe_and_resets_the_interval() {
        let start = Instant::now();
        let mut heartbeat = Heartbeat::new(start);
        let ping_at = start + REMOTE_HEARTBEAT_INTERVAL;
        assert!(matches!(heartbeat.poll(ping_at), HeartbeatAction::SendPing));

        let activity_at = ping_at + Duration::from_secs(1);
        heartbeat.record_activity(activity_at);

        // Immediately after activity, neither the outstanding-probe timeout
        // nor a fresh probe should fire.
        assert!(matches!(
            heartbeat.poll(activity_at + Duration::from_millis(1)),
            HeartbeatAction::Wait
        ));
        // The interval must be measured from the new activity, not the
        // original start.
        assert!(matches!(
            heartbeat.poll(activity_at + REMOTE_HEARTBEAT_INTERVAL - Duration::from_millis(1)),
            HeartbeatAction::Wait
        ));
        assert!(matches!(
            heartbeat.poll(activity_at + REMOTE_HEARTBEAT_INTERVAL),
            HeartbeatAction::SendPing
        ));
    }

    #[test]
    fn heartbeat_pong_cancels_a_pending_probe_same_as_raw_activity() {
        let start = Instant::now();
        let mut heartbeat = Heartbeat::new(start);
        let ping_at = start + REMOTE_HEARTBEAT_INTERVAL;
        assert!(matches!(heartbeat.poll(ping_at), HeartbeatAction::SendPing));

        let pong_at = ping_at + Duration::from_millis(50);
        heartbeat.record_activity(pong_at);

        assert!(matches!(
            heartbeat.poll(pong_at + REMOTE_HEARTBEAT_PONG_TIMEOUT),
            HeartbeatAction::Wait
        ));
    }

    #[test]
    fn heartbeat_times_out_when_pong_never_arrives() {
        let start = Instant::now();
        let mut heartbeat = Heartbeat::new(start);
        let ping_at = start + REMOTE_HEARTBEAT_INTERVAL;
        assert!(matches!(heartbeat.poll(ping_at), HeartbeatAction::SendPing));

        assert!(matches!(
            heartbeat.poll(ping_at + REMOTE_HEARTBEAT_PONG_TIMEOUT - Duration::from_millis(1)),
            HeartbeatAction::Wait
        ));
        assert!(matches!(
            heartbeat.poll(ping_at + REMOTE_HEARTBEAT_PONG_TIMEOUT),
            HeartbeatAction::TimedOut
        ));
    }
}
