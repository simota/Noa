//! `CSI 8 ; rows ; cols t`: a program resizing its window to a grid size
//! (Ghostty 1.4.0 `vt-window-resize-allowed`, default off).

use super::*;

/// Ghostty's floor, so a program can't shrink the window to hide its output.
const MIN_RESIZE_COLS: u16 = 40;
const MIN_RESIZE_ROWS: u16 = 10;

impl App {
    /// Resize `window_id` so its terminal is `request`'s grid size. Ignored
    /// unless the config allows it and the pane is the only terminal in a
    /// normal, non-fullscreen window with a single tab — otherwise it would
    /// resize sibling splits or tabs (which share the window frame), as in
    /// Ghostty's `TerminalController.resizeWindow`.
    pub(super) fn handle_window_resize_request(
        &mut self,
        window_id: WindowId,
        request: noa_grid::WindowResizeRequest,
    ) {
        if !self.config.vt_window_resize_allowed
            || self.is_quick_terminal_window(window_id)
            || self.is_scratch_terminal_window(window_id)
        {
            return;
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let tabs = self
            .windows
            .values()
            .filter(|other| other.group == state.group)
            .count();
        if state.pane_count() != 1 || tabs != 1 || state.window.fullscreen().is_some() {
            return;
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.fonts.get(self.window_font_px(window_id)).metrics();
        // Measure from the pane bounds (the single pane's rect) derived from
        // the window's live size, not `surface.rect`, which lags until the
        // `Resized` of an earlier request is handled.
        let pane = self.window_pane_bounds(window_id);
        let (dw, dh) = window_resize_delta_px(request, metrics, self.padding, pane);
        #[cfg(target_os = "macos")]
        {
            if (dw, dh) == (0, 0) {
                return;
            }
            let scale = state.window.scale_factor();
            crate::macos_window::resize_window_by(
                &state.window,
                dw as f64 / scale,
                dh as f64 / scale,
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            let live = state.window.inner_size();
            let mut target =
                window_resize_target_inner(request, (dw, dh), live, state.vt_resize_pending);
            if let Some(monitor) = state.window.current_monitor() {
                target.width = target.width.min(monitor.size().width);
                target.height = target.height.min(monitor.size().height);
            }
            if target == state.vt_resize_pending.unwrap_or(live) {
                return;
            }
            let applied = state.window.request_inner_size(target).is_some();
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.vt_resize_pending = (!applied).then_some(target);
            }
        }
    }
}

/// The inner size to request off macOS, where a resize lands asynchronously:
/// a requested axis is the live size plus its delta (both measured now), and
/// a kept (zero) axis keeps the size still `pending` from an earlier request,
/// so `8;30;0t` then `8;0;100t` don't undo each other before `Resized`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(super) fn window_resize_target_inner(
    request: noa_grid::WindowResizeRequest,
    (dw, dh): (i64, i64),
    live: PhysicalSize<u32>,
    pending: Option<PhysicalSize<u32>>,
) -> PhysicalSize<u32> {
    let base = pending.unwrap_or(live);
    let axis = |cells: u16, live: u32, delta: i64, base: u32| {
        if cells == 0 {
            base
        } else {
            (i64::from(live) + delta).max(1) as u32
        }
    };
    PhysicalSize::new(
        axis(request.cols, live.width, dw, base.width),
        axis(request.rows, live.height, dh, base.height),
    )
}

/// The change in physical pixels that makes `pane` hold `request`'s grid. A
/// zero dimension keeps that axis as is; others are raised to the floor.
pub(super) fn window_resize_delta_px(
    request: noa_grid::WindowResizeRequest,
    metrics: noa_font::Metrics,
    padding: GridPadding,
    pane: PaneRectApp,
) -> (i64, i64) {
    let delta = |cells: u16, min: u16, cell: f32, pad: f32, current: u32| {
        if cells == 0 {
            return 0;
        }
        let target = (f32::from(cells.max(min)) * cell + pad).ceil() as i64;
        target - i64::from(current)
    };
    (
        delta(
            request.cols,
            MIN_RESIZE_COLS,
            metrics.cell_w,
            padding.horizontal(),
            pane.w,
        ),
        delta(
            request.rows,
            MIN_RESIZE_ROWS,
            metrics.cell_h,
            padding.vertical(),
            pane.h,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> noa_font::Metrics {
        noa_font::Metrics {
            cell_w: 9.0,
            cell_h: 18.0,
            ascent: 14.0,
            descent: 4.0,
            line_gap: 0.0,
            underline_position: -2.0,
            underline_thickness: 1.0,
        }
    }

    fn request(rows: u16, cols: u16) -> noa_grid::WindowResizeRequest {
        noa_grid::WindowResizeRequest { rows, cols }
    }

    #[test]
    fn delta_is_the_gap_between_the_requested_grid_and_the_pane() {
        let padding = GridPadding::new(2.0, 2.0, 2.0, 2.0);
        let pane = PaneRectApp::new(0, 0, 724, 436);
        // 100×9+4 = 904, 30×18+4 = 544.
        assert_eq!(
            window_resize_delta_px(request(30, 100), metrics(), padding, pane),
            (180, 108)
        );
    }

    #[test]
    fn zero_dimension_keeps_that_axis() {
        let pane = PaneRectApp::new(0, 0, 720, 432);
        assert_eq!(
            window_resize_delta_px(request(0, 100), metrics(), GridPadding::ZERO, pane),
            (180, 0)
        );
        assert_eq!(
            window_resize_delta_px(request(30, 0), metrics(), GridPadding::ZERO, pane),
            (0, 108)
        );
    }

    #[test]
    fn kept_axis_inherits_a_pending_resize_off_macos() {
        let live = PhysicalSize::new(800, 500);
        let pending = Some(PhysicalSize::new(800, 600));
        // `8;0;100t` while `8;30;0t`'s height is still pending.
        assert_eq!(
            window_resize_target_inner(request(0, 100), (180, 0), live, pending),
            PhysicalSize::new(980, 600)
        );
        assert_eq!(
            window_resize_target_inner(request(0, 100), (180, 0), live, None),
            PhysicalSize::new(980, 500)
        );
    }

    #[test]
    fn tiny_requests_are_raised_to_forty_by_ten() {
        let pane = PaneRectApp::new(0, 0, 720, 432);
        // 40×9 = 360, 10×18 = 180.
        assert_eq!(
            window_resize_delta_px(request(1, 1), metrics(), GridPadding::ZERO, pane),
            (-360, -252)
        );
    }
}
