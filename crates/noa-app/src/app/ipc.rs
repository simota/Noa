//! `noa-ipc` server lifecycle + the two integration seams the spec calls for
//! (`docs/specs/noa-server.md` §L2 "クレート配置 & 統合点"): a main-thread
//! read snapshot rebuilt in `about_to_wait` (mirrors
//! `App::sync_applescript_snapshot`) and a mutation path through
//! `UserEvent::IpcAction` (DEC-C).

use std::hash::{Hash, Hasher};

use super::applescript::{ScriptSpawn, usable_cwd};
use super::state::{IpcSpawn, Orchestration};
use super::*;
use crate::ipc_bridge::{
    AppIpcBackend, IpcActionKind, IpcActionReply, PanelSurfaceFacts, bare_panel, card_to_panel,
    registry_key,
};
use crate::session_store::SessionCardId;
use noa_ipc::Panel;

/// Coarse refresh interval for the IPC snapshot's lock-bearing fields
/// (mirrors `APPLESCRIPT_SNAPSHOT_REFRESH` in `app/applescript.rs`).
const IPC_SNAPSHOT_REFRESH: Duration = Duration::from_millis(500);

impl App {
    /// Start the `noa-ipc` server once, after the app is running
    /// (mirrors `install_applescript_if_needed`). A no-op when
    /// `server-enable` is false. Bind failure logs a warning and leaves the
    /// app running without a server (FR-2).
    pub(super) fn install_ipc_server_if_needed(&mut self) {
        if self.ipc_install_attempted {
            return;
        }
        self.ipc_install_attempted = true;
        if !self.config.server_enable {
            // Disabled is not a failure — clear any stale error from a
            // previous enabled-but-failed-to-bind session so the Settings
            // panel's status row reads plain "Stopped", not a leftover
            // bind-failure reason (settings-panel-server-status).
            self.ipc_last_error = None;
            self.refresh_theme_settings_server_status();
            return;
        }

        let Some(token_path) = noa_config::server_token_path() else {
            log::warn!("noa-ipc: cannot resolve token path, server not started");
            self.ipc_last_error = Some("cannot resolve token path".to_string());
            self.refresh_theme_settings_server_status();
            return;
        };
        let token =
            match noa_ipc::load_or_create_token(&token_path, self.config.server_token.as_deref()) {
                Ok(token) => token,
                Err(err) => {
                    log::warn!("noa-ipc: failed to load/create token: {err}");
                    self.ipc_last_error = Some(format!("failed to load token: {err}"));
                    self.refresh_theme_settings_server_status();
                    return;
                }
            };
        let allowed_scopes = noa_ipc::ScopeSet::parse_list(&self.config.server_scopes);
        // noa-config already validates `server-bind` parses as an IP address
        // (falling back to the loopback default on a bad value), so this
        // `.parse()` cannot fail in practice; the `unwrap_or` is defense in
        // depth only, matching this call site's existing bind-failure
        // handling rather than trusting a single validation layer.
        let bind_addr: std::net::IpAddr = self
            .config
            .server_bind
            .parse()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let backend = AppIpcBackend {
            shared: self.ipc_shared.clone(),
            proxy: self.proxy.clone(),
            pending: self.ipc_pending.clone(),
            next_request: self.ipc_next_request.clone(),
        };
        let config = noa_ipc::ServerConfig {
            port: self.config.server_port,
            bind_addr,
            token,
            allowed_scopes,
            hello_deadline: noa_ipc::ServerConfig::DEFAULT_HELLO_DEADLINE,
            handshake_timeout: noa_ipc::ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
        };
        match noa_ipc::Server::start(
            config,
            std::sync::Arc::new(backend),
            self.ipc_broadcaster.clone(),
        ) {
            Ok(handle) => {
                log::info!(
                    "noa-ipc: listening on {}:{}",
                    handle.bind_addr(),
                    handle.port()
                );
                if !handle.bind_addr().is_loopback() {
                    log::warn!(
                        "noa-server listening on a non-loopback address ({}) — LAN-exposed; token auth still required",
                        handle.bind_addr()
                    );
                }
                self.ipc_server = Some(handle);
                self.ipc_last_error = None;
            }
            Err(err) => {
                log::warn!(
                    "noa-ipc: failed to bind {bind_addr}:{}: {err}",
                    self.config.server_port
                );
                self.ipc_last_error = Some(format!(
                    "failed to bind {bind_addr}:{}: {err}",
                    self.config.server_port
                ));
            }
        }
        self.refresh_theme_settings_server_status();
    }

    /// settings-panel-server-status: the `ServerStatus` row's display text
    /// for the server's *current* live state — running (with client count),
    /// stopped, or the last bind failure. A pure formatting call over
    /// `self.ipc_server`/`self.ipc_broadcaster`/`self.ipc_last_error`, kept
    /// as its own method so both `open_theme_settings_session` (seeding a
    /// freshly opened row) and [`Self::refresh_theme_settings_server_status`]
    /// (pushing an update into an already-open one) compute it identically.
    pub(super) fn server_status_display(&self) -> String {
        let running = self.ipc_server.as_ref().map(|handle| {
            (
                handle.bind_addr().to_string(),
                handle.port(),
                self.ipc_broadcaster.connection_count(),
            )
        });
        crate::theme_settings::format_server_status(running, self.ipc_last_error.as_deref())
    }

    /// settings-panel-server-status (liveness, R-3/AC-4a-equivalent): push a
    /// fresh `ServerStatus` row value into the open Settings panel, if any
    /// is open. The render path (wgpu overlay text + the native macOS card)
    /// only ever reads `ThemeSettings`/`RowDraft` — it has no `&App`
    /// reference — so a toggle's effect can't be picked up by simply
    /// re-rendering; this out-of-band push is the mechanism instead. Called
    /// from every branch of [`Self::install_ipc_server_if_needed`] (which
    /// [`Self::restart_ipc_server`] always re-runs), so a `server-enable`/
    /// `server-port`/`server-scopes` edit reflects in an already-open panel
    /// within one `ConfigWatcher` poll tick (~500ms) of the panel's own
    /// commit landing — no reopen needed. A no-op while no panel is open
    /// (the common case: this fires on every server (re)install, most of
    /// which happen before any Settings session exists).
    pub(super) fn refresh_theme_settings_server_status(&mut self) {
        if self.theme_settings.is_none() {
            return;
        }
        let status = self.server_status_display();
        let session = self.theme_settings.as_mut().expect("checked Some above");
        std::sync::Arc::make_mut(&mut session.state).set_server_status(status);
    }

    /// Tears down the running `noa-ipc` server (if any) and re-runs
    /// `install_ipc_server_if_needed` against the just-reloaded config
    /// (G-2: config reload's `server-enable`/`server-port`/`server-token`/
    /// `server-scopes` keys otherwise have no live effect). Dropping
    /// `ServerHandle` stops its accept loop and joins it, and every
    /// in-flight connection thread self-terminates within its own ~50ms
    /// read-timeout poll of the shared shutdown flag (`noa_ipc::server`).
    ///
    /// `self.ipc_broadcaster` outlives this restart (`ipc.rs`'s field doc),
    /// so panes spawned before the restart keep pushing to whichever server
    /// currently owns the broadcaster's connections — no respawn needed.
    pub(super) fn restart_ipc_server(&mut self) {
        self.ipc_server = None;
        self.ipc_install_attempted = false;
        self.install_ipc_server_if_needed();
    }

