//! The JSON-RPC-over-WebSocket server (spec §L2 "Transport & Handshake",
//! sync tungstenite + thread-per-connection + crossbeam, no async runtime —
//! NFR-3).

use std::collections::HashSet;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::protocol::WebSocketConfig;

/// Hard cap on concurrent connections (F-3 / DoS bound): a proxy flood
/// (Omen ④) closes excess accepts immediately rather than spawning an
/// unbounded number of connection threads.
const MAX_CONNECTIONS: usize = 32;

/// Bounds on a single incoming WebSocket frame/message (F-3 / DoS bound),
/// well above the largest legitimate request (`noa.sendText`'s text isn't
/// otherwise capped, but a well-behaved client's paste-sized input is far
/// under this) and far below tungstenite's 64 MiB/16 MiB defaults.
const MAX_WS_MESSAGE_SIZE: usize = 1024 * 1024;
const MAX_WS_FRAME_SIZE: usize = 256 * 1024;

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

fn connection_ws_config() -> WebSocketConfig {
    #[allow(deprecated)]
    WebSocketConfig {
        max_send_queue: None,
        write_buffer_size: 128 * 1024,
        max_write_buffer_size: usize::MAX,
        max_message_size: Some(MAX_WS_MESSAGE_SIZE),
        max_frame_size: Some(MAX_WS_FRAME_SIZE),
        accept_unmasked_frames: false,
    }
}

use crate::auth::{Scope, ScopeSet, constant_time_eq};
use crate::backend::IpcBackend;
use crate::error::{ErrorCode, IpcError};
use crate::protocol::*;
use crate::push::{AddSubscriptionError, Broadcaster, EventMask, PushQueue, QueuedNotification};

/// Server startup configuration (spec §L2 "Config キー").
pub struct ServerConfig {
    /// `0` binds an OS-assigned ephemeral port (tests use this; production
    /// uses config `server-port`, default `61771`).
    pub port: u16,
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
}

/// A handle to a running server. Dropping it stops the accept loop and
/// closes the listener.
pub struct ServerHandle {
    port: u16,
    shutdown: Arc<AtomicBool>,
    broadcaster: Broadcaster,
    accept_thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn broadcaster(&self) -> Broadcaster {
        self.broadcaster.clone()
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

pub struct Server;

impl Server {
    /// Binds `127.0.0.1:<port>` only (FR-2) and starts the accept loop.
    /// Never binds a non-loopback interface.
    ///
    /// `broadcaster` is supplied by the caller rather than created here so a
    /// long-lived registry can outlive any one `Server::start`/`ServerHandle`
    /// drop cycle (e.g. `noa-app`'s config-reload server restart): panes
    /// wired to the same `Broadcaster` before a restart keep pushing to
    /// whichever server currently owns its connections, without needing to
    /// be re-wired.
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
        let listener = TcpListener::bind(("127.0.0.1", config.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let shutdown = Arc::new(AtomicBool::new(false));
        let token = Arc::new(config.token);
        let allowed_scopes = config.allowed_scopes;
        let hello_deadline = config.hello_deadline;
        let handshake_timeout = config.handshake_timeout;
        let connection_count = Arc::new(AtomicUsize::new(0));

        let shutdown_loop = shutdown.clone();
        let broadcaster_loop = broadcaster.clone();
        let accept_thread = thread::spawn(move || {
            while !shutdown_loop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        // Refuse excess connections by closing immediately
                        // rather than spawning a thread for them (F-3).
                        if connection_count.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                            connection_count.fetch_sub(1, Ordering::SeqCst);
                            drop(stream);
                            continue;
                        }
                        let backend = backend.clone();
                        let token = token.clone();
                        let broadcaster = broadcaster_loop.clone();
                        let shutdown_conn = shutdown_loop.clone();
                        let connection_count = connection_count.clone();
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
                            ) {
                                log::debug!("noa-ipc: connection ended: {err}");
                            }
                        });
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(err) => {
                        log::warn!("noa-ipc: accept error: {err}");
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });

        Ok(ServerHandle { port, shutdown, broadcaster, accept_thread: Some(accept_thread) })
    }
}

