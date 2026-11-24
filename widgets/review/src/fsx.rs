//! Link-safe file access. A review reads files an agent or a branch can
//! shape (a snapshot source, a reply file, its own directories), so every
//! path below a trusted base is walked component by component: a symbolic
//! link anywhere on the way is refused, and the last component is opened
//! with `O_NOFOLLOW`, so a link swapped in between the check and the open
//! fails instead of being followed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// `O_NOFOLLOW`: open fails on a symbolic link.
#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;

/// The error a link on the way gives.
pub fn link_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} はシンボリックリンクを経由します", path.display()),
    )
}

/// Whether `err` is the one [`link_error`] or `O_NOFOLLOW` gives.
pub fn is_link_error(err: &io::Error) -> bool {
    // ELOOP: O_NOFOLLOW on a link (62 on macOS, 40 on Linux).
    err.kind() == io::ErrorKind::InvalidInput || matches!(err.raw_os_error(), Some(62 | 40))
}

/// Checks that every existing component of `path` below `base` is a real
/// directory (the last one may be a file), none a symbolic link. `path`
/// must lie under `base`; `base` itself is trusted.
pub fn check_no_links(base: &Path, path: &Path) -> io::Result<()> {
    let rest = path.strip_prefix(base).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} は {} の外です", path.display(), base.display()),
        )
    })?;
    let mut at = base.to_path_buf();
    let parts: Vec<Component> = rest.components().collect();
    for (i, part) in parts.iter().enumerate() {
        match part {
            Component::Normal(name) => at.push(name),
            _ => return Err(link_error(path)),
        }
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(link_error(&at)),
            Ok(meta) if i + 1 < parts.len() && !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} はフォルダではありません", at.display()),
                ));
            }
            Ok(_) => {}
            // Nothing further down exists either.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Opens `path` (under `base`) to read, refusing links on the way and at
/// the end.
pub fn open_read(base: &Path, path: &Path) -> io::Result<File> {
    check_no_links(base, path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} は通常のファイルではありません", path.display()),
        ));
    }
    Ok(file)
}

/// Reads at most `cap` bytes; the flag says whether the file held more.
pub fn read_capped(file: File, cap: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    let over = bytes.len() as u64 > cap;
    bytes.truncate(cap as usize);
    Ok((bytes, over))
}

/// Creates the directories of `path` below `base` that do not exist yet,
/// refusing any link on the way.
pub fn create_dirs(base: &Path, path: &Path) -> io::Result<()> {
    check_no_links(base, path)?;
    let rest = path
        .strip_prefix(base)
        .map_err(|_| link_error(path))?
        .to_path_buf();
    let mut at = base.to_path_buf();
    for part in rest.components() {
        at.push(part);
        match fs::create_dir(&at) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let meta = fs::symlink_metadata(&at)?;
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(link_error(&at));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes `bytes` to `path` (under `base`) whole: into a temporary file
/// created exclusively beside it, then renamed over it. A link left where
/// the temporary file or `path` would go is never written through.
pub fn write_replace(base: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or(base);
    check_no_links(base, parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp: PathBuf = parent.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Writes `bytes` to a new file `path` (under `base`), refusing links on
/// the way: an existing file or link at `path` fails with `AlreadyExists`
/// and is left as it was.
pub fn write_new(base: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    check_no_links(base, path.parent().unwrap_or(base))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("noa-review-fsx-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn links_anywhere_below_the_base_are_refused() {
        let base = temp("links");
        let outside = temp("links-outside");
        fs::write(outside.join("secret"), "s").unwrap();
        fs::create_dir_all(base.join("a")).unwrap();
        fs::write(base.join("a/ok.txt"), "ok").unwrap();
        symlink(&outside, base.join("a/dir")).unwrap();
        symlink(outside.join("secret"), base.join("a/file")).unwrap();
        assert!(open_read(&base, &base.join("a/ok.txt")).is_ok());
        assert!(is_link_error(
            &open_read(&base, &base.join("a/dir/secret")).unwrap_err()
        ));
        assert!(is_link_error(
            &open_read(&base, &base.join("a/file")).unwrap_err()
        ));
        assert!(create_dirs(&base, &base.join("a/dir/new")).is_err());
        assert!(!outside.join("new").exists());
        assert!(check_no_links(&base, &outside.join("secret")).is_err());
    }

    #[test]
    fn a_replace_never_writes_through_a_link() {
        let base = temp("replace");
        let victim = temp("replace-victim").join("victim");
        fs::write(&victim, "keep").unwrap();
        // A link where the report goes, and where an old temporary name was.
        symlink(&victim, base.join("report.md")).unwrap();
        symlink(&victim, base.join(".report.md.tmp")).unwrap();
        write_replace(&base, &base.join("report.md"), b"new").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(fs::read_to_string(base.join("report.md")).unwrap(), "new");
        assert!(
            !fs::symlink_metadata(base.join("report.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_new_file_is_never_written_over_a_file_or_through_a_link() {
        let base = temp("new");
        let victim = temp("new-victim").join("victim");
        fs::write(&victim, "keep").unwrap();
        symlink(&victim, base.join("link.md")).unwrap();
        fs::write(base.join("old.md"), "old").unwrap();
        let link = write_new(&base, &base.join("link.md"), b"x").unwrap_err();
        assert_eq!(link.kind(), io::ErrorKind::AlreadyExists);
        let old = write_new(&base, &base.join("old.md"), b"x").unwrap_err();
        assert_eq!(old.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(fs::read_to_string(base.join("old.md")).unwrap(), "old");
        write_new(&base, &base.join("new.md"), b"new").unwrap();
        assert_eq!(fs::read_to_string(base.join("new.md")).unwrap(), "new");
        symlink(temp("new-dir-victim"), base.join("d")).unwrap();
        assert!(is_link_error(
            &write_new(&base, &base.join("d/f.md"), b"x").unwrap_err()
        ));
    }

    #[test]
    fn reads_stop_past_the_cap() {
        let base = temp("cap");
        fs::write(base.join("f"), "abcdef").unwrap();
        let (bytes, over) = read_capped(open_read(&base, &base.join("f")).unwrap(), 4).unwrap();
        assert_eq!((bytes.as_slice(), over), (&b"abcd"[..], true));
        let (bytes, over) = read_capped(open_read(&base, &base.join("f")).unwrap(), 6).unwrap();
        assert_eq!((bytes.len(), over), (6, false));
    }
}
