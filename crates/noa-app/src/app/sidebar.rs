//! Session-sidebar subsystem — the `App`-side glue that turns the pure
//! [`crate::session_store`] + [`crate::sidebar`] modules into a live feature:
//! applying io-thread deltas, garbage-collecting torn-down sessions,
//! per-window toggle + grid-first resize, click routing, and the draw path.
//!
//! Everything visual/windowing lives here (not in the two pure modules), so
//! `session_store.rs`/`sidebar.rs` stay GUI-agnostic (NFR-6). The draw path
//! reads only the store and the pure layout — it never locks a `Terminal`
//! (NFR-1/AC-17).

use std::fmt::Write as _;

use noa_core::{Color, Rgb};

use super::*;
use crate::session_store::{SessionCard, SessionDelta, StatusDot, status_dot};
use crate::sidebar::{
    CARD_MENU_ITEMS, CARD_PREVIEW_LABEL, CardLines, CardRects, HeaderRects, SIDEBAR_CARD_H,
    SidebarRect, card_lines, card_local_rects, header_rects, header_status_label, icon_glyph,
    sidebar_bands, sidebar_layout,
};

/// Whether an io-thread [`SessionDelta`] targeting a window with the given
/// sidebar eligibility should reach the store (FR-14/AC-16b). Quick-terminal
/// windows are ineligible and must never get a card, so their io-thread-posted
/// `Upsert`/`Bell` are dropped here at the apply boundary — a QT pane shares the
/// app-wide publish gate, so with a sidebar open elsewhere it would otherwise
/// leak a card into every window's sidebar. App-originated `Remove`/`Branch`/
/// `Rename` only ever target real windows, so they pass through unconditionally
/// (and dropping a QT `Remove` would be harmless anyway).
fn session_delta_should_apply(delta: &SessionDelta, window_eligible: bool) -> bool {
    match delta {
        SessionDelta::Upsert { .. } | SessionDelta::Bell { .. } => window_eligible,
        SessionDelta::Remove { .. } | SessionDelta::Branch { .. } | SessionDelta::Rename { .. } => {
            true
        }
    }
}

// Sidebar palette (⚠G: compile-time, no config knob — matches the mockup's dark
// chrome; the terminal panes keep their own theme).
const SIDEBAR_BG: Rgb = Rgb::new(0x14, 0x16, 0x1b);
const SIDEBAR_FG: Rgb = Rgb::new(0xd8, 0xdc, 0xe4);
const SIDEBAR_DIM_FG: Rgb = Rgb::new(0x8a, 0x90, 0x9c);
/// Very dim tone for the preview heading and its separator rule.
const SIDEBAR_FAINT_FG: Rgb = Rgb::new(0x56, 0x5c, 0x68);
const SIDEBAR_MENU_BG: Rgb = Rgb::new(0x24, 0x28, 0x30);
const SIDEBAR_DOT_BLUE: Rgb = Rgb::new(0x4c, 0x9a, 0xff);
const SIDEBAR_DOT_GREEN: Rgb = Rgb::new(0x46, 0xc4, 0x66);
const SIDEBAR_DOT_YELLOW: Rgb = Rgb::new(0xe6, 0xb4, 0x50);
/// A card's own background — slightly lighter than the band so each card reads
/// as a distinct rounded surface (mockup parity).
const SIDEBAR_CARD_BG: Rgb = Rgb::new(0x1c, 0x20, 0x28);
/// The selected card's background — brighter still, paired with the accent ring.
const SIDEBAR_CARD_BG_SELECTED: Rgb = Rgb::new(0x25, 0x2c, 0x39);
/// The subtle 1px border stroked around every (unselected) card.
const SIDEBAR_CARD_BORDER: Rgb = Rgb::new(0x2b, 0x30, 0x3b);
/// The accent (focus ring + left edge bar) for the selected card.
const SIDEBAR_ACCENT: Rgb = Rgb::new(0x4c, 0x9a, 0xff);
/// The rounded header session-name pill background.
const SIDEBAR_PILL_BG: Rgb = Rgb::new(0x24, 0x2a, 0x35);

