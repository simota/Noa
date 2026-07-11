//! The JSON-RPC-over-WebSocket server (spec §L2 "Transport & Handshake",
//! sync tungstenite + thread-per-connection + crossbeam, no async runtime —
//! NFR-3).

use std::collections::HashSet;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
use crate::push::{Broadcaster, EventMask, PushQueue, QueuedNotification};

/// Server startup configuration (spec §L2 "Config キー").
pub struct ServerConfig {
    /// `0` binds an OS-assigned ephemeral port (tests use this; production
    /// uses config `server-port`, default `61771`).
    pub port: u16,
    pub token: String,
    pub allowed_scopes: ScopeSet,
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
    pub fn start(config: ServerConfig, backend: Arc<dyn IpcBackend>) -> io::Result<ServerHandle> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let shutdown = Arc::new(AtomicBool::new(false));
        let broadcaster = Broadcaster::new();
        let token = Arc::new(config.token);
        let allowed_scopes = config.allowed_scopes;
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
                            if let Err(err) = handle_connection(
                                stream,
                                backend,
                                token,
                                allowed_scopes,
                                broadcaster,
                                shutdown_conn,
                            ) {
                                log::debug!("noa-ipc: connection ended: {err}");
                            }
                            connection_count.fetch_sub(1, Ordering::SeqCst);
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

fn handle_connection(
    stream: TcpStream,
    backend: Arc<dyn IpcBackend>,
    token: Arc<String>,
    allowed_scopes: ScopeSet,
    broadcaster: Broadcaster,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
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

    let mut ws = tungstenite::accept_hdr_with_config(stream, callback, Some(connection_ws_config()))
        .map_err(|err| io::Error::other(err.to_string()))?;
    ws.get_mut().set_read_timeout(Some(Duration::from_millis(50)))?;

    let (conn_id, queue) = broadcaster.register_connection();
    let mut session = Session {
        hello_done: false,
        header_authed: header_authed.load(Ordering::SeqCst),
        granted_scopes: ScopeSet::empty(),
    };

    let result = run_connection_loop(&mut ws, &backend, &token, allowed_scopes, &broadcaster, conn_id, &queue, &mut session, &shutdown);

    broadcaster.unregister_connection(conn_id);
    result
}

#[allow(clippy::too_many_arguments)]
fn run_connection_loop(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    backend: &Arc<dyn IpcBackend>,
    token: &str,
    allowed_scopes: ScopeSet,
    broadcaster: &Broadcaster,
    conn_id: u64,
    queue: &Arc<PushQueue>,
    session: &mut Session,
    shutdown: &Arc<AtomicBool>,
) -> io::Result<()> {
    loop {
        if shutdown.load(Ordering::SeqCst) {
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
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

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
    let req: RpcRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(_) => return Some(error_response(Value::Null, ErrorCode::ParseError, "parse error")),
    };
    let id = req.id.clone();

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
    let sub_id = broadcaster
        .add_subscription(conn_id, mask, pane_ids)
        .ok_or_else(|| RpcFail::new(ErrorCode::InvalidRequest, "connection not registered"))?;
    Ok(serde_json::to_value(SubscribeResult { subscription_id: WireId(sub_id) }).unwrap())
}

fn handle_unsubscribe(broadcaster: &Broadcaster, conn_id: u64, params: Value) -> Result<Value, RpcFail> {
    let p: UnsubscribeParams = serde_json::from_value(params).map_err(|_| RpcFail::invalid_params())?;
    broadcaster.remove_subscription(conn_id, p.subscription_id.0);
    Ok(serde_json::to_value(OkResult::ok()).unwrap())
}
