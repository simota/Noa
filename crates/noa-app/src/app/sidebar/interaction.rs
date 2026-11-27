use super::*;

impl App {
    pub(in crate::app) fn focus_next_notification(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        let windows = self.session_windows_for_window(window_id);
        if let Some(card) = self.session_store.next_notification(&windows) {
            self.hide_tab_overview();
            self.focus_session_card(window_id, card);
        }
    }

    /// Route a left-press at `point` (physical px) that lands in the focused
    /// window's sidebar band. Returns `true` when the click was consumed, so
    /// the caller stops before the terminal/split handling sees it (the
    /// terminal must never see a sidebar click). Card hits switch focus to that
    /// session's window (FR-3, A-flavor); the toolbar `+` opens a cwd-inherited
    /// new tab (FR-6); a card `…` opens/closes its close-menu (FR-7).
    pub(in crate::app) fn handle_sidebar_press(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 || point.x >= inset {
            return false;
        }
        // Any sidebar click while an inline rename is open cancels it (mirrors
        // the `…` popup's click-anywhere dismissal); the click still routes.
        self.cancel_sidebar_rename();
        let metrics = self.sidebar_metrics(window_id);

        // An open card `…` menu takes the click first: an item hit runs the
        // action, anything else dismisses the popup (and falls through to normal
        // routing so the same click still selects/scrolls). Remember which card
        // was dismissed so a click on its own `…` button doesn't immediately
        // reopen the menu it just closed (a toggle-then-retoggle).
        let mut dismissed_menu: Option<SessionCardId> = None;
        if let Some(open) = self.windows.get(&window_id).and_then(|s| s.sidebar_menu) {
            if let Some(anchor) = self.card_menu_anchor(window_id, open) {
                let bounds = self.sidebar_layout_bounds(window_id, inset);
                let popup = metrics.card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), bounds);
                // The draw model uses this same bounded rect. If the band is
                // too short for all items, it is empty and cannot receive a hit.
                if let Some(item) = metrics.card_menu_hit_test(popup, point) {
                    self.close_sidebar_menu(window_id);
                    self.activate_card_menu_item(event_loop, window_id, open, item);
                    return true;
                }
            }
            self.close_sidebar_menu(window_id);
            dismissed_menu = Some(open);
        }

        let (bounds, scroll) = {
            let Some(state) = self.windows.get(&window_id) else {
                return false;
            };
            (
                self.sidebar_layout_bounds(window_id, inset),
                state.sidebar_scroll,
            )
        };
        let specs = self.sidebar_card_specs(window_id);
        match metrics.hit_test_cards(bounds, &specs, scroll, point) {
            Some(crate::sidebar::SidebarHit::Card(card)) => {
                let Some(spec) = specs.iter().find(|spec| spec.id == card) else {
                    return true;
                };
                // Begin a *pending* drag-reorder rather than focusing now: a
                // plain click (release without moving past the threshold) still
                // selects the card in `finish_active_sidebar_drag`, while a drag
                // reorders it. `grab_dy` anchors the floating card to the cursor.
                let card_top = metrics
                    .layout_cards(bounds, &specs, scroll)
                    .cards
                    .iter()
                    .find(|c| c.id == card)
                    .map_or(point.y as i64, |c| {
                        i64::from(c.bounds.y) - i64::from(c.clipped_top)
                    });
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.sidebar_drag = Some(SidebarDrag {
                        card,
                        mode: spec.mode,
                        start_y: point.y as i64,
                        grab_dy: point.y as i64 - card_top,
                        current_y: point.y as i64,
                        active: false,
                    });
                }
                true
            }
            Some(crate::sidebar::SidebarHit::CardMenu(card)) => {
                // If this same click just dismissed this card's open menu, leave
                // it closed instead of reopening it (toggle-then-retoggle).
                if dismissed_menu != Some(card) {
                    self.toggle_sidebar_menu(window_id, card);
                }
                true
            }
            Some(crate::sidebar::SidebarHit::NewSession) => {
                // `+`: new tab in the focused window, cwd inherited from the
                // active session via the existing new-tab path (FR-6/AC-8). A
                // spawn failure is surfaced (not silently swallowed) so a dead
                // `+` click leaves a trace instead of looking like a no-op.
                if let Err(err) = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow) {
                    log::warn!("sidebar +: failed to spawn new tab: {err:#}");
                }
                true
            }
            // Inside the band but not on any actionable target: consume it too,
            // since the band is not part of the terminal surface.
            None => true,
        }
    }

    /// Run a chosen card `…` menu item (FR-7). `Close` routes through the
    /// existing pane teardown for that session (which cascades to `close_tab`
    /// when it is the tab's last pane), so the card disappears via the normal GC
    /// choke point (AC-9b). `Rename` opens the inline name editor on the card,
    /// bound to `window_id` — the window whose sidebar the menu was clicked in.
    fn activate_card_menu_item(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        card: SessionCardId,
        item: crate::sidebar::CardMenuItem,
    ) {
        match item {
            crate::sidebar::CardMenuItem::Close => {
                let target_window = WindowId::from(card.window_id.0);
                self.request_close_pane(event_loop, target_window, card.pane_id);
            }
            crate::sidebar::CardMenuItem::Rename => self.start_sidebar_rename(window_id, card),
        }
    }

    /// Open the inline rename editor on `card` (FR-7 Rename), seeded with the
    /// name the card currently displays — including a shadowing tab title
    /// (tab-title REQ-TTL-11) — so a small correction doesn't require
    /// retyping.
    fn start_sidebar_rename(&mut self, window_id: WindowId, card: SessionCardId) {
        let tab_title = self.tab_title_override_for_card(&card);
        let buffer = self
            .session_store
            .get(&card)
            .map(|c| match (&c.name_override, tab_title) {
                (None, Some(title)) => title,
                _ => c.display_name().to_string(),
            })
            .unwrap_or_default();
        self.sidebar_rename = Some(SidebarRenameSession {
            window_id,
            card,
            buffer,
        });
        self.request_sidebar_redraw();
    }

    /// One keystroke for the open inline rename (FR-7 Rename): printable text
    /// appends, Backspace pops, Enter commits a non-empty trimmed name as a
    /// [`SessionDelta::Rename`] (an all-whitespace buffer cancels instead, so a
    /// card can't end up unnamed), Escape cancels. Everything is consumed —
    /// the session is modal for its window's keyboard.
    pub(in crate::app) fn handle_sidebar_rename_key(&mut self, event: &KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.cancel_sidebar_rename();
            }
            Key::Named(NamedKey::Enter) => {
                let Some(session) = self.sidebar_rename.take() else {
                    return;
                };
                if let Some(name) = committed_rename_name(&session.buffer) {
                    self.session_store.apply(SessionDelta::Rename {
                        id: session.card,
                        name,
                    });
                }
                self.request_sidebar_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.sidebar_rename.as_mut() {
                    session.buffer.pop();
                }
                self.request_sidebar_redraw();
            }
            _ => {
                // Cmd/Ctrl/Alt combos are not text; swallow them (modal) but
                // don't edit the buffer.
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return;
                }
                let Some(text) = event.text.as_deref() else {
                    return;
                };
                self.push_sidebar_rename_text(text);
            }
        }
    }

    /// Append printable text to the open rename buffer (typed keys and
    /// committed IME compositions share this path).
    pub(in crate::app) fn push_sidebar_rename_text(&mut self, text: &str) {
        let sanitized = sanitize_rename_input(text);
        if sanitized.is_empty() {
            return;
        }
        if let Some(session) = self.sidebar_rename.as_mut() {
            session.buffer.push_str(&sanitized);
            self.request_sidebar_redraw();
        }
    }

    /// Drop the open inline rename without committing, repainting so the
    /// original name returns.
    pub(in crate::app) fn cancel_sidebar_rename(&mut self) {
        if self.sidebar_rename.take().is_some() {
            self.request_sidebar_redraw();
        }
    }

    /// Toggle the `…` menu popup for `card` in `window_id` (FR-7): a click on the
    /// already-open card's button closes it, otherwise it opens for that card.
    fn toggle_sidebar_menu(&mut self, window_id: WindowId, card: SessionCardId) {
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.sidebar_menu = if state.sidebar_menu == Some(card) {
                None
            } else {
                Some(card)
            };
            state.window.request_redraw();
        }
    }

    /// Close any open card `…` menu in `window_id`.
    pub(in crate::app) fn close_sidebar_menu(&mut self, window_id: WindowId) {
        if let Some(state) = self.windows.get_mut(&window_id)
            && state.sidebar_menu.take().is_some()
        {
            state.window.request_redraw();
        }
    }

    /// The on-screen anchor (the card's `menu_button` rect) for an open menu, or
    /// `None` when that card has scrolled out of view. Recomputes the pure
    /// layout the drawer uses so the popup tracks the card.
    fn card_menu_anchor(&self, window_id: WindowId, card: SessionCardId) -> Option<SidebarRect> {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let specs = self.sidebar_card_specs(window_id);
        let layout =
            self.sidebar_metrics(window_id)
                .layout_cards(bounds, &specs, state.sidebar_scroll);
        layout
            .cards
            .iter()
            .find(|c| c.id == card)
            .map(|c| c.menu_button)
    }

    /// Scroll the sidebar card list when the wheel turns over the band
    /// (FR-15). Returns `true` when consumed (so the terminal never scrolls).
    /// `lines` uses a compact card's height so selecting a longer card does not
    /// change wheel sensitivity.
    pub(in crate::app) fn handle_sidebar_wheel(&mut self, window_id: WindowId, lines: f32) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        let point = self
            .windows
            .get(&window_id)
            .and_then(|s| s.last_mouse_point);
        if inset == 0 || point.is_none_or(|p| p.x >= inset) {
            return false;
        }
        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let metrics = self.sidebar_metrics(window_id);
        let viewport_h = metrics.bands(bounds).viewport.h;
        let specs = self.sidebar_card_specs(window_id);
        let content_h = metrics.content_height_cards(&specs);
        let step = metrics.compact_card_height() as f32;
        let delta = (-lines * step).round() as i64;
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let scroll = crate::sidebar::clamp_scroll(state.sidebar_scroll, content_h, viewport_h);
        let next = (i64::from(scroll) + delta).clamp(0, i64::from(u32::MAX)) as u32;
        state.sidebar_scroll = crate::sidebar::clamp_scroll(next, content_h, viewport_h);
        state.window.request_redraw();
        // The pointer hasn't moved but the cards under it have: recompute the
        // hover so the lifted face follows the scroll instead of sticking.
        self.update_sidebar_button_hover(window_id, point);
        true
    }

    /// Switch focus to the window/pane a clicked card belongs to (FR-3,
    /// A-flavor: focus only, never an active-swap). Converts the card's
    /// GUI-agnostic window id back to the winit `WindowId`.
    fn focus_session_card(&mut self, source_window: WindowId, card: SessionCardId) {
        let window_id = WindowId::from(card.window_id.0);
        if !self.windows.contains_key(&window_id) {
            if let Some(state) = self.windows.get_mut(&source_window) {
                state.sidebar_drag = None;
            }
            return;
        }
        // Capture the full card's top before the former selection collapses.
        // Native-tab switches have separate scroll offsets, so carry the same
        // visual anchor into the newly focused tab as well.
        let top_in_viewport = self.windows.get(&source_window).and_then(|state| {
            let inset = self.window_sidebar_inset_px(source_window);
            let bounds = self.sidebar_layout_bounds(source_window, inset);
            let specs = self.sidebar_card_specs(source_window);
            let layout = self.sidebar_metrics(source_window).layout_cards(
                bounds,
                &specs,
                state.sidebar_scroll,
            );
            layout
                .cards
                .iter()
                .find(|rects| rects.id == card)
                .map(|rects| {
                    i64::from(rects.bounds.y)
                        - i64::from(rects.clipped_top)
                        - i64::from(layout.viewport.y)
                })
        });
        // Read the frozen press geometry before releasing it, then let the
        // new selection expand when its scroll anchor is resolved below.
        // The destination may retain a drag whose release happened while a
        // different tab was focused; it must not override the new selection.
        for id in [source_window, window_id] {
            if let Some(state) = self.windows.get_mut(&id) {
                state.sidebar_drag = None;
            }
        }
        // floating-panes REQ-016: a float's card brings it into view first.
        if self.session_card_is_float(&card) {
            self.focus_tab(window_id);
            self.reveal_float(window_id, card.pane_id);
        } else {
            self.focus_pane(window_id, card.pane_id);
            self.focus_tab(window_id);
        }
        let inset = self.window_sidebar_inset_px(window_id);
        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let metrics = self.sidebar_metrics(window_id);
        let specs = self.sidebar_card_specs(window_id);
        let scroll = metrics.scroll_for_card_anchor(
            &specs,
            card,
            top_in_viewport.unwrap_or(0),
            metrics.bands(bounds).viewport.h,
        );
        if let Some(state) = self.windows.get_mut(&window_id)
            && let Some(scroll) = scroll
        {
            state.sidebar_scroll = scroll;
            state.window.request_redraw();
        }
        let point = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_point);
        self.update_sidebar_button_hover(window_id, point);
    }

    /// Advance an in-flight sidebar card drag on a cursor move. Returns `true`
    /// whenever a press-originated drag is pending for this window, so the caller
    /// stops before the terminal's selection/mouse-report handling sees the move
    /// (a pending drag must never start a text selection). The drag only flips to
    /// `active` — and starts requesting redraws for the floating card / drop
    /// indicator — once the pointer moves past a small DPR-scaled threshold.
    pub(in crate::app) fn drag_active_sidebar(
        &mut self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let scale = self
            .windows
            .get(&window_id)
            .map_or(1.0, |s| s.window.scale_factor() as f32);
        let threshold = (SIDEBAR_DRAG_THRESHOLD * scale).max(1.0) as i64;
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let Some(drag) = state.sidebar_drag.as_mut() else {
            return false;
        };
        drag.current_y = point.y as i64;
        if !drag.active && (drag.current_y - drag.start_y).abs() >= threshold {
            drag.active = true;
        }
        if drag.active {
            state.window.request_redraw();
        }
        true
    }

    /// Finish a sidebar card drag on left-release. A release that never crossed
    /// the drag threshold is a plain click and selects the card (the focus we
    /// deferred at press time). An active drag commits the reorder by mapping the
    /// drop position to a neighbor anchor and calling
    /// [`SessionStore::move_card_before`]. Returns `true` when a drag was in
    /// flight (so the caller swallows the release).
    pub(in crate::app) fn finish_active_sidebar_drag(&mut self, window_id: WindowId) -> bool {
        let Some(drag) = self.windows.get(&window_id).and_then(|s| s.sidebar_drag) else {
            return false;
        };
        if !drag.active {
            self.focus_session_card(window_id, drag.card);
            return true;
        }
        let anchor = self.sidebar_drop_anchor(window_id, drag.current_y);
        // Resolve the drop against the same frozen row that was drawn for the
        // drag; releasing first could change the insertion target mid-drop.
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.sidebar_drag = None;
        }
        if self.session_store.move_card_before(drag.card, anchor) {
            self.request_sidebar_redraw();
        } else if let Some(state) = self.windows.get(&window_id) {
            // Order unchanged (dropped in place): repaint so the floating card
            // snaps back to its slot.
            state.window.request_redraw();
        }
        true
    }

    /// The neighbor card a drop at `pointer_y` (physical px) should insert
    /// *before*, or `None` for a drop past the last card (append). Recomputes the
    /// pure layout the drawer uses so the drop target matches what's on screen.
    fn sidebar_drop_anchor(&self, window_id: WindowId, pointer_y: i64) -> Option<SessionCardId> {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let metrics = self.sidebar_metrics(window_id);
        let vp = metrics.bands(bounds).viewport;
        let specs = self.sidebar_card_specs(window_id);
        let py = pointer_y.clamp(0, u32::MAX as i64) as u32;
        let idx = metrics.drop_index_cards(vp, &specs, state.sidebar_scroll, py);
        specs.get(idx).map(|spec| spec.id)
    }
}

