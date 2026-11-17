//! agent-orchestration D1–D3 over a real loopback server and the typed
//! client: spawn extensions on `noa.newTab`/`noa.split`, their scope rule,
//! and notification delivery to a waiting client.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use noa_ipc::backend::{GridResult, IpcBackend, PaneRef, SpawnTarget, TextResult, WindowRef};
use noa_ipc::error::IpcError;
use noa_ipc::protocol::{
    EventKind, Panel, PanelAgent, SpawnConfig, SpawnParams, SplitDirection, SplitPlacement,
    TextSource, WireId,
};
use noa_ipc::{Broadcaster, Client, ClientError, Notification, ScopeSet, Server, ServerConfig};

const TOKEN: &str = "orchestration-token";

#[derive(Default)]
struct SpawnBackend {
    spawns: Mutex<Vec<(SpawnTarget, SpawnParams)>>,
    legacy_splits: Mutex<Vec<(PaneRef, SplitDirection)>>,
    sent: Mutex<Vec<(PaneRef, String, bool)>>,
}

impl IpcBackend for SpawnBackend {
    fn list_panels(&self) -> Vec<Panel> {
        vec![panel(1, 0)]
    }

    fn get_text(&self, _: PaneRef, _: TextSource, _: usize) -> Result<TextResult, IpcError> {
        Err(IpcError::UnknownPane)
    }

    fn get_grid(&self, _: PaneRef, _: u64, _: u64) -> Result<GridResult, IpcError> {
        Err(IpcError::UnknownPane)
    }

    fn send_text(&self, pane: PaneRef, text: &str, paste: bool) -> Result<(), IpcError> {
        self.sent
            .lock()
            .unwrap()
            .push((pane, text.to_string(), paste));
        Ok(())
    }

    fn focus_pane(&self, _: PaneRef) -> Result<(), IpcError> {
        Ok(())
    }

    fn new_tab(&self, _: Option<WindowRef>) -> Result<PaneRef, IpcError> {
        Ok(500)
    }

    fn split(&self, pane: PaneRef, direction: SplitDirection) -> Result<PaneRef, IpcError> {
        self.legacy_splits.lock().unwrap().push((pane, direction));
        Ok(600)
    }

    fn close_pane(&self, _: PaneRef) -> Result<(), IpcError> {
        Ok(())
    }

    fn spawn(&self, target: SpawnTarget, params: SpawnParams) -> Result<PaneRef, IpcError> {
        self.spawns.lock().unwrap().push((target, params));
        Ok(700)
    }
}

fn panel(pane: u64, seq: u64) -> Panel {
    Panel {
        window_group_id: WireId(1),
        window_id: WireId(1),
        pane_id: WireId(pane),
        name: "claude".to_string(),
        cwd: "/tmp".to_string(),
        branch: None,
        process: Some("claude".to_string()),
        busy: false,
        attention: false,
        attachable: true,
        preview: Vec::new(),
        surface_id: Some("0x0000000000000001".to_string()),
        agent: Some(PanelAgent {
            kind: Some("claude".to_string()),
            state: Some("finished".to_string()),
            detail: None,
            seq,
        }),
        exit: None,
        parent_pane_id: Some(WireId(9)),
        label: Some("fix-tests".to_string()),
    }
}

fn start(scopes: &[&str]) -> (Arc<SpawnBackend>, Broadcaster, noa_ipc::ServerHandle) {
    let backend = Arc::new(SpawnBackend::default());
    let broadcaster = Broadcaster::new();
    let handle = Server::start(
        ServerConfig {
            port: 0,
            bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
            token: TOKEN.to_string(),
            allowed_scopes: ScopeSet::from_strings(scopes.iter().copied()),
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
            handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
        },
        backend.clone(),
        broadcaster.clone(),
    )
    .expect("server should bind an ephemeral loopback port");
    (backend, broadcaster, handle)
}

fn connect(handle: &noa_ipc::ServerHandle, scopes: &[&str]) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}/", handle.port()),
        TOKEN,
        ScopeSet::from_strings(scopes.iter().copied()),
    )
    .unwrap()
}

