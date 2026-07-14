//! Timer-driven `App` operations: cursor blink, attention blink, sidebar clock,
//! and delayed overview redraw wake-ups.

use super::*;

/// Whether cached cursor state (see `Surface::cursor_blink_state`) is
/// currently blink-eligible: visible, at the live viewport (not scrolled into
/// scrollback), and rendered in one of the `Blinking*` DECSCUSR styles.
fn cursor_blink_state_wants_blink(cached: &CursorBlinkState) -> bool {
    cached.visible
        && cached.at_live_viewport
        && matches!(
            cached.style,
            CursorStyle::BlinkingBlock
                | CursorStyle::BlinkingUnderline
                | CursorStyle::BlinkingBar
                | CursorStyle::BlinkingBlockHollow
        )
}

fn cursor_blink_focus_gate<Window: Copy + PartialEq>(
    sticky_focused: Option<Window>,
    os_focused: Option<Window>,
    window_id: Window,
    occluded: bool,
) -> bool {
    sticky_focused == Some(window_id) && os_focused == Some(window_id) && !occluded
}

const LIVE_WALLPAPER_FADE_DURATION: Duration = Duration::from_secs(2);
const LIVE_WALLPAPER_FADE_FRAME_INTERVAL: Duration = Duration::from_millis(16);

fn live_wallpaper_timer_step(
    deadline: Option<Instant>,
    now: Instant,
    interval: Duration,
    eligible: bool,
    wants_rotation: bool,
) -> (Option<Instant>, bool) {
    if !eligible || !wants_rotation {
        return (None, false);
    }
    match deadline {
        None => (Some(now + interval), false),
        Some(deadline) if now < deadline => (Some(deadline), false),
        Some(_) => (Some(now + interval), true),
    }
}

fn live_wallpaper_fade_progress(
    started_at: Instant,
    now: Instant,
    duration: Duration,
) -> (f32, bool) {
    if duration.is_zero() {
        return (1.0, true);
    }
    let elapsed = now.saturating_duration_since(started_at);
    if elapsed >= duration {
        return (1.0, true);
    }
    let linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    let eased = linear * linear * (3.0 - 2.0 * linear);
    (eased, false)
}

impl App {
    /// Whether the focused pane has a displayable `Blinking*` cursor. Reads
    /// `Surface::cursor_blink_state` — refreshed by `redraw` under the
    /// terminal lock it already takes per pane — instead of locking the
    /// terminal here: this runs on every blink-interval wake, and `redraw`
    /// already pays for the read this tick would otherwise duplicate.
    fn focused_cursor_wants_blink(&self) -> bool {
        let Some(window_id) = self.focused else {
            return false;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return false;
        };
        if !cursor_blink_focus_gate(self.focused, self.os_focused, window_id, state.occluded) {
            return false;
        }
        let Some(surface) = state.focused_surface() else {
            return false;
        };
        cursor_blink_state_wants_blink(&surface.cursor_blink_state)
    }

    pub(super) fn reset_cursor_blink_phase(&mut self) {
        self.cursor_blink_visible = true;
        self.cursor_blink_deadline = None;
    }

