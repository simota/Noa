//! Connection-level tests against a real loopback TCP + tungstenite client,
//! exercising the acceptance criteria in `docs/specs/noa-server.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use noa_ipc::backend::{GridResult, IpcBackend, PaneRef, TextResult, WindowRef};
use noa_ipc::error::IpcError;
use noa_ipc::protocol::{Panel, Row, SplitDirection, TextSource, WireId};
use noa_ipc::{ScopeSet, Server, ServerConfig};
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

#[derive(Default)]
struct MockBackend {
    panels: Mutex<Vec<Panel>>,
    closed_panes: Mutex<std::collections::HashSet<u64>>,
    sent_text: Mutex<Vec<(u64, String)>>,
    focused: Mutex<Vec<u64>>,
    grid_rows: Mutex<HashMap<u64, Vec<Row>>>,
    text: Mutex<HashMap<u64, String>>,
    /// Panes that make `get_text` return `IpcError::Internal` (R-3 test).
    internal_error_panes: Mutex<std::collections::HashSet<u64>>,
}

impl IpcBackend for MockBackend {
    fn list_panels(&self) -> Vec<Panel> {
        self.panels.lock().unwrap().clone()
    }

    fn get_text(&self, pane: PaneRef, _source: TextSource, _max_bytes: usize) -> Result<TextResult, IpcError> {
        if self.internal_error_panes.lock().unwrap().contains(&pane) {
            return Err(IpcError::Internal("backend exploded".to_string()));
        }
        if self.closed_panes.lock().unwrap().contains(&pane) {
            return Err(IpcError::PaneClosed);
        }
        match self.text.lock().unwrap().get(&pane) {
            Some(text) => Ok(TextResult { text: text.clone(), truncated: false }),
            None => Err(IpcError::UnknownPane),
        }
    }

    fn get_grid(&self, pane: PaneRef, start_row: u64, row_count: u64) -> Result<GridResult, IpcError> {
        let rows = self.grid_rows.lock().unwrap().get(&pane).cloned().ok_or(IpcError::UnknownPane)?;
        let rows: Vec<Row> = rows
            .into_iter()
            .filter(|r| r.row >= start_row && r.row < start_row + row_count)
            .collect();
        Ok(GridResult { cols: 80, rows, has_more: false })
    }

    fn send_text(&self, pane: PaneRef, text: &str) -> Result<(), IpcError> {
        if self.closed_panes.lock().unwrap().contains(&pane) {
            return Err(IpcError::PaneClosed);
        }
        self.sent_text.lock().unwrap().push((pane, text.to_string()));
        Ok(())
    }

    fn focus_pane(&self, pane: PaneRef) -> Result<(), IpcError> {
        if self.closed_panes.lock().unwrap().contains(&pane) {
            return Err(IpcError::PaneClosed);
        }
        self.focused.lock().unwrap().push(pane);
        Ok(())
    }

    fn new_tab(&self, _window: Option<WindowRef>) -> Result<PaneRef, IpcError> {
        Ok(999)
    }

    fn split(&self, pane: PaneRef, _direction: SplitDirection) -> Result<PaneRef, IpcError> {
        if self.closed_panes.lock().unwrap().contains(&pane) {
            return Err(IpcError::PaneClosed);
        }
        Ok(pane + 1000)
    }

    fn close_pane(&self, pane: PaneRef) -> Result<(), IpcError> {
        self.closed_panes.lock().unwrap().insert(pane);
        Ok(())
    }
}

fn start_test_server(backend: Arc<MockBackend>, token: &str, scopes: ScopeSet) -> noa_ipc::ServerHandle {
    Server::start(
        ServerConfig {
            port: 0,
            token: token.to_string(),
            allowed_scopes: scopes,
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
        },
        backend,
    )
    .expect("server should bind an ephemeral loopback port")
}

fn start_test_server_with_hello_deadline(
    backend: Arc<MockBackend>,
    token: &str,
    scopes: ScopeSet,
    hello_deadline: Duration,
) -> noa_ipc::ServerHandle {
    Server::start(
        ServerConfig { port: 0, token: token.to_string(), allowed_scopes: scopes, hello_deadline },
        backend,
    )
    .expect("server should bind an ephemeral loopback port")
}