    /// Rebuild the IPC read snapshot (panels + pane-id registry + terminal
    /// handles) and broadcast a `state_changed` diff to subscribers (FR-16).
    /// Called from `about_to_wait`; a structural-signature change (F-5:
    /// pane set, busy, attention, or display name) rebuilds immediately —
    /// the coarser `IPC_SNAPSHOT_REFRESH` tick still runs on top of that to
    /// catch cwd/preview staleness the cheap signature doesn't cover.
    /// Gated on the server actually running so a disabled server costs
    /// nothing (spec "Zero overhead when disabled").
    pub(super) fn sync_ipc_snapshot(&mut self) {
        if self.ipc_server.is_none() {
            return;
        }

        let sig = self.ipc_structural_signature();
        let due = self
            .ipc_snapshot_at
            .is_none_or(|at| at.elapsed() >= IPC_SNAPSHOT_REFRESH);
        if sig == self.ipc_snapshot_sig && !due {
            return;
        }
        self.ipc_snapshot_sig = sig;
        self.ipc_snapshot_at = Some(Instant::now());

        let mut panels = Vec::new();
        let mut terminals = HashMap::new();
        // Raw attach resources are registered when the Surface is spawned,
        // before the PTY's first output creates its session card. Treat the
        // Surface map as the lifetime authority so an initially silent pane
        // cannot lose its attach endpoint during the first snapshot tick.
        let mut live_keys: HashSet<_> = self
            .windows
            .iter()
            .flat_map(|(window_id, state)| {
                let window_id = u64::from(*window_id);
                state
                    .surfaces
                    .keys()
                    .map(move |pane_id| (window_id, pane_id.get()))
            })
            .collect();
        let previous_panels = {
            let mut shared = self.ipc_shared.lock();
            for (id, card) in self.session_store.ordered_cards() {
                let window_id = WindowId::from(id.window_id.0);
                let Some(state) = self.windows.get(&window_id) else {
                    continue;
                };
                // Floats are not listed over IPC, except a worker float: one
                // with a parent pane (floating-panes REQ-023 as amended).
                let has_parent = state.surfaces.get(&id.pane_id).is_some_and(|surface| {
                    surface
                        .orchestration
                        .as_ref()
                        .is_some_and(|meta| meta.parent.is_some())
                });
                if !state.contains_pane(id.pane_id)
                    || !listed_over_ipc(state.is_float(id.pane_id), has_parent)
                {
                    continue;
                }
                let key = registry_key(id);
                let ipc_id = shared.registry.mint(key.0, key.1);
                let attachable = shared.attach_panes.contains_key(&key);
                live_keys.insert(key);
                let surface = state.surfaces.get(&id.pane_id);
                panels.push(card_to_panel(
                    ipc_id,
                    state.group.0,
                    key.0,
                    card,
                    attachable,
                    surface.map(surface_facts).unwrap_or_default(),
                ));
                if let Some(surface) = surface {
                    terminals.insert(key, surface.terminal.clone());
                }
            }
            let listed: HashSet<(u64, u64)> = terminals.keys().copied().collect();
            let eligible: HashSet<WindowId> = self
                .windows
                .keys()
                .copied()
                .filter(|window_id| self.window_sidebar_eligible(*window_id))
                .collect();
            let candidates = self.windows.iter().flat_map(|(window_id, state)| {
                let eligible = eligible.contains(window_id);
                state
                    .surfaces
                    .iter()
                    .map(move |(pane_id, surface)| ListCandidate {
                        key: (u64::from(*window_id), pane_id.get()),
                        float: state.is_float(*pane_id),
                        worker: surface.orchestration.is_some(),
                        parent: surface
                            .orchestration
                            .as_ref()
                            .is_some_and(|meta| meta.parent.is_some()),
                        eligible,
                    })
            });
            for key in uncarded_panes(&listed, candidates) {
                let window_id = WindowId::from(key.0);
                let pane_id = PaneId::new(key.1);
                let Some((state, surface)) = self
                    .windows
                    .get(&window_id)
                    .and_then(|state| Some((state, state.surfaces.get(&pane_id)?)))
                else {
                    continue;
                };
                let ipc_id = shared.registry.mint(key.0, key.1);
                let attachable = shared.attach_panes.contains_key(&key);
                panels.push(bare_panel(
                    ipc_id,
                    state.group.0,
                    key.0,
                    attachable,
                    surface_facts(surface),
                ));
                terminals.insert(key, surface.terminal.clone());
            }
            let previous = std::mem::replace(&mut shared.panels, panels);
            shared.terminals = terminals;
            let stale_attach_keys = stale_registry_keys(shared.attach_panes.keys(), &live_keys);
            for key in stale_attach_keys {
                if let Some(attach) = shared.attach_panes.remove(&key) {
                    attach.shutdown();
                }
            }
            // Closed panes never reappear under the same `(window_id,
            // pane_id)` key (`WindowId`/`PaneId` are never reused), so
            // pruning anything absent from the live Surface set is safe.
            shared.registry.prune(&live_keys);
            previous
        };

        // Diff by pane id and broadcast only what changed or was added
        // (F-5): a pane that disappeared from `panels` (closed) has no
        // removal event in v1 — the spec leaves that to the next
        // `noa.listPanels` poll, so it's simply absent from this diff.
        //
        // With nobody subscribed there is no one to tell. Asked after the new
        // listing is in place: a waiter subscribing from here on lists it
        // itself before it waits (`+ctl wait`, `send --wait`).
        if !self.ipc_broadcaster.has_state_changed_subscribers() {
            return;
        }
        let changed = changed_panels(previous_panels, &self.ipc_shared.lock().panels);
        if !changed.is_empty() {
            self.ipc_broadcaster.broadcast_state_changed(changed);
        }
    }

    /// A lock-free signature over the IPC snapshot's cheap, frequently
    /// changing fields (pane set, busy, attention, display name, and the
    /// agent-orchestration state a waiter blocks on: agent report count,
    /// held-open exit) — cwd,
    /// branch, process, and preview are left to the coarser
    /// `IPC_SNAPSHOT_REFRESH` tick since they either change rarely or are
    /// too expensive to read every wake (preview specifically is never
    /// cloned here). Mirrors `applescript_structural_signature`.
    fn ipc_structural_signature(&self) -> u64 {
        let mut panes: Vec<_> = self
            .windows
            .iter()
            .flat_map(|(window_id, state)| {
                state.surfaces.iter().map(move |(pane_id, surface)| {
                    ((u64::from(*window_id), pane_id.get()), surface.exited)
                })
            })
            .collect();
        panes.sort_unstable();
        structural_signature(&self.session_store.ordered_cards(), &panes)
    }

    /// Handle `UserEvent::IpcAction { request_id }`: take the pending
    /// mutation, execute it through the same internal methods the
    /// AppleScript/UI paths already use, and reply. A missing/raced request
    /// (the connection thread already gave up) is silently dropped.
    pub(super) fn handle_ipc_action(&mut self, event_loop: &ActiveEventLoop, request_id: u64) {
        let Some(pending) = self.ipc_pending.lock().remove(&request_id) else {
            return;
        };
        let result = self.execute_ipc_action(event_loop, pending.action);
        let _ = pending.reply.send(result);
    }

