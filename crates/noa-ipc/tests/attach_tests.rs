//! Deterministic protocol tests for the raw noa client-mode attach channel.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use noa_ipc::backend::{GridResult, IpcBackend, PaneRef, TextResult, WindowRef};
use noa_ipc::client::{Client, ClientError};
use noa_ipc::error::IpcError;
use noa_ipc::protocol::{Panel, SplitDirection, TextSource, WireId};
use noa_ipc::{
    ATTACH_HANDSHAKE_CLOSE_REASON, AttachOutputSender, Broadcaster, ScopeSet, Server, ServerConfig,
};
use serde_json::{Value, json};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Socket = WebSocket<MaybeTlsStream<std::net::TcpStream>>;

static QA_TEST_LOCK: Mutex<()> = Mutex::new(());

struct TestBackend {
    seed: Mutex<Vec<u8>>,
    closed: Mutex<HashSet<PaneRef>>,
    outputs: Mutex<HashMap<(PaneRef, u64), AttachOutputSender>>,
    inputs: Mutex<Vec<(PaneRef, u64, Vec<u8>)>>,
    input_event: Sender<Instant>,
    input_rx: Receiver<Instant>,
    detached: Mutex<Vec<(PaneRef, u64)>>,
    detach_event: Sender<()>,
    detach_rx: Receiver<()>,
    resizes: Mutex<Vec<(PaneRef, u16, u16)>>,
    resize_event: Sender<()>,
    resize_rx: Receiver<()>,
}

impl TestBackend {
    fn new() -> Self {
        let (input_event, input_rx) = bounded(8);
        let (detach_event, detach_rx) = bounded(8);
        let (resize_event, resize_rx) = bounded(8);
        TestBackend {
            seed: Mutex::new(b"synthetic-seed".to_vec()),
            closed: Mutex::new(HashSet::new()),
            outputs: Mutex::new(HashMap::new()),
            inputs: Mutex::new(Vec::new()),
            input_event,
            input_rx,
            detached: Mutex::new(Vec::new()),
            detach_event,
            detach_rx,
            resizes: Mutex::new(Vec::new()),
            resize_event,
            resize_rx,
        }
    }

    fn output(&self) -> AttachOutputSender {
        self.outputs
            .lock()
            .unwrap()
            .values()
            .next()
            .expect("attach output should be registered before seed returns")
            .clone()
    }
}

impl IpcBackend for TestBackend {
    fn list_panels(&self) -> Vec<Panel> {
        vec![Panel {
            window_group_id: WireId(1),
            window_id: WireId(1),
            pane_id: WireId(1),
            name: "shell".to_string(),
            cwd: "/tmp".to_string(),
            branch: None,
            process: None,
            busy: false,
            attention: false,
            attachable: true,
            preview: Vec::new(),
        }]
    }

    fn get_text(
        &self,
        pane: PaneRef,
        source: TextSource,
        max_bytes: usize,
    ) -> Result<TextResult, IpcError> {
        if pane != 1 {
            return Err(IpcError::UnknownPane);
        }
        assert_eq!(source, TextSource::Scrollback);
        assert_eq!(max_bytes, 1024);
        Ok(TextResult {
            text: "typed-scrollback".to_string(),
            truncated: false,
        })
    }

    fn get_grid(
        &self,
        _pane: PaneRef,
        _start_row: u64,
        _row_count: u64,
    ) -> Result<GridResult, IpcError> {
        Err(IpcError::Unsupported("get_grid"))
    }

    fn send_text(&self, _pane: PaneRef, _text: &str, _paste: bool) -> Result<(), IpcError> {
        Err(IpcError::Unsupported("send_text"))
    }

    fn focus_pane(&self, _pane: PaneRef) -> Result<(), IpcError> {
        Err(IpcError::Unsupported("focus_pane"))
    }

    fn new_tab(&self, window: Option<WindowRef>) -> Result<PaneRef, IpcError> {
        assert_eq!(window, None);
        Ok(2)
    }

