//! Evidence on disk (L1-5, L1-5a, L1-8): what a reply's cited files are
//! when it is read, their copies in the judge's reply dir
//! (`<reply dir>/evidence/`), person-attached media, the per-round
//! `manifest.md`, and the 証跡ごと書き出す bundle. Every directory is opened
//! one component at a time from its base and every file created
//! exclusively (`fsx::open_dir`, `fsx::write_new_at`): no link is followed
//! and nothing is written over.

use std::io;
use std::path::{Path, PathBuf};

use crate::model::types::{Evidence, EvidenceKey, FileFact, Media, PartId, RoundNo, Stored};

/// The manifest each round's evidence directory gets.
pub const MANIFEST_FILE: &str = "manifest.md";

/// One file to copy at ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestItem {
    pub key: EvidenceKey,
    /// As the reply cites it, relative to the verifier's `r<N>/<verifier>/`.
    pub path: String,
    /// The most bytes copied ([`crate::model::types::MAX_LOG_BYTES`] for a
    /// log, [`crate::media::cap`] for a file).
    pub cap: u64,
}

/// A person's media, once copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// `user/<n>`.
    pub key: EvidenceKey,
    pub stored: Stored,
    pub media: Media,
}

/// What each of `paths` (relative to `dir`, the verifier's
/// `r<N>/<verifier>/`) is: a regular file and its size, missing, a link or
/// something else, and the media its leading bytes and its extension say.
/// A link anywhere between `base` and the file makes it [`FileState::Link`](crate::model::types::FileState::Link).
pub fn inspect(_base: &Path, _dir: &Path, _paths: &[String]) -> Vec<FileFact> {
    todo!("U3: evidence::inspect (L1-5 validation facts)")
}

/// Copies each item from `src_dir` (below `src_base`) to
/// `<reply_dir>/evidence/r<round>/<part>/<path>` (below `reply_base`,
/// [`crate::store::round_evidence_dir`]; re-run logs go to the sibling
/// `<part>.rerun/`, [`crate::rerun::ingest_logs`]), hashing it. A file
/// gone (or swapped for a link) since validation gives an error for that
/// item; the others are still copied.
pub fn ingest(
    _src_base: &Path,
    _src_dir: &Path,
    _reply_base: &Path,
    _reply_dir: &Path,
    _round: RoundNo,
    _part: &PartId,
    _items: &[IngestItem],
) -> Vec<(EvidenceKey, Result<Stored, String>)> {
    todo!("U3: evidence::ingest (L1-5)")
}

/// Copies the person's `source` (validated as media, L1-5a) to
/// `<reply_dir>/evidence/user/<n>.<ext>`, `n` the first free number,
/// without following a link, and hashes it.
pub fn attach(_reply_base: &Path, _reply_dir: &Path, _source: &Path) -> Result<Attached, String> {
    todo!("U3: evidence::attach (L1-5a)")
}

/// Writes `<reply_dir>/evidence/r<round>/manifest.md`, listing every item
/// of `items` with its key, stored path, size and digest, and returns its
/// path.
pub fn write_manifest(
    _reply_base: &Path,
    _reply_dir: &Path,
    _round: RoundNo,
    _items: &[&Evidence],
) -> io::Result<PathBuf> {
    todo!("U3: evidence::write_manifest")
}

/// The last `lines` lines of a stored copy (a failing log's tail for the
/// judge's brief and 修正を依頼).
pub fn tail(
    _reply_base: &Path,
    _reply_dir: &Path,
    _stored: &Stored,
    _lines: usize,
) -> io::Result<String> {
    todo!("U3: evidence::tail")
}

/// A stored copy's bytes, at most `cap` of them, and whether it was cut.
pub fn read_stored(
    _reply_base: &Path,
    _reply_dir: &Path,
    _stored: &Stored,
    _cap: u64,
) -> io::Result<(Vec<u8>, bool)> {
    todo!("U3: evidence::read_stored")
}

/// 証跡ごと書き出す (L1-8): writes `report` as `<dest>/report.md` and
/// copies every item of `stored` from the reply dir to `<dest>/<its
/// path>`. `dest` must be empty or missing; nothing is overwritten and no
/// link is followed.
pub fn export_bundle(
    _reply_base: &Path,
    _reply_dir: &Path,
    _dest: &Path,
    _report: &str,
    _stored: &[&Stored],
) -> Result<(), String> {
    todo!("U3: evidence::export_bundle")
}
