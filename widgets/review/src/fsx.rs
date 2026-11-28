//! Link-safe file access. A review reads files an agent or a branch can
//! shape (a snapshot source, a reply file, its own directories), so every
//! path below a trusted base is opened component by component: a symbolic
//! link anywhere on the way is refused, and files are accessed relative to
//! the opened parent, so swapping its name for a link cannot redirect I/O.

use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

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
    let name = path.file_name().ok_or_else(|| link_error(path))?;
    let dir = open_dir(base, path.parent().unwrap_or(base), false)?;
    open_read_at(&dir, name)
}

fn open_read_at(dir: &OwnedFd, name: &OsStr) -> io::Result<File> {
    let name = c_name(name)?;
    // O_NONBLOCK lets a FIFO be rejected by metadata without waiting for a
    // writer. It does not change reads of regular files.
    // SAFETY: `dir` is an open directory and `name` a C string.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} は通常のファイルではありません", name.to_string_lossy()),
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
    open_dir(base, path, true).map(drop)
}

/// Writes `bytes` to `path` (under `base`) whole: into a temporary file
/// created exclusively beside it, then renamed over it. A link left where
/// the temporary file or `path` would go is never written through.
pub fn write_replace(base: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| link_error(path))?;
    let dir = open_dir(base, path.parent().unwrap_or(base), false)?;
    write_replace_at(&dir, name, bytes)
}

fn write_replace_at(dir: &OwnedFd, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let tmp = c_name(OsStr::new(&format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )))?;
    let name = c_name(name)?;
    // SAFETY: `dir` is an open directory and `tmp` a C string.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            tmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    let renamed = written.and_then(|()| {
        // SAFETY: both names are C strings, relative to the same open directory.
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                dir.as_raw_fd(),
                name.as_ptr(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    });
    if let Err(e) = renamed {
        let _ = remove_if_same_at(dir, OsStr::from_bytes(tmp.as_bytes()), &file);
        return Err(e);
    }
    Ok(())
}

/// Removes a file through its opened parent; a link at the filename is
/// removed itself, never followed.
pub fn remove_file(base: &Path, path: &Path) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| link_error(path))?;
    let dir = open_dir(base, path.parent().unwrap_or(base), false)?;
    remove_file_at(&dir, name)
}

fn remove_file_at(dir: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: `dir` is an open directory and `name` a C string.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `name` as a C string for the `*at` calls.
fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "名前に NUL が含まれます"))
}

/// The directory `dir` below the trusted `base`, opened one component at a
/// time relative to the one before (`openat` with `O_NOFOLLOW` and
/// `O_DIRECTORY`), so a link anywhere on the way, also one swapped in while
/// it is walked, fails instead of being followed. With `create`, missing
/// components are made on the way (`mkdirat`, relative to the same
/// descriptors). Files made through the descriptor land in the directory
/// that was checked, whatever its path names by then.
pub fn open_dir(base: &Path, dir: &Path, create: bool) -> io::Result<OwnedFd> {
    let rest = dir.strip_prefix(base).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} は {} の外です", dir.display(), base.display()),
        )
    })?;
    let mut fd = OwnedFd::from(File::open(base)?);
    let mut at = base.to_path_buf();
    for part in rest.components() {
        let Component::Normal(name) = part else {
            return Err(link_error(dir));
        };
        at.push(name);
        let name = c_name(name)?;
        if create {
            // SAFETY: `fd` is an open directory and `name` a C string.
            let made = unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o755) };
            if made != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::AlreadyExists {
                    return Err(err);
                }
            }
        }
        // SAFETY: as above; the descriptor returned is owned below.
        let next = unsafe {
            libc::openat(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            let err = io::Error::last_os_error();
            // A link fails the open (ELOOP, or ENOTDIR on macOS with
            // O_DIRECTORY); which it was only picks the message.
            let link = err.raw_os_error() == Some(libc::ELOOP)
                || fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink());
            return Err(if link { link_error(&at) } else { err });
        }
        // SAFETY: `next` was just opened and nothing else owns it.
        fd = unsafe { OwnedFd::from_raw_fd(next) };
    }
    Ok(fd)
}

/// Writes `bytes` to a new file `name` in the directory `dir` (from
/// [`open_dir`]): an existing file or link there fails with
/// `AlreadyExists` and is left as it was; a write that fails removes the
/// file through the same descriptor.
pub fn write_new_at(dir: &OwnedFd, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let name = c_name(name)?;
    // SAFETY: `dir` is an open directory and `name` a C string.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) else {
        return Ok(());
    };
    // The half-written file goes, if its name still holds it; a cleanup
    // that cannot be done is said, not dropped.
    match remove_if_same_at(dir, OsStr::from_bytes(name.as_bytes()), &file) {
        Ok(()) => Err(err),
        Err(why) => Err(io::Error::new(
            err.kind(),
            format!("{err}（書きかけのファイルを消せませんでした: {why}）"),
        )),
    }
}