fn command_config() -> SpawnConfig {
    SpawnConfig {
        command: Some(vec!["codex".to_string(), "--full-auto".to_string()]),
        cwd: Some("/repo".to_string()),
        wait_after_command: true,
        ..SpawnConfig::default()
    }
}

#[test]
fn plain_split_keeps_the_legacy_backend_path() {
    let (backend, _broadcaster, handle) = start(&["read", "control"]);
    let mut client = connect(&handle, &["read", "control"]);

    let pane = client
        .spawn_split(1, SplitPlacement::Vertical, &SpawnParams::default())
        .unwrap();

    assert_eq!(pane, 600);
    assert_eq!(
        *backend.legacy_splits.lock().unwrap(),
        vec![(1, SplitDirection::Vertical)]
    );
    assert!(backend.spawns.lock().unwrap().is_empty());
}

#[test]
fn auto_split_with_a_command_reaches_spawn_with_every_field() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let params = SpawnParams {
        config: Some(command_config()),
        label: Some("review".to_string()),
        parent_pane_id: Some(WireId(1)),
        focus: Some(false),
    };

    let pane = client
        .spawn_split(1, SplitPlacement::Auto, &params)
        .unwrap();

    assert_eq!(pane, 700);
    let spawns = backend.spawns.lock().unwrap();
    assert_eq!(
        spawns[0].0,
        SpawnTarget::Split {
            pane: 1,
            direction: None
        }
    );
    assert_eq!(spawns[0].1, params);
}

// Worker floats (floating-panes REQ-023 as amended): `direction: "float"`
// reaches the backend as a float on the pane, extensions or not.
#[test]
fn a_float_placement_reaches_spawn_as_a_float_on_the_pane() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let params = SpawnParams {
        config: Some(command_config()),
        label: Some("review".to_string()),
        parent_pane_id: Some(WireId(1)),
        focus: Some(false),
    };

    client
        .spawn_split(1, SplitPlacement::Float, &params)
        .unwrap();

    let spawns = backend.spawns.lock().unwrap();
    assert_eq!(spawns[0].0, SpawnTarget::Float { pane: 1 });
    assert_eq!(spawns[0].1, params);
    assert!(backend.legacy_splits.lock().unwrap().is_empty());
}

// Review finding: a float spawned without `parentPaneId` (bare, or with only
// a label) was exposed over IPC, though only worker floats may be.
#[test]
fn a_float_without_a_parent_pane_is_refused() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let labelled = SpawnParams {
        label: Some("scratch".to_string()),
        ..SpawnParams::default()
    };

    assert!(
        client
            .spawn_split(1, SplitPlacement::Float, &SpawnParams::default())
            .is_err()
    );
    assert!(
        client
            .spawn_split(1, SplitPlacement::Float, &labelled)
            .is_err()
    );
    assert!(backend.spawns.lock().unwrap().is_empty());
}

#[test]
fn auto_split_without_extensions_still_goes_through_spawn() {
    let (backend, _broadcaster, handle) = start(&["read", "control"]);
    let mut client = connect(&handle, &["read", "control"]);

    client
        .spawn_split(1, SplitPlacement::Auto, &SpawnParams::default())
        .unwrap();

    assert!(backend.legacy_splits.lock().unwrap().is_empty());
    assert_eq!(backend.spawns.lock().unwrap().len(), 1);
}

#[test]
fn a_command_needs_the_input_scope_on_top_of_control() {
    let (backend, _broadcaster, handle) = start(&["read", "control"]);
    let mut client = connect(&handle, &["read", "control"]);
    let params = SpawnParams {
        config: Some(command_config()),
        ..SpawnParams::default()
    };

    let result = client.spawn_tab(None, &params);

    assert!(matches!(result, Err(ClientError::Rpc { code: -32003, .. })));
    assert!(backend.spawns.lock().unwrap().is_empty());

    // A label alone runs nothing, so control suffices.
    let labelled = SpawnParams {
        label: Some("scratch".to_string()),
        ..SpawnParams::default()
    };
    assert_eq!(client.spawn_tab(Some(3), &labelled).unwrap(), 700);
    assert_eq!(
        backend.spawns.lock().unwrap()[0].0,
        SpawnTarget::Tab { window: Some(3) }
    );
}