    fn execute_ipc_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        action: IpcActionKind,
    ) -> Result<IpcActionReply, noa_ipc::IpcError> {
        match action {
            IpcActionKind::FocusPane { pane } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                self.focus_pane(window_id, pane_id);
                self.focus_tab(window_id);
                Ok(IpcActionReply::Ok)
            }
            IpcActionKind::NewTab { window } => {
                let target_window = match window {
                    Some(id) => Some(self.resolve_ipc_window_or_group(id)?),
                    None => None,
                };
                // `newTab`'s `windowId` targets the logical window group
                // (spec: a tab is created *in* a window). Temporarily
                // pointing `self.focused` at a tab of that group makes
                // `spawn_tab`'s `SpawnTarget::CurrentWindow` join it; the
                // spawn itself then focuses the new tab, matching normal
                // "new tab" UX.
                if let Some(window_id) = target_window
                    && self.windows.contains_key(&window_id)
                {
                    self.focused = Some(window_id);
                }
                let new_window_id = self
                    .spawn_tab(event_loop, SpawnTarget::CurrentWindow)
                    .map_err(|err| {
                        // Don't echo the raw spawn error to the client — it can
                        // carry local shell/filesystem detail (a failed execve
                        // path, etc.). Log the specifics server-side; the client
                        // gets a generic Internal error.
                        log::warn!("noa-ipc: newTab spawn failed: {err}");
                        noa_ipc::IpcError::Internal("failed to spawn tab".to_string())
                    })?;
                let pane_id = self
                    .windows
                    .get(&new_window_id)
                    .map(|state| state.focused_pane)
                    .ok_or_else(|| {
                        noa_ipc::IpcError::Internal("new tab has no pane".to_string())
                    })?;
                let ipc_id = self.mint_ipc_pane(new_window_id, pane_id);
                Ok(IpcActionReply::NewPane(ipc_id))
            }
            IpcActionKind::Split { pane, direction } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                self.focus_pane(window_id, pane_id);
                let direction = match direction {
                    noa_ipc::SplitDirection::Horizontal => Direction::Right,
                    noa_ipc::SplitDirection::Vertical => Direction::Down,
                };
                self.new_split(window_id, direction);
                let new_pane = self
                    .windows
                    .get(&window_id)
                    .map(|state| state.focused_pane)
                    .filter(|pane| *pane != pane_id)
                    .ok_or_else(|| {
                        noa_ipc::IpcError::Internal("split did not create a pane".to_string())
                    })?;
                let ipc_id = self.mint_ipc_pane(window_id, new_pane);
                Ok(IpcActionReply::NewPane(ipc_id))
            }
            IpcActionKind::ClosePane { pane } => {
                // IPC control scope is authorized automation (spec: closePane
                // closes immediately, no GUI confirmation) — go straight to
                // the same force-close path the confirm dialog's own accept
                // handler uses (`ConfirmAction::ClosePane` in
                // `input_ops/clipboard_confirm.rs`), bypassing
                // `request_close_pane`'s running-process dialog entirely so
                // no dialog is ever opened and the pane is closed for real
                // before this replies `Ok`.
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                self.close_pane(event_loop, window_id, pane_id);
                // Like a spawn: a client listing right after this reply must
                // no longer see the pane.
                self.ipc_snapshot_sig = 0;
                self.ipc_snapshot_at = None;
                self.sync_ipc_snapshot();
                Ok(IpcActionReply::Ok)
            }
            IpcActionKind::SendText {
                pane,
                text,
                paste,
                require_paste_mode,
            } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                let bytes = if paste {
                    let bracketed = self.bracketed_paste(window_id, pane_id);
                    check_paste_mode(pane, require_paste_mode, bracketed)?;
                    input::applescript_input_bytes(&text, bracketed)
                } else {
                    // Raw injection (noa.sendText paste:false): keyboard-like
                    // input, not a paste, so skip the bracketed-paste wrap
                    // and don't mark_pane_paste_input — that flag feeds the
                    // auto-approve guard's paste heuristics, which don't
                    // apply here.
                    input::raw_input_bytes(&text)
                };
                if let Some(bytes) = bytes {
                    if paste {
                        self.mark_pane_paste_input(window_id, pane_id);
                    }
                    self.snap_pane_viewport_to_bottom(window_id, pane_id);
                    // The queue outcome is the reply: a full input budget or a
                    // gone sink must surface to the automation client, which
                    // otherwise proceeds as if the text had been accepted
                    // (N02). `write_pane_pty_bytes` only logs those.
                    let result = self.queue_pane_pty_bytes(window_id, pane_id, bytes);
                    ipc_input_queue_result(result)?;
                    // Answering a pane acknowledges its notification, like
                    // focusing it does: without that `attention` never drops
                    // and a hookless agent's next turn cannot raise it again
                    // (agent-orchestration D6). Only input that was accepted
                    // answers. The drop is pushed now, before the pane's reply
                    // can come back as a later event on this thread.
                    if self.acknowledge_session_card(window_id, pane_id) {
                        self.ipc_snapshot_sig = 0;
                        self.ipc_snapshot_at = None;
                        self.sync_ipc_snapshot();
                    }
                }
                Ok(IpcActionReply::Ok)
            }
            IpcActionKind::Spawn { target, params } => self
                .execute_ipc_spawn(event_loop, target, params)
                .map(IpcActionReply::NewPane),
            IpcActionKind::ResizePane { pane, cols, rows } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                let surface = self
                    .windows
                    .get_mut(&window_id)
                    .and_then(|state| state.surfaces.get_mut(&pane_id))
                    .ok_or(noa_ipc::IpcError::PaneClosed)?;
                let resize_tx = match &surface.transport {
                    SurfaceTransport::Local(local) => local.resize_tx.clone(),
                    SurfaceTransport::Remote(_) => {
                        return Err(noa_ipc::IpcError::Unsupported(
                            "resize server-side remote pane",
                        ));
                    }
                };
                apply_attach_grid_first_resize(
                    &surface.terminal,
                    &mut surface.grid_size,
                    GridSize::new(cols, rows),
                    |size| {
                        resize_tx
                            .send(size)
                            .map_err(|_| noa_ipc::IpcError::PaneClosed)
                    },
                )?;
                if let SurfaceTransport::Local(local) = &surface.transport
                    && let Some(io_thread) = local.io_thread.as_ref()
                {
                    io_thread.request_ipc_output_refresh();
                }
                Ok(IpcActionReply::Ok)
            }
        }
    }

    /// `noa.newTab`/`noa.split` with agent-orchestration extensions (D2):
    /// spawn with the request's config through the same `ScriptSpawn`
    /// overrides AppleScript's `with configuration` uses, place an `auto`
    /// split among the anchor's own spawns, and put keyboard focus back when
    /// asked to. Returns the new pane's IPC id.
    fn execute_ipc_spawn(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: noa_ipc::SpawnTarget,
        params: noa_ipc::SpawnParams,
    ) -> Result<u64, noa_ipc::IpcError> {
        if let Some(parent) = params.parent_pane_id {
            self.resolve_ipc_pane(parent.0)?;
            check_nesting(parent.0, |pane| self.ipc_orchestrator_of(pane))?;
            // REL-02: counted here, on the UI thread that also creates the
            // pane, so concurrent spawns from one parent cannot overshoot.
            if let Some(most) = params.max_children {
                let children = self.windows.values().flat_map(|state| {
                    state.surfaces.values().map(|surface| {
                        (
                            surface.orchestration.as_ref().and_then(|meta| meta.parent),
                            surface.exited,
                        )
                    })
                });
                check_max_children(parent.0, most, children)?;
            }
        }
        let previous_focus = self
            .focused
            .and_then(|window| Some((window, self.windows.get(&window)?.focused_pane)));
        let focus = params.focus;
        let focused_before = self.window_focused_panes();
        // Placing the worker focuses panes (split candidates, the anchor),
        // which acknowledges their notifications; nobody looked at them.
        let unread_before = self.session_store.unread_flags();
        let board_of = board_orchestrator(params.label.as_deref());
        let result = self.ipc_spawn_pane(event_loop, target, params);
        let focused_after = self.window_focused_panes();
        let restores = focus_to_restore(
            focus,
            previous_focus,
            &focused_before,
            &focused_after,
            result.as_ref().ok().copied(),
            self.focused,
        );
        for (window, pane, raise_tab) in restores {
            if self
                .windows
                .get(&window)
                .is_some_and(|state| state.contains_pane(pane))
            {
                self.focus_pane(window, pane);
                if raise_tab {
                    // A script must not pull Noa in front of the app the
                    // user is in (agent-automation ASSUME-6).
                    self.raise_tab_without_activating(window);
                }
            }
        }
        if self.session_store.restore_unread(&unread_before) {
            self.request_sidebar_redraw();
        }
        let (window_id, pane_id) = result?;
        // D10: a new board takes back the workers an earlier board of the
        // same orchestrator handed back to it. After the focus restore, so
        // the pane the user has focused is the one left where it is.
        if let Some(orchestrator) = board_of.and_then(|id| self.resolve_ipc_pane(id).ok()) {
            self.restow_handed_back_floats(window_id, pane_id, orchestrator);
        }
        let ipc_id = self.mint_ipc_pane(window_id, pane_id);
        // Rebuild the snapshot before replying, so a client that lists panes
        // right after this reply already sees the new worker.
        self.ipc_snapshot_sig = 0;
        self.ipc_snapshot_at = None;
        self.sync_ipc_snapshot();
        Ok(ipc_id)
    }

    /// Every window's focused pane, to see which ones a spawn moved.
    fn window_focused_panes(&self) -> Vec<(WindowId, PaneId)> {
        self.windows
            .iter()
            .map(|(window, state)| (*window, state.focused_pane))
            .collect()
    }

    /// The spawn half of [`Self::execute_ipc_spawn`]: create the pane and
    /// apply its config and orchestration metadata. Focus is left wherever
    /// the spawn put it.
    fn ipc_spawn_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: noa_ipc::SpawnTarget,
        params: noa_ipc::SpawnParams,
    ) -> Result<(WindowId, PaneId), noa_ipc::IpcError> {
        let config = params.config.unwrap_or_default();
        let spawn_cwd = usable_cwd(config.cwd.clone());
        let spawn = ScriptSpawn {
            cwd: spawn_cwd.clone(),
            argv: config.command.clone(),
            env: config.env.clone().into_iter().collect(),
            wait_after_command: config.wait_after_command,
            ..ScriptSpawn::default()
        };

        let created = match target {
            noa_ipc::SpawnTarget::Tab { window } => {
                let window = match window {
                    Some(id) => Some(self.resolve_ipc_window_or_group(id)?),
                    None => None,
                };
                self.ipc_spawn_tab(event_loop, window, spawn)?
            }
            noa_ipc::SpawnTarget::Float { pane } => {
                let (window_id, target) = self.resolve_ipc_pane(pane)?;
                let (anchor, full, stowed) = self
                    .windows
                    .get(&window_id)
                    .map(|state| {
                        let anchor = float_host(
                            target,
                            state.floats.get(target).map(|float| float.anchor),
                            state.split_focus(),
                        );
                        let anchor_label = state
                            .surfaces
                            .get(&anchor)
                            .and_then(|surface| surface.orchestration.as_ref())
                            .and_then(|orchestration| orchestration.label.as_deref());
                        // D13: a worker's worker goes on the board its own
                        // orchestrator (the worker) is stowed on.
                        let parent_stowed_under = params
                            .parent_pane_id
                            .and_then(|parent| self.resolve_ipc_pane(parent.0).ok())
                            .filter(|(window, pane)| {
                                *window == window_id
                                    && state.floats.get(*pane).is_some_and(|float| {
                                        float.is_stowed()
                                            && float.anchor
                                                == crate::floating::FloatAnchor::Pane(anchor)
                                    })
                            })
                            .and_then(|(_, pane)| state.surfaces.get(&pane))
                            .and_then(|surface| surface.orchestration.as_ref())
                            .and_then(|orchestration| orchestration.parent);
                        let stowed = stows_on(
                            anchor_label,
                            params.parent_pane_id.map(|id| id.0),
                            parent_stowed_under,
                        );
                        let most = crate::floating::max_on_anchor(stowed);
                        let full = state
                            .floats
                            .count_on(crate::floating::FloatAnchor::Pane(anchor))
                            >= most;
                        (anchor, full.then_some(most), stowed)
                    })
                    .ok_or(noa_ipc::IpcError::PaneClosed)?;
                let float = self
                    .ipc_float_with(window_id, anchor, stowed, spawn, float_start(params.focus))
                    .map_err(|reason| {
                        noa_ipc::IpcError::Internal(if let Some(most) = full {
                            format!("this pane already holds the most floats it can ({most})")
                        } else {
                            format!("the float could not open: {reason}")
                        })
                    })?;
                (window_id, float)
            }
            noa_ipc::SpawnTarget::Split { pane, direction } => {
                let (window_id, anchor) = self.resolve_ipc_pane(pane)?;
                let candidates = match direction {
                    Some(noa_ipc::SplitDirection::Horizontal) => {
                        vec![(window_id, anchor, Direction::Right)]
                    }
                    Some(noa_ipc::SplitDirection::Vertical) => {
                        vec![(window_id, anchor, Direction::Down)]
                    }
                    None => self.auto_split_candidates(window_id, anchor, pane),
                };
                let split = candidates
                    .into_iter()
                    .find_map(|(window, target, direction)| {
                        self.ipc_split_with(window, target, direction, &spawn)
                            .map(|pane| (window, pane))
                    });
                match (split, direction) {
                    (Some(created), _) => created,
                    // `auto` overflows into a new tab of the same window once
                    // no pane has room left to split.
                    (None, None) => self.ipc_spawn_tab(event_loop, Some(window_id), spawn)?,
                    (None, Some(_)) => {
                        return Err(noa_ipc::IpcError::Internal(
                            "split did not create a pane".to_string(),
                        ));
                    }
                }
            }
        };
        let (window_id, pane_id) = created;

        if (params.parent_pane_id.is_some() || params.label.is_some())
            && let Some(surface) = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
        {
            surface.orchestration = Some(Orchestration {
                parent: params.parent_pane_id.map(|id| id.0),
                label: params.label,
                cwd: spawn_cwd,
            });
        }
        if let Some(argv) = config.command
            && let Some(surface) = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
        {
            surface.ipc_spawn = Some(IpcSpawn {
                argv,
                started_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs()),
            });
        }
        self.write_initial_input(window_id, pane_id, config.initial_input);
        Ok((window_id, pane_id))
    }

    /// Split `target` in `window_id` with `spawn`'s overrides; the new pane,
    /// or `None` when the split had no room.
    fn ipc_split_with(
        &mut self,
        window_id: WindowId,
        target: PaneId,
        direction: Direction,
        spawn: &ScriptSpawn,
    ) -> Option<PaneId> {
        let state = self.windows.get(&window_id)?;
        // A float is not in the split layout: there is nothing to split.
        if state.is_float(target) {
            return None;
        }
        let before: HashSet<PaneId> = state.surfaces.keys().copied().collect();
        self.focus_pane(window_id, target);
        self.script_spawn = Some(spawn.clone());
        self.new_split(window_id, direction);
        self.script_spawn = None;
        let state = self.windows.get(&window_id)?;
        split_created(&before, state.surfaces.keys().copied())
    }

    /// A worker float over `anchor` with `spawn`'s overrides: collapsed to
    /// its title strip so it stays out of the way until someone opens it, or
    /// open with focus when `start` says so; `None` when the pane already
    /// holds as many floats as it can.
    fn ipc_float_with(
        &mut self,
        window_id: WindowId,
        anchor: PaneId,
        stowed: bool,
        spawn: ScriptSpawn,
        start: FloatStart,
    ) -> Result<PaneId, String> {
        self.script_spawn = Some(spawn);
        let stow = match (stowed, start) {
            (false, _) => super::floating::Stow::No,
            (true, FloatStart::OpenFocused) => super::floating::Stow::Open,
            (true, FloatStart::Collapsed) => super::floating::Stow::Collapsed,
        };
        let float = self.create_float(
            window_id,
            anchor,
            stow,
            super::floating::IpcExposure::Listed,
        );
        self.script_spawn = None;
        let float = float?;
        match start {
            // A stowed worker starts collapsed already.
            FloatStart::Collapsed if !stowed => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.floats.toggle_collapse(float);
                }
            }
            FloatStart::Collapsed => {}
            FloatStart::OpenFocused => self.focus_pane(window_id, float),
        }
        self.after_float_change(window_id);
        Ok(float)
    }

    /// A new tab with `spawn`'s overrides, in `window`'s group (the focused
    /// window's when `None`), like `noa.newTab`.
    fn ipc_spawn_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: Option<WindowId>,
        spawn: ScriptSpawn,
    ) -> Result<(WindowId, PaneId), noa_ipc::IpcError> {
        if let Some(window_id) = window
            && self.windows.contains_key(&window_id)
        {
            self.focused = Some(window_id);
        }
        // Without a requested cwd the tab inherits the focused shell's, like
        // an interactive New Tab.
        let cwd = spawn.cwd.clone().or_else(|| self.focused_pane_cwd());
        self.script_spawn = Some(spawn);
        let result = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow);
        self.script_spawn = None;
        let new_window_id = result.map_err(|err| {
            log::warn!("noa-ipc: spawn failed (cwd {cwd:?}): {err}");
            noa_ipc::IpcError::Internal("failed to spawn pane".to_string())
        })?;
        let pane_id = self
            .windows
            .get(&new_window_id)
            .map(|state| state.focused_pane)
            .ok_or_else(|| noa_ipc::IpcError::Internal("new tab has no pane".to_string()))?;
        Ok((new_window_id, pane_id))
    }

    /// Where an `auto` split goes (agent-orchestration D2), most preferred
    /// first: among `anchor` and the panes in its window spawned with
    /// `anchor_ipc` as parent. See [`auto_split_order`].
    fn auto_split_candidates(
        &self,
        window_id: WindowId,
        anchor: PaneId,
        anchor_ipc: u64,
    ) -> Vec<(WindowId, PaneId, Direction)> {
        let Some(anchor_rect) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&anchor))
            .map(|surface| surface.rect)
        else {
            return Vec::new();
        };
        let surfaces = self.windows.iter().flat_map(|(window, state)| {
            state
                .surfaces
                .iter()
                .map(move |(pane, surface)| SpawnedPane {
                    window: *window,
                    pane: *pane,
                    rect: surface.rect,
                    parent: surface.orchestration.as_ref().and_then(|meta| meta.parent),
                    float: state.is_float(*pane),
                })
        });
        let children = anchor_children(anchor_ipc, surfaces);
        auto_split_order((window_id, anchor, anchor_rect), &children)
    }

    /// Resolve `noa.newTab`'s `windowId` param (F-2). The wire value is
    /// *not* a pane-registry id: `listPanels`' `Panel.windowId` is the raw
    /// native winit window id (a tab), and `Panel.windowGroupId` is the
    /// logical window group a tab belongs to (spec §L2 "ID モデル": "階層は
    /// windowGroup(論理ウィンドウ)→ window(ネイティブタブ)→ pane"). The
    /// spec's method table just calls this param "指定ウィンドウ" without
    /// picking one of the two, so both are accepted here: a native window id
    /// is tried first (exact tab match), falling back to a window-group id
    /// (any live tab in that group). Neither found -> `UnknownPane`
    /// (-32002).
    fn resolve_ipc_window_or_group(&self, id: u64) -> Result<WindowId, noa_ipc::IpcError> {
        let window_id = WindowId::from(id);
        if self.windows.contains_key(&window_id) {
            return Ok(window_id);
        }
        self.windows
            .iter()
            .find(|(_, state)| state.group.0 == id)
            .map(|(window_id, _)| *window_id)
            .ok_or(noa_ipc::IpcError::UnknownPane)
    }

    /// Resolve an IPC pane id to its live `(WindowId, PaneId)`, rejecting
    /// stale registry entries whose pane has since closed (spec: "removed
    /// panes → later lookups return UnknownPane").
    pub(super) fn resolve_ipc_pane(
        &self,
        pane: u64,
    ) -> Result<(WindowId, PaneId), noa_ipc::IpcError> {
        let (native_window_id, native_pane_id) = self
            .ipc_shared
            .lock()
            .registry
            .resolve(pane)
            .ok_or(noa_ipc::IpcError::UnknownPane)?;
        let window_id = WindowId::from(native_window_id);
        let pane_id = PaneId::new(native_pane_id);
        if self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.contains_pane(pane_id))
        {
            Ok((window_id, pane_id))
        } else {
            Err(noa_ipc::IpcError::UnknownPane)
        }
    }

    /// The IPC output-push handle every newly spawned pane's io thread
    /// carries (FR-17), unconditionally — even when the `noa-ipc` server
    /// isn't running yet. Mints the pane's IPC id eagerly (rather than
    /// waiting for the next `sync_ipc_snapshot` tick) so output can push
    /// from the pane's very first byte once someone does subscribe.
    ///
    /// R-3: this used to return `None` while the server was disabled, and
    /// that `Option` was baked into the pane's io thread once at spawn —
    /// enabling the server later via config reload left every
    /// already-spawned pane permanently silent, since nothing ever re-wired
    /// it. Handing out a tap unconditionally fixes that; the actual "is
    /// this worth doing" gate has moved to
    /// `Broadcaster::has_output_subscriber_for(pane_id)`, consulted per feed
    /// in `feed_terminal_batch` — a running-but-unsubscribed server now also
    /// costs zero per-feed work, which the old tap-presence gate couldn't
    /// express (a running server always had *a* tap, subscribed or not); and
    /// a server with subscribers elsewhere but none for this particular pane
    /// costs zero too.
    pub(super) fn ipc_output_tap(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> crate::io_thread::IpcOutputTap {
        let ipc_pane_id = self.mint_ipc_pane(window_id, pane_id);
        crate::io_thread::IpcOutputTap {
            broadcaster: self.ipc_broadcaster.clone(),
            ipc_pane_id,
        }
    }

    /// Wire a local pane's generation-aware raw attach resources eagerly at
    /// spawn. The returned tap is passed to that pane's io thread.
    pub(super) fn register_ipc_attach_pane(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        terminal: Arc<Mutex<Terminal>>,
        input: crate::io_thread::PtyInputQueue,
    ) -> crate::io_thread::RawAttachTap {
        let raw_output = crate::io_thread::RawAttachTap::default();
        let id = SessionCardId::new(
            crate::session_store::SessionWindowId(u64::from(window_id)),
            pane_id,
        );
        let key = registry_key(id);
        let mut shared = self.ipc_shared.lock();
        shared.registry.mint(key.0, key.1);
        shared.attach_panes.insert(
            key,
            crate::ipc_bridge::IpcAttachPane::new(terminal, raw_output.clone(), input),
        );
        raw_output
    }

    pub(super) fn cleanup_ipc_attach_pane(&self, window_id: WindowId, pane_id: PaneId) {
        let key = (u64::from(window_id), pane_id.get());
        let attach = self.ipc_shared.lock().attach_panes.remove(&key);
        if let Some(attach) = attach {
            attach.shutdown();
        }
    }

    /// The orchestrator IPC pane `pane` was spawned for, when it is a
    /// worker (`parentPaneId`).
    fn ipc_orchestrator_of(&self, pane: u64) -> Option<u64> {
        let (window_id, pane_id) = self.resolve_ipc_pane(pane).ok()?;
        self.windows
            .get(&window_id)?
            .surfaces
            .get(&pane_id)?
            .orchestration
            .as_ref()?
            .parent
    }

    fn mint_ipc_pane(&self, window_id: WindowId, pane_id: PaneId) -> u64 {
        let id = SessionCardId::new(
            crate::session_store::SessionWindowId(u64::from(window_id)),
            pane_id,
        );
        let (window, pane) = registry_key(id);
        self.ipc_shared.lock().registry.mint(window, pane)
    }
}

