//! The JSON-RPC-over-WebSocket server (spec §L2 "Transport & Handshake",
//! sync tungstenite + thread-per-connection + crossbeam, no async runtime —
//! NFR-3).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};

/// Hard cap on concurrent connections (F-3 / DoS bound): a proxy flood
/// (Omen ④) closes excess accepts immediately rather than spawning an
/// unbounded number of connection threads.
const MAX_CONNECTIONS: usize = 32;

/// Per-remote-source-IP cap on concurrent connections, a subset of the global
/// [`MAX_CONNECTIONS`] budget (LAN-exposure hardening). On a non-loopback
/// (`server-bind = 0.0.0.0`) server, a single remote host — authenticated or
/// not — otherwise races the whole 32-slot pool to exhaustion by opening
/// connections faster than the hello deadline reclaims them, denying every
/// other client. Capping per source IP keeps one host from monopolizing the
/// pool while still admitting one fully populated nine-pane Client Mode tab:
/// every attached pane holds control + raw sockets and briefly opens a third
/// read socket for scrollback backfill.
///
/// Loopback peers are deliberately exempt (see the accept loop): there every
/// local client shares `127.0.0.1`, so a per-IP cap couldn't tell a hostile
/// local script from a legitimate one and would only shrink the usable pool
/// for the default loopback-only deployment.
const MAX_REMOTE_PANES_PER_CLIENT_TAB: usize = 9;
const PEAK_CONNECTIONS_PER_REMOTE_PANE: usize = 3;
const MAX_CONNECTIONS_PER_REMOTE_IP: usize =
    MAX_REMOTE_PANES_PER_CLIENT_TAB * PEAK_CONNECTIONS_PER_REMOTE_PANE;

/// Bounds on a single incoming WebSocket frame/message (F-3 / DoS bound),
/// well above the largest legitimate request (`noa.sendText`'s text isn't
/// otherwise capped, but a well-behaved client's paste-sized input is far
/// under this) and far below tungstenite's 64 MiB/16 MiB defaults.
const MAX_WS_MESSAGE_SIZE: usize = 1024 * 1024;
const MAX_WS_FRAME_SIZE: usize = 256 * 1024;
const MAX_RESIZE_COLS: u16 = 4096;
const MAX_RESIZE_ROWS: u16 = 4096;
const MAX_RESIZE_CELLS: u32 = 1024 * 1024;

/// Per-read/write idle bound during the WS handshake — the ceiling any one
/// `DeadlineStream` read/write call is ever given, even when more of the
/// absolute handshake deadline (R-2) remains.
const HANDSHAKE_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Write timeout applied to the raw `TcpStream` for the entire lifetime of a
/// connection *after* the WS handshake completes (R-4): without this, a
/// subscriber that stops reading while the server keeps pushing
/// notifications blocks this thread's `ws.send` forever once the kernel's
/// TCP send buffer fills — the thread never reaches its shutdown-flag poll
/// again, and its connection slot + broadcaster registration leak for the
/// life of the process.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Stable close reason for attach authentication/protocol failures. The raw
/// channel never carries JSON-RPC frames, so the noa error code is surfaced in
/// a policy-violation close frame instead.
pub const ATTACH_HANDSHAKE_CLOSE_REASON: &str = "-32008 attach handshake failure";

const ATTACH_PATH: &str = "/attach";
const ATTACH_READ_POLL: Duration = Duration::from_millis(1);
/// Bound one output-drain turn so a continuously refilled PTY queue cannot
/// starve client input waiting on the same WebSocket.
const ATTACH_OUTPUT_BYTES_PER_POLL: usize = 64 * 1024;

fn connection_ws_config() -> WebSocketConfig {
    #[allow(deprecated)]
    WebSocketConfig {
        max_send_queue: None,
        write_buffer_size: 128 * 1024,
        // Bounds both control and raw connections. Raw output also passes
        // through the exact byte-counted 1 MiB queue in `attach`.
        max_write_buffer_size: crate::attach::ATTACH_OUTPUT_CAPACITY_BYTES,
        max_message_size: Some(MAX_WS_MESSAGE_SIZE),
        max_frame_size: Some(MAX_WS_FRAME_SIZE),
        accept_unmasked_frames: false,
    }
}

use crate::attach::{
    ATTACH_BINARY_CHUNK_BYTES, AttachOutputReceiver, AttachRegistry, AttachTryRecvError,
    LeaseIdentity, ReserveError, output_channel,
};
use crate::auth::{Scope, ScopeSet, constant_time_eq};
use crate::backend::{IpcBackend, PaneRef};
use crate::error::{ErrorCode, IpcError};
use crate::protocol::*;
use crate::push::{AddSubscriptionError, Broadcaster, EventMask, PushQueue, QueuedNotification};

/// Server startup configuration (spec §L2 "Config キー").
pub struct ServerConfig {
    /// `0` binds an OS-assigned ephemeral port (tests use this; production
    /// uses config `server-port`, default `61771`).
    pub port: u16,
    /// Interface address to bind (v2 LAN opt-in). Production uses config
    /// `server-bind`, default [`ServerConfig::DEFAULT_BIND_ADDR`] (loopback
    /// only, matching the locked v1 spec's FR-2). Token auth (FR-3) applies
    /// unconditionally regardless of bind address.
    pub bind_addr: std::net::IpAddr,
    pub token: String,
    pub allowed_scopes: ScopeSet,
    /// How long an accepted, WS-handshaked connection may go without
    /// completing `noa.hello` before it's closed and its slot reclaimed
    /// (R-1). Defaults to [`ServerConfig::DEFAULT_HELLO_DEADLINE`]; tests
    /// shorten this to make the deadline observable without a real wait.
    pub hello_deadline: Duration,
    /// Absolute wall-clock bound on completing the WS handshake itself
    /// (R-2): a slowloris that keeps feeding the raw TCP stream a few bytes
    /// every `HANDSHAKE_IO_TIMEOUT` would otherwise stall
    /// `accept_hdr_with_config` indefinitely, since a per-read idle timeout
    /// alone never expires as long as *some* byte arrives before each read
    /// times out. Defaults to [`ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT`];
    /// tests shorten this to make the deadline observable without a real
    /// wait.
    pub handshake_timeout: Duration,
}

impl ServerConfig {
    pub const DEFAULT_HELLO_DEADLINE: Duration = Duration::from_secs(10);
    pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    pub const DEFAULT_BIND_ADDR: std::net::IpAddr =
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
}

/// A handle to a running server. Dropping it stops the accept loop and
/// closes the listener.
pub struct ServerHandle {
    port: u16,
    bind_addr: std::net::IpAddr,
    shutdown: Arc<AtomicBool>,
    broadcaster: Broadcaster,
    connection_count: Arc<AtomicUsize>,
    accept_thread: Option<JoinHandle<()>>,
    /// Shared with the accept thread. The handle keeps its own reference so
    /// the listener fd is guaranteed still open whenever
    /// [`ServerHandle::stop_accept_thread`] needs to force-close it — the fd
    /// can't have been dropped (and its number reused) while this `Arc` is
    /// alive.
    listener: Arc<TcpListener>,
}

impl ServerHandle {
    /// Bound on how long a shutdown waits for the accept thread to observe
    /// the flag and exit before detaching it with a warning.
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn bind_addr(&self) -> std::net::IpAddr {
        self.bind_addr
    }

