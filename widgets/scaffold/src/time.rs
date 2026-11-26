//! Milliseconds since the epoch, and the UTC civil date of one.

use std::time::{SystemTime, UNIX_EPOCH};

pub type Millis = i64;

pub fn now_ms() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as Millis)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` of `ms` in UTC (Howard Hinnant's civil-from-days).
pub fn date(ms: Millis) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `HH:MM` of `ms` in UTC.
pub fn clock(ms: Millis) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// The local time zone's offset from UTC at `ms`, in milliseconds.
pub fn local_offset(ms: Millis) -> Millis {
    let secs = ms.div_euclid(1000) as libc::time_t;
    // SAFETY: `localtime_r` only writes the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as Millis * 1000 } else { 0 }
}

/// `YYYY-MM-DD HH:MM` of `ms` shifted by `offset` milliseconds.
pub fn stamp(ms: Millis, offset: Millis) -> String {
    format!("{} {}", date(ms + offset), clock(ms + offset))
}

/// Whether `s` is a `YYYY-MM-DD` date that exists.
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_732_600_000_000), "2024-11-26");
        assert_eq!(date(951_782_400_000), "2000-02-29");
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(clock(1_732_600_000_000), "05:46");
    }

    #[test]
    fn dates_are_checked() {
        assert!(is_date("2026-11-26"));
        assert!(is_date("2024-02-29"));
        assert!(!is_date("2026-02-29"));
        assert!(!is_date("2026-13-01"));
        assert!(!is_date("2026-1-01"));
        assert!(!is_date("２０２６-11-26"));
    }
}
