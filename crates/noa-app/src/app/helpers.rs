use super::*;
#[cfg(all(test, target_os = "macos"))]
use winit::platform::macos::OptionAsAlt;

pub(super) const MIN_RUNTIME_FONT_SIZE: f32 = 6.0;
pub(super) const MAX_RUNTIME_FONT_SIZE: f32 = 96.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RuntimeFontSizeUpdate {
    pub(super) point_size: f32,
    pub(super) changed: bool,
}

pub(super) fn runtime_font_size_update(
    current: f32,
    startup: f32,
    action: FontSizeAction,
) -> RuntimeFontSizeUpdate {
    let requested = match action {
        FontSizeAction::Increase => current + 1.0,
        FontSizeAction::Decrease => current - 1.0,
        FontSizeAction::Reset => startup,
    };
    let point_size = clamp_runtime_font_size(requested);
    RuntimeFontSizeUpdate {
        point_size,
        changed: !current.is_finite() || (point_size - current).abs() > f32::EPSILON,
    }
}

pub(super) fn clamp_runtime_font_size(point_size: f32) -> f32 {
    if point_size.is_finite() {
        point_size.clamp(MIN_RUNTIME_FONT_SIZE, MAX_RUNTIME_FONT_SIZE)
    } else {
        MIN_RUNTIME_FONT_SIZE
    }
}

