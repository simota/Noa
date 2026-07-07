use super::super::*;

impl App {
    pub(in crate::app) fn request_window_redraw(&self, window_id: WindowId) {
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn window_titlebar_inset_px(&self, window_id: WindowId) -> u32 {
        let Some(state) = self.windows.get(&window_id) else {
            return 0;
        };
        crate::macos_window::top_chrome_inset_px(&state.window).unwrap_or_else(|| {
            titlebar_top_inset_px(
                self.config.macos_titlebar_style,
                state.window.scale_factor(),
            )
        })
    }

    /// Physical left/right/bottom margin around the pane area — non-zero only
    /// under the `transparent` titlebar style (see [`content_margin_px`]).
    pub(in crate::app) fn window_content_margin_px(&self, window_id: WindowId) -> u32 {
        let scale = self
            .windows
            .get(&window_id)
            .map_or(1.0, |state| state.window.scale_factor());
        content_margin_px(self.config.macos_titlebar_style, scale)
    }

    /// The pane-area bounds for `window_id`: the full window minus the
    /// sidebar band and the transparent-titlebar chrome insets. The single
    /// source of truth shared by layout, zoom, and divider hit-testing so
    /// they can never disagree.
    pub(in crate::app) fn window_pane_bounds(&self, window_id: WindowId) -> PaneRectApp {
        let Some(state) = self.windows.get(&window_id) else {
            return PaneRectApp::new(0, 0, 0, 0);
        };
        content_inset_bounds(
            sidebar_inset_bounds(
                pane_bounds_for_size(state.window.inner_size()),
                self.window_sidebar_inset_px(window_id),
            ),
            self.window_titlebar_inset_px(window_id),
            self.window_content_margin_px(window_id),
        )
    }

    pub(in crate::app) fn relayout_and_resize_window(&mut self, window_id: WindowId) {
        #[cfg(target_os = "macos")]
        if let Some(state) = self.windows.get(&window_id)
            && let Some(gpu) = self.gpu.as_ref()
        {
            crate::macos_window::set_window_background_color(
                &state.window,
                gpu.theme.default_bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&state.window, gpu.theme.default_bg);
            }
        }

        let Some(metrics) = self.gpu.as_ref().map(|gpu| gpu.font.metrics()) else {
            return;
        };
        let padding = self.padding;
        // The pane area is the window minus the sidebar band and the
        // transparent-titlebar chrome (Omen P1: `pane_bounds_for_size` itself
        // is untouched — the insets live in `window_pane_bounds`).
        let bounds = self.window_pane_bounds(window_id);
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let targets = zoom_resize_targets(&state.split_tree, state.zoomed, bounds)
            .into_iter()
            .map(|(pane_id, rect)| {
                (
                    pane_id,
                    rect,
                    grid_size_for_pane_rect(rect, metrics, padding),
                )
            })
            .collect::<Vec<_>>();

        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        apply_pane_resize_batch(state, &targets, metrics, padding);

        // Resize overlay (Ghostty `resize-overlay`): surface the focused
        // pane's new `cols × rows` as a transient toast when the grid
        // actually changed. Under `after-first` the window's initial layout
        // (no previous grid) stays silent.
        if let Some(grid) = targets
            .iter()
            .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
            .map(|(_, _, grid)| (grid.cols, grid.rows))
        {
            let changed = state.last_grid.is_some_and(|prev| prev != grid);
            let first = state.last_grid.is_none();
            state.last_grid = Some(grid);
            let show = match self.config.resize_overlay {
                noa_config::ResizeOverlay::Never => false,
                noa_config::ResizeOverlay::Always => changed || first,
                noa_config::ResizeOverlay::AfterFirst => changed,
            };
            if show {
                state.resize_overlay = Some((
                    format!("{} × {}", grid.0, grid.1),
                    Instant::now() + RESIZE_OVERLAY_DURATION,
                ));
                state.window.request_redraw();
            }
        }
    }
}
