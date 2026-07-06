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

pub(crate) fn apply_pane_resize_batch(
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

    // Panes whose grid actually changes, captured before any mutation. A
    // same-size `Terminal::resize` is NOT a no-op (it resets the scroll
    // region and dirties every row), and relayouts also run on events that
    // usually change nothing (window focus), so unchanged panes must be
    // left completely untouched.
    let changed: std::collections::HashSet<PaneId> = targets
        .iter()
        .filter(|(pane_id, _, grid_size)| {
            state
                .surfaces
                .get(pane_id)
                .is_some_and(|surface| surface.grid_size != *grid_size)
        })
        .map(|(pane_id, _, _)| *pane_id)
        .collect();

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
        let mut terminal = surface.terminal.lock();
        if changed.contains(&pane_id) {
            terminal.resize(grid_size);
        }
        if let Some(rect) = rect {
            let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, padding);
            terminal.set_pixel_metrics(cw, ch, taw, tah);
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
            let _ = surface.resize_tx.send(grid_size);
        }
    }
}