    pub fn broadcaster(&self) -> Broadcaster {
        self.broadcaster.clone()
    }

    /// Includes both control and raw attach WebSocket connections.
    pub fn active_connection_count(&self) -> usize {
        self.connection_count.load(Ordering::SeqCst)
    }

    /// Stop the accept loop: set the shutdown flag, unblock the blocking
    /// `accept()` (wake connection, falling back to force-closing the
    /// listener socket), and join the thread with a bounded timeout.
    /// Returns whether the thread was actually reaped. Factored out of
    /// `Drop` so tests can drive the wake-failure path with an unreachable
    /// `wake_addr`.
    fn stop_accept_thread(&mut self, wake_addr: std::net::SocketAddr) -> bool {
        self.shutdown.store(true, Ordering::SeqCst);
        let Some(handle) = self.accept_thread.take() else {
            return true;
        };
        // The accept thread sits in a *blocking* `accept()` (zero idle
        // wake-ups — it used to poll at 20ms forever); wake it with a
        // throwaway loopback connection so it can observe the shutdown
        // flag and exit.
        if let Err(err) = TcpStream::connect_timeout(&wake_addr, Duration::from_millis(250)) {
            // Wake connection failed (exhausted fd table, firewall filter,
            // half-torn-down network stack …). Force-close the listening
            // socket instead: on macOS a blocked `accept(2)` does NOT return
            // on `shutdown(2)` of a listening socket (that fails ENOTCONN),
            // but it does return `ECONNABORTED` once the socket itself is
            // deallocated — which `force_close_listener`'s dup2-to-/dev/null
            // achieves without ever double-closing a possibly-reused fd
            // number. Without this, the parked thread (and the port) leaked
            // for the life of the process.
            log::warn!(
                "noa-ipc: could not wake accept thread for shutdown ({err}); force-closing the listener"
            );
            force_close_listener(&self.listener);
        }
        // The accept loop rechecks the flag before handling any accepted
        // stream, so a wake connection is dropped unhandled; a force-closed
        // listener surfaces as an accept error, whose handler also rechecks
        // the flag. Either way the thread exits promptly — but bound the
        // wait anyway so a pathological stall can only cost `JOIN_TIMEOUT`,
        // never hang the caller (the main thread, on config-reload server
        // restarts).
        let deadline = Instant::now() + Self::JOIN_TIMEOUT;
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !handle.is_finished() {
            log::warn!(
                "noa-ipc: accept thread did not stop within {:?}; detaching it",
                Self::JOIN_TIMEOUT
            );
            return false;
        }
        if let Err(err) = handle.join() {
            log::warn!("noa-ipc: accept thread panicked during shutdown: {err:?}");
            return false;
        }
        true
    }
}

/// Close the listening socket out from under a thread blocked in `accept()`
/// without freeing its fd *number*: `dup2` an fd for `/dev/null` over the
/// listener's fd. The kernel drops the socket's last reference (waking the
/// blocked `accept(2)` with `ECONNABORTED` — verified on macOS; `shutdown(2)`
/// on a listening socket fails `ENOTCONN` and wakes nothing), while the fd
/// number stays validly open on `/dev/null`, so the accept thread's eventual
/// `TcpListener` drop closes that harmless dup instead of racing a reused fd
/// (no double-close).
#[cfg(unix)]
fn force_close_listener(listener: &TcpListener) {
    use std::os::fd::AsRawFd;
    match std::fs::File::open("/dev/null") {
        Ok(devnull) => {
            let rv = unsafe { libc::dup2(devnull.as_raw_fd(), listener.as_raw_fd()) };
            if rv < 0 {
                log::warn!(
                    "noa-ipc: dup2 over the listener fd failed: {}",
                    io::Error::last_os_error()
                );
            }
        }
        Err(err) => log::warn!("noa-ipc: cannot open /dev/null to close the listener: {err}"),
    }
}

#[cfg(not(unix))]
fn force_close_listener(_listener: &TcpListener) {
    // No safe way to close a listener out from under a blocked accept
    // without fd-number reuse hazards on this platform; the thread stays
    // parked until process exit (the pre-hardening behavior).
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // An unspecified bind address (`0.0.0.0`/`::`) is reachable via the
        // matching loopback.
        let wake_ip = match self.bind_addr {
            std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            }
            std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
            ip => ip,
        };
        self.stop_accept_thread(std::net::SocketAddr::new(wake_ip, self.port));
    }
}

pub struct Server;

impl Server {
    /// Binds `<bind_addr>:<port>` and starts the accept loop. Defaults to
    /// `127.0.0.1` only (FR-2); binds a non-loopback interface only when the
    /// caller opts in via `config.bind_addr` (v2 LAN opt-in, `server-bind`).
    ///
    /// `broadcaster` is supplied by the caller rather than created here so
    /// long-lived push connections and attach ownership can outlive any one
    /// `Server::start`/`ServerHandle` drop cycle (e.g. `noa-app`'s
    /// config-reload server restart). Panes keep pushing without being
    /// re-wired, and old/new connection threads consult the same attach lease
    /// registry while their lifetimes overlap.
    pub fn start(
        config: ServerConfig,
        backend: Arc<dyn IpcBackend>,
        broadcaster: Broadcaster,
    ) -> io::Result<ServerHandle> {
        if config.token.trim().is_empty() {
            // Defense in depth alongside `auth::load_or_create_token`'s
            // empty-token fallback (R-1): no call path can ever start a
            // server that authenticates every bearer with "".
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "server token must not be empty",
            ));
        }
        let bind_addr = config.bind_addr;
        // The listener stays *blocking*: the accept thread parks in
        // `accept()` and wakes only for a real connection (or the
        // `ServerHandle::drop` wake connection) instead of the previous
        // non-blocking accept + 20ms sleep poll, which burned ~42 wake-ups/s
        // for the whole life of the server even with zero clients.
        let listener = Arc::new(TcpListener::bind((bind_addr, config.port))?);
        let port = listener.local_addr()?.port();

        let shutdown = Arc::new(AtomicBool::new(false));
        let token = Arc::new(config.token);
        let allowed_scopes = config.allowed_scopes;
        let hello_deadline = config.hello_deadline;
        let handshake_timeout = config.handshake_timeout;
        let connection_count = Arc::new(AtomicUsize::new(0));
        let connection_count_handle = connection_count.clone();
        let attach_registry = broadcaster.attach_registry();
        let per_ip_counts: Arc<Mutex<HashMap<IpAddr, usize>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let shutdown_loop = shutdown.clone();
        let broadcaster_loop = broadcaster.clone();
        let listener_loop = listener.clone();
        let accept_thread = thread::spawn(move || {
            while !shutdown_loop.load(Ordering::SeqCst) {
                match listener_loop.accept() {
                    Ok((stream, addr)) => {
                        // Recheck after every (blocking) accept: a shutdown
                        // wake connection (see `ServerHandle::drop`) must be
                        // dropped unhandled, not given a connection thread.
                        if shutdown_loop.load(Ordering::SeqCst) {
                            drop(stream);
                            break;
                        }
                        // Refuse excess connections by closing immediately
                        // rather than spawning a thread for them (F-3).
                        if connection_count.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                            connection_count.fetch_sub(1, Ordering::SeqCst);
                            drop(stream);
                            continue;
                        }
                        // Per-remote-IP cap (LAN-exposure hardening): keep a
                        // single non-loopback source from racing the whole
                        // pool to exhaustion. Loopback peers are exempt — a
                        // per-IP cap there would just shrink the default
                        // deployment's usable pool without isolating anything.
                        // `ip_slot` is `Some` only for a peer that consumed a
                        // per-IP slot, so the `ConnectionGuard` releases
                        // exactly what was reserved on every exit path.
                        let ip_slot = match reserve_ip_slot(&per_ip_counts, addr.ip()) {
                            IpSlot::Exempt => None,
                            IpSlot::Reserved(ip) => Some(ip),
                            IpSlot::Rejected => {
                                connection_count.fetch_sub(1, Ordering::SeqCst);
                                drop(stream);
                                continue;
                            }
                        };
                        let backend = backend.clone();
                        let token = token.clone();
                        let broadcaster = broadcaster_loop.clone();
                        let shutdown_conn = shutdown_loop.clone();
                        let connection_count = connection_count.clone();
                        let per_ip_counts = per_ip_counts.clone();
                        let attach_registry = attach_registry.clone();
                        thread::spawn(move || {
                            // R-4: `handle_connection` owns a `ConnectionGuard`
                            // that decrements `connection_count` and
                            // unregisters the broadcaster connection on every
                            // exit path — normal return, `?`-propagated I/O
                            // error, *and* an unwinding panic (this crate
                            // doesn't set `panic = "abort"`, so the guard's
                            // `Drop` still runs). No cleanup happens out here
                            // anymore.
                            if let Err(err) = handle_connection(
                                stream,
                                backend,
                                token,
                                allowed_scopes,
                                hello_deadline,
                                handshake_timeout,
                                broadcaster,
                                shutdown_conn,
                                connection_count,
                                per_ip_counts,
                                ip_slot,
                                attach_registry,
                            ) {
                                log::debug!("noa-ipc: connection ended: {err}");
                            }
                        });
                    }
                    Err(err) => {
                        // A shutdown that couldn't open a wake connection
                        // force-closes the listener instead, surfacing here
                        // as ECONNABORTED (see `stop_accept_thread`) — exit
                        // immediately rather than logging and backing off.
                        if shutdown_loop.load(Ordering::SeqCst) {
                            break;
                        }
                        // Transient accept failure (e.g. EMFILE, aborted
                        // connection). Back off briefly so a persistent error
                        // can't spin this thread; the loop condition rechecks
                        // the shutdown flag each iteration.
                        log::warn!("noa-ipc: accept error: {err}");
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });

        Ok(ServerHandle {
            port,
            bind_addr,
            shutdown,
            broadcaster,
            connection_count: connection_count_handle,
            accept_thread: Some(accept_thread),
            listener,
        })
    }
}

