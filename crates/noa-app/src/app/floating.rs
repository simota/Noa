//! Floating panes (`docs/specs/floating-panes.md`): wiring the pure model in
//! `crate::floating` to windows, ptys, input and the renderer.
//!
//! A float's `Surface` lives in its tab's `WindowState::surfaces` beside the
//! split panes', so io routing, input, titles and close confirmation treat it
//! as a pane. `WindowState::floats` holds its geometry; the split tree never
//! names it.

use super::*;
use crate::commands::FloatAction;
use crate::floating::{
    FloatAnchor, FloatDragKind, FloatHit, FloatHitPart, FloatLayout, FloatMetrics, FloatState,
    MAX_PER_ANCHOR, PlacedFloat, StripButton,
};

/// Two presses on a title strip within this long toggle collapse (REQ-009).
const STRIP_DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// How long the "limit reached" toast stays up (REQ-019).
const LIMIT_TOAST_DURATION: Duration = Duration::from_millis(1800);
/// Render ids for title strips: a float's pane id with the top bit set, which
/// `PaneId::alloc` never reaches.
const STRIP_RENDER_ID_BIT: u64 = 1 << 63;

/// A float's title strip, kept as a one-row terminal so the cell renderer
/// draws it like any pane.
pub(super) struct FloatStrip {
    terminal: Terminal,
    cols: u16,
    bytes: String,
}

/// The geometry a tab's floats resolve against, owned so it can outlive the
/// `WindowState` borrow it was read from.
pub(super) struct FloatGeometry {
    tab: PaneRectApp,
    window: PaneRectApp,
    panes: Vec<(PaneId, PaneRectApp)>,
    zoomed: Option<PaneId>,
    pub(super) metrics: FloatMetrics,
}

impl FloatGeometry {
    pub(super) fn layout(&self) -> FloatLayout<'_> {
        FloatLayout {
            tab: self.tab,
            window: self.window,
            panes: &self.panes,
            zoomed: self.zoomed,
        }
    }
}

/// One visible float, ready to draw.
pub(super) struct FloatDraw {
    pub(super) placed: PlacedFloat,
    pub(super) focused: bool,
    strip_bytes: String,
    strip_cols: u16,
    strip_bg: noa_core::Rgb,
}

/// What a left press over the float layer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FloatPress {
    /// No float under the pointer.
    Miss,
    /// The press landed on a float's terminal: the ordinary pane press
    /// handling takes it, but nothing under the float may.
    Content,
    /// A title strip, button or resize edge took the press.
    Consumed,
}

/// What is being closed, for counting the programs that end with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseScope {
    /// One tab; the rest of its window stays.
    Tab,
    /// The tab's whole window, or the app.
    Window,
}

/// Whether a tab's window-anchored floats outlive the close: `has_heir` is
/// whether another tab of the window exists to take them.
pub(super) fn window_floats_outlive_close(scope: CloseScope, has_heir: bool) -> bool {
    scope == CloseScope::Tab && has_heir
}

/// Why a tab's floats may have gone out of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HideCause {
    /// The window reported itself occluded.
    Occluded,
    /// Another tab of the same window came to the front.
    SiblingTabFronted,
}

/// REQ-024: whether `cause` takes the keyboard from a focused pane- or
/// tab-anchored float.
pub(super) fn hide_unfocuses_float(cause: HideCause) -> bool {
    // Occlusion also means minimized, covered by another app or on another
    // Space: the float is still where the user left it, and so is its focus.
    cause == HideCause::SiblingTabFronted
}

/// REQ-011: whether a float's strip or edges swallow a button event. The
/// left button has its own handling there (`float_mouse_press`).
/// `owed_release` is whether this is the release of a button some pane saw
/// pressed.
pub(super) fn chrome_swallows_button(
    over_float_chrome: bool,
    left: bool,
    scripted: bool,
    owed_release: bool,
) -> bool {
    over_float_chrome && !left && !scripted && !owed_release
}

/// The pane a press is aimed at: `float_content` is the float whose terminal
/// is under the pointer now, `hovered` what the last pointer motion found.
/// `split_under` is the split pane under the pointer now.
pub(super) fn press_hover(
    float_content: Option<PaneId>,
    split_under: Option<PaneId>,
    hovered: Option<PaneId>,
) -> Option<PaneId> {
    // What is under the pointer now outranks what the last motion found: a
    // keyboard focus change moves the hover without moving the pointer.
    float_content.or(split_under).or(hovered)
}

/// What happens to the "a float's chrome took the left press" flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChromePressEvent {
    /// A left press the chrome took.
    PressConsumed,
    /// Any other left press.
    PressOther,
    /// The left release.
    Release,
    /// Focus loss, occlusion or a tab switch: the release may never come.
    Interrupt,
}

/// The flag after `event`. While it is set, the next left release ends at
/// the float layer.
pub(super) fn chrome_press_flag(event: ChromePressEvent) -> bool {
    // Only the press the chrome took owes a swallowed release. Any other
    // press, or a gesture cut short before its release, clears the debt, so
    // it cannot swallow the release of a later ordinary click.
    event == ChromePressEvent::PressConsumed
}

/// Whether a pane is reachable over IPC: listed, pushed to output
/// subscribers and attachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IpcExposure {
    Listed,
    Hidden,
}

/// REQ-023: what a floating pane gets.
pub(super) fn float_ipc_exposure() -> IpcExposure {
    IpcExposure::Hidden
}

/// The panes that can show a child-exited bar: every pane with a visible
/// terminal.
pub(super) fn child_exited_bar_panes(mut split: Vec<PaneId>, floats: Vec<PaneId>) -> Vec<PaneId> {
    split.extend(floats);
    split
}

/// Whether a pane's scrollbar thumb is drawn: `track` is the strip along
/// the pane's right edge the thumb lives in, `covers` the floats stacked
/// above the pane.
pub(super) fn scroll_thumb_shown(track: PaneRectApp, covers: &[PaneRectApp]) -> bool {
    !covers.iter().any(|cover| {
        cover.x < track.right()
            && track.x < cover.right()
            && cover.y < track.bottom()
            && track.y < cover.bottom()
    })
}

