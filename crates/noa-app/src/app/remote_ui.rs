//! `Attach Remote` command orchestration.
//!
//! UI state stays on the main thread. Network calls run on short-lived sync
//! workers; workers publish results into a pending table and wake winit with
//! an Eq request id. Bearer tokens are captured only by those worker closures
//! (or moved directly into the long-lived connection manager), never by
//! [`crate::UserEvent`] or a render snapshot.

use super::*;

const MISSING_TOKEN_MESSAGE: &str = "Set client-token or client-token-file in config.";

pub(super) type RemotePendingTable = Arc<Mutex<HashMap<u64, RemoteWorkerResult>>>;

pub(super) struct RemoteUiSession {
    pub(super) window_id: WindowId,
    phase: RemoteUiPhase,
    opened_at: Instant,
}

enum RemoteUiPhase {
    EndpointInput {
        buffer: String,
        validation_error: Option<String>,
    },
    Loading {
        request_id: u64,
        message: String,
    },
    Picker {
        endpoint: crate::remote_attach::RemoteEndpoint,
        picker: RemotePicker,
    },
    Retry {
        pane_id: PaneId,
        disabled_reason: Option<String>,
    },
    Error {
        message: String,
    },
}

impl RemoteUiSession {
    fn endpoint_input(window_id: WindowId, buffer: String, error: Option<String>) -> Self {
        Self {
            window_id,
            phase: RemoteUiPhase::EndpointInput {
                buffer,
                validation_error: error,
            },
            opened_at: Instant::now(),
        }
    }

    fn loading(window_id: WindowId, request_id: u64, message: String) -> Self {
        Self {
            window_id,
            phase: RemoteUiPhase::Loading {
                request_id,
                message,
            },
            opened_at: Instant::now(),
        }
    }

    fn error(window_id: WindowId, message: impl Into<String>) -> Self {
        Self {
            window_id,
            phase: RemoteUiPhase::Error {
                message: message.into(),
            },
            opened_at: Instant::now(),
        }
    }

