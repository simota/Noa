//! Redraw pacing: floors how often the io thread pokes the winit event loop
//! to repaint, and bounds how long a redraw can be withheld under
//! synchronized output (DECSET 2026).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The longest the io thread withholds a redraw while an application holds
/// synchronized output (DECSET 2026) open. Mode 2026 has no standardized
/// timeout — the spec leaves it to the terminal, tmux uses 1s — and Ghostty
/// dodges the question by presenting on a vsync timer, so a frame left mid-sync
/// simply isn't shown until the next vsync after release. noa's renderer is
/// event-driven off pty output instead, so *suppressing the redraw request with
/// no fallback* strands the display on a stale frame whenever an app leaves
/// 2026 set across an input-wait, or (more often) when pty batching keeps
/// ending a coalesced read mid-frame during rapid repaints — e.g. holding an
/// arrow key to move through a Claude Code selection menu. Capping suppression
/// at the render cadence keeps the screen live: a well-behaved single frame
/// still paints atomically at its ESU (which arrives well within the cap, so
/// the cap never fires for it), while sustained back-to-back frames refresh at
/// ~10fps instead of freezing until output happens to stop on a frame boundary.
///
/// `pub(crate)`: `app::render`'s `sync_output_snapshot_decision` shares this
/// same cap for its own suppression window (how long a pane may keep
/// presenting a held snapshot instead of reading the terminal — see that
/// function's doc comment) so the two independent mode-2026 timeouts — one
/// gating redraw *requests* here, one gating the redraw *read* there — can
/// never drift apart into two different effective limits.
pub(crate) const SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION: Duration = Duration::from_millis(100);

/// Floor between consecutive redraw requests outside synchronized output.
/// Each `UserEvent::Redraw` is a real OS wake-up of the winit event loop and
/// the display can't present faster than its refresh, so a flood of parse
/// batches requesting one repaint each is pure overhead. A withheld redraw
/// arms the same trailing deadline as synchronized output, so a burst's
/// final frame always paints. This is the *fallback* used when a window's
/// actual refresh rate isn't known (one 120Hz frame); [`RedrawFloor`] derives
/// a tighter, monitor-accurate value at runtime via
/// [`redraw_floor_from_refresh_millihertz`].
pub(super) const REDRAW_MIN_INTERVAL: Duration = Duration::from_millis(8);

/// Sane clamp range for a monitor-derived redraw floor — guards against a
/// platform reporting an implausible refresh rate (e.g. 0 or absurdly high)
/// producing a floor that either busy-loops or stalls visibly.
const REDRAW_MIN_INTERVAL_FLOOR: Duration = Duration::from_millis(4);
const REDRAW_MIN_INTERVAL_CEILING: Duration = Duration::from_millis(33);