/// Per-connection session state, gated on a successful `noa.hello`.
struct Session {
    hello_done: bool,
    header_authed: bool,
    granted_scopes: ScopeSet,
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
        DeadlineStream { inner, mode: StreamMode::Handshake { deadline: Instant::now() + timeout } }
    }

    fn arm_handshake(&self, deadline: Instant) -> io::Result<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "handshake deadline exceeded"));
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
        self.inner.set_read_timeout(Some(Duration::from_millis(50)))?;
        // R-4: a bounded write timeout for the connection's whole life, not
        // just the handshake — see `WRITE_TIMEOUT`'s doc comment.
        self.inner.set_write_timeout(Some(WRITE_TIMEOUT))?;
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
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn_id) = self.conn_id {
            self.broadcaster.unregister_connection(conn_id);
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
) -> io::Result<()> {
    let mut guard = ConnectionGuard { broadcaster: broadcaster.clone(), conn_id: None, connection_count };

    stream.set_nonblocking(false)?;
    stream.set_nodelay(true).ok();

    let header_authed = Arc::new(AtomicBool::new(false));
    let header_authed_cb = header_authed.clone();
    let token_cb = token.clone();
    let callback = move |req: &Request, response: Response| {
        if let Some(value) = req.headers().get("Authorization")
            && let Ok(text) = value.to_str()
            && let Some(presented) = text.strip_prefix("Bearer ")
            && constant_time_eq(presented.as_bytes(), token_cb.as_bytes())
        {
            header_authed_cb.store(true, Ordering::SeqCst);
        }
        Ok(response)
    };

    // R-2: the handshake itself is now bounded by an absolute deadline via
    // `DeadlineStream`, not just a per-read idle timeout — see its doc
    // comment for why that distinction matters against a slowloris client.
    let deadline_stream = DeadlineStream::new_handshake(stream, handshake_timeout);
    let mut ws = tungstenite::accept_hdr_with_config(deadline_stream, callback, Some(connection_ws_config()))
        .map_err(|err| io::Error::other(err.to_string()))?;
    ws.get_mut().mark_connected()?;

    let (conn_id, queue) = broadcaster.register_connection();
    guard.conn_id = Some(conn_id);
    let mut session = Session {
        hello_done: false,
        header_authed: header_authed.load(Ordering::SeqCst),
        granted_scopes: ScopeSet::empty(),
    };

    let connected_at = Instant::now();
    run_connection_loop(
        &mut ws,
        &backend,
        &token,
        allowed_scopes,
        &broadcaster,
        conn_id,
        &queue,
        &mut session,
        &shutdown,
        connected_at,
        hello_deadline,
    )
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
                if let Some(response) = dispatch(&text, backend, token, allowed_scopes, broadcaster, conn_id, session) {
                    ws.send(Message::Text(response)).map_err(ws_err_to_io)?;
                }
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).map_err(ws_err_to_io)?;
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(err))
                if err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut =>
            {
                // Plain 50ms read-poll timeout (or a write timing out, R-4) —
                // the hello-deadline check now lives at the top of the loop,
                // so this arm only needs to keep polling.
                continue;
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
            Err(err) => return Err(ws_err_to_io(err)),
        }
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
        QueuedNotification::Output { pane_id, lines, dropped } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "noa.output",
            "params": OutputParams { pane_id: WireId(pane_id), lines, dropped },
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
        RpcFail { code, message: message.into() }
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
        Err(RpcFail::new(ErrorCode::ScopeDenied, format!("missing scope: {}", scope.as_str())))
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
        Err(_) => return Some(error_response(Value::Null, ErrorCode::ParseError, "parse error")),
    };
    let req: RpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(_) => {
            return Some(error_response(Value::Null, ErrorCode::InvalidRequest, "invalid request"));
        }
    };
    let id = req.id.clone();

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
        return Some(error_response(id, ErrorCode::Auth, "noa.hello required before other methods"));
    }

    let outcome = (|| -> Result<Value, RpcFail> {
        match req.method.as_str() {
            "noa.listPanels" => {
                require_scope(session, Scope::Read)?;
                Ok(serde_json::to_value(ListPanelsResult { panels: backend.list_panels() }).unwrap())
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
                handle_close_pane(backend, req.params)
            }
            "noa.subscribe" => {
                require_scope(session, Scope::Read)?;
                handle_subscribe(broadcaster, conn_id, req.params)
            }
            "noa.unsubscribe" => {
                require_scope(session, Scope::Read)?;
                handle_unsubscribe(broadcaster, conn_id, req.params)
            }
            _ => Err(RpcFail::new(ErrorCode::MethodNotFound, format!("unknown method: {}", req.method))),
        }
    })();

    Some(match outcome {
        Ok(result) => success_response(id, result),
        Err(fail) => error_response(id, fail.code, &fail.message),
    })
}

