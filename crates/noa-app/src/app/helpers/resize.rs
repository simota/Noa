//! Font-size and pane-resize planning helpers.

use super::*;

pub(crate) fn runtime_font_size_update(
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

pub(crate) fn clamp_runtime_font_size(point_size: f32) -> f32 {
    if point_size.is_finite() {
        point_size.clamp(MIN_RUNTIME_FONT_SIZE, MAX_RUNTIME_FONT_SIZE)
    } else {
        MIN_RUNTIME_FONT_SIZE
    }
}

#[cfg(test)]
pub(crate) fn font_size_resize_plan<Id: Copy>(
    windows: impl IntoIterator<Item = (Id, PhysicalSize<u32>)>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> Vec<(Id, GridSize)> {
    windows
        .into_iter()
        .map(|(id, size)| (id, grid_size_for_physical_size(size, metrics, padding)))
        .collect()
}

pub(crate) fn pane_resize_batch_plan<Id: Copy>(
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
pub(crate) fn pixel_metrics_for_pane(
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

/// The live half of a relayout, run on every call (never throttled): update
/// each pane's pixel `rect` and its terminal's pixel metrics (for XTWINOPS
/// reports). Keeping this live means the wgpu surface, the pane viewport rects,
/// and pixel metrics all track the window size frame-by-frame during a drag, so
/// the reflow-throttled grid never letterboxes into black bands — the surface
/// is always sized and cleared to the terminal background. The expensive part
/// (scrollback reflow + pty winsize) is [`apply_pane_grid_resize`], gated by
/// `WindowState::resize_throttle`.
pub(crate) fn apply_pane_layout_live(
    state: &mut WindowState,
    targets: &[(PaneId, PaneRectApp, GridSize)],
    metrics: noa_font::Metrics,
    padding: GridPadding,
) {
    for (pane_id, rect, _grid_size) in targets {
        let Some(surface) = state.surfaces.get_mut(pane_id) else {
            continue;
        };
        surface.rect = *rect;
        let (cw, ch, taw, tah) = pixel_metrics_for_pane(*rect, metrics, padding);
        // Cheap store under the lock (four u32s) — deliberately NOT the
        // scrollback-walking `terminal.resize`, which the throttle defers.
        surface.terminal.lock().set_pixel_metrics(cw, ch, taw, tah);
    }
}

/// The throttled half of a relayout: reflow each changed pane's terminal grid,
/// then send the new pty winsize. Runs on the leading edge and every trailing
/// edge of `WindowState::resize_throttle`, never once per drag frame.
///
/// Grid-first (CLAUDE.md): every changed pane's grid is reflowed *before* any
/// pty winsize goes out, on every coalesced apply — so the shell's SIGWINCH
/// repaint never lands in a stale grid. Grid resize and pty winsize are driven
/// from the *same* `targets`, so they can never diverge, and intermediate
/// (skipped) sizes reach neither — no SIGWINCH storm into a grid we didn't
/// reflow. A pane that closed mid-drag is simply absent from `surfaces` and
/// skipped; a dropped `resize_tx` is ignored (no panic).
pub(crate) fn apply_pane_grid_resize(state: &mut WindowState, targets: &[(PaneId, GridSize)]) {
    // Grid-first ordering (all `GridResize` actions before any `PtyResize`) is
    // built and invariant-tested by `pane_resize_batch_plan`.
    let plan = pane_resize_batch_plan(targets.iter().copied());

    // Panes whose grid actually changes, captured before any mutation. A
    // same-size `Terminal::resize` is NOT a no-op (it resets the scroll
    // region and dirties every row), and relayouts also run on events that
    // usually change nothing (window focus), so unchanged panes must be
    // left completely untouched.
    let changed: std::collections::HashSet<PaneId> = targets
        .iter()
        .filter(|(pane_id, grid_size)| {
            state
                .surfaces
                .get(pane_id)
                .is_some_and(|surface| surface.grid_size != *grid_size)
        })
        .map(|(pane_id, _)| *pane_id)
        .collect();

    for action in &plan {
        let PaneResizeAction::GridResize(pane_id, grid_size) = *action else {
            continue;
        };
        let Some(surface) = state.surfaces.get_mut(&pane_id) else {
            continue;
        };
        // Kept in lockstep with the terminal grid (throttled together): it is
        // read for mouse→cell mapping and scroll snapshots, which must agree
        // with the grid actually reflowed here, not the live pixel rect.
        surface.grid_size = grid_size;
        if changed.contains(&pane_id) {
            surface.terminal.lock().resize(grid_size);
        }
    }

    for action in plan {
        let PaneResizeAction::PtyResize(pane_id, grid_size) = action else {
            continue;
        };
        if !changed.contains(&pane_id) {
            continue;
        }
        if let Some(surface) = state.surfaces.get(&pane_id) {
            match &surface.transport {
                SurfaceTransport::Local(local) => {
                    let _ = local.resize_tx.send(grid_size);
                }
                SurfaceTransport::Remote(remote) => {
                    if let Some(connection) = remote.connection.as_ref() {
                        let _ = connection.resize(grid_size);
                    }
                }
            }
        }
    }
}