/// What can cut a float drag short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DragInterrupt {
    Occluded,
    TabHidden,
    FocusLost,
}

/// Every one of them: the release may land elsewhere or never arrive, and a
/// drag left standing would follow the next plain pointer motion.
pub(super) fn ends_float_drag(interrupt: DragInterrupt) -> bool {
    match interrupt {
        DragInterrupt::Occluded | DragInterrupt::TabHidden | DragInterrupt::FocusLost => true,
    }
}

/// The return pane after split pane `closed` went away while a float held
/// focus; `next_split` is the closed pane's neighbour.
pub(super) fn return_pane_after_close(
    return_pane: PaneId,
    closed: PaneId,
    next_split: PaneId,
) -> PaneId {
    if return_pane == closed {
        next_split
    } else {
        return_pane
    }
}

/// Whether a left release ends at the float layer: it finished a drag, or
/// its press was taken by a float's strip, button or edge.
pub(super) fn left_release_swallowed(drag_finished: bool, press_consumed: bool) -> bool {
    drag_finished || press_consumed
}

/// The tab that takes over a window's floats: `tabs` are the window's other
/// tabs in order, each with whether it is on screen.
pub(super) fn pick_window_float_heir(tabs: &[(WindowId, bool)]) -> Option<WindowId> {
    tabs.iter()
        .find(|(_, on_screen)| *on_screen)
        .or(tabs.first())
        .map(|(tab, _)| *tab)
}

/// REQ-008: whether focusing a pane again brings it to the front.
pub(super) fn refocus_raises(already_focused: bool, is_float: bool) -> bool {
    already_focused && is_float
}

/// The resize overlay's `(changed, first)` for the focused pane's grid.
pub(super) fn resize_overlay_change(
    previous: Option<(PaneId, (u16, u16))>,
    now: (PaneId, (u16, u16)),
) -> (bool, bool) {
    // A different focused pane is a focus change, not a resize.
    (
        previous.is_some_and(|(pane, grid)| pane == now.0 && grid != now.1),
        previous.is_none(),
    )
}

/// How `focus_pane` reaches its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusRoute {
    /// Take the keyboard as is.
    Direct,
    /// A float that is collapsed or off screen: bring it into view first.
    Reveal,
}

/// `float_accepts_focus` is `None` for a split pane, else whether the float
/// is on screen and expanded.
pub(super) fn focus_route(float_accepts_focus: Option<bool>) -> FocusRoute {
    match float_accepts_focus {
        Some(false) => FocusRoute::Reveal,
        Some(true) | None => FocusRoute::Direct,
    }
}

/// The `(focused_pane, return_pane)` a tab should hold, given the ones it
/// has: `return_pane` always names a live split pane.
pub(super) fn repaired_focus(
    focused: PaneId,
    return_pane: PaneId,
    is_float: impl Fn(PaneId) -> bool,
    is_live: impl Fn(PaneId) -> bool,
    first_leaf: Option<PaneId>,
) -> (PaneId, PaneId) {
    if !is_float(focused) {
        // A split focus that left the tab (its pane moved away) falls back
        // to a pane that is still here.
        let focused = if is_live(focused) {
            focused
        } else {
            first_leaf.unwrap_or(focused)
        };
        (focused, focused)
    } else if is_float(return_pane) || !is_live(return_pane) {
        (focused, first_leaf.unwrap_or(return_pane))
    } else {
        (focused, return_pane)
    }
}

/// The pane a wheel turn goes to.
pub(super) fn wheel_pane(
    scripted: Option<PaneId>,
    hovered: Option<PaneId>,
    focused: PaneId,
    over_float_chrome: bool,
) -> Option<PaneId> {
    if scripted.is_some() {
        return scripted;
    }
    // A float's strip and edges swallow the wheel (REQ-011): without this
    // the fallback would scroll the focused pane under them.
    if over_float_chrome {
        return None;
    }
    Some(hovered.unwrap_or(focused))
}

pub(super) fn strip_render_pane_id(float: PaneId) -> RenderPaneId {
    RenderPaneId::new(float.get() | STRIP_RENDER_ID_BIT)
}

fn float_metrics(metrics: noa_font::Metrics, padding: GridPadding) -> FloatMetrics {
    FloatMetrics {
        cell_w: metrics.cell_w.round().max(1.0) as u32,
        cell_h: metrics.cell_h.round().max(1.0) as u32,
        pad_w: padding.horizontal().round().max(0.0) as u32,
        pad_h: padding.vertical().round().max(0.0) as u32,
        pad_left: padding.left.round().max(0.0) as u32,
    }
}

