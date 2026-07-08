//! Timer-driven `App` operations: cursor blink, attention blink, sidebar clock,
//! and delayed overview redraw wake-ups.

use super::*;

impl App {
    /// Whether the focused pane has a displayable `Blinking*` cursor.
    fn focused_cursor_wants_blink(&self) -> bool {
        let Some(window_id) = self.focused else {
            return false;
        };
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
        else {
            return false;
        };
        let terminal = surface.terminal.lock();
        let cursor = terminal.active().cursor;
        cursor.visible
            && terminal.viewport_offset() == 0
            && matches!(
                cursor.style,
                CursorStyle::BlinkingBlock
                    | CursorStyle::BlinkingUnderline
                    | CursorStyle::BlinkingBar
            )
    }

    /// Advance the cursor blink phase and return the next wake-up deadline.
    pub(super) fn tick_cursor_blink(&mut self) -> Option<Instant> {
        if !self.focused_cursor_wants_blink() {
            self.cursor_blink_visible = true;
            self.cursor_blink_deadline = None;
            return None;
        }

        let now = Instant::now();
        let deadline = *self
            .cursor_blink_deadline
            .get_or_insert(now + CURSOR_BLINK_INTERVAL);
        if now < deadline {
            return Some(deadline);
        }

        self.cursor_blink_visible = !self.cursor_blink_visible;
        let next = now + CURSOR_BLINK_INTERVAL;
        self.cursor_blink_deadline = Some(next);
        if let Some(window_id) = self.focused
            && let Some(state) = self.windows.get(&window_id)
        {
            state.window.request_redraw();
        }
        Some(next)
    }

    /// Whether an attention marker is currently visible for `id` (FR-A1).
    pub(super) fn attention_marker_visible(&self, id: &SessionCardId) -> bool {
        match self.attention_onset.get(id) {
            Some(onset) => crate::sidebar::attention_blink_on(
                onset.elapsed(),
                ATTENTION_BLINK_DURATION,
                ATTENTION_BLINK_INTERVAL,
            ),
            None => true,
        }
    }

    fn any_attention_blinking(&self) -> bool {
        self.attention_onset
            .values()
            .any(|onset| onset.elapsed() < ATTENTION_BLINK_DURATION)
    }

    /// Advance the attention blink and return the next wake-up deadline.
    pub(super) fn tick_attention_blink(&mut self) -> Option<Instant> {
        if !self.any_attention_blinking() {
            // Final repaint on disarm so the settled steady-on marker is drawn.
            if self.attention_blink_deadline.take().is_some() {
                self.request_sidebar_redraw();
                self.mark_attention_overview_tiles_dirty();
            }
            return None;
        }
        let now = Instant::now();
        if let Some(deadline) = self.attention_blink_deadline
            && now < deadline
        {
            return Some(deadline);
        }
        // An elapsed deadline means a marker crossed a phase boundary: repaint.
        // A fresh arm (`None`) skips it — the apply site already repainted the
        // onset frame.
        if self.attention_blink_deadline.is_some() {
            self.request_sidebar_redraw();
            self.mark_attention_overview_tiles_dirty();
        }
        // Wake at the earliest onset-relative phase boundary across every
        // blinking marker, not `now + interval`: the visible phase is computed
        // from each onset, so an unaligned deadline would paint every flip
        // late and jitter the duty cycle (worst with staggered onsets).
        self.attention_blink_deadline = self.next_attention_blink_deadline(now);
        self.attention_blink_deadline
    }

    /// The earliest next blink phase boundary across all attention onsets, or
    /// `None` when every marker has settled.
    fn next_attention_blink_deadline(&self, now: Instant) -> Option<Instant> {
        self.attention_onset
            .values()
            .filter_map(|onset| {
                crate::sidebar::next_attention_blink_boundary(
                    now.saturating_duration_since(*onset),
                    ATTENTION_BLINK_DURATION,
                    ATTENTION_BLINK_INTERVAL,
                )
                .map(|boundary| *onset + boundary)
            })
            .min()
    }