/// Outcome of reserving a per-remote-IP connection slot in the accept loop.
enum IpSlot {
    /// Loopback peer — no per-IP accounting applies (see
    /// [`MAX_CONNECTIONS_PER_REMOTE_IP`]).
    Exempt,
    /// A non-loopback peer under its per-IP cap; the reserved slot must be
    /// released on connection teardown.
    Reserved(IpAddr),
    /// A non-loopback peer already at [`MAX_CONNECTIONS_PER_REMOTE_IP`] — the
    /// accept loop closes the stream immediately.
    Rejected,
}

/// Reserves a per-IP connection slot for `ip`, returning whether the accept
/// loop may proceed. Loopback is exempt; a non-loopback peer is admitted only
/// while it holds fewer than [`MAX_CONNECTIONS_PER_REMOTE_IP`] live
/// connections, incrementing its count on success. Released by
/// [`release_ip_slot`] (via `ConnectionGuard`) on teardown.
fn reserve_ip_slot(counts: &Mutex<HashMap<IpAddr, usize>>, ip: IpAddr) -> IpSlot {
    if ip.is_loopback() {
        return IpSlot::Exempt;
    }
    let mut counts = counts.lock().unwrap();
    let count = counts.entry(ip).or_insert(0);
    if *count >= MAX_CONNECTIONS_PER_REMOTE_IP {
        return IpSlot::Rejected;
    }
    *count += 1;
    IpSlot::Reserved(ip)
}

/// Releases a per-IP slot previously reserved by [`reserve_ip_slot`], removing
/// the map entry once its last connection is gone so an idle server holds no
/// per-IP bookkeeping.
fn release_ip_slot(counts: &Mutex<HashMap<IpAddr, usize>>, ip: IpAddr) {
    let mut counts = counts.lock().unwrap();
    if let Some(count) = counts.get_mut(&ip) {
        *count -= 1;
        if *count == 0 {
            counts.remove(&ip);
        }
    }
}

/// Per-connection session state, gated on a successful `noa.hello`.
struct Session {
    hello_done: bool,
    header_authed: bool,
    granted_scopes: ScopeSet,
    attach_authority: String,
    attach_leases: HashMap<PaneRef, LeaseIdentity>,
}

enum ConnectionRoute {
    Control { authority: String },
    Attach,
    Invalid,
}

/// Where a [`DeadlineStream`] is in its lifecycle: bounded by an absolute
/// wall-clock deadline during the WS handshake (R-2), or past it and running
/// under the normal fixed read-poll/write-timeout pair (R-4).
enum StreamMode {
    Handshake { deadline: Instant },
    Connected,
}

/// Wraps the raw `TcpStream` so every read/write during the WS handshake is
/// bounded by an *absolute* deadline (R-2), not just a per-call idle timeout.
/// A per-call idle timeout alone never expires as long as some byte arrives
/// before each individual read/write times out — a slowloris client that
/// trickles the HTTP upgrade one byte at a time, each within
/// `HANDSHAKE_IO_TIMEOUT`, would hold the connection (and its slot) open
/// indefinitely. Each read/write here first computes the time remaining
/// until `deadline`, fails fast with `TimedOut` once none is left, and
/// otherwise arms the socket's timeout to `min(remaining, HANDSHAKE_IO_TIMEOUT)`
/// before delegating.
///
/// After a successful handshake, [`DeadlineStream::mark_connected`] switches
/// this to a plain pass-through with the connection's normal fixed
/// read-poll (50ms) and write (`WRITE_TIMEOUT`, R-4) timeouts, set once
/// rather than recomputed per call.
struct DeadlineStream {
    inner: TcpStream,
    mode: StreamMode,
}

impl DeadlineStream {
    fn new_handshake(inner: TcpStream, timeout: Duration) -> Self {
        DeadlineStream {
            inner,
            mode: StreamMode::Handshake {
                deadline: Instant::now() + timeout,
            },
        }
    }

    fn arm_handshake(&self, deadline: Instant) -> io::Result<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "handshake deadline exceeded",
            ));
        }
        let bound = remaining.min(HANDSHAKE_IO_TIMEOUT);
        self.inner.set_read_timeout(Some(bound))?;
        self.inner.set_write_timeout(Some(bound))?;
        Ok(())
    }

    /// Switches this stream from the handshake's absolute-deadline mode to
    /// the connection's normal steady-state timeouts. Called once,
    /// immediately after `accept_hdr_with_config` returns successfully.
    fn mark_connected(&mut self) -> io::Result<()> {
        self.mode = StreamMode::Connected;
        self.inner
            .set_read_timeout(Some(Duration::from_millis(50)))?;
        // R-4: a bounded write timeout for the connection's whole life, not
        // just the handshake — see `WRITE_TIMEOUT`'s doc comment.
        self.inner.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(())
    }

    fn mark_attach_connected(&mut self) -> io::Result<()> {
        self.mode = StreamMode::Connected;
        self.inner.set_read_timeout(Some(ATTACH_READ_POLL))?;
        self.inner
            .set_write_timeout(Some(crate::attach::ATTACH_BACKPRESSURE_TIMEOUT))?;
        Ok(())
    }
}

