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

use noa_core::Rgb;

use super::*;
use crate::session_store::{SessionDelta, StatusDot, status_dot};
use crate::sidebar::{
    CARD_MENU_ITEMS, CardLines, HeaderRects, SidebarRect, card_lines, header_rects,
    header_status_label, sidebar_bands, sidebar_layout,
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
const SIDEBAR_MENU_BG: Rgb = Rgb::new(0x24, 0x28, 0x30);
const SIDEBAR_DOT_BLUE: Rgb = Rgb::new(0x4c, 0x9a, 0xff);
const SIDEBAR_DOT_GREEN: Rgb = Rgb::new(0x46, 0xc4, 0x66);
const SIDEBAR_DOT_YELLOW: Rgb = Rgb::new(0xe6, 0xb4, 0x50);

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

/// One positioned text run in the synthetic sidebar grid (already converted
/// from the pure layout's pixel rects to cell coordinates). `bg` fills the run's
/// cells (used by the `…` menu popup so it visually sits above the cards);
/// `None` leaves the sidebar background showing.
struct SidebarTextRun {
    col: u16,
    row: u16,
    text: String,
    fg: Rgb,
    bg: Option<Rgb>,
}

/// The full per-frame sidebar draw model: the synthetic grid size and every
/// positioned text run. Built with only the store + pure layout (no
/// `Terminal` lock — AC-17), then rasterized into the band texture.
pub(super) struct SidebarDrawModel {
    inset: u32,
    height: u32,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
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
        let height = state.window.inner_size().height.max(1);
        let band = PaneRectApp::new(0, 0, inset, height);
        let grid = grid_size_for_pane_rect(band, metrics, self.padding);

        let bounds = SidebarRect::new(0, 0, inset, height);
        let ids = self.session_store.ordered_ids();
        let layout = sidebar_layout(bounds, &ids, state.sidebar_scroll);
        let bands = sidebar_bands(bounds);

        // Pixel → cell conversion, matching where the band `Renderer` places
        // cell (0,0): at the padding origin.
        let cell_w = metrics.cell_w.max(1.0);
        let cell_h = metrics.cell_h.max(1.0);
        let pad_left = self.padding.left;
        let pad_top = self.padding.top;
        let to_cell = |x: u32, y: u32| -> (u16, u16) {
            let col = ((x as f32 - pad_left) / cell_w).round().max(0.0) as u16;
            let row = ((y as f32 - pad_top) / cell_h).round().max(0.0) as u16;
            (col.min(grid.cols.saturating_sub(1)), row.min(grid.rows.saturating_sub(1)))
        };

        let mut runs: Vec<SidebarTextRun> = Vec::new();
        let push = |rect: SidebarRect, text: String, fg: Rgb, runs: &mut Vec<SidebarTextRun>| {
            if rect.w == 0 || rect.h == 0 || text.is_empty() {
                return;
            }
            let (col, row) = to_cell(rect.x, rect.y);
            runs.push(SidebarTextRun {
                col,
                row,
                text,
                fg,
                bg: None,
            });
        };

        // Header (FR-5): status label, centered window title, session-name pill.
        let header: HeaderRects = header_rects(bands.header);
        push(
            header.status_label,
            header_status_label(self.session_store.busy_count()),
            SIDEBAR_FG,
            &mut runs,
        );
        push(header.title, tab_title(&state.title), SIDEBAR_FG, &mut runs);
        if let Some(card) = self
            .session_store
            .get(&Self::session_card_id(window_id, state.focused_pane))
        {
            push(header.name_pill, card.display_name().to_string(), SIDEBAR_DIM_FG, &mut runs);
        }

        // Cards (FR-2): icon+name / cwd+branch / two preview lines / updated,
        // plus the status dot.
        let now = sidebar_wall_clock_now();
        for card_rects in &layout.cards {
            let Some(card) = self.session_store.get(&card_rects.id) else {
                continue;
            };
            let lines: CardLines = card_lines(card, now);
            push(card_rects.name_line, lines.name, SIDEBAR_FG, &mut runs);
            push(card_rects.meta_line, lines.meta, SIDEBAR_DIM_FG, &mut runs);
            for (slot, preview) in card_rects.preview.iter().zip(lines.preview.iter()) {
                push(*slot, preview.clone(), SIDEBAR_DIM_FG, &mut runs);
            }
            push(card_rects.updated, lines.updated, SIDEBAR_DIM_FG, &mut runs);
            push(
                card_rects.dot,
                "●".to_string(),
                status_dot_rgb(status_dot(card)),
                &mut runs,
            );
        }

        // Card `…` menu popup (FR-7): drawn last so it sits above the cards. Each
        // item is a full-width background-filled run. Skipped when the open
        // card has scrolled out of the visible layout.
        if let Some(open) = state.sidebar_menu
            && let Some(card_rects) = layout.cards.iter().find(|c| c.id == open)
        {
            let popup = crate::sidebar::card_menu_popup_rect(
                card_rects.menu_button,
                CARD_MENU_ITEMS.len(),
                inset,
            );
            let menu_cols = (popup.w as f32 / cell_w).round().max(1.0) as usize;
            for (index, &item) in CARD_MENU_ITEMS.iter().enumerate() {
                let item_rect = crate::sidebar::card_menu_item_rect(popup, index);
                if item_rect.bottom() > height {
                    continue;
                }
                let (col, row) = to_cell(item_rect.x, item_rect.y);
                let label = format!(" {:<width$}", crate::sidebar::card_menu_label(item), width = menu_cols.saturating_sub(1));
                runs.push(SidebarTextRun {
                    col,
                    row,
                    text: label,
                    fg: SIDEBAR_FG,
                    bg: Some(SIDEBAR_MENU_BG),
                });
            }
        }

        Some(SidebarDrawModel {
            inset,
            height,
            grid,
            runs,
        })
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

/// Rasterize the sidebar band (background + positioned text/dots) with the
/// single reused `Renderer` and composite it onto `view` at the window's left
/// inset via the reused rounded-card pipeline. Runs inline in `redraw` with the
/// already-borrowed `gpu`, so the model must be prebuilt (no `self` here).
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
        gpu.sidebar_renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            surface_format,
            &mut gpu.font,
            padding,
        )
        .ok();
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
    // Reuse the band texture across frames, reallocating only when the band
    // size changes (window / sidebar-width resize) — F2.
    let band_size = PixelSize {
        w: model.inset.max(1),
        h: model.height.max(1),
    };
    if gpu
        .sidebar_band
        .as_ref()
        .is_none_or(|(size, texture, _)| *size != band_size || texture.format() != surface_format)
    {
        let band_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-sidebar-band"),
            size: wgpu::Extent3d {
                width: band_size.w,
                height: band_size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: surface_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let band_view = band_texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.sidebar_band = Some((band_size, band_texture, band_view));
    }

    let (Some(renderer), Some(card), Some((_, _, band_view))) = (
        gpu.sidebar_renderer.as_mut(),
        gpu.sidebar_card.as_ref(),
        gpu.sidebar_band.as_ref(),
    ) else {
        return;
    };

    // Synthetic terminal carrying the whole band's text (background = the
    // sidebar bg so empty cells match), fed positioned runs.
    let mut term = Terminal::new(model.grid);
    term.set_base_colors(SIDEBAR_FG, SIDEBAR_BG, SIDEBAR_FG, gpu.theme.palette);
    let mut stream = Stream::new();
    // Autowrap off so a long cwd/preview clips at the right margin instead of
    // wrapping to the next row and shifting every run below it.
    stream.feed(b"\x1b[?7l", &mut term);
    let mut feed = String::new();
    for run in &model.runs {
        feed.clear();
        // CUP is 1-based; position, then set truecolor fg (+bg for popup runs),
        // write, reset.
        let _ = write!(feed, "\x1b[{};{}H", run.row + 1, run.col + 1);
        let _ = write!(feed, "\x1b[38;2;{};{};{}m", run.fg.r, run.fg.g, run.fg.b);
        if let Some(bg) = run.bg {
            let _ = write!(feed, "\x1b[48;2;{};{};{}m", bg.r, bg.g, bg.b);
        }
        let _ = write!(feed, "{}\x1b[0m", run.text);
        stream.feed(feed.as_bytes(), &mut term);
    }
    let mut snapshot = FrameSnapshot::from_terminal(&mut term);
    snapshot.cursor.visible = false;

    renderer.resize(band_size);
    renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
    renderer.set_clear_color(rgb_to_rgba(SIDEBAR_BG));
    renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
    renderer.draw(&gpu.device, &gpu.queue, band_view);

    // Composite the band flat (no rounding/border) onto the surface's left
    // inset. `overlay_texture_cards` loads (doesn't clear) so the panes to the
    // right are untouched.
    let style = CardStyle {
        background: rgb_to_rgba(SIDEBAR_BG),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    card.pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &style,
        &[CardTexturePlacement {
            texture_view: band_view,
            x: 0,
            y: 0,
            w: model.inset,
            h: model.height,
            selected: false,
        }],
    );
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
