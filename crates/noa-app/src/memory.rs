//! Post-quiescence memory trimming.
//!
//! After a large output burst (flood, `cat`, TUI repaint storm) the malloc
//! zones keep the freed pages dirty — `footprint` reports them under
//! `MALLOC_SMALL`/`MALLOC_LARGE` as "reclaimable" but the process footprint
//! stays at its high-water mark until real system memory pressure arrives.
//! [`release_reclaimable_memory`] returns those pages to the OS eagerly; it is
//! fired from the event loop's `tick_memory_trim` once per burst, a few
//! seconds after the last pty-driven redraw (never on the hot path).

/// Return malloc's freed-but-dirty pages to the OS.
///
/// `malloc_zone_pressure_relief(NULL, 0)` walks every malloc zone and
/// madvises free pages away — the same relief the default allocator performs
/// under a system memory-pressure notification, minus the waiting. It only
/// touches free blocks, so live allocations (and therefore performance of
/// everything already resident) are unaffected. Costs single-digit
/// milliseconds after a large burst; callers must be quiescent.
///
/// Measured caveat: under the xzone allocator (macOS 26 on Apple Silicon)
/// the call reports 0 bytes relieved — xzone returns free pages on its own
/// schedule and ignores this hook. It still relieves the classic scalable
/// zones on older configurations, so the (cheap, one-shot) call stays.
pub(crate) fn release_reclaimable_memory() {
    #[cfg(target_os = "macos")]
    // SAFETY: `malloc_zone_pressure_relief` is a libSystem call that is safe
    // with a NULL zone (documented "all zones") and a 0 goal ("as much as
    // possible"); it does not invalidate live allocations.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// `<malloc/malloc.h>`: `size_t malloc_zone_pressure_relief(malloc_zone_t *zone, size_t goal)`.
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}
