//! The snapshot check every entry gets, and contestants can run themselves.
//!
//! [`inspect`] takes a snapshot directory and writes, under an output
//! directory, what the judge works from: the viewer copy (see
//! [`crate::preview`]), the screenshots (see [`crate::shots`]) and a
//! `manifest.md` listing every file with its size and hash, the references the
//! viewer copy could not inline, and the capture conditions. The runner calls
//! it on each round snapshot; `competition --check=DIR` runs the same steps on
//! an entry directory before submitting.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{preview, shots};

/// File name of the manifest inside a check directory.
pub const MANIFEST: &str = "manifest.md";

/// What one check produced.
#[derive(Debug)]
pub struct Inspection {
    pub manifest: PathBuf,
    /// The viewer copy, when the snapshot has a root `index.html`.
    pub preview: Option<preview::Preview>,
    /// Screenshot results in [`shots::VIEWPORTS`] order; empty without a browser.
    pub shots: Vec<io::Result<PathBuf>>,
}

impl Inspection {
    /// One line per problem a contestant or the runner should hear about.
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .preview
            .iter()
            .flat_map(|p| p.issues.iter().map(|i| format!("not inlined: {i}")))
            .collect();
        out.extend(
            self.shots
                .iter()
                .filter_map(|r| r.as_ref().err().map(|e| format!("screenshot: {e}"))),
        );
        out
    }
}

/// 64-bit FNV-1a: a stable content fingerprint for the manifest (not a
/// security hash; it only tells two snapshots of a file apart).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Every file under `dir` (skipping dot entries), as sorted `/` paths.
fn list_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            list_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Checks `snapshot_dir` and writes the results under `out_dir`.
pub fn inspect(snapshot_dir: &Path, out_dir: &Path) -> io::Result<Inspection> {
    fs::create_dir_all(out_dir)?;
    let preview = preview::write(snapshot_dir)?;
    let chrome = shots::find_chrome();
    let shot_results = match (&preview, &chrome) {
        (Some(p), Some(chrome)) => shots::capture(chrome, &p.page, out_dir),
        _ => Vec::new(),
    };

    let mut files = Vec::new();
    list_files(snapshot_dir, snapshot_dir, &mut files)?;
    files.sort();

    let mut md = String::from("# Snapshot check\n\n");
    let _ = writeln!(md, "Snapshot: {}\n", snapshot_dir.display());
    let _ = writeln!(
        md,
        "## Files\n\n| path | bytes | fnv1a-64 |\n|---|---:|---|"
    );
    let mut total = 0u64;
    for (rel, path) in &files {
        let bytes = fs::read(path)?;
        total += bytes.len() as u64;
        let _ = writeln!(md, "| {rel} | {} | {:016x} |", bytes.len(), fnv1a64(&bytes));
    }
    let _ = writeln!(md, "\n{} files, {total} bytes.\n", files.len());

    let _ = writeln!(md, "## Viewer copy\n");
    match &preview {
        Some(p) => {
            let _ = writeln!(md, "{}\n", p.page.display());
            let _ = writeln!(md, "{}\n", preview::INLINE_RULES);
            if p.issues.is_empty() {
                let _ = writeln!(md, "Every local reference was inlined.\n");
            } else {
                let _ = writeln!(md, "Not inlined (the viewer cannot load these):\n");
                for issue in &p.issues {
                    let _ = writeln!(md, "- {issue}");
                }
                md.push('\n');
            }
        }
        None => {
            let _ = writeln!(
                md,
                "No root `index.html`, so no viewer copy and no screenshots.\n"
            );
        }
    }

    let _ = writeln!(md, "## Screenshots\n");
    let _ = writeln!(md, "Conditions: {}\n", shots::CONDITIONS);
    match (&preview, &chrome) {
        (None, _) => {
            let _ = writeln!(md, "(none: no viewer copy)");
        }
        (_, None) => {
            let _ = writeln!(
                md,
                "(none: no Chrome found; set {} to a Chrome or Chromium binary)",
                shots::CHROME_ENV
            );
        }
        (Some(_), Some(chrome)) => {
            let _ = writeln!(md, "Browser: {}\n", chrome.display());
            for (viewport, result) in shots::VIEWPORTS.iter().zip(&shot_results) {
                match result {
                    Ok(path) => {
                        let _ = writeln!(md, "- {}", path.display());
                    }
                    Err(e) => {
                        let _ = writeln!(md, "- {}: failed: {e}", shots::file_name(*viewport));
                    }
                }
            }
        }
    }

    let manifest = out_dir.join(MANIFEST);
    fs::write(&manifest, md)?;
    Ok(Inspection {
        manifest,
        preview,
        shots: shot_results,
    })
}

