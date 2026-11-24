//! Unix times, RFC 3339 parsing and civil dates in a time zone, without a
//! date crate. Times are `i64` unix seconds.

use serde::{Deserialize, Serialize};

use crate::model::Unix;

/// A time zone as a UTC offset at a moment.
pub trait Tz: Send + Sync {
    fn offset_secs(&self, at: Unix) -> i32;
}

/// The system's local zone (`localtime_r`).
pub struct SystemTz;

impl Tz for SystemTz {
    fn offset_secs(&self, at: Unix) -> i32 {
        let t = at as libc::time_t;
        // SAFETY: `tm` is plain data; `localtime_r` fills it for a valid `t`.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
        if ok { tm.tm_gmtoff as i32 } else { 0 }
    }
}

/// A constant offset in seconds east of UTC.
pub struct FixedTz(pub i32);

impl Tz for FixedTz {
    fn offset_secs(&self, _at: Unix) -> i32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LocalDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub date: LocalDate,
    pub hour: u8,
    /// 0 = Monday.
    pub weekday: u8,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(date: LocalDate) -> i64 {
    let y = i64::from(date.year) - i64::from(date.month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(date.month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(date.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> LocalDate {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    LocalDate { year, month, day }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// RFC 3339 (`Z` or `±hh:mm`, optional fraction; `T` or a space) to unix seconds.
/// The fraction is truncated.
pub fn parse_rfc3339(s: &str) -> Option<Unix> {
    let b = s.trim().as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let punct = |at: usize, want: &[u8]| b.get(at).is_some_and(|c| want.contains(c));
    let year = num(0, 4)?;
    let month = num(5, 2)?;
    let day = num(8, 2)?;
    let (hour, min, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(punct(4, b"-")
        && punct(7, b"-")
        && punct(10, b"Tt ")
        && punct(13, b":")
        && punct(16, b":"))
    {
        return None;
    }
    if !(1..=12).contains(&month)
        || day < 1
        || day > i64::from(days_in_month(year as i32, month as u8))
        || hour > 23
        || min > 59
        || sec > 60
    {
        return None;
    }
    let mut i = 19;
    if punct(i, b".") {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let offset = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && punct(i + 3, b":") => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let secs = oh * 3600 + om * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    let date = LocalDate {
        year: year as i32,
        month: month as u8,
        day: day as u8,
    };
    Some(days_from_civil(date) * 86_400 + hour * 3600 + min * 60 + sec - offset)
}

/// The local civil date, hour and weekday of an instant.
pub fn local(at: Unix, tz: &dyn Tz) -> LocalTime {
    let t = at + i64::from(tz.offset_secs(at));
    let days = t.div_euclid(86_400);
    LocalTime {
        date: civil_from_days(days),
        hour: (t.rem_euclid(86_400) / 3600) as u8,
        // 1970-01-01 was a Thursday (3 with Monday = 0).
        weekday: (days + 3).rem_euclid(7) as u8,
    }
}

/// Unix time of the first instant of a local date. A day with a UTC-offset
/// change inside it uses the offset in force at the start of that civil day.
pub fn day_start(date: LocalDate, tz: &dyn Tz) -> Unix {
    let naive = days_from_civil(date) * 86_400;
    // Offset at the guessed instant, then once more at the corrected one.
    let guess = naive - i64::from(tz.offset_secs(naive));
    naive - i64::from(tz.offset_secs(guess))
}

pub fn add_days(date: LocalDate, days: i64) -> LocalDate {
    civil_from_days(days_from_civil(date) + days)
}

/// The current unix time.
pub fn now() -> Unix {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_with_and_without_fraction_and_offsets() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-11-24T12:00:00+09:00"),
            Some(1_795_489_200)
        );
        assert_eq!(
            parse_rfc3339("2026-11-24T03:00:00Z"),
            parse_rfc3339("2026-11-24T12:00:00+09:00")
        );
        assert_eq!(
            parse_rfc3339("2026-11-24T03:00:00.987654321Z"),
            parse_rfc3339("2026-11-24T03:00:00Z")
        );
        assert_eq!(
            parse_rfc3339("2026-11-23T22:30:00.5-04:30"),
            parse_rfc3339("2026-11-24T03:00:00Z")
        );
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400));
        for bad in [
            "",
            "not a date",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-11-24T03:00:00",
            "2026-11-24T03:00:00.Z",
            "2026-11-24T03:00:00+0900",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_dates_cross_midnight_in_fixed_tz() {
        let at = parse_rfc3339("2026-11-24T16:30:00Z").unwrap();
        let jst = local(at, &FixedTz(9 * 3600));
        assert_eq!(
            jst.date,
            LocalDate {
                year: 2026,
                month: 11,
                day: 25
            }
        );
        assert_eq!(jst.hour, 1);
        let utc = local(at, &FixedTz(0));
        assert_eq!(utc.date.day, 24);
        assert_eq!(utc.hour, 16);
        let west = local(at, &FixedTz(-17 * 3600));
        assert_eq!(west.date.day, 23);
        let tz = FixedTz(9 * 3600);
        let start = day_start(jst.date, &tz);
        assert_eq!(start, parse_rfc3339("2026-11-25T00:00:00+09:00").unwrap());
        assert_eq!(local(start - 1, &tz).date.day, 24);
        assert_eq!(
            add_days(
                LocalDate {
                    year: 2026,
                    month: 12,
                    day: 31
                },
                1
            ),
            LocalDate {
                year: 2027,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            add_days(
                LocalDate {
                    year: 2028,
                    month: 3,
                    day: 1
                },
                -1
            ),
            LocalDate {
                year: 2028,
                month: 2,
                day: 29
            }
        );
    }

    #[test]
    fn weekday_is_monday_zero() {
        let tz = FixedTz(0);
        // 2026-11-23 is a Monday.
        let monday = parse_rfc3339("2026-11-23T12:00:00Z").unwrap();
        assert_eq!(local(monday, &tz).weekday, 0);
        assert_eq!(local(monday + 86_400 * 3, &tz).weekday, 3);
        assert_eq!(local(monday + 86_400 * 6, &tz).weekday, 6);
        assert_eq!(local(0, &tz).weekday, 3); // 1970-01-01, a Thursday
        assert_eq!(local(-86_400, &tz).weekday, 2);
    }
}
