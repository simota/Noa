//! Env-gated startup phase tracing (`NOA_STARTUP_TRACE=1`).
//!
//! Each [`mark`] prints one `NOA_STARTUP_TRACE <ms> <stage>` line to stderr
//! with a monotonic timestamp relative to [`init`] (process entry). Compiled
//! in always but fully inert — a single relaxed atomic-pointer load — unless
//! the env var is set, so it can stay in release builds for benchmarking.

use std::sync::OnceLock;
use std::time::Instant;

/// `None` until [`init`] runs; `Some(None)` when tracing is disabled;
/// `Some(Some(origin))` when enabled.
static ORIGIN: OnceLock<Option<Instant>> = OnceLock::new();

/// Capture the trace origin. Call as the first statement of `main`.
/// Idempotent; later callers keep the first origin.
pub fn init() {
    let enabled = std::env::var_os("NOA_STARTUP_TRACE").is_some_and(|v| v == "1");
    let _ = ORIGIN.set(enabled.then(Instant::now));
    mark("process-entry");
}

/// Emit one trace line for `stage`. No-op unless [`init`] saw
/// `NOA_STARTUP_TRACE=1`.
pub fn mark(stage: &str) {
    if let Some(Some(origin)) = ORIGIN.get() {
        eprintln!(
            "NOA_STARTUP_TRACE {:9.3}ms {stage}",
            origin.elapsed().as_secs_f64() * 1e3
        );
    }
}

/// Emit `stage` exactly once across the process lifetime (for per-frame call
/// sites that should only record their first occurrence).
pub fn mark_once(stage: &str, guard: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering;
    if ORIGIN.get().is_some_and(Option::is_some)
        && guard
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        mark(stage);
    }
}