type Sock = WebSocket<MaybeTlsStream<std::net::TcpStream>>;

fn connect_plain(port: u16) -> Sock {
    let url = format!("ws://127.0.0.1:{port}/");
    let (socket, _resp) = tungstenite::connect(url).expect("connect should succeed");
    socket
}

fn connect_with_bearer(port: u16, token: &str) -> Sock {
    let url = format!("ws://127.0.0.1:{port}/");
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (socket, _resp) = tungstenite::connect(request).expect("connect should succeed");
    socket
}

fn send_rpc(sock: &mut Sock, id: i64, method: &str, params: Value) {
    let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    sock.send(Message::Text(req.to_string())).unwrap();
}

fn recv_json(sock: &mut Sock) -> Value {
    loop {
        match sock.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

fn hello(sock: &mut Sock, id: i64, protocol_version: u64, token: Option<&str>, scopes: &[&str]) -> Value {
    send_rpc(sock, id, "noa.hello", json!({ "protocolVersion": protocol_version, "token": token, "scopes": scopes }));
    recv_json(sock)
}

// ---- AC-4: no/wrong token -> -32001, no backend method invoked ----

#[test]
fn ac4_missing_token_rejected() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend.clone(), "secret-token", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());

    let resp = hello(&mut sock, 1, 1, None, &["read"]);
    assert_eq!(resp["error"]["code"], -32001);

    send_rpc(&mut sock, 2, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32001);

    let _ = sock.close(None);
}

#[test]
fn ac4_wrong_token_rejected() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "secret-token", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());

    let resp = hello(&mut sock, 1, 1, Some("wrong"), &["read"]);
    assert_eq!(resp["error"]["code"], -32001);
    let _ = sock.close(None);
}

// ---- AC-5 / AC-6 / AC-20: scope matrix ----

#[test]
fn ac5_read_only_client_cannot_send_text_or_focus() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend.clone(), "tok", ScopeSet::from_strings(["read", "control", "input"]));
    let mut sock = connect_plain(handle.port());
    let resp = hello(&mut sock, 1, 1, Some("tok"), &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));

    send_rpc(&mut sock, 2, "noa.sendText", json!({ "paneId": "1", "text": "hi" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32003);
    assert!(backend.sent_text.lock().unwrap().is_empty());

    send_rpc(&mut sock, 3, "noa.focusPane", json!({ "paneId": "1" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32003);
    assert!(backend.focused.lock().unwrap().is_empty());

    let _ = sock.close(None);
}

#[test]
fn ac6_control_without_input_cannot_send_text() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::from_strings(["read", "control", "input"]));
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read", "control"]);

    send_rpc(&mut sock, 2, "noa.sendText", json!({ "paneId": "1", "text": "hi" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32003);
    let _ = sock.close(None);
}

#[test]
fn ac20_default_scopes_grant_read_only_even_if_more_requested() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    let resp = hello(&mut sock, 1, 1, Some("tok"), &["read", "control", "input"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = sock.close(None);
}

#[test]
fn ac20_read_input_config_grants_input_not_control() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::from_strings(["read", "input"]));
    let mut sock = connect_plain(handle.port());
    let resp = hello(&mut sock, 1, 1, Some("tok"), &["read", "control", "input"]);
    let granted = resp["result"]["grantedScopes"].as_array().unwrap();
    let granted: Vec<&str> = granted.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(granted.contains(&"input"));
    assert!(!granted.contains(&"control"));
    let _ = sock.close(None);
}

// ---- AC-7 / AC-21: hello version mismatch, unknown fields, unknown method ----

#[test]
fn ac7_version_mismatch_rejected() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    let resp = hello(&mut sock, 1, 999, Some("tok"), &["read"]);
    assert_eq!(resp["error"]["code"], -32006);
    let _ = sock.close(None);
}

#[test]
fn ac7_unknown_fields_ignored() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    send_rpc(
        &mut sock,
        1,
        "noa.hello",
        json!({ "protocolVersion": 1, "token": "tok", "scopes": ["read"], "somethingUnknown": 42 }),
    );
    let resp = recv_json(&mut sock);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = sock.close(None);
}

