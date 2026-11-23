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
use noa_ipc::{
    Broadcaster, ChangedPanel, Client, ClientError, FieldMask, Notification, PanelField, ScopeSet,
    SendTextOptions, Server, ServerConfig, UpdateSubscriptionParams,
};

const TOKEN: &str = "orchestration-token";
/// A pane whose program has not enabled bracketed paste yet.
const STARTING_PANE: PaneRef = 4;
/// A pane on the alternate screen.
const ALT_SCREEN_PANE: PaneRef = 5;
/// A pane whose foreground process is a plain shell.
const SHELL_PANE: PaneRef = 6;

#[derive(Default)]
struct SpawnBackend {
    spawns: Mutex<Vec<(SpawnTarget, SpawnParams)>>,
    legacy_splits: Mutex<Vec<(PaneRef, SplitDirection)>>,
    sent: Mutex<Vec<(PaneRef, String, bool, bool)>>,
}

impl IpcBackend for SpawnBackend {
    fn list_panels(&self) -> Vec<Panel> {
        vec![panel(1, 0)]
    }

    fn get_text(&self, pane: PaneRef, _: TextSource, _: usize) -> Result<TextResult, IpcError> {
        match pane {
            ALT_SCREEN_PANE => Ok(TextResult {
                text: "tui".to_string(),
                truncated: false,
                alternate_screen: true,
            }),
            _ => Err(IpcError::UnknownPane),
        }
    }

    fn get_grid(&self, _: PaneRef, _: u64, _: u64) -> Result<GridResult, IpcError> {
        Err(IpcError::UnknownPane)
    }

    fn send_text(
        &self,
        pane: PaneRef,
        text: &str,
        paste: bool,
        require_paste_mode: bool,
        refuse_shell: bool,
    ) -> Result<(), IpcError> {
        if pane == STARTING_PANE && paste && require_paste_mode {
            return Err(IpcError::NotReady("not in paste mode yet".to_string()));
        }
        if pane == SHELL_PANE && refuse_shell {
            return Err(IpcError::NotReady("a shell is in front".to_string()));
        }
        self.sent
            .lock()
            .unwrap()
            .push((pane, text.to_string(), paste, require_paste_mode));
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
        // The app counts children; this stands in for its refusal.
        if params.max_children == Some(0) {
            return Err(IpcError::LimitReached("0 children allowed".to_string()));
        }
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
            blocked: None,
        }),
        exit: None,
        parent_pane_id: Some(WireId(9)),
        label: Some("fix-tests".to_string()),
        spawn: None,
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
        max_children: Some(3),
        auto_approve: None,
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
        max_children: None,
        auto_approve: None,
    };

    client
        .spawn_split(1, SplitPlacement::Float, &params)
        .unwrap();

    let spawns = backend.spawns.lock().unwrap();
    assert_eq!(spawns[0].0, SpawnTarget::Float { pane: 1 });
    assert_eq!(spawns[0].1, params);
    assert!(backend.legacy_splits.lock().unwrap().is_empty());
}

// agent-orchestration D10: a worker floats over its orchestrator's board, so
// the pane it goes over and its parent differ; both reach the backend.
#[test]
fn a_float_may_go_over_a_pane_other_than_its_parent() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let params = SpawnParams {
        config: Some(command_config()),
        label: None,
        parent_pane_id: Some(WireId(1)),
        focus: Some(false),
        max_children: None,
        auto_approve: None,
    };

    client
        .spawn_split(2, SplitPlacement::Float, &params)
        .unwrap();

    let spawns = backend.spawns.lock().unwrap();
    assert_eq!(spawns[0].0, SpawnTarget::Float { pane: 2 });
    assert_eq!(spawns[0].1.parent_pane_id, Some(WireId(1)));
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
fn auto_approve_needs_input_and_applies_to_new_tabs_only() {
    let on = SpawnParams {
        auto_approve: Some(true),
        ..SpawnParams::default()
    };
    let (backend, _broadcaster, handle) = start(&["read", "control"]);
    let mut client = connect(&handle, &["read", "control"]);
    assert!(matches!(
        client.spawn_tab(None, &on),
        Err(ClientError::Rpc { code: -32003, .. })
    ));
    assert!(backend.spawns.lock().unwrap().is_empty());

    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    client.spawn_tab(None, &on).unwrap();
    assert_eq!(backend.spawns.lock().unwrap()[0].1.auto_approve, Some(true));

    // A split shares its tab's setting with the panes already there.
    for placement in [SplitPlacement::Horizontal, SplitPlacement::Auto] {
        assert!(matches!(
            client.spawn_split(1, placement, &on),
            Err(ClientError::Rpc { code: -32602, .. })
        ));
    }
    assert_eq!(backend.spawns.lock().unwrap().len(), 1);
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
    client
        .send_text_with(
            1,
            "gated",
            SendTextOptions {
                paste: true,
                require_paste_mode: true,
                refuse_shell: false,
            },
        )
        .unwrap();

    assert_eq!(
        *backend.sent.lock().unwrap(),
        vec![
            (1, "hello".to_string(), true, false),
            (1, "\r".to_string(), false, false),
            (1, "gated".to_string(), true, true),
        ]
    );
}