fn handle_hello(id: Value, params: Value, token: &str, allowed_scopes: ScopeSet, session: &mut Session) -> String {
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
    let max_bytes = p.max_bytes.unwrap_or(DEFAULT_TEXT_MAX_BYTES);
    let backend_result = backend.get_text(p.pane_id.0, p.source, max_bytes)?;
    let (text, truncated_here) = truncate_tail(&backend_result.text, max_bytes);
    let result = GetTextResult { pane_id: p.pane_id, text, truncated: truncated_here || backend_result.truncated };
    Ok(serde_json::to_value(result).unwrap())
}

fn handle_get_grid(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: GetGridParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let backend_result = backend.get_grid(p.pane_id.0, p.start_row, p.row_count)?;
    let backend_has_more = backend_result.has_more;
    let (rows, has_more) = cap_grid_rows(backend_result.rows, DEFAULT_GRID_CAP_BYTES)
        .map_err(|GridCapExceeded| RpcFail::new(ErrorCode::PayloadTooLarge, "row exceeds response cap"))?;
    let has_more = has_more || backend_has_more;
    let result = GetGridResult { pane_id: p.pane_id, cols: backend_result.cols, start_row: p.start_row, rows, has_more };
    Ok(serde_json::to_value(result).unwrap())
}

fn handle_send_text(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: SendTextParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.send_text(p.pane_id.0, &p.text)?;
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
    Ok(serde_json::to_value(PaneIdResult { pane_id: WireId(pane_id) }).unwrap())
}

fn handle_split(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: SplitParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let pane_id = backend.split(p.pane_id.0, p.direction)?;
    Ok(serde_json::to_value(PaneIdResult { pane_id: WireId(pane_id) }).unwrap())
}

fn handle_close_pane(backend: &Arc<dyn IpcBackend>, params: Value) -> Result<Value, RpcFail> {
    let p: PaneIdParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    backend.close_pane(p.pane_id.0)?;
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

fn handle_subscribe(broadcaster: &Broadcaster, conn_id: u64, params: Value) -> Result<Value, RpcFail> {
    let p: SubscribeParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    let mask = EventMask::from_events(&p.events);
    let pane_ids = p.pane_ids.map(|ids| ids.into_iter().map(|id| id.0).collect::<HashSet<u64>>());
    let sub_id = broadcaster.add_subscription(conn_id, mask, pane_ids).map_err(|err| match err {
        AddSubscriptionError::ConnectionNotFound => {
            RpcFail::new(ErrorCode::InvalidRequest, "connection not registered")
        }
        // R-2: cap on subscriptions per connection — the connection stays
        // open, this one `noa.subscribe` call just fails.
        AddSubscriptionError::LimitExceeded => {
            RpcFail::new(ErrorCode::PayloadTooLarge, "subscription limit exceeded")
        }
    })?;
    Ok(serde_json::to_value(SubscribeResult { subscription_id: WireId(sub_id) }).unwrap())
}

fn handle_unsubscribe(broadcaster: &Broadcaster, conn_id: u64, params: Value) -> Result<Value, RpcFail> {
    let p: UnsubscribeParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    broadcaster.remove_subscription(conn_id, p.subscription_id.0);
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn deadline_stream_arms_the_socket_timeout_to_the_remaining_deadline_capped_by_handshake_io_timeout() {
        let (_client, server) = tcp_pair();
        let stream = DeadlineStream::new_handshake(server, Duration::from_millis(50));
        // Plenty of deadline left, but the per-call bound is still capped at
        // `HANDSHAKE_IO_TIMEOUT`, never the other way around.
        stream.arm_handshake(Instant::now() + Duration::from_secs(60)).unwrap();
        let bound = stream.inner.read_timeout().unwrap().unwrap();
        assert!(bound <= HANDSHAKE_IO_TIMEOUT);
    }

    // ---- R-4: post-handshake timeouts + exit-path cleanup guarantee ----

    #[test]
    fn deadline_stream_mark_connected_applies_the_steady_state_timeouts() {
        let (_client, server) = tcp_pair();
        let mut stream = DeadlineStream::new_handshake(server, Duration::from_secs(5));
        stream.mark_connected().unwrap();
        assert_eq!(stream.inner.read_timeout().unwrap(), Some(Duration::from_millis(50)));
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
            let _guard =
                ConnectionGuard { broadcaster: broadcaster.clone(), conn_id: None, connection_count: connection_count.clone() };
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
            };
        }
        assert_eq!(broadcaster.connection_count(), 0);
        assert_eq!(connection_count.load(Ordering::SeqCst), 0);
    }
}