/// `competition --check=ENTRY [--out=DIR]`: snapshots `entry` the way a
/// submission is snapshotted, into `out/snapshot`, then [`inspect`]s it into
/// `out`.
pub fn check_entry(entry: &Path, out: &Path) -> io::Result<Inspection> {
    if !entry.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", entry.display()),
        ));
    }
    let snapshot = out.join("snapshot");
    // The snapshot step deletes its destination first: it must not be, hold,
    // or sit inside the entry, or the entry is lost or copied into itself.
    // Compared both as written and with symlinks resolved on each side
    // (`/tmp` is `/private/tmp` on macOS).
    let entry_abs = lexical(&std::path::absolute(entry)?);
    let entry_real = entry.canonicalize()?;
    let out_abs = lexical(&std::path::absolute(out)?);
    // Resolved from the path as written: `link/..` is the link target's
    // parent on disk, not what `lexical` makes of it.
    let out_real = real_path(&std::path::absolute(out)?)?;
    for e in [&entry_abs, &entry_real] {
        for o in [&out_abs, &out_real] {
            if e.starts_with(o.join("snapshot")) || o.starts_with(e) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "--out {} overlaps the entry {}; pick a directory outside it",
                        out.display(),
                        entry.display()
                    ),
                ));
            }
        }
    }
    crate::store::snapshot_artifacts(entry, &snapshot)?;
    inspect(&snapshot, out)
}

/// `path` (absolute, lexical) with symlinks resolved: its deepest existing
/// ancestor canonicalized, the not-yet-created rest appended.
fn real_path(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return Ok(path.to_owned());
        };
        // `file_name` is `None` for a trailing `..`; keep it to resolve later.
        rest.push(
            existing
                .components()
                .next_back()
                .map_or_else(Default::default, |c| c.as_os_str().to_owned()),
        );
        existing = parent;
    }
    let mut real = existing.canonicalize()?;
    for part in rest.iter().rev() {
        real.push(part);
    }
    Ok(lexical(&real))
}

/// `path` with `.` and `..` resolved lexically (no symlinks followed).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("competition-check-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_output_that_overlaps_the_entry_is_refused_and_the_entry_survives() {
        let work = scratch("overlap");
        let entry = work.join("snapshot");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("index.html"), "<p>keep me</p>").unwrap();
        // `out/snapshot` is the entry itself, and `out` is its parent.
        assert!(check_entry(&entry, &work).is_err());
        assert!(entry.join("index.html").is_file(), "the entry was deleted");
        // An output inside the entry would copy the entry into itself.
        assert!(check_entry(&entry, &entry.join("out")).is_err());
        assert!(entry.join("index.html").is_file());
        let _ = fs::remove_dir_all(&work);
    }

    #[cfg(unix)]
    #[test]
    fn an_output_reached_through_a_symlink_is_still_an_overlap() {
        let work = scratch("symlink");
        let real = work.join("real");
        let entry = real.join("snapshot");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("index.html"), "<p>keep me</p>").unwrap();
        let link = work.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // `link/snapshot` is the entry itself, reached through the link.
        assert!(check_entry(&entry, &link).is_err());
        assert!(entry.join("index.html").is_file(), "the entry was deleted");
        // `link/snapshot/out` sits inside the entry.
        assert!(check_entry(&entry, &link.join("snapshot").join("out")).is_err());
        assert!(
            !entry.join("out").exists(),
            "the entry was copied into itself"
        );
        let _ = fs::remove_dir_all(&work);
    }

    #[cfg(unix)]
    #[test]
    fn a_dot_dot_after_a_symlink_is_resolved_on_the_real_side() {
        let work = scratch("dotdot");
        let data = work.join("data");
        let entry = data.join("snapshot");
        fs::create_dir_all(&entry).unwrap();
        fs::create_dir_all(data.join("subdir")).unwrap();
        fs::write(entry.join("index.html"), "<p>keep me</p>").unwrap();
        fs::create_dir_all(work.join("w")).unwrap();
        let link = work.join("w").join("link");
        std::os::unix::fs::symlink(data.join("subdir"), &link).unwrap();
        // `link/..` is `data` on disk, so `link/../snapshot` is the entry,
        // though it reads as `w/snapshot`.
        assert!(check_entry(&entry, &link.join("..")).is_err());
        assert!(entry.join("index.html").is_file(), "the entry was deleted");
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn a_missing_entry_is_an_error_not_a_stale_snapshot() {
        let work = scratch("missing");
        let out = work.join("out");
        fs::create_dir_all(out.join("snapshot")).unwrap();
        fs::write(out.join("snapshot/index.html"), "<p>old</p>").unwrap();
        assert!(check_entry(&work.join("no-such-entry"), &out).is_err());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn fnv1a64_matches_reference_values() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn manifest_lists_files_and_what_was_not_inlined() {
        let root = std::env::temp_dir().join(format!("competition-check-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let entry = root.join("entry");
        fs::create_dir_all(entry.join("assets")).unwrap();
        fs::write(
            entry.join("index.html"),
            r#"<img src="assets/missing.png">"#,
        )
        .unwrap();
        fs::write(entry.join("assets/a.txt"), "abc").unwrap();
        // No browser, so the check stays fast and deterministic.
        // SAFETY: tests in this module do not read the variable concurrently.
        unsafe { std::env::set_var(shots::CHROME_ENV, root.join("no-such-chrome")) };

        let out = root.join("out");
        let inspection = check_entry(&entry, &out).unwrap();
        let md = fs::read_to_string(&inspection.manifest).unwrap();
        assert!(md.contains("| assets/a.txt | 3 |"), "{md}");
        assert!(md.contains("| index.html |"), "{md}");
        assert!(md.contains("- assets/missing.png: file not found"), "{md}");
        assert!(md.contains("no Chrome found"), "{md}");
        assert_eq!(
            inspection.problems(),
            vec!["not inlined: assets/missing.png: file not found".to_owned()]
        );
        let _ = fs::remove_dir_all(&root);
    }
}
