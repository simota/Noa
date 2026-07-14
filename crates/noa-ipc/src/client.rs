//! Synchronous control and raw-attach client for another noa instance.
//!
//! This module intentionally contains no reconnect policy or UI state. The
//! application owns those concerns while reusing this typed, sync transport.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::attach::{ATTACH_BINARY_CHUNK_BYTES, MAX_ATTACH_SEED_BYTES};
use crate::auth::ScopeSet;
use crate::protocol::{
    AttachResult, GetTextResult, HelloResult, ListPanelsResult, OkResult, PaneIdResult, Panel,
    SplitDirection, TextSource, WireId,
};
use crate::server::ATTACH_HANDSHAKE_CLOSE_REASON;

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Default raw-socket poll bound used by a dedicated sync connection-manager
/// thread. It keeps input/resize/shutdown command handling responsive without
/// treating an idle remote pane as disconnected.
pub const DEFAULT_ATTACH_POLL_TIMEOUT: Duration = Duration::from_millis(10);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_SEED_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WS_MESSAGE_SIZE: usize = 1024 * 1024;
const MAX_WS_FRAME_SIZE: usize = 256 * 1024;
const ATTACH_PATH: &str = "/attach";

/// A connected JSON-RPC control client. Reconnect/backoff belongs to
/// `noa-app`; this type represents one live transport attempt.
pub struct Client {
    socket: Socket,
    next_id: u64,
    granted_scopes: ScopeSet,
    control_origin: (String, u16),
}

