use super::super::*;
// v3 paging pure fns: imported locally (rather than through app.rs's shared
// `use crate::session_overview::{...}` block) to keep this file's diff
// self-contained.
use crate::session_overview::{WHEEL_PAGE_THRESHOLD, page_after_wheel, page_step};

impl App {
    pub(in crate::app) fn focus_overview_tile_at_last_cursor(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(point) = overview.last_cursor_point else {
            return;
        };

        // v3 paging: hit-test against the current page's tab slice only — every
        // tile on a page is live (no placeholders), so `layout.tiles` alone
        // covers the whole grid.
        let page_view = self.overview_page_view();
        let Some(layout) = self.overview_layout(&page_view.slice) else {
            return;
        };
        let Some(target_tab) =
            overview_tile_target_at_point(&page_view.slice, &layout.tiles, point)
        else {
            return;
        };
        // Overview U1: a click selects the tab AND focuses the pane under the
        // pointer within that tab's internal layout (preserving the per-pane
        // click UX). Resolve the clicked pane against the tab's scaled sub-rects
        // before dismissing; fall back to the tab's own focused pane when the
        // point is over a divider gap.
        let target_pane =
            self.overview_tab_pane_at_point(target_tab, &layout.tiles, &page_view.slice, point);
        // The clicked tile becomes the selection too, not just the focus
        // target — a click and an arrow-keyed Return should leave the
        // Overview in the same selected state. The index is page-local.
        if let Some(index) = page_view.slice.iter().position(|id| *id == target_tab)
            && let Some(overview) = self.overview_window.as_mut()
        {
            overview.selected = index;
        }
        self.focus_tile_from_overview(target_tab, target_pane);
    }