/// Strip control characters from a chunk of rename input (typed keys and
/// committed IME compositions share this), yielding only the printable text to
/// append to the buffer.
fn sanitize_rename_input(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// The name a rename buffer commits to on Enter, or `None` when it is empty
/// once trimmed — an all-whitespace buffer cancels rather than naming a card
/// blank (FR-7).
fn committed_rename_name(buffer: &str) -> Option<String> {
    let name = buffer.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rename_input_keeps_printable_text() {
        assert_eq!(sanitize_rename_input("build"), "build");
        assert_eq!(sanitize_rename_input("プロジェクト"), "プロジェクト");
    }

    #[test]
    fn sanitize_rename_input_drops_control_characters() {
        // Newline, tab, carriage return and NUL are all control chars.
        assert_eq!(sanitize_rename_input("a\nb\tc\r"), "abc");
        assert_eq!(sanitize_rename_input("\u{0}\u{7}"), "");
    }

    #[test]
    fn sanitize_rename_input_preserves_interior_spaces() {
        // A space is not a control char, so it survives; only the buffer's
        // eventual trim (on commit) removes leading/trailing whitespace.
        assert_eq!(sanitize_rename_input("my repo"), "my repo");
    }

    #[test]
    fn committed_rename_name_trims_and_keeps_nonempty() {
        assert_eq!(
            committed_rename_name("  build  "),
            Some("build".to_string())
        );
        assert_eq!(committed_rename_name("build"), Some("build".to_string()));
    }

    #[test]
    fn committed_rename_name_rejects_blank_buffer() {
        assert_eq!(committed_rename_name(""), None);
        assert_eq!(committed_rename_name("   "), None);
        assert_eq!(committed_rename_name("\t\n"), None);
    }
}
