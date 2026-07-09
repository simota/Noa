//! Split-pane operations: creating panes, moving focus, resizing, equalizing,
//! and toggling split zoom.

use super::*;

impl App {
    pub(super) fn new_split(&mut self, window_id: WindowId, direction: Direction) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some((focused_pane, new_pane, focused_rect, auto_approve_enabled)) =
            self.windows.get_mut(&window_id).and_then(|state| {
                let focused_rect = state.focused_surface()?.rect;
                if !can_create_split_in_direction(state.pane_count(), focused_rect, direction)
                    || !can_add_pane_in_direction(&state.split_tree, state.focused_pane, direction)
                {
                    return None;
                }
                let new_pane = mint_available_pane_id(&mut state.next_pane_id, |pane| {
                    state.surfaces.contains_key(&pane)
                        || split_tree::contains_pane(&state.split_tree, pane)
                });
                Some((
                    state.focused_pane,
                    new_pane,
                    focused_rect,
                    state.auto_approve_enabled.clone(),
                ))
            })
        else {
            return;
        };

        let grid_size = grid_size_for_pane_rect(focused_rect, gpu.font.metrics(), self.padding);
        let inherited_cwd = self.pane_cwd(window_id, focused_pane);
        let new_surface = match self.spawn_pane_surface(
            window_id,
            new_pane,
            grid_size,
            focused_rect,
            inherited_cwd,
            auto_approve_enabled,
        ) {
            Ok(surface) => surface,
            Err(err) => {
                log::warn!("failed to spawn split pty: {err}");
                return;
            }
        };

        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            };
            if !split_pane_in_direction(&mut state.split_tree, focused_pane, new_pane, direction) {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            }
            state.surfaces.insert(new_pane, new_surface);
            state.focused_pane = new_pane;
            state.zoomed = None;
            state.last_mouse_pane = Some(new_pane);
            state.window.clone()
        };

        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.persist_session();
    }

    pub(super) fn focus_split_direction(&mut self, window_id: WindowId, direction: Direction) {
        let Some(next) = self.windows.get(&window_id).and_then(|state| {
            focus_in_direction(&state.split_tree, state.focused_pane, direction)
                .filter(|pane| state.contains_pane(*pane))
        }) else {
            return;
        };
        self.focus_pane(window_id, next);
    }

    pub(super) fn focus_pane(&mut self, window_id: WindowId, pane_id: PaneId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if !state.contains_pane(pane_id) || state.focused_pane == pane_id {
            return;
        }
        let losing = state.focused_pane;
        let losing_preedit = state
            .surfaces
            .get(&losing)
            .is_some_and(|surface| surface.ime_state.preedit_active());
        let plan = focus_switch_plan(losing, pane_id);

        if let Some(state) = self.windows.get_mut(&window_id) {
            for op in plan {
                match op {
                    ImeOp::CommitPreedit(pane) => {
                        if let Some(surface) = state.surfaces.get_mut(&pane) {
                            surface.ime_state.commit_preedit();
                        }
                    }
                    ImeOp::RetargetIme(pane) => {
                        if state.contains_pane(pane) {
                            state.focused_pane = pane;
                            state.last_mouse_pane = Some(pane);
                        }
                    }
                }
            }
            // The OS-level composition session survives our local clear —
            // without this, the IME keeps composing and its next Preedit
            // lands on the newly focused pane. Toggling IME off/on discards
            // the marked text so the new pane starts clean.
            if losing_preedit {
                state.window.set_ime_allowed(false);
                state.window.set_ime_allowed(true);
            }
        }
        self.update_focused_ime_cursor_area(window_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn resize_focused_split(&mut self, window_id: WindowId, direction: Direction) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            resize_split(
                &mut state.split_tree,
                state.focused_pane,
                direction,
                SPLIT_RESIZE_STEP_PX,
            );
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    pub(super) fn equalize_splits(&mut self, window_id: WindowId) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            equalize(&mut state.split_tree);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    pub(super) fn toggle_split_zoom(&mut self, window_id: WindowId) {
        let bounds = self.window_pane_bounds(window_id);
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            let decision = zoom_toggle(&state.split_tree, state.zoomed, state.focused_pane, bounds);
            state.zoomed = decision.zoomed;
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }
}