// A send that refuses a shell reaches the backend with the flag: the server
// probes the foreground process there, and a refusal writes nothing.
#[test]
fn a_send_refusing_a_shell_is_refused_as_not_ready() {
    let (backend, _broadcaster, handle) = start(&["read", "input"]);
    let mut client = connect(&handle, &["read", "input"]);
    let options = SendTextOptions {
        paste: false,
        require_paste_mode: false,
        refuse_shell: true,
    };
    let refused = client.send_text_with(SHELL_PANE, "\r", options);
    assert!(matches!(
        refused,
        Err(ClientError::Rpc { code: -32010, .. })
    ));
    assert!(backend.sent.lock().unwrap().is_empty());
    client.send_text_with(1, "\r", options).unwrap();
    assert_eq!(backend.sent.lock().unwrap().len(), 1);
}

#[test]
fn plain_shells_are_told_from_other_programs() {
    for shell in ["zsh", "-zsh", "/bin/bash", "fish", "nu", "pwsh"] {
        assert!(noa_ipc::is_plain_shell(shell), "{shell}");
    }
    for other in ["claude", "codex", "node", "vim", "zshell"] {
        assert!(!noa_ipc::is_plain_shell(other), "{other}");
    }
}

// REL-01: a send that requires paste mode to a pane that has not enabled
// it comes back as -32010 and nothing is written.
#[test]
fn a_send_requiring_paste_mode_is_refused_as_not_ready() {
    let (backend, _broadcaster, handle) = start(&["read", "input"]);
    let mut client = connect(&handle, &["read", "input"]);

    let refused = client.send_text_with(
        STARTING_PANE,
        "line one\nline two",
        SendTextOptions {
            paste: true,
            require_paste_mode: true,
            refuse_shell: false,
        },
    );

    assert!(matches!(
        refused,
        Err(ClientError::Rpc { code: -32010, .. })
    ));
    assert!(backend.sent.lock().unwrap().is_empty());
}

// REL-07: getText says when the pane is on the alternate screen; absent
// means false.
#[test]
fn get_text_reports_the_alternate_screen() {
    let (_backend, _broadcaster, handle) = start(&["read"]);
    let mut client = connect(&handle, &["read"]);

    let result = client
        .get_text(ALT_SCREEN_PANE, TextSource::Scrollback, None)
        .unwrap();

    assert!(result.alternate_screen);
    assert_eq!(result.text, "tui");
}

// REL-02: a backend refusal over `maxChildren` reaches the client as -32011.
#[test]
fn a_spawn_over_max_children_is_refused_as_limit_reached() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let params = SpawnParams {
        config: Some(command_config()),
        parent_pane_id: Some(WireId(1)),
        max_children: Some(0),
        ..SpawnParams::default()
    };

    let refused = client.spawn_split(1, SplitPlacement::Auto, &params);

    assert!(matches!(
        refused,
        Err(ClientError::Rpc { code: -32011, .. })
    ));
    assert!(backend.spawns.lock().unwrap().is_empty());
}

// REL-14: a worker cannot take a `board:` label; the board (no parent) can.
#[test]
fn a_worker_with_a_board_label_is_refused() {
    let (backend, _broadcaster, handle) = start(&["read", "control", "input"]);
    let mut client = connect(&handle, &["read", "control", "input"]);
    let board = SpawnParams {
        config: Some(command_config()),
        label: Some("board:1".to_string()),
        focus: Some(false),
        ..SpawnParams::default()
    };
    let worker = SpawnParams {
        parent_pane_id: Some(WireId(1)),
        ..board.clone()
    };

    assert!(matches!(
        client.spawn_split(1, SplitPlacement::Float, &worker),
        Err(ClientError::Rpc { code: -32602, .. })
    ));
    client.spawn_tab(None, &board).unwrap();
    assert_eq!(backend.spawns.lock().unwrap().len(), 1);
}