impl Client {
    pub fn connect(
        control_url: &str,
        token: &str,
        requested_scopes: ScopeSet,
    ) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(
            control_url,
            token,
            requested_scopes,
            CONNECT_TIMEOUT,
            CONTROL_REQUEST_TIMEOUT,
        )
    }

    fn connect_with_timeouts(
        control_url: &str,
        token: &str,
        requested_scopes: ScopeSet,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, ClientError> {
        if token.trim().is_empty() {
            return Err(ClientError::InvalidInput(
                "control token must not be empty".to_string(),
            ));
        }
        let control_origin = websocket_origin(control_url)?;
        let mut socket = connect_websocket(control_url, connect_timeout)?;
        set_socket_io_timeout(&mut socket, request_timeout)?;
        let mut client = Client {
            socket,
            next_id: 1,
            granted_scopes: ScopeSet::empty(),
            control_origin,
        };
        let result: HelloResult = client.request_with_timeout(
            "noa.hello",
            serde_json::json!({
                "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                "token": token,
                "scopes": requested_scopes.to_strings(),
            }),
            request_timeout,
        )?;
        if result.protocol_version != crate::protocol::PROTOCOL_VERSION {
            return Err(ClientError::Protocol(
                "server returned an incompatible protocol version".to_string(),
            ));
        }
        client.granted_scopes = ScopeSet::from_strings(result.granted_scopes);
        Ok(client)
    }

    pub fn granted_scopes(&self) -> ScopeSet {
        self.granted_scopes
    }

    pub fn list_panels(&mut self) -> Result<Vec<Panel>, ClientError> {
        let result: ListPanelsResult = self.request("noa.listPanels", serde_json::json!({}))?;
        Ok(result.panels)
    }

    pub fn get_text(
        &mut self,
        pane: u64,
        source: TextSource,
        max_bytes: Option<usize>,
    ) -> Result<GetTextResult, ClientError> {
        self.request(
            "noa.getText",
            serde_json::json!({
                "paneId": WireId(pane),
                "source": source,
                "maxBytes": max_bytes,
            }),
        )
    }

    pub fn new_tab(&mut self, window: Option<u64>) -> Result<u64, ClientError> {
        let result: PaneIdResult = self.request(
            "noa.newTab",
            serde_json::json!({ "windowId": window.map(WireId) }),
        )?;
        Ok(result.pane_id.0)
    }

    pub fn split(&mut self, pane: u64, direction: SplitDirection) -> Result<u64, ClientError> {
        let result: PaneIdResult = self.request(
            "noa.split",
            serde_json::json!({
                "paneId": WireId(pane),
                "direction": direction,
            }),
        )?;
        Ok(result.pane_id.0)
    }

    /// Reserves a single-use raw channel without opening it. Most callers
    /// should use [`Self::attach`]; this lower-level split is useful when a
    /// connection manager needs to stage resources before the second socket.
    pub fn reserve_attach(&mut self, pane: u64) -> Result<AttachResult, ClientError> {
        self.request("noa.attach", serde_json::json!({ "paneId": WireId(pane) }))
    }

    pub fn attach(&mut self, pane: u64) -> Result<AttachClient, ClientError> {
        let reservation = self.reserve_attach(pane)?;
        self.open_reserved_attach(pane, &reservation)
    }

    /// Opens the raw channel for a lease already obtained with
    /// [`Self::reserve_attach`]. The reservation remains owned by this control
    /// connection, and a failed raw handshake releases it before returning.
    pub fn open_reserved_attach(
        &mut self,
        pane: u64,
        reservation: &AttachResult,
    ) -> Result<AttachClient, ClientError> {
        validate_attach_url(&reservation.attach_url, &self.control_origin)?;
        match AttachClient::connect(pane, reservation) {
            Ok(client) => Ok(client),
            Err(err) => {
                let _ = self.detach(pane);
                Err(err)
            }
        }
    }

    pub fn detach(&mut self, pane: u64) -> Result<(), ClientError> {
        let _: OkResult =
            self.request("noa.detach", serde_json::json!({ "paneId": WireId(pane) }))?;
        Ok(())
    }

    pub fn resize_pane(&mut self, pane: u64, cols: u16, rows: u16) -> Result<(), ClientError> {
        if cols == 0 || rows == 0 {
            return Err(ClientError::InvalidInput(
                "remote pane dimensions must be non-zero".to_string(),
            ));
        }
        let _: OkResult = self.request(
            "noa.resizePane",
            serde_json::json!({
                "paneId": WireId(pane),
                "cols": cols,
                "rows": rows,
            }),
        )?;
        Ok(())
    }

    pub fn close_pane(&mut self, pane: u64) -> Result<(), ClientError> {
        let _: OkResult = self.request(
            "noa.closePane",
            serde_json::json!({ "paneId": WireId(pane) }),
        )?;
        Ok(())
    }

    fn request<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, ClientError> {
        self.request_with_timeout(method, params, CONTROL_REQUEST_TIMEOUT)
    }

    fn request_with_timeout<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: impl Serialize,
        timeout: Duration,
    ) -> Result<T, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.socket
            .send(Message::Text(request.to_string()))
            .map_err(ClientError::transport)?;

        let deadline = Instant::now() + timeout;
        loop {
            set_socket_read_timeout(&mut self.socket, remaining_until(deadline)?)?;
            match self.socket.read().map_err(ClientError::transport)? {
                Message::Text(text) => {
                    let response: RpcResponse = serde_json::from_str(&text).map_err(|_| {
                        ClientError::Protocol("control response is not valid JSON-RPC".to_string())
                    })?;
                    if response.id.as_u64() != Some(id) {
                        // Notifications and responses for a different owner are
                        // outside this request's contract.
                        continue;
                    }
                    if response.jsonrpc.as_deref() != Some("2.0") {
                        return Err(ClientError::Protocol(
                            "control response has an invalid jsonrpc version".to_string(),
                        ));
                    }
                    if let Some(error) = response.error {
                        return Err(ClientError::Rpc {
                            code: error.code,
                            message: error.message,
                        });
                    }
                    let result = response.result.ok_or_else(|| {
                        ClientError::Protocol("control response has no result".to_string())
                    })?;
                    return serde_json::from_value(result).map_err(|_| {
                        ClientError::Protocol("control result has an invalid shape".to_string())
                    });
                }
                Message::Ping(payload) => self
                    .socket
                    .send(Message::Pong(payload))
                    .map_err(ClientError::transport)?,
                Message::Pong(_) => {}
                Message::Close(frame) => return Err(ClientError::closed(frame)),
                _ => {
                    return Err(ClientError::Protocol(
                        "control channel received a non-text data frame".to_string(),
                    ));
                }
            }
        }
    }
}

