//! Session-sidebar subsystem — the `App`-side glue that turns the pure
//! [`crate::session_store`] + [`crate::sidebar`] modules into a live feature:
//! applying io-thread deltas, garbage-collecting torn-down sessions,
//! per-window toggle + grid-first resize, click routing, and the draw path.
//!
//! Everything visual/windowing lives here (not in the two pure modules), so
//! `session_store.rs`/`sidebar.rs` stay GUI-agnostic (NFR-6). The draw path
//! reads only the store and the pure layout — it never locks a `Terminal`
//! (NFR-1/AC-17).

use super::*;

impl App {
    /// The GUI-agnostic card key for a window/pane (NFR-6): winit's stable
    /// `WindowId` ↔ `u64` mapping is the single conversion point, matching what
    /// the io thread posts.
    pub(super) fn session_card_id(window_id: WindowId, pane_id: PaneId) -> SessionCardId {
        SessionCardId::new(SessionWindowId(u64::from(window_id)), pane_id)
    }

    /// Apply one io-thread [`SessionDelta`] to the store (FR-1) and repaint any
    /// window whose sidebar is showing, so a card's cwd/preview/bell refresh is
    /// visible. The main thread owns the store, so this is the only apply site.
    pub(super) fn apply_session_delta(&mut self, delta: crate::session_store::SessionDelta) {
        self.session_store.apply(delta);
        self.request_sidebar_redraw();
    }

    /// Request a redraw of every window currently showing its sidebar. Cheap:
    /// the sidebar is off by default and rarely on more than one window.
    pub(super) fn request_sidebar_redraw(&self) {
        for state in self.windows.values() {
            if state.sidebar_visible {
                state.window.request_redraw();
            }
        }
    }

    /// Every live session-card id across all sidebar-eligible windows
    /// (quick-terminal excluded — FR-14). The GC choke point feeds this to
    /// [`SessionStore::reconcile_sessions`].
    pub(super) fn live_session_card_ids(&self) -> Vec<SessionCardId> {
        let mut ids = Vec::new();
        for (window_id, state) in &self.windows {
            if self.is_quick_terminal_window(*window_id) {
                continue;
            }
            for pane_id in state.surfaces.keys() {
                ids.push(Self::session_card_id(*window_id, *pane_id));
            }
        }
        ids
    }

    /// Drop every store entry whose session no longer exists (FR-12). Funnelled
    /// through by all five teardown sites (close_tab / close_pane /
    /// close_pane_after_pty_exit / window remove / quit) so the store cannot
    /// outlive the panes it mirrors (Omen T7); `close_pane_after_pty_exit` and
    /// window-remove reach it transitively via `close_pane`/`close_tab`.
    pub(super) fn reconcile_session_store(&mut self) {
        let live = self.live_session_card_ids();
        self.session_store.reconcile_sessions(&live);
    }

    /// Clear the unread-bell flag on every card of a just-focused window
    /// (FR-11). Called from the `Focused(true)` handler.
    pub(super) fn clear_session_bell_for_window(&mut self, window_id: WindowId) {
        self.session_store
            .clear_bell_for_window(SessionWindowId(u64::from(window_id)));
        self.request_sidebar_redraw();
    }

    /// Whether a window may host a sidebar (FR-14): everything but the
    /// quick-terminal window.
    pub(super) fn window_sidebar_eligible(&self, window_id: WindowId) -> bool {
        crate::sidebar::is_sidebar_eligible(self.is_quick_terminal_window(window_id))
    }

    /// The sidebar's pixel inset for a window's pane area (FR-4/FR-14): the
    /// configured points times this window's scale factor when the sidebar is
    /// both visible and the window eligible, else 0. Recomputed from the live
    /// scale factor so a DPR change is picked up (Omen T8). The exclusion rule
    /// itself lives in the pure `sidebar::sidebar_inset` (AC-16a).
    pub(super) fn window_sidebar_inset_px(&self, window_id: WindowId) -> u32 {
        let Some(state) = self.windows.get(&window_id) else {
            return 0;
        };
        let scale = state.window.scale_factor() as f32;
        let inset = crate::sidebar::sidebar_inset(
            state.sidebar_visible,
            self.window_sidebar_eligible(window_id),
            self.config.sidebar_width * scale,
        );
        inset.round().max(0.0) as u32
    }

