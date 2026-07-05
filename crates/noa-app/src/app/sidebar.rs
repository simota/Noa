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
}