    /// The close-button (✕) target tab under the last cursor point, or `None`
    /// (REQ-OV-13), hit-tested against the current page's tab tiles only (v3
    /// paging — every page tile is live, so there is no separate placeholder
    /// row to chain in anymore).
    pub(in crate::app) fn overview_close_target_at_last_cursor(&self) -> Option<WindowId> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let metrics = self.overview_metrics()?;
        let page_view = self.overview_page_view();
        let layout = self.overview_layout(&page_view.slice)?;
        overview_close_target_at_point(&page_view.slice, &layout.tiles, point, metrics)
    }

    /// Activate a tab tile from the Overview: focus `pane` within `window_id`
    /// (or the tab's own focused pane when `pane` is `None`, e.g. a keyboard
    /// activation or a click over a divider gap) and dismiss the overlay
    /// (Exposé semantics — the host window's terminal becomes usable again).
    pub(in crate::app) fn focus_tile_from_overview(
        &mut self,
        window_id: WindowId,
        pane: Option<PaneId>,
    ) {
        let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        let pane = pane
            .filter(|pane| {
                self.windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(*pane))
            })
            .or_else(|| self.windows.get(&window_id).map(|state| state.focused_pane));
        self.hide_tab_overview();
        if let Some(pane) = pane {
            self.focus_pane(window_id, pane);
        }
        self.focused = Some(window_id);
        window.focus_window();
    }

    /// Drives the Overview-focused keymap directly from the keypress
    /// (REQ-OV-15), mirroring `handle_search_prompt_key`'s
    /// keypress-interception shape: arrows/Return/Esc/Cmd+1..9 are resolved
    /// here and never reach `handle_app_command`, so they can't be swallowed
    /// by `overview_command_scope`'s blanket `AppCommand` no-op. Every other
    /// key falls through to the normal keybind-resolve path, which still
    /// classifies terminal commands as Overview no-ops (REQ-OV-7).
    pub(in crate::app) fn handle_overview_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: &KeyEvent,
    ) {
        if let Some(action) = overview_key_action(&event.logical_key, self.modifiers) {
            match action {
                OverviewAction::MoveSelection(direction) => self.step_overview_selection(direction),
                OverviewAction::Activate => self.activate_overview_selection(),
                OverviewAction::SwitchToLive(n) => self.switch_to_live_overview_tile(n),
                OverviewAction::Dismiss => self.dismiss_or_clear_overview_search(),
                OverviewAction::ToggleZoom => self.toggle_overview_zoom(),
                OverviewAction::PageForward => self.step_overview_page(1),
                OverviewAction::PageBack => self.step_overview_page(-1),
            }
            return;
        }
        // Printable text / Backspace edits the "Search sessions" query (REQ-OV-16),
        // slotted after the Overview action keymap (arrows/Return/Esc/Cmd+N win)
        // and before the normal keybind fallthrough. Nothing here reaches a pty
        // (REQ-OV-7).
        if self.apply_overview_search_edit(event) {
            return;
        }
        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            self.handle_app_command(event_loop, command, CommandOrigin::OverviewWindow);
        }
    }

    /// Escape while the Overview is focused (REQ-OV-16): a non-empty search
    /// query is cleared first and the Overview stays open, an empty query
    /// dismisses it (two-stage Escape; no command-palette precedent, so the
    /// semantics are defined by `overview_escape_action`).
    pub(in crate::app) fn dismiss_or_clear_overview_search(&mut self) {
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        match overview_escape_action(query) {
            OverviewEscapeAction::ClearSearch => self.set_overview_search_query(String::new()),
            OverviewEscapeAction::Dismiss => self.hide_tab_overview(),
        }
    }

    /// Apply a printable-text append or Backspace pop to the "Search sessions"
    /// query (REQ-OV-16). Returns `true` when the key was consumed as a query
    /// edit. Cmd/Ctrl/Alt combos are not swallowed here (they fall through to
    /// the keybind path, mirroring the command palette's Cmd-swallow), so e.g.
    /// the Overview toggle chord still works while typing.
    pub(in crate::app) fn apply_overview_search_edit(&mut self, event: &KeyEvent) -> bool {
        let Some(mut query) = self
            .overview_window
            .as_ref()
            .map(|overview| overview.search_query.clone())
        else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if query.pop().is_none() {
                    // Already empty: still consumed (Backspace has no other
                    // meaning in the Overview) but no redraw is needed.
                    return true;
                }
            }
            _ => {
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return false;
                }
                let Some(text) = event.text.as_deref() else {
                    return false;
                };
                let mut appended = false;
                for c in text.chars().filter(|c| !c.is_control()) {
                    query.push(c);
                    appended = true;
                }
                if !appended {
                    return false;
                }
            }
        }
        self.set_overview_search_query(query);
        true
    }

    /// Replace the search query, reset the page and selection to the first
    /// tile (a query change re-orders and re-sizes the filtered result set,
    /// REQ-OV-16 / palette R-7 parity / REQ-OV-20 v3 paging), and request a
    /// redraw.
    pub(in crate::app) fn set_overview_search_query(&mut self, query: String) {
        // A query edit re-filters and re-slots every tile, so an in-flight
        // drag's source/target tiles no longer mean what they did — cancel it.
        self.cancel_overview_pane_drag();
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query = query;
            overview.page = 0;
            overview.selected = 0;
        } else {
            return;
        }
        // Filtering remaps each window to a new tile slot, so the now-visible
        // set must re-render into those slots instead of showing the previous
        // ordering's stale mirrors. Re-rendering still flows through the 10Hz
        // throttle (REQ-NF-4), so tiles refresh at the next due tick.
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
    }

    /// Arrow-key Overview selection move (REQ-OV-15a), within the current
    /// page's tiles only (v3 paging — arrows never cross a page boundary;
    /// PageUp/PageDown/wheel do that instead).
    pub(in crate::app) fn step_overview_selection(&mut self, direction: Direction) {
        let page_view = self.overview_page_view();
        let Some(layout) = self.overview_layout(&page_view.slice) else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        overview.selected = move_overview_selection(
            page_view.selected_in_page,
            layout.cols,
            page_view.slice.len(),
            direction,
        );
        self.request_overview_redraw();
    }

    /// Flip the Overview to the next (`direction > 0`) or previous
    /// (`direction < 0`) page (v3 paging, REQ-OV-18), clamped at the ends
    /// (no wrap, `page_step`). A no-op when already at that end.
    pub(in crate::app) fn step_overview_page(&mut self, direction: isize) {
        let len = self.overview_source_tab_ids().len();
        let Some(current_page) = self.overview_window.as_ref().map(|overview| overview.page) else {
            return;
        };
        let new_page = page_step(current_page, direction, len, OVERVIEW_GRID_CAP);
        if new_page != current_page {
            self.set_overview_page(new_page);
        }
    }

    /// Route a host-window wheel/trackpad turn to Overview page navigation
    /// (v3 paging, REQ-OV-18) via the accumulator-threshold seam
    /// (`page_after_wheel`). No pty passthrough while the overlay owns the
    /// window (REQ-OV-7) — the Overview has no scrollback of its own, so a
    /// wheel turn always means "page", never "scroll". A discrete mouse-wheel
    /// notch (`LineDelta`) is scaled to cross the threshold in one step, so
    /// one notch flips exactly one page; a trackpad's `PixelDelta` stream
    /// accumulates across calls like a continuous swipe.
    pub(in crate::app) fn apply_overview_wheel(&mut self, delta: MouseScrollDelta) {
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * WHEEL_PAGE_THRESHOLD,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        let len = self.overview_source_tab_ids().len();
        let Some((page, wheel_accum)) = self
            .overview_window
            .as_ref()
            .map(|overview| (overview.page, overview.wheel_accum))
        else {
            return;
        };
        let (new_page, new_accum) =
            page_after_wheel(page, wheel_accum, delta_y, len, OVERVIEW_GRID_CAP);
        let page_changed = new_page != page;
        if let Some(overview) = self.overview_window.as_mut() {
            overview.wheel_accum = new_accum;
        }
        if page_changed {
            self.set_overview_page(new_page);
        }
    }

    /// Apply a page change: store it, reset the selection to the first tile
    /// on the new page (mirrors the search-query-change reset, REQ-OV-20),
    /// mark every source tile dirty so the newly visible page's tiles render
    /// fresh mirrors rather than showing whatever tab last occupied that tile
    /// texture slot's *stale* pooled frame, and request a redraw.
    pub(in crate::app) fn set_overview_page(&mut self, page: usize) {
        // v1: no page auto-flip target-follow — a page change during a drag
        // cancels it (the source/target tiles of the new page are a different
        // set), rather than trying to carry the drag across pages.
        self.cancel_overview_pane_drag();
        if let Some(overview) = self.overview_window.as_mut() {
            overview.page = page;
            overview.selected = 0;
        } else {
            return;
        }
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
    }

    /// Tab toggles a quick-look zoom of the selected tile: the tile's card is
    /// re-composited enlarged and centered above the grid, easing between the
    /// two rects. Purely visual — the selection, hit-testing, and keyboard
    /// nav are unaffected.
    pub(in crate::app) fn toggle_overview_zoom(&mut self) {
        if let Some(overview) = self.overview_window.as_mut() {
            overview.zoomed = !overview.zoomed;
            overview.zoom_anim = Some(OverviewZoomAnim {
                tween: crate::anim::Tween::new(Instant::now(), crate::anim::DUR_BASE),
                expanding: overview.zoomed,
            });
            self.request_overview_redraw();
        }
    }

    /// Recompute which tile of the current page (v3 paging — every page
    /// tile is live, so there is no placeholder row to chain in) the cursor
    /// is over, and repaint on a change so the hover accent ring tracks the
    /// mouse. Pure math per mouse move; no GPU work unless it changed.
    pub(in crate::app) fn update_overview_hover(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let point = overview.last_cursor_point;
        let page_view = self.overview_page_view();
        let hovered = point.and_then(|point| {
            let layout = self.overview_layout(&page_view.slice)?;
            layout.tiles.iter().position(|rect| rect.contains(point))
        });
        if let Some(overview) = self.overview_window.as_mut()
            && overview.hovered != hovered
        {
            overview.hovered = hovered;
            self.request_overview_redraw();
        }
    }

    /// The status-dot color for a tile's title band, mirroring the sidebar's
    /// dot semantics (FR-11/FR-16): red while the attention marker is in its
    /// visible blink phase, else yellow for an unread bell, blue for a busy
    /// program, and `None` for idle (no dot). During the attention blink's
    /// hidden phase the underlying bell/busy color shows, so the band blinks
    /// in phase with the sidebar (FR-A2).
    pub(in crate::app) fn overview_tile_dot_color(
        &self,
        tile_id: OverviewTileId,
    ) -> Option<noa_core::Rgb> {
        let card_id = Self::session_card_id(tile_id.window_id, tile_id.pane_id);
        let card = self.session_store.get(&card_id)?;
        if card.attention && self.attention_marker_visible(&card_id) {
            Some(crate::chrome::palette().dot_red)
        } else if card.unread_bell {
            Some(crate::chrome::palette().dot_yellow)
        } else if card.busy {
            Some(crate::chrome::palette().dot_blue)
        } else {
            None
        }
    }

    /// Return activates the selected Overview tile (REQ-OV-15b). `selected`
    /// indexes into the current page's tile slice (v3 paging — a page has
    /// no placeholder rows, so every selectable index is a live tile).
    pub(in crate::app) fn activate_overview_selection(&mut self) {
        let page_view = self.overview_page_view();
        let Some(&target) = page_view.slice.get(page_view.selected_in_page) else {
            return;
        };
        self.focus_tile_from_overview(target, None);
    }

    /// Cmd+`n` (1-indexed) jumps straight to the `n`-th live tile of the
    /// *current page* (REQ-OV-15c, page-local per v3 paging). Out-of-range
    /// `n` (beyond this page's tile count) is a no-op rather than a panic —
    /// there is no tile to switch to.
    pub(in crate::app) fn switch_to_live_overview_tile(&mut self, n: usize) {
        let page_view = self.overview_page_view();
        if n == 0 || n > page_view.slice.len() {
            return;
        }
        let target = page_view.slice[n - 1];
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = n - 1;
        }
        self.focus_tile_from_overview(target, None);
    }

    /// Route a host-window event to the Overview while the overlay is
    /// visible. Returns `true` when the event was consumed by the Overview;
    /// `false` lets the normal terminal-window handling run (surface
    /// reconfigure, occlusion flag, and focus bookkeeping stay with the host
    /// window).
    pub(in crate::app) fn overview_intercept_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: &WindowEvent,
    ) -> bool {
        match event {
            WindowEvent::RedrawRequested => {
                self.redraw_overview();
                true
            }
            WindowEvent::CursorMoved { position, .. } => {
                let point = split_point_from_physical_position(*position);
                if let Some(overview) = self.overview_window.as_mut() {
                    overview.last_cursor_point = point;
                }
                self.update_overview_hover();
                // Advance an in-flight pane drag (threshold promotion + chip/
                // highlight repaint) after the hover update, so the drop-target
                // highlight reads the freshly-resolved `hovered` tile.
                self.drag_active_overview_pane();
                true
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(host) = self.overview_host() {
                    self.on_cursor_left(host);
                }
                true
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if *button == MouseButton::Left {
                    match *state {
                        ElementState::Pressed => {
                            // REQ-OV-13: the close-button corner wins over the
                            // tile body. Close the targeted pane (`close_pane`
                            // falls back to closing the tab when it was the last
                            // pane); otherwise arm a pending pane drag from the
                            // tile under the press. A below-threshold release
                            // resolves that drag back to the old plain-click
                            // tile focus (`finish_overview_pane_drag`).
                            if let Some(target_tab) = self.overview_close_target_at_last_cursor() {
                                // Overview U1: the tab tile's close button closes
                                // the tab's focused pane (`close_pane` falls back
                                // to closing the whole tab when it was the last
                                // pane).
                                let focused_pane = self
                                    .windows
                                    .get(&target_tab)
                                    .map(|state| state.focused_pane);
                                self.hide_tab_overview();
                                if let Some(window_state) = self.windows.get(&target_tab) {
                                    window_state.window.focus_window();
                                }
                                self.focused = Some(target_tab);
                                if let Some(pane) = focused_pane {
                                    self.request_close_pane(event_loop, target_tab, pane);
                                }
                            } else {
                                self.arm_overview_pane_drag();
                            }
                        }
                        ElementState::Released => self.finish_overview_pane_drag(event_loop),
                    }
                }
                true
            }
            // v3 paging (REQ-OV-18): a wheel/trackpad turn flips pages
            // instead of scrolling — no pty passthrough while the overlay
            // owns the window (REQ-OV-7), and the Overview has no
            // scrollback of its own.
            WindowEvent::MouseWheel { delta, .. } => {
                self.apply_overview_wheel(*delta);
                true
            }
            // No pty passthrough while the overlay owns the window
            // (REQ-OV-7): IME events die here.
            WindowEvent::Ime(_) => true,
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
                true
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.handle_overview_key(event_loop, event);
                }
                true
            }
            // Closing the host closes the tab itself; drop the overlay first
            // so the close-confirm flow (if any) is visible and reachable.
            WindowEvent::CloseRequested => {
                self.hide_tab_overview();
                false
            }
            _ => false,
        }
    }
}