/// A project icon's tint (FR-9 mockup parity), so the icon column carries a
/// little color rather than flat gray.
fn icon_color(icon: crate::session_store::IconKind) -> Rgb {
    use crate::session_store::IconKind;
    match icon {
        IconKind::Rust => Rgb::new(0xd9, 0x82, 0x5a),
        IconKind::Node => Rgb::new(0x6c, 0xc2, 0x4a),
        IconKind::Terraform => Rgb::new(0x84, 0x4f, 0xba),
        IconKind::Go => Rgb::new(0x4c, 0xb9, 0xd4),
        IconKind::Python => Rgb::new(0x5a, 0x9f, 0xd4),
        IconKind::Git => Rgb::new(0xe0, 0x6c, 0x4e),
        IconKind::Folder => SIDEBAR_DIM_FG,
    }
}

/// Resolve a preview span's cell color to an RGB for the sidebar (FR-2). The
/// terminal default fg maps to the sidebar's dim gray (so undifferentiated
/// output recedes); a concrete ANSI/truecolor is resolved through the theme
/// palette and dimmed a touch so the preview sits behind the name and meta.
fn resolve_preview_fg(theme: &Theme, color: Color) -> Rgb {
    match color {
        Color::Default => SIDEBAR_DIM_FG,
        other => {
            let [r, g, b, _] = theme.resolve(other, true);
            let dim = 0.88;
            Rgb::new(
                (r * 255.0 * dim).round().clamp(0.0, 255.0) as u8,
                (g * 255.0 * dim).round().clamp(0.0, 255.0) as u8,
                (b * 255.0 * dim).round().clamp(0.0, 255.0) as u8,
            )
        }
    }
}

/// The dot glyph color for a card's status (FR-11), driven by the pure
/// `status_dot` mapping in `session_store` (AC-13).
fn status_dot_rgb(dot: StatusDot) -> Rgb {
    match dot {
        StatusDot::Blue => SIDEBAR_DOT_BLUE,
        StatusDot::Green => SIDEBAR_DOT_GREEN,
        StatusDot::Yellow => SIDEBAR_DOT_YELLOW,
    }
}

fn rgb_to_rgba(color: Rgb) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    ]
}

/// One positioned text run in a synthetic sidebar grid (already converted from
/// the pure layout's pixel rects to cell coordinates). `bg` fills the run's
/// cells (used by the `…` menu popup and the selected-card accent bar); `None`
/// leaves the underlying background showing. `bold` renders the run's cells in
/// the bold weight (card names).
struct SidebarTextRun {
    col: u16,
    row: u16,
    text: String,
    fg: Rgb,
    bg: Option<Rgb>,
    bold: bool,
}

impl SidebarTextRun {
    fn new(col: u16, row: u16, text: String, fg: Rgb) -> Self {
        Self {
            col,
            row,
            text,
            fg,
            bg: None,
            bold: false,
        }
    }
}

/// One session card's own rounded-card render: its window-space rect, the
/// per-card grid, background color, selection flag, and the text runs in the
/// card's local texture space. Only fully-visible cards get one; partially
/// scrolled cards stay flat on the backdrop.
struct SidebarCardDraw {
    rect: SidebarRect,
    grid: GridSize,
    bg: Rgb,
    selected: bool,
    runs: Vec<SidebarTextRun>,
}

/// The open card `…` menu popup, composited above the cards so a rounded card
/// can never hide it.
struct SidebarMenuDraw {
    rect: SidebarRect,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
}