impl io::Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let StreamMode::Handshake { deadline } = self.mode {
            self.arm_handshake(deadline)?;
        }
        self.inner.read(buf)
    }
}

impl io::Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let StreamMode::Handshake { deadline } = self.mode {
            self.arm_handshake(deadline)?;
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Guarantees the two pieces of shared state a connection thread touches —
/// the accept loop's `connection_count` and the `Broadcaster`'s connection
/// registry — are released on *every* exit path out of `handle_connection`,
/// including an unwinding panic (R-4 audit): normal return, any `?`-
/// propagated I/O error, the hello-deadline close, the shutdown-flag close,
/// and a write-timeout/error. Constructed at the very top of
/// `handle_connection` (before the slot could otherwise be "half-owned")
/// with `conn_id: None`, then updated once `register_connection` succeeds;
/// `Drop` only unregisters a `Some` id, so a connection that never gets past
/// the handshake still frees its `connection_count` slot without touching a
/// broadcaster entry that was never created.
struct ConnectionGuard {
    broadcaster: Broadcaster,
    conn_id: Option<u64>,
    connection_count: Arc<AtomicUsize>,
    /// The per-IP slot this connection reserved, if any (`None` for loopback
    /// peers, which don't consume one). Released alongside the global count so
    /// a per-IP slot never leaks on any exit path — the same guarantee the
    /// `connection_count` decrement already gives (R-4 audit).
    per_ip_counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ip_slot: Option<IpAddr>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn_id) = self.conn_id {
            self.broadcaster.unregister_connection(conn_id);
        }
        if let Some(ip) = self.ip_slot {
            release_ip_slot(&self.per_ip_counts, ip);
        }
        self.connection_count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_connection(
    stream: TcpStream,
    backend: Arc<dyn IpcBackend>,
    token: Arc<String>,
    allowed_scopes: ScopeSet,
    hello_deadline: Duration,
    handshake_timeout: Duration,
    broadcaster: Broadcaster,
    shutdown: Arc<AtomicBool>,
    connection_count: Arc<AtomicUsize>,
    per_ip_counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ip_slot: Option<IpAddr>,
    attach_registry: AttachRegistry,
) -> io::Result<()> {
    let mut guard = ConnectionGuard {
        broadcaster: broadcaster.clone(),
        conn_id: None,
        connection_count,
        per_ip_counts,
        ip_slot,
    };

    stream.set_nonblocking(false)?;
    stream.set_nodelay(true).ok();
    let fallback_authority = stream.local_addr()?.to_string();

    let header_authed = Arc::new(AtomicBool::new(false));
    let header_authed_cb = header_authed.clone();
    let route = Arc::new(Mutex::new(None));
    let route_cb = route.clone();
    let token_cb = token.clone();
    let callback = move |req: &Request, response: Response| {
        let path = req.uri().path();
        let selected = if path == "/" {
            if let Some(value) = req.headers().get("Authorization")
                && let Ok(text) = value.to_str()
                && let Some(presented) = text.strip_prefix("Bearer ")
                && constant_time_eq(presented.as_bytes(), token_cb.as_bytes())
            {
                header_authed_cb.store(true, Ordering::SeqCst);
            }
            let authority = req
                .headers()
                .get("Host")
                .and_then(|value| value.to_str().ok())
                .filter(|host| !host.is_empty())
                .unwrap_or(&fallback_authority)
                .to_string();
            ConnectionRoute::Control { authority }
        } else if path == ATTACH_PATH && req.uri().query().is_none() {
            ConnectionRoute::Attach
        } else {
            ConnectionRoute::Invalid
        };
        *route_cb.lock().unwrap() = Some(selected);
        Ok(response)
    };

    // R-2: the handshake itself is now bounded by an absolute deadline via
    // `DeadlineStream`, not just a per-read idle timeout — see its doc
    // comment for why that distinction matters against a slowloris client.
    let deadline_stream = DeadlineStream::new_handshake(stream, handshake_timeout);
    let mut ws = tungstenite::accept_hdr_with_config(
        deadline_stream,
        callback,
        Some(connection_ws_config()),
    )
    .map_err(|err| io::Error::other(err.to_string()))?;
    let selected = route
        .lock()
        .unwrap()
        .take()
        .unwrap_or(ConnectionRoute::Invalid);
    match selected {
        ConnectionRoute::Control { authority } => {
            ws.get_mut().mark_connected()?;
            let (conn_id, queue) = broadcaster.register_connection();
            guard.conn_id = Some(conn_id);
            let mut session = Session {
                hello_done: false,
                header_authed: header_authed.load(Ordering::SeqCst),
                granted_scopes: ScopeSet::empty(),
                attach_authority: authority,
                attach_leases: HashMap::new(),
            };
            let connected_at = Instant::now();
            run_connection_loop(
                &mut ws,
                &backend,
                &token,
                allowed_scopes,
                &broadcaster,
                &attach_registry,
                conn_id,
                &queue,
                &mut session,
                &shutdown,
                connected_at,
                hello_deadline,
            )
        }
        ConnectionRoute::Attach => {
            ws.get_mut().mark_attach_connected()?;
            run_attach_connection(&mut ws, &backend, &attach_registry, &shutdown)
        }
        ConnectionRoute::Invalid => close_policy(&mut ws, "invalid websocket path"),
    }
    // `guard` drops here on every path above (including the `?`s earlier in
    // this function and any error `run_connection_loop` returns),
    // unregistering the broadcaster connection and decrementing
    // `connection_count`.
}

#[allow(clippy::too_many_arguments)]
fn run_connection_loop(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    backend: &Arc<dyn IpcBackend>,
    token: &str,
    allowed_scopes: ScopeSet,
    broadcaster: &Broadcaster,
    attach_registry: &AttachRegistry,
    conn_id: u64,
    queue: &Arc<PushQueue>,
    session: &mut Session,
    shutdown: &Arc<AtomicBool>,
    connected_at: Instant,
    hello_deadline: Duration,
) -> io::Result<()> {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        // R-1: checked at the top of *every* iteration, before any read is
        // attempted — not just inside the read-timeout branch below. An
        // unauthenticated client that keeps sending well-formed
        // (non-`noa.hello`) requests fast enough that `ws.read()` always
        // returns `Ok` before the 50ms poll ever times out previously never
        // hit this check at all.
        if !session.hello_done && connected_at.elapsed() >= hello_deadline {
            return Ok(());
        }

        for item in queue.drain() {
            let text = serde_json::to_string(&to_notification(item)).unwrap_or_default();
            ws.send(Message::Text(text)).map_err(ws_err_to_io)?;
        }

        match ws.read() {
            Ok(Message::Text(text)) => {
                if let Some(response) = dispatch(
                    &text,
                    backend,
                    token,
                    allowed_scopes,
                    broadcaster,
                    attach_registry,
                    conn_id,
                    session,
                ) {
                    ws.send(Message::Text(response)).map_err(ws_err_to_io)?;
                }
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).map_err(ws_err_to_io)?;
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(err))
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                // Plain 50ms read-poll timeout (or a write timing out, R-4) —
                // the hello-deadline check now lives at the top of the loop,
                // so this arm only needs to keep polling.
                continue;
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(err) => return Err(ws_err_to_io(err)),
        }
    }
}

