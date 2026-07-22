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

pub(crate) fn tab_close_focus_decision<Id: Copy>(
    is_macos: bool,
    focused: Option<Id>,
    target_exists: bool,
) -> TabCloseFocusDecision<Id> {
    match (focused, target_exists, is_macos) {
        (Some(window_id), true, true) => TabCloseFocusDecision::Deferred(window_id),
        (Some(window_id), true, false) => TabCloseFocusDecision::Immediate(window_id),
        _ => TabCloseFocusDecision::NoTarget,
    }
}

pub(crate) fn should_apply_deferred_focus_restore<Id: Eq>(
    requested: Id,
    focused: Option<Id>,
    target_exists: bool,
) -> bool {
    target_exists && focused.as_ref() == Some(&requested)
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

/// Index at which a new tab should be inserted. A live anchor places the tab
/// immediately after itself; a missing or absent anchor safely falls back to
/// the end of the current order.
pub(crate) fn tab_insert_index<Id: Eq>(order: &[Id], anchor: Option<Id>) -> usize {
    anchor
        .as_ref()
        .and_then(|anchor| order.iter().position(|id| id == anchor))
        .map_or(order.len(), |index| index + 1)
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

/// Throttle interval for an occluded window's background pane-cache refresh
/// (tab-switch stall fix). Chosen so a long-hidden busy tab's row cache and
/// glyph atlas stay warm enough that the reveal frame's catch-up rebuild is
/// small, without running a rebuild on every single pty-output redraw a busy
/// occluded pane requests.
pub(crate) const BG_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Which (if any) occluded window's pane cache should be opportunistically
/// rebuilt right now (present-free, no swapchain touch), given every
/// currently-dirty occluded window and its own last-refresh time.
///
/// Throttle is GLOBAL, not per-window (kaizen cycle 4, finding P1-B): a
/// per-window `BG_REFRESH_INTERVAL` gate admits up to one full-viewport
/// rebuild PER WINDOW per interval, so N busy occluded tabs stall the event
/// loop — and so the foreground window's own rendering and input handling —
/// for up to N rebuilds every interval. At most one candidate is returned per
/// `interval`, app-wide.
///
/// Fairness among ready candidates: the one refreshed longest ago (or never,
/// which sorts as "most overdue") wins, so no single busy tab can starve the
/// others by continuously re-dirtying itself — every dirty occluded window
/// eventually gets its turn, oldest-refresh-first.
///
/// Called only from the `UserEvent::Redraw` path, which already fires
/// exclusively when the io thread observed new pty output — so this is never
/// invoked on a self-armed wake-up; an app with no occluded pty activity
/// never spends a cycle here.
pub(crate) fn background_refresh_selection<Id: Copy>(
    dirty_windows: &[(Id, Option<Instant>)],
    global_last_refresh: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> Option<Id> {
    if let Some(last) = global_last_refresh
        && now.saturating_duration_since(last) < interval
    {
        return None;
    }
    dirty_windows
        .iter()
        .max_by_key(|(_, last_refresh)| {
            last_refresh.map_or(Duration::MAX, |t| now.saturating_duration_since(t))
        })
        .map(|(id, _)| *id)
}

/// Next trailing wake-up deadline for the background-refresh backlog
/// (kaizen cycle 6, finding P2): `None` while the backlog is empty (fully
/// idle — the caller arms no timer at all), otherwise the earliest instant
/// the global throttle reopens. Closes the gap where a dirty candidate is
/// blocked purely by timing (its output landed inside the throttle window,
/// and nothing else ever triggers another check) — without this, it would
/// sit stale until an unrelated event happened to redraw again.
///
/// `dirty_remaining` is `!dirty_occluded_windows.is_empty()` evaluated AFTER
/// whatever this invocation of `maybe_background_refresh_pane_cache` did (or
/// didn't do): the same rule applies whether this call just serviced one
/// candidate and others remain (chain to drain the existing backlog one
/// throttle interval at a time) or was itself blocked by the throttle with
/// candidates still waiting (arm the one retry). Either way, `last_refresh`
/// (the GLOBAL last-refresh instant) is the anchor: the throttle can't
/// reopen before `last_refresh + interval`, regardless of which of those two
/// cases produced this call.
pub(crate) fn bg_refresh_wake_deadline(
    dirty_remaining: bool,
    last_refresh: Option<Instant>,
    interval: Duration,
) -> Option<Instant> {
    if !dirty_remaining {
        return None;
    }
    last_refresh.map(|last| last + interval)
}

/// Whether `redraw()`'s reveal frame may skip the pane-cache rebuild this
/// frame and present the renderer's already-built instances as-is.
/// `pending` is the window's one-shot post-`Occluded(false)` flag;
/// `has_renderable_frame` guards the case where the renderer has nothing
/// usable yet (never rendered, or the viewport changed since its last build)
/// — that case must fall back to the normal full rebuild instead of
/// presenting garbage or a wrongly-sized layout.
pub(crate) fn reveal_fast_path_decision(pending: bool, has_renderable_frame: bool) -> bool {
    pending && has_renderable_frame
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
        | AppCommand::CopyMode(_)
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
        | AppCommand::AttachRemote
        | AppCommand::ToggleCommandPalette
        | AppCommand::OpenThemePicker
        | AppCommand::OpenSettings
        | AppCommand::ToggleProcessMonitor
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
        | AppCommand::AttachRemote
        | AppCommand::OpenThemePicker
        | AppCommand::OpenSettings
        | AppCommand::ToggleProcessMonitor
        | AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::SendSelectionToPane
        | AppCommand::ExportScrollback
        | AppCommand::PipeScrollbackToPager
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::CopyMode(_)
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
