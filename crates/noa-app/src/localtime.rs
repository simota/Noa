//! The viewer's current local UTC offset, in seconds.
//!
//! The session sidebar stamps last-output timestamps as local civil wall-clock
//! (`session_store::WallClock`), so the io thread adds this offset to the
//! Unix-epoch second count before decomposing it. Isolated here because the
//! only portable source is platform-specific (Foundation on macOS); everywhere
//! else it degrades to UTC (offset 0), which keeps the dominant relative forms
//! ("3分前", "2時間前") exact and only shifts the absolute "昨日 HH:MM" clock.
//!
//! Queried once per sidebar publish (a cheap Foundation call), not cached, so a
//! DST transition or timezone change is picked up without app restart.

/// Seconds east of UTC for the machine's current local time zone.
#[cfg(target_os = "macos")]
pub(crate) fn local_offset_seconds() -> i64 {
    objc2_foundation::NSTimeZone::localTimeZone().secondsFromGMT() as i64
}

/// Non-macOS fallback: stamp in UTC (see the module docs).
#[cfg(not(target_os = "macos"))]
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