    fn request_id(&self) -> Option<u64> {
        match self.phase {
            RemoteUiPhase::Loading { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    pub(super) fn is_loading(&self) -> bool {
        matches!(self.phase, RemoteUiPhase::Loading { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RemotePickerChoice {
    ExistingPane {
        pane_id: u64,
        cached_title: Option<String>,
    },
    CreateNewTab,
    CreateSplit {
        pane_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemotePickerItem {
    label: String,
    hint: Option<String>,
    disabled_reason: Option<String>,
    choice: RemotePickerChoice,
}

impl RemotePickerItem {
    fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }

    fn display_label(&self) -> String {
        match self.disabled_reason.as_deref() {
            Some(reason) => format!("{} — Disabled: {reason}", self.label),
            None => self.label.clone(),
        }
    }
}

struct RemotePicker {
    items: Vec<RemotePickerItem>,
    selected: usize,
    panel_count: usize,
}

impl RemotePicker {
    fn new(mut panels: Vec<noa_ipc::Panel>, granted: noa_ipc::ScopeSet) -> Self {
        panels.sort_by_key(|panel| (panel.window_group_id.0, panel.window_id.0, panel.pane_id.0));
        let panel_count = panels.len();
        let attach_granted = granted.contains(noa_ipc::Scope::Attach);
        let control_granted = granted.contains(noa_ipc::Scope::Control);
        let attach_disabled =
            (!attach_granted).then(|| "Attach scope was not granted.".to_string());

        let mut items = panels
            .iter()
            .map(|panel| RemotePickerItem {
                label: panel_label(panel),
                hint: Some(format!("Pane {}", panel.pane_id.0)),
                disabled_reason: attach_disabled.clone().or_else(|| {
                    (!panel.attachable)
                        .then(|| "This pane does not expose a raw attach endpoint.".to_string())
                }),
                choice: RemotePickerChoice::ExistingPane {
                    pane_id: panel.pane_id.0,
                    cached_title: nonempty(panel.name.as_str()).map(str::to_string),
                },
            })
            .collect::<Vec<_>>();

        items.push(RemotePickerItem {
            label: "Create New Tab".to_string(),
            hint: None,
            disabled_reason: creation_disabled_reason(attach_granted, control_granted, true),
            choice: RemotePickerChoice::CreateNewTab,
        });
        items.push(RemotePickerItem {
            label: "Create Split".to_string(),
            hint: panels
                .first()
                .map(|panel| format!("From Pane {}", panel.pane_id.0)),
            disabled_reason: creation_disabled_reason(
                attach_granted,
                control_granted,
                panel_count > 0,
            ),
            choice: RemotePickerChoice::CreateSplit {
                pane_id: panels.first().map(|panel| panel.pane_id.0).unwrap_or(0),
            },
        });

        let selected = items
            .iter()
            .position(RemotePickerItem::enabled)
            .unwrap_or(0);
        Self {
            items,
            selected,
            panel_count,
        }
    }

    fn move_up(&mut self) {
        if let Some(index) = self.items[..self.selected]
            .iter()
            .rposition(RemotePickerItem::enabled)
        {
            self.selected = index;
        }
    }

    fn move_down(&mut self) {
        let from = self.selected.saturating_add(1);
        if let Some(offset) = self
            .items
            .get(from..)
            .and_then(|items| items.iter().position(RemotePickerItem::enabled))
        {
            self.selected = from + offset;
        }
    }

    fn selected_choice(&self) -> Option<RemotePickerChoice> {
        self.items
            .get(self.selected)
            .filter(|item| item.enabled())
            .map(|item| item.choice.clone())
    }
}

fn creation_disabled_reason(
    attach_granted: bool,
    control_granted: bool,
    has_target: bool,
) -> Option<String> {
    if !attach_granted {
        Some("Attach scope was not granted.".to_string())
    } else if !control_granted {
        Some("Control scope was not granted.".to_string())
    } else if !has_target {
        Some("No remote pane is available to split.".to_string())
    } else {
        None
    }
}

fn panel_label(panel: &noa_ipc::Panel) -> String {
    let name = nonempty(panel.name.as_str()).unwrap_or("Untitled");
    let cwd = nonempty(panel.cwd.as_str()).unwrap_or("Unknown directory");
    format!("{name} — {cwd}")
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

enum RemoteStartGate {
    MissingToken,
    EndpointInput { seed: String, error: Option<String> },
    Warn(crate::remote_attach::RemoteEndpoint),
    Start(crate::remote_attach::RemoteEndpoint),
}

fn remote_start_gate(endpoint: Option<&str>, token: Option<&str>) -> RemoteStartGate {
    if token.and_then(nonempty).is_none() {
        return RemoteStartGate::MissingToken;
    }
    let Some(endpoint) = endpoint.and_then(nonempty) else {
        return RemoteStartGate::EndpointInput {
            seed: String::new(),
            error: None,
        };
    };
    match crate::remote_attach::RemoteEndpoint::parse(endpoint) {
        Ok(endpoint) if endpoint.requires_unencrypted_warning() => RemoteStartGate::Warn(endpoint),
        Ok(endpoint) => RemoteStartGate::Start(endpoint),
        Err(error) => RemoteStartGate::EndpointInput {
            seed: endpoint.to_string(),
            error: Some(error.to_string()),
        },
    }
}

fn warning_worker_endpoint(
    confirmed: bool,
    endpoint: crate::remote_attach::RemoteEndpoint,
) -> Option<crate::remote_attach::RemoteEndpoint> {
    confirmed.then_some(endpoint)
}

fn retry_disabled_reason(state: &crate::remote_attach::RemoteAttachState) -> Option<&'static str> {
    match state {
        crate::remote_attach::RemoteAttachState::Detached => None,
        crate::remote_attach::RemoteAttachState::Connected => {
            Some("Remote pane is already connected.")
        }
        crate::remote_attach::RemoteAttachState::Reconnecting { .. } => {
            Some("Remote reconnect is already in progress.")
        }
    }
}

enum RemoteRetryGate {
    Disabled(&'static str),
    MissingToken,
    InvalidEndpoint(String),
    Warn(crate::remote_attach::RemoteEndpoint),
    Start(crate::remote_attach::RemoteEndpoint),
}

fn remote_retry_gate(
    state: &crate::remote_attach::RemoteAttachState,
    endpoint: &str,
    token: Option<&str>,
) -> RemoteRetryGate {
    if let Some(reason) = retry_disabled_reason(state) {
        return RemoteRetryGate::Disabled(reason);
    }
    if token.and_then(nonempty).is_none() {
        return RemoteRetryGate::MissingToken;
    }
    match crate::remote_attach::RemoteEndpoint::parse(endpoint) {
        Ok(endpoint) if endpoint.requires_unencrypted_warning() => RemoteRetryGate::Warn(endpoint),
        Ok(endpoint) => RemoteRetryGate::Start(endpoint),
        Err(error) => RemoteRetryGate::InvalidEndpoint(error.to_string()),
    }
}

struct RemoteDiscovery {
    panels: Vec<noa_ipc::Panel>,
    granted: noa_ipc::ScopeSet,
}

enum RemoteWorkerPayload {
    Discovery(RemoteDiscovery),
    PaneReady {
        pane_id: u64,
        cached_title: Option<String>,
        cleanup_client: Box<noa_ipc::Client>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteSurfaceLaunchOutcome {
    LocalSurfaceReady,
    FailedBeforeSurface,
}

impl RemoteSurfaceLaunchOutcome {
    fn requires_created_pane_cleanup(self) -> bool {
        matches!(self, Self::FailedBeforeSurface)
    }
}

pub(super) struct RemoteWorkerResult {
    window_id: WindowId,
    endpoint: crate::remote_attach::RemoteEndpoint,
    outcome: Result<RemoteWorkerPayload, String>,
}

fn publish_pending_result<T>(
    pending: &Mutex<HashMap<u64, T>>,
    request_id: u64,
    result: T,
    notify: impl FnOnce() -> bool,
) -> Option<T> {
    pending.lock().insert(request_id, result);
    if notify() {
        None
    } else {
        pending.lock().remove(&request_id)
    }
}

fn cleanup_remote_worker_result(result: RemoteWorkerResult) {
    if let Ok(RemoteWorkerPayload::PaneReady {
        pane_id,
        mut cleanup_client,
        ..
    }) = result.outcome
    {
        let _ = cleanup_client.close_pane(pane_id);
    }
}

fn join_workers_and_drain_pending<T>(
    workers: &mut Vec<std::thread::JoinHandle<()>>,
    pending: &Mutex<HashMap<u64, T>>,
    mut cleanup: impl FnMut(T),
) {
    for worker in workers.drain(..) {
        if let Err(error) = worker.join() {
            log::warn!("remote worker panicked during shutdown: {error:?}");
        }
    }
    for result in std::mem::take(&mut *pending.lock()).into_values() {
        cleanup(result);
    }
}

fn requested_discovery_scopes() -> noa_ipc::ScopeSet {
    noa_ipc::ScopeSet::from_strings(["read", "control", "attach"])
}

impl App {
    fn track_remote_worker(&mut self, worker: std::thread::JoinHandle<()>) {
        let mut running = Vec::with_capacity(self.remote_workers.len() + 1);
        for existing in self.remote_workers.drain(..) {
            if existing.is_finished() {
                if let Err(error) = existing.join() {
                    log::warn!("remote worker panicked: {error:?}");
                }
            } else {
                running.push(existing);
            }
        }
        running.push(worker);
        self.remote_workers = running;
    }

    fn schedule_remote_pane_cleanup(&mut self, mut client: Box<noa_ipc::Client>, pane_id: u64) {
        match std::thread::Builder::new()
            .name("noa-remote-cleanup".to_string())
            .spawn(move || {
                let _ = client.close_pane(pane_id);
            }) {
            Ok(worker) => self.track_remote_worker(worker),
            Err(error) => log::warn!("failed to start remote pane cleanup worker: {error}"),
        }
    }

    fn schedule_remote_worker_result_cleanup(&mut self, result: RemoteWorkerResult) {
        if let Ok(RemoteWorkerPayload::PaneReady {
            pane_id,
            cleanup_client,
            ..
        }) = result.outcome
        {
            self.schedule_remote_pane_cleanup(cleanup_client, pane_id);
        }
    }

    pub(super) fn shutdown_remote_requests(&mut self) {
        join_workers_and_drain_pending(
            &mut self.remote_workers,
            self.remote_pending.as_ref(),
            cleanup_remote_worker_result,
        );
    }

    pub(in crate::app) fn begin_attach_remote(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        if self.confirm_dialog.is_some() || self.active_overlay(window_id) != ActiveOverlay::None {
            return;
        }
        if let Some((pane_id, state)) = self.windows.get(&window_id).and_then(|window| {
            let pane_id = window.focused_pane;
            let surface = window.focused_surface()?;
            let SurfaceTransport::Remote(remote) = &surface.transport else {
                return None;
            };
            Some((pane_id, remote.state.lock().clone()))
        }) {
            self.remote_ui = Some(RemoteUiSession {
                window_id,
                phase: RemoteUiPhase::Retry {
                    pane_id,
                    disabled_reason: retry_disabled_reason(&state).map(str::to_string),
                },
                opened_at: Instant::now(),
            });
            self.request_window_redraw(window_id);
            return;
        }
        match remote_start_gate(
            self.config.client_remote.as_deref(),
            self.config.client_token.as_deref(),
        ) {
            RemoteStartGate::MissingToken => {
                self.remote_ui = Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
                self.request_window_redraw(window_id);
            }
            RemoteStartGate::EndpointInput { seed, error } => {
                self.remote_ui = Some(RemoteUiSession::endpoint_input(window_id, seed, error));
                self.request_window_redraw(window_id);
            }
            RemoteStartGate::Warn(endpoint) => self.open_remote_warning(window_id, endpoint),
            RemoteStartGate::Start(endpoint) => self.start_remote_discovery(window_id, endpoint),
        }
    }

    fn open_remote_warning(
        &mut self,
        window_id: WindowId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        self.remote_ui = None;
        self.open_confirm_dialog(
            window_id,
            remote_warning_message(&endpoint),
            ConfirmAction::AttachRemote {
                window_id,
                endpoint,
            },
        );
    }

    fn open_detached_remote_retry_warning(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        self.remote_ui = None;
        self.open_confirm_dialog(
            window_id,
            remote_warning_message(&endpoint),
            ConfirmAction::RetryDetachedRemote {
                window_id,
                pane_id,
                endpoint,
            },
        );
    }

    pub(in crate::app) fn confirm_remote_warning(
        &mut self,
        window_id: WindowId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        if let Some(endpoint) = warning_worker_endpoint(true, endpoint) {
            self.start_remote_discovery(window_id, endpoint);
        }
    }

    pub(in crate::app) fn confirm_detached_remote_retry(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        if let Some(endpoint) = warning_worker_endpoint(true, endpoint) {
            self.start_detached_remote_connection(window_id, pane_id, endpoint);
        }
    }

    fn start_remote_discovery(
        &mut self,
        window_id: WindowId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        let Some(token) = self
            .config
            .client_token
            .clone()
            .filter(|value| nonempty(value).is_some())
        else {
            self.remote_ui = Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
            self.request_window_redraw(window_id);
            return;
        };
        let request_id = self.next_remote_request_id();
        self.remote_ui = Some(RemoteUiSession::loading(
            window_id,
            request_id,
            format!("Connecting to {}…", endpoint.authority()),
        ));
        self.request_window_redraw(window_id);

        let worker_endpoint = endpoint.clone();
        let pending = Arc::clone(&self.remote_pending);
        let proxy = self.proxy.clone();
        let spawn = std::thread::Builder::new()
            .name("noa-remote-discovery".to_string())
            .spawn(move || {
                let outcome = (|| {
                    let mut client = noa_ipc::Client::connect(
                        &worker_endpoint.control_url(),
                        &token,
                        requested_discovery_scopes(),
                    )
                    .map_err(|error| format!("Remote connection failed: {error}"))?;
                    let granted = client.granted_scopes();
                    // `control + attach` is sufficient for Create New Tab.
                    // A server may legitimately grant that subset without
                    // `read`; only existing-pane discovery depends on read.
                    let panels = if granted.contains(noa_ipc::Scope::Read) {
                        client
                            .list_panels()
                            .map_err(|error| format!("Remote pane discovery failed: {error}"))?
                    } else {
                        Vec::new()
                    };
                    Ok(RemoteWorkerPayload::Discovery(RemoteDiscovery {
                        panels,
                        granted,
                    }))
                })();
                let unpublished = publish_pending_result(
                    pending.as_ref(),
                    request_id,
                    RemoteWorkerResult {
                        window_id,
                        endpoint: worker_endpoint,
                        outcome,
                    },
                    || {
                        proxy
                            .send_event(UserEvent::RemoteRequestCompleted { request_id })
                            .is_ok()
                    },
                );
                if let Some(result) = unpublished {
                    cleanup_remote_worker_result(result);
                }
            });
        match spawn {
            Ok(worker) => self.track_remote_worker(worker),
            Err(error) => {
                self.remote_ui = Some(RemoteUiSession::error(
                    window_id,
                    format!("Unable to start remote discovery: {error}"),
                ));
                self.request_window_redraw(window_id);
            }
        }
    }

    fn start_remote_create(
        &mut self,
        window_id: WindowId,
        endpoint: crate::remote_attach::RemoteEndpoint,
        choice: RemotePickerChoice,
    ) {
        let Some(token) = self
            .config
            .client_token
            .clone()
            .filter(|value| nonempty(value).is_some())
        else {
            self.remote_ui = Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
            self.request_window_redraw(window_id);
            return;
        };
        let request_id = self.next_remote_request_id();
        let label = match choice {
            RemotePickerChoice::CreateNewTab => "Creating remote tab…",
            RemotePickerChoice::CreateSplit { .. } => "Creating remote split…",
            RemotePickerChoice::ExistingPane { .. } => return,
        };
        self.remote_ui = Some(RemoteUiSession::loading(
            window_id,
            request_id,
            label.to_string(),
        ));
        self.request_window_redraw(window_id);

        let worker_endpoint = endpoint.clone();
        let pending = Arc::clone(&self.remote_pending);
        let proxy = self.proxy.clone();
        let spawn = std::thread::Builder::new()
            .name("noa-remote-create".to_string())
            .spawn(move || {
                let outcome = (|| {
                    let mut client = noa_ipc::Client::connect(
                        &worker_endpoint.control_url(),
                        &token,
                        requested_discovery_scopes(),
                    )
                    .map_err(|error| format!("Remote connection failed: {error}"))?;
                    if !client.granted_scopes().contains(noa_ipc::Scope::Attach) {
                        return Err("Attach scope was not granted.".to_string());
                    }
                    if !client.granted_scopes().contains(noa_ipc::Scope::Control) {
                        return Err("Control scope was not granted.".to_string());
                    }
                    let pane_id = match choice {
                        RemotePickerChoice::CreateNewTab => client.new_tab(None),
                        RemotePickerChoice::CreateSplit { pane_id } => {
                            client.split(pane_id, noa_ipc::SplitDirection::Vertical)
                        }
                        RemotePickerChoice::ExistingPane { .. } => unreachable!(),
                    }
                    .map_err(|error| format!("Remote pane creation failed: {error}"))?;
                    Ok(RemoteWorkerPayload::PaneReady {
                        pane_id,
                        cached_title: None,
                        cleanup_client: Box::new(client),
                    })
                })();
                let unpublished = publish_pending_result(
                    pending.as_ref(),
                    request_id,
                    RemoteWorkerResult {
                        window_id,
                        endpoint: worker_endpoint,
                        outcome,
                    },
                    || {
                        proxy
                            .send_event(UserEvent::RemoteRequestCompleted { request_id })
                            .is_ok()
                    },
                );
                if let Some(result) = unpublished {
                    cleanup_remote_worker_result(result);
                }
            });
        match spawn {
            Ok(worker) => self.track_remote_worker(worker),
            Err(error) => {
                self.remote_ui = Some(RemoteUiSession::error(
                    window_id,
                    format!("Unable to start remote pane creation: {error}"),
                ));
                self.request_window_redraw(window_id);
            }
        }
    }

    fn next_remote_request_id(&self) -> u64 {
        self.remote_next_request
            .fetch_add(1, Ordering::Relaxed)
            .max(1)
    }

    pub(in crate::app) fn handle_remote_request_completed(
        &mut self,
        event_loop: &ActiveEventLoop,
        request_id: u64,
    ) {
        let Some(result) = self.remote_pending.lock().remove(&request_id) else {
            return;
        };
        let current = self.remote_ui.as_ref().is_some_and(|session| {
            session.window_id == result.window_id && session.request_id() == Some(request_id)
        });
        if !current || !self.windows.contains_key(&result.window_id) {
            self.schedule_remote_worker_result_cleanup(result);
            return;
        }

        match result.outcome {
            Ok(RemoteWorkerPayload::Discovery(discovery)) => {
                if !discovery.granted.contains(noa_ipc::Scope::Attach) {
                    self.remote_ui = Some(RemoteUiSession::error(
                        result.window_id,
                        "Attach scope was not granted.",
                    ));
                } else {
                    self.remote_ui = Some(RemoteUiSession {
                        window_id: result.window_id,
                        phase: RemoteUiPhase::Picker {
                            endpoint: result.endpoint,
                            picker: RemotePicker::new(discovery.panels, discovery.granted),
                        },
                        opened_at: Instant::now(),
                    });
                }
                self.request_window_redraw(result.window_id);
            }
            Ok(RemoteWorkerPayload::PaneReady {
                pane_id,
                cached_title,
                cleanup_client,
            }) => {
                let outcome = self.launch_remote_surface(
                    event_loop,
                    result.window_id,
                    result.endpoint,
                    pane_id,
                    cached_title,
                );
                if outcome.requires_created_pane_cleanup() {
                    self.schedule_remote_pane_cleanup(cleanup_client, pane_id);
                }
            }
            Err(message) => {
                self.remote_ui = Some(RemoteUiSession::error(result.window_id, message));
                self.request_window_redraw(result.window_id);
            }
        }
    }

    pub(in crate::app) fn handle_remote_ui_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            // Once a worker may have issued `noa.newTab`/`noa.split`, UI-only
            // cancellation can orphan the newly created remote process. Keep
            // every loading operation owned until its result is reconciled.
            if self.remote_ui.as_ref().is_some_and(|session| {
                session.window_id == window_id
                    && matches!(session.phase, RemoteUiPhase::Loading { .. })
            }) {
                return;
            }
            self.remote_ui = None;
            self.request_window_redraw(window_id);
            return;
        }

        let phase_kind = self.remote_ui.as_ref().and_then(|session| {
            (session.window_id == window_id).then_some(match session.phase {
                RemoteUiPhase::EndpointInput { .. } => 0,
                RemoteUiPhase::Picker { .. } => 1,
                RemoteUiPhase::Retry { .. } => 2,
                RemoteUiPhase::Loading { .. } | RemoteUiPhase::Error { .. } => 3,
            })
        });
        match phase_kind {
            Some(0) => self.handle_remote_endpoint_key(window_id, event),
            Some(1) => self.handle_remote_picker_key(event_loop, window_id, event),
            Some(2) => self.handle_remote_retry_key(window_id, event),
            Some(3) | None => {}
            Some(_) => unreachable!(),
        }
    }

    fn handle_remote_retry_key(&mut self, window_id: WindowId, event: &KeyEvent) {
        if !matches!(event.logical_key, Key::Named(NamedKey::Enter)) {
            return;
        }
        let pane_id = self
            .remote_ui
            .as_ref()
            .and_then(|session| match &session.phase {
                RemoteUiPhase::Retry {
                    pane_id,
                    disabled_reason: None,
                } => Some(*pane_id),
                _ => None,
            });
        let Some(pane_id) = pane_id else {
            return;
        };
        let retry_target = self
            .windows
            .get(&window_id)
            .and_then(|window| window.surfaces.get(&pane_id))
            .and_then(|surface| match &surface.transport {
                SurfaceTransport::Remote(remote) => Some((
                    remote.state.lock().clone(),
                    remote.identity.endpoint.clone(),
                )),
                SurfaceTransport::Local(_) => None,
            });
        let Some((state, endpoint)) = retry_target else {
            self.remote_ui = Some(RemoteUiSession::error(
                window_id,
                "Remote pane is no longer available.",
            ));
            self.request_window_redraw(window_id);
            return;
        };

        match remote_retry_gate(&state, &endpoint, self.config.client_token.as_deref()) {
            RemoteRetryGate::Disabled(reason) => {
                self.remote_ui = Some(RemoteUiSession {
                    window_id,
                    phase: RemoteUiPhase::Retry {
                        pane_id,
                        disabled_reason: Some(reason.to_string()),
                    },
                    opened_at: Instant::now(),
                });
                self.request_window_redraw(window_id);
            }
            RemoteRetryGate::MissingToken => {
                self.remote_ui = Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
                self.request_window_redraw(window_id);
            }
            RemoteRetryGate::InvalidEndpoint(error) => {
                self.remote_ui = Some(RemoteUiSession::error(
                    window_id,
                    format!("Saved remote endpoint is invalid: {error}"),
                ));
                self.request_window_redraw(window_id);
            }
            RemoteRetryGate::Warn(endpoint) => {
                self.open_detached_remote_retry_warning(window_id, pane_id, endpoint);
            }
            RemoteRetryGate::Start(endpoint) => {
                self.start_detached_remote_connection(window_id, pane_id, endpoint);
            }
        }
    }

    fn handle_remote_endpoint_key(&mut self, window_id: WindowId, event: &KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                let endpoint = self
                    .remote_ui
                    .as_ref()
                    .and_then(|session| match &session.phase {
                        RemoteUiPhase::EndpointInput { buffer, .. } => Some(buffer.clone()),
                        _ => None,
                    });
                let Some(endpoint) = endpoint else {
                    return;
                };
                match remote_start_gate(
                    Some(endpoint.as_str()),
                    self.config.client_token.as_deref(),
                ) {
                    RemoteStartGate::MissingToken => {
                        self.remote_ui =
                            Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
                    }
                    RemoteStartGate::EndpointInput { seed, error } => {
                        self.remote_ui =
                            Some(RemoteUiSession::endpoint_input(window_id, seed, error));
                    }
                    RemoteStartGate::Warn(endpoint) => {
                        self.open_remote_warning(window_id, endpoint);
                        return;
                    }
                    RemoteStartGate::Start(endpoint) => {
                        self.start_remote_discovery(window_id, endpoint);
                        return;
                    }
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(RemoteUiPhase::EndpointInput {
                    buffer,
                    validation_error,
                }) = self.remote_ui.as_mut().map(|session| &mut session.phase)
                {
                    buffer.pop();
                    *validation_error = None;
                }
                self.request_window_redraw(window_id);
            }
            _ if !self.modifiers.super_key() => {
                if let Some(text) = event.text.as_deref() {
                    self.push_remote_ui_text(text);
                    self.request_window_redraw(window_id);
                }
            }
            _ => {}
        }
    }

    fn handle_remote_picker_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(RemoteUiPhase::Picker { picker, .. }) =
                    self.remote_ui.as_mut().map(|session| &mut session.phase)
                {
                    picker.move_up();
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(RemoteUiPhase::Picker { picker, .. }) =
                    self.remote_ui.as_mut().map(|session| &mut session.phase)
                {
                    picker.move_down();
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::Enter) => {
                let selected = self
                    .remote_ui
                    .as_ref()
                    .and_then(|session| match &session.phase {
                        RemoteUiPhase::Picker { endpoint, picker } => picker
                            .selected_choice()
                            .map(|choice| (endpoint.clone(), choice)),
                        _ => None,
                    });
                let Some((endpoint, choice)) = selected else {
                    return;
                };
                match choice {
                    RemotePickerChoice::ExistingPane {
                        pane_id,
                        cached_title,
                    } => {
                        let _ = self.launch_remote_surface(
                            event_loop,
                            window_id,
                            endpoint,
                            pane_id,
                            cached_title,
                        );
                    }
                    choice @ (RemotePickerChoice::CreateNewTab
                    | RemotePickerChoice::CreateSplit { .. }) => {
                        self.start_remote_create(window_id, endpoint, choice)
                    }
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn push_remote_ui_text(&mut self, text: &str) {
        let filtered = text
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        if filtered.is_empty() {
            return;
        }
        if let Some(RemoteUiPhase::EndpointInput {
            buffer,
            validation_error,
        }) = self.remote_ui.as_mut().map(|session| &mut session.phase)
        {
            buffer.push_str(&filtered);
            *validation_error = None;
        }
    }

    pub(in crate::app) fn remote_ui_snapshot(
        &self,
        window_id: WindowId,
    ) -> Option<(CommandPaletteSnapshot, Instant)> {
        let session = self
            .remote_ui
            .as_ref()
            .filter(|session| session.window_id == window_id)?;
        let snapshot = match &session.phase {
            RemoteUiPhase::EndpointInput {
                buffer,
                validation_error,
            } => {
                let mut query = buffer.clone();
                query.push_str(self.modal_preedit_for(window_id, ModalImeTarget::RemoteUi));
                let mut rows = vec![PaletteRow::Header {
                    label: "Remote endpoint (host:port)".to_string(),
                }];
                if let Some(error) = validation_error {
                    rows.push(PaletteRow::Entry {
                        title: format!("Invalid endpoint — {error}"),
                        hint: Some("Edit and press Enter".to_string()),
                        match_positions: Vec::new(),
                        enabled: false,
                    });
                }
                CommandPaletteSnapshot {
                    query,
                    rows,
                    selected: 0,
                    total_entries: 0,
                }
            }
            RemoteUiPhase::Loading { message, .. } => status_snapshot(message, "Please wait"),
            RemoteUiPhase::Error { message } => status_snapshot(message, "Esc to close"),
            RemoteUiPhase::Retry {
                disabled_reason, ..
            } => {
                let enabled = disabled_reason.is_none();
                CommandPaletteSnapshot {
                    query: "Remote connection".to_string(),
                    rows: vec![PaletteRow::Entry {
                        title: match disabled_reason {
                            Some(reason) => format!("Retry Attach — Disabled: {reason}"),
                            None => "Retry Attach".to_string(),
                        },
                        hint: Some("Enter".to_string()),
                        match_positions: Vec::new(),
                        enabled,
                    }],
                    selected: 0,
                    total_entries: 1,
                }
            }
            RemoteUiPhase::Picker { picker, .. } => CommandPaletteSnapshot {
                query: if picker.panel_count == 0 {
                    "No remote panes found — create one below".to_string()
                } else {
                    "Select a remote pane".to_string()
                },
                rows: picker
                    .items
                    .iter()
                    .map(|item| PaletteRow::Entry {
                        title: item.display_label(),
                        hint: item.hint.clone(),
                        match_positions: Vec::new(),
                        enabled: item.enabled(),
                    })
                    .collect(),
                selected: picker.selected,
                total_entries: picker.items.len(),
            },
        };
        Some((snapshot, session.opened_at))
    }

    fn start_detached_remote_connection(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        endpoint: crate::remote_attach::RemoteEndpoint,
    ) {
        let token = match self
            .config
            .client_token
            .clone()
            .and_then(|value| crate::remote_attach::RemoteToken::new(value).ok())
        {
            Some(token) => token,
            None => {
                self.remote_ui = Some(RemoteUiSession::error(window_id, MISSING_TOKEN_MESSAGE));
                self.request_window_redraw(window_id);
                return;
            }
        };
        let remote_parts = self
            .windows
            .get(&window_id)
            .and_then(|window| window.surfaces.get(&pane_id))
            .and_then(|surface| match &surface.transport {
                SurfaceTransport::Remote(remote)
                    if remote.identity.endpoint == endpoint.authority()
                        && matches!(
                            *remote.state.lock(),
                            crate::remote_attach::RemoteAttachState::Detached
                        ) =>
                {
                    Some((
                        Arc::clone(&surface.terminal),
                        Arc::clone(&surface.overview_snapshot),
                        Arc::clone(&remote.state),
                        remote.identity.pane_id,
                        remote.connection.is_some(),
                    ))
                }
                SurfaceTransport::Remote(_) | SurfaceTransport::Local(_) => None,
            });
        let Some((terminal, overview_snapshot, remote_state, remote_pane_id, has_connection)) =
            remote_parts
        else {
            self.remote_ui = Some(RemoteUiSession::error(
                window_id,
                "Remote pane is no longer detached.",
            ));
            self.request_window_redraw(window_id);
            return;
        };

        let credentials =
            crate::remote_attach::RemoteConnectionCredentials::new(endpoint, token, remote_pane_id);
        if has_connection {
            let retried = self
                .windows
                .get(&window_id)
                .and_then(|window| window.surfaces.get(&pane_id))
                .and_then(|surface| match &surface.transport {
                    SurfaceTransport::Remote(remote) => remote.connection.as_ref(),
                    SurfaceTransport::Local(_) => None,
                })
                .is_some_and(|connection| connection.manual_retry(credentials));
            if retried {
                self.remote_ui = None;
            } else {
                self.remote_ui = Some(RemoteUiSession::error(
                    window_id,
                    "Unable to retry the remote connection.",
                ));
            }
            self.request_window_redraw(window_id);
            return;
        }
        write_remote_status(&terminal, "Connecting to remote pane…");
        let connection = match crate::remote_attach::spawn_remote_connection(
            credentials,
            Arc::clone(&terminal),
            remote_state,
            crate::io_thread::OverviewPublish {
                slot: overview_snapshot,
                visible: Arc::clone(&self.overview_visible_gate),
            },
            self.proxy.clone(),
            window_id,
            pane_id,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                write_remote_status(&terminal, "Failed to start remote connection.");
                log::warn!("failed to restart remote connection manager: {error}");
                self.remote_ui = Some(RemoteUiSession::error(
                    window_id,
                    "Unable to retry the remote connection.",
                ));
                self.request_window_redraw(window_id);
                return;
            }
        };
        if let Some(SurfaceTransport::Remote(remote)) = self
            .windows
            .get_mut(&window_id)
            .and_then(|window| window.surfaces.get_mut(&pane_id))
            .map(|surface| &mut surface.transport)
        {
            remote.connection = Some(connection);
        }
        self.remote_ui = None;
        self.request_window_redraw(window_id);
    }

    fn launch_remote_surface(
        &mut self,
        event_loop: &ActiveEventLoop,
        source_window_id: WindowId,
        endpoint: crate::remote_attach::RemoteEndpoint,
        remote_pane_id: u64,
        cached_title: Option<String>,
    ) -> RemoteSurfaceLaunchOutcome {
        let Some(source_group) = self
            .windows
            .get(&source_window_id)
            .map(|window| window.group)
        else {
            return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
        };
        let token = match self.config.client_token.clone() {
            Some(token) => match crate::remote_attach::RemoteToken::new(token) {
                Ok(token) => token,
                Err(_) => {
                    self.remote_ui = Some(RemoteUiSession::error(
                        source_window_id,
                        MISSING_TOKEN_MESSAGE,
                    ));
                    self.request_window_redraw(source_window_id);
                    return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
                }
            },
            None => {
                self.remote_ui = Some(RemoteUiSession::error(
                    source_window_id,
                    MISSING_TOKEN_MESSAGE,
                ));
                self.request_window_redraw(source_window_id);
                return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
            }
        };
        let identity = crate::remote_attach::RemotePaneIdentity {
            endpoint: endpoint.authority().to_string(),
            pane_id: remote_pane_id,
            cached_title,
        };
        let window_id = match self.spawn_detached_remote_tab_in_group(
            event_loop,
            source_group,
            identity.clone(),
        ) {
            Ok(window_id) => window_id,
            Err(error) => {
                self.remote_ui = Some(RemoteUiSession::error(
                    source_window_id,
                    format!("Unable to open remote tab: {error:#}"),
                ));
                self.request_window_redraw(source_window_id);
                return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
            }
        };
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
        };
        let remote_parts = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| match &surface.transport {
                SurfaceTransport::Remote(remote) => Some((
                    Arc::clone(&surface.terminal),
                    Arc::clone(&surface.overview_snapshot),
                    Arc::clone(&remote.state),
                )),
                SurfaceTransport::Local(_) => None,
            });
        let Some((terminal, overview_snapshot, remote_state)) = remote_parts else {
            return RemoteSurfaceLaunchOutcome::FailedBeforeSurface;
        };
        write_remote_status(&terminal, "Connecting to remote pane…");
        let credentials =
            crate::remote_attach::RemoteConnectionCredentials::new(endpoint, token, remote_pane_id);
        let connection = match crate::remote_attach::spawn_remote_connection(
            credentials,
            Arc::clone(&terminal),
            remote_state,
            crate::io_thread::OverviewPublish {
                slot: overview_snapshot,
                visible: Arc::clone(&self.overview_visible_gate),
            },
            self.proxy.clone(),
            window_id,
            pane_id,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                write_remote_status(&terminal, "Failed to start remote connection.");
                log::warn!("failed to start remote connection manager: {error}");
                self.remote_ui = None;
                self.request_window_redraw(window_id);
                return RemoteSurfaceLaunchOutcome::LocalSurfaceReady;
            }
        };
        if let Some(SurfaceTransport::Remote(remote)) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
            .map(|surface| &mut surface.transport)
        {
            remote.connection = Some(connection);
        }
        self.remote_ui = None;
        self.request_window_redraw(window_id);
        RemoteSurfaceLaunchOutcome::LocalSurfaceReady
    }
}

fn status_snapshot(message: &str, hint: &str) -> CommandPaletteSnapshot {
    CommandPaletteSnapshot {
        query: "Attach Remote".to_string(),
        rows: vec![PaletteRow::Entry {
            title: message.to_string(),
            hint: Some(hint.to_string()),
            match_positions: Vec::new(),
            enabled: false,
        }],
        selected: 0,
        total_entries: 0,
    }
}

fn remote_warning_message(endpoint: &crate::remote_attach::RemoteEndpoint) -> String {
    format!(
        "This connection is not encrypted by Noa. Connect to {} only over a trusted LAN or protected tunnel?",
        endpoint.authority()
    )
}

fn write_remote_status(terminal: &Arc<Mutex<Terminal>>, message: &str) {
    let mut terminal = terminal.lock();
    let mut stream = noa_vt::Stream::new();
    let bytes = format!("\x1b[2J\x1b[H{message}\r\n");
    stream.feed(bytes.as_bytes(), &mut *terminal);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(pane_id: u64, name: &str) -> noa_ipc::Panel {
        noa_ipc::Panel {
            window_group_id: noa_ipc::WireId(1),
            window_id: noa_ipc::WireId(1),
            pane_id: noa_ipc::WireId(pane_id),
            name: name.to_string(),
            cwd: format!("/work/{name}"),
            branch: None,
            process: None,
            busy: false,
            attention: false,
            attachable: true,
            preview: Vec::new(),
        }
    }

    #[test]
    fn missing_token_stops_before_endpoint_prompt_or_worker_gate() {
        assert!(matches!(
            remote_start_gate(Some("127.0.0.1:61771"), None),
            RemoteStartGate::MissingToken
        ));
        assert_eq!(
            MISSING_TOKEN_MESSAGE,
            "Set client-token or client-token-file in config."
        );
    }

    #[test]
    fn created_pane_cleanup_is_required_until_local_surface_is_ready() {
        assert!(RemoteSurfaceLaunchOutcome::FailedBeforeSurface.requires_created_pane_cleanup());
        assert!(!RemoteSurfaceLaunchOutcome::LocalSurfaceReady.requires_created_pane_cleanup());
    }

    #[test]
    fn failed_event_publish_returns_result_and_clears_pending_ownership() {
        let pending = Mutex::new(HashMap::new());
        let unpublished = publish_pending_result(&pending, 41, "pane-ready", || false);
        assert_eq!(unpublished, Some("pane-ready"));
        assert!(pending.lock().is_empty());

        let unpublished = publish_pending_result(&pending, 42, "discovery", || true);
        assert_eq!(unpublished, None);
        assert_eq!(pending.lock().remove(&42), Some("discovery"));
    }

    #[test]
    fn shutdown_joins_workers_before_draining_pending_results() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let worker_pending = Arc::clone(&pending);
        let mut workers = vec![std::thread::spawn(move || {
            worker_pending.lock().insert(9, "created-pane");
        })];
        let mut cleaned = Vec::new();

        join_workers_and_drain_pending(&mut workers, pending.as_ref(), |result| {
            cleaned.push(result);
        });

        assert!(workers.is_empty());
        assert!(pending.lock().is_empty());
        assert_eq!(cleaned, ["created-pane"]);
    }

    #[test]
    fn non_loopback_warning_precedes_worker_and_cancel_yields_no_request() {
        let endpoint = match remote_start_gate(Some("192.0.2.10:61771"), Some("token")) {
            RemoteStartGate::Warn(endpoint) => endpoint,
            _ => panic!("non-loopback endpoint must stop at the warning gate"),
        };
        assert!(warning_worker_endpoint(false, endpoint.clone()).is_none());
        assert_eq!(
            warning_worker_endpoint(true, endpoint).unwrap().authority(),
            "192.0.2.10:61771"
        );
    }

    #[test]
    fn picker_rows_are_stable_and_disabled_reasons_are_visible() {
        let picker = RemotePicker::new(
            vec![panel(9, "late"), panel(2, "early")],
            noa_ipc::ScopeSet::from_strings(["read", "attach"]),
        );
        assert!(picker.items[0].label.starts_with("early"));
        assert!(picker.items[1].label.starts_with("late"));
        assert_eq!(picker.items[2].label, "Create New Tab");
        assert!(
            picker.items[2]
                .display_label()
                .contains("Disabled: Control scope was not granted.")
        );
        assert!(
            picker.items[3]
                .display_label()
                .contains("Disabled: Control scope was not granted.")
        );
    }

    #[test]
    fn picker_skips_panels_without_a_raw_attach_endpoint() {
        let mut unavailable = panel(2, "remote-hop");
        unavailable.attachable = false;
        let picker = RemotePicker::new(
            vec![unavailable, panel(7, "local-shell")],
            noa_ipc::ScopeSet::from_strings(["read", "attach"]),
        );

        assert!(
            picker.items[0]
                .display_label()
                .contains("Disabled: This pane does not expose a raw attach endpoint.")
        );
        assert!(matches!(
            picker.selected_choice(),
            Some(RemotePickerChoice::ExistingPane { pane_id: 7, .. })
        ));
    }

    #[test]
    fn picker_navigation_and_create_options_dispatch_existing_rpcs() {
        let mut picker = RemotePicker::new(
            vec![panel(7, "shell")],
            noa_ipc::ScopeSet::from_strings(["read", "control", "attach"]),
        );
        assert!(matches!(
            picker.selected_choice(),
            Some(RemotePickerChoice::ExistingPane { pane_id: 7, .. })
        ));
        picker.move_down();
        assert_eq!(
            picker.selected_choice(),
            Some(RemotePickerChoice::CreateNewTab)
        );
        picker.move_down();
        assert_eq!(
            picker.selected_choice(),
            Some(RemotePickerChoice::CreateSplit { pane_id: 7 })
        );
        assert_eq!(
            picker.items[picker.selected].hint.as_deref(),
            Some("From Pane 7")
        );
        picker.move_down();
        assert_eq!(
            picker.selected_choice(),
            Some(RemotePickerChoice::CreateSplit { pane_id: 7 })
        );
        picker.move_up();
        assert_eq!(
            picker.selected_choice(),
            Some(RemotePickerChoice::CreateNewTab)
        );
    }

    #[test]
    fn focused_retry_is_enabled_only_for_detached_remote_state() {
        assert_eq!(
            retry_disabled_reason(&crate::remote_attach::RemoteAttachState::Detached),
            None
        );
        assert_eq!(
            retry_disabled_reason(&crate::remote_attach::RemoteAttachState::Connected),
            Some("Remote pane is already connected.")
        );
        assert_eq!(
            retry_disabled_reason(&crate::remote_attach::RemoteAttachState::Reconnecting {
                attempt: 3,
                delay: Duration::from_secs(4),
            }),
            Some("Remote reconnect is already in progress.")
        );
    }

    #[test]
    fn restored_detached_retry_requires_warning_before_same_pane_connection() {
        let detached = crate::remote_attach::RemoteAttachState::Detached;
        assert!(matches!(
            remote_retry_gate(&detached, "192.0.2.10:61771", Some("token")),
            RemoteRetryGate::Warn(_)
        ));
        assert!(matches!(
            remote_retry_gate(&detached, "127.0.0.1:61771", Some("new-token")),
            RemoteRetryGate::Start(_)
        ));
        assert!(matches!(
            remote_retry_gate(&detached, "127.0.0.1:61771", None),
            RemoteRetryGate::MissingToken
        ));
        assert!(matches!(
            remote_retry_gate(&detached, "127.0.0.1:61771", Some("token")),
            RemoteRetryGate::Start(_)
        ));
        let warning_endpoint = match remote_retry_gate(&detached, "192.0.2.10:61771", Some("token"))
        {
            RemoteRetryGate::Warn(endpoint) => endpoint,
            _ => panic!("restored non-loopback retry must require confirmation"),
        };
        assert_eq!(
            remote_warning_message(&warning_endpoint),
            "This connection is not encrypted by Noa. Connect to 192.0.2.10:61771 only over a trusted LAN or protected tunnel?"
        );
        assert!(matches!(
            remote_retry_gate(&detached, "192.0.2.10:61771", None),
            RemoteRetryGate::MissingToken
        ));
    }

    #[test]
    fn empty_picker_exposes_creation_and_never_turns_enter_into_retry() {
        let picker = RemotePicker::new(
            Vec::new(),
            noa_ipc::ScopeSet::from_strings(["control", "attach"]),
        );
        assert_eq!(picker.panel_count, 0);
        assert_eq!(
            picker.selected_choice(),
            Some(RemotePickerChoice::CreateNewTab)
        );
        assert!(
            picker.items[1]
                .display_label()
                .contains("No remote pane is available to split.")
        );
    }
}