    fn mark_attention_overview_tiles_dirty(&mut self) {
        let ids: Vec<SessionCardId> = self.attention_onset.keys().copied().collect();
        for id in ids {
            let window_id = WindowId::from(id.window_id.0);
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, id.pane_id));
        }
        self.request_overview_redraw();
    }

    /// Repaint visible sidebars once a minute so relative timestamps advance.
    pub(super) fn tick_sidebar_clock(&mut self) -> Option<Instant> {
        if !self.any_sidebar_visible() {
            self.sidebar_clock_deadline = None;
            return None;
        }
        let now = Instant::now();
        match self.sidebar_clock_deadline {
            Some(deadline) if now < deadline => Some(deadline),
            _ => {
                if self.sidebar_clock_deadline.take().is_some() {
                    self.request_sidebar_redraw();
                }
                let next = now + Duration::from_secs(60);
                self.sidebar_clock_deadline = Some(next);
                Some(next)
            }
        }
    }

    /// Re-sort visible sidebars' cards by update recency every
    /// `SIDEBAR_AUTOSORT_INTERVAL`, repainting only when the order actually
    /// changed. The refresh also fires on the arming tick (sidebar just became
    /// visible), so a stale order never shows for a full interval. A tick that
    /// lands mid-drag skips the refresh — re-sorting would shuffle the list
    /// under the pointer and retarget the drop — and retries next interval.
    pub(super) fn tick_sidebar_autosort(&mut self) -> Option<Instant> {
        if !self.any_sidebar_visible() {
            self.sidebar_autosort_deadline = None;
            return None;
        }
        let now = Instant::now();
        match self.sidebar_autosort_deadline {
            Some(deadline) if now < deadline => Some(deadline),
            _ => {
                let drag_active = self
                    .windows
                    .values()
                    .any(|state| state.sidebar_drag.as_ref().is_some_and(|drag| drag.active));
                if !drag_active && self.session_store.refresh_auto_order() {
                    self.request_sidebar_redraw();
                }
                let next = now + SIDEBAR_AUTOSORT_INTERVAL;
                self.sidebar_autosort_deadline = Some(next);
                Some(next)
            }
        }
    }

    /// Expire transient per-window overlays (the `cols × rows` resize toast
    /// and the `visual-bell` flash), repainting a window whose overlay just
    /// ended, and report the earliest pending expiry.
    pub(super) fn tick_transient_overlays(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        for state in self.windows.values_mut() {
            if let Some((_, until)) = state.resize_overlay {
                if now >= until {
                    state.resize_overlay = None;
                    state.window.request_redraw();
                } else {
                    next = Some(next.map_or(until, |n| n.min(until)));
                }
            }
            if let Some(until) = state.bell_flash_until {
                if now >= until {
                    state.bell_flash_until = None;
                    state.window.request_redraw();
                } else {
                    next = Some(next.map_or(until, |n| n.min(until)));
                }
            }
        }
        let before = self.auto_approve_flash_until.len();
        self.auto_approve_flash_until.retain(|_, until| {
            if now >= *until {
                false
            } else {
                next = Some(next.map_or(*until, |n| n.min(*until)));
                true
            }
        });
        if self.auto_approve_flash_until.len() != before {
            self.request_sidebar_redraw();
        }
        next
    }

    /// Poll the open theme-settings overlay's font-size debouncer (R-9): if
    /// a burst has settled, apply it via the real runtime-font-size path and
    /// report the next wake-up only while something is still pending — an
    /// idle overlay (no font-size edits) never re-arms this tick at all
    /// (NFR-2: no busy-polling).
    pub(super) fn tick_theme_settings_debounce(&mut self) -> Option<Instant> {
        let session = self.theme_settings.as_mut()?;
        let now = Instant::now();
        if let Some(value) = session.state.poll_font_size(now) {
            let window_id = session.window_id;
            self.apply_runtime_font_size(window_id, value);
        }
        self.theme_settings.as_ref().and_then(|session| {
            session
                .state
                .font_size_debounce_pending()
                .then_some(now + Duration::from_millis(30))
        })
    }

    /// Wake the Session Overview once the earliest throttle-blocked dirty tile
    /// becomes due.
    pub(super) fn tick_overview_backlog(&mut self) -> Option<Instant> {
        let deadline = self.overview_wake_deadline?;
        if Instant::now() < deadline {
            return Some(deadline);
        }
        self.overview_wake_deadline = None;
        self.request_overview_redraw();
        None
    }
}
