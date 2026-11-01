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
        let Some(surface) = state.focused_surface() else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.fonts.get(self.window_font_px(window_id)).metrics();
        let (dw, dh) = window_resize_delta_px(request, metrics, self.padding, surface.rect);
        #[cfg(target_os = "macos")]
        {
            let scale = state.window.scale_factor();
            crate::macos_window::resize_window_by(
                &state.window,
                dw as f64 / scale,
                dh as f64 / scale,
            );
            // `Resized` arrives later through winit's queue, but the next
            // request's delta is measured from `surface.rect`: relayout now
            // from the frame AppKit already applied (`inner_size` reads it
            // live) so back-to-back requests don't add the delta twice.
            self.relayout_and_resize_window(window_id);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let inner = state.window.inner_size();
            let mut width = (i64::from(inner.width) + dw).max(1) as u32;
            let mut height = (i64::from(inner.height) + dh).max(1) as u32;
            if let Some(monitor) = state.window.current_monitor() {
                width = width.min(monitor.size().width);
                height = height.min(monitor.size().height);
            }
            // `Some` means the size applied immediately: relayout now, as on
            // macOS, so the next request measures from the new pane rect.
            if state
                .window
                .request_inner_size(PhysicalSize::new(width, height))
                .is_some()
            {
                self.relayout_and_resize_window(window_id);
            }
        }
    }
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
    fn tiny_requests_are_raised_to_forty_by_ten() {
        let pane = PaneRectApp::new(0, 0, 720, 432);
        // 40×9 = 360, 10×18 = 180.
        assert_eq!(
            window_resize_delta_px(request(1, 1), metrics(), GridPadding::ZERO, pane),
            (-360, -252)
        );
    }
}