/// Where to put keyboard focus after an extended spawn, in order, as
/// `(window, pane, raise_tab)`. Every tab whose focused pane the spawn moved
/// (its target, or an `auto` candidate that had no room: a split focuses its
/// target before it knows there is room) gets its own pane back, except the
/// tab holding a new pane the caller wants focused. Then the pane that had
/// focus goes last when the caller passed `focus: false` or the spawn failed;
/// otherwise the new pane does, its tab raised when it is not the front one.
/// `before`/`after` are every window's focused pane around the spawn;
/// `created` is the new pane.
fn focus_to_restore(
    focus: Option<bool>,
    previous: Option<(WindowId, PaneId)>,
    before: &[(WindowId, PaneId)],
    after: &[(WindowId, PaneId)],
    created: Option<(WindowId, PaneId)>,
    now_focused: Option<WindowId>,
) -> Vec<(WindowId, PaneId, bool)> {
    let keep_new = created.filter(|_| focus != Some(false));
    let keep_new_window = keep_new.map(|(window, _)| window);
    let previous_window = previous.map(|(window, _)| window);
    let mut restores: Vec<_> = before
        .iter()
        .filter(|(window, pane)| {
            after
                .iter()
                .any(|(after_window, after_pane)| after_window == window && after_pane != pane)
        })
        .filter(|(window, _)| Some(*window) != keep_new_window)
        // Without a new pane to focus, the previous pane is restored last
        // below; don't restore its window twice.
        .filter(|(window, _)| keep_new.is_some() || Some(*window) != previous_window)
        .map(|(window, pane)| (*window, *pane, false))
        .collect();
    match keep_new {
        Some((window, pane)) => {
            if Some(window) != now_focused {
                restores.push((window, pane, true));
            }
        }
        None => {
            if let Some((window, pane)) = previous {
                restores.push((window, pane, Some(window) != now_focused));
            }
        }
    }
    restores
}