/// The full per-frame sidebar draw model. Built with only the store + pure
/// layout (no `Terminal` lock — AC-17). `runs` is the flat dark backdrop
/// (header/toolbar chrome + every card's text) rasterized into the band
/// texture; `cards` are the per-card rounded overlays drawn on top for fully
/// visible cards; `menu` is the optional popup above them all.
pub(super) struct SidebarDrawModel {
    inset: u32,
    height: u32,
    scale: f32,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
    cards: Vec<SidebarCardDraw>,
    menu: Option<SidebarMenuDraw>,
}

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
    ///
    /// Deltas for an ineligible (quick-terminal) window are dropped here
    /// (FR-14/AC-16b): a QT pane shares the app-wide publish gate, so without
    /// this guard its output would leak a card into every window's sidebar
    /// whenever a sidebar is open elsewhere. Because the card never enters, no
    /// reconcile is needed when the quick terminal is torn down.
    pub(super) fn apply_session_delta(&mut self, delta: SessionDelta) {
        let window_id = WindowId::from(delta.id().window_id.0);
        if !session_delta_should_apply(&delta, self.window_sidebar_eligible(window_id)) {
            return;
        }
        // A cwd change (new card or a changed cwd on an existing one) triggers a
        // branch + icon poll on the dedicated worker (FR-8/FR-9), never on the
        // io read loop (NFR-2/AC-18). Compared before `apply` moves the delta.
        if let SessionDelta::Upsert { id, cwd, .. } = &delta
            && !cwd.is_empty()
            && self.session_store.get(id).is_none_or(|card| &card.cwd != cwd)
        {
            self.request_branch_poll(*id, cwd.clone());
        }
        self.session_store.apply(delta);
        self.request_sidebar_redraw();
    }

    /// Queue a branch/icon poll for a card whose cwd just changed (FR-8/FR-9).
    /// Forwarded to the dedicated worker thread so `git` never runs on the io
    /// read loop (NFR-2). A no-op if the worker has already been torn down.
    fn request_branch_poll(&self, id: SessionCardId, cwd: String) {
        if let Some(worker) = self.branch_poll.as_ref() {
            worker.request(id, cwd);
        }
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
        state.sidebar_menu = None;
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
    /// session's window (FR-3, A-flavor); the toolbar `+` opens a cwd-inherited
    /// new tab (FR-6); a card `…` opens/closes its close-menu (FR-7).
    pub(super) fn handle_sidebar_press(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 || point.x >= inset {
            return false;
        }

        // An open card `…` menu takes the click first: an item hit runs the
        // action, anything else dismisses the popup (and falls through to normal
        // routing so the same click still selects/scrolls). Remember which card
        // was dismissed so a click on its own `…` button doesn't immediately
        // reopen the menu it just closed (a toggle-then-retoggle).
        let mut dismissed_menu: Option<SessionCardId> = None;
        if let Some(open) = self.windows.get(&window_id).and_then(|s| s.sidebar_menu) {
            if let Some(anchor) = self.card_menu_anchor(window_id, open) {
                let popup =
                    crate::sidebar::card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), inset);
                if let Some(item) = crate::sidebar::card_menu_hit_test(popup, point) {
                    self.close_sidebar_menu(window_id);
                    self.activate_card_menu_item(event_loop, open, item);
                    return true;
                }
            }
            self.close_sidebar_menu(window_id);
            dismissed_menu = Some(open);
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
            Some(crate::sidebar::SidebarHit::CardMenu(card)) => {
                // If this same click just dismissed this card's open menu, leave
                // it closed instead of reopening it (toggle-then-retoggle).
                if dismissed_menu != Some(card) {
                    self.toggle_sidebar_menu(window_id, card);
                }
                true
            }
            Some(crate::sidebar::SidebarHit::NewSession) => {
                // `+`: new tab in the focused window, cwd inherited from the
                // active session via the existing new-tab path (FR-6/AC-8).
                let _ = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow);
                true
            }
            // Header `…` has no v1 action set (Open Question 5); consume it so
            // the terminal never sees the press.
            Some(crate::sidebar::SidebarHit::Menu) => true,
            // Inside the band but not on any actionable target: consume it too,
            // since the band is not part of the terminal surface.
            None => true,
        }
    }

    /// Run a chosen card `…` menu item (FR-7). `Close` routes through the
    /// existing pane teardown for that session (which cascades to `close_tab`
    /// when it is the tab's last pane), so the card disappears via the normal GC
    /// choke point (AC-9b). `Rename`'s UI is deferred (not in
    /// [`crate::sidebar::CARD_MENU_ITEMS`]), so this only ever sees `Close`.
    fn activate_card_menu_item(
        &mut self,
        event_loop: &ActiveEventLoop,
        card: SessionCardId,
        item: crate::sidebar::CardMenuItem,
    ) {
        match item {
            crate::sidebar::CardMenuItem::Close => {
                let window_id = WindowId::from(card.window_id.0);
                self.request_close_pane(event_loop, window_id, card.pane_id);
            }
            crate::sidebar::CardMenuItem::Rename => {}
        }
    }

    /// Toggle the `…` menu popup for `card` in `window_id` (FR-7): a click on the
    /// already-open card's button closes it, otherwise it opens for that card.
    fn toggle_sidebar_menu(&mut self, window_id: WindowId, card: SessionCardId) {
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.sidebar_menu = if state.sidebar_menu == Some(card) {
                None
            } else {
                Some(card)
            };
            state.window.request_redraw();
        }
    }

    /// Close any open card `…` menu in `window_id`.
    pub(super) fn close_sidebar_menu(&mut self, window_id: WindowId) {
        if let Some(state) = self.windows.get_mut(&window_id)
            && state.sidebar_menu.take().is_some()
        {
            state.window.request_redraw();
        }
    }

    /// The on-screen anchor (the card's `menu_button` rect) for an open menu, or
    /// `None` when that card has scrolled out of view. Recomputes the pure
    /// layout the drawer uses so the popup tracks the card.
    fn card_menu_anchor(&self, window_id: WindowId, card: SessionCardId) -> Option<SidebarRect> {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let bounds = SidebarRect::new(0, 0, inset, state.window.inner_size().height);
        let ids = self.session_store.ordered_ids();
        let layout = sidebar_layout(bounds, &ids, state.sidebar_scroll);
        layout
            .cards
            .iter()
            .find(|c| c.id == card)
            .map(|c| c.menu_button)
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

    /// Build the per-frame sidebar draw model for `window_id` (FR-2/FR-5), or
    /// `None` when the window has no visible sidebar. Reads only the store and
    /// the pure layout — never a `Terminal` (AC-17). Computed before the redraw
    /// path borrows `gpu`/`state` mutably, so the drawer can run inline.
    pub(super) fn sidebar_draw_model(&self, window_id: WindowId) -> Option<SidebarDrawModel> {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        let state = self.windows.get(&window_id)?;
        let metrics = gpu.font.metrics();
        let theme = &gpu.theme;
        let scale = state.window.scale_factor() as f32;
        let height = state.window.inner_size().height.max(1);
        let band = PaneRectApp::new(0, 0, inset, height);
        let grid = grid_size_for_pane_rect(band, metrics, self.padding);

        let bounds = SidebarRect::new(0, 0, inset, height);
        let ids = self.session_store.ordered_ids();
        let layout = sidebar_layout(bounds, &ids, state.sidebar_scroll);
        let bands = sidebar_bands(bounds);

        // Pixel → cell conversion, matching where a `Renderer` places cell (0,0):
        // at the padding origin. One closure per grid (band / card / menu).
        let cell_w = metrics.cell_w.max(1.0);
        let cell_h = metrics.cell_h.max(1.0);
        let pad_left = self.padding.left;
        let pad_top = self.padding.top;
        let to_cell = |grid: GridSize| {
            move |x: u32, y: u32| px_to_cell(x, y, pad_left, pad_top, cell_w, cell_h, grid)
        };
        let band_cell = to_cell(grid);

        let mut runs: Vec<SidebarTextRun> = Vec::new();
        let selected_id = Self::session_card_id(window_id, state.focused_pane);

        // Header chrome (FR-5): status label, centered title, the session-name
        // pill (a flat filled pill on the backdrop), and the toolbar +/… glyphs.
        let header: HeaderRects = header_rects(bands.header);
        runs.extend(window_run(
            &band_cell,
            header.status_label,
            header_status_label(self.session_store.busy_count()),
            SIDEBAR_FG,
            false,
        ));
        runs.extend(window_run(
            &band_cell,
            header.title,
            tab_title(&state.title),
            SIDEBAR_DIM_FG,
            false,
        ));
        if let Some(card) = self.session_store.get(&selected_id)
            && header.name_pill.w > 0
            && header.name_pill.h > 0
        {
            let (col, row) = band_cell(header.name_pill.x, header.name_pill.y);
            let pill_cols = (header.name_pill.w as f32 / cell_w).round().max(1.0) as usize;
            let text = format!(
                " {:<width$}",
                card.display_name(),
                width = pill_cols.saturating_sub(1)
            );
            runs.push(SidebarTextRun {
                col,
                row,
                text,
                fg: SIDEBAR_FG,
                bg: Some(SIDEBAR_PILL_BG),
                bold: true,
            });
        }
        runs.extend(window_run(
            &band_cell,
            layout.new_button,
            "+".to_string(),
            SIDEBAR_FG,
            true,
        ));
        runs.extend(window_run(
            &band_cell,
            layout.menu_button,
            "…".to_string(),
            SIDEBAR_DIM_FG,
            false,
        ));

        // Card text on the flat backdrop, plus a rounded overlay for every
        // fully-visible card (FR-2). Partially-scrolled cards stay flat.
        let now = sidebar_wall_clock_now();
        let mut cards: Vec<SidebarCardDraw> = Vec::new();
        let card_band = PaneRectApp::new(0, 0, inset, SIDEBAR_CARD_H);
        let card_grid = grid_size_for_pane_rect(card_band, metrics, self.padding);
        let card_cell = to_cell(card_grid);
        for card_rects in &layout.cards {
            let Some(card) = self.session_store.get(&card_rects.id) else {
                continue;
            };
            let lines: CardLines = card_lines(card, now);
            emit_card_text(&mut runs, card_rects, card, &lines, theme, &band_cell);

            if card_rects.bounds.h == SIDEBAR_CARD_H {
                let selected = card_rects.id == selected_id;
                let local = card_local_rects(card_rects.id, inset);
                let mut card_runs = Vec::new();
                emit_card_text(&mut card_runs, &local, card, &lines, theme, &card_cell);
                cards.push(SidebarCardDraw {
                    rect: card_rects.bounds,
                    grid: card_grid,
                    bg: if selected {
                        SIDEBAR_CARD_BG_SELECTED
                    } else {
                        SIDEBAR_CARD_BG
                    },
                    selected,
                    runs: card_runs,
                });
            }
        }

        // Card `…` menu popup (FR-7): its own overlay, composited above the cards
        // so a rounded card can never hide it. Skipped when the open card has
        // scrolled out of view or the popup would spill past the window bottom.
        let menu = state.sidebar_menu.and_then(|open| {
            let card_rects = layout.cards.iter().find(|c| c.id == open)?;
            let popup = crate::sidebar::card_menu_popup_rect(
                card_rects.menu_button,
                CARD_MENU_ITEMS.len(),
                inset,
            );
            if popup.w == 0 || popup.h == 0 || popup.bottom() > height {
                return None;
            }
            let menu_band = PaneRectApp::new(0, 0, popup.w, popup.h);
            let menu_grid = grid_size_for_pane_rect(menu_band, metrics, self.padding);
            let menu_cell = to_cell(menu_grid);
            let mut menu_runs = Vec::new();
            for (index, &item) in CARD_MENU_ITEMS.iter().enumerate() {
                let item_rect = crate::sidebar::card_menu_item_rect(popup, index);
                let (col, row) = menu_cell(
                    item_rect.x.saturating_sub(popup.x),
                    item_rect.y.saturating_sub(popup.y),
                );
                menu_runs.push(SidebarTextRun::new(
                    col,
                    row,
                    format!(" {}", crate::sidebar::card_menu_label(item)),
                    SIDEBAR_FG,
                ));
            }
            Some(SidebarMenuDraw {
                rect: popup,
                grid: menu_grid,
                runs: menu_runs,
            })
        });

        Some(SidebarDrawModel {
            inset,
            height,
            scale,
            grid,
            runs,
            cards,
            menu,
        })
    }
}

