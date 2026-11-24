//! Screenshots and recordings (L1-5a): which media a file is by its leading
//! bytes and by its extension, the cap each kind gets, and video marks.
//! Pure.

use crate::model::types::{MAX_FILE_BYTES, MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, Media};

/// How many leading bytes [`sniff`] needs.
pub const SNIFF_BYTES: usize = 32;

/// The media `head` (a file's first [`SNIFF_BYTES`] bytes, or fewer for a
/// short file) starts like: PNG, JPEG, GIF, WebP, MP4 / QuickTime (`ftyp`
/// box) or WebM; `None` for anything else.
pub fn sniff(_head: &[u8]) -> Option<Media> {
    todo!("U3: media::sniff")
}

/// The media `path`'s extension names (case-insensitive); `None` for any
/// other extension.
pub fn by_extension(_path: &str) -> Option<Media> {
    todo!("U3: media::by_extension")
}

/// A video mark's `t`, `mm:ss[.f]` or `hh:mm:ss[.f]`, as milliseconds
/// from the start: the first field one or two digits and each later one
/// two, minutes and seconds 00 to 59, `f` one to three digits of a second
/// (`00:12.5` is 12 500). The error says what is wrong, for a problem line
/// that names the item and the value (L1-6, AC-30).
pub fn parse_mark(t: &str) -> Result<u64, String> {
    let (clock, fraction) = match t.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (t, None),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [m, s] => {
            let minutes = field(m, "minutes", 1)?;
            if minutes > 59 {
                return Err("minutes must be 00 to 59 (hh:mm:ss past an hour)".into());
            }
            (0, minutes, field(s, "seconds", 2)?)
        }
        [h, m, s] => (
            field(h, "hours", 1)?,
            field(m, "minutes", 2)?,
            field(s, "seconds", 2)?,
        ),
        _ => return Err("expected mm:ss[.f] or hh:mm:ss[.f]".into()),
    };
    if minutes > 59 {
        return Err("minutes must be 00 to 59".into());
    }
    if seconds > 59 {
        return Err("seconds must be 00 to 59".into());
    }
    let millis = match fraction {
        None => 0,
        Some(f) if (1..=3).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            let digits: u64 = f.parse().expect("one to three ASCII digits");
            digits * 10u64.pow(3 - f.len() as u32)
        }
        Some(_) => return Err("the fraction must be one to three digits".into()),
    };
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

/// One clock field of [`parse_mark`]: `min_digits` to two ASCII digits.
fn field(text: &str, name: &str, min_digits: usize) -> Result<u64, String> {
    if !(min_digits..=2).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(if min_digits == 1 {
            format!("{name} must be one or two digits")
        } else {
            format!("{name} must be two digits")
        });
    }
    Ok(text.parse().expect("one or two ASCII digits"))
}

/// The largest file of this kind; [`MAX_FILE_BYTES`] for a file that is
/// not media.
pub fn cap(media: Option<Media>) -> u64 {
    match media {
        Some(media) if media.is_video() => MAX_VIDEO_BYTES,
        Some(_) => MAX_IMAGE_BYTES,
        None => MAX_FILE_BYTES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_read_as_milliseconds_from_the_start() {
        for (t, ms) in [
            ("00:04", 4_000),
            ("00:06.5", 6_500),
            ("00:12.5", 12_500),
            ("0:04", 4_000),
            ("59:59", 3_599_000),
            ("01:30.25", 90_250),
            ("00:00.007", 7),
            ("00:00.070", 70),
            ("1:02:03", 3_723_000),
            ("01:02:03.4", 3_723_400),
            ("99:59:59.999", 359_999_999),
        ] {
            assert_eq!(parse_mark(t), Ok(ms), "{t:?}");
        }
    }

    #[test]
    fn a_malformed_mark_names_its_problem() {
        for (t, problem) in [
            ("", "expected mm:ss[.f] or hh:mm:ss[.f]"),
            ("12", "expected mm:ss[.f] or hh:mm:ss[.f]"),
            ("1:2:3:4", "expected mm:ss[.f] or hh:mm:ss[.f]"),
            ("00.5:04", "expected mm:ss[.f] or hh:mm:ss[.f]"),
            ("000:04", "minutes must be one or two digits"),
            (":04", "minutes must be one or two digits"),
            ("0a:04", "minutes must be one or two digits"),
            ("60:00", "minutes must be 00 to 59 (hh:mm:ss past an hour)"),
            ("00:4", "seconds must be two digits"),
            ("00:004", "seconds must be two digits"),
            ("00:-4", "seconds must be two digits"),
            (" 00:04", "minutes must be one or two digits"),
            ("00:60", "seconds must be 00 to 59"),
            ("100:00:00", "hours must be one or two digits"),
            ("1:2:03", "minutes must be two digits"),
            ("1:60:00", "minutes must be 00 to 59"),
            ("1:00:60", "seconds must be 00 to 59"),
            ("00:04.", "the fraction must be one to three digits"),
            ("00:04.1234", "the fraction must be one to three digits"),
            ("00:04.5s", "the fraction must be one to three digits"),
            ("00:04.5.1", "the fraction must be one to three digits"),
            ("００:04", "minutes must be one or two digits"),
        ] {
            assert_eq!(parse_mark(t), Err(problem.to_string()), "{t:?}");
        }
    }
}