struct ActiveAttachGuard {
    registry: AttachRegistry,
    backend: Arc<dyn IpcBackend>,
    identity: LeaseIdentity,
}

impl Drop for ActiveAttachGuard {
    fn drop(&mut self) {
        if self.registry.release_generation(self.identity) {
            let _ = self
                .backend
                .detach_attach(self.identity.pane, self.identity.generation);
        }
    }
}

fn run_attach_connection(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    backend: &Arc<dyn IpcBackend>,
    registry: &AttachRegistry,
    shutdown: &Arc<AtomicBool>,
) -> io::Result<()> {
    let Some(identity) = authenticate_attach(ws, registry, shutdown)? else {
        return Ok(());
    };
    let _lease = ActiveAttachGuard {
        registry: registry.clone(),
        backend: backend.clone(),
        identity,
    };

    let (output, receiver) = output_channel();
    // The backend performs raw-tap registration and seed snapshot atomically.
    // Only after it returns do we touch the socket, so a Terminal lock held by
    // the application can never include a WebSocket write.
    let seed = match backend.open_attach(identity.pane, identity.generation, output) {
        Ok(seed) => seed,
        Err(_) => return close_attach_failure(ws),
    };
    send_attach_seed(ws, &seed)?;

    run_raw_loop(ws, backend, registry, shutdown, identity, receiver)
}

fn authenticate_attach(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    registry: &AttachRegistry,
    shutdown: &Arc<AtomicBool>,
) -> io::Result<Option<LeaseIdentity>> {
    let deadline = Instant::now() + crate::attach::ATTACH_TOKEN_TTL;
    loop {
        if shutdown.load(Ordering::SeqCst) || Instant::now() >= deadline {
            close_attach_failure(ws)?;
            return Ok(None);
        }
        match ws.read() {
            Ok(Message::Binary(presented)) => {
                return match registry.authenticate(&presented) {
                    Ok(identity) => Ok(Some(identity)),
                    Err(_) => {
                        close_attach_failure(ws)?;
                        Ok(None)
                    }
                };
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).map_err(ws_err_to_io)?;
            }
            Ok(Message::Close(_)) => return Ok(None),
            Ok(_) => {
                close_attach_failure(ws)?;
                return Ok(None);
            }
            Err(tungstenite::Error::Io(err))
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(None);
            }
            Err(err) => return Err(ws_err_to_io(err)),
        }
    }
}

fn run_raw_loop(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    backend: &Arc<dyn IpcBackend>,
    registry: &AttachRegistry,
    shutdown: &Arc<AtomicBool>,
    identity: LeaseIdentity,
    receiver: AttachOutputReceiver,
) -> io::Result<()> {
    loop {
        if shutdown.load(Ordering::SeqCst) || !registry.is_active(identity) {
            return Ok(());
        }

        if !drain_attach_output(&receiver, |bytes| send_attach_binary(ws, &bytes))? {
            return Ok(());
        }

        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if backend
                    .write_attach(identity.pane, identity.generation, &bytes)
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).map_err(ws_err_to_io)?;
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {
                close_attach_failure(ws)?;
                return Ok(());
            }
            Err(tungstenite::Error::Io(err))
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(err) => return Err(ws_err_to_io(err)),
        }
    }
}

/// Send one logical raw byte stream as frame-safe Binary messages. WebSocket
/// message boundaries are not PTY boundaries; receivers concatenate bytes in
/// arrival order.
fn send_attach_binary(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    bytes: &[u8],
) -> io::Result<()> {
    for chunk in bytes.chunks(ATTACH_BINARY_CHUNK_BYTES) {
        ws.send(Message::Binary(chunk.to_vec()))
            .map_err(ws_err_to_io)?;
    }
    Ok(())
}

/// The initial seed is a bounded sequence of Binary chunks followed by one
/// empty Binary message. The terminator is unambiguous because the raw output
/// loop starts only after this function returns.
fn send_attach_seed(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    seed: &[u8],
) -> io::Result<()> {
    send_attach_binary(ws, seed)?;
    ws.send(Message::Binary(Vec::new())).map_err(ws_err_to_io)
}

fn drain_attach_output(
    receiver: &AttachOutputReceiver,
    mut send: impl FnMut(Vec<u8>) -> io::Result<()>,
) -> io::Result<bool> {
    let mut sent_bytes = 0usize;
    while sent_bytes < ATTACH_OUTPUT_BYTES_PER_POLL {
        match receiver.try_recv() {
            Ok(bytes) => {
                sent_bytes = sent_bytes.saturating_add(bytes.len());
                send(bytes)?;
            }
            Err(AttachTryRecvError::Empty) => return Ok(true),
            Err(AttachTryRecvError::Closed) => return Ok(false),
        }
    }
    Ok(true)
}

fn close_attach_failure(ws: &mut tungstenite::WebSocket<DeadlineStream>) -> io::Result<()> {
    close_policy(ws, ATTACH_HANDSHAKE_CLOSE_REASON)
}

fn close_policy(
    ws: &mut tungstenite::WebSocket<DeadlineStream>,
    reason: &'static str,
) -> io::Result<()> {
    let frame = CloseFrame {
        code: CloseCode::Policy,
        reason: Cow::Borrowed(reason),
    };
    match ws.close(Some(frame)) {
        Ok(()) | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
            Ok(())
        }
        Err(err) => Err(ws_err_to_io(err)),
    }
}

fn ws_err_to_io(err: tungstenite::Error) -> io::Error {
    io::Error::other(err.to_string())
}

fn to_notification(item: QueuedNotification) -> Value {
    match item {
        QueuedNotification::StateChanged { panels, dropped } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "noa.stateChanged",
            "params": StateChangedParams { panels, dropped },
        }),
        QueuedNotification::Output {
            pane_id,
            coordinate_generation,
            lines,
            dropped,
        } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "noa.output",
            "params": OutputParams {
                pane_id: WireId(pane_id),
                coordinate_generation,
                lines,
                dropped,
            },
        }),
    }
}

// ---- request/response envelope ----

#[derive(Clone, Debug, serde::Deserialize)]
struct RpcRequest {
    /// Present so a missing `jsonrpc` field still parses (defaults to
    /// `Value::Null`, which fails the `== "2.0"` check below) rather than
    /// erroring out at `serde_json::from_str` with a generic parse error —
    /// R-5 wants a missing field and a wrong version to both reach the same
    /// -32600 `InvalidRequest` path with a specific message.
    #[serde(default)]
    jsonrpc: Value,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

const JSONRPC_VERSION: &str = "2.0";

struct RpcFail {
    code: ErrorCode,
    message: String,
}

impl RpcFail {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        RpcFail {
            code,
            message: message.into(),
        }
    }

    fn invalid_params() -> Self {
        RpcFail::new(ErrorCode::InvalidParams, "invalid params")
    }
}

impl From<IpcError> for RpcFail {
    fn from(err: IpcError) -> Self {
        RpcFail::new(err.code(), err.to_string())
    }
}

fn success_response(id: Value, result: Value) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: ErrorCode, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code.code(), "message": message },
    })
    .to_string()
}