    /// Advance the cursor blink phase and return the next wake-up deadline.
    pub(super) fn tick_cursor_blink(&mut self) -> Option<Instant> {
        if !self.focused_cursor_wants_blink() {
            self.reset_cursor_blink_phase();
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

    fn live_wallpaper_eligible(&self) -> bool {
        self.os_focused.is_some() && self.windows.values().any(|state| !state.occluded)
    }

    fn live_wallpaper_interval(&self) -> Duration {
        Duration::from_secs(
            self.config
                .background_image_interval_secs
                .max(noa_config::MIN_BACKGROUND_IMAGE_INTERVAL_SECS),
        )
    }

    pub(super) fn sync_current_background_image_to_window(&mut self, window_id: WindowId) {
        self.sync_background_image_to_window(window_id, Instant::now());
    }

    fn sync_background_image_to_window(&mut self, window_id: WindowId, now: Instant) {
        let transition = self.live_wallpaper_transition.as_ref().map(|transition| {
            let (progress, _) =
                live_wallpaper_fade_progress(transition.started_at, now, transition.duration);
            (
                transition.previous.clone(),
                transition.current.clone(),
                progress,
            )
        });
        let image = self.background_image.current_image();
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if let Some((previous, current, progress)) = transition {
            state.renderer.set_background_image_transition(
                &gpu.device,
                &gpu.queue,
                previous,
                current,
                progress,
            );
        } else {
            state
                .renderer
                .set_background_image(&gpu.device, &gpu.queue, image);
        }
    }

    fn sync_current_background_image_to_visible_windows(&mut self) {
        let image = self.background_image.current_image();
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        for state in self.windows.values_mut() {
            if state.occluded {
                continue;
            }
            state
                .renderer
                .set_background_image(&gpu.device, &gpu.queue, image.clone());
            state.window.request_redraw();
        }
    }

    fn begin_live_wallpaper_transition(
        &mut self,
        previous: Option<noa_render::BackgroundImage>,
        current: Option<noa_render::BackgroundImage>,
        now: Instant,
    ) {
        self.live_wallpaper_transition = Some(LiveWallpaperTransition {
            previous,
            current,
            started_at: now,
            duration: LIVE_WALLPAPER_FADE_DURATION,
        });
        self.sync_live_wallpaper_transition_to_visible_windows(now);
    }

    fn sync_live_wallpaper_transition_to_visible_windows(&mut self, now: Instant) {
        let Some(transition) = self.live_wallpaper_transition.as_ref() else {
            return;
        };
        let (progress, _) =
            live_wallpaper_fade_progress(transition.started_at, now, transition.duration);
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        for state in self.windows.values_mut() {
            if state.occluded {
                continue;
            }
            state.renderer.set_background_image_transition(
                &gpu.device,
                &gpu.queue,
                transition.previous.clone(),
                transition.current.clone(),
                progress,
            );
            state.window.request_redraw();
        }
    }

    fn update_live_wallpaper_transition_progress(&mut self, progress: f32) {
        for state in self.windows.values_mut() {
            if state.occluded {
                continue;
            }
            state
                .renderer
                .set_background_image_transition_progress(progress);
            state.window.request_redraw();
        }
    }

    /// Advance Kitty-graphics animations across every pane and return the next
    /// frame-due wake-up. `noa-grid` holds no timer: it advances against a
    /// monotonic ms clock (`kitty_anim_origin`) supplied here, and reports the
    /// soonest next deadline, which this schedules like the other tick sources.
    pub(super) fn tick_kitty_animations(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let origin = self.kitty_anim_origin.unwrap_or(now);
        let now_ms = now.saturating_duration_since(origin).as_millis() as u64;

        let mut next_wake_ms: Option<u64> = None;
        let mut any_running = false;
        let mut dirty_windows: Vec<WindowId> = Vec::new();
        for (window_id, state) in &self.windows {
            let mut changed = false;
            for surface in state.surfaces.values() {
                // Cheap atomic poll before locking: the common case is no
                // animation anywhere, and this skips the terminal lock
                // entirely for it instead of taking it just to ask.
                if !surface.kitty_animation_flag.load(Ordering::Relaxed) {
                    continue;
                }
                any_running = true;
                let mut term = surface.terminal.lock();
                let tick = term.advance_kitty_animations(now_ms);
                changed |= tick.changed;
                if let Some(w) = tick.next_wake {
                    next_wake_ms = Some(next_wake_ms.map_or(w, |cur| cur.min(w)));
                }
            }
            if changed {
                dirty_windows.push(*window_id);
            }
        }

        if !any_running {
            self.kitty_anim_origin = None;
            self.kitty_anim_deadline = None;
            return None;
        }
        self.kitty_anim_origin = Some(origin);
        for window_id in dirty_windows {
            if let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
        }
        self.kitty_anim_deadline = next_wake_ms.map(|ms| origin + Duration::from_millis(ms));
        self.kitty_anim_deadline
    }

    pub(super) fn tick_live_wallpaper(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let eligible = self.live_wallpaper_eligible();
        if !eligible {
            self.live_wallpaper_deadline = None;
            if self.live_wallpaper_transition.take().is_some() {
                self.sync_current_background_image_to_visible_windows();
            }
            return None;
        }
        if let Some(transition) = self.live_wallpaper_transition.as_ref() {
            let (progress, finished) =
                live_wallpaper_fade_progress(transition.started_at, now, transition.duration);
            if finished {
                self.live_wallpaper_transition = None;
                self.sync_current_background_image_to_visible_windows();
                return self.live_wallpaper_deadline;
            }
            self.update_live_wallpaper_transition_progress(progress);
            let frame_deadline = now + LIVE_WALLPAPER_FADE_FRAME_INTERVAL;
            return Some(match self.live_wallpaper_deadline {
                Some(rotation_deadline) => frame_deadline.min(rotation_deadline),
                None => frame_deadline,
            });
        }
        let (deadline, should_rotate) = live_wallpaper_timer_step(
            self.live_wallpaper_deadline,
            now,
            self.live_wallpaper_interval(),
            eligible,
            self.background_image.wants_rotation(),
        );
        self.live_wallpaper_deadline = deadline;
        if should_rotate {
            let previous = self.background_image.current_image();
            if self.background_image.advance() {
                let current = self.background_image.current_image();
                self.begin_live_wallpaper_transition(previous, current, now);
                return Some(now + LIVE_WALLPAPER_FADE_FRAME_INTERVAL);
            }
        }
        if !self.background_image.wants_rotation() {
            self.live_wallpaper_deadline = None;
        }
        self.live_wallpaper_deadline
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
            if let Some(toast) = &state.resize_overlay {
                if now >= toast.until {
                    state.resize_overlay = None;
                    state.window.request_redraw();
                } else {
                    let until = toast.until;
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

    /// Poll the open theme-settings overlay's font-size debouncer (R-9) and
    /// its R-7/C-5 post-Reset flash: if a font-size burst has settled, apply
    /// it via the real runtime-font-size path; if the flash just elapsed,
    /// clear it and force one more redraw (nothing else would repaint an
    /// otherwise-idle overlay right at that instant). Reports the next
    /// wake-up only while something is still pending — an idle overlay (no
    /// font-size edits, no recent reset) never re-arms this tick at all
    /// (NFR-2: no busy-polling).
    pub(super) fn tick_theme_settings_debounce(&mut self) -> Option<Instant> {
        let session = self.theme_settings.as_mut()?;
        let now = Instant::now();
        if let Some(value) = std::sync::Arc::make_mut(&mut session.state).poll_font_size(now) {
            let window_id = session.window_id;
            self.apply_runtime_font_size(window_id, value);
        }
        let session = self.theme_settings.as_mut()?;
        let window_id = session.window_id;
        if std::sync::Arc::make_mut(&mut session.state).poll_reset_flash(now) {
            self.request_window_redraw(window_id);
        }
        self.theme_settings.as_ref().and_then(|session| {
            let debounce_deadline = session
                .state
                .font_size_debounce_pending()
                .then_some(now + Duration::from_millis(30));
            let flash_deadline = session.state.reset_flash_deadline();
            match (debounce_deadline, flash_deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            }
        })
    }

    /// Fire any window's due grid-reflow throttle (item 1) and report the
    /// earliest pending trailing wake-up. Each window's `resize_throttle`
    /// coalesced its mid-drag relayouts; this delivers the trailing apply —
    /// including the final authoritative size once the drag stops — via the
    /// same `about_to_wait` + `WaitUntil` mechanism the other ticks use. The
    /// apply reflows the grid (grid-first) and sends the pty winsize; a pane
    /// that closed mid-drag is skipped inside `apply_pane_grid_resize`, and a
    /// window with nothing pending schedules no wake-up (no busy-polling).
    pub(super) fn tick_resize_throttle(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let copy_resize_due = self.copy_mode.as_ref().is_some_and(|session| {
            self.windows
                .get(&session.window_id)
                .and_then(|state| state.resize_throttle.next_deadline())
                .is_some_and(|deadline| deadline <= now)
        });
        if copy_resize_due {
            self.end_copy_mode();
        }
        for state in self.windows.values_mut() {
            if let Some(targets) = state.resize_throttle.poll(now) {
                apply_pane_grid_resize(state, &targets);
                // The reflowed grid must repaint; the surface/rects were
                // already live, so this shows the freshly reflowed content.
                state.window.request_redraw();
            }
            if let Some(deadline) = state.resize_throttle.next_deadline() {
                next = Some(next.map_or(deadline, |n| n.min(deadline)));
            }
        }
        next
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

#[cfg(test)]
mod tests {
    use super::{
        LIVE_WALLPAPER_FADE_DURATION, cursor_blink_focus_gate, cursor_blink_state_wants_blink,
        live_wallpaper_fade_progress, live_wallpaper_timer_step,
    };
    use crate::app::state::CursorBlinkState;
    use noa_grid::CursorStyle;
    use std::time::{Duration, Instant};

    /// `tick_cursor_blink` reads this cache instead of locking the terminal
    /// (see `Surface::cursor_blink_state`), so the gate it feeds must react
    /// to every field the old locked read checked: visibility, viewport
    /// position, and DECSCUSR style — a stale-cache bug here would silently
    /// keep (or stop) blinking regardless of the pane's real state.
    #[test]
    fn cursor_blink_state_wants_blink_checks_visibility_viewport_and_style() {
        let blinking_and_live = CursorBlinkState {
            visible: true,
            style: CursorStyle::BlinkingBar,
            at_live_viewport: true,
        };
        assert!(cursor_blink_state_wants_blink(&blinking_and_live));

        assert!(
            !cursor_blink_state_wants_blink(&CursorBlinkState {
                visible: false,
                ..blinking_and_live
            }),
            "an invisible cursor must never blink even if the style is Blinking*"
        );
        assert!(
            !cursor_blink_state_wants_blink(&CursorBlinkState {
                at_live_viewport: false,
                ..blinking_and_live
            }),
            "scrolled into scrollback must not blink the (not-drawn) live cursor"
        );
        assert!(
            !cursor_blink_state_wants_blink(&CursorBlinkState {
                style: CursorStyle::SteadyBar,
                ..blinking_and_live
            }),
            "a Steady* style must never blink regardless of visibility/viewport"
        );
    }

    /// `Surface::cursor_blink_state` is refreshed only inside `redraw`, which
    /// returns before reaching the per-pane cache refresh while
    /// `state.occluded` is true (see `redraw`'s early `if state.occluded {
    /// return; }` in `render.rs`). So on re-occlusion the cache can still say
    /// "blink-eligible" from just before the window was hidden.
    /// `focused_cursor_wants_blink` combines `cursor_blink_focus_gate` with
    /// this cache, and must not blink on a stale cache alone — occlusion has
    /// to independently veto it regardless of what the cache says.
    #[test]
    fn occluded_window_suppresses_blink_even_with_a_stale_blink_eligible_cache() {
        let stale_cache_says_blink = CursorBlinkState {
            visible: true,
            style: CursorStyle::BlinkingBlock,
            at_live_viewport: true,
        };
        assert!(
            cursor_blink_state_wants_blink(&stale_cache_says_blink),
            "test setup: the cache alone must read as blink-eligible"
        );
        assert!(
            !cursor_blink_focus_gate(Some(1_u8), Some(1_u8), 1_u8, true),
            "occlusion must veto blink regardless of a stale blink-eligible cache"
        );
    }

    #[test]
    fn cursor_blink_focus_gate_requires_real_os_focus_and_visibility() {
        assert!(cursor_blink_focus_gate(Some(1_u8), Some(1_u8), 1_u8, false));
        assert!(
            !cursor_blink_focus_gate(Some(1_u8), None, 1_u8, false),
            "sticky focus alone must not keep cursor blink armed while the app is backgrounded"
        );
        assert!(!cursor_blink_focus_gate(
            Some(1_u8),
            Some(2_u8),
            1_u8,
            false
        ));
        assert!(!cursor_blink_focus_gate(Some(1_u8), Some(1_u8), 1_u8, true));
        assert!(!cursor_blink_focus_gate(None, Some(1_u8), 1_u8, false));
    }

    #[test]
    fn live_wallpaper_timer_arms_waits_and_rotates_one_step_when_due() {
        let now = Instant::now();
        let interval = Duration::from_secs(5);

        let (deadline, rotate) = live_wallpaper_timer_step(None, now, interval, true, true);
        assert_eq!(deadline, Some(now + interval));
        assert!(!rotate);

        let (deadline, rotate) =
            live_wallpaper_timer_step(deadline, now + Duration::from_secs(4), interval, true, true);
        assert_eq!(deadline, Some(now + interval));
        assert!(!rotate);

        let (deadline, rotate) =
            live_wallpaper_timer_step(deadline, now + interval, interval, true, true);
        assert_eq!(deadline, Some(now + interval + interval));
        assert!(rotate);
    }

    #[test]
    fn live_wallpaper_timer_does_not_catch_up_after_background_pause() {
        let now = Instant::now();
        let interval = Duration::from_secs(5);
        let (deadline, rotate) = live_wallpaper_timer_step(None, now, interval, true, true);
        assert_eq!(deadline, Some(now + interval));
        assert!(!rotate);

        let paused = now + Duration::from_secs(20);
        let (deadline, rotate) = live_wallpaper_timer_step(deadline, paused, interval, false, true);
        assert_eq!(deadline, None);
        assert!(!rotate);

        let (deadline, rotate) = live_wallpaper_timer_step(None, paused, interval, true, true);
        assert_eq!(deadline, Some(paused + interval));
        assert!(
            !rotate,
            "resume must arm a fresh interval instead of replaying missed rotations"
        );
    }

    #[test]
    fn live_wallpaper_timer_disarms_without_rotation_work() {
        let now = Instant::now();
        let interval = Duration::from_secs(5);
        let stale_deadline = Some(now - Duration::from_secs(5));

        let (deadline, rotate) =
            live_wallpaper_timer_step(stale_deadline, now, interval, true, false);
        assert_eq!(deadline, None);
        assert!(!rotate);
    }

    #[test]
    fn live_wallpaper_fade_progress_clamps_and_completes() {
        let now = Instant::now();
        let duration = LIVE_WALLPAPER_FADE_DURATION;
        assert_eq!(duration, Duration::from_secs(2));

        assert_eq!(
            live_wallpaper_fade_progress(now, now - Duration::from_millis(10), duration),
            (0.0, false)
        );
        let (mid, done) = live_wallpaper_fade_progress(now, now + duration / 2, duration);
        assert!(
            (mid - 0.5).abs() < 0.001,
            "smoothstep midpoint should be 0.5: {mid}"
        );
        assert!(!done);
        assert_eq!(
            live_wallpaper_fade_progress(now, now + duration, duration),
            (1.0, true)
        );
        assert_eq!(
            live_wallpaper_fade_progress(now, now, Duration::ZERO),
            (1.0, true)
        );
    }
}