pub(super) fn float_geometry(
    state: &WindowState,
    bounds: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> FloatGeometry {
    let panes = visible_pane_ids(&state.split_tree, None)
        .into_iter()
        .filter_map(|pane| {
            state
                .surfaces
                .get(&pane)
                .map(|surface| (pane, surface.rect))
        })
        .collect();
    FloatGeometry {
        tab: bounds,
        window: bounds,
        panes,
        zoomed: state.zoomed,
        metrics: float_metrics(metrics, padding),
    }
}

/// The layout targets of every visible, expanded float: its content rect and
/// the grid that fits it. A collapsed or hidden float is absent, so its pty is
/// never resized (CFR-002).
pub(super) fn float_layout_targets(
    state: &WindowState,
    bounds: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> Vec<(PaneId, PaneRectApp, GridSize)> {
    if state.floats.is_empty() {
        return Vec::new();
    }
    let geometry = float_geometry(state, bounds, metrics, padding);
    state
        .floats
        .draw_order(&geometry.layout(), geometry.metrics)
        .into_iter()
        .filter_map(|placed| {
            let content = placed.content?;
            Some((
                placed.id,
                content,
                grid_size_for_pane_rect(content, metrics, padding),
            ))
        })
        .collect()
}

fn anchor_icon(anchor: FloatAnchor) -> char {
    match anchor {
        FloatAnchor::Pane(_) => '\u{25a3}',
        FloatAnchor::Tab => '\u{25a4}',
        FloatAnchor::Window => '\u{25eb}',
    }
}

/// The title strip as VT bytes for a one-row terminal `cols` wide (REQ-002):
/// anchor icon, title, agent state, then the maximize, collapse and close
/// buttons, each two cells wide.
fn strip_bytes(
    cols: u16,
    anchor: FloatAnchor,
    state: FloatState,
    title: &str,
    status: &str,
    bg: noa_core::Rgb,
    fg: noa_core::Rgb,
) -> String {
    let cols = usize::from(cols);
    let maximize = if state == FloatState::Maximized {
        '\u{2750}'
    } else {
        '\u{25a1}'
    };
    let collapse = if state == FloatState::Collapsed {
        '+'
    } else {
        '\u{2013}'
    };
    let buttons = format!(" {maximize} {collapse} \u{00d7}");
    let buttons_at = cols.saturating_sub(6);
    let status_len = status.chars().count();
    let status_at = buttons_at.saturating_sub(status_len + 1);
    let title_room = status_at.saturating_sub(3);
    let title: String = title.chars().take(title_room).collect();
    let mut out = format!(
        "\x1b[?25l\x1b[?7l\x1b[0;48;2;{};{};{};38;2;{};{};{}m\x1b[H\x1b[2K",
        bg.r, bg.g, bg.b, fg.r, fg.g, fg.b
    );
    out.push_str(&format!("{} {title}", anchor_icon(anchor)));
    if status_len > 0 && status_at > 2 {
        out.push_str(&format!("\x1b[1;{}H{status}", status_at + 1));
    }
    if cols >= 8 {
        out.push_str(&format!("\x1b[1;{}H{buttons}", buttons_at + 1));
    }
    out
}

impl App {
    fn window_float_geometry(&self, window_id: WindowId) -> Option<FloatGeometry> {
        let state = self.windows.get(&window_id)?;
        let metrics = self
            .gpu
            .as_ref()?
            .fonts
            .get(self.window_font_px(window_id))
            .metrics();
        Some(float_geometry(
            state,
            self.window_pane_bounds(window_id),
            metrics,
            self.padding,
        ))
    }

    /// `None` for a split pane, else whether float `id` can take the
    /// keyboard as it is.
    pub(super) fn float_accepts_focus(&self, window_id: WindowId, id: PaneId) -> Option<bool> {
        let state = self.windows.get(&window_id)?;
        if !state.is_float(id) {
            return None;
        }
        let geometry = self.window_float_geometry(window_id)?;
        Some(
            state
                .floats
                .accepts_focus(id, &geometry.layout(), geometry.metrics),
        )
    }

    /// Whether `window_id`'s window-anchored floats outlive the tab: another
    /// tab of the same window remains to take them.
    pub(super) fn window_floats_survive_tab(&self, window_id: WindowId) -> bool {
        self.window_floats_survive(window_id, CloseScope::Tab)
    }

    pub(super) fn window_floats_survive(&self, window_id: WindowId, scope: CloseScope) -> bool {
        window_floats_outlive_close(scope, self.window_float_heir(window_id).is_some())
    }

    fn window_float_heir(&self, window_id: WindowId) -> Option<WindowId> {
        let group = self.windows.get(&window_id)?.group;
        let tabs: Vec<(WindowId, bool)> = self
            .window_order
            .iter()
            .filter(|id| **id != window_id)
            .filter_map(|id| {
                let state = self.windows.get(id)?;
                (state.group == group).then_some((*id, !state.occluded && !state.background_tab))
            })
            .collect();
        pick_window_float_heir(&tabs)
    }

    /// The front-most float under `point`, if any.
    pub(super) fn float_hit_at(
        &self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> Option<FloatHit> {
        let state = self.windows.get(&window_id)?;
        if state.floats.is_empty() {
            return None;
        }
        let geometry = self.window_float_geometry(window_id)?;
        state
            .floats
            .hit_test(point, &geometry.layout(), geometry.metrics)
    }

    /// Everything `redraw` needs to draw this tab's floats, back to front.
    pub(super) fn float_draws(&self, window_id: WindowId) -> Vec<FloatDraw> {
        let Some(state) = self.windows.get(&window_id) else {
            return Vec::new();
        };
        if state.floats.is_empty() {
            return Vec::new();
        }
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return Vec::new();
        };
        let palette = crate::chrome::palette();
        state
            .floats
            .draw_order(&geometry.layout(), geometry.metrics)
            .into_iter()
            .map(|placed| {
                let card = self
                    .session_store
                    .get(&Self::session_card_id(window_id, placed.id));
                let title = card
                    .map(|card| card.name_override.as_deref().unwrap_or(&card.name))
                    .filter(|name| !name.is_empty())
                    .unwrap_or("float");
                let status = card.and_then(|card| card.agent_status.as_ref());
                let label = status.map_or("", |status| status.state.label());
                // REQ-014: the strip takes the color the sidebar card's badge
                // takes while its agent needs attention.
                let attention = status.is_some_and(|status| status.state.needs_attention());
                let focused = state.focused_pane == placed.id;
                let (bg, fg) = if attention {
                    (palette.dot_red, palette.bg)
                } else if focused {
                    (palette.card_selected, palette.fg)
                } else {
                    (palette.band, palette.dim_fg)
                };
                let strip_cols = (placed.strip.w.saturating_sub(geometry.metrics.pad_w)
                    / geometry.metrics.cell_w)
                    .clamp(1, u32::from(u16::MAX)) as u16;
                FloatDraw {
                    placed,
                    focused,
                    strip_bytes: strip_bytes(
                        strip_cols,
                        placed.anchor,
                        placed.state,
                        title,
                        label,
                        bg,
                        fg,
                    ),
                    strip_cols,
                    strip_bg: bg,
                }
            })
            .collect()
    }

    pub(super) fn handle_float_action(&mut self, window_id: WindowId, action: FloatAction) {
        if action == FloatAction::New {
            self.new_float(window_id);
            return;
        }
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let focused_float = state
            .is_float(state.focused_pane)
            .then_some(state.focused_pane);
        let layout = geometry.layout();
        let metrics = geometry.metrics;
        match action {
            FloatAction::New => {}
            FloatAction::ToggleFocus => {
                let target = match focused_float {
                    Some(_) => Some(state.split_focus()),
                    None => state.floats.toggle_focus_target(&layout, metrics),
                };
                if let Some(target) = target {
                    self.focus_pane(window_id, target);
                }
            }
            FloatAction::FocusNext | FloatAction::FocusPrevious => {
                let forward = action == FloatAction::FocusNext;
                if let Some(target) =
                    state
                        .floats
                        .cycle_from(focused_float, forward, &layout, metrics)
                {
                    self.focus_pane(window_id, target);
                }
            }
            FloatAction::ToggleCollapse
            | FloatAction::ToggleMaximize
            | FloatAction::CycleAnchor => {
                let Some(target) = state.floats.action_target(focused_float, &layout, metrics)
                else {
                    return;
                };
                match action {
                    FloatAction::ToggleCollapse => self.toggle_float_collapse(window_id, target),
                    FloatAction::ToggleMaximize => self.toggle_float_maximize(window_id, target),
                    _ => self.cycle_float_anchor(window_id, target),
                }
            }
        }
    }

    fn show_float_limit_toast(&mut self, window_id: WindowId) {
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.resize_overlay = Some(Toast {
                text: format!("Floating pane limit reached ({MAX_PER_ANCHOR})"),
                until: Instant::now() + LIMIT_TOAST_DURATION,
                kind: ToastKind::Resize,
            });
            state.window.request_redraw();
        }
    }

    /// REQ-001: a float on the tab's return pane.
    pub(super) fn new_float(&mut self, window_id: WindowId) {
        // REQ-021: the disposable windows hold no floats.
        if self.is_scratch_terminal_window(window_id) || self.is_quick_terminal_window(window_id) {
            return;
        }
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let anchor_pane = state.split_focus();
        if state.floats.count_on(FloatAnchor::Pane(anchor_pane)) >= MAX_PER_ANCHOR {
            self.show_float_limit_toast(window_id);
            return;
        }
        let Some(rect) =
            state
                .floats
                .default_content_rect(anchor_pane, &geometry.layout(), geometry.metrics)
        else {
            return;
        };
        let grid_size = grid_size_for_pane_rect(
            rect,
            gpu.fonts.get(self.window_font_px(window_id)).metrics(),
            self.padding,
        );
        let auto_approve_enabled = state.auto_approve_enabled.clone();
        let redraw_floor = state.redraw_floor.clone();
        let id = PaneId::alloc();
        let surface = match self.spawn_float_surface(
            window_id,
            id,
            grid_size,
            rect,
            self.pane_cwd(window_id, anchor_pane),
            auto_approve_enabled,
            redraw_floor,
        ) {
            Ok(surface) => surface,
            Err(err) => {
                log::warn!("failed to spawn floating pane pty: {err}");
                return;
            }
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            let mut surface = surface;
            surface.shutdown();
            return;
        };
        if state
            .floats
            .create(id, anchor_pane, &geometry.layout(), geometry.metrics)
            .is_err()
        {
            let mut surface = surface;
            surface.shutdown();
            return;
        }
        state.surfaces.insert(id, surface);
        self.focus_pane(window_id, id);
        self.after_float_change(window_id);
    }

    /// Relayout, repaint and persist after a float's geometry, state or
    /// existence changed.
    fn after_float_change(&mut self, window_id: WindowId) {
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        self.request_window_redraw(window_id);
        self.persist_session();
    }

    /// REQ-009.
    pub(super) fn toggle_float_collapse(&mut self, window_id: WindowId, id: PaneId) {
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        let was_focused = state.focused_pane == id;
        let return_pane = state.split_focus();
        match state.floats.toggle_collapse(id) {
            Some(FloatState::Collapsed) => {
                if was_focused {
                    self.focus_pane(window_id, return_pane);
                }
            }
            Some(_) => self.focus_pane(window_id, id),
            None => return,
        }
        self.after_float_change(window_id);
    }

    /// REQ-010.
    pub(super) fn toggle_float_maximize(&mut self, window_id: WindowId, id: PaneId) {
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if state.floats.toggle_maximize(id).is_none() {
            return;
        }
        state.floats.raise(id);
        self.after_float_change(window_id);
    }

    /// REQ-006.
    pub(super) fn cycle_float_anchor(&mut self, window_id: WindowId, id: PaneId) {
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        match state
            .floats
            .cycle_anchor(id, &geometry.layout(), geometry.metrics)
        {
            Ok(Some(_)) => self.after_float_change(window_id),
            Ok(None) => {}
            Err(_) => self.show_float_limit_toast(window_id),
        }
    }

    /// Tear a float down: its pty, its strip, any drag on it. The caller
    /// relayouts.
    fn discard_float(&mut self, window_id: WindowId, id: PaneId) {
        self.end_copy_mode_for_pane(window_id, id);
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id && session.pane_id == id)
        {
            self.search_prompt = None;
        }
        if self.hovered_link == Some((window_id, id)) {
            self.hovered_link = None;
        }
        self.cleanup_ipc_attach_pane(window_id, id);
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if state.floats.remove(id).is_none() {
            return;
        }
        state.float_strips.remove(&id);
        if state.float_drag.is_some_and(|drag| drag.id == id) {
            state.float_drag = None;
        }
        if state.mouse_capture_pane == Some(id) {
            state.mouse_capture_pane = None;
        }
        if state.last_mouse_pane == Some(id) {
            state.last_mouse_pane = None;
        }
        if state.focused_pane == id {
            let losing_preedit = state.end_pane_preedit(id);
            state.focused_pane = state.return_pane;
            state.last_mouse_pane = Some(state.return_pane);
            split_ops::reset_ime_for_focus_switch(&state.window, losing_preedit);
        }
        if let Some(mut surface) = state.surfaces.remove(&id) {
            surface.shutdown();
        }
    }

    /// REQ-012: close one float; focus returns to the split pane.
    pub(super) fn close_float(&mut self, window_id: WindowId, id: PaneId) {
        self.discard_float(window_id, id);
        self.reconcile_session_store();
        self.after_float_change(window_id);
    }

    /// REQ-013: the floats anchored to a closing split pane go with it. The
    /// caller relayouts.
    pub(super) fn discard_pane_floats(&mut self, window_id: WindowId, pane: PaneId) {
        let ids: Vec<PaneId> = self.windows.get(&window_id).map_or_else(Vec::new, |state| {
            state
                .floats
                .floats()
                .iter()
                .filter(|float| float.anchor == FloatAnchor::Pane(pane))
                .map(|float| float.id)
                .collect()
        });
        for id in ids {
            self.discard_float(window_id, id);
        }
    }

    /// REQ-024: a focused float that is no longer on screen, or collapsed,
    /// hands focus to the return pane.
    pub(super) fn sync_float_focus(&mut self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let focused = state.focused_pane;
        if !state.is_float(focused) {
            return;
        }
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return;
        };
        let shown = state
            .floats
            .placed(focused, &geometry.layout(), geometry.metrics)
            .is_some_and(|placed| placed.state != FloatState::Collapsed);
        if !shown {
            let return_pane = state.split_focus();
            self.focus_pane(window_id, return_pane);
        }
    }

    /// A float drag or chrome press in `window_id` is cut short: the release
    /// may land elsewhere or never arrive.
    pub(super) fn interrupt_float_pointer(
        &mut self,
        window_id: WindowId,
        interrupt: DragInterrupt,
    ) {
        if ends_float_drag(interrupt) {
            self.finish_float_drag(window_id);
        }
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.float_press_consumed = chrome_press_flag(ChromePressEvent::Interrupt);
        }
    }

    /// A float's pty: spawned like a split pane's, with the IPC exposure
    /// floats get.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_float_surface(
        &self,
        window_id: WindowId,
        id: PaneId,
        grid_size: GridSize,
        rect: PaneRectApp,
        cwd: Option<String>,
        auto_approve_enabled: Arc<AtomicBool>,
        redraw_floor: crate::io_thread::RedrawFloor,
    ) -> anyhow::Result<Surface> {
        self.spawn_pane_surface_in(
            window_id,
            id,
            grid_size,
            rect,
            cwd,
            auto_approve_enabled,
            redraw_floor,
            false,
            float_ipc_exposure(),
        )
    }

    /// REQ-024: `window_id`'s tab came to the front, so its sibling tabs are
    /// out of view.
    pub(super) fn unfocus_floats_in_sibling_tabs(&mut self, window_id: WindowId) {
        if !hide_unfocuses_float(HideCause::SiblingTabFronted) {
            return;
        }
        let Some(group) = self.windows.get(&window_id).map(|state| state.group) else {
            return;
        };
        let siblings: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(id, state)| **id != window_id && state.group == group)
            .map(|(id, _)| *id)
            .collect();
        for sibling in siblings {
            self.unfocus_float_on_tab_hide(sibling);
        }
    }

    /// REQ-024: leaving a tab drops focus from its pane- and tab-anchored
    /// floats, so coming back lands on the split pane.
    pub(super) fn unfocus_float_on_tab_hide(&mut self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let focused = state.focused_pane;
        let window_anchored = state
            .floats
            .get(focused)
            .is_some_and(|float| float.anchor == FloatAnchor::Window);
        if state.is_float(focused) && !window_anchored {
            let return_pane = state.split_focus();
            self.focus_pane(window_id, return_pane);
        }
    }

    /// A left press at the last pointer position, offered to the float layer
    /// before the split layout (REQ-004, REQ-011).
    pub(super) fn float_mouse_press(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
    ) -> FloatPress {
        let Some(point) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_point)
        else {
            return FloatPress::Miss;
        };
        let Some(hit) = self.float_hit_at(window_id, point) else {
            return FloatPress::Miss;
        };
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return FloatPress::Miss;
        };
        let begin_drag = |app: &mut Self, kind: FloatDragKind| {
            if let Some(state) = app.windows.get_mut(&window_id) {
                state.float_drag = state.floats.begin_drag(
                    hit.id,
                    kind,
                    point,
                    &geometry.layout(),
                    geometry.metrics,
                );
            }
        };
        match hit.part {
            FloatHitPart::Content => return FloatPress::Content,
            FloatHitPart::Resize(edges) => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.floats.raise(hit.id);
                }
                begin_drag(self, FloatDragKind::Resize(edges));
            }
            FloatHitPart::Strip => {
                let now = Instant::now();
                let double = self.windows.get_mut(&window_id).is_some_and(|state| {
                    let double = state
                        .float_strip_click
                        .is_some_and(|(id, at)| id == hit.id && now - at <= STRIP_DOUBLE_CLICK);
                    state.float_strip_click = (!double).then_some((hit.id, now));
                    double
                });
                if double {
                    self.toggle_float_collapse(window_id, hit.id);
                    return FloatPress::Consumed;
                }
                let collapsed = self.windows.get(&window_id).is_some_and(|state| {
                    state
                        .floats
                        .get(hit.id)
                        .is_some_and(|float| float.state == FloatState::Collapsed)
                });
                if collapsed {
                    if let Some(state) = self.windows.get_mut(&window_id) {
                        state.floats.raise(hit.id);
                    }
                } else {
                    self.focus_pane(window_id, hit.id);
                }
                begin_drag(self, FloatDragKind::Move);
            }
            FloatHitPart::Button(StripButton::Close) => {
                self.request_close_pane(event_loop, window_id, hit.id);
            }
            FloatHitPart::Button(StripButton::Collapse) => {
                self.toggle_float_collapse(window_id, hit.id);
            }
            FloatHitPart::Button(StripButton::Maximize) => {
                self.toggle_float_maximize(window_id, hit.id);
            }
            FloatHitPart::Button(StripButton::Anchor) => {
                self.cycle_float_anchor(window_id, hit.id);
            }
        }
        self.request_window_redraw(window_id);
        FloatPress::Consumed
    }

    pub(super) fn drag_active_float(
        &mut self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let Some(drag) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.float_drag)
        else {
            return false;
        };
        let Some(geometry) = self.window_float_geometry(window_id) else {
            return false;
        };
        if let Some(state) = self.windows.get_mut(&window_id) {
            state
                .floats
                .drag_to(&drag, point, &geometry.layout(), geometry.metrics);
        }
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        self.request_window_redraw(window_id);
        true
    }

    pub(super) fn finish_float_drag(&mut self, window_id: WindowId) -> bool {
        let finished = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.float_drag.take())
            .is_some();
        if finished {
            self.persist_session();
        }
        finished
    }

    /// Move `ids`' floats and their `Surface`s from one tab to another.
    fn transfer_floats(
        &mut self,
        source_window: WindowId,
        dest_window: WindowId,
        anchor: FloatAnchor,
    ) {
        if source_window == dest_window {
            return;
        }
        let Some(dest_state) = self.windows.get(&dest_window) else {
            return;
        };
        let dest_auto_approve_enabled = dest_state.auto_approve_enabled.clone();
        let dest_redraw_floor = dest_state.redraw_floor.clone();
        let Some(source_state) = self.windows.get_mut(&source_window) else {
            return;
        };
        let floats = source_state.floats.take_anchor(anchor);
        if floats.is_empty() {
            return;
        }
        let ids: Vec<PaneId> = floats.iter().map(|float| float.id).collect();
        for id in &ids {
            self.end_copy_mode_for_pane(source_window, *id);
            if self
                .search_prompt
                .as_ref()
                .is_some_and(|session| session.window_id == source_window && session.pane_id == *id)
            {
                self.search_prompt = None;
            }
            if self.hovered_link == Some((source_window, *id)) {
                self.hovered_link = None;
            }
        }
        let Some(source_state) = self.windows.get_mut(&source_window) else {
            return;
        };
        let mut surfaces = Vec::with_capacity(ids.len());
        for id in &ids {
            source_state.float_strips.remove(id);
            if source_state.float_drag.is_some_and(|drag| drag.id == *id) {
                source_state.float_drag = None;
            }
            if source_state.mouse_capture_pane == Some(*id) {
                source_state.mouse_capture_pane = None;
            }
            if source_state.last_mouse_pane == Some(*id) {
                source_state.last_mouse_pane = None;
            }
            if source_state.focused_pane == *id {
                let losing_preedit = source_state.end_pane_preedit(*id);
                source_state.focused_pane = source_state.return_pane;
                split_ops::reset_ime_for_focus_switch(&source_state.window, losing_preedit);
            }
            if let Some(mut surface) = source_state.surfaces.remove(id) {
                split_ops::retarget_surface_io(
                    &surface,
                    dest_window,
                    &dest_auto_approve_enabled,
                    &dest_redraw_floor,
                );
                surface.hover_link = None;
                surface.last_mouse_cell = None;
                surface.last_mouse_pixel = None;
                surface.reported_cell = None;
                surface.reported_pixel = None;
                surfaces.push((*id, surface));
            }
        }
        let Some(dest_state) = self.windows.get_mut(&dest_window) else {
            for (_, mut surface) in surfaces {
                surface.shutdown();
            }
            return;
        };
        for (id, surface) in surfaces {
            dest_state.surfaces.insert(id, surface);
            dest_state.renderer.invalidate_pane(render_pane_id(id));
        }
        dest_state.floats.adopt(floats);
        for id in ids {
            self.rekey_moved_pane(source_window, id, dest_window, &dest_auto_approve_enabled);
        }
    }

    /// L2.3: a split pane that moved to another tab takes its floats along.
    /// `tab_closes` is whether `pane` was the source tab's last split pane.
    pub(super) fn move_pane_floats(
        &mut self,
        source_window: WindowId,
        pane: PaneId,
        dest_window: WindowId,
        tab_closes: bool,
    ) {
        let survive = self.window_floats_survive_tab(source_window);
        for anchor in crate::floating::FloatSet::anchors_following_pane(pane, tab_closes, survive) {
            self.transfer_floats(source_window, dest_window, anchor);
        }
    }

    /// L2.3: window-anchored floats show over whichever tab of their window
    /// is on screen. Called when `window_id`'s tab comes to the front.
    pub(super) fn adopt_window_floats(&mut self, window_id: WindowId) {
        let Some(group) = self.windows.get(&window_id).map(|state| state.group) else {
            return;
        };
        let sources: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(id, state)| {
                **id != window_id
                    && state.group == group
                    && state.floats.count_on(FloatAnchor::Window) > 0
            })
            .map(|(id, _)| *id)
            .collect();
        if sources.is_empty() {
            return;
        }
        for source in &sources {
            self.transfer_floats(*source, window_id, FloatAnchor::Window);
            self.relayout_and_resize_window(*source);
        }
        self.relayout_and_resize_window(window_id);
        self.request_window_redraw(window_id);
    }

    /// L2.3: a tab that closes, or leaves for another window, hands its
    /// window-anchored floats to another tab of the window it was in, when
    /// one remains.
    pub(super) fn rehome_window_floats_before_close(&mut self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if state.floats.count_on(FloatAnchor::Window) == 0 {
            return;
        }
        if let Some(heir) = self.window_float_heir(window_id) {
            self.transfer_floats(window_id, heir, FloatAnchor::Window);
            self.relayout_and_resize_window(heir);
            self.request_window_redraw(heir);
        }
    }

    /// REQ-016: make `id` reachable and focus it — unzoom another pane,
    /// expand a collapsed float. A float its anchor is too small to show
    /// leaves focus on that anchor's split pane.
    pub(super) fn reveal_float(&mut self, window_id: WindowId, id: PaneId) {
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        let Some(float) = state.floats.get(id) else {
            return;
        };
        let anchor = float.anchor;
        let collapsed = float.state == FloatState::Collapsed;
        if let FloatAnchor::Pane(pane) = anchor
            && state.zoomed.is_some_and(|zoomed| zoomed != pane)
        {
            state.zoomed = None;
        }
        if collapsed {
            state.floats.toggle_collapse(id);
        }
        self.relayout_and_resize_window(window_id);
        let shown = self
            .window_float_geometry(window_id)
            .is_some_and(|geometry| {
                self.windows.get(&window_id).is_some_and(|state| {
                    state
                        .floats
                        .is_visible(id, &geometry.layout(), geometry.metrics)
                })
            });
        let target = if shown {
            id
        } else {
            match anchor {
                FloatAnchor::Pane(pane) => pane,
                FloatAnchor::Tab | FloatAnchor::Window => self
                    .windows
                    .get(&window_id)
                    .map_or(id, |state| state.split_focus()),
            }
        };
        self.focus_pane(window_id, target);
        self.request_window_redraw(window_id);
    }
}