#[test]
fn ac21_unknown_method_then_connection_still_works() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.nonexistent", json!({}));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32601);

    send_rpc(&mut sock, 3, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert!(resp.get("result").is_some(), "connection must still serve requests: {resp:?}");
    let _ = sock.close(None);
}

// ---- AC-9: truncation helper ----

#[test]
fn ac9_truncation_helper_is_tail_priority() {
    let (text, truncated) = noa_ipc::protocol::truncate_tail("hello world", 5);
    assert!(truncated);
    assert_eq!(text, "world");

    let (text, truncated) = noa_ipc::protocol::truncate_tail("short", 100);
    assert!(!truncated);
    assert_eq!(text, "short");
}

#[test]
fn ac9_get_text_over_the_wire_truncates_and_flags() {
    let backend = Arc::new(MockBackend::default());
    backend.text.lock().unwrap().insert(1, "x".repeat(1000));
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getText", json!({ "paneId": "1", "source": "screen", "maxBytes": 100 }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["result"]["truncated"], true);
    assert_eq!(resp["result"]["text"].as_str().unwrap().len(), 100);
    let _ = sock.close(None);
}

// ---- AC-10: getGrid paging ----

#[test]
fn ac10_get_grid_returns_range_only_rows() {
    let backend = Arc::new(MockBackend::default());
    let rows: Vec<Row> = (0..50).map(|i| Row { row: i, spans: vec![] }).collect();
    backend.grid_rows.lock().unwrap().insert(1, rows);
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getGrid", json!({ "paneId": "1", "startRow": 10, "rowCount": 5 }));
    let resp = recv_json(&mut sock);
    let rows = resp["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["row"], 10);
    assert_eq!(rows[4]["row"], 14);
    let _ = sock.close(None);
}

#[test]
fn ac10_get_grid_has_more_when_capped() {
    let backend = Arc::new(MockBackend::default());
    let big_text = "y".repeat(5000);
    let rows: Vec<Row> = (0..200)
        .map(|i| Row { row: i, spans: vec![noa_ipc::protocol::Span { text: big_text.clone(), fg: None, bg: None, attrs: None }] })
        .collect();
    backend.grid_rows.lock().unwrap().insert(1, rows);
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getGrid", json!({ "paneId": "1", "startRow": 0, "rowCount": 200 }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["result"]["hasMore"], true);
    let rows = resp["result"]["rows"].as_array().unwrap();
    assert!(rows.len() < 200);
    let _ = sock.close(None);
}

// ---- AC-15: unknown pane / pane closed / oversize ----

#[test]
fn ac15_unknown_pane_returns_32002() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getText", json!({ "paneId": "999", "source": "screen" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32002);
    let _ = sock.close(None);
}

#[test]
fn ac15_pane_closed_returns_32004() {
    let backend = Arc::new(MockBackend::default());
    backend.closed_panes.lock().unwrap().insert(5);
    let handle = start_test_server(backend, "tok", ScopeSet::from_strings(["read", "control"]));
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read", "control"]);

    send_rpc(&mut sock, 2, "noa.focusPane", json!({ "paneId": "5" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32004);
    let _ = sock.close(None);
}

#[test]
fn ac15_oversize_single_row_returns_32005() {
    let backend = Arc::new(MockBackend::default());
    let huge = "z".repeat(300 * 1024);
    backend
        .grid_rows
        .lock()
        .unwrap()
        .insert(1, vec![Row { row: 0, spans: vec![noa_ipc::protocol::Span { text: huge, fg: None, bg: None, attrs: None }] }]);
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getGrid", json!({ "paneId": "1", "startRow": 0, "rowCount": 1 }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32005);
    let _ = sock.close(None);
}

// ---- R-3: backend Internal error surfaces as -32603 ----

#[test]
fn r3_backend_internal_error_maps_to_32603() {
    let backend = Arc::new(MockBackend::default());
    backend.internal_error_panes.lock().unwrap().insert(1);
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.getText", json!({ "paneId": "1", "source": "screen" }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32603);
    let _ = sock.close(None);
}

// ---- R-5: JSON-RPC envelope validation ----

#[test]
fn r5_missing_jsonrpc_field_rejected() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());

    // Pre-auth: `noa.hello` itself must be rejected without a valid `jsonrpc`.
    let req = json!({ "id": 1, "method": "noa.hello", "params": { "protocolVersion": 1, "token": "tok", "scopes": ["read"] } });
    sock.send(Message::Text(req.to_string())).unwrap();
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32600);

    // Connection stays open: a compliant hello now succeeds.
    let resp = hello(&mut sock, 2, 1, Some("tok"), &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));

    // Post-auth: a subsequent method without `jsonrpc` is also rejected.
    let req = json!({ "id": 3, "method": "noa.listPanels", "params": {} });
    sock.send(Message::Text(req.to_string())).unwrap();
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32600);

    // Connection still open after the rejection.
    send_rpc(&mut sock, 4, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert!(resp.get("result").is_some());
    let _ = sock.close(None);
}

