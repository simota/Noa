//! Wire-level authorization and error propagation for repository widgets.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noa_ipc::widget::{InstanceParams, ListParams, OpenParams, WidgetCall, WidgetError};
use noa_ipc::{
    Broadcaster, Client, ClientError, GridResult, IpcBackend, IpcError, Panel, ScopeSet, Server,
    ServerConfig, SplitDirection, TextResult, TextSource, WidgetErrorKind,
};
use serde_json::{Value, json};

struct Backend {
    enabled: bool,
    calls: AtomicUsize,
}

impl IpcBackend for Backend {
    fn list_panels(&self) -> Arc<Vec<Panel>> {
        Arc::new(Vec::new())
    }
    fn get_text(&self, _: u64, _: TextSource, _: usize) -> Result<TextResult, IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn get_grid(&self, _: u64, _: u64, _: u64) -> Result<GridResult, IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn send_text(&self, _: u64, _: &str, _: bool, _: bool, _: bool) -> Result<(), IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn focus_pane(&self, _: u64) -> Result<(), IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn new_tab(&self, _: Option<u64>) -> Result<u64, IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn split(&self, _: u64, _: SplitDirection) -> Result<u64, IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn close_pane(&self, _: u64) -> Result<(), IpcError> {
        Err(IpcError::UnknownPane)
    }
    fn widget(&self, call: WidgetCall) -> Result<Value, WidgetError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if !self.enabled {
            return Err(WidgetError::unsupported());
        }
        if matches!(call, WidgetCall::Get(_)) {
            return Err(WidgetError::new(
                WidgetErrorKind::NotFound,
                "instance is gone",
            ));
        }
        Ok(json!({"method":call.method(),"params":call.params()}))
    }
    fn capabilities(&self) -> Vec<&'static str> {
        if self.enabled {
            vec![noa_ipc::CAPABILITY_REPO_WIDGETS_V1]
        } else {
            Vec::new()
        }
    }
}

fn start(enabled: bool, scopes: &[&str]) -> (Arc<Backend>, noa_ipc::ServerHandle, Client) {
    let backend = Arc::new(Backend {
        enabled,
        calls: AtomicUsize::new(0),
    });
    let handle = Server::start(
        ServerConfig {
            port: 0,
            bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
            token: "widget-test-token".into(),
            allowed_scopes: ScopeSet::from_strings(scopes.iter().copied()),
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
            handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
        },
        backend.clone(),
        Broadcaster::new(),
    )
    .unwrap();
    let client = Client::connect(
        &format!("ws://127.0.0.1:{}/", handle.port()),
        "widget-test-token",
        ScopeSet::from_strings(["read", "control"]),
    )
    .unwrap();
    (backend, handle, client)
}

fn open() -> WidgetCall {
    WidgetCall::Open(OpenParams {
        repo: "/repo".into(),
        id: Some("status".into()),
        definition: None,
        data: None,
        request_id: "open-1".into(),
        owner_pane_id: None,
    })
}

#[test]
fn read_only_connections_cannot_open_widgets_and_control_needs_no_input_scope() {
    let (backend, _server, mut client) = start(true, &["read"]);
    assert!(client.supports_repo_widgets());
    client
        .widget(&WidgetCall::List(ListParams {
            repo: "/repo".into(),
        }))
        .unwrap();
    assert!(matches!(
        client.widget(&open()),
        Err(ClientError::Rpc { code: -32003, .. })
    ));
    assert_eq!(backend.calls.load(Ordering::Relaxed), 1);
    let (backend, _server, mut client) = start(true, &["read", "control"]);
    assert_eq!(client.widget(&open()).unwrap()["method"], "noa.widget.open");
    assert_eq!(backend.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn unavailable_services_do_not_advertise_widgets_and_typed_errors_reach_the_client() {
    let (_backend, _server, mut client) = start(false, &["read", "control"]);
    assert!(!client.supports_repo_widgets());
    assert!(matches!(
        client.widget(&open()),
        Err(ClientError::Rpc { code: -32018, .. })
    ));
    let (_backend, _server, mut client) = start(true, &["read", "control"]);
    assert!(matches!(
        client.widget(&WidgetCall::Get(InstanceParams {
            instance_id: "wi_0123456789abcdef0123456789abcdef".into()
        })),
        Err(ClientError::Rpc { code: -32020, .. })
    ));
}