// REL-10: past the connection limit the server answers with a 1013 close,
// which the client reports as its own error rather than a lost connection.
#[test]
fn a_connection_over_the_limit_is_closed_as_busy() {
    let (_backend, _broadcaster, handle) = start(&["read"]);
    let held: Vec<Client> = (0..32).map(|_| connect(&handle, &["read"])).collect();

    let refused = Client::connect(
        &format!("ws://127.0.0.1:{}/", handle.port()),
        TOKEN,
        ScopeSet::from_strings(["read"]),
    );

    assert!(
        matches!(refused, Err(ClientError::TooManyConnections(_))),
        "{:?}",
        refused.err()
    );
    drop(held);
}

// REL-04: a request frame over the server's bound closes the connection with
// 1009, which the client reports as its own error.
#[test]
fn an_oversized_send_is_closed_as_too_big() {
    let (backend, _broadcaster, handle) = start(&["read", "input"]);
    let mut client = connect(&handle, &["read", "input"]);

    let refused = client.send_text(1, &"x".repeat(300 * 1024), true);

    assert!(
        matches!(refused, Err(ClientError::MessageTooBig(_))),
        "{refused:?}"
    );
    assert!(backend.sent.lock().unwrap().is_empty());
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
        Notification::StateChanged {
            panels,
            removed_pane_ids,
            dropped,
        } => {
            assert!(removed_pane_ids.is_empty());
            assert!(!dropped);
            assert_eq!(panels, vec![panel(1, 7)]);
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

/// The next `stateChanged` as `(panels, removed_pane_ids)`; it must not
/// report a loss.
fn next_state_changed(client: &mut Client) -> (Vec<Panel>, Vec<u64>) {
    match client
        .next_notification(Duration::from_secs(5))
        .unwrap()
        .expect("a stateChanged notification")
    {
        Notification::StateChanged {
            panels,
            removed_pane_ids,
            dropped,
        } => {
            assert!(!dropped);
            (panels, removed_pane_ids)
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

fn changed(panel: Panel, fields: &[PanelField]) -> ChangedPanel {
    ChangedPanel {
        panel,
        fields: FieldMask::from_fields(fields),
    }
}

// Pane subscriptions AC-4, end to end through the typed client: a field
// filter, a removal notice and an in-place scope update. One connection's
// notifications arrive in the order they were broadcast, so a notification
// the filter should have held back would arrive before the expected one.
#[test]
fn the_client_reads_removals_and_filtered_updates_and_widens_its_scope() {
    let (_backend, broadcaster, handle) = start(&["read"]);
    let mut client = connect(&handle, &["read"]);
    let subscription = client
        .subscribe_with(
            &[EventKind::StateChanged],
            Some(&[1]),
            Some(&[PanelField::Agent]),
        )
        .unwrap();

    broadcaster.broadcast_state_delta(vec![changed(panel(1, 1), &[PanelField::Preview])], vec![]);
    broadcaster.broadcast_state_delta(vec![changed(panel(1, 2), &[PanelField::Agent])], vec![]);
    assert_eq!(next_state_changed(&mut client), (vec![panel(1, 2)], vec![]));

    broadcaster.broadcast_state_delta(vec![], vec![1]);
    assert_eq!(next_state_changed(&mut client), (vec![], vec![1]));

    // Pane 2 is outside the scope until the update widens it.
    broadcaster.broadcast_state_delta(vec![changed(panel(2, 3), &[PanelField::Agent])], vec![]);
    client
        .update_subscription(&UpdateSubscriptionParams {
            subscription_id: WireId(subscription),
            pane_ids: Some(Some(vec![WireId(1), WireId(2)])),
            ..UpdateSubscriptionParams::default()
        })
        .unwrap();
    broadcaster.broadcast_state_delta(vec![changed(panel(2, 4), &[PanelField::Agent])], vec![]);
    assert_eq!(next_state_changed(&mut client), (vec![panel(2, 4)], vec![]));

    // The id is kept: another update of it still finds it, and an id this
    // connection does not own is refused.
    client
        .update_subscription(&UpdateSubscriptionParams {
            subscription_id: WireId(subscription),
            fields: Some(None),
            ..UpdateSubscriptionParams::default()
        })
        .unwrap();
    let unknown = client.update_subscription(&UpdateSubscriptionParams {
        subscription_id: WireId(subscription + 100),
        ..UpdateSubscriptionParams::default()
    });
    assert!(
        matches!(unknown, Err(ClientError::Rpc { code: -32602, .. })),
        "{unknown:?}"
    );
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
