//! `noa-ipc` server lifecycle + the two integration seams the spec calls for
//! (`docs/specs/noa-server.md` §L2 "クレート配置 & 統合点"): a main-thread
//! read snapshot rebuilt in `about_to_wait` (mirrors
//! `App::sync_applescript_snapshot`) and a mutation path through
//! `UserEvent::IpcAction` (DEC-C).

use std::hash::{Hash, Hasher};

use super::*;
use crate::ipc_bridge::{
    AppIpcBackend, IpcActionKind, IpcActionReply, card_to_panel, registry_key,
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
                if !state.contains_pane(id.pane_id) {
                    continue;
                }
                let key = registry_key(id);
                let ipc_id = shared.registry.mint(key.0, key.1);
                let attachable = shared.attach_panes.contains_key(&key);
                live_keys.insert(key);
                panels.push(card_to_panel(
                    ipc_id,
                    state.group.0,
                    key.0,
                    card,
                    attachable,
                ));
                if let Some(surface) = state.surfaces.get(&id.pane_id) {
                    terminals.insert(key, surface.terminal.clone());
                }
            }
            let previous = std::mem::replace(&mut shared.panels, panels.clone());
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
        let previous_by_id: HashMap<u64, Panel> = previous_panels
            .into_iter()
            .map(|panel| (panel.pane_id.0, panel))
            .collect();
        let changed: Vec<Panel> = panels
            .iter()
            .filter(|panel| previous_by_id.get(&panel.pane_id.0) != Some(*panel))
            .cloned()
            .collect();
        if !changed.is_empty() {
            self.ipc_broadcaster.broadcast_state_changed(changed);
        }
    }

    /// A lock-free signature over the IPC snapshot's cheap, frequently
    /// changing fields (pane set, busy, attention, display name) — cwd,
    /// branch, process, and preview are left to the coarser
    /// `IPC_SNAPSHOT_REFRESH` tick since they either change rarely or are
    /// too expensive to read every wake (preview specifically is never
    /// cloned here). Mirrors `applescript_structural_signature`.
    fn ipc_structural_signature(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (id, card) in self.session_store.ordered_cards() {
            id.window_id.0.hash(&mut hasher);
            id.pane_id.get().hash(&mut hasher);
            card.busy.hash(&mut hasher);
            card.attention.hash(&mut hasher);
            card.display_name().hash(&mut hasher);
        }
        hasher.finish()
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
                self.focused = Some(window_id);
                self.focus_pane(window_id, pane_id);
                if let Some(window) = self.windows.get(&window_id).map(|s| s.window.clone()) {
                    window.focus_window();
                }
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
                Ok(IpcActionReply::Ok)
            }
            IpcActionKind::SendText { pane, text, paste } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                let bytes = if paste {
                    let bracketed = self.bracketed_paste(window_id, pane_id);
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
                    self.write_pane_pty_bytes(window_id, pane_id, bytes);
                }
                Ok(IpcActionReply::Ok)
            }
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
                Ok(IpcActionReply::Ok)
            }
        }
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
    fn resolve_ipc_pane(&self, pane: u64) -> Result<(WindowId, PaneId), noa_ipc::IpcError> {
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

    fn mint_ipc_pane(&self, window_id: WindowId, pane_id: PaneId) -> u64 {
        let id = SessionCardId::new(
            crate::session_store::SessionWindowId(u64::from(window_id)),
            pane_id,
        );
        let (window, pane) = registry_key(id);
        self.ipc_shared.lock().registry.mint(window, pane)
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