/// The panels of `current` that are new or differ from `previous`, in
/// `current`'s order: the `noa.stateChanged` diff (F-5).
fn changed_panels(previous: Vec<Panel>, current: &[Panel]) -> Vec<Panel> {
    let previous_by_id: HashMap<u64, Panel> = previous
        .into_iter()
        .map(|panel| (panel.pane_id.0, panel))
        .collect();
    current
        .iter()
        .filter(|panel| previous_by_id.get(&panel.pane_id.0) != Some(*panel))
        .cloned()
        .collect()
}

/// What `sync_ipc_snapshot` rebuilds on as soon as it changes: each card's
/// listed facts, and each live pane `((window, pane), exited)`, sorted.
fn structural_signature(
    cards: &[(SessionCardId, &crate::session_store::SessionCard)],
    panes: &[((u64, u64), bool)],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (id, card) in cards {
        id.window_id.0.hash(&mut hasher);
        id.pane_id.get().hash(&mut hasher);
        card.busy.hash(&mut hasher);
        card.attention.hash(&mut hasher);
        card.display_name().hash(&mut hasher);
        card.agent_seq.hash(&mut hasher);
        card.process.hash(&mut hasher);
    }
    // Every pane, not only carded ones: card-less panes are listed too.
    panes.hash(&mut hasher);
    hasher.finish()
}

/// Whether IPC lists a pane (floating-panes REQ-023 as amended): every split
/// pane, and a float only when an orchestrator spawned it as a worker (it has
/// a parent pane), so `+ctl` can wait on, read and close it. AppleScript, AX
/// and the Overview still leave every float out.
fn listed_over_ipc(float: bool, has_parent: bool) -> bool {
    !float || has_parent
}

/// The pane a split created: the one surface that was not there before.
/// Not "whatever has focus now": a split whose target could not take focus
/// (a collapsed or hidden float) leaves it on another pane.
fn split_created(
    before: &HashSet<PaneId>,
    mut after: impl Iterator<Item = PaneId>,
) -> Option<PaneId> {
    after.find(|pane| !before.contains(pane))
}

/// The split pane a new worker float goes over when `target` asked for it.
/// A float can only hang from a pane of the split layout, so a float worker
/// that orchestrates in turn hands its workers to the pane it floats over
/// (the tab's split focus for a tab- or window-anchored float).
fn float_host(
    target: PaneId,
    target_anchor: Option<crate::floating::FloatAnchor>,
    split_focus: PaneId,
) -> PaneId {
    match target_anchor {
        None => target,
        Some(crate::floating::FloatAnchor::Pane(pane)) => pane,
        Some(_) => split_focus,
    }
}

/// agent-orchestration D10: a worker float is stowed when it goes over its
/// orchestrator's board — the pane `+ctl board --open` labelled
/// `board:<orchestrator's IPC id>`. Collapsed, it then shows no strip; the
/// board's row stands in for it. D13: a worker's worker is stowed too, on
/// the board its orchestrator is stowed on: `parent_stowed_under` is the
/// parent's own orchestrator when the parent is a worker stowed on this
/// anchor.
fn stows_on(
    anchor_label: Option<&str>,
    parent: Option<u64>,
    parent_stowed_under: Option<u64>,
) -> bool {
    let (Some(label), Some(parent)) = (anchor_label, parent) else {
        return false;
    };
    [Some(parent), parent_stowed_under]
        .into_iter()
        .flatten()
        .any(|orchestrator| label == format!("board:{orchestrator}"))
}

/// agent-orchestration D10: the orchestrator a pane labelled
/// `board:<orchestrator's IPC id>` is the board of.
fn board_orchestrator(label: Option<&str>) -> Option<u64> {
    label?.strip_prefix("board:")?.parse().ok()
}

/// agent-orchestration D13: nesting goes one level deep. A spawn's `parent`
/// may be a worker (its workers go on its own orchestrator's board), but not
/// a worker's worker. `orchestrator_of` names a pane's own orchestrator.
fn check_nesting(
    parent: u64,
    orchestrator_of: impl Fn(u64) -> Option<u64>,
) -> Result<(), noa_ipc::IpcError> {
    match orchestrator_of(parent) {
        Some(grandparent) if orchestrator_of(grandparent).is_some() => {
            Err(noa_ipc::IpcError::InvalidParams(format!(
                "nesting goes one level deep: pane {parent} is already a worker's worker"
            )))
        }
        _ => Ok(()),
    }
}

/// How a float spawned over IPC starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FloatStart {
    Collapsed,
    OpenFocused,
}

/// `focus` (omitted means true, as for a split) opens the float and moves
/// focus to it; otherwise it stays collapsed to its title strip.
fn float_start(focus: Option<bool>) -> FloatStart {
    if focus.unwrap_or(true) {
        FloatStart::OpenFocused
    } else {
        FloatStart::Collapsed
    }
}

/// One live pane `sync_ipc_snapshot` may list without a session card.
struct ListCandidate {
    key: (u64, u64),
    float: bool,
    /// Spawned by an orchestrator over IPC.
    worker: bool,
    /// Spawned with a parent pane (an orchestrator's worker).
    parent: bool,
    /// In a window that keeps session cards (not the quick terminal or the
    /// scratch popup), so its pane is listed once it has a card anyway.
    eligible: bool,
}

/// Panes to list although they have no session card yet, which only appears
/// with a pane's first output: an orchestrator's workers (a silent `sleep`,
/// or an agent still starting), and any pane of a card-keeping window, such
/// as an orchestrator script that prints nothing before its first `+ctl`.
/// Without them `+ctl wait` reads a just-spawned worker as closed, and `+ctl`
/// cannot find its own pane (agent-orchestration D1). A float is listed only
/// when it is a worker (REQ-023 as amended).
fn uncarded_panes(
    listed: &HashSet<(u64, u64)>,
    candidates: impl Iterator<Item = ListCandidate>,
) -> Vec<(u64, u64)> {
    candidates
        .filter(|candidate| {
            (candidate.worker || candidate.eligible)
                && listed_over_ipc(candidate.float, candidate.parent)
                && !listed.contains(&candidate.key)
        })
        .map(|candidate| candidate.key)
        .collect()
}

/// `Panel` facts that live on the `Surface` (agent-orchestration D1).
fn surface_facts(surface: &Surface) -> PanelSurfaceFacts {
    PanelSurfaceFacts {
        surface_id: surface.surface_id.clone(),
        exit: surface.exited.then_some(surface.exit_status).flatten(),
        parent_pane_id: surface.orchestration.as_ref().and_then(|meta| meta.parent),
        label: surface
            .orchestration
            .as_ref()
            .and_then(|meta| meta.label.clone()),
        cwd: surface
            .orchestration
            .as_ref()
            .and_then(|meta| meta.cwd.clone()),
        spawn: surface.ipc_spawn.as_ref().map(|spawn| noa_ipc::PanelSpawn {
            argv: spawn.argv.clone(),
            started_at: spawn.started_at,
        }),
    }
}