impl WindowState {
    /// Keep `return_pane` naming a live split pane.
    pub(super) fn sync_return_pane(&mut self) {
        let (focused, return_pane) = repaired_focus(
            self.focused_pane,
            self.return_pane,
            |pane| self.is_float(pane),
            |pane| self.contains_pane(pane),
            visible_pane_ids(&self.split_tree, None).into_iter().next(),
        );
        self.focused_pane = focused;
        self.return_pane = return_pane;
    }

    /// The strip snapshot for `draw`, refreshed only when its text or width
    /// changed.
    pub(super) fn float_strip_snapshot(&mut self, draw: &FloatDraw) -> FrameSnapshot {
        let strip = self
            .float_strips
            .entry(draw.placed.id)
            .or_insert_with(|| FloatStrip {
                terminal: Terminal::new(GridSize::new(draw.strip_cols, 1)),
                cols: draw.strip_cols,
                bytes: String::new(),
            });
        if strip.cols != draw.strip_cols {
            strip.terminal = Terminal::new(GridSize::new(draw.strip_cols, 1));
            strip.cols = draw.strip_cols;
            strip.bytes.clear();
        }
        if strip.bytes != draw.strip_bytes {
            Stream::new().feed(draw.strip_bytes.as_bytes(), &mut strip.terminal);
            strip.bytes.clone_from(&draw.strip_bytes);
        }
        let mut snapshot = FrameSnapshot::from_terminal(&mut strip.terminal);
        snapshot.focused = false;
        snapshot
    }
}