/// Pixel → cell for a synthetic sidebar grid whose `Renderer` places cell (0,0)
/// at the padding origin, clamped into the grid.
fn px_to_cell(
    x: u32,
    y: u32,
    pad_left: f32,
    pad_top: f32,
    cell_w: f32,
    cell_h: f32,
    grid: GridSize,
) -> (u16, u16) {
    let col = ((x as f32 - pad_left) / cell_w).round().max(0.0) as u16;
    let row = ((y as f32 - pad_top) / cell_h).round().max(0.0) as u16;
    (
        col.min(grid.cols.saturating_sub(1)),
        row.min(grid.rows.saturating_sub(1)),
    )
}

/// A single-rect text run positioned via `to_cell`, or `None` for an empty rect
/// or text (so callers can `extend`).
fn window_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    text: String,
    fg: Rgb,
    bold: bool,
) -> Option<SidebarTextRun> {
    if rect.w == 0 || rect.h == 0 || text.is_empty() {
        return None;
    }
    let (col, row) = to_cell(rect.x, rect.y);
    Some(SidebarTextRun {
        col,
        row,
        text,
        fg,
        bg: None,
        bold,
    })
}

/// Emit one card's text runs (status dot, project icon, bold name, updated-time,
/// meta, the dim "最終出力" heading + separator, and the color-run preview)
/// through `to_cell`. Shared by the flat backdrop (window coords) and each
/// rounded overlay (card-local coords) so both agree on layout.
fn emit_card_text(
    out: &mut Vec<SidebarTextRun>,
    rects: &CardRects,
    card: &SessionCard,
    lines: &CardLines,
    theme: &Theme,
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
) {
    out.extend(window_run(
        to_cell,
        rects.dot,
        "●".to_string(),
        status_dot_rgb(status_dot(card)),
        false,
    ));
    out.extend(window_run(
        to_cell,
        rects.icon,
        icon_glyph(card.icon).to_string(),
        icon_color(card.icon),
        false,
    ));
    out.extend(window_run(
        to_cell,
        rects.name_line,
        lines.name.clone(),
        SIDEBAR_FG,
        true,
    ));
    out.extend(window_run(
        to_cell,
        rects.updated,
        lines.updated.clone(),
        SIDEBAR_DIM_FG,
        false,
    ));
    out.extend(window_run(
        to_cell,
        rects.meta_line,
        lines.meta.clone(),
        SIDEBAR_DIM_FG,
        false,
    ));

    // Preview heading + separator rule (FR-2 mockup parity).
    if rects.label.w > 0 && rects.label.h > 0 {
        let (col, row) = to_cell(rects.label.x, rects.label.y);
        out.push(SidebarTextRun::new(
            col,
            row,
            CARD_PREVIEW_LABEL.to_string(),
            SIDEBAR_FAINT_FG,
        ));
        // A dim rule fills from just past the heading to the card's right pad.
        let heading_cols = CARD_PREVIEW_LABEL.chars().count() as u16 * 2;
        let sep_start = col.saturating_add(heading_cols).saturating_add(1);
        let (end_col, _) = to_cell(rects.label.right().saturating_sub(1), rects.label.y);
        if end_col > sep_start {
            out.push(SidebarTextRun::new(
                sep_start,
                row,
                "─".repeat((end_col - sep_start) as usize),
                SIDEBAR_FAINT_FG,
            ));
        }
    }

    // Preview lines, each in its original ANSI colors (FR-2).
    for (slot, line) in rects.preview.iter().zip(lines.preview.iter()) {
        if slot.w == 0 || slot.h == 0 {
            continue;
        }
        let (mut col, row) = to_cell(slot.x, slot.y);
        for span in line {
            if span.text.is_empty() {
                continue;
            }
            out.push(SidebarTextRun::new(
                col,
                row,
                span.text.clone(),
                resolve_preview_fg(theme, span.fg),
            ));
            col = col.saturating_add(span.text.chars().count() as u16);
        }
    }
}