/// REL-01: a paste that asked for paste mode is refused, with nothing
/// written, while the pane has not enabled bracketed paste: its program may
/// still be starting, and every newline of the text would land as Enter.
fn check_paste_mode(
    pane: u64,
    require_paste_mode: bool,
    bracketed: bool,
) -> Result<(), noa_ipc::IpcError> {
    if require_paste_mode && !bracketed {
        return Err(noa_ipc::IpcError::NotReady(format!(
            "pane {pane} has not enabled bracketed paste yet (its program may still be \
             starting); nothing was sent"
        )));
    }
    Ok(())
}

/// REL-02: refuse a spawn once `parent` has `most` children. `children` is
/// every pane's orchestration parent and whether it has exited; a pane held
/// open after its child exited still counts, as the client's own count does.
fn check_max_children(
    parent: u64,
    most: u32,
    children: impl Iterator<Item = (Option<u64>, bool)>,
) -> Result<(), noa_ipc::IpcError> {
    let (mut live, mut exited) = (0u32, 0u32);
    for (_, gone) in children.filter(|(of, _)| *of == Some(parent)) {
        if gone {
            exited += 1;
        } else {
            live += 1;
        }
    }
    if live + exited >= most {
        return Err(noa_ipc::IpcError::LimitReached(format!(
            "pane {parent} already has {} children ({live} live, {exited} exited); \
             maxChildren is {most}",
            live + exited
        )));
    }
    Ok(())
}

/// A live pane and the orchestrator that spawned it, if any.
struct SpawnedPane {
    window: WindowId,
    pane: PaneId,
    rect: PaneRectApp,
    parent: Option<u64>,
    /// A worker float, which is not in the split layout.
    float: bool,
}

/// The panes `anchor_ipc` spawned that an `auto` split may divide, with
/// their windows: in any tab, so a worker that overflowed into a new tab is
/// split before yet another tab opens.
fn anchor_children(
    anchor_ipc: u64,
    surfaces: impl Iterator<Item = SpawnedPane>,
) -> Vec<(WindowId, PaneId, PaneRectApp)> {
    surfaces
        .filter(|surface| surface.parent == Some(anchor_ipc) && !surface.float)
        .map(|surface| (surface.window, surface.pane, surface.rect))
        .collect()
}

/// The order an `auto` split tries (agent-orchestration D2): with no
/// spawned children yet, the anchor splits to its right; otherwise the
/// largest child splits along its longer side (ties go to the lowest pane
/// id), then the other side, then every smaller child the same way. The
/// caller falls back to a new tab when none of them has room.
fn auto_split_order(
    anchor: (WindowId, PaneId, PaneRectApp),
    children: &[(WindowId, PaneId, PaneRectApp)],
) -> Vec<(WindowId, PaneId, Direction)> {
    if children.is_empty() {
        return vec![
            (anchor.0, anchor.1, Direction::Right),
            (anchor.0, anchor.1, Direction::Down),
        ];
    }
    let mut by_size = children.to_vec();
    by_size.sort_by_key(|(_, pane, rect)| {
        (
            std::cmp::Reverse(u64::from(rect.w) * u64::from(rect.h)),
            pane.get(),
        )
    });
    by_size
        .into_iter()
        .flat_map(|(window, pane, rect)| {
            let (long, short) = if rect.w >= rect.h {
                (Direction::Right, Direction::Down)
            } else {
                (Direction::Down, Direction::Right)
            };
            [(window, pane, long), (window, pane, short)]
        })
        .collect()
}

/// The output tap of a pane IPC must not see (floating-panes REQ-023): it
/// pushes to a broadcaster nothing can subscribe to, under no pane id.
pub(super) fn hidden_ipc_output_tap() -> crate::io_thread::IpcOutputTap {
    crate::io_thread::IpcOutputTap {
        broadcaster: noa_ipc::Broadcaster::new(),
        ipc_pane_id: 0,
    }
}

fn stale_registry_keys<'a>(
    registered: impl Iterator<Item = &'a (u64, u64)>,
    live: &HashSet<(u64, u64)>,
) -> Vec<(u64, u64)> {
    registered
        .filter(|key| !live.contains(key))
        .copied()
        .collect()
}

fn apply_attach_grid_first_resize(
    terminal: &Arc<Mutex<Terminal>>,
    current_size: &mut GridSize,
    new_size: GridSize,
    dispatch_pty_resize: impl FnOnce(GridSize) -> Result<(), noa_ipc::IpcError>,
) -> Result<(), noa_ipc::IpcError> {
    if *current_size == new_size {
        return Ok(());
    }
    *current_size = new_size;
    terminal.lock().resize(new_size);
    dispatch_pty_resize(new_size)
}

/// Map a pty input queue outcome onto the `noa.sendText` reply. `Queued` and
/// `Deferred` are both acceptance (the queue owns the bytes and will write
/// them); `Dropped` means the pane's input budget is exhausted because the
/// foreground program isn't reading its tty, and `Disconnected` that the
/// pane has no live sink (writer thread or remote transport gone). Neither
/// of the last two may be reported as success. "Accepted by the queue" is
/// still not "consumed by the child" — that guarantee doesn't exist.
fn ipc_input_queue_result(
    result: crate::io_thread::QueueInputResult,
) -> Result<(), noa_ipc::IpcError> {
    use crate::io_thread::QueueInputResult;
    match result {
        QueueInputResult::Queued | QueueInputResult::Deferred => Ok(()),
        QueueInputResult::Dropped => Err(noa_ipc::IpcError::Internal(
            "pty input queue is full: the foreground program is not reading its tty".to_string(),
        )),
        QueueInputResult::Disconnected => Err(noa_ipc::IpcError::PaneClosed),
    }
}

#[cfg(test)]
mod attach_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn attach_resize_dispatch_observes_grid_already_resized() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut current_size = GridSize::new(80, 24);
        let dispatched = AtomicBool::new(false);

        apply_attach_grid_first_resize(
            &terminal,
            &mut current_size,
            GridSize::new(120, 40),
            |size| {
                assert_eq!(terminal.lock().size, size, "grid must resize first");
                dispatched.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();

        assert!(dispatched.load(Ordering::SeqCst));
        assert_eq!(current_size, GridSize::new(120, 40));
    }

    #[test]
    fn attach_cleanup_retains_live_surface_without_session_card() {
        let silent_surface = (7, 1);
        let closed_surface = (8, 2);
        let registered = [silent_surface, closed_surface];
        let live = HashSet::from([silent_surface]);

        assert_eq!(
            stale_registry_keys(registered.iter(), &live),
            vec![closed_surface]
        );
    }
}

#[cfg(test)]
mod focus_restore_tests {
    use super::*;

    fn w(id: u64) -> WindowId {
        WindowId::from(id)
    }

    fn p(id: u64) -> PaneId {
        PaneId::new(id)
    }

    fn previous() -> Option<(WindowId, PaneId)> {
        Some((w(1), p(7)))
    }

    /// One window whose focused pane stayed put.
    fn unchanged() -> Vec<(WindowId, PaneId)> {
        vec![(w(1), p(7))]
    }

    // Review finding: a split or tab spawn that failed had already moved
    // focus to its target, and returned before focus went back.
    #[test]
    fn a_failed_spawn_puts_focus_back() {
        assert_eq!(
            focus_to_restore(
                Some(false),
                previous(),
                &unchanged(),
                &unchanged(),
                None,
                Some(w(2))
            ),
            vec![(w(1), p(7), true)]
        );
        // With no new pane to focus, even a default-focus spawn gives the
        // focus back.
        assert_eq!(
            focus_to_restore(
                None,
                previous(),
                &unchanged(),
                &unchanged(),
                None,
                Some(w(1))
            ),
            vec![(w(1), p(7), false)]
        );
    }

    #[test]
    fn a_successful_spawn_keeps_focus_unless_asked_not_to() {
        let created = Some((w(1), p(9)));
        for focus in [None, Some(true)] {
            assert!(
                focus_to_restore(
                    focus,
                    previous(),
                    &unchanged(),
                    &unchanged(),
                    created,
                    Some(w(1))
                )
                .is_empty()
            );
        }
        assert_eq!(
            focus_to_restore(
                Some(false),
                previous(),
                &unchanged(),
                &unchanged(),
                created,
                Some(w(1))
            ),
            vec![(w(1), p(7), false)]
        );
    }

    // Review finding: an `auto` candidate in the focused tab had no room, so
    // that tab's focus stayed on the candidate while the split succeeded in
    // a background tab; the focused tab was skipped as "previous".
    #[test]
    fn the_focused_tab_gets_its_pane_back_when_the_split_lands_elsewhere() {
        let previous = Some((w(1), p(7)));
        let before = vec![(w(1), p(7)), (w(2), p(20))];
        let after = vec![(w(1), p(11)), (w(2), p(21))];

        let restores = focus_to_restore(
            None,
            previous,
            &before,
            &after,
            Some((w(2), p(21))),
            Some(w(1)),
        );

        assert_eq!(restores[0], (w(1), p(7), false));
    }

    // Review finding: `--focus` split a worker in a background tab, but
    // nothing brought that tab forward, so the new pane never got the
    // keyboard.
    #[test]
    fn a_focused_spawn_in_a_background_tab_brings_that_tab_forward() {
        let previous = Some((w(1), p(7)));
        let before = vec![(w(1), p(7)), (w(2), p(20))];
        let after = vec![(w(1), p(7)), (w(2), p(21))];

        assert_eq!(
            focus_to_restore(
                Some(true),
                previous,
                &before,
                &after,
                Some((w(2), p(21))),
                Some(w(1))
            ),
            vec![(w(2), p(21), true)]
        );
    }

