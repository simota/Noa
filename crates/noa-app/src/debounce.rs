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