/// Removes `name` from the directory `dir` only while it is `file` (the
/// same device and inode, the entry itself not followed): a name taken by
/// another file or link in the meantime is left alone and said.
pub fn remove_if_same_at(dir: &OwnedFd, name: &OsStr, file: &File) -> io::Result<()> {
    let c = c_name(name)?;
    // SAFETY: `stat` is plain data the calls fill in.
    let mut ours: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `file` is open; `ours` is writable.
    if unsafe { libc::fstat(file.as_raw_fd(), &mut ours) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    let mut there: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `dir` is an open directory, `c` a C string, `there` writable.
    let found = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            &mut there,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if found != 0 {
        return Err(io::Error::last_os_error());
    }
    if there.st_dev != ours.st_dev || there.st_ino != ours.st_ino {
        return Err(io::Error::other(format!(
            "{} は別のファイルに置き換わっていたため消していません",
            name.to_string_lossy()
        )));
    }
    // SAFETY: as above.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Writes `bytes` to a new file `path` (under `base`): its directory is
/// opened with [`open_dir`] and the file made in it with [`write_new_at`].
pub fn write_new(base: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| link_error(path))?.to_owned();
    let dir = open_dir(base, path.parent().unwrap_or(base), false)?;
    write_new_at(&dir, &name, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

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
    fn a_directory_swapped_for_a_link_after_it_was_opened_takes_the_file_itself() {
        let base = temp("swap");
        let outside = temp("swap-outside");
        fs::create_dir(base.join("a")).unwrap();
        let dir = open_dir(&base, &base.join("a"), false).unwrap();
        // The checked directory moves away and a link takes its name.
        fs::rename(base.join("a"), base.join("a-old")).unwrap();
        symlink(&outside, base.join("a")).unwrap();
        write_new_at(&dir, OsStr::new("f.md"), b"x").unwrap();
        assert_eq!(fs::read_to_string(base.join("a-old/f.md")).unwrap(), "x");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        // Opening by the path now meets the link and stops.
        assert!(is_link_error(
            &open_dir(&base, &base.join("a"), false).unwrap_err()
        ));
        assert!(is_link_error(
            &write_new(&base, &base.join("a/g.md"), b"x").unwrap_err()
        ));
        assert!(is_link_error(
            &open_dir(&base, &base.join("a/deeper"), true).unwrap_err()
        ));
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn reads_replacements_and_removals_stay_in_the_opened_directory() {
        let base = temp("io-swap");
        let outside = temp("io-swap-outside");
        fs::create_dir(base.join("a")).unwrap();
        fs::write(base.join("a/f"), "inside").unwrap();
        fs::write(outside.join("f"), "outside").unwrap();
        let dir = open_dir(&base, &base.join("a"), false).unwrap();
        fs::rename(base.join("a"), base.join("a-old")).unwrap();
        symlink(&outside, base.join("a")).unwrap();

        let mut text = String::new();
        open_read_at(&dir, OsStr::new("f"))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "inside");
        write_replace_at(&dir, OsStr::new("f"), b"replaced").unwrap();
        assert_eq!(
            fs::read_to_string(base.join("a-old/f")).unwrap(),
            "replaced"
        );
        remove_file_at(&dir, OsStr::new("f")).unwrap();
        assert!(!base.join("a-old/f").exists());
        assert_eq!(fs::read_to_string(outside.join("f")).unwrap(), "outside");
    }

    #[test]
    fn named_pipes_are_rejected_without_waiting_for_a_writer() {
        let base = temp("fifo");
        let path = base.join("reply.json");
        let name = c_name(path.as_os_str()).unwrap();
        // SAFETY: `name` is a C string naming a new entry in the test directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = open_read(&base, &path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("通常のファイルではありません"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_cleanup_removes_only_the_file_it_made() {
        let base = temp("cleanup");
        let dir = open_dir(&base, &base.join("d"), true).unwrap();
        write_new_at(&dir, OsStr::new("f"), b"ours").unwrap();
        let ours = File::open(base.join("d/f")).unwrap();
        // Its name now holds another file (and a link stands at a third).
        fs::rename(base.join("d/f"), base.join("d/moved")).unwrap();
        fs::write(base.join("d/f"), "theirs").unwrap();
        let other = remove_if_same_at(&dir, OsStr::new("f"), &ours).unwrap_err();
        assert!(other.to_string().contains("置き換わっていた"), "{other}");
        assert_eq!(fs::read_to_string(base.join("d/f")).unwrap(), "theirs");
        symlink(base.join("d/moved"), base.join("d/link")).unwrap();
        assert!(remove_if_same_at(&dir, OsStr::new("link"), &ours).is_err());
        assert!(fs::symlink_metadata(base.join("d/link")).is_ok());
        // Under its own name, it goes.
        remove_if_same_at(&dir, OsStr::new("moved"), &ours).unwrap();
        assert!(!base.join("d/moved").exists());
        // Gone already: said, not ignored.
        assert!(remove_if_same_at(&dir, OsStr::new("moved"), &ours).is_err());
    }

    #[test]
    fn missing_directories_are_made_on_the_way_and_paths_outside_refused() {
        let base = temp("mkdirat");
        let dir = open_dir(&base, &base.join("x/y/z"), true).unwrap();
        write_new_at(&dir, OsStr::new("f"), b"1").unwrap();
        assert_eq!(fs::read_to_string(base.join("x/y/z/f")).unwrap(), "1");
        assert!(open_dir(&base, &temp("mkdirat-other"), true).is_err());
        assert!(open_dir(&base, &base.join("x/../y"), true).is_err());
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