    fn split(&self, pane: PaneRef, direction: SplitDirection) -> Result<PaneRef, IpcError> {
        assert_eq!(pane, 1);
        assert_eq!(direction, SplitDirection::Vertical);
        Ok(11)
    }

    fn close_pane(&self, pane: PaneRef) -> Result<(), IpcError> {
        if pane != 1 {
            return Err(IpcError::UnknownPane);
        }
        self.closed.lock().unwrap().insert(pane);
        Ok(())
    }

    fn validate_attach(&self, pane: PaneRef) -> Result<(), IpcError> {
        if !matches!(pane, 1 | 2) {
            return Err(IpcError::UnknownPane);
        }
        if self.closed.lock().unwrap().contains(&pane) {
            return Err(IpcError::PaneClosed);
        }
        Ok(())
    }

    fn open_attach(
        &self,
        pane: PaneRef,
        generation: u64,
        output: AttachOutputSender,
    ) -> Result<Vec<u8>, IpcError> {
        self.validate_attach(pane)?;
        self.outputs
            .lock()
            .unwrap()
            .insert((pane, generation), output);
        Ok(self.seed.lock().unwrap().clone())
    }

    fn write_attach(&self, pane: PaneRef, generation: u64, bytes: &[u8]) -> Result<(), IpcError> {
        if !self
            .outputs
            .lock()
            .unwrap()
            .contains_key(&(pane, generation))
        {
            return Err(IpcError::PaneClosed);
        }
        self.inputs
            .lock()
            .unwrap()
            .push((pane, generation, bytes.to_vec()));
        let _ = self.input_event.send(Instant::now());
        Ok(())
    }

    fn detach_attach(&self, pane: PaneRef, generation: u64) -> Result<(), IpcError> {
        self.outputs.lock().unwrap().remove(&(pane, generation));
        self.detached.lock().unwrap().push((pane, generation));
        let _ = self.detach_event.send(());
        Ok(())
    }

    fn resize_pane(&self, pane: PaneRef, cols: u16, rows: u16) -> Result<(), IpcError> {
        self.validate_attach(pane)?;
        self.resizes.lock().unwrap().push((pane, cols, rows));
        let _ = self.resize_event.send(());
        Ok(())
    }
}

fn start(scopes: ScopeSet) -> (Arc<TestBackend>, noa_ipc::ServerHandle) {
    let backend = Arc::new(TestBackend::new());
    let handle = start_with_broadcaster(backend.clone(), scopes, Broadcaster::new());
    (backend, handle)
}

fn start_with_broadcaster(
    backend: Arc<TestBackend>,
    scopes: ScopeSet,
    broadcaster: Broadcaster,
) -> noa_ipc::ServerHandle {
    let handle = Server::start(
        ServerConfig {
            port: 0,
            bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
            token: "control-token".to_string(),
            allowed_scopes: scopes,
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
            handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
        },
        backend,
        broadcaster,
    )
    .unwrap();
    handle
}

fn connect_control(port: u16, scopes: &[&str]) -> Socket {
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    send_rpc(
        &mut socket,
        1,
        "noa.hello",
        json!({
            "protocolVersion": 1,
            "token": "control-token",
            "scopes": scopes,
        }),
    );
    assert!(recv_json(&mut socket).get("result").is_some());
    socket
}

fn send_rpc(socket: &mut Socket, id: i64, method: &str, params: Value) {
    socket
        .send(Message::Text(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        ))
        .unwrap();
}

fn recv_json(socket: &mut Socket) -> Value {
    loop {
        match socket.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).unwrap(),
            Message::Pong(_) => {}
            other => panic!("unexpected control frame: {other:?}"),
        }
    }
}

fn reserve(socket: &mut Socket, id: i64) -> (String, String) {
    reserve_pane(socket, id, 1)
}

