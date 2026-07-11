//! A pure debounce timer state machine (theme-settings-ui R-9): the caller
//! `submit`s values as they arrive and `poll`s on its own schedule to learn
//! when a burst has quieted down long enough to fire the last submitted
//! value. No timers, no threads, no GPU — the caller supplies `now` on every
//! call, which is what makes this deterministic and unit-testable without a
//! real clock. Wired as `ThemeSettings::font_size_debounce`, submitted on
//! every font-size row edit and polled via `poll_font_size`; `pub mod` so it
//! is reachable from outside the crate.

use std::time::{Duration, Instant};

/// Debounces a burst of `submit`s down to a single fire of the last
/// submitted value, `window` after that last submit.
#[derive(Clone)]
pub struct Debouncer<T> {
    window: Duration,
    /// The most recent not-yet-fired value and the instant it was submitted,
    /// or `None` between bursts (nothing pending) and right after a fire
    /// (cleared so it can never fire twice for one burst).
    pending: Option<(T, Instant)>,
}

impl<T> Debouncer<T> {
    /// A debouncer that fires `window` after the last `submit` in a burst.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending: None,
        }
    }

    /// Record `value`, restarting the debounce window from `now`. Replaces
    /// (never accumulates) any earlier value still pending in this burst —
    /// only the last submitted value fires.
    pub fn submit(&mut self, value: T, now: Instant) {
        self.pending = Some((value, now));
    }

    /// If a pending value's window has elapsed as of `now`, fire it (`Some`)
    /// and clear the pending slot so it cannot fire again for the same
    /// burst. Returns `None` while still inside the window, or when nothing
    /// is pending (including right after a previous fire, until the next
    /// `submit`).
    pub fn poll(&mut self, now: Instant) -> Option<T> {
        let (_, last_submit) = self.pending.as_ref()?;
        if now.duration_since(*last_submit) < self.window {
            return None;
        }
        self.pending.take().map(|(value, _)| value)
    }

    /// Discard a pending, not-yet-fired value without firing it (the Esc
    /// path, R-16 edge case). Returns whether a value was actually
    /// discarded (`false` if nothing was pending).
    pub fn cancel(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// Whether a value is currently waiting out its debounce window. Lets a
    /// caller that polls on a timer (e.g. `App::tick_theme_settings_debounce`)
    /// only keep re-arming its wake-up while there is actually something to
    /// fire, instead of polling on a fixed interval for the debouncer's
    /// entire lifetime.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// A leading+trailing throttle: unlike [`Debouncer`] (which only ever fires the
/// *last* value once a burst quiets), this fires the *first* value in a burst
/// immediately, then coalesces the rest to at most one fire per `interval`, and
/// always delivers the final submitted value on a trailing fire once the burst
/// stops. It exists for continuous window resize (grid reflow item 1): the first
/// drag size must apply live (Ghostty parity), intermediate sizes coalesce so a
/// deep-scrollback reflow can't run on every cell-width boundary, and the final
/// authoritative size must always land.
///
/// Pure state machine like [`Debouncer`]: the caller supplies `now` on every
/// call, so it is deterministic and unit-testable without a real clock. Wired as
/// `WindowState::resize_throttle`, `submit`ted from `relayout_and_resize_window`
/// and `poll`ed from `App::tick_resize_throttle` (pumped in `about_to_wait`).
#[derive(Clone)]
pub struct Throttle<T> {
    interval: Duration,
    /// When a value was last fired (leading or trailing), or `None` before the
    /// first fire ever. Drives both the leading-edge readiness check and the
    /// trailing-fire deadline.
    last_fire: Option<Instant>,
    /// The most recent submitted value that has not yet fired, coalesced across
    /// a burst (only the latest is kept). `None` between bursts and right after
    /// any fire. Non-`None` implies `last_fire` is `Some` (a pending value is
    /// only stored when a leading fire already happened this interval).
    pending: Option<T>,
}

impl<T> Throttle<T> {
    /// A throttle that fires at most once per `interval`, on the leading edge
    /// and then on trailing edges.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_fire: None,
            pending: None,
        }
    }

    /// Submit a value. On the leading edge — the first submit ever, or the
    /// first after `interval` of quiet — it fires immediately and the value is
    /// returned (`Some`) for the caller to apply now. Otherwise it is coalesced
    /// into the pending slot (replacing any earlier not-yet-fired value) and
    /// `None` is returned; it will fire from [`Self::poll`] once `interval`
    /// since the last fire elapses.
    #[must_use]
    pub fn submit(&mut self, value: T, now: Instant) -> Option<T> {
        let ready = match self.last_fire {
            None => true,
            Some(last) => now.duration_since(last) >= self.interval,
        };
        if ready {
            self.last_fire = Some(now);
            self.pending = None;
            Some(value)
        } else {
            self.pending = Some(value);
            None
        }
    }

    /// Fire the coalesced pending value if `interval` since the last fire has
    /// elapsed as of `now`, returning it (`Some`) for the caller to apply.
    /// Returns `None` while still inside the interval or when nothing is
    /// pending. Always delivers the *final* submitted value of a burst — a
    /// caller that keeps polling until [`Self::next_deadline`] is `None` can
    /// never drop the authoritative last size.
    #[must_use]
    pub fn poll(&mut self, now: Instant) -> Option<T> {
        let last = self.last_fire?;
        self.pending.as_ref()?;
        if now.duration_since(last) < self.interval {
            return None;
        }
        self.last_fire = Some(now);
        self.pending.take()
    }

    /// The next instant [`Self::poll`] should be attempted — `last_fire +
    /// interval` while a value is pending, else `None` (nothing to fire, so no
    /// wake-up needs scheduling).
    pub fn next_deadline(&self) -> Option<Instant> {
        let last = self.last_fire?;
        self.pending.as_ref().map(|_| last + self.interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_millis(150);

    // AC-6: a burst of submits <150ms apart fires exactly once, with the
    // last submitted value, once 150ms have elapsed since that last submit.
    #[test]
    fn burst_fires_once_with_last_value_after_window_elapses() {
        let mut debouncer: Debouncer<u32> = Debouncer::new(WINDOW);
        let t0 = Instant::now();

        debouncer.submit(1, t0);
        debouncer.submit(2, t0 + Duration::from_millis(50));
        debouncer.submit(3, t0 + Duration::from_millis(100));

        // Only 100ms since the last submit (at t0+100ms) — still inside the
        // 150ms window, so nothing fires yet even though 200ms have passed
        // since the very first submit.
        assert_eq!(debouncer.poll(t0 + Duration::from_millis(200)), None);

        // 150ms after the last submit: fires, and with the last value (3),
        // not the first (1) or a middle one (2).
        assert_eq!(debouncer.poll(t0 + Duration::from_millis(250)), Some(3));

        // At most once per burst: polling again later still returns None.
        assert_eq!(debouncer.poll(t0 + Duration::from_millis(500)), None);
    }

    // Esc path (R-16): cancel discards a pending value so it never fires,
    // and reports whether it actually discarded anything.
    #[test]
    fn cancel_discards_pending_value_and_reports_it() {
        let mut debouncer: Debouncer<u32> = Debouncer::new(WINDOW);
        let t0 = Instant::now();

        debouncer.submit(1, t0);
        assert!(debouncer.cancel(), "a pending value was discarded");
        assert!(!debouncer.cancel(), "nothing left to discard");
        assert_eq!(
            debouncer.poll(t0 + WINDOW),
            None,
            "a cancelled value must never fire"
        );
    }

    // A submit after a fire starts a fresh burst rather than being ignored
    // or immediately re-firing the old value.
    #[test]
    fn submit_after_fire_starts_a_new_burst() {
        let mut debouncer: Debouncer<u32> = Debouncer::new(WINDOW);
        let t0 = Instant::now();

        debouncer.submit(1, t0);
        assert_eq!(debouncer.poll(t0 + WINDOW), Some(1));

        let t1 = t0 + Duration::from_millis(400);
        debouncer.submit(2, t1);
        assert_eq!(
            debouncer.poll(t1 + Duration::from_millis(50)),
            None,
            "the new burst has its own fresh window"
        );
        assert_eq!(debouncer.poll(t1 + WINDOW), Some(2));
    }

    // Cancelling a debouncer that never received a submit is a harmless
    // no-op, not a panic or a spurious `true`.
    #[test]
    fn cancel_on_empty_debouncer_is_a_no_op() {
        let mut debouncer: Debouncer<u32> = Debouncer::new(WINDOW);
        assert!(!debouncer.cancel());
    }
}

#[cfg(test)]
mod throttle_tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_millis(80);

    // Leading edge: the very first submit fires immediately (a drag's first
    // size applies live, not after a delay).
    #[test]
    fn first_submit_fires_immediately() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        assert_eq!(throttle.submit(1, t0), Some(1));
        // Nothing pending after a leading fire, so no wake-up is scheduled.
        assert_eq!(throttle.next_deadline(), None);
    }

    // Mid-drag: submits within the interval after the leading fire are
    // coalesced (return None, don't fire), and only the latest is retained —
    // the deep-scrollback reflow can't run on every cell-width boundary.
    #[test]
    fn mid_drag_submits_coalesce_and_keep_only_the_latest() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        assert_eq!(throttle.submit(1, t0), Some(1));

        assert_eq!(throttle.submit(2, t0 + Duration::from_millis(10)), None);
        assert_eq!(throttle.submit(3, t0 + Duration::from_millis(20)), None);
        assert_eq!(throttle.submit(4, t0 + Duration::from_millis(30)), None);

        // A wake-up is now scheduled one interval after the leading fire.
        assert_eq!(throttle.next_deadline(), Some(t0 + INTERVAL));
        // Still inside the interval: nothing fires yet.
        assert_eq!(throttle.poll(t0 + Duration::from_millis(40)), None);
        // At the interval boundary the *latest* coalesced value (4) fires —
        // not the leading value (1) or any middle one (2, 3).
        assert_eq!(throttle.poll(t0 + INTERVAL), Some(4));
    }

    // Trailing: once the drag stops, the final submitted size is always
    // delivered by a later poll, even though it was coalesced.
    #[test]
    fn trailing_poll_always_delivers_the_final_value() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        assert_eq!(throttle.submit(1, t0), Some(1));
        // Final drag size, submitted while still inside the interval.
        assert_eq!(throttle.submit(99, t0 + Duration::from_millis(50)), None);
        // The drag has ended; no further submits arrive. The trailing fire
        // still delivers 99 at the interval boundary.
        assert!(throttle.next_deadline().is_some());
        assert_eq!(throttle.poll(t0 + INTERVAL), Some(99));
        // Drained: nothing left pending, no wake-up scheduled.
        assert_eq!(throttle.next_deadline(), None);
        assert_eq!(throttle.poll(t0 + INTERVAL * 2), None);
    }

    // A sustained drag (submit every 10ms for ~200ms) fires roughly once per
    // interval — bounded reflow frequency — while the last size still lands.
    #[test]
    fn sustained_drag_fires_about_once_per_interval() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        let mut fires = Vec::new();

        // 21 frames, 10ms apart (0..=200ms). Each frame submits, then a poll
        // models the event loop's `about_to_wait` pass at the same instant.
        for step in 0..=20u32 {
            let now = t0 + Duration::from_millis(u64::from(step) * 10);
            if let Some(v) = throttle.submit(step, now) {
                fires.push(v);
            }
            if let Some(v) = throttle.poll(now) {
                fires.push(v);
            }
        }
        // Drain the trailing fire after the drag stops.
        if let Some(deadline) = throttle.next_deadline()
            && let Some(v) = throttle.poll(deadline)
        {
            fires.push(v);
        }

        // Leading (frame 0) + one fire per ~80ms across 200ms ≈ 3-4 total —
        // far fewer than the 21 submits, so the reflow is bounded.
        assert!(
            (2..=4).contains(&fires.len()),
            "expected ~1 fire per interval, got {fires:?}"
        );
        assert_eq!(fires.first(), Some(&0), "leading value fires first");
        assert_eq!(
            fires.last(),
            Some(&20),
            "final submitted value must always be delivered"
        );
    }

    // After the interval elapses with no activity, the next submit is a fresh
    // leading edge (fires immediately) rather than being coalesced.
    #[test]
    fn submit_after_quiet_period_fires_leading_again() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        assert_eq!(throttle.submit(1, t0), Some(1));
        // Well past the interval, nothing pending in between.
        let t1 = t0 + INTERVAL + Duration::from_millis(500);
        assert_eq!(throttle.submit(2, t1), Some(2));
    }

    // An idle throttle (no submits, or fully drained) schedules no wake-up and
    // yields nothing to poll — no busy-polling.
    #[test]
    fn idle_throttle_is_quiet() {
        let mut throttle: Throttle<u32> = Throttle::new(INTERVAL);
        let t0 = Instant::now();
        assert_eq!(throttle.next_deadline(), None);
        assert_eq!(throttle.poll(t0), None);
    }
}
