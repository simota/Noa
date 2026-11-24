//! Screenshots and recordings (L1-5a): which media a file is by its leading
//! bytes and by its extension, the cap each kind gets, and video marks.
//! Pure.

use crate::model::types::{MAX_FILE_BYTES, MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, Media};

/// How many leading bytes [`sniff`] needs: enough for a WebM's whole EBML
/// header (about 40 bytes as muxers write it, its `DocType` at bytes 24–35).
pub const SNIFF_BYTES: usize = 64;

/// The `ftyp` brands that say MP4 video: the ISO base media family
/// (`isom`, `iso2`–`iso9`), MP4 (`mp41`, `mp42`, `mp71`), H.264 and AV1
/// (`avc1`, `av01`), Apple's M4V, DASH and CMAF (`dash`, `cmfc`, `cmf2`),
/// MPEG-4 mobile (`mmp4`), Flash video (`f4v `), Sony's XAVC and MSNV and
/// Dolby Vision (`dby1`).
const MP4_BRANDS: [&[u8; 4]; 25] = [
    b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"iso7", b"iso8", b"iso9", b"mp41",
    b"mp42", b"mp71", b"avc1", b"av01", b"M4V ", b"M4VH", b"M4VP", b"dash", b"cmfc", b"cmf2",
    b"mmp4", b"f4v ", b"XAVC", b"MSNV", b"dby1",
];

/// Major brands of ISO base media files that are not video here, whatever
/// brands they list as compatible: HEIF and AVIF images and image
/// sequences, M4A / M4B / M4P and Flash audio, and 3GP / 3G2.
fn not_video(major: &[u8]) -> bool {
    const IMAGE_OR_AUDIO: [&[u8; 4]; 19] = [
        b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"hevm", b"hevs", b"mif1", b"mif2",
        b"msf1", b"miaf", b"avif", b"avis", b"M4A ", b"M4B ", b"M4P ", b"F4A ", b"F4B ",
    ];
    IMAGE_OR_AUDIO.iter().any(|brand| major == &brand[..])
        || major.starts_with(b"3gp")
        || major.starts_with(b"3g2")
}

/// The media `head` (a file's first [`SNIFF_BYTES`] bytes, or fewer for a
/// short file) starts like: PNG, JPEG, GIF, WebP, MP4 / QuickTime (`ftyp`
/// box with a video brand) or WebM (an EBML header whose `DocType` is
/// `webm`); `None` for anything else.
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
    // QuickTime (`qt  `), or it or one of the compatible brands after it
    // (from byte 16 to the end of the box) saying MP4 video, unless the
    // major brand says an image or audio.
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let major = &head[8..12];
        if major == b"qt  " {
            return Some(Media::QuickTime);
        }
        if not_video(major) {
            return None;
        }
        // A size of 0 runs the box to the end of the file, 1 gives it in
        // eight more bytes: either way, as far as `head` goes.
        let size = u32::from_be_bytes(head[..4].try_into().expect("four bytes")) as usize;
        let end = match size {
            0 | 1 => head.len(),
            size => size.min(head.len()),
        };
        let compatible = head.get(16..end).unwrap_or_default().chunks_exact(4);
        return std::iter::once(major)
            .chain(compatible)
            .any(|brand| MP4_BRANDS.iter().any(|mp4| brand == &mp4[..]))
            .then_some(Media::Mp4);
    }
    // EBML: WebM and Matroska share the header, told apart by its DocType.
    if head.starts_with(&EBML_ID) {
        return (doc_type(head)? == b"webm").then_some(Media::Webm);
    }
    None
}

/// The EBML header's element id; the `DocType` element's.
const EBML_ID: [u8; 4] = [0x1a, 0x45, 0xdf, 0xa3];
const DOC_TYPE_ID: [u8; 2] = [0x42, 0x82];

/// An EBML variable-length integer at the start of `bytes`: its value
/// (the length marker taken off) and its length; `None` when it does not
/// fit in `bytes` or is longer than eight bytes.
fn vint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || bytes.len() < len {
        return None;
    }
    let value = bytes[1..len]
        .iter()
        .fold(u64::from(first) & (0xff >> len), |value, &b| {
            (value << 8) | u64::from(b)
        });
    Some((value, len))
}