    // Review finding: spawning into the orchestrator's tab while the human
    // works in another left the orchestrator's tab focused on the new worker,
    // so the human's next keystrokes there went to the worker.
    #[test]
    fn the_spawn_window_gets_its_own_focused_pane_back() {
        let human = Some((w(2), p(20)));
        let before = vec![(w(1), p(7)), (w(2), p(20))];
        let after = vec![(w(1), p(9)), (w(2), p(20))];

        assert_eq!(
            focus_to_restore(
                Some(false),
                human,
                &before,
                &after,
                Some((w(1), p(9))),
                Some(w(2))
            ),
            vec![(w(1), p(7), false), (w(2), p(20), false)],
            "the orchestrator tab first, then the human's pane last"
        );
        // A default-focus spawn keeps the new pane focused in its own tab,
        // but a failed auto candidate elsewhere still goes back.
        let after_auto = vec![(w(1), p(9)), (w(3), p(31))];
        let before_auto = vec![(w(1), p(7)), (w(3), p(30))];
        assert_eq!(
            focus_to_restore(
                None,
                human,
                &before_auto,
                &after_auto,
                Some((w(1), p(9))),
                Some(w(1))
            ),
            vec![(w(3), p(30), false)]
        );
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    pub(super) fn panel(pane: u64, state: &str, preview: &[&str]) -> Panel {
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "windowGroupId": "1", "windowId": "1", "paneId": pane.to_string(),
            "name": format!("claude · w{pane}"), "cwd": format!("/repo/wt/w{pane}"),
            "busy": true, "attention": false,
            "preview": preview.iter().enumerate().map(|(row, text)| serde_json::json!({
                "row": row, "spans": [{ "text": text, "fg": "#5fc4a0" }, { "text": " (12s)" }],
            })).collect::<Vec<_>>(),
            "parentPaneId": "1", "label": format!("w{pane}"),
            "agent": { "kind": "claude", "state": state, "detail": "running tests", "seq": 3 },
        }))
        .unwrap();
        panel.surface_id = Some(format!("0x{pane:016x}"));
        panel
    }

    fn ids(panels: &[Panel]) -> Vec<u64> {
        panels.iter().map(|panel| panel.pane_id.0).collect()
    }

    #[test]
    fn the_diff_holds_new_and_changed_panels_in_listing_order() {
        let previous = vec![
            panel(2, "running", &["a"]),
            panel(3, "running", &["b"]),
            panel(4, "finished", &[]),
            panel(9, "running", &[]),
        ];
        let current = vec![
            panel(7, "running", &[]),
            panel(4, "finished", &[]),
            panel(3, "running", &["b", "c"]),
            panel(2, "running", &["a"]),
            panel(5, "permission", &[]),
        ];
        let changed = changed_panels(previous.clone(), &current);
        assert_eq!(ids(&changed), [7, 3, 5]);
        assert_eq!(changed[1], current[2]);
        assert!(changed_panels(current.clone(), &current).is_empty());
        assert_eq!(ids(&changed_panels(Vec::new(), &current)), ids(&current));
        assert!(changed_panels(previous, &[]).is_empty());
        // A pane listed twice is compared against the last of its ids.
        let twice = vec![panel(2, "running", &["a"]), panel(2, "finished", &[])];
        assert_eq!(
            ids(&changed_panels(twice, &[panel(2, "running", &["a"])])),
            [2]
        );
    }
}

#[cfg(test)]
mod diff_perf {
    use super::diff_tests::panel;
    use super::*;
    use std::time::Instant;

    /// The snapshot sync's publish step with twenty listed workers and
    /// nothing changed (the idle case), as it was — a full copy for the
    /// shared listing, then the diff — and as it is, with and without a
    /// `stateChanged` subscriber. Run with
    /// `cargo test --release -p noa-app diff_perf -- --ignored --nocapture`.
    #[test]
    #[ignore = "release-mode measurement of the snapshot publish step"]
    fn publish_cost_with_twenty_panels() {
        let iters = 20_000u32;
        let listing = || -> Vec<Panel> {
            (2..22)
                .map(|pane| {
                    panel(
                        pane,
                        "running",
                        &["● Running the test suite", "  cargo test"],
                    )
                })
                .collect()
        };
        let shared = parking_lot::Mutex::new(listing());
        let idle = noa_ipc::Broadcaster::new();
        let watched = noa_ipc::Broadcaster::new();
        let (conn, _queue) = watched.register_connection();
        watched
            .add_subscription(conn, noa_ipc::EventMask::STATE_CHANGED, None)
            .unwrap();
        let run = |name: &str, publish: &mut dyn FnMut(Vec<Panel>) -> usize| {
            let batches: Vec<Vec<Panel>> = (0..iters).map(|_| listing()).collect();
            let at = Instant::now();
            let mut sent = 0;
            for panels in batches {
                sent += publish(panels);
            }
            let per = at.elapsed().as_secs_f64() * 1e6 / f64::from(iters);
            println!("{name:<28} {per:>7.2}µs per sync, {sent} panels sent");
        };
        run("copy then diff (before)", &mut |panels| {
            let previous = std::mem::replace(&mut *shared.lock(), panels.clone());
            changed_panels(previous, &panels).len()
        });
        run("no subscriber", &mut |panels| {
            let _previous = std::mem::replace(&mut *shared.lock(), panels);
            if !idle.has_state_changed_subscribers() {
                return 0;
            }
            unreachable!()
        });
        run("a subscriber", &mut |panels| {
            let previous = std::mem::replace(&mut *shared.lock(), panels);
            if !watched.has_state_changed_subscribers() {
                return 0;
            }
            changed_panels(previous, &shared.lock()).len()
        });
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    // Review finding: a card-less pane (a silent worker, a fresh split) is
    // listed, but appearing or closing did not change the signature, so the
    // listing lagged until the next timed refresh.
    #[test]
    fn a_card_less_pane_coming_or_going_changes_the_signature() {
        let worker = ((1, 2), false);
        let with = structural_signature(&[], &[((1, 1), false), worker]);
        let without = structural_signature(&[], &[((1, 1), false)]);

        assert_ne!(with, without);
    }

    #[test]
    fn a_pane_exiting_changes_the_signature() {
        let running = structural_signature(&[], &[((1, 2), false)]);
        let exited = structural_signature(&[], &[((1, 2), true)]);

        assert_ne!(running, exited);
    }
}

#[cfg(test)]
mod unlisted_worker_tests {
    use super::*;

    /// A candidate in a window that keeps no session cards (quick terminal).
    fn candidate(key: (u64, u64), float: bool, worker: bool) -> ListCandidate {
        ListCandidate {
            key,
            float,
            worker,
            parent: worker,
            eligible: false,
        }
    }

    // Review finding: an orchestrator started with `Noa -e script` printed
    // nothing before its first `+ctl spawn`, so its pane had no card, was not
    // listed, and `+ctl` could not find itself by NOA_SURFACE_ID.
    #[test]
    fn a_silent_ordinary_pane_is_listed_before_its_first_output() {
        let listed = HashSet::new();
        let ordinary = ListCandidate {
            key: (1, 1),
            float: false,
            worker: false,
            parent: false,
            eligible: true,
        };

        assert_eq!(
            uncarded_panes(&listed, [ordinary].into_iter()),
            vec![(1, 1)]
        );
    }

    #[test]
    fn a_silent_worker_is_listed_before_its_first_output() {
        let listed = HashSet::from([(1, 1)]);
        let candidates = [
            candidate((1, 1), false, false),
            candidate((1, 2), false, true),
            candidate((1, 3), false, false),
            candidate((1, 4), true, true),
            candidate((1, 5), true, false),
        ];

        // A worker float is listed (floating-panes REQ-023 as amended); any
        // other float never is.
        assert_eq!(
            uncarded_panes(&listed, candidates.into_iter()),
            vec![(1, 2), (1, 4)],
            "only card-less workers are added, a worker float included"
        );
    }

    // Review finding: a float with only a label counted as a worker, so it
    // was listed though it has no orchestrator.
    #[test]
    fn a_float_needs_a_parent_to_be_listed() {
        let listed = HashSet::new();
        let labelled_float = ListCandidate {
            key: (1, 6),
            float: true,
            worker: true,
            parent: false,
            eligible: true,
        };
        assert!(uncarded_panes(&listed, [labelled_float].into_iter()).is_empty());
    }

    #[test]
    fn only_a_float_that_is_a_worker_is_listed_over_ipc() {
        assert!(listed_over_ipc(false, false));
        assert!(listed_over_ipc(false, true));
        assert!(listed_over_ipc(true, true));
        assert!(!listed_over_ipc(true, false));
    }

    #[test]
    fn a_worker_with_a_card_is_not_listed_twice() {
        let listed = HashSet::from([(1, 2)]);

        assert!(uncarded_panes(&listed, [candidate((1, 2), false, true)].into_iter()).is_empty());
    }
}

#[cfg(test)]
mod auto_split_tests {
    use super::*;

    // Review finding, reproduced in a real Noa: a float worker running
    // `+ctl spawn` asked for a float over itself, which no float can hang
    // from, and was told its pane held the maximum of 8.
    #[test]
    fn a_float_workers_own_worker_floats_over_the_pane_below() {
        use crate::floating::FloatAnchor;
        let (split, float, focus) = (PaneId::new(1), PaneId::new(4), PaneId::new(2));
        assert_eq!(float_host(split, None, focus), split);
        assert_eq!(
            float_host(float, Some(FloatAnchor::Pane(split)), focus),
            split
        );
        assert_eq!(float_host(float, Some(FloatAnchor::Tab), focus), focus);
        assert_eq!(float_host(float, Some(FloatAnchor::Window), focus), focus);
    }