    /// Recompute the app-wide io-thread gate: on while any eligible window
    /// shows its sidebar (Omen T1 — a distinct flag from the overview gate).
    pub(super) fn refresh_sidebar_visible_gate(&self) {
        let any_visible = self.windows.iter().any(|(window_id, state)| {
            state.sidebar_visible && self.window_sidebar_eligible(*window_id)
        });
        self.sidebar_visible_gate
            .store(any_visible, std::sync::atomic::Ordering::Relaxed);
    }

    /// Toggle the sidebar on the focused window only (FR-4), then grid-first
    /// resize that window's panes to the new pane area (Omen P3/AC-5) — no
    /// other window's visibility or grid is touched. A no-op for an ineligible
    /// (quick-terminal) focused window.
    pub(super) fn toggle_sidebar(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        if !self.window_sidebar_eligible(window_id) {
            return;
        }
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        state.sidebar_visible = !state.sidebar_visible;
        state.sidebar_scroll = 0;
        let window = state.window.clone();

        self.refresh_sidebar_visible_gate();
        // Grid-first: `relayout_and_resize_window` applies the inset then routes
        // through `pane_resize_batch_plan` (grid resize before pty winsize).
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
    }

    /// Route a left-press at `point` (physical px) that lands in the focused
    /// window's sidebar band. Returns `true` when the click was consumed, so
    /// the caller stops before the terminal/split handling sees it (the
    /// terminal must never see a sidebar click). Card hits switch focus to that
    /// session's window (FR-3, A-flavor); the toolbar `+`/`…` and per-card menu
    /// are stubbed for PR4.
    pub(super) fn handle_sidebar_press(&mut self, window_id: WindowId, point: split_tree::Point) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 || point.x >= inset {
            return false;
        }
        let (bounds, scroll) = {
            let Some(state) = self.windows.get(&window_id) else {
                return false;
            };
            let size = state.window.inner_size();
            (
                crate::sidebar::SidebarRect::new(0, 0, inset, size.height),
                state.sidebar_scroll,
            )
        };
        let ids = self.session_store.ordered_ids();
        match crate::sidebar::sidebar_hit_test(bounds, &ids, scroll, point) {
            Some(crate::sidebar::SidebarHit::Card(card)) => {
                self.focus_session_card(card);
                true
            }
            // A press anywhere else in the band (toolbar buttons, per-card menu,
            // header, gutters) is still consumed so the terminal never sees it;
            // the +/…/CardMenu handlers land in PR4.
            Some(_) => true,
            // Inside the band but not on any actionable target: consume it too,
            // since the band is not part of the terminal surface.
            None => true,
        }
    }

    /// Scroll the sidebar card list when the wheel turns over the band
    /// (FR-15). Returns `true` when consumed (so the terminal never scrolls).
    /// `lines` is the wheel delta in card-stride units; positive scrolls down.
    pub(super) fn handle_sidebar_wheel(&mut self, window_id: WindowId, lines: f32) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        let point = self.windows.get(&window_id).and_then(|s| s.last_mouse_point);
        if inset == 0 || point.is_none_or(|p| p.x >= inset) {
            return false;
        }
        let Some(state) = self.windows.get(&window_id) else {
            return false;
        };
        let bounds = crate::sidebar::SidebarRect::new(0, 0, inset, state.window.inner_size().height);
        let viewport_h = crate::sidebar::sidebar_bands(bounds).viewport.h;
        let content_h = crate::sidebar::content_height(self.session_store.len());
        let step = crate::sidebar::SIDEBAR_CARD_STRIDE as f32;
        let delta = (-lines * step).round() as i64;
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let next = (state.sidebar_scroll as i64 + delta).max(0) as u32;
        state.sidebar_scroll = crate::sidebar::clamp_scroll(next, content_h, viewport_h);
        state.window.request_redraw();
        true
    }

    /// Switch focus to the window/pane a clicked card belongs to (FR-3,
    /// A-flavor: focus only, never an active-swap). Converts the card's
    /// GUI-agnostic window id back to the winit `WindowId`.
    fn focus_session_card(&mut self, card: SessionCardId) {
        let window_id = WindowId::from(card.window_id.0);
        let Some(window) = self.windows.get(&window_id).map(|state| state.window.clone()) else {
            return;
        };
        self.focus_pane(window_id, card.pane_id);
        self.focused = Some(window_id);
        window.focus_window();
    }
}