fn require_scope(session: &Session, scope: Scope) -> Result<(), RpcFail> {
    if session.granted_scopes.contains(scope) {
        Ok(())
    } else {
        Err(RpcFail::new(
            ErrorCode::ScopeDenied,
            format!("missing scope: {}", scope.as_str()),
        ))
    }
}

/// Parses and dispatches one request, returning the response text to send
/// (or `None` — never happens today, every request gets a response, but the
/// signature stays permissive for future fire-and-forget notifications).
#[allow(clippy::too_many_arguments)]
fn dispatch(
    raw: &str,
    backend: &Arc<dyn IpcBackend>,
    token: &str,
    allowed_scopes: ScopeSet,
    broadcaster: &Broadcaster,
    attach_registry: &AttachRegistry,
    conn_id: u64,
    session: &mut Session,
) -> Option<String> {
    // R-4: syntactically-invalid JSON is -32700 ParseError; syntactically
    // valid JSON that isn't a well-formed request object (not an object at
    // all, a batch array, or missing/wrong-typed `method`) is -32600
    // InvalidRequest. Two stages so each gets its own code instead of both
    // collapsing into ParseError via a single `from_str::<RpcRequest>`.
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            return Some(error_response(
                Value::Null,
                ErrorCode::ParseError,
                "parse error",
            ));
        }
    };
    let req: RpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(_) => {
            return Some(error_response(
                Value::Null,
                ErrorCode::InvalidRequest,
                "invalid request",
            ));
        }
    };
    let id = req.id.clone();

    // R-3: the public spec pins `id` to `number | string` (§2 JSON-RPC
    // 規約). Anything else — missing (defaults to `Value::Null` above),
    // `null`, an object, an array, or a bool — must fail closed with
    // -32600 InvalidRequest before dispatch, including side-effecting
    // methods like `sendText`/`closePane`; echo `id: null` since the
    // offending id itself isn't a valid response id. Checked before
    // `noa.hello` too, so a bad id can't even complete auth.
    if !matches!(id, Value::Number(_) | Value::String(_)) {
        return Some(error_response(
            Value::Null,
            ErrorCode::InvalidRequest,
            "id must be a number or string",
        ));
    }

    // R-5: reject anything but exactly "2.0" (missing counts as invalid)
    // before any dispatch — including `noa.hello` itself (pre-auth) and
    // every other method (post-auth). The connection stays open; only this
    // one request is rejected, mirroring every other per-request error path
    // here.
    if req.jsonrpc.as_str() != Some(JSONRPC_VERSION) {
        return Some(error_response(
            id,
            ErrorCode::InvalidRequest,
            "missing or unsupported jsonrpc version, expected \"2.0\"",
        ));
    }

    if req.method == "noa.hello" {
        return Some(handle_hello(id, req.params, token, allowed_scopes, session));
    }

    if !session.hello_done {
        return Some(error_response(
            id,
            ErrorCode::Auth,
            "noa.hello required before other methods",
        ));
    }

    let outcome = (|| -> Result<Value, RpcFail> {
        match req.method.as_str() {
            "noa.listPanels" => {
                require_scope(session, Scope::Read)?;
                Ok(serde_json::to_value(ListPanelsResult {
                    panels: backend.list_panels(),
                })
                .unwrap())
            }
            "noa.getText" => {
                require_scope(session, Scope::Read)?;
                handle_get_text(backend, req.params)
            }
            "noa.getGrid" => {
                require_scope(session, Scope::Read)?;
                handle_get_grid(backend, req.params)
            }
            "noa.sendText" => {
                require_scope(session, Scope::Input)?;
                handle_send_text(backend, req.params)
            }
            "noa.focusPane" => {
                require_scope(session, Scope::Control)?;
                handle_focus_pane(backend, req.params)
            }
            "noa.newTab" => {
                require_scope(session, Scope::Control)?;
                handle_new_tab(backend, req.params)
            }
            "noa.split" => {
                require_scope(session, Scope::Control)?;
                handle_split(backend, req.params)
            }
            "noa.closePane" => {
                require_scope(session, Scope::Control)?;
                handle_close_pane(backend, attach_registry, req.params)
            }
            "noa.attach" => {
                require_scope(session, Scope::Attach)?;
                handle_attach(backend, attach_registry, session, req.params)
            }
            "noa.detach" => {
                require_scope(session, Scope::Attach)?;
                handle_detach(backend, attach_registry, session, req.params)
            }
            "noa.resizePane" => {
                require_scope(session, Scope::Attach)?;
                handle_resize_pane(backend, attach_registry, session, req.params)
            }
            "noa.subscribe" => {
                require_scope(session, Scope::Read)?;
                handle_subscribe(broadcaster, conn_id, req.params)
            }
            "noa.unsubscribe" => {
                require_scope(session, Scope::Read)?;
                handle_unsubscribe(broadcaster, conn_id, req.params)
            }
            _ => Err(RpcFail::new(
                ErrorCode::MethodNotFound,
                format!("unknown method: {}", req.method),
            )),
        }
    })();

    Some(match outcome {
        Ok(result) => success_response(id, result),
        Err(fail) => error_response(id, fail.code, &fail.message),
    })
}