#[cfg(test)]
fn font_size_resize_plan<Id: Copy>(
    windows: impl IntoIterator<Item = (Id, PhysicalSize<u32>)>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> Vec<(Id, GridSize)> {
    windows
        .into_iter()
        .map(|(id, size)| (id, grid_size_for_physical_size(size, metrics, padding)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneResizeAction<Id> {
    GridResize(Id, GridSize),
    PtyResize(Id, GridSize),
}

fn pane_resize_batch_plan<Id: Copy>(
    panes: impl IntoIterator<Item = (Id, GridSize)>,
) -> Vec<PaneResizeAction<Id>> {
    let panes = panes.into_iter().collect::<Vec<_>>();
    let mut plan = Vec::with_capacity(panes.len().saturating_mul(2));
    plan.extend(
        panes
            .iter()
            .map(|(pane_id, grid_size)| PaneResizeAction::GridResize(*pane_id, *grid_size)),
    );
    plan.extend(
        panes
            .iter()
            .map(|(pane_id, grid_size)| PaneResizeAction::PtyResize(*pane_id, *grid_size)),
    );
    plan
}

/// Pixel metrics for `XTWINOPS` reports (`CSI 14/16 t`). Derived from the
/// same `rect`/`padding` the caller already used to compute this pane's
/// `GridSize` (via `grid_size_for_pane_rect`) — not reconstructed
/// independently as `cell_w × cols`, which would drift from `rect` whenever
/// the pane's pixel size isn't an exact multiple of the cell size.
pub(super) fn pixel_metrics_for_pane(
    rect: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> (u32, u32, u32, u32) {
    let cell_w_px = metrics.cell_w.round().max(0.0) as u32;
    let cell_h_px = metrics.cell_h.round().max(0.0) as u32;
    let text_area_w_px = (rect.w as f32 - padding.horizontal()).max(0.0).round() as u32;
    let text_area_h_px = (rect.h as f32 - padding.vertical()).max(0.0).round() as u32;
    (cell_w_px, cell_h_px, text_area_w_px, text_area_h_px)
}

pub(super) fn apply_pane_resize_batch(
    state: &mut WindowState,
    targets: &[(PaneId, PaneRectApp, GridSize)],
    metrics: noa_font::Metrics,
    padding: GridPadding,
) {
    let plan = pane_resize_batch_plan(
        targets
            .iter()
            .map(|(pane_id, _, grid_size)| (*pane_id, *grid_size)),
    );

    for action in &plan {
        let PaneResizeAction::GridResize(pane_id, grid_size) = *action else {
            continue;
        };
        let Some(surface) = state.surfaces.get_mut(&pane_id) else {
            continue;
        };
        let rect = targets
            .iter()
            .find(|(target, _, _)| *target == pane_id)
            .map(|(_, rect, _)| *rect);
        if let Some(rect) = rect {
            surface.rect = rect;
        }
        surface.grid_size = grid_size;
        let mut terminal = surface.terminal.lock().expect("terminal mutex poisoned");
        terminal.resize(grid_size);
        if let Some(rect) = rect {
            let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, padding);
            terminal.set_pixel_metrics(cw, ch, taw, tah);
        }
    }

    for action in plan {
        let PaneResizeAction::PtyResize(pane_id, grid_size) = action else {
            continue;
        };
        if let Some(surface) = state.surfaces.get(&pane_id) {
            let _ = surface.resize_tx.send(grid_size);
        }
    }
}

pub(super) fn shutdown_pane_io_threads<'a>(surfaces: impl IntoIterator<Item = &'a mut Surface>) {
    for surface in surfaces {
        surface.shutdown();
    }
}

pub(super) fn surface_has_running_program(surface: &Surface) -> bool {
    surface
        .terminal
        .lock()
        .expect("terminal mutex poisoned")
        .has_running_program()
}

pub(super) fn running_program_count<'a>(surfaces: impl IntoIterator<Item = &'a Surface>) -> usize {
    surfaces
        .into_iter()
        .filter(|surface| surface_has_running_program(surface))
        .count()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseConfirmTarget {
    Pane,
    Session,
    Window,
    App,
}

pub(super) fn close_confirm_message(target: CloseConfirmTarget, running_programs: usize) -> String {
    match target {
        CloseConfirmTarget::Pane => {
            "A program is still running in this pane. Close it?".to_string()
        }
        CloseConfirmTarget::Session => {
            close_confirm_plural(running_programs, "this session", "Close this session?")
        }
        CloseConfirmTarget::Window => {
            close_confirm_plural(running_programs, "this window", "Close this window?")
        }
        CloseConfirmTarget::App => close_confirm_plural(running_programs, "noa", "Quit noa?"),
    }
}

pub(super) fn close_confirm_plural(running_programs: usize, scope: &str, question: &str) -> String {
    if running_programs == 1 {
        format!("A program is still running in {scope}. {question}")
    } else {
        format!("{running_programs} programs are still running in {scope}. {question}")
    }
}

pub(super) fn pane_bounds_for_size(size: PhysicalSize<u32>) -> PaneRectApp {
    PaneRectApp::new(0, 0, size.width, size.height)
}

pub(super) fn can_split_rect(rect: PaneRectApp, orientation: SplitOrientation) -> bool {
    let required = MIN_PANE_SIZE_PX
        .saturating_mul(2)
        .saturating_add(split_tree::DIVIDER_WIDTH_PX);
    match orientation {
        SplitOrientation::Horizontal => rect.w >= required,
        SplitOrientation::Vertical => rect.h >= required,
    }
}

pub(super) fn grid_size_for_pane_rect(
    rect: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    grid_size_for_physical_size(PhysicalSize::new(rect.w, rect.h), metrics, padding)
}

pub(super) fn split_point_from_physical_position(
    position: PhysicalPosition<f64>,
) -> Option<split_tree::Point> {
    if !position.x.is_finite() || !position.y.is_finite() || position.x < 0.0 || position.y < 0.0 {
        return None;
    }
    Some(split_tree::Point::new(
        position.x.floor().min(f64::from(u32::MAX)) as u32,
        position.y.floor().min(f64::from(u32::MAX)) as u32,
    ))
}

pub(super) fn render_pane_id(pane_id: PaneId) -> RenderPaneId {
    RenderPaneId::new(pane_id.get())
}

pub(super) fn render_pane_rect(rect: PaneRectApp) -> PaneRect {
    PaneRect::new(rect.x, rect.y, rect.w, rect.h)
}

pub(super) fn visible_pane_ids(tree: &SplitTree, zoomed: Option<PaneId>) -> Vec<PaneId> {
    split_tree::zoom_decision(tree, zoomed, PaneRectApp::new(0, 0, 0, 0)).draw_panes
}

pub(super) fn tab_title(title: &str) -> String {
    if title.is_empty() {
        "noa".to_string()
    } else {
        title.to_string()
    }
}

pub(super) fn apply_terminal_action(terminal: &mut Terminal, action: TerminalAction) {
    match action {
        TerminalAction::Clear => terminal.clear_active_display_and_scrollback(),
        TerminalAction::ClearScrollback => terminal.clear_scrollback(),
        TerminalAction::SelectAll => terminal.select_all(),
    }
}

pub(super) fn apply_viewport_scroll(
    terminal: &mut Terminal,
    grid_size: GridSize,
    scroll: ViewportScroll,
) {
    let page_rows = usize::from(grid_size.rows.saturating_sub(1).max(1));
    match scroll {
        ViewportScroll::LineUp => terminal.scroll_viewport_up(1),
        ViewportScroll::LineDown => terminal.scroll_viewport_down(1),
        ViewportScroll::PageUp => terminal.scroll_viewport_up(page_rows),
        ViewportScroll::PageDown => terminal.scroll_viewport_down(page_rows),
        ViewportScroll::Top => terminal.scroll_viewport_to_top(),
        ViewportScroll::Bottom => terminal.scroll_viewport_to_bottom(),
        ViewportScroll::PrevPrompt => {
            terminal.scroll_to_prompt(PromptJump::Prev);
        }
        ViewportScroll::NextPrompt => {
            terminal.scroll_to_prompt(PromptJump::Next);
        }
    }
}

pub(super) fn apply_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    grid_size: GridSize,
    scroll: ViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_viewport_scroll(terminal, grid_size, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseWheelViewportScroll {
    Up(usize),
    Down(usize),
}

pub(super) fn mouse_wheel_viewport_scroll(
    delta: MouseScrollDelta,
    cell_height: f32,
) -> Option<MouseWheelViewportScroll> {
    let (delta_y, rows) = match delta {
        MouseScrollDelta::LineDelta(_, y) => (y, y.abs().ceil() as usize),
        MouseScrollDelta::PixelDelta(position) => {
            let y = position.y as f32;
            let rows = (y.abs() / cell_height.max(f32::EPSILON)).ceil() as usize;
            (y, rows)
        }
    };

    if !delta_y.is_finite() || delta_y == 0.0 || rows == 0 {
        return None;
    }

    if delta_y > 0.0 {
        Some(MouseWheelViewportScroll::Up(rows))
    } else {
        Some(MouseWheelViewportScroll::Down(rows))
    }
}

pub(super) fn apply_mouse_wheel_viewport_scroll(
    terminal: &mut Terminal,
    scroll: MouseWheelViewportScroll,
) {
    match scroll {
        MouseWheelViewportScroll::Up(rows) => terminal.scroll_viewport_up(rows),
        MouseWheelViewportScroll::Down(rows) => terminal.scroll_viewport_down(rows),
    }
}

pub(super) fn apply_mouse_wheel_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    scroll: MouseWheelViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_mouse_wheel_viewport_scroll(terminal, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabCloseOutcome<Id> {
    Stale,
    Quit,
    Continue { focused: Option<Id> },
}

pub(super) fn close_tab_outcome<Id: Copy + Eq>(
    order: &[Id],
    focused: Option<Id>,
    closing: Id,
    keep_alive_when_empty: bool,
) -> TabCloseOutcome<Id> {
    let Some(closing_index) = order.iter().position(|id| *id == closing) else {
        return TabCloseOutcome::Stale;
    };
    if order.len() == 1 {
        if keep_alive_when_empty {
            return TabCloseOutcome::Continue { focused: None };
        }
        return TabCloseOutcome::Quit;
    }

    let next_focus = if focused == Some(closing) {
        order.get(closing_index + 1).copied().or_else(|| {
            closing_index
                .checked_sub(1)
                .and_then(|idx| order.get(idx).copied())
        })
    } else {
        focused.filter(|id| {
            order
                .iter()
                .any(|existing| existing == id && *existing != closing)
        })
    };
    TabCloseOutcome::Continue {
        focused: next_focus.or_else(|| order.iter().copied().find(|id| *id != closing)),
    }
}

/// Which tab group a spawned tab should join, given the spawn target and the
/// focused window's group (if any). The `Fresh` arm defers minting an id to
/// the caller ([`App::allocate_group_id`]) so this stays a pure decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupChoice<G> {
    Existing(G),
    Fresh,
}

pub(super) fn spawn_group_choice<G: Copy>(
    target: SpawnTarget,
    focused_group: Option<G>,
) -> GroupChoice<G> {
    match target {
        SpawnTarget::NewWindow => GroupChoice::Fresh,
        SpawnTarget::CurrentWindow => match focused_group {
            Some(group) => GroupChoice::Existing(group),
            None => GroupChoice::Fresh,
        },
    }
}

/// The ids in `order` whose group is `group`, preserving `order`. Backs
/// [`App::close_window`] (which closes every tab of the focused window's
/// group) and keeps the group-membership filter unit-testable without a live
/// window map.
pub(super) fn ids_in_group<Id: Copy, G: Copy + Eq>(
    order: &[Id],
    group_of: impl Fn(Id) -> Option<G>,
    group: G,
) -> Vec<Id> {
    order
        .iter()
        .copied()
        .filter(|id| group_of(*id) == Some(group))
        .collect()
}

pub(super) fn overview_tile_source_order<W: Copy + Eq, P: Copy>(
    window_order: &[W],
    mut live_window: impl FnMut(W) -> bool,
    mut pane_ids_for_window: impl FnMut(W) -> Vec<P>,
    overview_window: Option<W>,
) -> Vec<(W, P)> {
    window_order
        .iter()
        .copied()
        .filter(|id| Some(*id) != overview_window && live_window(*id))
        .flat_map(|window_id| {
            pane_ids_for_window(window_id)
                .into_iter()
                .map(move |pane_id| (window_id, pane_id))
        })
        .collect()
}

pub(super) fn overview_tile_target_at_point<Id: Copy>(
    source_ids: &[Id],
    tile_rects: &[PaneRectApp],
    point: split_tree::Point,
) -> Option<Id> {
    let tiles = source_ids
        .iter()
        .copied()
        .zip(tile_rects.iter().copied())
        .collect::<Vec<_>>();
    hit_test_overview_grid(&tiles, point)
}

/// The close-button (✕) target for `point` (REQ-OV-13). Deliberately a
/// separate hit-test surface from [`overview_tile_target_at_point`]: the caller
/// checks this one *first* so a click landing on the title bar's close-button
/// corner closes the tab rather than focusing it, even though both rects
/// overlap there. `tile_rects` covers both live tiles and placeholder rows —
/// every tile has a title bar with a close button.
pub(super) fn overview_close_target_at_point<Id: Copy>(
    source_ids: &[Id],
    tile_rects: &[PaneRectApp],
    point: split_tree::Point,
) -> Option<Id> {
    let tiles = source_ids
        .iter()
        .copied()
        .zip(tile_rects.iter().copied())
        .collect::<Vec<_>>();
    overview_close_hit_test(&tiles, point)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetedRedrawDecision {
    Stale,
    Suppress,
    Request,
}

pub(super) fn targeted_redraw_decision(exists: bool, occluded: bool) -> TargetedRedrawDecision {
    if !exists {
        TargetedRedrawDecision::Stale
    } else if occluded {
        TargetedRedrawDecision::Suppress
    } else {
        TargetedRedrawDecision::Request
    }
}

pub(super) fn pane_user_event_redraw_decision(
    pane_state: Option<(bool, bool)>,
) -> TargetedRedrawDecision {
    let Some((pane_exists, occluded)) = pane_state else {
        return TargetedRedrawDecision::Stale;
    };
    targeted_redraw_decision(pane_exists, occluded)
}

pub(super) fn overview_redraw_decision(
    source_state: Option<(bool, bool)>,
    overview_visible: bool,
    overview_occluded: bool,
) -> TargetedRedrawDecision {
    let Some((source_exists, source_occluded)) = source_state else {
        return TargetedRedrawDecision::Stale;
    };
    if !overview_visible || !source_exists {
        TargetedRedrawDecision::Stale
    } else if overview_occluded || source_occluded {
        TargetedRedrawDecision::Suppress
    } else {
        TargetedRedrawDecision::Request
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandScope {
    App,
    FocusedTab,
    NativeTabGroup,
    Overview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandOrigin {
    App,
    TerminalWindow,
    OverviewWindow,
}

pub(super) fn command_scope(command: AppCommand) -> CommandScope {
    match command {
        AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::CloseTab => CommandScope::FocusedTab,
        AppCommand::ToggleTabOverview
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab => CommandScope::NativeTabGroup,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::ToggleCommandPalette
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::CloseWindow
        | AppCommand::Quit => CommandScope::App,
    }
}

/// Build the render-facing palette payload from the app-side session,
/// resolving each filtered command's title and (current) keybind hint. Takes
/// no terminal lock — the palette is terminal-independent (R-12).
pub(super) fn command_palette_snapshot(
    keybinds: &KeybindEngine,
    palette: &CommandPalette,
) -> CommandPaletteSnapshot {
    let rows = palette
        .filtered()
        .iter()
        .map(|&command| {
            (
                command_palette::command_palette_title(command).to_string(),
                command_palette::command_palette_keybind(keybinds, command),
            )
        })
        .collect();
    CommandPaletteSnapshot {
        query: palette.query().to_string(),
        rows,
        selected: palette.selected(),
    }
}

pub(super) fn overview_command_scope(command: AppCommand) -> CommandScope {
    match command {
        AppCommand::ToggleTabOverview => CommandScope::NativeTabGroup,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::Quit
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry => CommandScope::App,
        // The palette does not open while the overview is focused (v1, R-10):
        // Overview scope makes `ToggleCommandPalette` a no-op there (AC-15).
        AppCommand::ToggleCommandPalette
        | AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::CloseTab
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab
        | AppCommand::CloseWindow => CommandScope::Overview,
    }
}

pub(super) fn overview_should_intercept_command(
    command: AppCommand,
    overview_visible: bool,
    origin: CommandOrigin,
) -> bool {
    overview_visible
        && origin != CommandOrigin::TerminalWindow
        && overview_command_scope(command) == CommandScope::Overview
}

pub(super) fn try_peek_overview_snapshot(
    terminal: &Arc<Mutex<Terminal>>,
) -> Option<Arc<FrameSnapshot>> {
    match terminal.try_lock() {
        Ok(term) => Some(Arc::new(FrameSnapshot::peek(&term))),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Poisoned(_)) => panic!("terminal mutex poisoned"),
    }
}

pub(super) fn resolve_command_target<Id: Copy>(
    command: AppCommand,
    focused: Option<Id>,
) -> Option<Id> {
    if command_scope(command) == CommandScope::FocusedTab {
        focused
    } else {
        None
    }
}

pub(super) fn tab_overview_visibility_after_dispatch(
    command: AppCommand,
    overview_visible: bool,
) -> Option<bool> {
    match command {
        AppCommand::ToggleTabOverview => Some(!overview_visible),
        _ => None,
    }
}

/// Clear the overview surface to the backdrop color when there are no tiles to
/// composite (the card composite pass otherwise does the clear itself).
pub(super) fn clear_overview_surface(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    color: [f32; 4],
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("noa-overview-empty-clear-encoder"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("noa-overview-empty-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(color[0]),
                        g: f64::from(color[1]),
                        b: f64::from(color[2]),
                        a: f64::from(color[3]),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    queue.submit(Some(encoder.finish()));
}

/// Choose the swapchain surface format, preferring a **non-sRGB** format
/// (`Bgra8Unorm`) over an sRGB one (`Bgra8UnormSrgb`).
///
/// This is the WP3 (REQ-AA-1) "native gamma-correct AA" fix. When the
/// surface format `.is_srgb()`, the GPU's fixed-function alpha blend unit
/// decodes stored texels to linear before blending and re-encodes to sRGB
/// on write — so `wgpu::BlendState::ALPHA_BLENDING` (`pipeline.rs`) executes
/// in **linear** space. That's a different blend space than Ghostty's
/// `native` macOS text-rendering mode, which blends glyph coverage against
/// the background directly in gamma-encoded space (how CoreText/FreeType
/// render by default) — the mismatch visibly thins dark-on-light glyph
/// edges relative to Ghostty.
///
/// Preferring a non-sRGB surface format makes all blending — solid
/// backgrounds, selection highlights, and glyph coverage — happen in gamma
/// space, matching `native`. This is in lockstep with
/// `Renderer::new`'s `target_format_is_srgb: format.is_srgb()`
/// (`noa-render/src/renderer.rs`), which routes `surface_output_rgba`
/// (`noa-render/src/renderer.rs`) into its no-op branch whenever the
/// surface format is non-sRGB: colors are written to the target unchanged,
/// no double-gamma. Do **not** "fix" this back to preferring
/// `Bgra8UnormSrgb` — that reintroduces the linear-blend thinning bug.
/// Falls back to `Bgra8UnormSrgb`, then to the first available format, if
/// the adapter offers no non-sRGB option.
pub(super) fn preferred_surface_format(available: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    available
        .iter()
        .copied()
        .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
        .or_else(|| {
            available
                .iter()
                .copied()
                .find(|f| *f == wgpu::TextureFormat::Bgra8UnormSrgb)
        })
        .unwrap_or(available[0])
}

/// Pick the surface's composite-alpha mode. An opaque window keeps the
/// existing Opaque preference (solid terminal colors). A transparent window
/// (`background-opacity` below 1.0) instead prefers, in order, `PostMultiplied`
/// (our colors are straight, non-premultiplied), then `PreMultiplied`, then
/// `Inherit`, before falling back to whatever the surface offers first.
pub(super) fn preferred_surface_alpha_mode(
    caps: &wgpu::SurfaceCapabilities,
    transparent: bool,
) -> wgpu::CompositeAlphaMode {
    let preference: &[wgpu::CompositeAlphaMode] = if transparent {
        &[
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
    } else {
        &[wgpu::CompositeAlphaMode::Opaque]
    };
    preference
        .iter()
        .copied()
        .find(|mode| caps.alpha_modes.contains(mode))
        .or_else(|| caps.alpha_modes.first().copied())
        .unwrap_or(wgpu::CompositeAlphaMode::Auto)
}

pub(super) fn focus_report_bytes(focused: bool, focus_reporting: bool) -> Option<&'static [u8]> {
    if !focus_reporting {
        return None;
    }
    if focused {
        Some(b"\x1b[I")
    } else {
        Some(b"\x1b[O")
    }
}

pub(super) fn font_pixel_size(point_size: f32, scale_factor: f64) -> f32 {
    (point_size * scale_factor.max(f64::EPSILON) as f32).max(1.0)
}

pub(super) fn initial_window_logical_size(
    metrics: noa_font::Metrics,
    grid_size: GridSize,
    scale_factor: f64,
    padding: GridPadding,
) -> LogicalSize<f64> {
    let scale_factor = scale_factor.max(f64::EPSILON) as f32;
    let physical_w = (metrics.cell_w * grid_size.cols as f32 + padding.horizontal())
        .ceil()
        .max(1.0);
    let physical_h = (metrics.cell_h * grid_size.rows as f32 + padding.vertical())
        .ceil()
        .max(1.0);

    LogicalSize::new(
        (physical_w / scale_factor) as f64,
        (physical_h / scale_factor) as f64,
    )
}

pub(super) fn grid_size_for_physical_size(
    size: PhysicalSize<u32>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    let content_width = (size.width as f32 - padding.horizontal()).max(0.0);
    let content_height = (size.height as f32 - padding.vertical()).max(0.0);
    let cols = (content_width / metrics.cell_w.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = (content_height / metrics.cell_h.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    GridSize::new(cols, rows)
}

pub(super) fn update_ime_cursor_area(
    window: &Window,
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) {
    let (position, size) = ime_cursor_area(metrics, x, y, pane_rect, padding);
    window.set_ime_cursor_area(position, size);
}

pub(super) fn ime_cursor_area(
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let position = PhysicalPosition::new(
        (pane_rect.x as f32 + padding.left + metrics.cell_w * x as f32)
            .round()
            .max(0.0) as i32,
        (pane_rect.y as f32 + padding.top + metrics.cell_h * y as f32)
            .round()
            .max(0.0) as i32,
    );
    let size = PhysicalSize::new(
        metrics.cell_w.ceil().max(1.0) as u32,
        metrics.cell_h.ceil().max(1.0) as u32,
    );
    (position, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_option_as_alt_maps_to_winit_modes() {
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::None),
            OptionAsAlt::None
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Left),
            OptionAsAlt::OnlyLeft
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Right),
            OptionAsAlt::OnlyRight
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Both),
            OptionAsAlt::Both
        );
    }

    #[test]
    fn quick_terminal_slide_offset_spans_hidden_to_revealed() {
        let height = 400.0;
        // Fully hidden: the whole panel sits above the screen top.
        assert!((quick_terminal_top_offset(height, 0.0) - (-height)).abs() < 0.001);
        // Fully revealed: flush with the screen top.
        assert!(quick_terminal_top_offset(height, 1.0).abs() < 0.001);
        // Monotonic: more reveal never moves the panel back up.
        let quarter = quick_terminal_top_offset(height, 0.25);
        let half = quick_terminal_top_offset(height, 0.5);
        assert!(quarter < half);
        assert!(half < 0.0);
    }

    #[test]
    fn ease_out_cubic_is_clamped_and_anchored() {
        assert!((ease_out_cubic(0.0)).abs() < 0.001);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 0.001);
        // Clamps out-of-range input rather than overshooting.
        assert!((ease_out_cubic(-1.0)).abs() < 0.001);
        assert!((ease_out_cubic(2.0) - 1.0).abs() < 0.001);
        // Ease-out front-loads progress: past the midpoint by t=0.5.
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn quick_terminal_progress_is_linear_and_clamped() {
        let duration = Duration::from_millis(200);
        assert!((quick_terminal_progress(Duration::ZERO, duration)).abs() < 0.001);
        assert!(
            (quick_terminal_progress(Duration::from_millis(100), duration) - 0.5).abs() < 0.001
        );
        assert!(
            (quick_terminal_progress(Duration::from_millis(400), duration) - 1.0).abs() < 0.001
        );
        // A zero-length slide is instantly complete (no divide-by-zero).
        assert!((quick_terminal_progress(Duration::ZERO, Duration::ZERO) - 1.0).abs() < 0.001);
    }

    #[test]
    fn quick_terminal_height_is_a_clamped_screen_fraction() {
        assert_eq!(quick_terminal_height(1000, 0.4), 400);
        assert_eq!(quick_terminal_height(1000, 1.0), 1000);
        // Fraction is clamped to a usable range and never exceeds the screen.
        assert_eq!(quick_terminal_height(1000, 2.0), 1000);
        assert_eq!(quick_terminal_height(1000, 0.0), 50);
    }

    fn metrics(cell_w: f32, cell_h: f32) -> noa_font::Metrics {
        noa_font::Metrics {
            cell_w,
            cell_h,
            ascent: cell_h * 0.75,
            descent: cell_h * 0.25,
            line_gap: 0.0,
            underline_position: 0.0,
            underline_thickness: 1.0,
        }
    }

    fn terminal_with_scrollback(grid_size: GridSize) -> Terminal {
        let mut terminal = Terminal::new(grid_size);
        let mut stream = Stream::new();
        stream.feed(b"A\r\nB\r\nC\r\nD\r\nE\r\nF", &mut terminal);
        terminal
    }

    #[test]
    fn font_pixel_size_scales_logical_points() {
        assert_eq!(font_pixel_size(14.0, 1.0), 14.0);
        assert_eq!(font_pixel_size(14.0, 2.0), 28.0);
    }

    #[test]
    fn resolve_grid_padding_keeps_defaults_for_unset_axes() {
        assert_eq!(resolve_grid_padding(None, None), DEFAULT_GRID_PADDING);
    }

    #[test]
    fn resolve_grid_padding_applies_value_to_both_edges_of_an_axis() {
        let padding = resolve_grid_padding(Some(8.0), Some(4.0));
        assert_eq!(padding, GridPadding::new(4.0, 8.0, 4.0, 8.0));

        // Only x set: y keeps the asymmetric default (top 0, bottom 16).
        let x_only = resolve_grid_padding(Some(10.0), None);
        assert_eq!(x_only, GridPadding::new(0.0, 10.0, 16.0, 10.0));

        // Only y set: x keeps the default 16 on both sides.
        let y_only = resolve_grid_padding(None, Some(2.0));
        assert_eq!(y_only, GridPadding::new(2.0, 16.0, 2.0, 16.0));
    }

    #[test]
    fn resolve_cursor_style_is_none_when_nothing_is_configured() {
        assert_eq!(resolve_cursor_style(None, None), None);
    }

    #[test]
    fn resolve_cursor_style_defaults_shape_and_blink() {
        // Only blink toggled: shape defaults to block.
        assert_eq!(
            resolve_cursor_style(None, Some(false)),
            Some(CursorStyle::SteadyBlock)
        );
        // Only shape set: blink defaults on.
        assert_eq!(
            resolve_cursor_style(Some(noa_config::CursorShape::Bar), None),
            Some(CursorStyle::BlinkingBar)
        );
    }

    #[test]
    fn resolve_cursor_style_maps_every_combination() {
        use noa_config::CursorShape;
        let cases = [
            (CursorShape::Block, true, CursorStyle::BlinkingBlock),
            (CursorShape::Block, false, CursorStyle::SteadyBlock),
            (CursorShape::Bar, true, CursorStyle::BlinkingBar),
            (CursorShape::Bar, false, CursorStyle::SteadyBar),
            (CursorShape::Underline, true, CursorStyle::BlinkingUnderline),
            (CursorShape::Underline, false, CursorStyle::SteadyUnderline),
        ];
        for (shape, blink, expected) in cases {
            assert_eq!(
                resolve_cursor_style(Some(shape), Some(blink)),
                Some(expected)
            );
        }
    }

    #[test]
    fn initial_window_size_converts_physical_metrics_to_logical_size() {
        let size = initial_window_logical_size(
            metrics(16.0, 32.0),
            GridSize::new(80, 24),
            2.0,
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(size.width, 656.0);
        assert_eq!(size.height, 392.0);
    }

    #[test]
    fn surface_format_prefers_non_srgb_for_native_gamma_correct_blending() {
        // WP3 / REQ-AA-1 / AC-WP3-01: a non-sRGB surface format keeps the
        // fixed-function alpha blend unit in gamma space, matching
        // Ghostty's `native` macOS text-rendering mode.
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ]),
            wgpu::TextureFormat::Bgra8Unorm
        );
    }

    #[test]
    fn surface_format_falls_back_to_srgb_when_no_non_srgb_option_exists() {
        assert_eq!(
            preferred_surface_format(&[wgpu::TextureFormat::Bgra8UnormSrgb]),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn surface_format_falls_back_to_first_available_when_neither_bgra8_option_exists() {
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureFormat::Rgba8Unorm,
            ]),
            wgpu::TextureFormat::Rgba16Float
        );
    }

    #[test]
    fn surface_alpha_mode_prefers_opaque_to_keep_terminal_colors_solid() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::Opaque,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, false),
            wgpu::CompositeAlphaMode::Opaque
        );
    }

    #[test]
    fn surface_alpha_mode_falls_back_when_opaque_is_unavailable() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![wgpu::CompositeAlphaMode::Inherit],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, false),
            wgpu::CompositeAlphaMode::Inherit
        );
    }

    #[test]
    fn surface_alpha_mode_prefers_post_multiplied_when_transparent() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::PostMultiplied,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::PostMultiplied
        );
    }

    #[test]
    fn surface_alpha_mode_transparent_falls_back_through_preference_order() {
        // No PostMultiplied — the next preferred transparent mode wins.
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::PreMultiplied
        );
    }

    #[test]
    fn surface_alpha_mode_transparent_falls_back_to_first_when_none_preferred() {
        // Only Opaque is offered — a transparent window still has to pick
        // something, so it takes the surface's first advertised mode.
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::Opaque
        );
    }

    #[test]
    fn scale_factor_grid_recompute_uses_new_cell_metrics() {
        let size = PhysicalSize::new(968, 600);

        assert_eq!(
            grid_size_for_physical_size(size, metrics(12.0, 24.0), DEFAULT_GRID_PADDING),
            GridSize::new(78, 24)
        );
        assert_eq!(
            grid_size_for_physical_size(size, metrics(16.0, 30.0), DEFAULT_GRID_PADDING),
            GridSize::new(58, 19)
        );
        assert_eq!(
            grid_size_for_physical_size(
                PhysicalSize::new(1, 1),
                metrics(16.0, 30.0),
                DEFAULT_GRID_PADDING,
            ),
            GridSize::new(1, 1)
        );
    }

    #[test]
    fn runtime_font_size_actions_adjust_and_reset_to_startup_size() {
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 16.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 14.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(18.0, 15.0, FontSizeAction::Reset),
            RuntimeFontSizeUpdate {
                point_size: 15.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Reset),
            RuntimeFontSizeUpdate {
                point_size: 15.0,
                changed: false
            }
        );
    }

    #[test]
    fn runtime_font_size_actions_clamp_to_supported_range() {
        assert_eq!(
            runtime_font_size_update(96.0, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 96.0,
                changed: false
            }
        );
        assert_eq!(
            runtime_font_size_update(6.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 6.0,
                changed: false
            }
        );
        assert_eq!(
            runtime_font_size_update(120.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 96.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(f32::NAN, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 6.0,
                changed: true
            }
        );
    }

    #[test]
    fn font_size_resize_plan_recomputes_each_window_grid_from_new_metrics() {
        let plan = font_size_resize_plan(
            [
                (1_u8, PhysicalSize::new(968, 600)),
                (2_u8, PhysicalSize::new(488, 300)),
            ],
            metrics(16.0, 30.0),
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(
            plan,
            vec![(1, GridSize::new(58, 19)), (2, GridSize::new(28, 9))]
        );
    }

    #[test]
    fn ime_cursor_area_tracks_grid_cell_in_physical_pixels() {
        let (position, size) = ime_cursor_area(
            metrics(7.5, 15.25),
            2,
            3,
            PaneRectApp::new(0, 0, 100, 100),
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(position.x, 31);
        assert_eq!(position.y, 46);
        assert_eq!(size.width, 8);
        assert_eq!(size.height, 16);
    }

    #[test]
    fn viewport_scroll_commands_move_by_line_page_and_extremes() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineUp);
        assert_eq!(terminal.viewport_offset(), 1);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageUp);
        assert_eq!(terminal.viewport_offset(), 3);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineDown);
        assert_eq!(terminal.viewport_offset(), 2);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageDown);
        assert_eq!(terminal.viewport_offset(), 0);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Top);
        assert_eq!(terminal.viewport_offset(), terminal.scrollback_len());

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Bottom);
        assert_eq!(terminal.viewport_offset(), 0);
    }

    #[test]
    fn viewport_scroll_snapshot_tracks_scrolled_row_base() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);
        let before_row_base = terminal.active().visible_row_base();

        let snapshot =
            apply_viewport_scroll_and_snapshot(&mut terminal, grid_size, ViewportScroll::LineUp);

        assert_eq!(terminal.viewport_offset(), 1);
        assert_ne!(snapshot.row_base, before_row_base);
        assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
        assert_eq!(
            snapshot.abs_row_base,
            terminal.active().rows_evicted() + terminal.active().visible_row_base()
        );
        assert!(
            snapshot.row_dirty.iter().all(|&dirty| dirty),
            "overview snapshots are full-row dirty"
        );
        assert!(!snapshot.cursor.visible);
    }

    #[test]
    fn mouse_wheel_delta_maps_to_viewport_scroll_rows() {
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 2.0), 20.0),
            Some(MouseWheelViewportScroll::Up(2))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 20.0),
            Some(MouseWheelViewportScroll::Down(1))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 45.0)),
                15.0,
            ),
            Some(MouseWheelViewportScroll::Up(3))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -20.0)),
                15.0,
            ),
            Some(MouseWheelViewportScroll::Down(2))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 0.0), 20.0),
            None
        );
    }

    #[test]
    fn mouse_wheel_viewport_scroll_moves_terminal_viewport() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Up(2));
        assert_eq!(terminal.viewport_offset(), 2);

        apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Down(1));
        assert_eq!(terminal.viewport_offset(), 1);
    }

    #[test]
    fn mouse_wheel_viewport_scroll_snapshot_tracks_scrolled_row_base() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        let snapshot = apply_mouse_wheel_viewport_scroll_and_snapshot(
            &mut terminal,
            MouseWheelViewportScroll::Up(2),
        );

        assert_eq!(terminal.viewport_offset(), 2);
        assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
        assert_eq!(
            snapshot.abs_row_base,
            terminal.active().rows_evicted() + terminal.active().visible_row_base()
        );
        assert!(!snapshot.cursor.visible);
    }

    #[test]
    fn terminal_clear_action_uses_grid_clear_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));
        terminal.scroll_viewport_up(1);
        terminal.pending_writes.extend_from_slice(b"reply");

        apply_terminal_action(&mut terminal, TerminalAction::Clear);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.viewport_offset(), 0);
        assert_eq!(terminal.pending_writes, b"reply");
    }

    #[test]
    fn terminal_clear_scrollback_action_preserves_live_grid() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::ClearScrollback);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.primary.grid[0].cells[0].ch, 'D');
        assert_eq!(terminal.primary.grid[1].cells[0].ch, 'E');
        assert_eq!(terminal.primary.grid[2].cells[0].ch, 'F');
    }

    #[test]
    fn terminal_select_all_action_uses_grid_selection_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::SelectAll);

        assert_eq!(
            terminal.selected_text().as_deref(),
            Some("A\nB\nC\nD\nE\nF")
        );
    }

    #[test]
    fn close_tab_outcome_is_unambiguous() {
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(2), 9, false),
            TabCloseOutcome::Stale
        );
        assert_eq!(
            close_tab_outcome(&[1], Some(1), 1, false),
            TabCloseOutcome::Quit
        );
        assert_eq!(
            close_tab_outcome(&[1], Some(1), 1, true),
            TabCloseOutcome::Continue { focused: None }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(2), 2, false),
            TabCloseOutcome::Continue { focused: Some(3) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(3), 3, false),
            TabCloseOutcome::Continue { focused: Some(2) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(1), 2, false),
            TabCloseOutcome::Continue { focused: Some(1) }
        );
    }

    #[test]
    fn close_confirm_message_names_scope_and_count() {
        assert_eq!(
            close_confirm_message(CloseConfirmTarget::Pane, 1),
            "A program is still running in this pane. Close it?"
        );
        assert_eq!(
            close_confirm_message(CloseConfirmTarget::Window, 2),
            "2 programs are still running in this window. Close this window?"
        );
        assert_eq!(
            close_confirm_message(CloseConfirmTarget::App, 1),
            "A program is still running in noa. Quit noa?"
        );
    }

    #[test]
    fn spawn_group_choice_routes_new_tab_and_new_window() {
        // New Tab joins the focused window's group; with no focus (startup) it
        // falls back to a fresh group.
        assert_eq!(
            spawn_group_choice(SpawnTarget::CurrentWindow, Some(7_u64)),
            GroupChoice::Existing(7)
        );
        assert_eq!(
            spawn_group_choice::<u64>(SpawnTarget::CurrentWindow, None),
            GroupChoice::Fresh
        );
        // New Window always starts a fresh group, even when one is focused.
        assert_eq!(
            spawn_group_choice(SpawnTarget::NewWindow, Some(7_u64)),
            GroupChoice::Fresh
        );
        assert_eq!(
            spawn_group_choice::<u64>(SpawnTarget::NewWindow, None),
            GroupChoice::Fresh
        );
    }

    #[test]
    fn ids_in_group_filters_focused_windows_tabs() {
        // Two windows: tabs 1,3 in group 0; tabs 2,4 in group 1. Close Window
        // for the group-0 window must target exactly its tabs, in order.
        let order = [1_u8, 2, 3, 4];
        let group_of = |id: u8| match id {
            1 | 3 => Some(0_u8),
            2 | 4 => Some(1_u8),
            _ => None,
        };
        assert_eq!(ids_in_group(&order, group_of, 0), vec![1, 3]);
        assert_eq!(ids_in_group(&order, group_of, 1), vec![2, 4]);
        // A group with no live tabs yields nothing.
        assert_eq!(ids_in_group(&order, group_of, 9), Vec::<u8>::new());
    }

    #[test]
    fn overview_window_order_excludes_overview_and_closed_tabs() {
        let window_order = [1_u8, 2, 3, 4];
        let live_windows = |id| id != 3;
        let panes_for_window = |id| vec![id + 10];

        let sources =
            overview_tile_source_order(&window_order, live_windows, panes_for_window, Some(4));

        assert_eq!(sources, vec![(1, 11), (2, 12)]);
    }

    #[test]
    fn overview_window_order_expands_each_tab_to_panes_in_leaf_order() {
        let window_order = [1_u8, 2, 3];
        let live_windows = |id| id != 2;
        let panes_for_window = |id| match id {
            1 => vec![11, 12, 13],
            3 => vec![31],
            _ => Vec::new(),
        };

        let sources =
            overview_tile_source_order(&window_order, live_windows, panes_for_window, None);

        assert_eq!(sources, vec![(1, 11), (1, 12), (1, 13), (3, 31)]);
    }

    #[test]
    fn overview_click_hit_test_resolves_only_live_tiles() {
        let source_ids = [10_u8, 11, 12, 13, 14, 15, 16, 17, 18, 19];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 90, 120), 9, 0, 0);

        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(45, 45)
            ),
            Some(14)
        );
        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(15, 105)
            ),
            None
        );
    }

    #[test]
    fn overview_close_hit_test_is_exclusive_with_tile_focus() {
        let source_ids = [10_u8, 11, 12, 13];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 200, 200), 9, 0, 0);
        // Tile 0's close button sits at its top-right corner; its body center
        // sits well inside. The two must resolve disjointly (REQ-OV-13).
        let tile0 = layout.tiles[0];
        let close_point = split_tree::Point::new(tile0.right() - 2, tile0.y + 2);
        let body_point = split_tree::Point::new(tile0.x + tile0.w / 2, tile0.y + tile0.h / 2);

        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10)
        );
        assert_eq!(
            overview_tile_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10),
            "both rects overlap at the corner; the caller's close-first ordering picks the close"
        );
        // The body center is a focus hit but never a close hit.
        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, body_point),
            None
        );
        assert_eq!(
            overview_tile_target_at_point(&source_ids, &layout.tiles, body_point),
            Some(10)
        );
    }

    #[test]
    fn targeted_redraw_decision_drops_stale_and_suppresses_occluded_tabs() {
        assert_eq!(
            targeted_redraw_decision(false, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            targeted_redraw_decision(true, true),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            targeted_redraw_decision(true, false),
            TargetedRedrawDecision::Request
        );
    }

    #[test]
    fn stale_pane_user_event_redraw_decision_noops_without_panicking() {
        assert_eq!(
            pane_user_event_redraw_decision(None),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((false, false))),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((true, true))),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((true, false))),
            TargetedRedrawDecision::Request
        );
    }

    #[test]
    fn overview_redraw_decision_respects_visibility_and_occlusion() {
        assert_eq!(
            overview_redraw_decision(None, true, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((false, false)), true, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), false, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), true, true),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            overview_redraw_decision(Some((true, true)), true, false),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), true, false),
            TargetedRedrawDecision::Request
        );
    }

    #[test]
    fn multi_pane_resize_batching_resizes_all_grids_before_pty_winsize_sends() {
        let first = PaneId::new(1);
        let second = PaneId::new(2);
        let third = PaneId::new(3);

        let plan = pane_resize_batch_plan([
            (first, GridSize::new(40, 12)),
            (second, GridSize::new(41, 12)),
            (third, GridSize::new(80, 6)),
        ]);

        assert_eq!(
            plan,
            vec![
                PaneResizeAction::GridResize(first, GridSize::new(40, 12)),
                PaneResizeAction::GridResize(second, GridSize::new(41, 12)),
                PaneResizeAction::GridResize(third, GridSize::new(80, 6)),
                PaneResizeAction::PtyResize(first, GridSize::new(40, 12)),
                PaneResizeAction::PtyResize(second, GridSize::new(41, 12)),
                PaneResizeAction::PtyResize(third, GridSize::new(80, 6)),
            ]
        );
    }

    // FM-4 regression: text-area px must come from the same `rect`/padding
    // grid_size_for_pane_rect used, not an independent cell_w × cols
    // multiplication — which would drift whenever the pane's pixel size
    // isn't an exact multiple of the cell size (as here: 137px / 9px cells).
    #[test]
    fn pixel_metrics_for_pane_derive_text_area_from_rect_not_from_grid_size() {
        let rect = PaneRectApp::new(0, 0, 137, 245);
        let metrics = metrics(9.0, 18.0);

        let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

        assert_eq!(cw, 9);
        assert_eq!(ch, 18);
        // 137 - (16 left + 16 right) = 105, 245 - (0 top + 16 bottom) = 229 —
        // NOT floor(105/9)=11 cols * 9 = 99, which cell_w × cols would give.
        assert_eq!(taw, 105);
        assert_eq!(tah, 229);
    }

    #[test]
    fn pixel_metrics_for_pane_clamps_padding_larger_than_rect_to_zero() {
        let rect = PaneRectApp::new(0, 0, 10, 10);
        let metrics = metrics(9.0, 18.0);

        let (_, _, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

        assert_eq!(taw, 0);
        assert_eq!(tah, 0);
    }

    #[test]
    fn focus_reporting_encodes_csi_i_and_csi_o_only_when_enabled() {
        assert_eq!(focus_report_bytes(true, true), Some(b"\x1b[I".as_slice()));
        assert_eq!(focus_report_bytes(false, true), Some(b"\x1b[O".as_slice()));
        assert_eq!(focus_report_bytes(true, false), None);
        assert_eq!(focus_report_bytes(false, false), None);
    }

    #[test]
    fn command_target_resolution_uses_focused_tab_only_for_terminal_commands() {
        let focused = Some(42_u8);
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::CloseTab,
        ] {
            assert_eq!(resolve_command_target(command, focused), focused);
        }

        for command in [
            AppCommand::NewTab,
            AppCommand::SelectTab(1),
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::Quit,
        ] {
            assert_eq!(resolve_command_target(command, focused), None);
        }
    }

    #[test]
    fn toggle_tab_overview_is_a_native_tab_group_command() {
        assert_eq!(
            command_scope(AppCommand::ToggleTabOverview),
            CommandScope::NativeTabGroup
        );
        assert_eq!(
            resolve_command_target(AppCommand::ToggleTabOverview, Some(42_u8)),
            None
        );
    }

    #[test]
    fn overview_command_scope_resolves_terminal_commands_to_no_ops() {
        let focused = Some(42_u8);
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::NewSplitRight,
            AppCommand::NewSplitDown,
            AppCommand::FocusDirection(Direction::Left),
            AppCommand::ResizeSplit(Direction::Right),
            AppCommand::EqualizeSplits,
            AppCommand::ToggleSplitZoom,
            AppCommand::CloseTab,
        ] {
            assert_eq!(overview_command_scope(command), CommandScope::Overview);
            assert_eq!(resolve_command_target(command, focused), focused);
        }

        assert_eq!(
            overview_command_scope(AppCommand::ToggleTabOverview),
            CommandScope::NativeTabGroup
        );
    }

    #[test]
    fn overview_intercepts_only_non_terminal_window_commands() {
        let command = AppCommand::Paste;

        assert!(overview_should_intercept_command(
            command,
            true,
            CommandOrigin::OverviewWindow
        ));
        assert!(overview_should_intercept_command(
            command,
            true,
            CommandOrigin::App
        ));
        assert!(!overview_should_intercept_command(
            command,
            true,
            CommandOrigin::TerminalWindow
        ));
        assert!(!overview_should_intercept_command(
            command,
            false,
            CommandOrigin::OverviewWindow
        ));
        assert!(!overview_should_intercept_command(
            AppCommand::ToggleTabOverview,
            true,
            CommandOrigin::OverviewWindow
        ));
    }

    #[test]
    fn overview_snapshot_seed_skips_locked_terminal_without_waiting() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));
        let _guard = terminal.lock().expect("terminal mutex poisoned");

        assert!(try_peek_overview_snapshot(&terminal).is_none());
    }

    #[test]
    fn overview_snapshot_seed_peeks_available_terminal() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));

        assert!(try_peek_overview_snapshot(&terminal).is_some());
    }

    #[test]
    fn toggle_tab_overview_dispatch_flips_visibility() {
        let overview_visible =
            tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, false)
                .expect("toggle command should update overview state");
        assert!(overview_visible);
        assert_eq!(
            tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, overview_visible),
            Some(false)
        );
        assert_eq!(
            tab_overview_visibility_after_dispatch(AppCommand::Copy, overview_visible),
            None
        );
    }

    #[test]
    fn empty_terminal_title_falls_back_to_app_name() {
        assert_eq!(tab_title(""), "noa");
        assert_eq!(tab_title("shell"), "shell");
    }

    #[test]
    fn command_palette_toggle_is_app_scoped_and_overview_no_op() {
        // AC-1: openable from any tab. AC-15: a no-op while the overview is
        // focused (Overview scope).
        assert_eq!(
            command_scope(AppCommand::ToggleCommandPalette),
            CommandScope::App
        );
        assert_eq!(
            overview_command_scope(AppCommand::ToggleCommandPalette),
            CommandScope::Overview
        );
    }

    #[test]
    fn command_palette_snapshot_reflects_query_selection_and_keybinds() {
        // AC-18: the render payload mirrors the session (query / filtered
        // titles + keybind hints / selected) with no terminal involved.
        let keybinds = KeybindEngine::default();
        let palette = CommandPalette::open();

        let snapshot = command_palette_snapshot(&keybinds, &palette);
        assert_eq!(snapshot.query, "");
        assert_eq!(snapshot.selected, 0);
        assert_eq!(
            snapshot.rows.len(),
            command_palette::command_palette_entries().len()
        );
        // First entry is About (no binding); Copy carries its cmd+c hint.
        assert_eq!(snapshot.rows[0], ("About noa".to_string(), None));
        assert!(
            snapshot
                .rows
                .contains(&("Copy to Clipboard".to_string(), Some("cmd+c".to_string()))),
            "keybind hints are resolved from the engine"
        );
    }
}