/// Wall-clock now, in the viewer's local zone, for the sidebar's relative
/// updated-time (mirrors the io thread's stamp so both agree).
fn sidebar_wall_clock_now() -> crate::session_store::WallClock {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    crate::session_store::civil_from_unix_secs(unix + crate::localtime::local_offset_seconds())
}

/// Rasterize one synthetic sidebar grid (background + positioned text/dots)
/// with the reused `Renderer` into `view`. `base_bg` fills the empty cells and
/// the clear color so a card texture reads as its own surface.
#[allow(clippy::too_many_arguments)]
fn rasterize_runs(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut FontGrid,
    theme: &Theme,
    view: &wgpu::TextureView,
    size: PixelSize,
    grid: GridSize,
    base_bg: Rgb,
    runs: &[SidebarTextRun],
) {
    let mut term = Terminal::new(grid);
    term.set_base_colors(SIDEBAR_FG, base_bg, SIDEBAR_FG, theme.palette);
    let mut stream = Stream::new();
    // Autowrap off so a long cwd/preview clips at the right margin instead of
    // wrapping to the next row and shifting every run below it.
    stream.feed(b"\x1b[?7l", &mut term);
    let mut feed = String::new();
    for run in runs {
        feed.clear();
        // CUP is 1-based; position, optional bold, truecolor fg (+bg), write, reset.
        let _ = write!(feed, "\x1b[{};{}H", run.row + 1, run.col + 1);
        if run.bold {
            let _ = write!(feed, "\x1b[1m");
        }
        let _ = write!(feed, "\x1b[38;2;{};{};{}m", run.fg.r, run.fg.g, run.fg.b);
        if let Some(bg) = run.bg {
            let _ = write!(feed, "\x1b[48;2;{};{};{}m", bg.r, bg.g, bg.b);
        }
        let _ = write!(feed, "{}\x1b[0m", run.text);
        stream.feed(feed.as_bytes(), &mut term);
    }
    let mut snapshot = FrameSnapshot::from_terminal(&mut term);
    snapshot.cursor.visible = false;

    renderer.resize(size);
    renderer.rebuild_cells(&snapshot, font, theme);
    renderer.set_clear_color(rgb_to_rgba(base_bg));
    renderer.sync_atlas(device, queue, font);
    renderer.draw(device, queue, view);
}

