//! Env-gated tab-switch (occlusion-reveal) latency instrumentation
//! (`NOA_TAB_SWITCH_TRACE=1`).
//!
//! macOS tabs are separate windows: switching tabs occludes the outgoing
//! window and un-occludes the incoming one. This traces the incoming
//! window's reveal path:
//!
//! ```text
//! WindowEvent::Occluded(false)                              [t0]
//!   → configure_wgpu_surface (swapchain 1×1 → full size)    [t1]
//!   → request_redraw → redraw()
//!       → rebuild_panes (PaneRenderCache rebuild, possibly
//!         a full rebuild of every visible row)               [t2]
//!       → surface present()                                  [t3]
//! ```
//!
//! and logs `t1 − t0` (surface configure), `t2 − t1` (pane cache rebuild,
//! plus rows rebuilt), and `t3 − t0` (total reveal→present) to stderr.
//!
//! Zero cost when disabled: every public hook first checks one cached bool
//! (a `OnceLock` env read) and returns.
//!
//! Attribution model: the tracer is process-global and keeps a single
//! pending reveal (like `latency_trace`'s single pending keypress) — it is
//! meant for a one-window-at-a-time tab-switch benchmark, not for
//! attribution across concurrently occluding/revealing windows. A reveal
//! whose window never presents again (e.g. immediately re-occluded) never
//! logs; the next reveal simply replaces it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// `0` = no pending sample; real timestamps are nudged to ≥ 1.
static REVEAL_AT: AtomicU64 = AtomicU64::new(0);
static CONFIGURE_NS: AtomicU64 = AtomicU64::new(0);
static REBUILD_NS: AtomicU64 = AtomicU64::new(0);
static ROWS_REBUILT: AtomicU64 = AtomicU64::new(0);
/// Set when the pending reveal's frame took the fast path (presented the
/// renderer's already-cached instances instead of forcing a synchronous
/// rebuild). Read and cleared by `on_present`.
static FAST_PATH: AtomicBool = AtomicBool::new(false);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("NOA_TAB_SWITCH_TRACE").is_ok_and(|value| !value.is_empty() && value != "0")
    })
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    (Instant::now().saturating_duration_since(epoch()).as_nanos() as u64).max(1)
}

/// Main thread, on `WindowEvent::Occluded(false)`, before
/// `configure_wgpu_surface` runs. Starts a new sample (replacing any
/// unfinished one — see the attribution model above).
pub(crate) fn on_reveal_start() {
    if !enabled() {
        return;
    }
    CONFIGURE_NS.store(0, Ordering::Relaxed);
    REBUILD_NS.store(0, Ordering::Relaxed);
    ROWS_REBUILT.store(0, Ordering::Relaxed);
    FAST_PATH.store(false, Ordering::Relaxed);
    REVEAL_AT.store(now_ns(), Ordering::Relaxed);
}

/// Main thread, immediately before `configure_wgpu_surface` runs on the
/// reveal path. Returns `0` when disabled.
pub(crate) fn configure_start() -> u64 {
    if !enabled() { 0 } else { now_ns() }
}

/// Main thread, immediately after `configure_wgpu_surface` returns.
pub(crate) fn on_surface_configured(start: u64) {
    if start == 0 || !enabled() || REVEAL_AT.load(Ordering::Relaxed) == 0 {
        return;
    }
    CONFIGURE_NS.store(now_ns().saturating_sub(start), Ordering::Relaxed);
}

/// Main thread, at the top of `redraw()`'s `rebuild_panes` call. Returns `0`
/// when disabled.
pub(crate) fn rebuild_start() -> u64 {
    if !enabled() { 0 } else { now_ns() }
}

/// Main thread, immediately after `rebuild_panes` returns, with the row
/// count it reports via `Renderer::rows_rebuilt_last_frame()`.
pub(crate) fn on_pane_rebuild(start: u64, rows_rebuilt: u64) {
    if start == 0 || !enabled() || REVEAL_AT.load(Ordering::Relaxed) == 0 {
        return;
    }
    REBUILD_NS.store(now_ns().saturating_sub(start), Ordering::Relaxed);
    ROWS_REBUILT.store(rows_rebuilt, Ordering::Relaxed);
}

