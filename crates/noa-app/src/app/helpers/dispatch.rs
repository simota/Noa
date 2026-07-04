//! Tab-close/group, overview, and command-scope dispatch helpers.

use super::*;


pub(crate) fn close_tab_outcome<Id: Copy + Eq>(
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

pub(crate) fn spawn_group_choice<G: Copy>(
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
pub(crate) fn ids_in_group<Id: Copy, G: Copy + Eq>(
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

pub(crate) fn overview_tile_source_order<W: Copy + Eq, P: Copy>(
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

pub(crate) fn overview_tile_target_at_point<Id: Copy>(
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
pub(crate) fn overview_close_target_at_point<Id: Copy>(
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

pub(crate) fn targeted_redraw_decision(exists: bool, occluded: bool) -> TargetedRedrawDecision {
    if !exists {
        TargetedRedrawDecision::Stale
    } else if occluded {
        TargetedRedrawDecision::Suppress
    } else {
        TargetedRedrawDecision::Request
    }
}

pub(crate) fn pane_user_event_redraw_decision(
    pane_state: Option<(bool, bool)>,
) -> TargetedRedrawDecision {
    let Some((pane_exists, occluded)) = pane_state else {
        return TargetedRedrawDecision::Stale;
    };
    targeted_redraw_decision(pane_exists, occluded)
}

pub(crate) fn overview_redraw_decision(
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

pub(crate) fn command_scope(command: AppCommand) -> CommandScope {
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
pub(crate) fn command_palette_snapshot(
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

pub(crate) fn overview_command_scope(command: AppCommand) -> CommandScope {
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

pub(crate) fn overview_should_intercept_command(
    command: AppCommand,
    overview_visible: bool,
    origin: CommandOrigin,
) -> bool {
    overview_visible
        && origin != CommandOrigin::TerminalWindow
        && overview_command_scope(command) == CommandScope::Overview
}

pub(crate) fn try_peek_overview_snapshot(
    terminal: &Arc<Mutex<Terminal>>,
) -> Option<Arc<FrameSnapshot>> {
    match terminal.try_lock() {
        Ok(term) => Some(Arc::new(FrameSnapshot::peek(&term))),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Poisoned(_)) => panic!("terminal mutex poisoned"),
    }
}

pub(crate) fn resolve_command_target<Id: Copy>(
    command: AppCommand,
    focused: Option<Id>,
) -> Option<Id> {
    if command_scope(command) == CommandScope::FocusedTab {
        focused
    } else {
        None
    }
}

pub(crate) fn tab_overview_visibility_after_dispatch(
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
pub(crate) fn clear_overview_surface(
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