impl FloatDraw {
    pub(super) fn overlay_group(&self) -> noa_render::OverlayGroup {
        let mut panes = vec![strip_render_pane_id(self.placed.id)];
        if self.placed.content.is_some() {
            panes.push(render_pane_id(self.placed.id));
        }
        noa_render::OverlayGroup {
            outer: render_pane_rect(self.placed.outer),
            header: Some((render_pane_rect(self.placed.strip), self.strip_bg)),
            panes,
            focused: self.focused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc_004_strip_lists_icon_title_status_and_three_buttons_in_order() {
        let palette = crate::chrome::palette();
        let bytes = strip_bytes(
            40,
            FloatAnchor::Tab,
            FloatState::Normal,
            "claude",
            "approval needed",
            palette.band,
            palette.fg,
        );
        let mut terminal = Terminal::new(GridSize::new(40, 1));
        Stream::new().feed(bytes.as_bytes(), &mut terminal);
        let snapshot = FrameSnapshot::from_terminal(&mut terminal);
        let row: String = snapshot.rows[0].cells.iter().map(|cell| cell.ch).collect();
        let find = |needle: &str| {
            row.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))
        };
        let order = [
            find("\u{25a4}"),
            find("claude"),
            find("approval needed"),
            find("\u{25a1}"),
            find("\u{2013}"),
            find("\u{00d7}"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
        // The three buttons fill the last six cells, two cells each.
        let cells: Vec<char> = row.chars().collect();
        assert_eq!(
            cells[34..40],
            [' ', '\u{25a1}', ' ', '\u{2013}', ' ', '\u{00d7}']
        );
    }

    #[test]
    fn review_2_focus_left_on_a_moved_pane_falls_back_to_a_live_split_pane() {
        let (a, p, f) = (PaneId::new(1), PaneId::new(2), PaneId::new(3));
        // Pane P left for another tab with its float F; the tab's focus was
        // handed back to P, which is gone.
        let is_float = |_: PaneId| false;
        let is_live = |pane: PaneId| pane == a;
        assert_eq!(repaired_focus(p, p, is_float, is_live, Some(a)), (a, a));
        // A live split focus is left alone, and a focused float keeps focus.
        assert_eq!(repaired_focus(a, p, is_float, is_live, Some(a)), (a, a));
        let is_float = |pane: PaneId| pane == f;
        let is_live = |pane: PaneId| pane == a || pane == f;
        assert_eq!(repaired_focus(f, p, is_float, is_live, Some(a)), (f, a));
        assert_eq!(repaired_focus(f, a, is_float, is_live, Some(a)), (f, a));
    }

    #[test]
    fn review_3_a_collapsed_or_hidden_float_is_revealed_before_it_takes_focus() {
        assert_eq!(focus_route(None), FocusRoute::Direct);
        assert_eq!(focus_route(Some(true)), FocusRoute::Direct);
        assert_eq!(focus_route(Some(false)), FocusRoute::Reveal);
    }

    #[test]
    fn review_6_wheel_over_float_chrome_reaches_no_pane() {
        let (split, float) = (PaneId::new(1), PaneId::new(2));
        // Over a title strip: nothing hovered, and the focused split pane
        // must not scroll.
        assert_eq!(wheel_pane(None, None, split, true), None);
        assert_eq!(wheel_pane(None, Some(float), split, false), Some(float));
        assert_eq!(wheel_pane(None, None, split, false), Some(split));
        // A scripted wheel names its pane whatever the pointer is over.
        assert_eq!(wheel_pane(Some(float), None, split, true), Some(float));
    }

    #[test]
    fn review4_1_a_chrome_press_flag_does_not_outlive_its_gesture() {
        use ChromePressEvent::*;
        assert!(chrome_press_flag(PressConsumed));
        assert!(!chrome_press_flag(Release));
        // The release never came (focus loss, tab switch): the flag must not
        // swallow the release of the next ordinary click.
        assert!(!chrome_press_flag(Interrupt));
        assert!(!chrome_press_flag(PressOther));
    }

    #[test]
    fn review4_2_a_float_is_hidden_from_ipc() {
        assert_eq!(float_ipc_exposure(), IpcExposure::Hidden);
        let tap = crate::app::ipc::hidden_ipc_output_tap();
        assert!(!tap.broadcaster.has_output_subscriber_for(tap.ipc_pane_id));
    }

    #[test]
    fn review4_5_a_float_shows_a_child_exited_bar_like_a_split_pane() {
        let (split, float) = (PaneId::new(1), PaneId::new(2));
        assert_eq!(
            child_exited_bar_panes(vec![split], vec![float]),
            vec![split, float]
        );
    }

    #[test]
    fn review4_6_a_scroll_thumb_under_a_float_is_not_drawn() {
        let track = PaneRectApp::new(982, 0, 18, 600);
        assert!(scroll_thumb_shown(track, &[]));
        assert!(scroll_thumb_shown(
            track,
            &[PaneRectApp::new(100, 100, 300, 200)]
        ));
        // The float covers part of the track.
        assert!(!scroll_thumb_shown(
            track,
            &[PaneRectApp::new(200, 100, 800, 500)]
        ));
    }

    #[test]
    fn review3_3_press_outside_a_float_goes_to_the_split_pane_under_the_pointer() {
        let (split, float) = (PaneId::new(1), PaneId::new(2));
        // The float was created by keyboard, so the stale hover names it;
        // the pointer never moved and sits over the split pane.
        assert_eq!(press_hover(None, Some(split), Some(float)), Some(split));
        assert_eq!(press_hover(Some(float), None, Some(split)), Some(float));
        assert_eq!(press_hover(None, None, Some(float)), Some(float));
    }

    #[test]
    fn review3_4_losing_window_focus_ends_a_float_drag() {
        for interrupt in [
            DragInterrupt::Occluded,
            DragInterrupt::TabHidden,
            DragInterrupt::FocusLost,
        ] {
            assert!(ends_float_drag(interrupt), "{interrupt:?}");
        }
    }

    #[test]
    fn review3_5_closing_another_pane_leaves_a_live_return_pane_alone() {
        let (a, b, c) = (PaneId::new(1), PaneId::new(2), PaneId::new(3));
        assert_eq!(return_pane_after_close(a, c, b), a);
        assert_eq!(return_pane_after_close(c, c, b), b);
    }

    #[test]
    fn review3_6_chrome_lets_through_the_release_a_pane_is_owed() {
        assert!(!chrome_swallows_button(true, false, false, true));
        assert!(chrome_swallows_button(true, false, false, false));
    }

    #[test]
    fn review3_7_release_after_a_consumed_press_stays_in_the_float_layer() {
        assert!(left_release_swallowed(false, true));
        assert!(left_release_swallowed(true, false));
        assert!(!left_release_swallowed(false, false));
    }

    #[test]
    fn review3_8_window_floats_go_to_the_tab_on_screen() {
        let (t1, t2) = (WindowId::from(1), WindowId::from(2));
        assert_eq!(pick_window_float_heir(&[(t1, false), (t2, true)]), Some(t2));
        assert_eq!(
            pick_window_float_heir(&[(t1, false), (t2, false)]),
            Some(t1)
        );
        assert_eq!(pick_window_float_heir(&[]), None);
    }

    #[test]
    fn review2_1_occlusion_alone_keeps_a_floats_focus() {
        assert!(!hide_unfocuses_float(HideCause::Occluded));
        assert!(hide_unfocuses_float(HideCause::SiblingTabFronted));
    }

    #[test]
    fn review2_2_closing_the_window_ends_its_window_floats_too() {
        assert!(window_floats_outlive_close(CloseScope::Tab, true));
        assert!(!window_floats_outlive_close(CloseScope::Tab, false));
        assert!(!window_floats_outlive_close(CloseScope::Window, true));
        assert!(!window_floats_outlive_close(CloseScope::Window, false));
    }

    #[test]
    fn review2_3_float_chrome_swallows_right_and_middle_buttons() {
        assert!(chrome_swallows_button(true, false, false, false));
        // The left button is handled by the chrome itself, a scripted event
        // names its pane, and off the chrome nothing is swallowed.
        assert!(!chrome_swallows_button(true, true, false, false));
        assert!(!chrome_swallows_button(true, false, true, false));
        assert!(!chrome_swallows_button(false, false, false, false));
    }

    #[test]
    fn review2_6_refocusing_a_float_raises_it() {
        assert!(refocus_raises(true, true));
        assert!(!refocus_raises(true, false));
    }

    #[test]
    fn review2_7_press_goes_to_the_float_under_the_pointer_not_the_stale_hover() {
        let (split, float) = (PaneId::new(1), PaneId::new(2));
        assert_eq!(press_hover(Some(float), None, Some(split)), Some(float));
        assert_eq!(press_hover(Some(float), None, None), Some(float));
        assert_eq!(press_hover(None, None, Some(split)), Some(split));
        assert_eq!(press_hover(None, None, None), None);
    }

    #[test]
    fn review2_9_resize_overlay_ignores_a_change_of_focused_pane() {
        let (split, float) = (PaneId::new(1), PaneId::new(2));
        // Focus moved from a 120x40 split pane to an 80x24 float: nothing
        // was resized.
        assert_eq!(
            resize_overlay_change(Some((split, (120, 40))), (float, (80, 24))),
            (false, false)
        );
        assert_eq!(
            resize_overlay_change(Some((split, (120, 40))), (split, (100, 40))),
            (true, false)
        );
        assert_eq!(
            resize_overlay_change(None, (split, (120, 40))),
            (false, true)
        );
    }

    #[test]
    fn strip_render_ids_never_collide_with_pane_ids() {
        let pane = PaneId::new(7);
        assert_ne!(strip_render_pane_id(pane), render_pane_id(pane));
    }
}
