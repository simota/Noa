//! Captures a verification target into its snapshot (`<reply dir>/target/`)
//! and describes it in a [`TargetManifest`], plus the rebuild recipe every
//! verifier's worktree is built from (L1-1). Runs once, when a verification
//! starts: every agent then works from the same bytes, whatever happens to
//! the person's work tree.
//!
//! As review's capture: read-only git commands only, with optional locks
//! off, so the index and the work tree are never written; file identity
//! from NUL-separated machine output; files read without following
//! symbolic links (see `fsx`) and never past their cap.

use std::path::{Path, PathBuf};

use crate::model::types::{Millis, Rebuild, SpecFile, Target, TargetManifest};

/// Most files one target may hold.
pub const MAX_TARGET_FILES: usize = 400;
/// Most bytes all captured files may add up to.
pub const MAX_TARGET_BYTES: u64 = 24 * 1024 * 1024;
/// The largest diff a diff target may have.
pub const MAX_DIFF_BYTES: u64 = 4 * 1024 * 1024;
/// A larger file is left out of the snapshot.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// The largest `target/worktree.patch` (L1-1).
pub const MAX_WORKTREE_PATCH_BYTES: u64 = 16 * 1024 * 1024;
/// A larger binary untracked file refuses the start (L1-1).
pub const MAX_UNTRACKED_BINARY_BYTES: u64 = 1024 * 1024;
/// The largest spec file (L1-1).
pub const MAX_SPEC_BYTES: u64 = 1024 * 1024;

/// The three-dot diff (diff target) or the uncommitted changes against
/// `HEAD` in work-tree mode (state target).
pub const DIFF_FILE: &str = "diff.patch";
/// Post-images of the changed files.
pub const FILES_DIR: &str = "files";
pub const MANIFEST_FILE: &str = "manifest.json";
/// `git diff --binary HEAD` in work-tree mode.
pub const WORKTREE_PATCH: &str = "worktree.patch";
/// Untracked, non-ignored files, at their paths.
pub const UNTRACKED_DIR: &str = "untracked";
/// The spec file's copy.
pub const SPEC_FILE: &str = "spec.md";

/// What one capture gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub manifest: TargetManifest,
    pub rebuild: Rebuild,
    /// The spec file, when the target names one.
    pub spec: Option<SpecFile>,
}

/// A stable 64-bit FNV-1a digest of `bytes`.
pub fn digest_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Captures `target` into `dest` (created; it must not exist yet): the
/// review-compatible snapshot, `worktree.patch` and `untracked/` in
/// work-tree mode, and `spec.md`. Refuses a non-git directory, an empty
/// diff (diff target), an untracked binary over
/// [`MAX_UNTRACKED_BINARY_BYTES`] and every cap, with the reason.
pub fn capture(_target: &Target, _dest: &Path, _now: Millis) -> Result<Captured, String> {
    todo!("U3: capture (L1-1)")
}

/// The top level of the work tree holding `path`; a non-git directory is
/// refused with 「検証は git の作業ツリーでのみ行えます」.
pub fn repo_root(_path: &Path) -> Result<PathBuf, String> {
    todo!("U3: repo_root")
}

/// The base a diff target takes when none is given: the remote's default
/// branch, else `main`, else `master`.
pub fn default_base(_repo: &Path) -> Option<String> {
    todo!("U3: default_base")
}

/// Whether the snapshot in `dir` is still what `manifest`, `rebuild` and
/// `spec` describe: the manifest, the diff, every captured file, the
/// worktree patch, every untracked file and `spec.md` (when `spec` is
/// given; with `None` there must be no `spec.md`) hash the same. Nothing
/// is read through a link.
pub fn verify(
    _dir: &Path,
    _manifest: &TargetManifest,
    _rebuild: &Rebuild,
    _spec: Option<&SpecFile>,
) -> Result<(), String> {
    todo!("U3: verify")
}

/// Where a captured file (post-image) lives in the snapshot `dir`; `None`
/// for a path that is not plainly relative.
pub fn snapshot_path(_dir: &Path, _path: &str) -> Option<PathBuf> {
    todo!("U3: snapshot_path")
}
