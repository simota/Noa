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
            return;
        }

        let Some(token_path) = noa_config::server_token_path() else {
            log::warn!("noa-ipc: cannot resolve token path, server not started");
            return;
        };
        let token = match noa_ipc::load_or_create_token(
            &token_path,
            self.config.server_token.as_deref(),
        ) {
            Ok(token) => token,
            Err(err) => {
                log::warn!("noa-ipc: failed to load/create token: {err}");
                return;
            }
        };
        let allowed_scopes = noa_ipc::ScopeSet::parse_list(&self.config.server_scopes);
        let backend = AppIpcBackend {
            shared: self.ipc_shared.clone(),
            proxy: self.proxy.clone(),
            pending: self.ipc_pending.clone(),
            next_request: self.ipc_next_request.clone(),
        };
        let config = noa_ipc::ServerConfig {
            port: self.config.server_port,
            token,
            allowed_scopes,
        };
        match noa_ipc::Server::start(config, std::sync::Arc::new(backend)) {
            Ok(handle) => {
                log::info!("noa-ipc: listening on 127.0.0.1:{}", handle.port());
                self.ipc_server = Some(handle);
            }
            Err(err) => {
                log::warn!("noa-ipc: failed to bind 127.0.0.1:{}: {err}", self.config.server_port);
            }
        }
    }

    /// Tears down the running `noa-ipc` server (if any) and re-runs
    /// `install_ipc_server_if_needed` against the just-reloaded config
    /// (G-2: config reload's `server-enable`/`server-port`/`server-token`/
    /// `server-scopes` keys otherwise have no live effect). Dropping
    /// `ServerHandle` stops its accept loop and joins it, and every
    /// in-flight connection thread self-terminates within its own ~50ms
    /// read-timeout poll of the shared shutdown flag (`noa_ipc::server`).
    ///
    /// Panes spawned before this restart hold an `IpcOutputTap` built from
    /// the *old* `Broadcaster` (io thread wiring, `ipc_output_tap` below).
    /// That broadcaster keeps working as a value — its registry just has no
    /// connections left once they all disconnect, so its `try_send`s become
    /// permanent no-ops — but it can never reach the *new* server's
    /// connections. Those panes' output silently stops reaching `noa.output`
    /// subscribers until they're respawned; freshly spawned panes tap the
    /// new server correctly. Swapping the old taps live isn't done here: it
    /// would need every io thread to observe a hot-swappable broadcaster
    /// handle, which is more machinery than a config-driven server restart
    /// (an infrequent, deliberate action) justifies for v1.
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
        let Some(handle) = self.ipc_server.as_ref() else {
            return;
        };

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
        let mut live_keys = HashSet::new();
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
                live_keys.insert(key);
                panels.push(card_to_panel(ipc_id, state.group.0, key.0, card));
                if let Some(surface) = state.surfaces.get(&id.pane_id) {
                    terminals.insert(key, surface.terminal.clone());
                }
            }
            let previous = std::mem::replace(&mut shared.panels, panels.clone());
            shared.terminals = terminals;
            // Closed panes never reappear under the same `(window_id,
            // pane_id)` key (`WindowId`/`PaneId` are never reused), so
            // pruning anything absent from this tick's live set is safe.
            // A pane minted this same tick via `ipc_output_tap` (eager
            // mint at spawn, before it lands in `session_store`) is always
            // in `live_keys` too: spawn wiring inserts into
            // `session_store`/`self.windows` synchronously before the
            // event loop yields to the next `about_to_wait`.
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
            let broadcaster = handle.broadcaster();
            broadcaster.broadcast_state_changed(changed);
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
                    .map_err(|err| noa_ipc::IpcError::Internal(err.to_string()))?;
                let pane_id = self
                    .windows
                    .get(&new_window_id)
                    .map(|state| state.focused_pane)
                    .ok_or_else(|| noa_ipc::IpcError::Internal("new tab has no pane".to_string()))?;
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
                    .ok_or_else(|| noa_ipc::IpcError::Internal("split did not create a pane".to_string()))?;
                let ipc_id = self.mint_ipc_pane(window_id, new_pane);
                Ok(IpcActionReply::NewPane(ipc_id))
            }
            IpcActionKind::ClosePane { pane } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                self.request_close_pane(event_loop, window_id, pane_id);
                Ok(IpcActionReply::Ok)
            }
            IpcActionKind::SendText { pane, text } => {
                let (window_id, pane_id) = self.resolve_ipc_pane(pane)?;
                let bracketed = self.bracketed_paste(window_id, pane_id);
                if let Some(bytes) = input::applescript_input_bytes(&text, bracketed) {
                    self.mark_pane_paste_input(window_id, pane_id);
                    self.snap_pane_viewport_to_bottom(window_id, pane_id);
                    self.write_pane_pty_bytes(window_id, pane_id, bytes);
                }
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

    /// The IPC output-push handle a newly spawned pane's io thread should
    /// carry (FR-17), or `None` when the server isn't running — the io
    /// thread then does no extra work per feed (spec "Zero overhead when
    /// disabled"). Mints the pane's IPC id eagerly (rather than waiting for
    /// the next `sync_ipc_snapshot` tick) so output can push from the pane's
    /// very first byte.
    pub(super) fn ipc_output_tap(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> Option<crate::io_thread::IpcOutputTap> {
        let handle = self.ipc_server.as_ref()?;
        let ipc_pane_id = self.mint_ipc_pane(window_id, pane_id);
        Some(crate::io_thread::IpcOutputTap { broadcaster: handle.broadcaster(), ipc_pane_id })
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
