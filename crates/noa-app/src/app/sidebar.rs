//! Session-sidebar subsystem — the `App`-side glue that turns the pure
//! [`crate::session_store`] + [`crate::sidebar`] modules into a live feature:
//! applying io-thread deltas, garbage-collecting torn-down sessions,
//! per-window toggle + grid-first resize, click routing, and the draw path.
//!
//! Everything visual/windowing lives here (not in the two pure modules), so
//! `session_store.rs`/`sidebar.rs` stay GUI-agnostic (NFR-6). The draw path
//! reads only the store and the pure layout — it never locks a `Terminal`
//! (NFR-1/AC-17).

use std::collections::HashSet;
use std::fmt::Write as _;

use noa_core::Rgb;
use noa_render::{OverlayStyle, command_palette_layout};

use super::*;
use crate::session_store::{SessionCard, SessionDelta, StatusDot, status_dot};
use crate::sidebar::{
    AgentKind, CARD_MENU_ITEMS, CardLines, CardRects, HeaderRects, SidebarMetrics, SidebarRect,
    agent_display_name, card_lines, classify_agent, header_status_label, icon_glyph,
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
        SessionDelta::Upsert { .. }
        | SessionDelta::Bell { .. }
        | SessionDelta::Attention { .. } => window_eligible,
        SessionDelta::Remove { .. }
        | SessionDelta::Branch { .. }
        | SessionDelta::Rename { .. }
        | SessionDelta::Process { .. } => true,
    }
}

// Sidebar palette (⚠G: compile-time, no config knob — matches the mockup's dark
// chrome; the terminal panes keep their own theme). Sourced from the shared
// `crate::chrome` palette so the sidebar and the tab overview stay visually
// unified.
const SIDEBAR_BG: Rgb = crate::chrome::CHROME_BG;
const SIDEBAR_FG: Rgb = crate::chrome::CHROME_FG;
const SIDEBAR_DIM_FG: Rgb = crate::chrome::CHROME_DIM_FG;
const SIDEBAR_MENU_BG: Rgb = crate::chrome::CHROME_PILL;
const SIDEBAR_DOT_BLUE: Rgb = crate::chrome::CHROME_DOT_BLUE;
const SIDEBAR_DOT_GREEN: Rgb = crate::chrome::CHROME_DOT_GREEN;
const SIDEBAR_DOT_YELLOW: Rgb = crate::chrome::CHROME_DOT_YELLOW;
/// Attention (FR-16): a program is waiting for the user's reply.
const SIDEBAR_DOT_RED: Rgb = crate::chrome::CHROME_DOT_RED;
/// A card's own background — slightly lighter than the band so each card reads
/// as a distinct rounded surface (mockup parity).
const SIDEBAR_CARD_BG: Rgb = crate::chrome::CHROME_CARD;
/// The selected card's background — brighter still, paired with the accent ring.
const SIDEBAR_CARD_BG_SELECTED: Rgb = crate::chrome::CHROME_CARD_SELECTED;
/// The subtle 1px border stroked around every (unselected) card.
const SIDEBAR_CARD_BORDER: Rgb = crate::chrome::CHROME_BORDER;
/// The accent (focus ring + left edge bar) for the selected card.
const SIDEBAR_ACCENT: Rgb = crate::chrome::CHROME_ACCENT;
/// The rounded header session-name pill background.
const SIDEBAR_PILL_BG: Rgb = crate::chrome::CHROME_BAND;
/// The hairline stroked along the sidebar/pane seam.
const SIDEBAR_DIVIDER: Rgb = crate::chrome::CHROME_DIVIDER;

// Seam treatment between the sidebar band and the terminal panes (logical px,
// scaled at draw time): a soft shadow the band casts rightward plus a crisp
// 1px hairline, so the two independently-themed surfaces meet with depth
// instead of a bare color boundary.
const SEAM_SHADOW_WIDTH: f32 = 10.0;
const SEAM_HAIRLINE_WIDTH: f32 = 1.0;

// Brand accents for recognized AI agents (agent branding). Truecolor, applied
// to the process row + header whenever the process classifies, busy or idle.
const AGENT_CLAUDE_FG: Rgb = Rgb::new(0xd9, 0x77, 0x57); // Anthropic clay
const AGENT_CODEX_FG: Rgb = Rgb::new(0x10, 0xa3, 0x7f); // OpenAI teal
const AGENT_AGY_FG: Rgb = Rgb::new(0x42, 0x85, 0xf4); // Google blue

