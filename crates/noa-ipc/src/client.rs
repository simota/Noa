//! Synchronous control and raw-attach client for another noa instance.
//!
//! This module intentionally contains no reconnect policy or UI state. The
//! application owns those concerns while reusing this typed, sync transport.

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::attach::{ATTACH_BINARY_CHUNK_BYTES, MAX_ATTACH_SEED_BYTES};
use crate::auth::ScopeSet;
use crate::protocol::{
    AttachResult, EventKind, GetTextResult, HelloResult, ListPanelsResult, PaneIdResult, Panel,
    PanelField, SpawnConfig, SpawnParams, SplitDirection, SplitPlacement, TextSource,
    UpdateSubscriptionParams, WireId,
};
use crate::server::ATTACH_HANDSHAKE_CLOSE_REASON;
use crate::timeline;

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Default raw-socket poll bound used by a dedicated sync connection-manager
/// thread. It keeps input/resize/shutdown command handling responsive without
/// treating an idle remote pane as disconnected.
pub const DEFAULT_ATTACH_POLL_TIMEOUT: Duration = Duration::from_millis(10);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_SEED_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WS_MESSAGE_SIZE: usize = 1024 * 1024;
// The server sends a capped `getText` response (up to ~1 MiB after JSON
// wrapping) as a single unfragmented text frame, so the frame bound must be
// as large as the message bound or a default-sized scrollback read would be
// rejected on receipt.
pub(crate) const MAX_WS_FRAME_SIZE: usize = MAX_WS_MESSAGE_SIZE;
const ATTACH_PATH: &str = "/attach";

/// Largest `text` one `noa.sendText` may carry, measured as its JSON-encoded
/// string (`serde_json::to_string(text)`, quotes and escapes included). The
/// server reads one request frame of at most 256 KiB and closes the
/// connection with 1009 on a larger one; this leaves room for the rest of
/// the request around the text.
pub const MAX_SEND_TEXT_BYTES: usize = 250 * 1024;
/// Notifications kept while no one reads them; older ones are dropped first
/// and the next [`Client::next_notification`] reports the loss.
const MAX_BUFFERED_NOTIFICATIONS: usize = 1024;

/// A server-to-client notification received on the control socket.
#[derive(Clone, Debug)]
pub enum Notification {
    /// `noa.stateChanged`: panels that changed or were added, and the ids
    /// of panes that closed (`removedPaneIds`, empty when the server sent
    /// none). `dropped` means earlier notifications were lost (server- or
    /// client-side), removals included: the caller must re-read state with
    /// [`Client::list_panels`] and treat every cached pane missing from that
    /// list as removed.
    StateChanged {
        panels: Vec<Panel>,
        removed_pane_ids: Vec<u64>,
        dropped: bool,
    },
    /// Any other notification (`noa.output`, …), by method name.
    Other { method: String, dropped: bool },
}

impl Notification {
    pub fn dropped(&self) -> bool {
        match self {
            Self::StateChanged { dropped, .. } | Self::Other { dropped, .. } => *dropped,
        }
    }

    fn mark_dropped(&mut self) {
        match self {
            Self::StateChanged { dropped, .. } | Self::Other { dropped, .. } => *dropped = true,
        }
    }
}

/// [`Client::spawn_board_tab`]'s request: `params` with `vars`' `PATH`,
/// `LANG` and `LC_*` added to its env where it sets none of its own.
fn board_tab_params(
    params: &SpawnParams,
    vars: impl Iterator<Item = (String, String)>,
) -> SpawnParams {
    let mut params = params.clone();
    let config = params.config.get_or_insert_with(SpawnConfig::default);
    for (key, value) in vars {
        if key == "PATH" || key == "LANG" || key.starts_with("LC_") {
            config.env.entry(key).or_insert(value);
        }
    }
    params
}