/// One authenticated raw attach socket. The initial Binary-message sequence,
/// terminated by an empty Binary message, is reassembled as the synthetic
/// seed; subsequent messages are available through [`Self::read_raw`].
pub struct AttachClient {
    pane: u64,
    socket: Socket,
    seed: Vec<u8>,
    /// Set when `poll_raw` silently drains a `Pong` frame. `take_pong`
    /// exposes it as a one-shot liveness signal for a caller-driven
    /// heartbeat; ordinary raw output never touches this flag.
    pong_received: bool,
}

impl AttachClient {
    pub fn connect(pane: u64, reservation: &AttachResult) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(pane, reservation, CONNECT_TIMEOUT, ATTACH_SEED_TIMEOUT)
    }

    fn connect_with_timeouts(
        pane: u64,
        reservation: &AttachResult,
        connect_timeout: Duration,
        seed_timeout: Duration,
    ) -> Result<Self, ClientError> {
        validate_attach_path(&reservation.attach_url)?;
        let mut socket = connect_websocket(reservation.attach_url.as_str(), connect_timeout)?;
        set_socket_io_timeout(&mut socket, seed_timeout)?;
        let seed_deadline = Instant::now() + seed_timeout;
        socket
            .send(Message::Binary(
                reservation.attach_token.as_bytes().to_vec(),
            ))
            .map_err(ClientError::transport)?;
        let seed = read_raw_frame_until(&mut socket, seed_deadline)?;
        set_socket_read_timeout(&mut socket, DEFAULT_ATTACH_POLL_TIMEOUT)?;
        set_socket_write_timeout(&mut socket, CONTROL_REQUEST_TIMEOUT)?;
        Ok(AttachClient {
            pane,
            socket,
            seed,
            pong_received: false,
        })
    }

    pub fn pane(&self) -> u64 {
        self.pane
    }

    pub fn seed(&self) -> &[u8] {
        &self.seed
    }

    pub fn take_seed(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.seed)
    }

    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        for chunk in bytes.chunks(ATTACH_BINARY_CHUNK_BYTES) {
            self.socket
                .send(Message::Binary(chunk.to_vec()))
                .map_err(ClientError::transport)?;
        }
        Ok(())
    }

    pub fn read_raw(&mut self) -> Result<Vec<u8>, ClientError> {
        read_raw_frame(&mut self.socket)
    }

    /// Polls for one raw output frame. A socket read timeout is an idle poll,
    /// returned as `Ok(None)`; it is not a disconnect. This is the intended
    /// seam for a sync dedicated thread that must also service app commands.
    pub fn poll_raw(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        match poll_raw_frame(&mut self.socket)? {
            RawPollEvent::Data(bytes) => Ok(Some(bytes)),
            RawPollEvent::Pong => {
                self.pong_received = true;
                Ok(None)
            }
            RawPollEvent::Idle => Ok(None),
        }
    }

    /// Sends an application-level `Ping` so a caller-driven heartbeat can
    /// detect a peer that has vanished without a TCP reset (e.g. a dropped
    /// Wi-Fi link) — an idle attach otherwise only ever sees read timeouts,
    /// which are indistinguishable from a merely quiet remote pane.
    pub fn send_ping(&mut self) -> Result<(), ClientError> {
        self.socket
            .send(Message::Ping(Vec::new()))
            .map_err(ClientError::transport)
    }

    /// Clears and returns whether a `Pong` was drained by `poll_raw` since
    /// the last call. One-shot so a heartbeat can treat it as an edge-typed
    /// liveness signal rather than a shared counter it must reset itself.
    pub fn take_pong(&mut self) -> bool {
        std::mem::take(&mut self.pong_received)
    }

    pub fn set_poll_timeout(&mut self, timeout: Duration) -> Result<(), ClientError> {
        if timeout.is_zero() {
            return Err(ClientError::InvalidInput(
                "attach poll timeout must be non-zero".to_string(),
            ));
        }
        set_socket_read_timeout(&mut self.socket, timeout)
    }

    pub fn close(&mut self) -> Result<(), ClientError> {
        self.socket.close(None).map_err(ClientError::transport)
    }
}