#[test]
fn r5_wrong_jsonrpc_version_rejected() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    let req = json!({ "jsonrpc": "1.0", "id": 2, "method": "noa.listPanels", "params": {} });
    sock.send(Message::Text(req.to_string())).unwrap();
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32600);

    // Connection stays open.
    send_rpc(&mut sock, 3, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert!(resp.get("result").is_some());
    let _ = sock.close(None);
}

// ---- R-1: Server::start refuses an empty configured token ----

#[test]
fn server_start_refuses_empty_token() {
    let backend = Arc::new(MockBackend::default());
    let result = Server::start(
        ServerConfig {
            port: 0,
            token: String::new(),
            allowed_scopes: ScopeSet::default_read_only(),
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
        },
        backend,
    );
    assert!(result.is_err(), "an empty token must never be accepted as a live server config");
}

#[test]
fn server_start_refuses_whitespace_only_token() {
    let backend = Arc::new(MockBackend::default());
    let result = Server::start(
        ServerConfig {
            port: 0,
            token: "   ".to_string(),
            allowed_scopes: ScopeSet::default_read_only(),
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
        },
        backend,
    );
    assert!(result.is_err());
}

// ---- R-2: unauthenticated connections cannot hold a slot forever ----

#[test]
fn r2_connection_without_hello_is_closed_after_the_hello_deadline() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server_with_hello_deadline(
        backend,
        "tok",
        ScopeSet::default_read_only(),
        Duration::from_millis(200),
    );
    let mut sock = connect_plain(handle.port());
    // Complete the WS handshake (implicit in `connect_plain`) but never send
    // `noa.hello`. Past the shortened deadline the server must close the
    // connection on its own rather than waiting on the client forever.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let result = loop {
        match sock.read() {
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {
                assert!(std::time::Instant::now() < deadline, "server never closed the idle connection");
                continue;
            }
            other => break other,
        }
    };
    assert!(result.is_err(), "connection past its hello deadline must be closed, got {result:?}");
}

#[test]
fn r2_slot_freed_by_a_reaped_connection_is_available_to_a_new_client() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server_with_hello_deadline(
        backend,
        "tok",
        ScopeSet::default_read_only(),
        Duration::from_millis(150),
    );

    // Open a connection and never send hello; let it sit past the deadline
    // so the server reaps it and frees its slot.
    let mut stalled = connect_plain(handle.port());
    std::thread::sleep(Duration::from_millis(400));
    // Drain until the socket reports the server-initiated close (R-2's
    // "socket reads EOF/close" acceptance bar).
    let closed = loop {
        match stalled.read() {
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            other => break other,
        }
    };
    assert!(closed.is_err(), "the stalled connection's socket must observe the close");

    // A fresh connection can still complete a full hello -> request
    // round-trip, proving the server isn't wedged (and, if this were run at
    // MAX_CONNECTIONS, that the reaped slot was released).
    let mut fresh = connect_plain(handle.port());
    let resp = hello(&mut fresh, 1, 1, Some("tok"), &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = fresh.close(None);
}

