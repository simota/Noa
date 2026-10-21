//! The viewer's current local UTC offset, in seconds.
//!
//! The session sidebar stamps last-output timestamps as local civil wall-clock
//! (`session_store::WallClock`), so the io thread adds this offset to the
//! Unix-epoch second count before decomposing it. Isolated here because the
//! only portable source is platform-specific (Foundation on macOS, libc
//! `localtime_r` on other Unix); anywhere else it degrades to UTC (offset 0), which keeps the dominant relative forms
//! (`3m ago`, `2h ago`) exact and only shifts the absolute `Yday HH:MM` clock.
//!
//! Queried once per sidebar publish (a cheap Foundation/libc call), not cached, so a
//! DST transition or timezone change is picked up without app restart.

/// Seconds east of UTC for the machine's current local time zone.
#[cfg(target_os = "macos")]
pub(crate) fn local_offset_seconds() -> i64 {
    objc2_foundation::NSTimeZone::localTimeZone().secondsFromGMT() as i64
}

/// Other Unix: libc's `localtime_r` resolves `TZ` / `/etc/localtime` and
/// reports the offset (DST included) in `tm_gmtoff`.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn local_offset_seconds() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: `tm` is a plain C struct for which all-zero is a valid value;
    // `localtime_r` only writes into the buffer we own and is thread-safe.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff
}

/// Non-Unix fallback: stamp in UTC (see the module docs).
#[cfg(not(unix))]
pub(crate) fn local_offset_seconds() -> i64 {
    0
}

/// Current local civil wall-clock used by session sidebar timestamps and
/// auto-approve audit entries.
pub(crate) fn wall_clock_now() -> crate::session_store::WallClock {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    crate::session_store::civil_from_unix_secs(unix + local_offset_seconds())
}