fn read_raw_frame(socket: &mut Socket) -> Result<Vec<u8>, ClientError> {
    loop {
        if let RawPollEvent::Data(bytes) = poll_raw_frame(socket)? {
            return Ok(bytes);
        }
    }
}

/// Outcome of one non-blocking-ish raw-attach socket poll. `Pong` is kept
/// distinct from `Idle` so a caller-driven heartbeat (`AttachClient::take_pong`)
/// can tell "the peer answered" from "nothing arrived before the timeout."
enum RawPollEvent {
    Data(Vec<u8>),
    Pong,
    Idle,
}

fn read_raw_frame_until(socket: &mut Socket, deadline: Instant) -> Result<Vec<u8>, ClientError> {
    let mut seed = Vec::new();
    loop {
        set_socket_read_timeout(socket, remaining_until(deadline)?)?;
        match socket.read() {
            Ok(Message::Binary(bytes)) if bytes.is_empty() => return Ok(seed),
            Ok(Message::Binary(bytes)) => {
                let Some(next_len) = seed.len().checked_add(bytes.len()) else {
                    return Err(ClientError::Protocol(
                        "attach seed length overflowed".to_string(),
                    ));
                };
                if next_len > MAX_ATTACH_SEED_BYTES {
                    return Err(ClientError::Protocol(format!(
                        "attach seed exceeds the {} byte limit",
                        MAX_ATTACH_SEED_BYTES
                    )));
                }
                seed.extend_from_slice(&bytes);
            }
            Ok(Message::Ping(payload)) => socket
                .send(Message::Pong(payload))
                .map_err(ClientError::transport)?,
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(frame)) => return Err(ClientError::closed(frame)),
            Ok(_) => {
                return Err(ClientError::Protocol(
                    "attach channel received a non-binary data frame".to_string(),
                ));
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(ClientError::Transport(
                    "attach seed deadline exceeded".to_string(),
                ));
            }
            Err(error) => return Err(ClientError::transport(error)),
        }
    }
}

fn poll_raw_frame(socket: &mut Socket) -> Result<RawPollEvent, ClientError> {
    loop {
        match socket.read() {
            Ok(Message::Binary(bytes)) => return Ok(RawPollEvent::Data(bytes)),
            Ok(Message::Ping(payload)) => socket
                .send(Message::Pong(payload))
                .map_err(ClientError::transport)?,
            Ok(Message::Pong(_)) => return Ok(RawPollEvent::Pong),
            Ok(Message::Close(frame)) => return Err(ClientError::closed(frame)),
            Ok(_) => {
                return Err(ClientError::Protocol(
                    "attach channel received a non-binary data frame".to_string(),
                ));
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(RawPollEvent::Idle);
            }
            Err(error) => return Err(ClientError::transport(error)),
        }
    }
}

fn set_socket_read_timeout(socket: &mut Socket, timeout: Duration) -> Result<(), ClientError> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| ClientError::Transport(error.to_string())),
        _ => Err(ClientError::InvalidInput(
            "raw attach polling currently requires a ws:// endpoint".to_string(),
        )),
    }
}

fn set_socket_write_timeout(socket: &mut Socket, timeout: Duration) -> Result<(), ClientError> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| ClientError::Transport(error.to_string())),
        _ => Err(ClientError::InvalidInput(
            "client mode currently requires a ws:// endpoint".to_string(),
        )),
    }
}

