//! One detached worktree per verifier (L1-3): made from the rebuild recipe
//! when round 1 starts, checked against the manifest, removed at close
//! only when it is still the worktree the log recorded. Naming and the
//! porcelain parse are pure; the rest runs `git`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::model::types::{PartId, Rebuild, SessionId, TargetManifest};

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// The checked-out commit; `None` for a bare entry.
    pub head: Option<String>,
    /// `refs/heads/…` when a branch is checked out.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// Where verifier `part`'s worktree goes: `<main>-wt/verify-<id>-<part>`,
/// beside the worktrees `Noa +ctl spawn --worktree` makes.
pub fn path_for(main: &Path, id: &SessionId, part: &PartId) -> PathBuf {
    let mut dir: OsString = main.as_os_str().to_owned();
    dir.push("-wt");
    PathBuf::from(dir).join(format!("verify-{id}-{part}"))
}

/// Makes a detached worktree of `repo` at `path` on `rebuild.head_sha`
/// (`git worktree add --detach`), applies `target/worktree.patch` and
/// copies `target/untracked/` from the snapshot `target_dir`, then checks
/// every manifest file and untracked file against its digest. A mismatch
/// is an error naming the files. No branch is created.
pub fn add(
    _repo: &Path,
    _path: &Path,
    _target_dir: &Path,
    _manifest: &TargetManifest,
    _rebuild: &Rebuild,
) -> Result<(), String> {
    todo!("U3: worktree::add (L1-3)")
}

/// Why `path` is not a detached worktree of `repo` at `head_sha` (named in
/// the error), as a resume checks it.
pub fn check(_repo: &Path, _path: &Path, _head_sha: &str) -> Result<(), String> {
    todo!("U3: worktree::check")
}

/// Removes `path` with `git worktree remove --force` when `git worktree
/// list --porcelain` shows it as a detached worktree of `repo` at that
/// path; anything else is left, and the error says why.
pub fn remove(_repo: &Path, _path: &Path) -> Result<(), String> {
    todo!("U3: worktree::remove")
}

/// `repo`'s worktrees, as `git worktree list --porcelain` gives them.
pub fn list(_repo: &Path) -> Result<Vec<Entry>, String> {
    todo!("U3: worktree::list")
}

/// Parses `git worktree list --porcelain` output (pure).
pub fn parse_porcelain(_text: &[u8]) -> Vec<Entry> {
    todo!("U3: parse_porcelain")
}
