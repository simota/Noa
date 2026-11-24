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

/// `mm:ss[.f]` or `hh:mm:ss[.f]` as milliseconds; `None` when malformed.
pub fn parse_mark(_t: &str) -> Option<u64> {
    todo!("U3: media::parse_mark")
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
