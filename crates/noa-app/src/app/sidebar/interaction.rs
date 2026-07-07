use super::*;

impl App {
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
                let popup = metrics.card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), inset);
                // Mirror the draw-side guard (`sidebar_draw_model` skips a popup
                // whose `bottom() > height`): a popup that would spill past the
                // window bottom is never rendered, so a click in its invisible
                // region must not fire an item — fall through to dismiss instead.
                let height = self
                    .windows
                    .get(&window_id)
                    .map_or(0, |s| s.window.inner_size().height);
                if popup.bottom() <= height
                    && let Some(item) = metrics.card_menu_hit_test(popup, point)
                {
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
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        match metrics.hit_test(bounds, &ids, scroll, point) {
            Some(crate::sidebar::SidebarHit::Card(card)) => {
                // Begin a *pending* drag-reorder rather than focusing now: a
                // plain click (release without moving past the threshold) still
                // selects the card in `finish_active_sidebar_drag`, while a drag
                // reorders it. `grab_dy` anchors the floating card to the cursor.
                let card_top = metrics
                    .layout(bounds, &ids, scroll)
                    .cards
                    .iter()
                    .find(|c| c.id == card)
                    .map_or(point.y as i64, |c| c.bounds.y as i64);
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.sidebar_drag = Some(SidebarDrag {
                        card,
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
                let name = session.buffer.trim().to_string();
                if !name.is_empty() {
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
        let mut appended = false;
        if let Some(session) = self.sidebar_rename.as_mut() {
            for c in text.chars().filter(|c| !c.is_control()) {
                session.buffer.push(c);
                appended = true;
            }
        }
        if appended {
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
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        let layout = self
            .sidebar_metrics(window_id)
            .layout(bounds, &ids, state.sidebar_scroll);
        layout
            .cards
            .iter()
            .find(|c| c.id == card)
            .map(|c| c.menu_button)
    }

    /// Scroll the sidebar card list when the wheel turns over the band
    /// (FR-15). Returns `true` when consumed (so the terminal never scrolls).
    /// `lines` is the wheel delta in card-stride units; positive scrolls down.
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
        let windows = self.session_windows_for_window(window_id);
        let content_h =
            metrics.content_height(self.session_store.ordered_ids_for_windows(&windows).len());
        let step = metrics.card_stride as f32;
        let delta = (-lines * step).round() as i64;
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let next = (state.sidebar_scroll as i64 + delta).max(0) as u32;
        state.sidebar_scroll = crate::sidebar::clamp_scroll(next, content_h, viewport_h);
        state.window.request_redraw();
        true
    }

    /// Switch focus to the window/pane a clicked card belongs to (FR-3,
    /// A-flavor: focus only, never an active-swap). Converts the card's
    /// GUI-agnostic window id back to the winit `WindowId`.
    fn focus_session_card(&mut self, card: SessionCardId) {
        let window_id = WindowId::from(card.window_id.0);
        let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        self.focus_pane(window_id, card.pane_id);
        self.focused = Some(window_id);
        window.focus_window();
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
        let Some(drag) = self
            .windows
            .get_mut(&window_id)
            .and_then(|s| s.sidebar_drag.take())
        else {
            return false;
        };
        if !drag.active {
            self.focus_session_card(drag.card);
            return true;
        }
        let anchor = self.sidebar_drop_anchor(window_id, drag.current_y);
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
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        let py = pointer_y.clamp(0, u32::MAX as i64) as u32;
        let idx = metrics.drop_index(vp, ids.len(), state.sidebar_scroll, py);
        ids.get(idx).copied()
    }
}