fn handle_hello(
    id: Value,
    params: Value,
    token: &str,
    allowed_scopes: ScopeSet,
    session: &mut Session,
) -> String {
    let hello: HelloParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(_) => return error_response(id, ErrorCode::InvalidParams, "invalid params"),
    };

    if hello.protocol_version != PROTOCOL_VERSION {
        return error_response(id, ErrorCode::VersionMismatch, "protocol version mismatch");
    }

    if !session.header_authed {
        match &hello.token {
            Some(presented) if constant_time_eq(presented.as_bytes(), token.as_bytes()) => {}
            _ => return error_response(id, ErrorCode::Auth, "invalid token"),
        }
    }

    session.hello_done = true;
    let requested = ScopeSet::from_strings(&hello.scopes);
    session.granted_scopes = requested.intersection(allowed_scopes);

    let result = HelloResult {
        protocol_version: PROTOCOL_VERSION,
        granted_scopes: session.granted_scopes.to_strings(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    success_response(id, serde_json::to_value(result).unwrap())
}

fn handle_get_text(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: GetTextParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    // R-1: clamp, don't reject — an over-large maxBytes is capped to
    // MAX_TEXT_MAX_BYTES before it ever reaches the backend, so an
    // authenticated client can't force an unbounded scrollback walk under
    // the terminal lock (NFR-4). Tail-truncation + `truncated: true` below
    // already communicates that the response is partial.
    let max_bytes = p
        .max_bytes
        .unwrap_or(DEFAULT_TEXT_MAX_BYTES)
        .min(MAX_TEXT_MAX_BYTES);
    let backend_result = backend.get_text(p.pane_id.0, p.source, max_bytes)?;
    let (text, truncated_here) = truncate_tail(&backend_result.text, max_bytes);
    let result = GetTextResult {
        pane_id: p.pane_id,
        text,
        truncated: truncated_here || backend_result.truncated,
    };
    Ok(serde_json::to_value(result).unwrap())
}

fn handle_get_grid(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: GetGridParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let backend_result = backend.get_grid(p.pane_id.0, p.start_row, p.row_count)?;
    let backend_has_more = backend_result.has_more;
    let (rows, has_more) =
        cap_grid_rows(backend_result.rows, DEFAULT_GRID_CAP_BYTES).map_err(|GridCapExceeded| {
            RpcFail::new(ErrorCode::PayloadTooLarge, "row exceeds response cap")
        })?;
    let has_more = has_more || backend_has_more;
    let result = GetGridResult {
        pane_id: p.pane_id,
        cols: backend_result.cols,
        start_row: p.start_row,
        coordinate_generation: backend_result.coordinate_generation,
        oldest_row: backend_result.oldest_row,
        next_row: backend_result.next_row,
        rows,
        has_more,
    };
    Ok(serde_json::to_value(result).unwrap())
}

fn handle_send_text(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: SendTextParams =
        serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.send_text(p.pane_id.0, &p.text, p.paste.unwrap_or(true))?;
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_focus_pane(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: PaneIdParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.focus_pane(p.pane_id.0)?;
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_new_tab(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: NewTabParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let pane_id = backend.new_tab(p.window_id.map(|w| w.0))?;
    Ok(serde_json::to_value(PaneIdResult {
        pane_id: WireId(pane_id),
    })
    .unwrap())
}

fn handle_split(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: SplitParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let pane_id = backend.split(p.pane_id.0, p.direction)?;
    Ok(serde_json::to_value(PaneIdResult {
        pane_id: WireId(pane_id),
    })
    .unwrap())
}

fn handle_close_pane(
    backend: &Arc<dyn IpcBackend>,
    attach_registry: &AttachRegistry,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: PaneIdParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.close_pane(p.pane_id.0)?;
    if let Some(identity) = attach_registry.release_pane(p.pane_id.0) {
        let _ = backend.detach_attach(identity.pane, identity.generation);
    }
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_attach(
    backend: &Arc<dyn IpcBackend>,
    attach_registry: &AttachRegistry,
    session: &mut Session,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: AttachParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.validate_attach(p.pane_id.0)?;
    let reservation = attach_registry
        .reserve(p.pane_id.0)
        .map_err(|err| match err {
            ReserveError::Conflict => RpcFail::new(
                ErrorCode::AttachConflict,
                "pane already has an attach lease",
            ),
        })?;
    session
        .attach_leases
        .insert(p.pane_id.0, reservation.identity);
    let attach_url = format!("ws://{}{ATTACH_PATH}", session.attach_authority);
    Ok(serde_json::to_value(AttachResult {
        attach_token: reservation.token,
        attach_url,
    })
    .unwrap())
}

fn handle_detach(
    backend: &Arc<dyn IpcBackend>,
    attach_registry: &AttachRegistry,
    session: &mut Session,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: DetachParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    if let Some(identity) = session.attach_leases.remove(&p.pane_id.0)
        && attach_registry.release_generation(identity)
    {
        backend.detach_attach(identity.pane, identity.generation)?;
    }
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_resize_pane(
    backend: &Arc<dyn IpcBackend>,
    attach_registry: &AttachRegistry,
    session: &Session,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: ResizePaneParams =
        serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    if p.cols == 0
        || p.rows == 0
        || p.cols > MAX_RESIZE_COLS
        || p.rows > MAX_RESIZE_ROWS
        || u32::from(p.cols)
            .checked_mul(u32::from(p.rows))
            .is_none_or(|cells| cells > MAX_RESIZE_CELLS)
    {
        return Err(RpcFail::invalid_params());
    }
    let identity = session
        .attach_leases
        .get(&p.pane_id.0)
        .copied()
        .ok_or_else(|| {
            RpcFail::new(
                ErrorCode::AttachConflict,
                "control session does not own the attach lease",
            )
        })?;
    let resize = attach_registry.with_current_lease(identity, || {
        backend.resize_pane(p.pane_id.0, p.cols, p.rows)
    });
    let Some(resize) = resize else {
        return Err(RpcFail::new(
            ErrorCode::AttachConflict,
            "control session does not own the current attach lease",
        ));
    };
    resize?;
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_subscribe(
    broadcaster: &Broadcaster,
    conn_id: u64,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: SubscribeParams =
        serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let mask = EventMask::from_events(&p.events);
    let pane_ids = p
        .pane_ids
        .map(|ids| ids.into_iter().map(|id| id.0).collect::<HashSet<u64>>());
    let sub_id = broadcaster
        .add_subscription(conn_id, mask, pane_ids)
        .map_err(|err| match err {
            AddSubscriptionError::ConnectionNotFound => {
                RpcFail::new(ErrorCode::InvalidRequest, "connection not registered")
            }
            // R-2: cap on subscriptions per connection — the connection stays
            // open, this one `noa.subscribe` call just fails.
            AddSubscriptionError::LimitExceeded => {
                RpcFail::new(ErrorCode::PayloadTooLarge, "subscription limit exceeded")
            }
        })?;
    Ok(serde_json::to_value(SubscribeResult {
        subscription_id: WireId(sub_id),
    })
    .unwrap())
}

fn handle_unsubscribe(
    broadcaster: &Broadcaster,
    conn_id: u64,
    params: Value,
) -> Result<Value, RpcFail> {
    let p: UnsubscribeParams =
        serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    broadcaster.remove_subscription(conn_id, p.subscription_id.0);
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_output_drain_yields_to_input_after_a_bounded_byte_turn() {
        let (sender, receiver) = crate::attach::output_channel();
        for _ in 0..128 {
            sender.send(vec![b'x'; 1024]).unwrap();
        }

        let mut frames = 0usize;
        let mut bytes = 0usize;
        assert!(
            drain_attach_output(&receiver, |frame| {
                frames += 1;
                bytes += frame.len();
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(bytes, ATTACH_OUTPUT_BYTES_PER_POLL);
        assert_eq!(frames, 64);
        assert!(
            receiver.try_recv().is_ok(),
            "the next turn retains queued output"
        );
    }
    use std::net::TcpListener;

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    // ---- R-2: DeadlineStream enforces an absolute handshake deadline ----

    #[test]
    fn deadline_stream_read_fails_fast_once_the_absolute_deadline_has_passed() {
        let (_client, server) = tcp_pair();
        let mut stream = DeadlineStream::new_handshake(server, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(20));
        let mut buf = [0u8; 8];
        let err = io::Read::read(&mut stream, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn deadline_stream_write_fails_fast_once_the_absolute_deadline_has_passed() {
        let (_client, server) = tcp_pair();
        let mut stream = DeadlineStream::new_handshake(server, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(20));
        let err = io::Write::write(&mut stream, b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn deadline_stream_arms_the_socket_timeout_to_the_remaining_deadline_capped_by_handshake_io_timeout()
     {
        let (_client, server) = tcp_pair();
        let stream = DeadlineStream::new_handshake(server, Duration::from_millis(50));
        // Plenty of deadline left, but the per-call bound is still capped at
        // `HANDSHAKE_IO_TIMEOUT`, never the other way around.
        stream
            .arm_handshake(Instant::now() + Duration::from_secs(60))
            .unwrap();
        let bound = stream.inner.read_timeout().unwrap().unwrap();
        assert!(bound <= HANDSHAKE_IO_TIMEOUT);
    }

    // ---- R-4: post-handshake timeouts + exit-path cleanup guarantee ----

    #[test]
    fn deadline_stream_mark_connected_applies_the_steady_state_timeouts() {
        let (_client, server) = tcp_pair();
        let mut stream = DeadlineStream::new_handshake(server, Duration::from_secs(5));
        stream.mark_connected().unwrap();
        assert_eq!(
            stream.inner.read_timeout().unwrap(),
            Some(Duration::from_millis(50))
        );
        assert_eq!(stream.inner.write_timeout().unwrap(), Some(WRITE_TIMEOUT));
    }

    #[test]
    fn connection_guard_decrements_connection_count_on_drop_even_with_no_registered_conn_id() {
        // Covers an exit path before `register_connection` ever ran (e.g. an
        // I/O error during the handshake) — the accept loop's slot must
        // still be freed even though there's no broadcaster entry to remove.
        let broadcaster = Broadcaster::new();
        let connection_count = Arc::new(AtomicUsize::new(1));
        {
            let _guard = ConnectionGuard {
                broadcaster: broadcaster.clone(),
                conn_id: None,
                connection_count: connection_count.clone(),
                per_ip_counts: Arc::new(Mutex::new(HashMap::new())),
                ip_slot: None,
            };
        }
        assert_eq!(connection_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn connection_guard_unregisters_the_broadcaster_connection_on_drop() {
        let broadcaster = Broadcaster::new();
        let (conn_id, _queue) = broadcaster.register_connection();
        assert_eq!(broadcaster.connection_count(), 1);
        let connection_count = Arc::new(AtomicUsize::new(1));
        {
            let _guard = ConnectionGuard {
                broadcaster: broadcaster.clone(),
                conn_id: Some(conn_id),
                connection_count: connection_count.clone(),
                per_ip_counts: Arc::new(Mutex::new(HashMap::new())),
                ip_slot: None,
            };
        }
        assert_eq!(broadcaster.connection_count(), 0);
        assert_eq!(connection_count.load(Ordering::SeqCst), 0);
    }

    // ---- per-remote-IP connection cap (LAN-exposure hardening) ----

    #[test]
    fn loopback_peers_are_exempt_from_the_per_ip_cap() {
        let counts = Mutex::new(HashMap::new());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        // Far more than the cap — every one is admitted and nothing is
        // recorded, so the default loopback deployment keeps the full pool.
        for _ in 0..MAX_CONNECTIONS_PER_REMOTE_IP * 4 {
            assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Exempt));
        }
        assert!(counts.lock().unwrap().is_empty());
    }

    #[test]
    fn a_non_loopback_source_is_capped_and_a_released_slot_is_reusable() {
        let counts = Mutex::new(HashMap::new());
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        for _ in 0..MAX_CONNECTIONS_PER_REMOTE_IP {
            assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Reserved(_)));
        }
        // The (cap + 1)-th concurrent connection from the same host is refused.
        assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Rejected));

        // Freeing one slot lets the host connect again.
        release_ip_slot(&counts, ip);
        assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Reserved(_)));
    }

    #[test]
    fn client_mode_scrollback_peak_fits_the_per_ip_cap() {
        let counts = Mutex::new(HashMap::new());
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        for _ in 0..4 * PEAK_CONNECTIONS_PER_REMOTE_PANE {
            assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Reserved(_)));
        }
    }

    #[test]
    fn one_source_hitting_its_cap_does_not_block_a_different_source() {
        let counts = Mutex::new(HashMap::new());
        let hog: IpAddr = "192.168.1.50".parse().unwrap();
        for _ in 0..MAX_CONNECTIONS_PER_REMOTE_IP {
            let _ = reserve_ip_slot(&counts, hog);
        }
        assert!(matches!(reserve_ip_slot(&counts, hog), IpSlot::Rejected));

        let other: IpAddr = "192.168.1.51".parse().unwrap();
        assert!(matches!(
            reserve_ip_slot(&counts, other),
            IpSlot::Reserved(_)
        ));
    }

    // ---- shutdown hardening: the accept thread must never leak ----

    /// Minimal backend for lifecycle-only tests: no RPC is ever dispatched.
    struct NoopBackend;
    impl IpcBackend for NoopBackend {
        fn list_panels(&self) -> Vec<Panel> {
            Vec::new()
        }
        fn get_text(
            &self,
            _pane: PaneRef,
            _source: crate::protocol::TextSource,
            _max_bytes: usize,
        ) -> Result<crate::backend::TextResult, IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn get_grid(
            &self,
            _pane: PaneRef,
            _start_row: u64,
            _row_count: u64,
        ) -> Result<crate::backend::GridResult, IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn send_text(&self, _pane: PaneRef, _text: &str, _paste: bool) -> Result<(), IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn focus_pane(&self, _pane: PaneRef) -> Result<(), IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn new_tab(&self, _window: Option<crate::backend::WindowRef>) -> Result<PaneRef, IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn split(
            &self,
            _pane: PaneRef,
            _direction: crate::protocol::SplitDirection,
        ) -> Result<PaneRef, IpcError> {
            Err(IpcError::PaneClosed)
        }
        fn close_pane(&self, _pane: PaneRef) -> Result<(), IpcError> {
            Err(IpcError::PaneClosed)
        }
    }

    /// The wake-failure path (`stop_accept_thread` with an unreachable wake
    /// address, standing in for an exhausted fd table or filtered loopback)
    /// must still unblock the parked `accept()` — by force-closing the
    /// listening socket — and reap the thread within the bounded join,
    /// instead of logging and leaking it (the pre-hardening behavior).
    #[test]
    fn wake_failure_force_closes_the_listener_and_reaps_the_accept_thread() {
        let mut handle = Server::start(
            ServerConfig {
                port: 0,
                bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
                token: "lifecycle-test-token".to_string(),
                allowed_scopes: ScopeSet::empty(),
                hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
                handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
            },
            Arc::new(NoopBackend),
            Broadcaster::new(),
        )
        .expect("server should bind an ephemeral loopback port");

        // A loopback port that refuses connections: bind an ephemeral
        // listener, note its port, and drop it before connecting.
        let dead_port = {
            let probe = TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let dead_addr =
            std::net::SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), dead_port);

        assert!(
            handle.stop_accept_thread(dead_addr),
            "wake failure must fall back to closing the listener so the \
             accept thread exits and joins within the bounded timeout"
        );
        // `Drop` runs after `accept_thread` was taken — a no-op second stop.
    }

    /// The normal shutdown path (reachable wake address) still reaps the
    /// thread — guarding the refactor of `Drop` into `stop_accept_thread`.
    #[test]
    fn normal_shutdown_wakes_and_reaps_the_accept_thread() {
        let mut handle = Server::start(
            ServerConfig {
                port: 0,
                bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
                token: "lifecycle-test-token".to_string(),
                allowed_scopes: ScopeSet::empty(),
                hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
                handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
            },
            Arc::new(NoopBackend),
            Broadcaster::new(),
        )
        .expect("server should bind an ephemeral loopback port");
        let wake_addr = std::net::SocketAddr::new(handle.bind_addr(), handle.port());
        assert!(handle.stop_accept_thread(wake_addr));
    }

    #[test]
    fn releasing_the_last_slot_removes_the_ip_bookkeeping_entry() {
        let counts = Mutex::new(HashMap::new());
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        assert!(matches!(reserve_ip_slot(&counts, ip), IpSlot::Reserved(_)));
        release_ip_slot(&counts, ip);
        assert!(
            counts.lock().unwrap().is_empty(),
            "no idle per-IP entry should linger"
        );
    }
}
