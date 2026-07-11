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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InterPaneTarget<Window, Pane> {
    pub(crate) window_id: Window,
    pub(crate) pane_id: Pane,
    pub(crate) tab_index: usize,
    pub(crate) pane_index: usize,
}

/// Candidate targets for a source pane's explicit one-shot transfer. The
/// window order supplies tab order; each tab's split-tree leaf order supplies
/// pane order. The source is excluded by `(window, pane)` pair, not by pane id
/// alone, because pane ids are scoped to a tab.
pub(crate) fn inter_pane_targets_in_group<W: Copy + Eq, G: Copy + Eq, P: Copy + Eq>(
    order: &[W],
    mut group_of: impl FnMut(W) -> Option<G>,
    mut pane_ids_for_window: impl FnMut(W) -> Vec<P>,
    source_window: W,
    source_pane: P,
) -> Vec<InterPaneTarget<W, P>> {
    let Some(source_group) = group_of(source_window) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let mut tab_index = 0;
    for window_id in order.iter().copied() {
        if group_of(window_id) != Some(source_group) {
            continue;
        }
        tab_index += 1;
        for (pane_offset, pane_id) in pane_ids_for_window(window_id).into_iter().enumerate() {
            if window_id == source_window && pane_id == source_pane {
                continue;
            }
            targets.push(InterPaneTarget {
                window_id,
                pane_id,
                tab_index,
                pane_index: pane_offset + 1,
            });
        }
    }
    targets
}

pub(crate) fn inter_pane_target_label(
    tab_index: usize,
    tab_title: Option<&str>,
    pane_index: usize,
    pane_id: u64,
) -> String {
    let title = tab_title
        .filter(|title| !title.trim().is_empty())
        .map(|title| format!(" - {}", title.trim()))
        .unwrap_or_default();
    format!("Tab {tab_index}{title} / Pane {pane_index} (PaneId {pane_id})")
}

pub(crate) fn split_tree_pane_ids(tree: &SplitTree) -> Vec<PaneId> {
    fn collect(tree: &SplitTree, out: &mut Vec<PaneId>) {
        match tree {
            SplitTree::Leaf { pane } => out.push(*pane),
            SplitTree::Split { first, second, .. } => {
                collect(first, out);
                collect(second, out);
            }
        }
    }

    let mut panes = Vec::new();
    collect(tree, &mut panes);
    panes
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
    metrics: OverviewMetrics,
) -> Option<Id> {
    let tiles = source_ids
        .iter()
        .copied()
        .zip(tile_rects.iter().copied())
        .collect::<Vec<_>>();
    overview_close_hit_test(&tiles, point, metrics)
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

pub(crate) fn keyboard_preedit_should_swallow_key<Id: Eq>(
    modal_preedit_owner: Option<Id>,
    window_id: Id,
    pane_preedit_active: bool,
) -> bool {
    modal_preedit_owner.is_some_and(|owner| owner == window_id) || pane_preedit_active
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
        | AppCommand::SendSelectionToPane
        | AppCommand::ExportScrollback
        | AppCommand::PipeScrollbackToPager
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewSplitLeft
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitUp
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::ToggleAutoApprove
        | AppCommand::SetTabTitle
        | AppCommand::CloseTab => CommandScope::FocusedTab,
        AppCommand::ToggleTabOverview
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab => CommandScope::NativeTabGroup,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::EditConfigFile
        | AppCommand::ReloadConfig
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::ToggleCommandPalette
        | AppCommand::OpenThemePicker
        | AppCommand::OpenSettings
        | AppCommand::ToggleFullscreen
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::ToggleSidebar
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
    mut command_enabled: impl FnMut(AppCommand) -> bool,
) -> CommandPaletteSnapshot {
    use command_palette::PaletteItem;
    let rows = palette
        .items()
        .iter()
        .map(|item| match item {
            PaletteItem::Header(category) => PaletteRow::Header {
                label: category.label().to_string(),
            },
            PaletteItem::Entry { command, positions } => PaletteRow::Entry {
                title: command_palette::command_palette_title(*command).to_string(),
                // Resolve the chord to macOS key symbols for display (E).
                hint: command_palette::command_palette_keybind(keybinds, *command)
                    .map(|chord| command_palette::keybind_symbols(&chord)),
                match_positions: positions.clone(),
                enabled: command_enabled(*command),
            },
        })
        .collect();
    CommandPaletteSnapshot {
        query: palette.query().to_string(),
        rows,
        selected: palette.selected(),
        total_entries: palette.entry_count(),
    }
}

pub(crate) fn overview_command_scope(command: AppCommand) -> CommandScope {
    match command {
        AppCommand::ToggleTabOverview => CommandScope::NativeTabGroup,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::EditConfigFile
        | AppCommand::ReloadConfig
        | AppCommand::Quit
        | AppCommand::ToggleFullscreen
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::ToggleSidebar => CommandScope::App,
        // The palette does not open while the overview is focused (v1, R-10):
        // Overview scope makes `ToggleCommandPalette` a no-op there (AC-15).
        AppCommand::ToggleCommandPalette
        | AppCommand::OpenThemePicker
        | AppCommand::OpenSettings
        | AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::SendSelectionToPane
        | AppCommand::ExportScrollback
        | AppCommand::PipeScrollbackToPager
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::NewSplitLeft
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitUp
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::ToggleAutoApprove
        | AppCommand::SetTabTitle
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

pub(crate) fn try_refresh_overview_snapshot(
    terminal: &Arc<Mutex<Terminal>>,
    slot: &Arc<Mutex<Option<Arc<FrameSnapshot>>>>,
) -> bool {
    let Some(term) = terminal.try_lock() else {
        return false;
    };
    let mut slot = slot.lock();
    FrameSnapshot::refresh_peek_slot(&mut slot, &term);
    true
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