/// The glyph + accent color + display label for a card's running process. A
/// recognized agent gets its brand glyph/color/name regardless of busy; a
/// generic process keeps the busy/idle dot semantics (green `✳` while running,
/// dim `❯` while idle). Glyphs: `✳` (proven), `◆` for Codex (a hexagon risks
/// tofu), `✦` for agy (a distinct four-point star; `★` is the safe fallback).
fn process_badge(process: &str, busy: bool) -> (String, Rgb) {
    match classify_agent(process) {
        AgentKind::ClaudeCode => (format!("✳ {}", agent_display_name(AgentKind::ClaudeCode, process)), AGENT_CLAUDE_FG),
        AgentKind::Codex => (format!("◆ {}", agent_display_name(AgentKind::Codex, process)), AGENT_CODEX_FG),
        AgentKind::Agy => (format!("✦ {}", agent_display_name(AgentKind::Agy, process)), AGENT_AGY_FG),
        AgentKind::Generic => {
            let glyph = if busy { "✳" } else { "❯" };
            let fg = if busy { SIDEBAR_DOT_GREEN } else { SIDEBAR_DIM_FG };
            (format!("{glyph} {process}"), fg)
        }
    }
}

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

/// The card's status dot with the attention blink applied (FR-A1): while an
/// attention marker is in its hidden phase, show the underlying status (bell /
/// busy / idle) instead of the red attention dot, so the dot blinks red↔status.
/// A settled or visible-phase attention keeps the red dot.
fn effective_status_dot(card: &SessionCard, attention_marker: bool) -> StatusDot {
    if card.attention && !attention_marker {
        if card.unread_bell {
            StatusDot::Yellow
        } else if card.busy {
            StatusDot::Blue
        } else {
            StatusDot::Green
        }
    } else {
        status_dot(card)
    }
}

/// The dot glyph color for a card's status (FR-11), driven by the pure
/// `status_dot` mapping in `session_store` (AC-13).
fn status_dot_rgb(dot: StatusDot) -> Rgb {
    match dot {
        StatusDot::Blue => SIDEBAR_DOT_BLUE,
        StatusDot::Green => SIDEBAR_DOT_GREEN,
        StatusDot::Yellow => SIDEBAR_DOT_YELLOW,
        StatusDot::Red => SIDEBAR_DOT_RED,
    }
}

/// The label appended to a card's process row while it awaits the user's reply
/// (FR-16), e.g. `✳ Claude Code · 応答待ち`.
const ATTENTION_LABEL: &str = "応答待ち";

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
    /// A pending interaction request in its visible blink phase (FR-16/FR-A1):
    /// the card gets a red ring instead of the blue focus ring.
    attention: bool,
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
    /// The scaled height of one card, sizing the per-card scratch texture.
    card_h: u32,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
    cards: Vec<SidebarCardDraw>,
    menu: Option<SidebarMenuDraw>,
}

/// The `SessionWindowId`s belonging to `target` among `pairs` (spec
/// `sidebar-per-window-sessions` R1/R2): a pure, winit-independent derivation
/// so the sidebar's group-scoping can be unit-tested without a window
/// (AC-10). Native tabs share one `WindowGroupId` but have distinct winit
/// `WindowId`s, so this is what makes sibling tabs' sessions show up
/// together in the sidebar.
fn windows_in_group(
    pairs: impl IntoIterator<Item = (SessionWindowId, WindowGroupId)>,
    target: WindowGroupId,
) -> HashSet<SessionWindowId> {
    pairs
        .into_iter()
        .filter(|(_, group)| *group == target)
        .map(|(window_id, _)| window_id)
        .collect()
}

impl App {
    /// The GUI-agnostic card key for a window/pane (NFR-6): winit's stable
    /// `WindowId` ↔ `u64` mapping is the single conversion point, matching what
    /// the io thread posts.
    pub(super) fn session_card_id(window_id: WindowId, pane_id: PaneId) -> SessionCardId {
        SessionCardId::new(SessionWindowId(u64::from(window_id)), pane_id)
    }