/// Main thread, when `redraw()` takes the reveal fast path (presents the
/// renderer's already-cached instances instead of rebuilding this frame).
pub(crate) fn on_fast_path_reveal() {
    if !enabled() || REVEAL_AT.load(Ordering::Relaxed) == 0 {
        return;
    }
    FAST_PATH.store(true, Ordering::Relaxed);
}

/// Main thread, at the top of a background pane-cache refresh for an occluded
/// window (see `App::background_refresh_pane_cache`). Returns `0` when
/// disabled.
pub(crate) fn bg_refresh_start() -> u64 {
    if !enabled() { 0 } else { now_ns() }
}

/// Main thread, immediately after a background refresh's `rebuild_panes`
/// returns. Logs its own line — independent of the reveal-sample state above,
/// since a background refresh has no `present()` to pair with.
pub(crate) fn on_bg_refresh(start: u64, rows_rebuilt: u64) {
    if start == 0 || !enabled() {
        return;
    }
    let us = now_ns().saturating_sub(start) / 1_000;
    eprintln!("[tab-switch-trace] bg-refresh {us}us rows={rows_rebuilt}");
}

/// Main thread, immediately after `SurfaceTexture::present()`. Closes the
/// pending reveal sample (if any) and logs the breakdown.
pub(crate) fn on_present() {
    if !enabled() {
        return;
    }
    let reveal_at = REVEAL_AT.swap(0, Ordering::Relaxed);
    if reveal_at == 0 {
        return;
    }
    let total_ns = now_ns().saturating_sub(reveal_at);
    let configure_ns = CONFIGURE_NS.load(Ordering::Relaxed);
    let rebuild_ns = REBUILD_NS.load(Ordering::Relaxed);
    let rows = ROWS_REBUILT.load(Ordering::Relaxed);
    let fast_path = FAST_PATH.swap(false, Ordering::Relaxed);
    eprintln!(
        "[tab-switch-trace] reveal→present {}us (surface-configure {}us, pane-rebuild {}us, rows_rebuilt={}){}",
        total_ns / 1_000,
        configure_ns / 1_000,
        rebuild_ns / 1_000,
        rows,
        if fast_path {
            " (fast-path, rows_rebuilt=0)"
        } else {
            ""
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The hooks must be inert when the env var is unset (the default in the
    // test runner): no state is written, no output produced. This pins the
    // zero-cost-when-disabled contract at its observable surface. Running
    // `cargo test` with `NOA_TAB_SWITCH_TRACE=1` set is legitimate (e.g. to
    // eyeball the trace output alongside the test run), so this test's
    // contract simply does not apply then — skip rather than assert-fail.
    #[test]
    fn disabled_hooks_leave_state_untouched() {
        if std::env::var("NOA_TAB_SWITCH_TRACE")
            .is_ok_and(|value| !value.is_empty() && value != "0")
        {
            eprintln!(
                "NOA_TAB_SWITCH_TRACE is set in the environment — skipping the \
                 disabled-hooks contract test"
            );
            return;
        }
        assert!(!enabled(), "test runner must not set NOA_TAB_SWITCH_TRACE");
        on_reveal_start();
        assert_eq!(configure_start(), 0);
        on_surface_configured(0);
        assert_eq!(rebuild_start(), 0);
        on_pane_rebuild(0, 42);
        on_fast_path_reveal();
        assert_eq!(bg_refresh_start(), 0);
        on_bg_refresh(0, 7);
        on_present();
        assert_eq!(REVEAL_AT.load(Ordering::Relaxed), 0);
        assert!(!FAST_PATH.load(Ordering::Relaxed));
        assert_eq!(CONFIGURE_NS.load(Ordering::Relaxed), 0);
        assert_eq!(REBUILD_NS.load(Ordering::Relaxed), 0);
        assert_eq!(ROWS_REBUILT.load(Ordering::Relaxed), 0);
    }
}