fn reserve_pane(socket: &mut Socket, id: i64, pane: u64) -> (String, String) {
    send_rpc(
        socket,
        id,
        "noa.attach",
        json!({ "paneId": pane.to_string() }),
    );
    let response = recv_json(socket);
    let url = response["result"]["attachUrl"]
        .as_str()
        .unwrap()
        .to_string();
    let token = response["result"]["attachToken"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, path) = url
        .strip_prefix("ws://")
        .and_then(|value| value.split_once('/'))
        .expect("attachUrl should be an absolute ws URL");
    assert_eq!(path, "attach");
    assert!(
        !url.contains(&token),
        "attachUrl must not contain its secret"
    );
    (url, token)
}

fn connect_attach(url: &str, token: String) -> Socket {
    let (mut raw, _) = tungstenite::connect(url).unwrap();
    raw.send(Message::Binary(token.into_bytes())).unwrap();
    assert_eq!(read_seed(&mut raw), b"synthetic-seed");
    raw
}

fn wait_for_connection_count(handle: &noa_ipc::ServerHandle, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while handle.active_connection_count() != expected && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(handle.active_connection_count(), expected);
}

fn set_read_timeout(socket: &mut Socket, timeout: Duration) {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(timeout)).unwrap(),
        _ => panic!("attach integration tests require a plain loopback socket"),
    }
}

fn wait_until_readable(socket: &mut Socket) {
    let mut byte = [0_u8; 1];
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            assert!(stream.peek(&mut byte).unwrap() > 0, "raw socket timed out")
        }
        _ => panic!("attach integration tests require a plain loopback socket"),
    }
}

fn read_binary(socket: &mut Socket) -> Vec<u8> {
    loop {
        match socket.read().unwrap() {
            Message::Binary(bytes) => return bytes,
            Message::Ping(payload) => socket.send(Message::Pong(payload)).unwrap(),
            Message::Pong(_) => {}
            Message::Close(frame) => panic!("raw attach closed unexpectedly: {frame:?}"),
            other => panic!("unexpected raw attach frame: {other:?}"),
        }
    }
}

fn read_seed(socket: &mut Socket) -> Vec<u8> {
    let mut seed = Vec::new();
    loop {
        match socket.read().unwrap() {
            Message::Binary(bytes) if bytes.is_empty() => return seed,
            Message::Binary(bytes) => seed.extend_from_slice(&bytes),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).unwrap(),
            Message::Pong(_) => {}
            Message::Close(frame) => panic!("raw attach closed during seed: {frame:?}"),
            other => panic!("unexpected raw attach seed frame: {other:?}"),
        }
    }
}

fn load_chunk(index: usize, len: usize) -> Vec<u8> {
    let mut chunk = vec![(index % 251) as u8; len];
    chunk[..size_of::<u64>()].copy_from_slice(&(index as u64).to_le_bytes());
    chunk
}

fn latency_stats(samples: &[Duration]) -> (Duration, Duration) {
    assert!(!samples.is_empty());
    let mean_nanos = samples.iter().map(Duration::as_nanos).sum::<u128>() / samples.len() as u128;
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let p95_index = (sorted.len() * 95).div_ceil(100) - 1;
    (
        Duration::from_nanos(mean_nanos.try_into().unwrap()),
        sorted[p95_index],
    )
}