    /// The [`SessionWindowId`]s of every tab sharing `window_id`'s logical
    /// window (`WindowGroupId`), for scoping the sidebar to one window
    /// (R1/R2). Empty when `window_id` has no entry in `self.windows` (a
    /// window mid-teardown), degrading to the header-only empty-store draw
    /// path.
    pub(super) fn session_windows_for_window(&self, window_id: WindowId) -> HashSet<SessionWindowId> {
        let Some(target_group) = self.windows.get(&window_id).map(|state| state.group) else {
            return HashSet::new();
        };
        let pairs = self.window_order.iter().filter_map(|id| {
            self.windows
                .get(id)
                .map(|state| (SessionWindowId(u64::from(*id)), state.group))
        });
        windows_in_group(pairs, target_group)
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
        // An agent session's bell is an interaction request, not a generic beep
        // (FR-A3): escalate it to an attention delta before the eligibility gate
        // so it flows through the same path as an OSC 9/777 request.
        let delta = self.escalate_agent_bell(delta);
        if !session_delta_should_apply(&delta, self.window_sidebar_eligible(window_id)) {
            return;
        }
        // Record the blink onset on the false→true attention transition (FR-A1);
        // FR-A7 keeps the existing onset if attention is already pending so a
        // repeat request doesn't restart the blink.
        if let SessionDelta::Attention { id } = &delta
            && self.session_store.get(id).is_none_or(|card| !card.attention)
        {
            self.attention_onset.insert(*id, Instant::now());
            // Re-arm the blink timer on the next `about_to_wait` pass.
            self.attention_blink_deadline = None;
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
        // Bell/attention flags also surface on the tab overview's title band
        // (FR-16), so the flagged pane's tile must re-stamp its label.
        let flags_overview_tile = matches!(
            &delta,
            SessionDelta::Bell { .. } | SessionDelta::Attention { .. }
        );
        let pane_id = delta.id().pane_id;
        self.session_store.apply(delta);
        self.request_sidebar_redraw();
        if flags_overview_tile {
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
            self.request_overview_redraw();
        }
    }

    /// Escalate an agent session's bell to an attention request (FR-A3): a bell
    /// from a card whose foreground process classifies as a known coding agent
    /// (`claude`/`codex`/`agy`/…) means it wants the user, so it becomes an
    /// `Attention` delta; a generic bell is returned unchanged. On the first
    /// escalation of an unfocused window, bounce the Dock once (FR-A5) — no OS
    /// notification, since bells are frequent. Any other delta passes through.
    fn escalate_agent_bell(&self, delta: SessionDelta) -> SessionDelta {
        let SessionDelta::Bell { id } = delta else {
            return delta;
        };
        let process = self.session_store.get(&id).and_then(|card| card.process.clone());
        if !crate::sidebar::bell_escalates_to_attention(process.as_deref()) {
            return delta;
        }
        // Bounce the Dock only on the transition into attention for an unfocused
        // window, so a burst of bells doesn't bounce repeatedly.
        let window_id = WindowId::from(id.window_id.0);
        let already = self.session_store.get(&id).is_some_and(|card| card.attention);
        if !already && self.os_focused != Some(window_id) {
            crate::notification::bounce_dock();
        }
        SessionDelta::Attention { id }
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
        if !self.sidebar_visible {
            return;
        }
        for (window_id, state) in self.windows.iter() {
            if self.window_sidebar_eligible(*window_id) {
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
        // Prune foreground-process probes for torn-down sessions at the same
        // choke point, so a closed pane's dup'd fd is released.
        if let Some(worker) = self.branch_poll.as_ref() {
            worker.retain_process_probes(&live);
        }
        // Drop attention-blink onsets for sessions that no longer exist, so the
        // blink timer can't stay armed for a torn-down card (FR-A1).
        self.attention_onset.retain(|id, _| live.contains(id));
        // An inline rename on a torn-down card has nothing to commit to.
        if self
            .sidebar_rename
            .as_ref()
            .is_some_and(|session| !live.contains(&session.card))
        {
            self.sidebar_rename = None;
        }
    }

    /// Clear the unread-bell and attention flags on every card of a
    /// just-focused window (FR-11/FR-16). Called from the `Focused(true)`
    /// handler. The window's overview tiles re-stamp their labels so a cleared
    /// attention marker disappears from the overview too.
    pub(super) fn clear_session_bell_for_window(&mut self, window_id: WindowId) {
        self.session_store
            .clear_bell_for_window(SessionWindowId(u64::from(window_id)));
        // Drop the blink onsets for this window so the timer disarms and the
        // marker stops (FR-A6). The store already cleared the attention flags.
        let sw = SessionWindowId(u64::from(window_id));
        self.attention_onset.retain(|id, _| id.window_id != sw);
        self.request_sidebar_redraw();
        for pane_id in self.overview_pane_ids_for_window(window_id) {
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        }
        self.request_overview_redraw();
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
            self.sidebar_visible,
            self.window_sidebar_eligible(window_id),
            self.config.sidebar_width * scale,
        );
        inset.round().max(0.0) as u32
    }

    /// The DPR-scaled layout metrics for a window (FR-4): built from the live
    /// scale factor, the same source as [`window_sidebar_inset_px`](Self::window_sidebar_inset_px),
    /// so the card heights and interior offsets scale with the inset. Falls back
    /// to scale 1.0 for an unknown window.
    fn sidebar_metrics(&self, window_id: WindowId) -> SidebarMetrics {
        let scale = self
            .windows
            .get(&window_id)
            .map_or(1.0, |state| state.window.scale_factor() as f32);
        SidebarMetrics::new(scale)
    }

    /// Recompute the app-wide io-thread gate: on while any eligible window
    /// shows its sidebar (Omen T1 — a distinct flag from the overview gate).
    pub(super) fn refresh_sidebar_visible_gate(&self) {
        let any_visible = self.sidebar_visible
            && self
                .windows
                .keys()
                .any(|window_id| self.window_sidebar_eligible(*window_id));
        self.sidebar_visible_gate
            .store(any_visible, std::sync::atomic::Ordering::Relaxed);
    }

    /// Toggle the session sidebar app-wide (FR-4): the sidebar's shown state is
    /// shared across every tab, so one toggle flips all eligible windows at once
    /// and each is grid-first resized to its new pane area (Omen P3/AC-5). A
    /// no-op when the app has no sidebar-eligible window (only a quick terminal).
    pub(super) fn toggle_sidebar(&mut self) {
        // Require at least one eligible window so an all-quick-terminal app can't
        // flip a flag with no visible effect.
        let eligible: Vec<WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|window_id| self.window_sidebar_eligible(*window_id))
            .collect();
        if eligible.is_empty() {
            return;
        }
        self.sidebar_visible = !self.sidebar_visible;
        // Per-window sidebar UI state resets on any visibility change: scroll
        // returns to the top and any open card menu closes.
        for window_id in &eligible {
            if let Some(state) = self.windows.get_mut(window_id) {
                state.sidebar_scroll = 0;
                state.sidebar_menu = None;
            }
        }
        // A toggle invalidates any inline rename (the editor is a sidebar surface).
        self.sidebar_rename = None;

        self.refresh_sidebar_visible_gate();
        // Grid-first for every eligible tab: `relayout_and_resize_window` applies
        // the inset then routes through `pane_resize_batch_plan` (grid resize
        // before pty winsize).
        for window_id in &eligible {
            self.relayout_and_resize_window(*window_id);
            if let Some(state) = self.windows.get(window_id) {
                state.window.request_redraw();
            }
        }
        if let Some(focused) = self.focused {
            self.update_focused_ime_cursor_area(focused);
        }
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
        // Any sidebar click while an inline rename is open cancels it (mirrors
        // the `…` popup's click-anywhere dismissal); the click still routes.
        self.cancel_sidebar_rename();
        let metrics = self.sidebar_metrics(window_id);

        // An open card `…` menu takes the click first: an item hit runs the
        // action, anything else dismisses the popup (and falls through to normal
        // routing so the same click still selects/scrolls). Remember which card
        // was dismissed so a click on its own `…` button doesn't immediately
        // reopen the menu it just closed (a toggle-then-retoggle).
        let mut dismissed_menu: Option<SessionCardId> = None;
        if let Some(open) = self.windows.get(&window_id).and_then(|s| s.sidebar_menu) {
            if let Some(anchor) = self.card_menu_anchor(window_id, open) {
                let popup = metrics.card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), inset);
                // Mirror the draw-side guard (`sidebar_draw_model` skips a popup
                // whose `bottom() > height`): a popup that would spill past the
                // window bottom is never rendered, so a click in its invisible
                // region must not fire an item — fall through to dismiss instead.
                let height = self
                    .windows
                    .get(&window_id)
                    .map_or(0, |s| s.window.inner_size().height);
                if popup.bottom() <= height
                    && let Some(item) = metrics.card_menu_hit_test(popup, point)
                {
                    self.close_sidebar_menu(window_id);
                    self.activate_card_menu_item(event_loop, window_id, open, item);
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
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        match metrics.hit_test(bounds, &ids, scroll, point) {
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
    /// choke point (AC-9b). `Rename` opens the inline name editor on the card,
    /// bound to `window_id` — the window whose sidebar the menu was clicked in.
    fn activate_card_menu_item(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        card: SessionCardId,
        item: crate::sidebar::CardMenuItem,
    ) {
        match item {
            crate::sidebar::CardMenuItem::Close => {
                let target_window = WindowId::from(card.window_id.0);
                self.request_close_pane(event_loop, target_window, card.pane_id);
            }
            crate::sidebar::CardMenuItem::Rename => self.start_sidebar_rename(window_id, card),
        }
    }

    /// Open the inline rename editor on `card` (FR-7 Rename), seeded with its
    /// current display name so a small correction doesn't require retyping.
    fn start_sidebar_rename(&mut self, window_id: WindowId, card: SessionCardId) {
        let buffer = self
            .session_store
            .get(&card)
            .map(|c| c.display_name().to_string())
            .unwrap_or_default();
        self.sidebar_rename = Some(SidebarRenameSession {
            window_id,
            card,
            buffer,
        });
        self.request_sidebar_redraw();
    }

    /// One keystroke for the open inline rename (FR-7 Rename): printable text
    /// appends, Backspace pops, Enter commits a non-empty trimmed name as a
    /// [`SessionDelta::Rename`] (an all-whitespace buffer cancels instead, so a
    /// card can't end up unnamed), Escape cancels. Everything is consumed —
    /// the session is modal for its window's keyboard.
    pub(super) fn handle_sidebar_rename_key(&mut self, event: &KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.cancel_sidebar_rename();
            }
            Key::Named(NamedKey::Enter) => {
                let Some(session) = self.sidebar_rename.take() else {
                    return;
                };
                let name = session.buffer.trim().to_string();
                if !name.is_empty() {
                    self.session_store.apply(SessionDelta::Rename {
                        id: session.card,
                        name,
                    });
                }
                self.request_sidebar_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.sidebar_rename.as_mut() {
                    session.buffer.pop();
                }
                self.request_sidebar_redraw();
            }
            _ => {
                // Cmd/Ctrl/Alt combos are not text; swallow them (modal) but
                // don't edit the buffer.
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return;
                }
                let Some(text) = event.text.as_deref() else {
                    return;
                };
                let mut appended = false;
                if let Some(session) = self.sidebar_rename.as_mut() {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        session.buffer.push(c);
                        appended = true;
                    }
                }
                if appended {
                    self.request_sidebar_redraw();
                }
            }
        }
    }