/// Ensure `slot` holds a scratch render texture of exactly `size`/`format`,
/// reallocating only when either changes (reused frame-to-frame — F2).
fn ensure_scratch(
    slot: &mut Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    device: &wgpu::Device,
    size: PixelSize,
    format: wgpu::TextureFormat,
    label: &'static str,
) {
    let size = PixelSize {
        w: size.w.max(1),
        h: size.h.max(1),
    };
    if slot
        .as_ref()
        .is_none_or(|(s, t, _)| *s != size || t.format() != format)
    {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.w,
                height: size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        *slot = Some((size, texture, view));
    }
}

/// Rasterize the sidebar and composite it onto `view` at the window's left
/// inset via the reused rounded-card pipeline: a flat dark backdrop (chrome +
/// card text), then each fully-visible card as a rounded card with a subtle
/// border and a focus ring on the selected one, then the optional `…` menu
/// popup above them all. Runs inline in `redraw` with the already-borrowed
/// `gpu`, so the model must be prebuilt (no `self` here).
pub(super) fn draw_sidebar_band(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    padding: GridPadding,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    model: &SidebarDrawModel,
) {
    // Lazily (re)build the reused band renderer + card pipeline for this format.
    if gpu
        .sidebar_renderer
        .as_ref()
        .is_none_or(|renderer| renderer.target_format() != surface_format)
    {
        gpu.sidebar_renderer =
            Renderer::new(&gpu.device, &gpu.queue, surface_format, &mut gpu.font, padding).ok();
    }
    if gpu
        .sidebar_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.sidebar_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            pipeline: CardPipeline::new(&gpu.device, surface_format),
        });
    }
    let band_size = PixelSize {
        w: model.inset.max(1),
        h: model.height.max(1),
    };
    ensure_scratch(
        &mut gpu.sidebar_band,
        &gpu.device,
        band_size,
        surface_format,
        "noa-sidebar-band",
    );
    if !model.cards.is_empty() {
        ensure_scratch(
            &mut gpu.sidebar_card_tex,
            &gpu.device,
            PixelSize {
                w: model.inset,
                h: SIDEBAR_CARD_H,
            },
            surface_format,
            "noa-sidebar-card",
        );
    }
    if let Some(menu) = &model.menu {
        ensure_scratch(
            &mut gpu.sidebar_menu_tex,
            &gpu.device,
            PixelSize {
                w: menu.rect.w,
                h: menu.rect.h,
            },
            surface_format,
            "noa-sidebar-menu",
        );
    }

    if gpu.sidebar_renderer.is_none() || gpu.sidebar_card.is_none() || gpu.sidebar_band.is_none() {
        return;
    }

    // 1) Flat dark backdrop (chrome + all card text) → band texture, composited
    // over the inset with no rounding. `overlay_texture_cards` loads (doesn't
    // clear) so the panes to the right are untouched.
    {
        let band_view = &gpu.sidebar_band.as_ref().unwrap().2;
        rasterize_runs(
            gpu.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.font,
            &gpu.theme,
            band_view,
            band_size,
            model.grid,
            SIDEBAR_BG,
            &model.runs,
        );
    }
    let flat_style = CardStyle {
        background: rgb_to_rgba(SIDEBAR_BG),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    gpu.sidebar_card.as_ref().unwrap().pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &flat_style,
        &[CardTexturePlacement {
            texture_view: &gpu.sidebar_band.as_ref().unwrap().2,
            x: 0,
            y: 0,
            w: model.inset,
            h: model.height,
            selected: false,
        }],
    );

    // 2) Each fully-visible card as a rounded card. One reused scratch texture
    // serves every card in turn (render → composite), so submits serialize the
    // reuse safely.
    let card_style = CardStyle {
        background: rgb_to_rgba(SIDEBAR_CARD_BG),
        border_color: rgb_to_rgba(SIDEBAR_CARD_BORDER),
        focus_color: rgb_to_rgba(SIDEBAR_ACCENT),
        corner_radius: 10.0 * model.scale,
        border_width: 1.0 * model.scale,
        focus_width: 2.0 * model.scale,
        focus_glow_width: 6.0 * model.scale,
    };
    for card_draw in &model.cards {
        let Some((_, _, card_view)) = gpu.sidebar_card_tex.as_ref() else {
            break;
        };
        rasterize_runs(
            gpu.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.font,
            &gpu.theme,
            card_view,
            PixelSize {
                w: model.inset,
                h: SIDEBAR_CARD_H,
            },
            card_draw.grid,
            card_draw.bg,
            &card_draw.runs,
        );
        gpu.sidebar_card.as_ref().unwrap().pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            view,
            surface_size,
            &card_style,
            &[CardTexturePlacement {
                texture_view: &gpu.sidebar_card_tex.as_ref().unwrap().2,
                x: card_draw.rect.x,
                y: card_draw.rect.y,
                w: card_draw.rect.w,
                h: card_draw.rect.h,
                selected: card_draw.selected,
            }],
        );
    }

    // 3) The `…` menu popup, composited above the cards.
    if let Some(menu) = &model.menu
        && let Some((_, _, menu_view)) = gpu.sidebar_menu_tex.as_ref()
    {
        rasterize_runs(
            gpu.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.font,
            &gpu.theme,
            menu_view,
            PixelSize {
                w: menu.rect.w,
                h: menu.rect.h,
            },
            menu.grid,
            SIDEBAR_MENU_BG,
            &menu.runs,
        );
        let menu_style = CardStyle {
            background: rgb_to_rgba(SIDEBAR_MENU_BG),
            border_color: rgb_to_rgba(SIDEBAR_CARD_BORDER),
            focus_color: [0.0; 4],
            corner_radius: 6.0 * model.scale,
            border_width: 1.0 * model.scale,
            focus_width: 0.0,
            focus_glow_width: 0.0,
        };
        gpu.sidebar_card.as_ref().unwrap().pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            view,
            surface_size,
            &menu_style,
            &[CardTexturePlacement {
                texture_view: &gpu.sidebar_menu_tex.as_ref().unwrap().2,
                x: menu.rect.x,
                y: menu.rect.y,
                w: menu.rect.w,
                h: menu.rect.h,
                selected: false,
            }],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::WallClock;

    fn upsert(window: u64) -> SessionDelta {
        SessionDelta::Upsert {
            id: SessionCardId::new(SessionWindowId(window), PaneId::new(1)),
            seq: 1,
            name: "shell".to_string(),
            cwd: "/repo".to_string(),
            busy: false,
            updated_at: WallClock {
                year: 2026,
                month: 7,
                day: 5,
                hour: 10,
                minute: 0,
            },
            preview: Vec::new(),
        }
    }

    // F1 (FR-14/AC-16b): a quick-terminal window is ineligible, so its
    // io-thread-posted Upsert/Bell are dropped at the apply boundary and never
    // land in the store — even though the QT pane shares the app-wide publish
    // gate. An eligible window's delta lands as normal.
    #[test]
    fn ineligible_window_deltas_are_dropped_at_the_apply_boundary() {
        let mut store = SessionStore::new();
        let delta = upsert(9);

        // Ineligible (quick-terminal) window: Upsert dropped, store stays empty.
        assert!(!session_delta_should_apply(&delta, false));
        if session_delta_should_apply(&delta, false) {
            store.apply(delta.clone());
        }
        assert_eq!(store.len(), 0);

        // Eligible window: the same Upsert lands.
        assert!(session_delta_should_apply(&delta, true));
        if session_delta_should_apply(&delta, true) {
            store.apply(delta);
        }
        assert_eq!(store.len(), 1);

        // Bell is gated the same way; Remove always applies (harmless for a QT).
        let id = SessionCardId::new(SessionWindowId(9), PaneId::new(1));
        assert!(!session_delta_should_apply(&SessionDelta::Bell { id }, false));
        assert!(session_delta_should_apply(&SessionDelta::Remove { id }, false));
    }
}
