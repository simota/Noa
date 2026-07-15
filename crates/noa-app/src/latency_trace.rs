//! Env-gated keypress→present latency instrumentation (`NOA_LATENCY_TRACE=1`).
//!
//! Measures the end-to-end input path a human keystroke travels:
//!
//! ```text
//! winit KeyboardInput (main thread)          → on_key_pressed()   [t0]
//!   → encode → PtyInputQueue → io thread → pty write
//!   → child echoes → pty read → parse into Terminal
//!                                (io thread) → on_pty_feed()      [t1]
//!   → UserEvent::Redraw → FrameSnapshot       (main thread, frame_start)
//!   → draw → surface present()                → on_present()      [t2]
//! ```
//!
//! and logs `t2 − t0` (plus the `t1 − t0` echo-fed split) to stderr. This is
//! a *present-call* proxy, not photon-to-glass: `wgpu`'s `present()` enqueues
//! the drawable for the next scan-out, so true glass latency adds up to one
//! refresh interval of compositor time on top of the logged value. With
//! `desired_maximum_frame_latency: 1` the drawable queue holds at most one
//! frame, so the proxy tracks glass latency to within a single vsync.
//!
//! Zero cost when disabled: every public hook first checks one cached bool
//! (a `OnceLock` env read) and returns.
//!
//! Attribution model: the tracer is process-global and keeps a single pending
//! keypress. A pty-output batch that arrives while a keypress is pending is
//! treated as its echo (t1), and the first present whose frame snapshot was
//! built after t1 closes the sample. Output unrelated to the keypress can
//! therefore steal the echo slot — the tracer is meant for a quiet
//! shell/`cat` echo benchmark, not for attribution under concurrent output.
//! A keypress that never produces output (keybind, swallowed by a modal,
//! non-echoing program) never logs; the next keypress simply replaces it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// `0` = empty in both slots below; real timestamps are nudged to ≥ 1.
static KEYPRESS_AT: AtomicU64 = AtomicU64::new(0);
static ECHO_FED_AT: AtomicU64 = AtomicU64::new(0);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("NOA_LATENCY_TRACE").is_ok_and(|value| !value.is_empty() && value != "0")
    })
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    (Instant::now().saturating_duration_since(epoch()).as_nanos() as u64).max(1)
}

/// Main thread, on every `WindowEvent::KeyboardInput` press. Starts a new
/// sample (replacing any unfinished one — see the attribution model above).
pub(crate) fn on_key_pressed() {
    if !enabled() {
        return;
    }
    ECHO_FED_AT.store(0, Ordering::Relaxed);
    KEYPRESS_AT.store(now_ns(), Ordering::Relaxed);
}

/// io thread, after a pty-output batch has been parsed into the `Terminal`.
/// The first batch after a pending keypress is taken as its echo.
pub(crate) fn on_pty_feed() {
    if !enabled() {
        return;
    }
    if KEYPRESS_AT.load(Ordering::Relaxed) != 0 && ECHO_FED_AT.load(Ordering::Relaxed) == 0 {
        ECHO_FED_AT.store(now_ns(), Ordering::Relaxed);
    }
}

/// Main thread, at the top of the redraw pass — *before* the `FrameSnapshot`
/// is built — so [`on_present`] can tell whether the presented frame's
/// snapshot could have contained the echo. Returns `0` when disabled.
pub(crate) fn frame_start() -> u64 {
    if !enabled() { 0 } else { now_ns() }
}

/// Main thread, immediately after `SurfaceTexture::present()`. Closes the
/// pending sample when this frame's snapshot was built after the echo was
/// fed (an earlier in-flight frame — e.g. a blink repaint — can present
/// between echo-fed and the echo frame; it is correctly skipped here).
pub(crate) fn on_present(frame_start_ns: u64) {
    if frame_start_ns == 0 || !enabled() {
        return;
    }
    let echo_fed = ECHO_FED_AT.load(Ordering::Relaxed);
    if echo_fed == 0 || frame_start_ns < echo_fed {
        return;
    }
    let keypress = KEYPRESS_AT.load(Ordering::Relaxed);
    if keypress == 0 {
        return;
    }
    KEYPRESS_AT.store(0, Ordering::Relaxed);
    ECHO_FED_AT.store(0, Ordering::Relaxed);
    let presented = now_ns();
    eprintln!(
        "[latency-trace] key→present {}us (echo fed +{}us)",
        (presented.saturating_sub(keypress)) / 1_000,
        (echo_fed.saturating_sub(keypress)) / 1_000,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The hooks must be inert when the env var is unset (the default in the
    // test runner): no state is written, no output produced. This pins the
    // zero-cost-when-disabled contract at its observable surface.
    #[test]
    fn disabled_hooks_leave_state_untouched() {
        assert!(!enabled(), "test runner must not set NOA_LATENCY_TRACE");
        on_key_pressed();
        on_pty_feed();
        assert_eq!(frame_start(), 0);
        on_present(0);
        assert_eq!(KEYPRESS_AT.load(Ordering::Relaxed), 0);
        assert_eq!(ECHO_FED_AT.load(Ordering::Relaxed), 0);
    }
}