fn set_socket_io_timeout(socket: &mut Socket, timeout: Duration) -> Result<(), ClientError> {
    set_socket_read_timeout(socket, timeout)?;
    set_socket_write_timeout(socket, timeout)
}

fn remaining_until(deadline: Instant) -> Result<Duration, ClientError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(ClientError::Transport(
            "websocket operation deadline exceeded".to_string(),
        ))
    } else {
        Ok(remaining)
    }
}

fn client_ws_config() -> WebSocketConfig {
    #[allow(deprecated)]
    WebSocketConfig {
        max_send_queue: None,
        write_buffer_size: 128 * 1024,
        max_write_buffer_size: 1024 * 1024,
        max_message_size: Some(MAX_WS_MESSAGE_SIZE),
        max_frame_size: Some(MAX_WS_FRAME_SIZE),
        accept_unmasked_frames: false,
    }
}

fn websocket_request(url: &str) -> Result<tungstenite::http::Request<()>, ClientError> {
    url.into_client_request().map_err(ClientError::transport)
}

fn websocket_origin(url: &str) -> Result<(String, u16), ClientError> {
    let request = websocket_request(url)?;
    let uri = request.uri();
    if uri.scheme_str() != Some("ws") {
        return Err(ClientError::InvalidInput(
            "client mode currently requires a ws:// endpoint".to_string(),
        ));
    }
    let host = uri
        .host()
        .ok_or_else(|| ClientError::InvalidInput("websocket endpoint has no host".to_string()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    Ok((host, uri.port_u16().unwrap_or(80)))
}

fn validate_attach_path(url: &str) -> Result<(), ClientError> {
    let request = websocket_request(url)?;
    let uri = request.uri();
    if uri.path() != ATTACH_PATH || uri.query().is_some() {
        return Err(ClientError::InvalidInput(
            "attach URL must use the fixed /attach endpoint".to_string(),
        ));
    }
    Ok(())
}

fn validate_attach_url(url: &str, control_origin: &(String, u16)) -> Result<(), ClientError> {
    validate_attach_path(url)?;
    if &websocket_origin(url)? != control_origin {
        return Err(ClientError::InvalidInput(
            "attach URL must use the control endpoint origin".to_string(),
        ));
    }
    Ok(())
}

fn connect_websocket(url: &str, timeout: Duration) -> Result<Socket, ClientError> {
    let deadline = Instant::now() + timeout;
    let request = websocket_request(url)?;
    let uri = request.uri().clone();
    if uri.scheme_str() != Some("ws") {
        return Err(ClientError::InvalidInput(
            "client mode currently requires a ws:// endpoint".to_string(),
        ));
    }
    let host = uri
        .host()
        .ok_or_else(|| ClientError::InvalidInput("websocket endpoint has no host".to_string()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri.port_u16().unwrap_or(80);
    let addresses = if let Ok(ip) = host.parse() {
        vec![SocketAddr::new(ip, port)]
    } else {
        resolve_socket_addrs_with_deadline(deadline, move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect())
        })?
    };
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        let remaining = remaining_until(deadline)?;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = stream.ok_or_else(|| {
        ClientError::Transport(
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "websocket endpoint resolved to no addresses".to_string()),
        )
    })?;
    stream
        .set_nodelay(true)
        .map_err(|error| ClientError::Transport(error.to_string()))?;
    let remaining = remaining_until(deadline)?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|error| ClientError::Transport(error.to_string()))?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|error| ClientError::Transport(error.to_string()))?;

    let stream = MaybeTlsStream::Plain(stream);
    let (socket, _) =
        tungstenite::client::client_with_config(request, stream, Some(client_ws_config()))
            .map_err(|error| ClientError::Transport(error.to_string()))?;
    Ok(socket)
}