    // Review finding, reproduced in a real Noa: a collapsed float worker ran
    // `+ctl spawn --split right` in a full window; focus stayed on the
    // orchestrator's pane, nothing was split, and the orchestrator's own pane
    // came back as the new worker (and got the float as its parent).
    #[test]
    fn a_split_that_created_nothing_returns_no_pane() {
        let (orchestrator, worker, float) = (PaneId::new(1), PaneId::new(2), PaneId::new(4));
        let before = HashSet::from([orchestrator, worker, float]);
        assert_eq!(split_created(&before, before.clone().into_iter()), None);
        let new = PaneId::new(5);
        assert_eq!(
            split_created(&before, [orchestrator, worker, float, new].into_iter()),
            Some(new)
        );
    }

    #[test]
    fn a_worker_is_stowed_only_over_its_own_orchestrators_board() {
        assert!(stows_on(Some("board:7"), Some(7), None));
        // Another orchestrator's board, an ordinary label, no label, no parent.
        assert!(!stows_on(Some("board:8"), Some(7), None));
        assert!(!stows_on(Some("fix-tests"), Some(7), None));
        assert!(!stows_on(None, Some(7), None));
        assert!(!stows_on(Some("board:7"), None, None));
    }

    // agent-orchestration D13: a worker (12) of a worker stowed on root 7's
    // board goes on that board too, stowed, instead of floating over it
    // with a strip.
    #[test]
    fn a_workers_worker_is_stowed_on_the_board_its_orchestrator_is_on() {
        assert!(stows_on(Some("board:7"), Some(12), Some(7)));
        // Its parent stowed on another root's board, or not stowed at all.
        assert!(!stows_on(Some("board:7"), Some(12), Some(8)));
        assert!(!stows_on(Some("board:7"), Some(12), None));
        assert!(!stows_on(None, Some(12), Some(7)));
    }

    #[test]
    fn a_board_label_names_its_orchestrator() {
        assert_eq!(board_orchestrator(Some("board:7")), Some(7));
        assert_eq!(board_orchestrator(Some("board:")), None);
        assert_eq!(board_orchestrator(Some("board:x")), None);
        assert_eq!(board_orchestrator(Some("fix-tests")), None);
        assert_eq!(board_orchestrator(None), None);
    }

    // agent-orchestration D13: a root (1) may have workers (2), and they
    // workers of their own (3), but those may not spawn.
    #[test]
    fn nesting_goes_one_level_deep() {
        let orchestrator_of = |pane: u64| match pane {
            2 => Some(1),
            3 => Some(2),
            _ => None,
        };
        assert!(check_nesting(1, orchestrator_of).is_ok());
        assert!(check_nesting(2, orchestrator_of).is_ok());
        let error = check_nesting(3, orchestrator_of).unwrap_err();
        assert_eq!(error.code(), noa_ipc::ErrorCode::InvalidParams);
        assert_eq!(
            error.to_string(),
            "nesting goes one level deep: pane 3 is already a worker's worker"
        );
    }

    // Review finding: `focus: true` (and `+ctl spawn --float --focus`) left
    // the new float collapsed and unfocused.
    #[test]
    fn a_float_spawned_with_focus_opens_and_takes_it() {
        assert_eq!(float_start(Some(true)), FloatStart::OpenFocused);
        assert_eq!(float_start(None), FloatStart::OpenFocused);
        assert_eq!(float_start(Some(false)), FloatStart::Collapsed);
    }

    // A worker float is not in the split layout, so an `auto` split never
    // tries to divide it.
    #[test]
    fn a_worker_float_is_not_split_by_auto_placement() {
        let tab = WindowId::from(1);
        let pane = |pane: u64, float: bool| SpawnedPane {
            window: tab,
            pane: PaneId::new(pane),
            rect: rect(400, 300),
            parent: Some(10),
            float,
        };
        let children = anchor_children(10, [pane(2, false), pane(3, true)].into_iter());
        assert_eq!(children, vec![(tab, PaneId::new(2), rect(400, 300))]);
    }

    fn rect(w: u32, h: u32) -> PaneRectApp {
        PaneRectApp { x: 0, y: 0, w, h }
    }

    fn tab(id: u64) -> WindowId {
        WindowId::from(id)
    }

    #[test]
    fn the_first_worker_splits_the_anchor_to_the_right() {
        let anchor = (tab(1), PaneId::new(1), rect(1600, 900));

        assert_eq!(
            auto_split_order(anchor, &[])[0],
            (tab(1), PaneId::new(1), Direction::Right)
        );
    }

    #[test]
    fn later_workers_split_the_largest_child_along_its_longer_side() {
        let anchor = (tab(1), PaneId::new(1), rect(800, 900));
        let children = [
            (tab(1), PaneId::new(5), rect(800, 450)),
            (tab(1), PaneId::new(4), rect(800, 900)),
        ];

        let order = auto_split_order(anchor, &children);

        // Pane 4 is largest and taller than wide, so it splits downward
        // first; the anchor itself is never split once it has children.
        assert_eq!(order[0], (tab(1), PaneId::new(4), Direction::Down));
        assert_eq!(order[1], (tab(1), PaneId::new(4), Direction::Right));
        assert_eq!(order[2], (tab(1), PaneId::new(5), Direction::Right));
        assert!(order.iter().all(|(_, pane, _)| *pane != PaneId::new(1)));
    }

    #[test]
    fn equal_sized_children_are_ordered_by_pane_id() {
        let anchor = (tab(1), PaneId::new(1), rect(800, 900));
        let children = [
            (tab(1), PaneId::new(9), rect(400, 400)),
            (tab(1), PaneId::new(3), rect(400, 400)),
        ];

        assert_eq!(auto_split_order(anchor, &children)[0].1, PaneId::new(3));
    }

    // Review finding: once the anchor's tab was full and a worker overflowed
    // into a new tab, that worker was never a candidate, so every later
    // spawn opened yet another tab instead of splitting it.
    #[test]
    fn a_worker_that_overflowed_into_another_tab_is_still_a_candidate() {
        let spawned = |window: u64, pane: u64, w: u32, h: u32, parent: Option<u64>| SpawnedPane {
            window: tab(window),
            pane: PaneId::new(pane),
            rect: rect(w, h),
            parent,
            float: false,
        };
        let surfaces = [
            spawned(1, 1, 200, 900, None),
            spawned(1, 2, 200, 900, Some(10)),
            spawned(2, 3, 1600, 900, Some(10)),
            spawned(2, 4, 1600, 900, Some(99)),
        ];

        let children = anchor_children(10, surfaces.into_iter());
        let order = auto_split_order((tab(1), PaneId::new(1), rect(200, 900)), &children);

        assert_eq!(
            children.len(),
            2,
            "both of the anchor's workers, in either tab"
        );
        assert_eq!(order[0], (tab(2), PaneId::new(3), Direction::Right));
    }
}

#[cfg(test)]
mod send_text_reply_tests {
    use super::*;
    use crate::io_thread::QueueInputResult;

    // N02: a rejected or sink-less queue must not turn into `Ok` for the
    // automation client; only actual acceptance does.
    #[test]
    fn send_text_reports_queue_rejection_instead_of_success() {
        assert!(ipc_input_queue_result(QueueInputResult::Queued).is_ok());
        assert!(ipc_input_queue_result(QueueInputResult::Deferred).is_ok());
        assert!(matches!(
            ipc_input_queue_result(QueueInputResult::Dropped),
            Err(noa_ipc::IpcError::Internal(message)) if message.contains("queue is full")
        ));
        assert!(matches!(
            ipc_input_queue_result(QueueInputResult::Disconnected),
            Err(noa_ipc::IpcError::PaneClosed)
        ));
    }
}

#[cfg(test)]
mod orchestration_limit_tests {
    use super::*;

    // REL-01: before the pane's program enables bracketed paste, a send that
    // asked for paste mode is refused as not ready; one that did not ask, or
    // a pane already in paste mode, goes through.
    #[test]
    fn a_paste_requiring_paste_mode_waits_for_bracketed_paste() {
        let refused = check_paste_mode(4, true, false).unwrap_err();
        assert!(matches!(refused, noa_ipc::IpcError::NotReady(_)));
        assert_eq!(refused.code().code(), -32010);
        assert!(refused.to_string().contains("pane 4"));

        assert!(check_paste_mode(4, true, true).is_ok());
        assert!(check_paste_mode(4, false, false).is_ok());
    }

    // REL-02: the parent's children are counted on the app's side, exited
    // panes held open included, and only the parent's own.
    #[test]
    fn max_children_counts_the_parents_live_and_exited_panes() {
        let panes = [
            (Some(3), false),
            (Some(3), true),
            (Some(9), false),
            (None, false),
        ];

        assert!(check_max_children(3, 3, panes.into_iter()).is_ok());
        let refused = check_max_children(3, 2, panes.into_iter()).unwrap_err();
        assert!(matches!(refused, noa_ipc::IpcError::LimitReached(_)));
        assert_eq!(refused.code().code(), -32011);
        assert!(
            refused.to_string().contains("1 live, 1 exited"),
            "{refused}"
        );
        assert!(check_max_children(9, 1, panes.into_iter()).is_err());
        assert!(check_max_children(7, 0, panes.into_iter()).is_err());
    }
}