/// A connected JSON-RPC control client. Reconnect/backoff belongs to
/// `noa-app`; this type represents one live transport attempt.
/// Requests that exceed the server's frame limit are rejected locally with
/// [`ClientError::InvalidInput`], leaving the connection usable.
pub struct Client {
    socket: Socket,
    next_id: u64,
    granted_scopes: ScopeSet,
    server_instance_id: String,
    server_version: String,
    /// `noa.hello`'s `capabilities` (empty from a server predating them).
    capabilities: Vec<String>,
    control_origin: (String, u16),
    /// Notifications that arrived while a request waited for its response.
    notifications: VecDeque<Notification>,
    notifications_dropped: bool,
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
            server_instance_id: String::new(),
            server_version: String::new(),
            capabilities: Vec::new(),
            control_origin,
            notifications: VecDeque::new(),
            notifications_dropped: false,
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
        client.server_instance_id = result.server_instance_id;
        client.server_version = result.server_version;
        client.capabilities = result.capabilities;
        Ok(client)
    }

    pub fn granted_scopes(&self) -> ScopeSet {
        self.granted_scopes
    }

    /// The server's per-process identity from `noa.hello`; empty when the
    /// server predates the field. Pane ids are only meaningful within one
    /// server instance.
    pub fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }

    /// The server's version from this connection's `noa.hello`
    /// (`serverVersion`), so what a request needs of the server is judged
    /// on the server that will take it.
    pub fn server_version(&self) -> &str {
        &self.server_version
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

    pub fn focus_pane(&mut self, pane: u64) -> Result<(), ClientError> {
        self.request_ok(
            "noa.focusPane",
            serde_json::json!({ "paneId": WireId(pane) }),
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
    /// `noa.newTab` with agent-orchestration spawn extensions.
    pub fn spawn_tab(
        &mut self,
        window: Option<u64>,
        params: &SpawnParams,
    ) -> Result<u64, ClientError> {
        let mut body = serde_json::to_value(params).map_err(|_| {
            ClientError::InvalidInput("spawn parameters are not serializable".to_string())
        })?;
        if let (Some(window), Some(object)) = (window, body.as_object_mut()) {
            object.insert("windowId".to_string(), serde_json::json!(WireId(window)));
        }
        let result: PaneIdResult = self.request("noa.newTab", body)?;
        Ok(result.pane_id.0)
    }

    /// agent-orchestration D20: `noa.newTab` with `board: true` — a new tab
    /// whose pane is a board that orchestrates itself. Returns the board's
    /// IPC id; floats spawned with [`Self::spawn_split`]
    /// (`SplitPlacement::Float`) over that pane, with `parent_pane_id` set to
    /// it, are stowed on the board. `params` takes `focus`, `auto_approve`
    /// and a `config` of only `cwd` and `env`; the server refuses the rest.
    /// This process's `PATH`, `LANG` and `LC_*` go into the board's env
    /// unless `params` sets them, as `+ctl board --open` gives a board its
    /// orchestrator's: a Noa started from the Dock has none of them.
    ///
    /// A Noa without [`crate::CAPABILITY_BOARD_TAB`] ignores `board` and
    /// opens a shell tab: check [`Self::supports_board_tab`] first.
    pub fn spawn_board_tab(
        &mut self,
        window: Option<u64>,
        params: &SpawnParams,
    ) -> Result<u64, ClientError> {
        let params = board_tab_params(params, std::env::vars());
        let mut body = serde_json::to_value(&params).map_err(|_| {
            ClientError::InvalidInput("spawn parameters are not serializable".to_string())
        })?;
        if let Some(object) = body.as_object_mut() {
            object.insert("board".to_string(), Value::Bool(true));
            if let Some(window) = window {
                object.insert("windowId".to_string(), serde_json::json!(WireId(window)));
            }
        }
        let result: PaneIdResult = self.request("noa.newTab", body)?;
        Ok(result.pane_id.0)
    }

    /// Whether the connected Noa takes `board: true` on `noa.newTab`
    /// ([`Self::spawn_board_tab`]): `noa.hello` listed
    /// [`crate::CAPABILITY_BOARD_TAB`].
    pub fn supports_board_tab(&self) -> bool {
        self.has_capability(crate::protocol::CAPABILITY_BOARD_TAB)
    }

    /// Whether this connection's `noa.hello` listed `capability`.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Whether the connected Noa serves the `noa.timeline.*` methods
    /// (`docs/specs/board-timeline.md` D5): `noa.hello` listed
    /// [`crate::CAPABILITY_BOARD_TIMELINE_V1`]. An older one answers them
    /// with -32601.
    pub fn supports_board_timeline(&self) -> bool {
        self.has_capability(crate::protocol::CAPABILITY_BOARD_TIMELINE_V1)
    }

    pub fn supports_repo_widgets(&self) -> bool {
        self.has_capability(crate::widget::CAPABILITY_REPO_WIDGETS_V1)
    }

    /// One typed repository widget call. A mutation must be retried with
    /// the same request id after an ambiguous transport failure.
    pub fn widget(&mut self, call: &crate::widget::WidgetCall) -> Result<Value, ClientError> {
        call.validate()
            .map_err(|err| ClientError::InvalidInput(err.to_string()))?;
        self.request(call.method(), call.params())
    }

    /// `noa.timeline.context`.
    pub fn timeline_context(
        &mut self,
        params: &timeline::ContextParams,
    ) -> Result<timeline::ContextResult, ClientError> {
        self.request(timeline::METHOD_CONTEXT, params)
    }

    /// `noa.timeline.boards`: one page, newest activity first. For the next
    /// page while `hasMore`, pass the result's `next_before` back as
    /// `params.before`.
    pub fn timeline_boards(
        &mut self,
        params: &timeline::BoardsParams,
    ) -> Result<timeline::BoardsResult, ClientError> {
        self.request(timeline::METHOD_BOARDS, params)
    }

    /// `noa.timeline.registerRun`.
    pub fn timeline_register_run(
        &mut self,
        params: &timeline::RegisterRunParams,
    ) -> Result<timeline::RegisterRunResult, ClientError> {
        self.request(timeline::METHOD_REGISTER_RUN, params)
    }

    /// `noa.timeline.setPublishPolicy`.
    pub fn timeline_set_publish_policy(
        &mut self,
        params: &timeline::SetPublishPolicyParams,
    ) -> Result<timeline::RunInfo, ClientError> {
        self.request(timeline::METHOD_SET_PUBLISH_POLICY, params)
    }

    /// `noa.timeline.post`. A `ClientError::Rpc` code names the failure's
    /// kind through [`timeline::TimelineErrorKind::from_code`].
    pub fn timeline_post(
        &mut self,
        params: &timeline::PostParams,
    ) -> Result<timeline::PostResult, ClientError> {
        self.request(timeline::METHOD_POST, params)
    }

    /// `noa.timeline.list`.
    pub fn timeline_list(
        &mut self,
        params: &timeline::ListParams,
    ) -> Result<timeline::PageResult, ClientError> {
        self.request(timeline::METHOD_LIST, params)
    }

    /// `noa.timeline.get`.
    pub fn timeline_get(
        &mut self,
        params: &timeline::GetParams,
    ) -> Result<timeline::GetResult, ClientError> {
        self.request(timeline::METHOD_GET, params)
    }

    /// `noa.timeline.changes`.
    pub fn timeline_changes(
        &mut self,
        params: &timeline::ChangesParams,
    ) -> Result<timeline::PageResult, ClientError> {
        self.request(timeline::METHOD_CHANGES, params)
    }

    /// `noa.split` with a placement (`auto` included) and agent-orchestration
    /// spawn extensions.
    pub fn spawn_split(
        &mut self,
        pane: u64,
        placement: SplitPlacement,
        params: &SpawnParams,
    ) -> Result<u64, ClientError> {
        let mut body = serde_json::to_value(params).map_err(|_| {
            ClientError::InvalidInput("spawn parameters are not serializable".to_string())
        })?;
        if let Some(object) = body.as_object_mut() {
            object.insert("paneId".to_string(), serde_json::json!(WireId(pane)));
            object.insert("direction".to_string(), serde_json::json!(placement));
        }
        let result: PaneIdResult = self.request("noa.split", body)?;
        Ok(result.pane_id.0)
    }

    /// `noa.sendText`. `paste: false` writes the UTF-8 bytes raw.
    pub fn send_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), ClientError> {
        self.send_text_with(
            pane,
            text,
            SendTextOptions {
                paste,
                require_paste_mode: false,
                refuse_shell: false,
            },
        )
    }

    /// `noa.sendText` with every option. With `require_paste_mode` (and
    /// `paste`), a pane that has not enabled bracketed paste yet is refused
    /// with `ClientError::Rpc { code: -32010, .. }` and nothing is written;
    /// a server that predates the option ignores it and sends as usual.
    pub fn send_text_with(
        &mut self,
        pane: u64,
        text: &str,
        options: SendTextOptions,
    ) -> Result<(), ClientError> {
        let mut params =
            serde_json::json!({ "paneId": WireId(pane), "text": text, "paste": options.paste });
        if options.require_paste_mode {
            params["requirePasteMode"] = Value::Bool(true);
        }
        if options.refuse_shell {
            params["refuseShell"] = Value::Bool(true);
        }
        self.request_ok("noa.sendText", params)
    }

    /// `noa.subscribe`; `panes: None` subscribes to every pane. Returns the
    /// subscription id. Notifications then arrive through
    /// [`Self::next_notification`].
    pub fn subscribe(
        &mut self,
        events: &[EventKind],
        panes: Option<&[u64]>,
    ) -> Result<u64, ClientError> {
        self.subscribe_with(events, panes, None)
    }

    /// [`Self::subscribe`] with a `state_changed` field filter: with
    /// `fields`, a changed panel is delivered only when one of these fields
    /// changed (added panels and removals always are). `fields: None` sends
    /// no `fields` key, so the request is the one [`Self::subscribe`] sent
    /// before the filter existed.
    pub fn subscribe_with(
        &mut self,
        events: &[EventKind],
        panes: Option<&[u64]>,
        fields: Option<&[PanelField]>,
    ) -> Result<u64, ClientError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct SubscribeResult {
            subscription_id: WireId,
        }
        let mut params = serde_json::json!({
            "events": events,
            "paneIds": panes.map(|panes| panes.iter().copied().map(WireId).collect::<Vec<_>>()),
        });
        if let Some(fields) = fields {
            params["fields"] = serde_json::json!(fields);
        }
        let result: SubscribeResult = self.request("noa.subscribe", params)?;
        Ok(result.subscription_id.0)
    }

    /// `noa.updateSubscription`: changes one of this connection's
    /// subscriptions in place, keeping its id. A key left `None` keeps its
    /// value; `Some(None)` for `pane_ids` / `fields` clears that filter. An
    /// unknown id (or another connection's) is `ClientError::Rpc { code:
    /// -32602, .. }`; a server that predates the method answers -32601.
    pub fn update_subscription(
        &mut self,
        params: &UpdateSubscriptionParams,
    ) -> Result<(), ClientError> {
        self.request_ok("noa.updateSubscription", params)
    }

    /// The next notification, waiting up to `timeout`. `Ok(None)` when none
    /// arrived in time.
    pub fn next_notification(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Notification>, ClientError> {
        if let Some(notification) = self.pop_notification() {
            return Ok(Some(notification));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            set_socket_read_timeout(&mut self.socket, remaining)?;
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).map_err(|_| {
                        ClientError::Protocol("control message is not valid JSON".to_string())
                    })?;
                    // A late response to a request that already gave up has
                    // no owner left; skip it.
                    if let Ok(notification) = parse_notification(value) {
                        self.push_notification(notification);
                        return Ok(self.pop_notification());
                    }
                }
                Ok(Message::Ping(payload)) => self
                    .socket
                    .send(Message::Pong(payload))
                    .map_err(ClientError::transport)?,
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(frame)) => return Err(ClientError::closed(frame)),
                Ok(_) => {
                    return Err(ClientError::Protocol(
                        "control channel received a non-text data frame".to_string(),
                    ));
                }
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(ClientError::transport(error)),
            }
        }
    }

    fn push_notification(&mut self, notification: Notification) {
        if self.notifications.len() >= MAX_BUFFERED_NOTIFICATIONS {
            self.notifications.pop_front();
            self.notifications_dropped = true;
        }
        self.notifications.push_back(notification);
    }

    fn pop_notification(&mut self) -> Option<Notification> {
        let mut notification = self.notifications.pop_front()?;
        if std::mem::take(&mut self.notifications_dropped) {
            notification.mark_dropped();
        }
        Some(notification)
    }

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
        self.request_ok("noa.detach", serde_json::json!({ "paneId": WireId(pane) }))
    }

    pub fn resize_pane(&mut self, pane: u64, cols: u16, rows: u16) -> Result<(), ClientError> {
        if cols == 0 || rows == 0 {
            return Err(ClientError::InvalidInput(
                "remote pane dimensions must be non-zero".to_string(),
            ));
        }
        self.request_ok(
            "noa.resizePane",
            serde_json::json!({
                "paneId": WireId(pane),
                "cols": cols,
                "rows": rows,
            }),
        )
    }

    pub fn close_pane(&mut self, pane: u64) -> Result<(), ClientError> {
        self.request_ok(
            "noa.closePane",
            serde_json::json!({ "paneId": WireId(pane) }),
        )
    }

    fn request_ok(&mut self, method: &str, params: impl Serialize) -> Result<(), ClientError> {
        let result: Value = self.request(method, params)?;
        match result.as_object().and_then(|object| object.get("ok")) {
            Some(Value::Bool(true)) => Ok(()),
            _ => Err(ClientError::Protocol(format!(
                "{method} did not answer an object with ok:true"
            ))),
        }
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
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        // Each request is one unfragmented text frame. Check its encoded
        // size, including JSON escaping and the envelope, before sending:
        // a 1009 close would also interrupt subscriptions on this socket.
        if request.len() > crate::server::MAX_WS_FRAME_SIZE {
            return Err(ClientError::InvalidInput(format!(
                "{method} request exceeds the {} byte frame limit ({} bytes)",
                crate::server::MAX_WS_FRAME_SIZE,
                request.len()
            )));
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.socket
            .send(Message::Text(request))
            .map_err(ClientError::transport)?;

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ClientError::request_timeout(method));
            }
            set_socket_read_timeout(&mut self.socket, remaining)?;
            let message = match self.socket.read() {
                Ok(message) => message,
                // The deadline passed with the connection still open: the
                // server is slow, not gone. A late response is skipped by
                // the next request (its id is not that request's).
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(ClientError::request_timeout(method));
                }
                Err(error) => return Err(ClientError::transport(error)),
            };
            match message {
                Message::Text(text) => match response_step(&text, id) {
                    ResponseStep::Notification(notification) => {
                        self.push_notification(notification);
                    }
                    // Notifications and responses for a different owner are
                    // outside this request's contract.
                    ResponseStep::Skip => {}
                    ResponseStep::Done(result) => return result,
                },
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
            .map_err(handshake_error)?;
    Ok(socket)
}