fn resolve_socket_addrs_with_deadline<F>(
    deadline: Instant,
    resolver: F,
) -> Result<Vec<SocketAddr>, ClientError>
where
    F: FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
{
    // `ToSocketAddrs` is synchronous and exposes no timeout. Isolate it from
    // the caller so the same overall deadline covers DNS and every following
    // TCP/WebSocket step. A resolver that ignores cancellation may finish in
    // the detached thread later, but it can no longer stall the client worker.
    remaining_until(deadline)?;
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("noa-ipc-dns".to_string())
        .spawn(move || {
            let _ = result_tx.send(resolver());
        })
        .map_err(|error| {
            ClientError::Transport(format!("failed to start DNS resolver: {error}"))
        })?;

    let remaining = remaining_until(deadline)?;
    match result_rx.recv_timeout(remaining) {
        Ok(Ok(addresses)) => Ok(addresses),
        Ok(Err(error)) => Err(ClientError::Transport(error.to_string())),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(ClientError::Transport(
            "websocket DNS resolution deadline exceeded".to_string(),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Transport(
            "websocket DNS resolver stopped unexpectedly".to_string(),
        )),
    }
}

#[derive(serde::Deserialize)]
struct RpcResponse {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(serde::Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid client input: {0}")]
    InvalidInput(String),
    #[error("websocket transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("{ATTACH_HANDSHAKE_CLOSE_REASON}")]
    AttachHandshakeFailure,
    #[error("websocket closed: {0}")]
    Closed(String),
}

impl ClientError {
    fn transport(error: tungstenite::Error) -> Self {
        ClientError::Transport(error.to_string())
    }

    fn closed(frame: Option<tungstenite::protocol::CloseFrame<'static>>) -> Self {
        match frame {
            Some(frame) if frame.reason.starts_with("-32008") => {
                ClientError::AttachHandshakeFailure
            }
            Some(frame) => ClientError::Closed(format!("{} {}", frame.code, frame.reason)),
            None => ClientError::Closed("without close frame".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn control_hello_has_a_bounded_response_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            assert!(matches!(socket.read().unwrap(), Message::Text(_)));
            thread::sleep(Duration::from_millis(200));
        });

        let started = Instant::now();
        let result = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::empty(),
            Duration::from_millis(100),
            Duration::from_millis(50),
        );
        assert!(matches!(result, Err(ClientError::Transport(_))));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    #[test]
    fn attach_seed_has_a_bounded_response_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let reservation = AttachResult {
            attach_url: format!("ws://{}/attach", listener.local_addr().unwrap()),
            attach_token: "one-time-token".to_string(),
        };
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            assert_eq!(
                socket.read().unwrap(),
                Message::Binary(b"one-time-token".to_vec())
            );
            thread::sleep(Duration::from_millis(200));
        });

        let started = Instant::now();
        let result = AttachClient::connect_with_timeouts(
            1,
            &reservation,
            Duration::from_millis(100),
            Duration::from_millis(50),
        );
        assert!(matches!(result, Err(ClientError::Transport(_))));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    #[test]
    fn attach_url_is_fixed_to_the_control_origin_and_path() {
        let origin = websocket_origin("ws://127.0.0.1:61771/").unwrap();
        assert!(validate_attach_url("ws://127.0.0.1:61771/attach", &origin).is_ok());
        assert!(validate_attach_url("ws://127.0.0.1:61772/attach", &origin).is_err());
        assert!(validate_attach_url("ws://127.0.0.1:61771/other", &origin).is_err());
        assert!(validate_attach_url("ws://127.0.0.1:61771/attach?x=1", &origin).is_err());
    }

    #[test]
    fn dns_resolution_is_bounded_by_the_connect_deadline() {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(25);
        let result = resolve_socket_addrs_with_deadline(deadline, || {
            thread::sleep(Duration::from_millis(500));
            Ok(Vec::<SocketAddr>::new())
        });

        assert!(
            matches!(result, Err(ClientError::Transport(message)) if message.contains("DNS resolution deadline"))
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "DNS must not outlive the caller's connect deadline"
        );
    }
}
