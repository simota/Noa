//! Minted ids and server timestamps (`docs/specs/board-timeline.md` L2-4).
//!
//! Every id is `<prefix>` + 16 lowercase hex digits drawn from
//! `rand::thread_rng`; the caller re-draws on a collision with the ids it
//! already holds (or, for a board, with an existing directory). Display
//! labels never become ids (§5).

use std::time::{SystemTime, UNIX_EPOCH};

use noa_ipc::timeline::IdKind;
use rand::RngCore;

/// A fresh id of `kind` that `taken` does not reject.
pub(crate) fn mint(kind: IdKind, taken: impl Fn(&str) -> bool) -> String {
    loop {
        let id = format!("{}{:016x}", kind.prefix(), rand::thread_rng().next_u64());
        if !taken(&id) {
            return id;
        }
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The current time as RFC 3339 UTC with milliseconds,
/// `YYYY-MM-DDTHH:MM:SS.fffZ` (the form the contract's timestamps take).
pub(crate) fn now_utc() -> String {
    utc_from_millis(now_millis())
}

pub(crate) fn utc_from_millis(ms: u64) -> String {
    let secs = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
    let clock = crate::session_store::civil_from_unix_secs(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        clock.year,
        clock.month,
        clock.day,
        clock.hour,
        clock.minute,
        secs.rem_euclid(60),
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::timeline::valid_id;

    #[test]
    fn minted_ids_have_the_contract_shape_and_skip_taken_ones() {
        for kind in [IdKind::Board, IdKind::Worker, IdKind::Run, IdKind::Post] {
            assert!(valid_id(kind, &mint(kind, |_| false)));
        }
        let first = std::cell::Cell::new(true);
        let id = mint(IdKind::Post, |_| first.replace(false));
        assert!(valid_id(IdKind::Post, &id));
    }

    #[test]
    fn utc_timestamps_are_rfc3339_with_millis() {
        assert_eq!(utc_from_millis(0), "1970-01-01T00:00:00.000Z");
        // 2024-02-29T23:59:59.123Z (a leap day).
        assert_eq!(
            utc_from_millis(1_709_251_199_123),
            "2024-02-29T23:59:59.123Z"
        );
    }
}
