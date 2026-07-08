use super::super::*;

impl App {
    pub(in crate::app) fn apply_selection_gesture(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        gesture: SelectionGesture,
    ) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
        {
            let mut terminal = surface.terminal.lock();
            match gesture {
                SelectionGesture::None => {}
                SelectionGesture::Clear { anchor } => {
                    terminal.clear_selection();
                    // Pin the drag anchor to content at press time; extending
                    // against this storage coordinate keeps the selection on
                    // the same text even if output scrolls mid-drag.
                    surface.selection_anchor = Some((
                        terminal.viewport_point_to_selection_point(anchor),
                        terminal.selection_rows_evicted(),
                    ));
                }
                SelectionGesture::Extend { anchor, focus } => {
                    let anchor = match surface.selection_anchor {
                        Some((point, evicted_then)) => {
                            // Rows evicted since capture shifted every storage
                            // coordinate up; re-align (a fully evicted anchor
                            // clamps to the oldest retained row).
                            let shift = terminal.selection_rows_evicted() - evicted_then;
                            if shift > point.y {
                                noa_grid::SelectionPoint::new(0, 0)
                            } else {
                                noa_grid::SelectionPoint::new(point.x, point.y - shift)
                            }
                        }
                        // No pinned anchor (e.g. tracking-mode handoff):
                        // fall back to the gesture's viewport anchor.
                        None => terminal.viewport_point_to_selection_point(anchor),
                    };
                    let focus = terminal.viewport_point_to_selection_point(focus);
                    terminal.set_selection(anchor, focus);
                }
                SelectionGesture::SelectWord(point) => {
                    surface.selection_anchor = None;
                    terminal.select_word_at_viewport_point(point)
                }
                SelectionGesture::SelectLine(point) => {
                    surface.selection_anchor = None;
                    terminal.select_line_at_viewport_point(point)
                }
            }
        }

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn start_split_drag_at_last_mouse_point(
        &mut self,
        window_id: WindowId,
    ) -> bool {
        let Some(target) = self.split_drag_target_at_last_mouse_point(window_id) else {
            return false;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        self.focused = Some(window_id);
        state.last_mouse_pane = None;
        state.active_split_drag = Some(target);
        true
    }

    pub(in crate::app) fn split_drag_target_at_last_mouse_point(
        &self,
        window_id: WindowId,
    ) -> Option<SplitResizeDrag> {
        let state = self.windows.get(&window_id)?;
        if state.zoomed.is_some() {
            return None;
        }
        let point = state.last_mouse_point?;
        // Same bounds as `relayout_and_resize_window`, so divider hit-testing
        // lines up with where the panes were actually laid out.
        let bounds = self.window_pane_bounds(window_id);
        split_resize_drag_target_at_point(&state.split_tree, bounds, point)
    }

    pub(in crate::app) fn drag_active_split(
        &mut self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let Some(target) = state.active_split_drag.clone() else {
                return false;
            };
            resize_split_to_drag_point(&mut state.split_tree, &target, point);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        true
    }

    pub(in crate::app) fn finish_active_split_drag(&mut self, window_id: WindowId) -> bool {
        self.windows
            .get_mut(&window_id)
            .and_then(|state| state.active_split_drag.take())
            .is_some()
    }

    pub(in crate::app) fn pane_cell_at_position(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
        metrics: noa_font::Metrics,
    ) -> Option<(PaneId, Point)> {
        let state = self.windows.get(&window_id)?;
        let point = split_point_from_physical_position(position)?;
        let layout = visible_pane_ids(&state.split_tree, state.zoomed)
            .into_iter()
            .filter_map(|pane_id| {
                state
                    .surfaces
                    .get(&pane_id)
                    .map(|surface| (pane_id, surface.rect))
            })
            .collect::<Vec<_>>();
        let pane_id = match hit_test(&layout, point) {
            Some(HitTarget::Pane(pane_id)) => pane_id,
            Some(HitTarget::Divider) | None => return None,
        };
        let surface = state.surfaces.get(&pane_id)?;
        let local_x = position.x - f64::from(surface.rect.x);
        let local_y = position.y - f64::from(surface.rect.y);
        let cell = mouse::physical_position_to_grid_point(
            local_x,
            local_y,
            metrics.cell_w,
            metrics.cell_h,
            surface.grid_size,
            self.padding,
        );
        Some((pane_id, cell))
    }

    /// The Cmd+hover link under the mouse in `window_id`'s focused-under-
    /// pointer pane, if `Cmd` is held and the cell under `last_mouse_cell`
    /// carries an OSC 8 hyperlink or sits inside an auto-detected
    /// `https?://` URL run. Reuses `last_mouse_pane`/`last_mouse_cell`
    /// (already kept up to date by every `CursorMoved`) instead of
    /// recomputing a pixel hit-test, so it can also be called from
    /// `ModifiersChanged` with the mouse stationary.
    pub(in crate::app) fn hover_link_target(
        &self,
        window_id: WindowId,
    ) -> Option<(PaneId, HoverLink)> {
        if !self.modifiers.super_key() {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return Some((pane_id, HoverLink::Registry(link_id.get())));
        }
        let url = noa_grid::detect_url_at_column(&row, cell.x)?;
        Some((
            pane_id,
            HoverLink::Range {
                y: cell.y,
                x_start: url.start_x,
                x_end: url.end_x,
            },
        ))
    }

    /// Recompute the Cmd+hover target for `window_id` and reconcile it into
    /// `Surface::hover_link` + the window's cursor icon. Called from every
    /// event that can change the answer: `CursorMoved` (pointer or pane
    /// moved) and `ModifiersChanged` (Cmd pressed/released with the mouse
    /// stationary).
    pub(in crate::app) fn sync_hover_link(&mut self, window_id: WindowId) {
        let target = self.hover_link_target(window_id);
        let target_pane = target.as_ref().map(|(pane_id, _)| *pane_id);

        // Clear a stale hover on whichever pane held it previously, if the
        // target has moved to a different pane/window or disappeared. This
        // is the only place a hover can go stale outside its own pane: a
        // pane's own hover_link is otherwise only ever written here.
        if let Some((prev_window, prev_pane)) = self.hovered_link
            && (prev_window != window_id || Some(prev_pane) != target_pane)
        {
            let cleared = self
                .windows
                .get_mut(&prev_window)
                .and_then(|state| state.surfaces.get_mut(&prev_pane))
                .is_some_and(|surface| surface.hover_link.take().is_some());
            if cleared && let Some(state) = self.windows.get(&prev_window) {
                state.window.request_redraw();
            }
            self.hovered_link = None;
        }

        if let Some((pane_id, link)) = target {
            self.hovered_link = Some((window_id, pane_id));
            let changed = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
                .is_some_and(|surface| {
                    let changed = surface.hover_link != Some(link);
                    surface.hover_link = Some(link);
                    changed
                });
            if changed && let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
        }

        self.update_cursor_icon(window_id);
    }

    /// Pointer cursor while a link is Cmd+hovered in `window_id`'s
    /// under-the-mouse pane, the platform default otherwise.
    pub(in crate::app) fn update_cursor_icon(&self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let hovering = state.sidebar_button_hover
            || state
                .last_mouse_pane
                .and_then(|pane_id| state.surfaces.get(&pane_id))
                .is_some_and(|surface| surface.hover_link.is_some());
        state.window.set_cursor(if hovering {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// Resolve the currently Cmd+hovered link in `window_id`'s under-the-
    /// mouse pane to its URI text, re-deriving it from live grid state
    /// (rather than caching the string on `Surface::hover_link`, which the
    /// renderer only needs the geometry of).
    pub(in crate::app) fn open_hovered_link(&self, window_id: WindowId) -> Option<String> {
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        surface.hover_link?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return terminal
                .hyperlinks
                .get(link_id.get())
                .map(|link| link.uri.clone());
        }
        noa_grid::detect_url_at_column(&row, cell.x).map(|url| url.uri)
    }
}