// ---- R-4: parse error (-32700) vs invalid request (-32600) ----

#[test]
fn r4_malformed_json_is_parse_error() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());

    sock.send(Message::Text("not json".to_string())).unwrap();
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32700);

    // Connection stays open.
    let resp = hello(&mut sock, 1, 1, Some("tok"), &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = sock.close(None);
}

#[test]
fn r4_well_formed_json_that_is_not_a_valid_request_is_invalid_request() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());

    for malformed in [json!([]), json!({}), json!({ "method": 1 })] {
        sock.send(Message::Text(malformed.to_string())).unwrap();
        let resp = recv_json(&mut sock);
        assert_eq!(resp["error"]["code"], -32600, "malformed request {malformed} should be -32600");
    }

    // Connection stays open after every rejection.
    let resp = hello(&mut sock, 1, 1, Some("tok"), &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = sock.close(None);
}

// ---- header-based auth (pre-authed via Authorization: Bearer) ----

#[test]
fn bearer_header_preauth_still_requires_hello_before_other_methods() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_with_bearer(handle.port(), "tok");

    // hello still required to establish protocol version / granted scopes.
    send_rpc(&mut sock, 1, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["error"]["code"], -32001);

    // hello with no token in params succeeds because the header already authed.
    let resp = hello(&mut sock, 2, 1, None, &["read"]);
    assert_eq!(resp["result"]["grantedScopes"], json!(["read"]));
    let _ = sock.close(None);
}

// ---- push: subscribe / unsubscribe -> state_changed + output ----

#[test]
fn subscribe_delivers_state_changed_and_unsubscribe_stops_it() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let broadcaster = handle.broadcaster();
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    send_rpc(&mut sock, 2, "noa.subscribe", json!({ "events": ["state_changed"] }));
    let resp = recv_json(&mut sock);
    let sub_id = resp["result"]["subscriptionId"].as_str().unwrap().to_string();

    let panel = Panel {
        window_group_id: WireId(1),
        window_id: WireId(1),
        pane_id: WireId(1),
        name: "shell".into(),
        cwd: "/tmp".into(),
        branch: None,
        process: None,
        busy: false,
        attention: false,
        preview: vec![],
    };
    broadcaster.broadcast_state_changed(vec![panel]);

    // wait past the connection's 50ms read-timeout poll interval.
    std::thread::sleep(Duration::from_millis(150));
    let notif = recv_json(&mut sock);
    assert_eq!(notif["method"], "noa.stateChanged");

    send_rpc(&mut sock, 3, "noa.unsubscribe", json!({ "subscriptionId": sub_id }));
    let resp = recv_json(&mut sock);
    assert_eq!(resp["result"]["ok"], true);

    broadcaster.broadcast_state_changed(vec![]);
    // nothing further should arrive; probe with a subsequent request/response
    // round-trip instead of a fixed sleep+assert-silence race.
    send_rpc(&mut sock, 4, "noa.listPanels", json!({}));
    let resp = recv_json(&mut sock);
    assert!(resp.get("result").is_some());
    let _ = sock.close(None);
}

// ---- F-3: bounded WS message size ----

#[test]
fn f3_oversized_message_closes_the_connection() {
    let backend = Arc::new(MockBackend::default());
    let handle = start_test_server(backend, "tok", ScopeSet::default_read_only());
    let mut sock = connect_plain(handle.port());
    hello(&mut sock, 1, 1, Some("tok"), &["read"]);

    // Over the server's 1 MiB max_message_size (F-3): the connection must be
    // torn down rather than the server accepting an unbounded payload.
    let oversized = "x".repeat(2 * 1024 * 1024);
    let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "noa.sendText", "params": { "paneId": "1", "text": oversized } });
    let _ = sock.send(Message::Text(req.to_string()));

    let result = loop {
        match sock.read() {
            Ok(Message::Text(_)) => continue,
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            other => break other,
        }
    };
    assert!(result.is_err(), "oversized message should close the connection, got {result:?}");
}