#[test]
fn attach_scope_is_explicit_and_conflicts_until_detach() {
    let (_backend, handle) = start(ScopeSet::default_read_only());
    let (mut socket, _) =
        tungstenite::connect(format!("ws://127.0.0.1:{}/", handle.port())).unwrap();
    send_rpc(
        &mut socket,
        1,
        "noa.hello",
        json!({
            "protocolVersion": 1,
            "token": "control-token",
            "scopes": ["attach"],
        }),
    );
    assert_eq!(recv_json(&mut socket)["result"]["grantedScopes"], json!([]));
    send_rpc(&mut socket, 2, "noa.attach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut socket)["error"]["code"], -32003);
    drop(socket);
    drop(handle);

    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut socket = connect_control(handle.port(), &["attach"]);
    send_rpc(
        &mut socket,
        2,
        "noa.resizePane",
        json!({ "paneId": "1", "cols": 90, "rows": 30 }),
    );
    assert_eq!(recv_json(&mut socket)["error"]["code"], -32007);

    let _ = reserve(&mut socket, 3);
    send_rpc(
        &mut socket,
        4,
        "noa.resizePane",
        json!({ "paneId": "1", "cols": 90, "rows": 30 }),
    );
    assert_eq!(recv_json(&mut socket)["result"]["ok"], true);
    backend
        .resize_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(backend.resizes.lock().unwrap()[0], (1, 90, 30));

    send_rpc(
        &mut socket,
        5,
        "noa.resizePane",
        json!({ "paneId": "1", "cols": 4096, "rows": 256 }),
    );
    assert_eq!(recv_json(&mut socket)["result"]["ok"], true);
    backend
        .resize_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    for (id, cols, rows) in [
        (6, 0, 1),
        (7, 4097, 1),
        (8, 1, 4097),
        (9, 4096, 257),
        (10, 65535, 65535),
    ] {
        send_rpc(
            &mut socket,
            id,
            "noa.resizePane",
            json!({ "paneId": "1", "cols": cols, "rows": rows }),
        );
        assert_eq!(recv_json(&mut socket)["error"]["code"], -32602);
    }
    assert_eq!(
        backend.resizes.lock().unwrap().as_slice(),
        &[(1, 90, 30), (1, 4096, 256)],
        "an unsafe grid size must be rejected before reaching the backend"
    );

    send_rpc(&mut socket, 11, "noa.attach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut socket)["error"]["code"], -32007);

    send_rpc(&mut socket, 12, "noa.detach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut socket)["result"]["ok"], true);
    let _ = reserve(&mut socket, 13);
}

#[test]
fn concurrent_attach_requests_yield_one_owner_and_one_conflict() {
    let (_backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut left = connect_control(handle.port(), &["attach"]);
    let mut right = connect_control(handle.port(), &["attach"]);
    let gate = Arc::new(std::sync::Barrier::new(3));

    let (left_response, right_response) = std::thread::scope(|scope| {
        let left_gate = Arc::clone(&gate);
        let left_socket = &mut left;
        let left_request = scope.spawn(move || {
            left_gate.wait();
            send_rpc(left_socket, 2, "noa.attach", json!({ "paneId": "1" }));
            recv_json(left_socket)
        });

        let right_gate = Arc::clone(&gate);
        let right_socket = &mut right;
        let right_request = scope.spawn(move || {
            right_gate.wait();
            send_rpc(right_socket, 2, "noa.attach", json!({ "paneId": "1" }));
            recv_json(right_socket)
        });

        gate.wait();
        (left_request.join().unwrap(), right_request.join().unwrap())
    });

    let left_won = left_response.get("result").is_some();
    let right_won = right_response.get("result").is_some();
    assert_ne!(left_won, right_won, "exactly one request must own the pane");

    let (winner_response, loser_response, winner_socket) = if left_won {
        (&left_response, &right_response, &mut left)
    } else {
        (&right_response, &left_response, &mut right)
    };
    assert_eq!(loser_response["error"]["code"], -32007);

    // The losing request must not disturb the winning reservation: its
    // one-time token still establishes the raw channel and receives a seed.
    let url = winner_response["result"]["attachUrl"].as_str().unwrap();
    let token = winner_response["result"]["attachToken"]
        .as_str()
        .unwrap()
        .to_string();
    let raw = connect_attach(url, token);

    send_rpc(winner_socket, 3, "noa.detach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(winner_socket)["result"]["ok"], true);
    drop(raw);
}

#[test]
fn delayed_detach_only_releases_the_calling_control_sessions_generation() {
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut old_control = connect_control(handle.port(), &["attach"]);
    let (old_url, old_token) = reserve(&mut old_control, 2);
    let mut old_raw = connect_attach(&old_url, old_token);
    old_raw.close(None).unwrap();
    drop(old_raw);
    backend
        .detach_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let mut new_control = connect_control(handle.port(), &["attach"]);
    let (new_url, new_token) = reserve(&mut new_control, 2);
    let mut new_raw = connect_attach(&new_url, new_token);
    let new_generation = backend
        .outputs
        .lock()
        .unwrap()
        .keys()
        .find_map(|&(pane, generation)| (pane == 1).then_some(generation))
        .expect("new generation should own the raw output");

    send_rpc(
        &mut old_control,
        3,
        "noa.resizePane",
        json!({ "paneId": "1", "cols": 120, "rows": 40 }),
    );
    assert_eq!(recv_json(&mut old_control)["error"]["code"], -32007);
    assert!(
        backend
            .resize_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a stale control session resized the new generation"
    );

    send_rpc(&mut old_control, 4, "noa.detach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut old_control)["result"]["ok"], true);
    assert!(
        backend
            .outputs
            .lock()
            .unwrap()
            .contains_key(&(1, new_generation)),
        "an old control session detached the new generation"
    );
    assert!(
        backend
            .detach_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "the delayed detach reached the backend"
    );

    new_raw
        .send(Message::Binary(b"still-owned".to_vec()))
        .unwrap();
    backend
        .input_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        backend.inputs.lock().unwrap().last().unwrap().2,
        b"still-owned"
    );
}

#[test]
fn overlapping_server_generations_share_attach_ownership() {
    let backend = Arc::new(TestBackend::new());
    let broadcaster = Broadcaster::new();
    let scopes = ScopeSet::from_strings(["attach"]);
    let old_server = start_with_broadcaster(backend.clone(), scopes, broadcaster.clone());

    let mut old_control = connect_control(old_server.port(), &["attach"]);
    let (old_url, old_token) = reserve(&mut old_control, 2);
    let mut old_raw = connect_attach(&old_url, old_token);

    // A config reload can start a fresh listener while connection threads
    // from the previous listener are still winding down. Model that overlap
    // deterministically by keeping both server generations alive.
    let new_server = start_with_broadcaster(backend.clone(), scopes, broadcaster);
    let mut new_control = connect_control(new_server.port(), &["attach"]);
    send_rpc(&mut new_control, 2, "noa.attach", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut new_control)["error"]["code"], -32007);

    old_raw.close(None).unwrap();
    drop(old_raw);
    backend
        .detach_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let (new_url, new_token) = reserve(&mut new_control, 3);
    let _new_raw = connect_attach(&new_url, new_token);

    send_rpc(
        &mut old_control,
        4,
        "noa.resizePane",
        json!({ "paneId": "1", "cols": 120, "rows": 40 }),
    );
    assert_eq!(recv_json(&mut old_control)["error"]["code"], -32007);
    assert!(
        backend
            .resize_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a stale server generation resized the new attach owner"
    );
}

#[test]
fn first_binary_token_is_secret_safe_one_time_and_mismatch_closes_1008() {
    let (_backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut control = connect_control(handle.port(), &["attach"]);
    let (url, token) = reserve(&mut control, 2);

    let (mut raw, _) = tungstenite::connect(&url).unwrap();
    raw.send(Message::Binary(b"wrong-token".to_vec())).unwrap();
    let frame = match raw.read().unwrap() {
        Message::Close(Some(frame)) => frame,
        other => panic!("expected close frame, got {other:?}"),
    };
    assert_eq!(frame.code, CloseCode::Policy);
    assert_eq!(frame.reason, ATTACH_HANDSHAKE_CLOSE_REASON);
    assert!(!frame.reason.contains(&token));

    // A random mismatch identifies no reservation. The actual token remains
    // valid, but is consumed before its successful connection becomes active.
    let (mut authenticated, _) = tungstenite::connect(&url).unwrap();
    authenticated
        .send(Message::Binary(token.as_bytes().to_vec()))
        .unwrap();
    assert_eq!(read_seed(&mut authenticated), b"synthetic-seed");

    let (mut replay, _) = tungstenite::connect(url).unwrap();
    replay.send(Message::Binary(token.into_bytes())).unwrap();
    let frame = match replay.read().unwrap() {
        Message::Close(Some(frame)) => frame,
        other => panic!("expected replay close frame, got {other:?}"),
    };
    assert_eq!(frame.code, CloseCode::Policy);
    assert_eq!(frame.reason, ATTACH_HANDSHAKE_CLOSE_REASON);
}

#[test]
fn typed_client_receives_seed_and_raw_bytes_and_sends_input_and_resize() {
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let control_url = format!("ws://127.0.0.1:{}/", handle.port());
    let mut control = Client::connect(
        &control_url,
        "control-token",
        ScopeSet::from_strings(["attach"]),
    )
    .unwrap();
    let mut raw = control.attach(1).unwrap();
    assert_eq!(raw.seed(), b"synthetic-seed");

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while handle.active_connection_count() != 2 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(handle.active_connection_count(), 2);

    // An idle 10 ms poll is not a disconnect; the same raw socket remains
    // usable for both later output and input.
    assert_eq!(raw.poll_raw().unwrap(), None);
    backend.output().send(b"live-output".to_vec()).unwrap();
    assert_eq!(raw.read_raw().unwrap(), b"live-output");

    raw.send_raw(b"client-input").unwrap();
    backend
        .input_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(backend.inputs.lock().unwrap()[0].2, b"client-input");

    control.resize_pane(1, 120, 40).unwrap();
    backend
        .resize_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(backend.resizes.lock().unwrap()[0], (1, 120, 40));

    control.detach(1).unwrap();
    backend
        .detach_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(backend.detached.lock().unwrap().len(), 1);
}

#[test]
fn typed_client_reassembles_seed_larger_than_one_websocket_frame() {
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let seed = (0..(300 * 1024 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    *backend.seed.lock().unwrap() = seed.clone();
    let control_url = format!("ws://127.0.0.1:{}/", handle.port());
    let mut control = Client::connect(
        &control_url,
        "control-token",
        ScopeSet::from_strings(["attach"]),
    )
    .unwrap();

    let raw = control.attach(1).unwrap();

    assert_eq!(raw.seed(), seed);
}

#[test]
fn typed_client_splits_large_input_without_changing_byte_order() {
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let control_url = format!("ws://127.0.0.1:{}/", handle.port());
    let mut control = Client::connect(
        &control_url,
        "control-token",
        ScopeSet::from_strings(["attach"]),
    )
    .unwrap();
    let mut raw = control.attach(1).unwrap();
    let input = (0..(300 * 1024 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();

    raw.send_raw(&input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let received = backend
            .inputs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, bytes)| bytes.len())
            .sum::<usize>();
        if received == input.len() {
            break;
        }
        assert!(Instant::now() < deadline, "large input was not delivered");
        let _ = backend.input_rx.recv_timeout(Duration::from_millis(10));
    }

    let received = backend
        .inputs
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, _, bytes)| bytes.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(received, input);
}

#[test]
fn typed_client_covers_read_and_control_calls_needed_by_client_mode() {
    let (backend, handle) = start(ScopeSet::from_strings(["read", "control"]));
    let control_url = format!("ws://127.0.0.1:{}/", handle.port());
    let mut client = Client::connect(
        &control_url,
        "control-token",
        ScopeSet::from_strings(["read", "control"]),
    )
    .unwrap();

    let panels = client.list_panels().unwrap();
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].pane_id, WireId(1));

    let text = client
        .get_text(1, TextSource::Scrollback, Some(1024))
        .unwrap();
    assert_eq!(text.pane_id, WireId(1));
    assert_eq!(text.text, "typed-scrollback");
    assert!(!text.truncated);

    assert_eq!(client.new_tab(None).unwrap(), 2);
    assert_eq!(client.split(1, SplitDirection::Vertical).unwrap(), 11);
    client.close_pane(1).unwrap();
    assert!(backend.closed.lock().unwrap().contains(&1));
}

#[test]
fn non_binary_raw_data_frame_is_rejected_and_close_pane_cleans_the_lease() {
    let (backend, handle) = start(ScopeSet::from_strings(["attach", "control"]));
    let mut control = connect_control(handle.port(), &["attach", "control"]);
    let (url, token) = reserve(&mut control, 2);
    let (mut raw, _) = tungstenite::connect(url).unwrap();
    raw.send(Message::Binary(token.into_bytes())).unwrap();
    assert_eq!(read_seed(&mut raw), b"synthetic-seed");
    raw.send(Message::Text("not raw bytes".to_string()))
        .unwrap();
    let frame = match raw.read().unwrap() {
        Message::Close(Some(frame)) => frame,
        other => panic!("expected policy close, got {other:?}"),
    };
    assert_eq!(frame.code, CloseCode::Policy);
    backend
        .detach_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    // Attach again, then close the pane through the existing control method.
    let (url, token) = reserve(&mut control, 3);
    let (mut raw, _) = tungstenite::connect(url).unwrap();
    raw.send(Message::Binary(token.into_bytes())).unwrap();
    assert_eq!(read_seed(&mut raw), b"synthetic-seed");
    send_rpc(&mut control, 4, "noa.closePane", json!({ "paneId": "1" }));
    assert_eq!(recv_json(&mut control)["result"]["ok"], true);
    backend
        .detach_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(backend.outputs.lock().unwrap().is_empty());
}

#[test]
fn typed_client_maps_attach_policy_close_to_handshake_error() {
    let (_backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut control = connect_control(handle.port(), &["attach"]);
    let (url, _token) = reserve(&mut control, 2);
    let invalid = noa_ipc::AttachResult {
        attach_url: url,
        attach_token: "wrong-token".to_string(),
    };
    assert!(matches!(
        noa_ipc::AttachClient::connect(1, &invalid),
        Err(ClientError::AttachHandshakeFailure)
    ));
}

#[test]
fn unknown_websocket_path_is_not_reported_as_an_attach_auth_failure() {
    let (_backend, handle) = start(ScopeSet::empty());
    let url = format!("ws://127.0.0.1:{}/unknown", handle.port());
    let (mut socket, _) = tungstenite::connect(url).unwrap();
    let frame = match socket.read().unwrap() {
        Message::Close(Some(frame)) => frame,
        other => panic!("expected policy close, got {other:?}"),
    };
    assert_eq!(frame.code, CloseCode::Policy);
    assert_eq!(frame.reason, "invalid websocket path");
    assert!(!frame.reason.contains("-32008"));
}

#[test]
fn attach_connection_counts_toward_the_existing_32_connection_cap() {
    let _qa_guard = QA_TEST_LOCK.lock().unwrap();
    let (_backend, handle) = start(ScopeSet::from_strings(["attach"]));

    let mut first_control = connect_control(handle.port(), &["attach"]);
    let (first_url, first_token) = reserve_pane(&mut first_control, 2, 1);
    let _first_raw = connect_attach(&first_url, first_token);

    // Reserve another pane while a slot is available. Its second WebSocket
    // is opened only after control + raw connections occupy all 32 slots.
    let mut second_control = connect_control(handle.port(), &["attach"]);
    let (second_url, _second_token) = reserve_pane(&mut second_control, 2, 2);
    let mut filler_controls = Vec::with_capacity(29);
    for _ in 0..29 {
        filler_controls.push(connect_control(handle.port(), &[]));
    }
    wait_for_connection_count(&handle, 32);

    assert!(
        tungstenite::connect(&second_url).is_err(),
        "the 33rd WebSocket must be refused before its attach handshake"
    );
    assert_eq!(handle.active_connection_count(), 32);
}

#[test]
fn attach_added_processing_latency_stays_below_one_millisecond() {
    let _qa_guard = QA_TEST_LOCK.lock().unwrap();
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut control = connect_control(handle.port(), &["attach"]);
    let (url, token) = reserve(&mut control, 2);
    let mut raw = connect_attach(&url, token);
    set_read_timeout(&mut raw, Duration::from_secs(2));

    const WARMUP_SAMPLES: usize = 64;
    const MEASURED_SAMPLES: usize = 512;
    const INPUT: &[u8] = b"latency-input";
    const OUTPUT: &[u8] = b"\x1b[38;5;42mlatency-output\x1b[0m\r\n";

    let mut input_latencies = Vec::with_capacity(MEASURED_SAMPLES);
    for sample in 0..WARMUP_SAMPLES + MEASURED_SAMPLES {
        let started = Instant::now();
        raw.send(Message::Binary(INPUT.to_vec())).unwrap();
        let backend_write = backend
            .input_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        if sample >= WARMUP_SAMPLES {
            input_latencies.push(backend_write.duration_since(started));
        }
    }

    let output = backend.output();
    let mut feed_sink = Vec::with_capacity((WARMUP_SAMPLES + MEASURED_SAMPLES) * OUTPUT.len());
    let mut receive_feed_latencies = Vec::with_capacity(MEASURED_SAMPLES);
    for sample in 0..WARMUP_SAMPLES + MEASURED_SAMPLES {
        output.send(OUTPUT.to_vec()).unwrap();
        // Socket readiness is awaited outside the measured region. The
        // budget is noa-added receive/decode/feed work, not network wait.
        wait_until_readable(&mut raw);
        let started = Instant::now();
        let received = read_binary(&mut raw);
        feed_sink.extend_from_slice(&received);
        let elapsed = started.elapsed();
        assert_eq!(received, OUTPUT);
        if sample >= WARMUP_SAMPLES {
            receive_feed_latencies.push(elapsed);
        }
    }
    std::hint::black_box(&feed_sink);

    let (input_mean, input_p95) = latency_stats(&input_latencies);
    let (feed_mean, feed_p95) = latency_stats(&receive_feed_latencies);
    let budget = Duration::from_millis(1);
    assert!(
        input_mean < budget && input_p95 < budget,
        "input receipt to backend write exceeded budget: mean={input_mean:?}, p95={input_p95:?}"
    );
    assert!(
        feed_mean < budget && feed_p95 < budget,
        "raw receive/feed equivalent exceeded budget: mean={feed_mean:?}, p95={feed_p95:?}"
    );
}

#[test]
fn normal_receiver_delivers_fifty_mib_in_order_without_disconnect() {
    let _qa_guard = QA_TEST_LOCK.lock().unwrap();
    let (backend, handle) = start(ScopeSet::from_strings(["attach"]));
    let mut control = connect_control(handle.port(), &["attach"]);
    let (url, token) = reserve(&mut control, 2);
    let mut raw = connect_attach(&url, token);
    set_read_timeout(&mut raw, Duration::from_secs(5));

    const CHUNK_BYTES: usize = 64 * 1024;
    const CHUNK_COUNT: usize = 800;
    // 64 KiB every 6.25 ms = 10 MiB/s. Sending 800 chunks therefore holds
    // that rate for five seconds rather than proving only an equivalent
    // burst volume.
    const CHUNK_INTERVAL: Duration = Duration::from_micros(6_250);
    const TRANSFER_DEADLINE: Duration = Duration::from_secs(30);
    assert_eq!(CHUNK_BYTES * CHUNK_COUNT, 50 * 1024 * 1024);

    let output = backend.output();
    let started = Instant::now();
    let sender = std::thread::spawn(move || {
        let pacing_origin = Instant::now();
        for index in 0..CHUNK_COUNT {
            output
                .send(load_chunk(index, CHUNK_BYTES))
                .expect("normal receiver must not trigger disconnect-on-overflow");
            let next = pacing_origin + CHUNK_INTERVAL * (index as u32 + 1);
            let remaining = next.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
    });

    for index in 0..CHUNK_COUNT {
        let received = read_binary(&mut raw);
        let expected = load_chunk(index, CHUNK_BYTES);
        assert!(received == expected, "load chunk {index} was out of order");
    }
    sender.join().unwrap();

    // The 30-second bound is deliberately a generous CI liveness watchdog
    // rather than a noisy micro-benchmark threshold; fixed-rate duration and
    // byte equality/order are the main gates.
    assert!(
        started.elapsed() < TRANSFER_DEADLINE,
        "50 MiB loopback transfer exceeded the CI liveness deadline"
    );

    raw.send(Message::Binary(b"connection-still-live".to_vec()))
        .unwrap();
    backend
        .input_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("raw attach disconnected after sustained transfer");
}
