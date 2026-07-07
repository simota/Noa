use super::super::*;

impl App {
    pub(in crate::app) fn focus_overview_tile_at_last_cursor(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(point) = overview.last_cursor_point else {
            return;
        };

        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(target) = overview_tile_target_at_point(&source_tile_ids, &layout.tiles, point)
        else {
            return;
        };
        // The clicked tile becomes the selection too, not just the focus
        // target — a click and an arrow-keyed Return should leave the
        // Overview in the same selected state.
        if let Some(index) = source_tile_ids.iter().position(|id| *id == target)
            && let Some(overview) = self.overview_window.as_mut()
        {
            overview.selected = index;
        }
        self.focus_tile_from_overview(target);
    }

    /// The close-button (✕) target under the last cursor point, or `None`
    /// (REQ-OV-13). Spans live tiles and placeholder rows — both carry a title
    /// bar with a close button, and both map back to a live source pane.
    pub(in crate::app) fn overview_close_target_at_last_cursor(&self) -> Option<OverviewTileId> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let metrics = self.overview_metrics()?;
        let source_tile_ids = self.overview_source_tile_ids();
        let layout = self.overview_layout(&source_tile_ids)?;
        let tile_rects: Vec<PaneRectApp> = layout
            .tiles
            .iter()
            .chain(layout.placeholders.iter())
            .copied()
            .collect();
        overview_close_target_at_point(&source_tile_ids, &tile_rects, point, metrics)
    }

    pub(in crate::app) fn focus_tile_from_overview(&mut self, tile_id: OverviewTileId) {
        let Some(window) = self
            .windows
            .get(&tile_id.window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        // Exposé semantics: activating a tile dismisses the overlay so the
        // host window's terminal is usable again (the host may itself be the
        // activation target).
        self.hide_tab_overview();
        self.focus_pane(tile_id.window_id, tile_id.pane_id);
        self.focused = Some(tile_id.window_id);
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

    /// Replace the search query, reset the selection to the first tile (a
    /// query change re-orders the result set, REQ-OV-16 / palette R-7 parity),
    /// and request a redraw.
    pub(in crate::app) fn set_overview_search_query(&mut self, query: String) {
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query = query;
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

    /// Arrow-key Overview selection move (REQ-OV-15a).
    pub(in crate::app) fn step_overview_selection(&mut self, direction: Direction) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        overview.selected = move_overview_selection(
            overview.selected,
            layout.cols,
            source_tile_ids.len(),
            direction,
        );
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

    /// Recompute which tile (live or placeholder, in source order) the cursor
    /// is over, and repaint on a change so the hover accent ring tracks the
    /// mouse. Pure math per mouse move; no GPU work unless it changed.
    pub(in crate::app) fn update_overview_hover(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let point = overview.last_cursor_point;
        let source_tile_ids = self.overview_source_tile_ids();
        let hovered = point.and_then(|point| {
            let layout = self.overview_layout(&source_tile_ids)?;
            layout
                .tiles
                .iter()
                .chain(layout.placeholders.iter())
                .position(|rect| rect.contains(point))
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
    /// indexes directly into the combined live + placeholder source order,
    /// so a selected placeholder row resolves to its source pane exactly the
    /// same way a selected live tile does.
    pub(in crate::app) fn activate_overview_selection(&mut self) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(&target) = source_tile_ids.get(overview.selected) else {
            return;
        };
        self.focus_tile_from_overview(target);
    }

    /// Cmd+`n` (1-indexed) jumps straight to the `n`-th live Overview tile
    /// (REQ-OV-15c). Out-of-range `n` (beyond the live tile count) is a
    /// no-op rather than a panic — there is no tile to switch to.
    pub(in crate::app) fn switch_to_live_overview_tile(&mut self, n: usize) {
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        if n == 0 || n > live_tile_count {
            return;
        }
        let target = source_tile_ids[n - 1];
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = n - 1;
        }
        self.focus_tile_from_overview(target);
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
                true
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(host) = self.overview_host() {
                    self.on_cursor_left(host);
                }
                true
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if *button == MouseButton::Left && *state == ElementState::Pressed {
                    // REQ-OV-13: the close-button corner wins over tile-focus.
                    // Close the targeted pane; `close_pane` falls back to
                    // closing the tab when it was the last pane.
                    if let Some(target) = self.overview_close_target_at_last_cursor() {
                        self.hide_tab_overview();
                        if let Some(window_state) = self.windows.get(&target.window_id) {
                            window_state.window.focus_window();
                        }
                        self.focused = Some(target.window_id);
                        self.request_close_pane(event_loop, target.window_id, target.pane_id);
                    } else {
                        self.focus_overview_tile_at_last_cursor();
                    }
                }
                true
            }
            // No pty passthrough while the overlay owns the window
            // (REQ-OV-7): scroll and IME events die here.
            WindowEvent::MouseWheel { .. } | WindowEvent::Ime(_) => true,
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