/// Derive the redraw floor from a window's monitor refresh rate
/// (`winit::monitor::MonitorHandle::refresh_rate_millihertz`), clamped to a
/// sane range and falling back to [`REDRAW_MIN_INTERVAL`] when the platform
/// can't report one (the query is best-effort — e.g. `None` under X11
/// without RandR). Pure and free of `winit::window::Window` so it's
/// unit-testable without a real window.
pub(crate) fn redraw_floor_from_refresh_millihertz(millihertz: Option<u32>) -> Duration {
    let derived = millihertz
        .filter(|&mhz| mhz > 0)
        .map(|mhz| Duration::from_nanos(1_000_000_000_000 / u64::from(mhz)));
    derived
        .unwrap_or(REDRAW_MIN_INTERVAL)
        .clamp(REDRAW_MIN_INTERVAL_FLOOR, REDRAW_MIN_INTERVAL_CEILING)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RedrawDecision {
    /// Ask the main thread to repaint now — the redraw floor (or, under
    /// synchronized output, the suppression cap) has elapsed since the last
    /// paint.
    Now,
    /// Withhold this feed's redraw. Wake and force one at `deadline` unless
    /// intervening output earns an immediate repaint first.
    Suppress { deadline: Instant },
}

/// Decide whether a just-fed batch should trigger a redraw, using the
/// default [`REDRAW_MIN_INTERVAL`] floor. Thin wrapper over
/// [`decide_redraw_floor`] kept so existing tests that don't care about a
/// monitor-derived floor need no change; [`RedrawFloor::decide`] (used by the
/// io thread itself) is the version that plugs in a runtime-derived
/// interval, so this is test-only now.
#[cfg(test)]
pub(super) fn decide_redraw(
    synchronized: bool,
    last_redraw: Option<Instant>,
    now: Instant,
) -> RedrawDecision {
    decide_redraw_floor(synchronized, last_redraw, now, REDRAW_MIN_INTERVAL)
}

/// Decide whether a just-fed batch should trigger a redraw. `synchronized` is
/// the terminal's DECSET 2026 state at end of batch; `last_redraw` is when the
/// io thread last actually asked the main thread to repaint. Two suppression
/// windows apply: outside synchronized output redraws are floored to
/// `min_interval` (ideally derived from the window's real refresh rate — see
/// [`redraw_floor_from_refresh_millihertz`]), so a flood of parse batches
/// can't wake the event loop faster than the display presents; a batch left
/// in synchronized output withholds its redraw (that is the point of mode
/// 2026) — but never for longer than [`SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`]
/// since the last paint, so a stalled or batch-straddled frame can't freeze
/// the screen. Either way a suppressed batch owes a redraw at `deadline`, so
/// a burst's final frame always paints.
pub(super) fn decide_redraw_floor(
    synchronized: bool,
    last_redraw: Option<Instant>,
    now: Instant,
    min_interval: Duration,
) -> RedrawDecision {
    let window = if synchronized {
        SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION
    } else {
        min_interval
    };
    match last_redraw {
        // Painted recently: hold this frame, but arm a deadline so the window
        // is enforced even if no further output arrives.
        Some(last) if now.saturating_duration_since(last) < window => RedrawDecision::Suppress {
            deadline: last + window,
        },
        // Never painted, or the window already elapsed: paint now.
        _ => RedrawDecision::Now,
    }
}

/// Fixed monotonic reference point every [`RedrawFloor`] timestamp is stored
/// relative to. An `Instant` can't live in an `AtomicU64` directly; a
/// process-wide nanosecond offset from a lazily-chosen epoch can.
fn redraw_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn nanos_since_epoch(at: Instant) -> u64 {
    at.saturating_duration_since(redraw_epoch()).as_nanos() as u64
}

fn instant_from_nanos(nanos: u64) -> Instant {
    redraw_epoch() + Duration::from_nanos(nanos)
}

/// Sentinel meaning "no redraw recorded yet" in [`RedrawFloor::last_redraw_at`].
/// Real timestamps are nudged to at least 1ns past the epoch (see
/// [`RedrawFloor::claim`]) so they never collide with it.
const NEVER: u64 = 0;

/// A window's redraw-floor clock, shared by every pane's io thread in that
/// window instead of each pane keeping its own. Without sharing, an N-pane
/// split earns up to N floored redraw wakes per floor window — extra event
/// loop wake-ups with no extra painted frames, since all panes render into
/// the same window on the same present. `min_interval` is written by the
/// main thread from the window's actual monitor refresh rate (falling back
/// to [`REDRAW_MIN_INTERVAL`]); `last_redraw_at` is written by whichever
/// pane's io thread wins the race to paint.
#[derive(Clone)]
pub(crate) struct RedrawFloor {
    last_redraw_at: Arc<AtomicU64>,
    min_interval_nanos: Arc<AtomicU64>,
}

impl RedrawFloor {
    pub(crate) fn new(min_interval: Duration) -> Self {
        // Force the epoch to exist now, before any real timestamp is ever
        // handed to this clock. `redraw_epoch()` is process-wide and set
        // once; if it instead initialized lazily on first use inside
        // `claim`/`last_redraw`, an `Instant` captured by the caller *before*
        // that first call (as every caller does — they read `Instant::now()`
        // then call in) could land before the epoch, and
        // `saturating_duration_since` would clamp it to zero.
        let _ = redraw_epoch();
        Self {
            last_redraw_at: Arc::new(AtomicU64::new(NEVER)),
            min_interval_nanos: Arc::new(AtomicU64::new(min_interval.as_nanos() as u64)),
        }
    }

    /// Called by the main thread when the window's refresh rate becomes
    /// known or changes (window creation, monitor change). `Relaxed`: io
    /// threads only need eventually-consistent visibility, not ordering
    /// against anything else.
    pub(crate) fn set_min_interval(&self, interval: Duration) {
        self.min_interval_nanos
            .store(interval.as_nanos() as u64, Ordering::Relaxed);
    }

    fn min_interval(&self) -> Duration {
        Duration::from_nanos(self.min_interval_nanos.load(Ordering::Relaxed))
    }

    fn last_redraw(&self) -> Option<Instant> {
        match self.last_redraw_at.load(Ordering::Acquire) {
            NEVER => None,
            nanos => Some(instant_from_nanos(nanos)),
        }
    }

    /// Records `at` as a redraw and reports whether it is the most recent
    /// one recorded so far. `fetch_max` makes this safe to call concurrently
    /// from every pane's io thread in the window: only the caller whose
    /// timestamp actually advances the clock gets `true` back, so panes that
    /// raced to the same floor deadline converge on a single winner instead
    /// of each sending its own wake.
    fn claim(&self, at: Instant) -> bool {
        let at_nanos = nanos_since_epoch(at).max(1); // never collide with NEVER (0)
        self.last_redraw_at.fetch_max(at_nanos, Ordering::AcqRel) < at_nanos
    }

    /// Unconditionally record a redraw that is happening regardless of the
    /// floor (e.g. one triggered by an unrelated per-pane throttle), so the
    /// shared clock stays accurate for other panes in this window.
    pub(super) fn record(&self, at: Instant) {
        let _ = self.claim(at);
    }

    /// Decide whether a just-fed batch should trigger a redraw against this
    /// window's shared clock (see [`decide_redraw_floor`]). A `Now` decision
    /// is recorded here so the next pane to ask — in this window, on any
    /// thread — sees it.
    pub(super) fn decide(&self, synchronized: bool, now: Instant) -> RedrawDecision {
        let decision =
            decide_redraw_floor(synchronized, self.last_redraw(), now, self.min_interval());
        if matches!(decision, RedrawDecision::Now) {
            self.claim(now);
        }
        decision
    }

    /// Attempt to fire an owed redraw deadline. Every pane suppressed within
    /// the same floor window computes the identical shared deadline (it's
    /// derived from this same clock), so without this guard they'd all fire
    /// in the same tick; `claim` lets exactly one through.
    pub(super) fn claim_deadline(&self, now: Instant) -> bool {
        self.claim(now)
    }

    /// [`RedrawFloor::decide`] for a batch that carries a user-input echo:
    /// the floor is bypassed and the repaint fires now. The floor exists to
    /// stop a flood of *program output* batches from waking the event loop
    /// faster than the display presents; a keystroke echo is the
    /// latency-critical path, its rate is bounded by typing/key-repeat speed
    /// (nowhere near the refresh rate), and withholding it up to one floor
    /// interval (~8ms) is a visible input-latency regression — e.g. when a
    /// blink-adjacent pane in the same window painted moments earlier and
    /// holds the shared clock. Synchronized output (DECSET 2026) is *not*
    /// bypassed: that suppression is an application-requested atomicity
    /// contract, not a pacing heuristic, so mid-sync input echoes keep
    /// deferring to [`decide_redraw_floor`]'s capped window.
    ///
    /// A `Now` result records on the shared clock (like `decide`) so other
    /// panes' floors account for this paint.
    pub(super) fn decide_input_echo(&self, synchronized: bool, now: Instant) -> RedrawDecision {
        if synchronized {
            return self.decide(true, now);
        }
        self.record(now);
        RedrawDecision::Now
    }
}