/// The `DocType` of the EBML header `head` starts with, read element by
/// element inside the header; `None` when it is not within `head`.
fn doc_type(head: &[u8]) -> Option<&[u8]> {
    let (size, len) = vint(&head[EBML_ID.len()..])?;
    let start = EBML_ID.len() + len;
    let end = start
        .saturating_add(usize::try_from(size).ok()?)
        .min(head.len());
    let mut at = start;
    while at < end {
        // An element id keeps its length marker: one to four bytes.
        let (_, id_len) = vint(&head[at..end]).filter(|(_, n)| *n <= 4)?;
        let id = &head[at..at + id_len];
        let (size, size_len) = vint(&head[at + id_len..end])?;
        let data = at + id_len + size_len;
        let next = data.checked_add(usize::try_from(size).ok()?)?;
        if id == DOC_TYPE_ID {
            return head.get(data..next);
        }
        at = next;
    }
    None
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

    /// An EBML header as ffmpeg writes it (its size in eight bytes, so the
    /// `DocType` sits at bytes 31–34), with `doc_type`.
    fn ebml(doc_type: &[u8]) -> Vec<u8> {
        let mut body = vec![
            0x42, 0x86, 0x81, 0x01, 0x42, 0xf7, 0x81, 0x01, 0x42, 0xf2, 0x81, 0x04, 0x42, 0xf3,
            0x81, 0x08, 0x42, 0x82,
        ];
        body.push(0x80 | doc_type.len() as u8);
        body.extend_from_slice(doc_type);
        body.extend_from_slice(&[0x42, 0x87, 0x81, 0x04, 0x42, 0x85, 0x81, 0x02]);
        let mut out = vec![0x1a, 0x45, 0xdf, 0xa3, 0x01, 0, 0, 0, 0, 0, 0];
        out.push(body.len() as u8);
        out.extend(body);
        out
    }

    fn ftyp(brand: &[u8; 4], compatible: &[u8]) -> Vec<u8> {
        let mut out = (16 + compatible.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"ftyp");
        out.extend_from_slice(brand);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(compatible);
        out
    }

    #[test]
    fn ac30_ftyp_brands_and_ebml_doc_types_are_read_not_guessed() {
        for brand in [b"isom", b"iso2", b"mp41", b"mp42", b"avc1", b"M4V "] {
            assert_eq!(sniff(&ftyp(brand, b"isom")), Some(Media::Mp4), "{brand:?}");
        }
        assert_eq!(sniff(&ftyp(b"qt  ", b"qt  ")), Some(Media::QuickTime));
        // HEIF / AVIF images, M4A audio: ISO base media, but not video.
        for brand in [
            b"heic", b"heix", b"mif1", b"msf1", b"avif", b"avis", b"M4A ",
        ] {
            assert_eq!(sniff(&ftyp(brand, b"mif1miaf")), None, "{brand:?}");
        }

        // A video whose major brand is a newer ISO one, or one this does
        // not know, with a video brand among its compatible ones (as
        // ffmpeg writes `iso8` with `iso8iso2mp41`, say).
        for (major, compatible) in [
            (b"iso8", &b"iso8iso2mp41"[..]),
            (b"iso9", b"iso9"),
            (b"av01", b"av01iso6mp41"),
            (b"zzzz", b"zzzzmp42isom"),
        ] {
            assert_eq!(
                sniff(&ftyp(major, compatible)),
                Some(Media::Mp4),
                "{major:?} {compatible:?}"
            );
        }
        // Images, audio and 3GP list video brands among their compatible
        // ones too; an unknown brand with none is not media either.
        for (major, compatible) in [
            (b"heic", &b"mif1heiciso8"[..]),
            (b"avif", b"avifmif1miafiso8"),
            (b"M4A ", b"M4A mp42isom"),
            (b"3gp4", b"isom3gp4"),
            (b"zzzz", b"zzzzabcd"),
        ] {
            assert_eq!(sniff(&ftyp(major, compatible)), None, "{major:?}");
        }
        // Compatible brands past the box's end are not its.
        let mut short = ftyp(b"zzzz", b"");
        short.extend_from_slice(b"mp42");
        assert_eq!(sniff(&short), None);

        let webm = ebml(b"webm");
        assert_eq!(&webm[31..35], b"webm", "past the first 32 bytes");
        assert_eq!(
            sniff(&webm[..SNIFF_BYTES.min(webm.len())]),
            Some(Media::Webm)
        );
        let mkv = ebml(b"matroska");
        assert!(mkv.len() > 32 && !mkv[..32].windows(8).any(|w| w == b"matroska"));
        assert_eq!(sniff(&mkv), None);
        // A header cut before its DocType is not taken for WebM.
        assert_eq!(sniff(&webm[..32]), None);
        assert_eq!(sniff(&webm[..33]), None);
        assert_eq!(sniff(&[0x1a, 0x45, 0xdf, 0xa3]), None);
        assert_eq!(sniff(&[0x1a, 0x45, 0xdf, 0xa3, 0x00, 0x42]), None);
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