/// A failed websocket handshake: [`ClientError::HandshakeDropped`] when the
/// server ended the connection under it (EOF or reset after the TCP
/// connect), as one at its connection limit does past the few refusals it
/// answers with 1013; anything else (a timeout, an HTTP refusal) is
/// [`ClientError::Transport`].
fn handshake_error<Role: tungstenite::handshake::HandshakeRole>(
    error: tungstenite::HandshakeError<Role>,
) -> ClientError {
    use std::io::ErrorKind;
    use tungstenite::error::{Error, ProtocolError};
    let dropped = match &error {
        tungstenite::HandshakeError::Failure(
            Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::HandshakeIncomplete),
        ) => true,
        tungstenite::HandshakeError::Failure(Error::Io(io)) => matches!(
            io.kind(),
            ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
                | ErrorKind::UnexpectedEof
        ),
        _ => false,
    };
    if dropped {
        ClientError::HandshakeDropped(error.to_string())
    } else {
        ClientError::Transport(error.to_string())
    }
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

/// What one text message means to a request waiting for response `id`.
#[derive(Debug)]
enum ResponseStep<T> {
    /// A notification, kept for [`Client::next_notification`].
    Notification(Notification),
    /// A response for another request; keep reading.
    Skip,
    /// The request's outcome.
    Done(Result<T, ClientError>),
}

fn protocol_error<T>(message: &str) -> ResponseStep<T> {
    ResponseStep::Done(Err(ClientError::Protocol(message.to_string())))
}

