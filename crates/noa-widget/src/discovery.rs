//! Definition discovery is read-only. Paths below the selected worktree are
//! opened relative to directory descriptors, without following symlinks.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::{
    DEFINITIONS_DIR, MANIFEST_FILE, MAX_DEFINITIONS, MAX_MANIFEST_BYTES, Manifest, WidgetError,
    check_id, content_digest,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoContext {
    pub worktree_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub repository_id: String,
    pub worktree_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Definition {
    pub manifest: Manifest,
    pub path: PathBuf,
    pub digest: String,
}

pub fn repo_context(path: &Path) -> Result<RepoContext, WidgetError> {
    let worktree_root = git_path(path, "--show-toplevel")?;
    let git_common_dir = git_path(path, "--git-common-dir")?;
    Ok(RepoContext {
        repository_id: identity("repository", &git_common_dir),
        worktree_id: identity("worktree", &worktree_root),
        worktree_root,
        git_common_dir,
    })
}

pub fn discover(repo: &Path) -> Result<Vec<Definition>, WidgetError> {
    let context = repo_context(repo)?;
    let registry_path = context.worktree_root.join(DEFINITIONS_DIR);
    let worktree = open_root(&context.worktree_root)?;
    let registry = match open_directory(&worktree, OsStr::new(DEFINITIONS_DIR)) {
        Ok(registry) => registry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(at(&registry_path, error)),
    };
    let mut names = directory_names(&registry).map_err(|e| at(&registry_path, e))?;
    names.sort();
    let mut definitions = Vec::new();
    for name in names {
        let mode = entry_mode(&registry, &name).map_err(|e| at(&registry_path.join(&name), e))?;
        if mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(WidgetError::new(format!(
                "{}: symbolic links are not allowed",
                registry_path.join(&name).display()
            )));
        }
        if mode & libc::S_IFMT != libc::S_IFDIR {
            continue;
        }
        let id = name
            .to_str()
            .ok_or_else(|| WidgetError::new("widget directory name must be UTF-8"))?;
        check_id(id)?;
        if definitions.len() >= MAX_DEFINITIONS {
            return Err(WidgetError::new(
                "at most 128 repository widgets are supported",
            ));
        }
        definitions.push(read_definition(&registry, &registry_path, id)?);
    }
    Ok(definitions)
}

pub fn load(repo: &Path, id: &str) -> Result<Definition, WidgetError> {
    check_id(id)?;
    let context = repo_context(repo)?;
    let registry_path = context.worktree_root.join(DEFINITIONS_DIR);
    let worktree = open_root(&context.worktree_root)?;
    let registry = open_directory(&worktree, OsStr::new(DEFINITIONS_DIR))
        .map_err(|e| at(&registry_path, e))?;
    read_definition(&registry, &registry_path, id)
}

fn read_definition(registry: &OwnedFd, path: &Path, id: &str) -> Result<Definition, WidgetError> {
    let directory_path = path.join(id);
    let directory = open_directory(registry, OsStr::new(id)).map_err(|e| at(&directory_path, e))?;
    let path = directory_path.join(MANIFEST_FILE);
    let file = open_file(&directory, OsStr::new(MANIFEST_FILE)).map_err(|e| at(&path, e))?;
    let metadata = file.metadata().map_err(|e| at(&path, e))?;
    if !metadata.is_file() {
        return Err(WidgetError::new(format!(
            "{} must be a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(WidgetError::new(format!(
            "{} exceeds 64 KiB",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| at(&path, e))?;
    let manifest = Manifest::from_slice(&bytes).map_err(|e| at(&path, e))?;
    if manifest.id != id {
        return Err(WidgetError::new(format!(
            "{}: manifest id {:?} must match directory {id:?}",
            path.display(),
            manifest.id
        )));
    }
    let digest = manifest.digest();
    Ok(Definition {
        manifest,
        path,
        digest,
    })
}

fn at(path: &Path, error: impl std::fmt::Display) -> WidgetError {
    WidgetError::new(format!("{}: {error}", path.display()))
}

fn identity(kind: &str, path: &Path) -> String {
    format!("{kind}:{}", content_digest(path.as_os_str().as_bytes()))
}

fn git_path(path: &Path, option: &str) -> Result<PathBuf, WidgetError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", option]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, _) in std::env::vars_os() {
        if name.as_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    let output = command.output().map_err(|e| at(path, e))?;
    if !output.status.success() {
        return Err(WidgetError::new(format!(
            "{} is not an accessible Git worktree: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut bytes = output.stdout;
    // Git appends one newline. A path itself can contain trailing newlines.
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(WidgetError::new("git returned an empty repository path"));
    }
    let result = PathBuf::from(OsString::from_vec(bytes));
    fs::canonicalize(&result).map_err(|e| at(&result, e))
}

fn open_root(path: &Path) -> Result<OwnedFd, WidgetError> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| at(path, e))?;
    Ok(file.into())
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.contains(&b'/') || bytes == b"." || bytes == b".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one path component",
        ));
    }
    CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn open_directory(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
    let name = c_name(name)?;
    // SAFETY: parent owns a live directory descriptor; name is terminated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this call just created fd, and no other value owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_file(parent: &OwnedFd, name: &OsStr) -> io::Result<File> {
    let name = c_name(name)?;
    // NONBLOCK prevents a FIFO with no writer from hanging discovery. The
    // caller rejects every nonregular file before reading its contents.
    // SAFETY: parent owns a live directory descriptor; name is terminated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this call just created fd, and no other value owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn entry_mode(parent: &OwnedFd, name: &OsStr) -> io::Result<libc::mode_t> {
    let name = c_name(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable storage; parent and name are live.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized stat.
    Ok(unsafe { stat.assume_init() }.st_mode)
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: the stream owns the descriptor transferred by fdopendir.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

fn directory_names(directory: &OwnedFd) -> io::Result<Vec<OsString>> {
    // A descriptor-backed listing keeps discovery on the same directory even
    // when a concurrent edit replaces its path with a symbolic link.
    // SAFETY: directory is live; fcntl creates an independently owned fd.
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership only on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: ownership was not transferred and duplicate is still live.
        unsafe {
            libc::close(duplicate);
        }
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        clear_errno();
        // SAFETY: stream is a live DIR, used by this thread only.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error);
            }
            break;
        }
        // SAFETY: readdir returned a valid entry whose name is terminated;
        // copy it before the next readdir call invalidates the pointer.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() >= MAX_DEFINITIONS * 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "widget registry contains too many entries",
            ));
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    Ok(names)
}

fn clear_errno() {
    // SAFETY: the C runtime supplies a valid pointer to this thread's errno.
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    // SAFETY: same contract on macOS.
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
}
