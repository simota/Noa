//! Screenshots and recordings (L1-5a): which media a file is by its leading
//! bytes and by its extension, the cap each kind gets, and video marks.
//! Pure.

use crate::model::types::{MAX_FILE_BYTES, MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, Media};

/// How many leading bytes [`sniff`] needs.
pub const SNIFF_BYTES: usize = 32;

/// The media `head` (a file's first [`SNIFF_BYTES`] bytes, or fewer for a
/// short file) starts like: PNG, JPEG, GIF, WebP, MP4 / QuickTime (`ftyp`
/// box) or WebM; `None` for anything else.
pub fn sniff(head: &[u8]) -> Option<Media> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Media::Png);
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(Media::Jpeg);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(Media::Gif);
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some(Media::Webp);
    }
    // ISO base media: a first box of type `ftyp`, its major brand saying
    // QuickTime (`qt  `) or one of the MP4 family.
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some(if &head[8..12] == b"qt  " {
            Media::QuickTime
        } else {
            Media::Mp4
        });
    }
    // EBML: WebM and Matroska share the header; only a Matroska DocType is
    // told apart, since WebM's may sit past the bytes read.
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) && !contains(head, b"matroska") {
        return Some(Media::Webm);
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The media `path`'s extension names (case-insensitive); `None` for any
/// other extension.
pub fn by_extension(path: &str) -> Option<Media> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => Media::Png,
        "jpg" | "jpeg" => Media::Jpeg,
        "gif" => Media::Gif,
        "webp" => Media::Webp,
        "mp4" | "m4v" => Media::Mp4,
        "mov" => Media::QuickTime,
        "webm" => Media::Webm,
        _ => return None,
    })
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
pub(crate) mod tests {
    use super::*;

    /// The leading bytes of a real file of each kind (headers as the
    /// formats define them; the rest of the file does not matter here).
    pub(crate) fn fixture(media: Media) -> Vec<u8> {
        let mut bytes: Vec<u8> = match media {
            Media::Png => b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
            Media::Jpeg => vec![
                0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0,
            ],
            Media::Gif => b"GIF89a\x01\0\x01\0".to_vec(),
            Media::Webp => b"RIFF\x24\0\0\0WEBPVP8 ".to_vec(),
            Media::Mp4 => b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2mp41".to_vec(),
            Media::QuickTime => b"\0\0\0\x14ftypqt  \0\0\0\0qt  ".to_vec(),
            Media::Webm => vec![
                0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01, 0x42, 0xf7, 0x81, 0x01, 0x42,
                0xf2, 0x81, 0x04, 0x42, 0xf3, 0x81, 0x08, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm',
            ],
        };
        bytes.resize(64, 0);
        bytes
    }

    #[test]
    fn ac30_each_media_fixture_is_sniffed_by_its_leading_bytes() {
        for media in Media::ALL {
            let bytes = fixture(media);
            assert_eq!(sniff(&bytes[..SNIFF_BYTES]), Some(media), "{media:?}");
        }
        for other in [
            &b""[..],
            b"plain text log\n",
            b"\x89PN",
            b"RIFF\x24\0\0\0WAVEfmt ",
            b"%PDF-1.7\n",
        ] {
            assert_eq!(sniff(other), None, "{other:?}");
        }
        let mut mkv = vec![0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x82, 0x88];
        mkv.extend_from_slice(b"matroska");
        assert_eq!(sniff(&mkv), None);
    }

    #[test]
    fn ac30_extensions_name_media_case_insensitively() {
        for (path, media) in [
            ("shots/a.png", Some(Media::Png)),
            ("shots/a.PNG", Some(Media::Png)),
            ("a.jpg", Some(Media::Jpeg)),
            ("a.JPEG", Some(Media::Jpeg)),
            ("a.gif", Some(Media::Gif)),
            ("a.webp", Some(Media::Webp)),
            ("video/x.mp4", Some(Media::Mp4)),
            ("video/x.m4v", Some(Media::Mp4)),
            ("video/x.MOV", Some(Media::QuickTime)),
            ("video/x.webm", Some(Media::Webm)),
            ("logs/1.log", None),
            ("png", None),
            (".png", None),
            ("dir.png/file", None),
        ] {
            assert_eq!(by_extension(path), media, "{path:?}");
        }
    }

    #[test]
    fn caps_follow_the_kind() {
        assert_eq!(cap(Some(Media::Png)), MAX_IMAGE_BYTES);
        assert_eq!(cap(Some(Media::QuickTime)), MAX_VIDEO_BYTES);
        assert_eq!(cap(None), MAX_FILE_BYTES);
    }

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