fn response_step<T: DeserializeOwned>(text: &str, id: u64) -> ResponseStep<T> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return protocol_error("control response is not valid JSON-RPC");
    };
    let value = match parse_notification(value) {
        Ok(notification) => return ResponseStep::Notification(notification),
        Err(value) => value,
    };
    let Ok(response) = RpcResponse::from_value(value) else {
        return protocol_error("control response is not valid JSON-RPC");
    };
    if response.id.as_u64() != Some(id) {
        return ResponseStep::Skip;
    }
    if response.jsonrpc.as_deref() != Some("2.0") {
        return protocol_error("control response has an invalid jsonrpc version");
    }
    if let Some(error) = response.error {
        return ResponseStep::Done(Err(ClientError::Rpc {
            code: error.code,
            message: error.message,
        }));
    }
    let Some(result) = response.result else {
        return protocol_error("control response has no result");
    };
    match serde_json::from_value(result) {
        Ok(result) => ResponseStep::Done(Ok(result)),
        Err(_) => protocol_error("control result has an invalid shape"),
    }
}

/// A JSON-RPC notification (a `method` and no `id`), or the value back for
/// a response. A notification's panels are moved out of it, not copied.
fn parse_notification(mut value: Value) -> Result<Notification, Value> {
    if value.get("id").is_some_and(|id| !id.is_null()) {
        return Err(value);
    }
    let Some(state_changed) = value
        .get("method")
        .and_then(Value::as_str)
        .map(|method| method == "noa.stateChanged")
    else {
        return Err(value);
    };
    let params = value.get_mut("params");
    let dropped = params
        .as_ref()
        .and_then(|params| params.get("dropped"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if state_changed {
        let (panels, removed) = match params {
            Some(params) => (
                params
                    .get_mut("panels")
                    .and_then(|panels| serde_json::from_value::<Vec<Panel>>(panels.take()).ok()),
                params
                    .get_mut("removedPaneIds")
                    .map_or(Some(Vec::new()), |ids| removed_pane_ids(ids.take())),
            ),
            None => (None, Some(Vec::new())),
        };
        // A panel or removal list this client cannot read is reported as a
        // loss, so the caller re-reads state instead of missing a change.
        return Ok(match (panels, removed) {
            (Some(panels), Some(removed_pane_ids)) => Notification::StateChanged {
                panels,
                removed_pane_ids,
                dropped,
            },
            _ => Notification::StateChanged {
                panels: Vec::new(),
                removed_pane_ids: Vec::new(),
                dropped: true,
            },
        });
    }
    let method = value["method"].as_str().unwrap_or_default().to_string();
    Ok(Notification::Other { method, dropped })
}

/// A present `removedPaneIds` value as pane ids; `None` when it is not a list
/// of ids (`null` included: the server omits the key when it has none).
fn removed_pane_ids(ids: Value) -> Option<Vec<u64>> {
    let ids = serde_json::from_value::<Vec<WireId>>(ids).ok()?;
    Some(ids.into_iter().map(|WireId(id)| id).collect())
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

impl RpcResponse {
    /// `serde_json::from_value`, with `result` moved out of the object
    /// rather than rebuilt from it: the result is the bulk of a reply (a
    /// pane listing), and is converted again into the caller's type.
    fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        // A sequence is a struct too, by position; leave that to serde.
        let Value::Object(mut object) = value else {
            return serde_json::from_value(value);
        };
        let mut field = |key| object.remove(key).unwrap_or(Value::Null);
        // Each field as its `#[serde(default)]` type reads it: `null` and
        // absent are the same.
        Ok(RpcResponse {
            jsonrpc: serde_json::from_value(field("jsonrpc"))?,
            id: field("id"),
            result: Some(field("result")).filter(|result| !result.is_null()),
            error: serde_json::from_value(field("error"))?,
        })
    }
}

/// Options of [`Client::send_text_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendTextOptions {
    /// Send through the bracketed-paste-aware encoding (`false`: raw bytes).
    pub paste: bool,
    /// Refuse (`-32010`) instead of sending while the pane has not enabled
    /// bracketed paste. Only meaningful with `paste`.
    pub require_paste_mode: bool,
    /// Refuse (`-32010`) instead of sending while the pane's foreground
    /// process is a plain shell, probed by the server at the moment of the
    /// request (`refuseShell`).
    pub refuse_shell: bool,
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
    /// The server ended the connection during the websocket handshake,
    /// after the TCP connect succeeded: a server at its connection limit
    /// drops the refusals past the few it closes with 1013, so retrying
    /// later can succeed.
    #[error("the server dropped the connection during the websocket handshake: {0}")]
    HandshakeDropped(String),
    /// A request's response did not arrive within its deadline while the
    /// connection stayed open: the server may still answer (late), so this
    /// is not a lost connection.
    #[error("request timed out: {0}")]
    Timeout(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("{ATTACH_HANDSHAKE_CLOSE_REASON}")]
    AttachHandshakeFailure,
    /// The server is at its connection limit and closed this connection
    /// with 1013 (Try Again Later); retrying later can succeed.
    #[error("the server has too many connections open (websocket closed: {0})")]
    TooManyConnections(String),
    /// The server closed the connection with 1009 (Message Too Big): a
    /// request was over its frame or message size bound.
    #[error("a request was too large for the server (websocket closed: {0})")]
    MessageTooBig(String),
    #[error("websocket closed: {0}")]
    Closed(String),
}

impl ClientError {
    fn transport(error: tungstenite::Error) -> Self {
        ClientError::Transport(error.to_string())
    }

    fn request_timeout(method: &str) -> Self {
        ClientError::Timeout(format!(
            "no response to {method} within the request deadline"
        ))
    }

    fn closed(frame: Option<tungstenite::protocol::CloseFrame<'static>>) -> Self {
        match frame {
            Some(frame) if frame.reason.starts_with("-32008") => {
                ClientError::AttachHandshakeFailure
            }
            Some(frame) if frame.code == CloseCode::Again => {
                ClientError::TooManyConnections(format!("{} {}", frame.code, frame.reason))
            }
            Some(frame) if frame.code == CloseCode::Size => {
                ClientError::MessageTooBig(format!("{} {}", frame.code, frame.reason))
            }
            Some(frame) => ClientError::Closed(format!("{} {}", frame.code, frame.reason)),
            None => ClientError::Closed("without close frame".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::OkResult;
    use std::net::TcpListener;
    use std::thread;

    const PANEL: &str = r##"{"windowGroupId":"1","windowId":"1","paneId":"7","name":"w","cwd":"/r","busy":false,"attention":true,"preview":[{"row":0,"spans":[{"text":"a","fg":"#5fc4a0"},{"text":"b","fg":3}]}],"agent":{"kind":"claude","state":"running","seq":4},"parentPaneId":"1","label":"x"}"##;

    /// Messages a request can receive, each an edge of `response_step`.
    fn response_corpus() -> Vec<String> {
        let ok = format!(r#"{{"panels":[{PANEL}]}}"#);
        let mut dup_field = PANEL.replacen(r#""name":"w""#, r#""name":"w","name":"v""#, 1);
        dup_field = format!(r#"{{"panels":[{dup_field}]}}"#);
        [
            format!(r#"{{"id":5,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"jsonrpc":"2.0","result":{ok},"id":5}}"#),
            format!(r#"{{"id":5,"result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":"1.0","result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":2,"result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":null,"result":{ok}}}"#),
            r#"{"error":{"code":-32004,"message":"pane closed"},"id":5,"jsonrpc":"2.0"}"#.into(),
            format!(r#"{{"error":{{"code":-1,"message":"m"}},"id":5,"jsonrpc":"2.0","result":{ok}}}"#),
            r#"{"error":{"code":-1,"message":"m"},"id":5,"jsonrpc":"2.0","result":7}"#.into(),
            r#"{"error":{"code":"x","message":"m"},"id":5,"jsonrpc":"2.0"}"#.into(),
            format!(r#"{{"error":null,"id":5,"jsonrpc":"2.0","result":{ok}}}"#),
            r#"{"id":5,"jsonrpc":"2.0","result":null}"#.into(),
            r#"{"id":5,"jsonrpc":"2.0"}"#.into(),
            r#"{"id":5,"jsonrpc":"2.0","result":{"panels":7}}"#.into(),
            r#"{"id":5,"jsonrpc":"2.0","result":[]}"#.into(),
            format!(r#"{{"id":4,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":"5","jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":5.0,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":-5,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":null,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":"2.0","method":"noa.x","result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":"2.0","result":{ok},"extra":[1,{{"a":2}}]}}"#),
            format!(r#"{{"id":5,"jsonrpc":"2.0","result":{{"panels":[],"more":1}},"result":{ok}}}"#),
            format!(r#"{{"id":5,"id":4,"jsonrpc":"2.0","result":{ok}}}"#),
            format!(r#"{{"id":5,"jsonrpc":"2.0","result":{dup_field}}}"#),
            format!(r#"{{"jsonrpc":"2.0","method":"noa.stateChanged","params":{{"panels":[{PANEL}]}}}}"#),
            r#"{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"dropped":true,"panels":[]}}"#.into(),
            r#"{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"panels":[{"bad":1}]}}"#.into(),
            r#"{"jsonrpc":"2.0","method":"noa.output","params":{"dropped":true}}"#.into(),
            r#"{"id":null,"method":"noa.output"}"#.into(),
            r#"{"method":7,"id":5,"jsonrpc":"2.0","result":{"panels":[]}}"#.into(),
            format!(r#"{{"id":5,"jsonrpc":"2.0","result":{ok}}} trailing"#),
            format!(r#"  {{"id":5,"jsonrpc":"2.0","result":{ok}}}  "#),
            r#"[{"id":5,"jsonrpc":"2.0","result":{"panels":[]}}]"#.into(),
            r#"{"id":18446744073709551615,"jsonrpc":"2.0","result":{"panels":[]}}"#.into(),
            "not json".into(),
            String::new(),
        ]
        .into()
    }

    fn step_text<T: DeserializeOwned + std::fmt::Debug>(text: &str, id: u64) -> String {
        format!("{:?}", response_step::<T>(text, id))
    }

    /// [`response_step`] as it read every reply before the result was
    /// moved out: the envelope through serde's derive.
    fn response_step_by_derive<T: DeserializeOwned>(text: &str, id: u64) -> ResponseStep<T> {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return protocol_error("control response is not valid JSON-RPC");
        };
        if let Some(notification) = parse_notification_by_ref(&value) {
            return ResponseStep::Notification(notification);
        }
        let Ok(response) = serde_json::from_value::<RpcResponse>(value) else {
            return protocol_error("control response is not valid JSON-RPC");
        };
        if response.id.as_u64() != Some(id) {
            return ResponseStep::Skip;
        }
        if response.jsonrpc.as_deref() != Some("2.0") {
            return protocol_error("control response has an invalid jsonrpc version");
        }
        if let Some(error) = response.error {
            return ResponseStep::Done(Err(ClientError::Rpc {
                code: error.code,
                message: error.message,
            }));
        }
        let Some(result) = response.result else {
            return protocol_error("control response has no result");
        };
        match serde_json::from_value(result) {
            Ok(result) => ResponseStep::Done(Ok(result)),
            Err(_) => protocol_error("control result has an invalid shape"),
        }
    }

    /// [`parse_notification`] as it was, reading a copy of the panels.
    fn parse_notification_by_ref(value: &Value) -> Option<Notification> {
        if value.get("id").is_some_and(|id| !id.is_null()) {
            return None;
        }
        let method = value.get("method")?.as_str()?;
        let params = value.get("params");
        let dropped = params
            .and_then(|params| params.get("dropped"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if method == "noa.stateChanged" {
            let panels = params
                .and_then(|params| params.get("panels"))
                .and_then(|panels| serde_json::from_value::<Vec<Panel>>(panels.clone()).ok());
            let removed = match params.and_then(|params| params.get("removedPaneIds")) {
                None => Some(Vec::new()),
                Some(ids) => serde_json::from_value::<Vec<WireId>>(ids.clone())
                    .ok()
                    .map(|ids| ids.into_iter().map(|id| id.0).collect()),
            };
            return Some(match (panels, removed) {
                (Some(panels), Some(removed_pane_ids)) => Notification::StateChanged {
                    panels,
                    removed_pane_ids,
                    dropped,
                },
                _ => Notification::StateChanged {
                    panels: Vec::new(),
                    removed_pane_ids: Vec::new(),
                    dropped: true,
                },
            });
        }
        Some(Notification::Other {
            method: method.to_string(),
            dropped,
        })
    }

    // REL-10/REL-04: the server's "busy" (1013) and "too big" (1009)
    // closes each have their own error, so a caller can tell them from the
    // server going away.
    #[test]
    fn busy_and_too_big_closes_are_their_own_errors() {
        use std::borrow::Cow;
        use tungstenite::protocol::CloseFrame;
        let close = |code, reason: &'static str| {
            ClientError::closed(Some(CloseFrame {
                code,
                reason: Cow::Borrowed(reason),
            }))
        };
        assert!(matches!(
            close(CloseCode::Again, crate::server::TOO_MANY_CONNECTIONS_REASON),
            ClientError::TooManyConnections(_)
        ));
        assert!(matches!(
            close(CloseCode::Size, crate::server::MESSAGE_TOO_BIG_REASON),
            ClientError::MessageTooBig(_)
        ));
        assert!(matches!(
            close(CloseCode::Away, "server shutdown"),
            ClientError::Closed(_)
        ));
    }

    // The limit leaves room for the request around the text inside the
    // server's 256 KiB frame bound.
    #[test]
    fn the_largest_send_text_fits_one_server_frame() {
        let text = "\"".repeat(MAX_SEND_TEXT_BYTES / 2 - 1);
        let encoded = serde_json::to_string(&text).unwrap();
        assert!(encoded.len() <= MAX_SEND_TEXT_BYTES);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": u64::MAX,
            "method": "noa.sendText",
            "params": {
                "paneId": WireId(u64::MAX),
                "text": text,
                "paste": true,
                "requirePasteMode": true,
            },
        });
        assert!(request.to_string().len() <= crate::server::MAX_WS_FRAME_SIZE);
    }

    fn client_with_replies(replies: Vec<Value>) -> (Client, thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let config = WebSocketConfig {
                max_frame_size: Some(crate::server::MAX_WS_FRAME_SIZE),
                ..WebSocketConfig::default()
            };
            let mut socket = tungstenite::accept_with_config(stream, Some(config)).unwrap();
            let hello = serde_json::json!({
                "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                "grantedScopes": [],
                "serverVersion": "t",
            });
            let mut requests = Vec::new();
            for result in std::iter::once(hello).chain(replies) {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("text request expected");
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"], "result": result,
                        })
                        .to_string(),
                    ))
                    .unwrap();
                requests.push(request);
            }
            requests
        });
        let client = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::empty(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        (client, server)
    }

    #[test]
    fn oversized_requests_are_not_sent_and_leave_the_connection_usable() {
        let (mut client, server) = client_with_replies(vec![
            serde_json::json!({ "ok": true }),
            serde_json::json!({ "panels": [] }),
        ]);
        let overhead = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "noa.sendText",
            "params": { "paneId": "7", "text": "", "paste": false },
        })
        .to_string()
        .len();
        let at_limit = "x".repeat(crate::server::MAX_WS_FRAME_SIZE - overhead);
        client.send_text(7, &at_limit, false).unwrap();

        // The whole encoded frame is bounded, including its envelope,
        // escaping and multibyte text, rather than just the input length.
        for oversized in [
            format!("{at_limit}x"),
            "\n".repeat(crate::server::MAX_WS_FRAME_SIZE / 2),
            "あ".repeat(crate::server::MAX_WS_FRAME_SIZE / 3 + 1),
        ] {
            let result = client.send_text(7, &oversized, false);
            assert!(
                matches!(result, Err(ClientError::InvalidInput(_))),
                "{result:?}"
            );
        }
        let spawn = client.spawn_tab(
            None,
            &SpawnParams {
                config: Some(SpawnConfig {
                    env: [(
                        "PROMPT".to_string(),
                        "x".repeat(crate::server::MAX_WS_FRAME_SIZE),
                    )]
                    .into(),
                    ..SpawnConfig::default()
                }),
                ..SpawnParams::default()
            },
        );
        assert!(
            matches!(spawn, Err(ClientError::InvalidInput(_))),
            "{spawn:?}"
        );
        assert!(client.list_panels().unwrap().is_empty());
        drop(client);

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1]["method"], "noa.sendText");
        assert_eq!(requests[1]["params"]["text"], at_limit);
        assert_eq!(requests[2]["method"], "noa.listPanels");
    }

    #[test]
    fn mutation_methods_require_an_object_with_ok_true() {
        type Operation = fn(&mut Client) -> Result<(), ClientError>;
        let operations: [(&str, Operation); 6] = [
            ("noa.focusPane", |client| client.focus_pane(7)),
            ("noa.sendText", |client| client.send_text(7, "hello", false)),
            ("noa.detach", |client| client.detach(7)),
            ("noa.resizePane", |client| client.resize_pane(7, 80, 24)),
            ("noa.closePane", |client| client.close_pane(7)),
            ("noa.updateSubscription", |client| {
                client.update_subscription(&UpdateSubscriptionParams {
                    subscription_id: WireId(1),
                    events: None,
                    pane_ids: None,
                    fields: None,
                })
            }),
        ];
        let rejected = [
            serde_json::json!({ "ok": false }),
            serde_json::json!([true]),
            serde_json::json!({}),
            serde_json::json!({ "ok": "true" }),
        ];
        let replies = rejected
            .iter()
            .cloned()
            .chain(std::iter::once(serde_json::json!({ "ok": true })))
            .flat_map(|reply| std::iter::repeat_n(reply, operations.len()))
            .collect();
        let (mut client, server) = client_with_replies(replies);
        for response in rejected {
            for (method, operation) in operations {
                let result = operation(&mut client);
                assert!(
                    matches!(&result, Err(ClientError::Protocol(message)) if message.contains(method)),
                    "{method} accepted {response}: {result:?}"
                );
            }
        }
        for (_, operation) in operations {
            operation(&mut client).unwrap();
        }
        drop(client);
        let requests = server.join().unwrap();
        for (index, request) in requests.iter().skip(1).enumerate() {
            assert_eq!(request["method"], operations[index % operations.len()].0);
        }
    }

    #[test]
    fn a_notification_reads_as_from_a_copy_of_its_panels() {
        let mut corpus = response_corpus();
        corpus.extend(
            [
                r#"{"method":"noa.stateChanged"}"#,
                r#"{"method":"noa.stateChanged","params":7}"#,
                r#"{"method":"noa.stateChanged","params":[{"panels":[]}]}"#,
                r#"{"method":"noa.stateChanged","params":{"dropped":"yes","panels":[]}}"#,
                r#"{"method":"noa.stateChanged","params":{"dropped":false,"panels":null}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"dropped":true},"id":null}"#,
                r#"{"method":"noa.output","params":{"dropped":true,"panels":[]},"id":0}"#,
                r#"{"method":"","params":{}}"#,
                r#"{"method":["noa.stateChanged"]}"#,
                r#"[{"method":"noa.stateChanged"}]"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":["13"]}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":["13","14"],"dropped":true}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":[13]}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":[]}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":null}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":7}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":["x"]}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":[{"a":1}]}}"#,
                r#"{"method":"noa.stateChanged","params":{"panels":[{"bad":1}],"removedPaneIds":["13"]}}"#,
                r#"{"method":"noa.stateChanged","params":{"removedPaneIds":["13"]}}"#,
                r#"{"method":"noa.output","params":{"removedPaneIds":7}}"#,
            ]
            .map(String::from),
        );
        corpus.push(format!(
            r#"{{"method":"noa.stateChanged","params":{{"panels":[{PANEL}],"removedPaneIds":["8"]}}}}"#
        ));
        for text in &corpus {
            let Ok(value) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            let old = parse_notification_by_ref(&value);
            match parse_notification(value.clone()) {
                Ok(notification) => {
                    assert_eq!(
                        format!("{old:?}"),
                        format!("{:?}", Some(notification)),
                        "{text}"
                    );
                }
                Err(back) => {
                    assert!(old.is_none(), "{text}");
                    assert_eq!(back, value, "a response comes back untouched: {text}");
                }
            }
        }
    }

    fn state_changed(text: &str) -> (Vec<Panel>, Vec<u64>, bool) {
        let value = serde_json::from_str::<Value>(text).unwrap();
        match parse_notification(value) {
            Ok(Notification::StateChanged {
                panels,
                removed_pane_ids,
                dropped,
            }) => (panels, removed_pane_ids, dropped),
            other => panic!("not a stateChanged: {other:?}"),
        }
    }

    #[test]
    fn removed_pane_ids_are_read_when_present() {
        let (panels, removed, dropped) = state_changed(&format!(
            r#"{{"jsonrpc":"2.0","method":"noa.stateChanged","params":{{"panels":[{PANEL}],"removedPaneIds":["13","14"],"dropped":true}}}}"#
        ));
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].pane_id, WireId(7));
        assert_eq!(removed, [13, 14]);
        assert!(dropped);
    }

    #[test]
    fn absent_removed_pane_ids_read_as_none_removed() {
        let (panels, removed, dropped) = state_changed(&format!(
            r#"{{"jsonrpc":"2.0","method":"noa.stateChanged","params":{{"panels":[{PANEL}]}}}}"#
        ));
        assert_eq!(panels.len(), 1);
        assert!(removed.is_empty());
        assert!(!dropped);
    }

    #[test]
    fn a_removal_only_frame_reads_as_removals_with_no_panels() {
        let (panels, removed, dropped) = state_changed(
            r#"{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"panels":[],"removedPaneIds":["13"]}}"#,
        );
        assert!(panels.is_empty());
        assert_eq!(removed, [13]);
        assert!(!dropped);
    }

    // A removal list this client cannot read may hide a closed pane, so it
    // reads as a loss (re-list), the same as an unreadable `panels`, and
    // the readable panels beside it are not delivered either.
    #[test]
    fn malformed_removed_pane_ids_read_as_a_loss() {
        for ids in [
            "7",
            "null",
            r#"["x"]"#,
            r#"[{"a":1}]"#,
            r#""13""#,
            r#"{"13":1}"#,
        ] {
            let (panels, removed, dropped) = state_changed(&format!(
                r#"{{"jsonrpc":"2.0","method":"noa.stateChanged","params":{{"panels":[{PANEL}],"removedPaneIds":{ids}}}}}"#
            ));
            assert!(panels.is_empty(), "{ids}");
            assert!(removed.is_empty(), "{ids}");
            assert!(dropped, "{ids}");
        }
    }

    #[test]
    fn a_reply_reads_as_through_the_derived_envelope() {
        let mut corpus = response_corpus();
        // Envelopes serde's derive also reads by position, and odd fields.
        corpus.extend(
            [
                r#"["2.0",5,{"panels":[]}]"#,
                r#"["2.0",5,null,[-1,"m"]]"#,
                r#"["2.0"]"#,
                r#"{"id":5,"jsonrpc":"2.0","error":[-2,"seq"]}"#,
                r#"{"id":5,"jsonrpc":"2.0","error":{"code":1,"message":"m","data":{}}}"#,
                r#"{"id":5,"jsonrpc":"2.0","error":{"code":1}}"#,
                r#"{"id":5,"jsonrpc":"2.0","error":7,"result":{"panels":[]}}"#,
                r#"{"id":5,"jsonrpc":["2.0"],"result":{"panels":[]}}"#,
                r#"{"id":{"n":5},"jsonrpc":"2.0","result":{"panels":[]}}"#,
                r#"5"#,
                r#"null"#,
                r#""text""#,
            ]
            .map(String::from),
        );
        for id in [4, 5, u64::MAX] {
            for text in &corpus {
                assert_eq!(
                    step_text::<ListPanelsResult>(text, id),
                    format!(
                        "{:?}",
                        response_step_by_derive::<ListPanelsResult>(text, id)
                    ),
                    "{text}"
                );
                assert_eq!(
                    step_text::<Value>(text, id),
                    format!("{:?}", response_step_by_derive::<Value>(text, id)),
                    "{text}"
                );
                assert_eq!(
                    step_text::<OkResult>(text, id),
                    format!("{:?}", response_step_by_derive::<OkResult>(text, id)),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn response_steps_are_pinned() {
        let corpus = response_corpus();
        let steps: Vec<String> = corpus
            .iter()
            .map(|text| step_text::<ListPanelsResult>(text, 5))
            .collect();
        fn kind(step: &str) -> &str {
            if step.starts_with("Done(Ok") {
                "ok"
            } else if step.starts_with("Done(Err(Rpc") {
                "rpc"
            } else if step.starts_with("Done(Err(Protocol") {
                "protocol"
            } else if step.starts_with("Notification") {
                "notification"
            } else {
                step
            }
        }
        let kinds: Vec<&str> = steps.iter().map(|step| kind(step)).collect();
        assert_eq!(
            kinds,
            [
                "ok",
                "ok",
                "protocol",
                "protocol",
                "protocol",
                "protocol",
                "rpc",
                "rpc",
                "rpc",
                "protocol",
                "ok",
                "protocol",
                "protocol",
                "protocol",
                "protocol",
                "Skip",
                "Skip",
                "Skip",
                "Skip",
                "Skip",
                "Skip",
                "ok",
                "ok",
                "ok",
                "Skip",
                "ok",
                "notification",
                "notification",
                "notification",
                "notification",
                "notification",
                "ok",
                "protocol",
                "ok",
                "protocol",
                "Skip",
                "protocol",
                "protocol",
            ]
        );
        // The last of duplicated keys wins, as a `Value` keeps it.
        assert!(steps[23].contains(r#"name: "w""#), "{}", steps[23]);
        assert!(steps[25].contains(r#"name: "v""#), "{}", steps[25]);
        assert!(steps[6].contains("-32004") && steps[6].contains("pane closed"));
        assert!(steps[29].contains("dropped: true"), "{}", steps[29]);
    }

    // Review finding #6: only a connection the server ended under the
    // handshake is retried by `+ctl`; a timeout or an HTTP refusal is not.
    #[test]
    fn a_dropped_handshake_is_told_from_other_failures() {
        use tungstenite::error::{Error, ProtocolError};
        type Failed = tungstenite::HandshakeError<
            tungstenite::handshake::client::ClientHandshake<std::io::Cursor<Vec<u8>>>,
        >;
        for error in [
            Error::Protocol(ProtocolError::HandshakeIncomplete),
            Error::ConnectionClosed,
            Error::Io(std::io::ErrorKind::ConnectionReset.into()),
            Error::Io(std::io::ErrorKind::BrokenPipe.into()),
        ] {
            let mapped = handshake_error(Failed::Failure(error));
            assert!(
                matches!(mapped, ClientError::HandshakeDropped(_)),
                "{mapped:?}"
            );
        }
        for error in [
            Error::Io(std::io::ErrorKind::TimedOut.into()),
            Error::Io(std::io::ErrorKind::WouldBlock.into()),
            Error::Protocol(ProtocolError::WrongHttpMethod),
        ] {
            let mapped = handshake_error(Failed::Failure(error));
            assert!(matches!(mapped, ClientError::Transport(_)), "{mapped:?}");
        }
    }

    // Review finding #6: past its few 1013 refusals, a full server drops a
    // connection during the handshake; that read as an unreachable Noa.
    #[test]
    fn a_connection_dropped_during_the_handshake_is_handshake_dropped() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });
        let result = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::empty(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        assert!(
            matches!(result, Err(ClientError::HandshakeDropped(_))),
            "{:?}",
            result.err()
        );
        server.join().unwrap();
    }

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
        assert!(matches!(result, Err(ClientError::Timeout(_))));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    // Review finding: a request past its deadline on a live connection read
    // as a lost connection (`Transport`), which ended a long `+ctl wait`.
    // It is its own error, and the connection keeps working afterwards: the
    // late response is skipped by the next request.
    #[test]
    fn a_request_past_its_deadline_is_a_timeout_and_the_connection_survives() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let id = |message: Message| -> u64 {
                let Message::Text(text) = message else {
                    panic!("text request expected");
                };
                serde_json::from_str::<Value>(&text).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            };
            let hello = id(socket.read().unwrap());
            let reply = |id: u64, result: Value| {
                Message::Text(
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                )
            };
            socket
                .send(reply(
                    hello,
                    serde_json::json!({
                        "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                        "grantedScopes": [],
                        "serverVersion": "t",
                    }),
                ))
                .unwrap();
            // The first listing is answered only after the client gave up.
            let slow = id(socket.read().unwrap());
            thread::sleep(Duration::from_millis(150));
            socket
                .send(reply(slow, serde_json::json!({ "panels": [] })))
                .unwrap();
            let next = id(socket.read().unwrap());
            socket
                .send(reply(next, serde_json::json!({ "panels": [] })))
                .unwrap();
            let _ = socket.read();
        });

        let mut client = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::empty(),
            Duration::from_secs(2),
            Duration::from_millis(50),
        )
        .unwrap();
        // The hello's serverVersion is kept for the client's own requests.
        assert_eq!(client.server_version(), "t");
        let first: Result<ListPanelsResult, _> = client.request_with_timeout(
            "noa.listPanels",
            serde_json::json!({}),
            Duration::from_millis(50),
        );
        assert!(matches!(first, Err(ClientError::Timeout(_))), "{first:?}");
        thread::sleep(Duration::from_millis(200));
        assert!(client.list_panels().unwrap().is_empty());
        drop(client);
        server.join().unwrap();
    }

    // AC-4: `subscribe` sends the request it sent before `fields` existed
    // (no `fields` key); `subscribe_with` adds the key only when asked.
    #[test]
    fn subscribe_sends_no_fields_key_and_subscribe_with_sends_one() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut requests = Vec::new();
            for result in [
                serde_json::json!({
                    "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                    "grantedScopes": ["read"],
                    "serverVersion": "t",
                }),
                serde_json::json!({ "subscriptionId": "1" }),
                serde_json::json!({ "subscriptionId": "2" }),
            ] {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("text request expected");
                };
                let id = serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
                socket
                    .send(Message::Text(
                        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
                            .to_string(),
                    ))
                    .unwrap();
                requests.push(text);
            }
            requests
        });

        let mut client = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::from_strings(["read"]),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        let events = [EventKind::StateChanged];
        assert_eq!(client.subscribe(&events, Some(&[3])).unwrap(), 1);
        assert_eq!(
            client
                .subscribe_with(&events, None, Some(&[PanelField::Agent]))
                .unwrap(),
            2
        );
        drop(client);
        let requests = server.join().unwrap();

        // The request exactly as `subscribe` built it before `fields`.
        assert_eq!(
            requests[1],
            r#"{"id":2,"jsonrpc":"2.0","method":"noa.subscribe","params":{"events":["state_changed"],"paneIds":["3"]}}"#
        );
        let with_fields: Value = serde_json::from_str(&requests[2]).unwrap();
        assert_eq!(
            with_fields["params"],
            serde_json::json!({
                "events": ["state_changed"],
                "paneIds": null,
                "fields": ["agent"],
            })
        );
    }

    // AC-3: only `{"ok":true}` confirms an update; `ok:false` is not success.
    #[test]
    fn update_subscription_rejects_a_negative_ok_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for result in [
                serde_json::json!({
                    "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                    "grantedScopes": ["read"],
                    "serverVersion": "t",
                }),
                serde_json::json!({ "ok": false }),
                serde_json::json!({ "ok": true }),
            ] {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("text request expected");
                };
                let id = serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
                socket
                    .send(Message::Text(
                        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
                            .to_string(),
                    ))
                    .unwrap();
            }
        });

        let mut client = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::from_strings(["read"]),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        let params = UpdateSubscriptionParams {
            subscription_id: WireId(1),
            events: None,
            pane_ids: None,
            fields: None,
        };
        let negative = client.update_subscription(&params);
        assert!(
            matches!(negative, Err(ClientError::Protocol(_))),
            "{negative:?}"
        );
        client.update_subscription(&params).unwrap();
        drop(client);
        server.join().unwrap();
    }

    // Only a JSON object with `"ok": true` confirms an update; serde would
    // otherwise read `OkResult` from the array `[true]` by position.
    #[test]
    fn update_subscription_rejects_a_non_object_result() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for result in [
                serde_json::json!({
                    "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                    "grantedScopes": ["read"],
                    "serverVersion": "t",
                }),
                serde_json::json!([true]),
                serde_json::json!({ "ok": true }),
            ] {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("text request expected");
                };
                let id = serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
                socket
                    .send(Message::Text(
                        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
                            .to_string(),
                    ))
                    .unwrap();
            }
        });

        let mut client = Client::connect_with_timeouts(
            &url,
            "token",
            ScopeSet::from_strings(["read"]),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        let params = UpdateSubscriptionParams {
            subscription_id: WireId(1),
            events: None,
            pane_ids: None,
            fields: None,
        };
        let array = client.update_subscription(&params);
        assert!(matches!(array, Err(ClientError::Protocol(_))), "{array:?}");
        client.update_subscription(&params).unwrap();
        drop(client);
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

    // agent-orchestration D20: the board gets the client's PATH and locale,
    // which a Noa started from the Dock lacks, unless the request set them;
    // the rest of the request and of the environment is left alone.
    #[test]
    fn a_board_tab_takes_the_clients_path_and_locale() {
        let given = SpawnParams {
            config: Some(SpawnConfig {
                cwd: Some("/repo".to_string()),
                env: [("LANG".to_string(), "C".to_string())].into(),
                ..SpawnConfig::default()
            }),
            focus: Some(false),
            auto_approve: Some(true),
            ..SpawnParams::default()
        };
        let vars = [
            ("PATH", "/opt/bin:/usr/bin"),
            ("LANG", "ja_JP.UTF-8"),
            ("LC_CTYPE", "UTF-8"),
            ("HOME", "/Users/x"),
            ("NOA_TOKEN", "secret"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()));

        let params = board_tab_params(&given, vars);

        let config = params.config.as_ref().unwrap();
        assert_eq!(config.env["PATH"], "/opt/bin:/usr/bin");
        assert_eq!(config.env["LANG"], "C");
        assert_eq!(config.env["LC_CTYPE"], "UTF-8");
        assert_eq!(config.env.len(), 3);
        assert_eq!(config.cwd.as_deref(), Some("/repo"));
        assert_eq!(config.command, None);
        assert_eq!(params.focus, Some(false));
        assert_eq!(params.auto_approve, Some(true));
        assert_eq!(params.label, None);
        // Without a config of its own, one is made for the env.
        let bare = board_tab_params(
            &SpawnParams::default(),
            std::iter::once(("PATH".to_string(), "/usr/bin".to_string())),
        );
        assert_eq!(bare.config.unwrap().env["PATH"], "/usr/bin");
    }
}