#[test]
fn malformed_spawn_params_are_invalid_params() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let empty_argv = SpawnParams {
        config: Some(SpawnConfig {
            command: Some(Vec::new()),
            ..SpawnConfig::default()
        }),
        ..SpawnParams::default()
    };
    let long_label = SpawnParams {
        label: Some("x".repeat(noa_ipc::protocol::MAX_SPAWN_LABEL_BYTES + 1)),
        ..SpawnParams::default()
    };
    let bad_env = SpawnParams {
        config: Some(SpawnConfig {
            env: [("A=B".to_string(), "c".to_string())].into(),
            ..SpawnConfig::default()
        }),
        ..SpawnParams::default()
    };

    for params in [empty_argv, long_label, bad_env] {
        assert!(matches!(
            client.spawn_tab(None, &params),
            Err(ClientError::Rpc { code: -32602, .. })
        ));
    }
    assert!(backend.spawns.lock().unwrap().is_empty());
}

#[test]
fn send_text_passes_the_paste_flag_through() {
    let (backend, _broadcaster, handle) = start(&["read", "input"]);
    let mut client = connect(&handle, &["read", "input"]);

    client.send_text(1, "hello", true).unwrap();
    client.send_text(1, "\r", false).unwrap();

    assert_eq!(
        *backend.sent.lock().unwrap(),
        vec![(1, "hello".to_string(), true), (1, "\r".to_string(), false)]
    );
}

#[test]
fn state_changed_reaches_a_subscribed_client_with_the_new_fields() {
    let (_backend, broadcaster, handle) = start(&["read"]);
    let mut client = connect(&handle, &["read"]);
    client
        .subscribe(&[EventKind::StateChanged], Some(&[1]))
        .unwrap();

    assert!(
        client
            .next_notification(Duration::from_millis(50))
            .unwrap()
            .is_none()
    );
    broadcaster.broadcast_state_changed(vec![panel(1, 7)]);

    let notification = client
        .next_notification(Duration::from_secs(5))
        .unwrap()
        .expect("a stateChanged notification");
    match notification {
        Notification::StateChanged { panels, dropped } => {
            assert!(!dropped);
            assert_eq!(panels, vec![panel(1, 7)]);
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[test]
fn a_notification_arriving_during_a_request_is_kept_for_the_next_read() {
    let (_backend, broadcaster, handle) = start(&["read"]);
    let mut client = connect(&handle, &["read"]);
    client.subscribe(&[EventKind::StateChanged], None).unwrap();
    broadcaster.broadcast_state_changed(vec![panel(1, 3)]);
    // Give the connection thread time to push it before the request below.
    std::thread::sleep(Duration::from_millis(200));

    assert_eq!(client.list_panels().unwrap().len(), 1);

    assert!(matches!(
        client.next_notification(Duration::from_millis(10)).unwrap(),
        Some(Notification::StateChanged { .. })
    ));
}

#[test]
fn a_v3_panel_without_the_new_fields_still_deserializes() {
    let panel: Panel = serde_json::from_value(serde_json::json!({
        "windowGroupId": "1", "windowId": "2", "paneId": "3",
        "name": "shell", "cwd": "/tmp", "busy": false, "attention": false,
        "attachable": true, "preview": []
    }))
    .unwrap();

    assert!(panel.agent.is_none() && panel.exit.is_none() && panel.surface_id.is_none());
    let wire = serde_json::to_value(&panel).unwrap();
    for key in ["agent", "exit", "surfaceId", "parentPaneId", "label"] {
        assert!(wire.get(key).is_none(), "{key} must be omitted when absent");
    }
}