    /// Drop the open inline rename without committing, repainting so the
    /// original name returns.
    pub(super) fn cancel_sidebar_rename(&mut self) {
        if self.sidebar_rename.take().is_some() {
            self.request_sidebar_redraw();
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
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        let layout = self
            .sidebar_metrics(window_id)
            .layout(bounds, &ids, state.sidebar_scroll);
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
        let metrics = SidebarMetrics::new(state.window.scale_factor() as f32);
        let viewport_h = metrics.bands(bounds).viewport.h;
        let windows = self.session_windows_for_window(window_id);
        let content_h = metrics.content_height(self.session_store.ordered_ids_for_windows(&windows).len());
        let step = metrics.card_stride as f32;
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
        // The sidebar rasterizes with its own dedicated, smaller font, so cell
        // placement uses that font's metrics (not the terminal font's).
        let metrics = gpu.sidebar_font.metrics();
        let scale = state.window.scale_factor() as f32;
        let layout_metrics = SidebarMetrics::new(scale);
        let height = state.window.inner_size().height.max(1);
        let band = PaneRectApp::new(0, 0, inset, height);
        let grid = grid_size_for_pane_rect(band, metrics, self.padding);

        let bounds = SidebarRect::new(0, 0, inset, height);
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        let layout = layout_metrics.layout(bounds, &ids, state.sidebar_scroll);
        let bands = layout_metrics.bands(bounds);

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
        let header: HeaderRects = layout_metrics.header_rects(bands.header);
        // The status label's precedence: any pending attention shows its count
        // (a request whose card scrolled out of the viewport must still be
        // noticeable, FR-16); else a recognized agent / busy process on the
        // focused session shows its badge; else the Idle/Running summary.
        let (busy_count, attention_count) = self.session_store.counts_for_windows(&windows);
        let (status_text, status_fg) = if attention_count > 0 {
            (format!("● {attention_count} {ATTENTION_LABEL}"), SIDEBAR_DOT_RED)
        } else {
            match self.session_store.get(&selected_id) {
                Some(card)
                    if card.busy
                        || card
                            .process
                            .as_deref()
                            .is_some_and(|p| classify_agent(p) != AgentKind::Generic) =>
                {
                    let process = card.process.clone().unwrap_or_else(|| "running".to_string());
                    process_badge(&process, card.busy)
                }
                _ => (header_status_label(busy_count), SIDEBAR_FG),
            }
        };
        runs.extend(window_run(&band_cell, header.status_label, status_text, status_fg, false));
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
        let home = std::env::var("HOME").ok();
        let palette = &gpu.theme.palette;
        let mut cards: Vec<SidebarCardDraw> = Vec::new();
        let card_band = PaneRectApp::new(0, 0, inset, layout_metrics.card_h);
        let card_grid = grid_size_for_pane_rect(card_band, metrics, self.padding);
        let card_cell = to_cell(card_grid);
        for card_rects in &layout.cards {
            let Some(card) = self.session_store.get(&card_rects.id) else {
                continue;
            };
            let lines: CardLines = card_lines(card, now, home.as_deref());
            let marker = self.attention_marker_visible(&card_rects.id);
            let renaming = self
                .sidebar_rename
                .as_ref()
                .filter(|session| session.window_id == window_id && session.card == card_rects.id)
                .map(|session| session.buffer.as_str());
            let full = card_rects.bounds.h == layout_metrics.card_h;
            // A fully-visible card is covered by its opaque rounded overlay, so
            // its backdrop text would never show — only emit it for partial
            // (edge-clipped) cards, which have no overlay.
            if !full {
                emit_card_text(
                    &mut runs, card_rects, card, &lines, &band_cell, marker, palette, renaming,
                );
            }

            if full {
                let selected = card_rects.id == selected_id;
                let local = layout_metrics.card_local_rects(card_rects.id, inset);
                let mut card_runs = Vec::new();
                emit_card_text(
                    &mut card_runs, &local, card, &lines, &card_cell, marker, palette, renaming,
                );
                cards.push(SidebarCardDraw {
                    rect: card_rects.bounds,
                    grid: card_grid,
                    bg: if selected {
                        SIDEBAR_CARD_BG_SELECTED
                    } else {
                        SIDEBAR_CARD_BG
                    },
                    selected,
                    attention: card.attention && marker,
                    runs: card_runs,
                });
            }
        }

        // Card `…` menu popup (FR-7): its own overlay, composited above the cards
        // so a rounded card can never hide it. Skipped when the open card has
        // scrolled out of view or the popup would spill past the window bottom.
        let menu = state.sidebar_menu.and_then(|open| {
            let card_rects = layout.cards.iter().find(|c| c.id == open)?;
            let popup =
                layout_metrics.card_menu_popup_rect(card_rects.menu_button, CARD_MENU_ITEMS.len(), inset);
            if popup.w == 0 || popup.h == 0 || popup.bottom() > height {
                return None;
            }
            let menu_band = PaneRectApp::new(0, 0, popup.w, popup.h);
            let menu_grid = grid_size_for_pane_rect(menu_band, metrics, self.padding);
            let menu_cell = to_cell(menu_grid);
            let mut menu_runs = Vec::new();
            for (index, &item) in CARD_MENU_ITEMS.iter().enumerate() {
                let item_rect = layout_metrics.card_menu_item_rect(popup, index);
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
            card_h: layout_metrics.card_h,
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

/// A truecolor SGR foreground prefix, embeddable inside a run's text (the run
/// text is fed through a `Stream`, so inline escapes recolor mid-run).
fn sgr_fg(color: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", color.r, color.g, color.b)
}

/// Resolve a preview span's pure `noa_core::Color` to a concrete sidebar RGB:
/// the theme palette for indexed colors, the raw value for truecolor, and the
/// sidebar's dim fg for the default (so uncolored output reads as secondary
/// text on the card).
fn resolve_preview_color(color: noa_core::Color, palette: &[Rgb; 256]) -> Rgb {
    match color {
        noa_core::Color::Default => SIDEBAR_DIM_FG,
        noa_core::Color::Palette(index) => palette[index as usize],
        noa_core::Color::Rgb(rgb) => rgb,
    }
}

/// Emit one card's text runs (status dot, project icon, bold name, cwd, the
/// meta row `process · ⎇ branch`, two color-run preview rows, updated-time)
/// through `to_cell`. Shared by the flat backdrop (window coords) and each
/// rounded overlay (card-local coords) so both agree on layout. `renaming`
/// carries the live rename buffer when this card's inline rename is open —
/// it replaces the name run with the buffer + caret in the accent color.
#[allow(clippy::too_many_arguments)]
fn emit_card_text(
    out: &mut Vec<SidebarTextRun>,
    rects: &CardRects,
    card: &SessionCard,
    lines: &CardLines,
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    attention_marker: bool,
    palette: &[Rgb; 256],
    renaming: Option<&str>,
) {
    out.extend(window_run(
        to_cell,
        rects.dot,
        "●".to_string(),
        status_dot_rgb(effective_status_dot(card, attention_marker)),
        false,
    ));
    out.extend(window_run(
        to_cell,
        rects.icon,
        icon_glyph(card.icon).to_string(),
        icon_color(card.icon),
        false,
    ));
    let (name_text, name_fg) = match renaming {
        // Inline rename (FR-7): the buffer plus a caret, in the accent color.
        Some(buffer) => (format!("{buffer}▏"), SIDEBAR_ACCENT),
        None => (lines.name.clone(), SIDEBAR_FG),
    };
    out.extend(window_run(to_cell, rects.name_line, name_text, name_fg, true));
    out.extend(window_run(
        to_cell,
        rects.cwd_line,
        lines.cwd.clone(),
        SIDEBAR_DIM_FG,
        false,
    ));

    // Meta row: a recognized AI agent gets its brand glyph/color/name (busy or
    // idle); any other process shows green `✳` while running, dim `❯` while
    // idle. The git branch follows on the same row, dim. A pending interaction
    // request (FR-16) overrides the badge with the attention color and appends
    // the waiting label; the treatment blinks (FR-A1) via `attention_marker`.
    if rects.meta.w > 0 && rects.meta.h > 0 {
        let (badge, badge_fg) = process_badge(&lines.process, card.busy);
        let (badge, badge_fg) = if card.attention && attention_marker {
            (format!("{badge} · {ATTENTION_LABEL}"), SIDEBAR_DOT_RED)
        } else {
            (badge, badge_fg)
        };
        let text = if lines.branch.is_empty() {
            badge
        } else {
            // The dim branch suffix is recolored inline; the run fg colors the
            // badge portion.
            format!("{badge}{} · ⎇ {}", sgr_fg(SIDEBAR_DIM_FG), lines.branch)
        };
        out.extend(window_run(to_cell, rects.meta, text, badge_fg, false));
    }

    // Last-output preview rows (up to 2), in their original ANSI colors: each
    // span is recolored inline via an embedded SGR prefix, so one run carries
    // the whole line. Rows the card has no preview line for stay blank.
    for (rect, line) in [
        (rects.preview1, card.preview.first()),
        (rects.preview2, card.preview.get(1)),
    ] {
        let Some(line) = line else { continue };
        let mut text = String::new();
        for span in line {
            text.push_str(&sgr_fg(resolve_preview_color(span.fg, palette)));
            text.push_str(&span.text);
        }
        let fg = line
            .first()
            .map(|span| resolve_preview_color(span.fg, palette))
            .unwrap_or(SIDEBAR_DIM_FG);
        out.extend(window_run(to_cell, rect, text, fg, false));
    }

    out.extend(window_run(
        to_cell,
        rects.updated,
        lines.updated.clone(),
        SIDEBAR_DIM_FG,
        false,
    ));
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

/// Corner radius (logical px) of the command-palette card (H).
const PALETTE_CARD_CORNER_RADIUS: f32 = 10.0;
/// Outer soft drop-shadow width (logical px) of the palette card (H).
const PALETTE_CARD_GLOW_WIDTH: f32 = 12.0;

/// Composite the open command palette as a single rounded card over the focused
/// pane (H). The block (query row + windowed list) is rasterized into a scratch
/// texture by the reused `palette_renderer`, then drawn as one rounded card:
/// a soft black drop shadow, the elevated surface, and a themed 1px border —
/// two card-pipeline passes (shadow+fill, then fill+border) over the same
/// texture. Runs inline in `redraw` after the panes and sidebar so the modal
/// always draws on top. The overlay's own square outline is dropped (the card
/// supplies the chrome); the hairline rule and accent bar ride inside the
/// texture.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_command_palette_card(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    palette: &CommandPaletteSnapshot,
    pane_rect: PaneRect,
    pane_cols: u16,
    pane_rows: u16,
    padding: GridPadding,
    scale: f32,
) {
    let Some(layout) = command_palette_layout(palette, pane_cols, pane_rows) else {
        return;
    };
    let metrics = gpu.font.metrics();
    let (cell_w, cell_h) = (metrics.cell_w, metrics.cell_h);
    let block_px = PixelSize {
        w: ((layout.block_cols as f32) * cell_w).ceil().max(1.0) as u32,
        h: ((layout.block_rows as f32) * cell_h).ceil().max(1.0) as u32,
    };

    // Lazily (re)build the reused block renderer + card pipeline for this format.
    // The block renderer uses zero padding so grid cell (c,r) maps to texture
    // pixel (c*cell_w, r*cell_h), making the scratch exactly the block size.
    if gpu
        .palette_renderer
        .as_ref()
        .is_none_or(|renderer| renderer.target_format() != surface_format)
    {
        gpu.palette_renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            surface_format,
            &mut gpu.font,
            GridPadding::new(0.0, 0.0, 0.0, 0.0),
        )
        .ok();
    }
    if gpu
        .palette_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.palette_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            pipeline: CardPipeline::new(&gpu.device, surface_format),
        });
    }
    ensure_scratch(
        &mut gpu.palette_scratch,
        &gpu.device,
        block_px,
        surface_format,
        "noa-command-palette",
    );
    if gpu.palette_renderer.is_none()
        || gpu.palette_card.is_none()
        || gpu.palette_scratch.is_none()
    {
        return;
    }

    // Rasterize the windowed block (rows sliced to the visible window, selection
    // rebased) into the scratch texture. The block fills the mini grid exactly,
    // so the overlay draws at the mini grid's origin.
    let visible = &palette.rows[layout.offset..layout.offset + layout.shown];
    let mini = CommandPaletteSnapshot {
        query: palette.query.clone(),
        rows: visible.to_vec(),
        selected: palette.selected.saturating_sub(layout.offset),
        total_entries: palette.total_entries,
    };
    let style = OverlayStyle::from_theme(&gpu.theme);
    {
        let mut term = Terminal::new(GridSize::new(layout.block_cols, layout.block_rows));
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;
        snapshot.command_palette = Some(mini);
        let scratch_view = &gpu.palette_scratch.as_ref().unwrap().2;
        let renderer = gpu.palette_renderer.as_mut().unwrap();
        renderer.resize(block_px);
        renderer.set_clear_color(style.surface_bg());
        renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        renderer.draw(&gpu.device, &gpu.queue, scratch_view);
    }

    // Card placement in window pixels: the block's grid origin within the pane,
    // offset by the pane's screen origin and the grid padding.
    let x = (pane_rect.x as f32 + padding.left + (layout.x0 as f32) * cell_w)
        .round()
        .max(0.0) as u32;
    let y = (pane_rect.y as f32 + padding.top + (layout.y0 as f32) * cell_h)
        .round()
        .max(0.0) as u32;
    let placement = |selected| CardTexturePlacement {
        texture_view: &gpu.palette_scratch.as_ref().unwrap().2,
        x,
        y,
        w: block_px.w,
        h: block_px.h,
        selected,
    };

    // Pass 1: fill + soft black drop shadow (selected → the shader's glow path).
    let shadow_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, 1.0],
        corner_radius: PALETTE_CARD_CORNER_RADIUS * scale,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: PALETTE_CARD_GLOW_WIDTH * scale,
    };
    // Pass 2: fill + themed 1px border, no glow (unselected → the border path).
    let border = style.border();
    let border_style = CardStyle {
        background: [0.0; 4],
        border_color: border,
        focus_color: border,
        corner_radius: PALETTE_CARD_CORNER_RADIUS * scale,
        border_width: 1.0 * scale,
        focus_width: 1.0 * scale,
        focus_glow_width: 0.0,
    };
    let card = &gpu.palette_card.as_ref().unwrap().pipeline;
    card.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &shadow_style,
        &[placement(true)],
    );
    card.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &border_style,
        &[placement(false)],
    );
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
            Renderer::new(&gpu.device, &gpu.queue, surface_format, &mut gpu.sidebar_font, padding).ok();
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
                h: model.card_h,
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
    // clear) so the panes to the right are untouched. The placement is drawn
    // `selected` with a black focus color and zero focus stroke, which turns
    // the card shader's outer glow into a soft shadow the band casts onto the
    // panes — the seam's depth cue (its crisp line is the hairline below).
    {
        let band_view = &gpu.sidebar_band.as_ref().unwrap().2;
        rasterize_runs(
            gpu.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
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
        focus_color: [0.0, 0.0, 0.0, 1.0],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: SEAM_SHADOW_WIDTH * model.scale,
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
            selected: true,
        }],
    );

    // 1b) Hairline divider over the band's rightmost pixel(s): a solid
    // `SIDEBAR_DIVIDER` strip that gives the seam a crisp edge against the
    // pane background (the terminal keeps its own theme, so the two surfaces
    // otherwise meet as unrelated colors).
    let hairline_w = (SEAM_HAIRLINE_WIDTH * model.scale).round().max(1.0) as u32;
    if model.inset > hairline_w {
        ensure_scratch(
            &mut gpu.sidebar_divider_tex,
            &gpu.device,
            PixelSize {
                w: hairline_w,
                h: model.height,
            },
            surface_format,
            "noa-sidebar-divider",
        );
        if let Some((_, _, divider_view)) = gpu.sidebar_divider_tex.as_ref() {
            rasterize_runs(
                gpu.sidebar_renderer.as_mut().unwrap(),
                &gpu.device,
                &gpu.queue,
                &mut gpu.sidebar_font,
                &gpu.theme,
                divider_view,
                PixelSize {
                    w: hairline_w,
                    h: model.height,
                },
                GridSize { cols: 1, rows: 1 },
                SIDEBAR_DIVIDER,
                &[],
            );
            let divider_style = CardStyle {
                background: rgb_to_rgba(SIDEBAR_DIVIDER),
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
                &divider_style,
                &[CardTexturePlacement {
                    texture_view: divider_view,
                    x: model.inset - hairline_w,
                    y: 0,
                    w: hairline_w,
                    h: model.height,
                    selected: false,
                }],
            );
        }
    }

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
    // A card whose attention marker is in its visible blink phase swaps the
    // blue focus accent for a red ring (FR-16/FR-A1) — drawn selected so the
    // ring + glow path lights up even when the card isn't the focused one.
    let attention_style = CardStyle {
        focus_color: rgb_to_rgba(SIDEBAR_DOT_RED),
        ..card_style
    };
    for card_draw in &model.cards {
        let Some((_, _, card_view)) = gpu.sidebar_card_tex.as_ref() else {
            break;
        };
        rasterize_runs(
            gpu.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            &gpu.theme,
            card_view,
            PixelSize {
                w: model.inset,
                h: model.card_h,
            },
            card_draw.grid,
            card_draw.bg,
            &card_draw.runs,
        );
        let (style, selected) = if card_draw.attention {
            (&attention_style, true)
        } else {
            (&card_style, card_draw.selected)
        };
        gpu.sidebar_card.as_ref().unwrap().pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            view,
            surface_size,
            style,
            &[CardTexturePlacement {
                texture_view: &gpu.sidebar_card_tex.as_ref().unwrap().2,
                x: card_draw.rect.x,
                y: card_draw.rect.y,
                w: card_draw.rect.w,
                h: card_draw.rect.h,
                selected,
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
            &mut gpu.sidebar_font,
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
        assert!(!session_delta_should_apply(
            &SessionDelta::Attention { id },
            false
        ));
        assert!(session_delta_should_apply(&SessionDelta::Remove { id }, false));
    }

    // AC-10 (R2): windows_in_group returns exactly the target group's
    // SessionWindowIds from a pair list mixing multiple groups and multiple
    // tabs (distinct WindowIds) per group.
    #[test]
    fn windows_in_group_returns_only_the_target_groups_windows() {
        let group_a = WindowGroupId(1);
        let group_b = WindowGroupId(2);
        let pairs = [
            (SessionWindowId(10), group_a),
            (SessionWindowId(11), group_a), // sibling tab of window 10
            (SessionWindowId(20), group_b),
            (SessionWindowId(21), group_b),
        ];

        let result = windows_in_group(pairs, group_a);
        assert_eq!(
            result,
            [SessionWindowId(10), SessionWindowId(11)].into_iter().collect()
        );
    }
}
