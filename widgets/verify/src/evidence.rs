//! Evidence on disk (L1-5, L1-5a, L1-8): what a reply's cited files are
//! when it is read, their copies in the judge's reply dir
//! (`<reply dir>/evidence/`), person-attached media, the per-round
//! `manifest.md`, and the 証跡ごと書き出す bundle. Every directory is opened
//! one component at a time from its base and every file created
//! exclusively (`fsx::open_dir`, `fsx::write_new_at`): no link is followed
//! and nothing is written over.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use crate::capture::digest_bytes;
use crate::fsx;
use crate::media;
use crate::model::types::{
    Evidence, EvidenceKey, FileFact, FileState, MAX_LOG_BYTES, MAX_VIDEO_BYTES, Media, PartId,
    RoundNo, Stored,
};
use crate::store;

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

/// Why a file below a base could not be opened for reading.
#[derive(Debug)]
enum Refused {
    /// The path is not plainly relative (`..`, absolute, empty).
    Unsafe,
    Missing,
    /// A symbolic link on the way or at the end.
    Link,
    /// Not a regular file (a FIFO, a device, a folder), or a file where a
    /// folder of the path should be.
    NotRegular,
    Io(io::Error),
}

impl Refused {
    fn state(&self) -> FileState {
        match self {
            // Not looked at: validation names the path itself (L1-6).
            Refused::Unsafe | Refused::Missing => FileState::Missing,
            Refused::Link => FileState::Link,
            Refused::NotRegular | Refused::Io(_) => FileState::NotRegular,
        }
    }

    fn message(&self, path: &Path) -> String {
        let path = path.display();
        match self {
            Refused::Unsafe => format!("{path} はフォルダの外を指しています"),
            Refused::Missing => format!("{path} がありません"),
            Refused::Link => format!("{path} はシンボリックリンクを経由します"),
            Refused::NotRegular => format!("{path} は通常のファイルではありません"),
            Refused::Io(e) => format!("{path} を読めません: {e}"),
        }
    }
}

/// `rel` as its components, when each is a plain name.
fn components(rel: &Path) -> Option<Vec<&OsStr>> {
    let parts: Option<Vec<&OsStr>> = rel
        .components()
        .map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    parts.filter(|parts| !parts.is_empty())
}

fn c_name(name: &OsStr) -> Result<CString, Refused> {
    CString::new(name.as_bytes()).map_err(|_| Refused::Unsafe)
}

/// What a failed `openat` of `name` in `dir` met: a link (`ELOOP`, or
/// `ENOTDIR` on a link with `O_DIRECTORY`), nothing, or something else.
fn refused_at(dir: &OwnedFd, name: &CString, err: io::Error) -> Refused {
    match err.raw_os_error() {
        Some(libc::ENOENT) => Refused::Missing,
        Some(libc::ELOOP) => Refused::Link,
        Some(libc::ENOTDIR) => {
            // SAFETY: `stat` is plain data the call fills in.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `dir` is open, `name` a C string, `st` writable.
            let found = unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    &mut st,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if found == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                Refused::Link
            } else {
                Refused::NotRegular
            }
        }
        _ => Refused::Io(err),
    }
}

/// Opens `dir/rel` (`dir` below the trusted `base`) to read, never through
/// a link, also not one swapped in after an earlier check: `dir` is opened
/// with [`fsx::open_dir`], each folder of `rel` with `openat`
/// (`O_NOFOLLOW | O_DIRECTORY`) relative to the one before, and the file
/// itself with `O_NOFOLLOW | O_NONBLOCK` (so a FIFO cannot block the
/// caller), then checked with `fstat` to be a regular file. Gives the file
/// and its size.
fn open_below(base: &Path, dir: &Path, rel: &Path) -> Result<(File, u64), Refused> {
    let parts = components(rel).ok_or(Refused::Unsafe)?;
    let mut at = fsx::open_dir(base, dir, false).map_err(|e| {
        if fsx::is_link_error(&e) {
            Refused::Link
        } else if e.kind() == io::ErrorKind::NotFound {
            Refused::Missing
        } else if e.raw_os_error() == Some(libc::ENOTDIR) {
            Refused::NotRegular
        } else {
            Refused::Io(e)
        }
    })?;
    let (last, folders) = parts.split_last().expect("not empty");
    for name in folders {
        let name = c_name(name)?;
        // SAFETY: `at` is an open directory and `name` a C string; the
        // descriptor returned is owned below.
        let next = unsafe {
            libc::openat(
                at.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(refused_at(&at, &name, io::Error::last_os_error()));
        }
        // SAFETY: `next` was just opened and nothing else owns it.
        at = unsafe { OwnedFd::from_raw_fd(next) };
    }
    let name = c_name(last)?;
    // SAFETY: as above.
    let fd = unsafe {
        libc::openat(
            at.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(refused_at(&at, &name, io::Error::last_os_error()));
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(Refused::Io)?;
    if !meta.is_file() {
        return Err(Refused::NotRegular);
    }
    Ok((file, meta.len()))
}

/// [`open_below`] for the rest of the crate (the re-run files): the file
/// and its size, `None` when nothing is there, and an error saying why
/// for a link or anything that is not a regular file.
pub(crate) fn open_file(base: &Path, dir: &Path, rel: &Path) -> io::Result<Option<(File, u64)>> {
    match open_below(base, dir, rel) {
        Ok(opened) => Ok(Some(opened)),
        Err(Refused::Missing) => Ok(None),
        Err(Refused::Io(e)) => Err(e),
        Err(refused) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            refused.message(rel),
        )),
    }
}

/// The first [`media::SNIFF_BYTES`] bytes of `file` (fewer for a short one).
fn head_of(file: &mut File) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(media::SNIFF_BYTES);
    file.take(media::SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(head)
}

/// What each of `paths` (relative to `dir`, the verifier's
/// `r<N>/<verifier>/`) is: a regular file and its size, missing, a link or
/// something else, and the media its leading bytes and its extension say.
/// A link anywhere between `base` and the file makes it [`FileState::Link`](crate::model::types::FileState::Link).
///
/// A path that is not plainly relative is not looked at and reads as
/// [`FileState::Missing`]; validation names it for what it is (L1-6).
pub fn inspect(base: &Path, dir: &Path, paths: &[String]) -> Vec<FileFact> {
    paths
        .iter()
        .map(|path| {
            let by_ext = media::by_extension(path);
            let (state, sniffed) = match open_below(base, dir, Path::new(path)) {
                Ok((mut file, bytes)) => (
                    FileState::Regular { bytes },
                    head_of(&mut file).ok().and_then(|head| media::sniff(&head)),
                ),
                Err(refused) => (refused.state(), None),
            };
            FileFact {
                path: path.clone(),
                state,
                sniffed,
                by_ext,
            }
        })
        .collect()
}

/// Writes `bytes` as the new file `rel` below `dir` (below the trusted
/// `base`), making the folders on the way without following a link; an
/// existing file or link at the name is left as it was and fails.
fn write_below(base: &Path, dir: &Path, rel: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = rel
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "名前がありません"))?;
    let folder = dir.join(rel.parent().unwrap_or(Path::new("")));
    let fd = fsx::open_dir(base, &folder, true)?;
    fsx::write_new_at(&fd, name, bytes)
}

/// Copies each item from `src_dir` (below `src_base`) to
/// `<reply_dir>/evidence/r<round>/<part>/<path>` (below `reply_base`,
/// [`crate::store::round_evidence_dir`]; re-run logs go to the sibling
/// `<part>.rerun/`, [`crate::rerun::ingest_logs`]), hashing it. A file
/// gone (or swapped for a link) since validation gives an error for that
/// item; the others are still copied.
///
/// The source is read as [`inspect`] looks at it (a link on the way,
/// also a parent folder swapped for one after validation, refuses it; a
/// FIFO cannot block); one grown past its cap since validation is refused
/// rather than cut. The copy is created exclusively; a file already at its
/// name (an ingest of the same round cut short before `EvidenceIngested`
/// was logged) is taken as the copy only when it is a regular file,
/// reached without a link, holding the same bytes, and is otherwise
/// refused and left as it is. Items citing the same
/// path share one copy: it is read and stored once (to the largest of
/// their caps) and each gets the same [`Stored`], or the same error, an
/// item whose own cap the file passes getting the cap error.
pub fn ingest(
    src_base: &Path,
    src_dir: &Path,
    reply_base: &Path,
    reply_dir: &Path,
    round: RoundNo,
    part: &PartId,
    items: &[IngestItem],
) -> Vec<(EvidenceKey, Result<Stored, String>)> {
    let into = store::round_evidence_dir(reply_dir, round, part);
    let recorded = store::round_evidence_dir(Path::new(""), round, part);
    let mut copies: BTreeMap<PathBuf, Result<Stored, String>> = BTreeMap::new();
    items
        .iter()
        .map(|item| {
            let rel = Path::new(&item.path);
            let copied = copies.entry(rel.to_owned()).or_insert_with(|| {
                let cap = items
                    .iter()
                    .filter(|other| Path::new(&other.path) == rel)
                    .map(|other| other.cap)
                    .max()
                    .unwrap_or(item.cap);
                let (file, _) = open_below(src_base, src_dir, rel).map_err(|r| match r {
                    Refused::Missing => {
                        format!("{} がありません（検証の後に消えました）", item.path)
                    }
                    Refused::Link => format!(
                        "{} はシンボリックリンクを経由します（検証の後に置き換わりました）",
                        item.path
                    ),
                    other => other.message(rel),
                })?;
                let (bytes, over) = fsx::read_capped(file, cap)
                    .map_err(|e| format!("{} を読めません: {e}", item.path))?;
                if over {
                    return Err(grown(&item.path, cap));
                }
                match write_below(reply_base, &into, rel, &bytes) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        same_copy(reply_base, &into, rel, &bytes)
                            .map_err(|why| format!("{} を保存できません: {why}", item.path))?;
                    }
                    Err(e) => return Err(format!("{} を保存できません: {e}", item.path)),
                }
                Ok(Stored {
                    path: recorded.join(rel),
                    bytes: bytes.len() as u64,
                    digest: digest_bytes(&bytes),
                })
            });
            let copied = match copied {
                Ok(stored) if stored.bytes > item.cap => Err(grown(&item.path, item.cap)),
                other => other.clone(),
            };
            (item.key.clone(), copied)
        })
        .collect()
}

/// Whether the file already at `dir/rel` (below `base`), where an ingest
/// found its exclusive create taken, is the copy of `bytes` an earlier
/// ingest of the same round made (cut short by a crash before it was
/// logged): a regular file, reached without a link, of the same size and
/// bytes (so the same digest). Anything else is said, and left as it is.
/// Re-run logs are taken back the same way ([`crate::rerun::ingest_logs`]).
pub(crate) fn same_copy(base: &Path, dir: &Path, rel: &Path, bytes: &[u8]) -> Result<(), String> {
    let (file, size) = open_below(base, dir, rel).map_err(|refused| refused.message(rel))?;
    let (there, over) = fsx::read_capped(file, bytes.len() as u64)
        .map_err(|e| format!("既にあるコピーを読めません: {e}"))?;
    if size != bytes.len() as u64 || over || there != bytes {
        return Err("既に別の内容のコピーがあります（上書きしません）".to_owned());
    }
    Ok(())
}

fn grown(path: &str, cap: u64) -> String {
    format!("{path} が上限 {cap} バイトを超えています（検証の後に大きくなりました）")
}

/// Copies the person's `source` (validated as media, L1-5a) to
/// `<reply_dir>/evidence/user/<n>.<ext>`, `n` the first free number,
/// without following a link, and hashes it.
///
/// `source` is the person's own choice: its folders are taken as given,
/// its last component is opened with `O_NOFOLLOW` (and `O_NONBLOCK`) and
/// must be a regular file. It must be media by its leading bytes, its
/// extension (when it names a media kind) must agree, and it must be
/// within its kind's cap. `n` is one past the highest number already
/// stored, so a number is never given twice while the files stay.
pub fn attach(reply_base: &Path, reply_dir: &Path, source: &Path) -> Result<Attached, String> {
    let shown = source.display();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .map_err(|e| {
            if matches!(e.raw_os_error(), Some(libc::ELOOP)) {
                format!("{shown} はシンボリックリンクです")
            } else {
                format!("{shown} を開けません: {e}")
            }
        })?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{shown} を読めません: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{shown} は通常のファイルではありません"));
    }
    let (bytes, over) = fsx::read_capped(file, MAX_VIDEO_BYTES)
        .map_err(|e| format!("{shown} を読めません: {e}"))?;
    let media = media::sniff(&bytes[..bytes.len().min(media::SNIFF_BYTES)]).ok_or_else(|| {
        format!("{shown} は画像（PNG・JPEG・GIF・WebP）か動画（MP4・MOV・WebM）ではありません")
    })?;
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    if let Some(named) = media::by_extension(&name)
        && named != media
    {
        return Err(format!(
            "{shown} の拡張子（{}）と中身（{}）が一致しません",
            named.name(),
            media.name()
        ));
    }
    let cap = media::cap(Some(media));
    if over || bytes.len() as u64 > cap {
        return Err(format!(
            "{shown} が大きすぎます（{}の上限 {} MiB）",
            media.label(),
            cap / (1024 * 1024)
        ));
    }

    let user_dir = store::user_evidence_dir(reply_dir);
    let dir = fsx::open_dir(reply_base, &user_dir, true)
        .map_err(|e| format!("証跡のフォルダを開けません: {e}"))?;
    let highest = fs::read_dir(&user_dir)
        .map_err(|e| format!("証跡のフォルダを読めません: {e}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let stem = name.to_str()?.split('.').next()?.to_owned();
            stem.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    let mut n = highest + 1;
    loop {
        let file_name = format!("{n}.{}", media.extension());
        match fsx::write_new_at(&dir, OsStr::new(&file_name), &bytes) {
            Ok(()) => {
                return Ok(Attached {
                    key: EvidenceKey::user(n),
                    stored: Stored {
                        path: store::user_evidence_dir(Path::new("")).join(file_name),
                        bytes: bytes.len() as u64,
                        digest: digest_bytes(&bytes),
                    },
                    media,
                });
            }
            // Taken meanwhile: the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < highest + 1000 => n += 1,
            Err(e) => return Err(format!("{file_name} を保存できません: {e}")),
        }
    }
}

/// A path for a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// Writes `<reply_dir>/evidence/r<round>/manifest.md`, listing every item
/// of `items` with its key, stored path, size and digest, and returns its
/// path.
///
/// Each item's re-run log gets a row of its own. Written to a temporary
/// file in the folder opened without following a link, then renamed over
/// `manifest.md`, so a later call replaces it and a link left at the name
/// is replaced, never written through.
pub fn write_manifest(
    reply_base: &Path,
    reply_dir: &Path,
    round: RoundNo,
    items: &[&Evidence],
) -> io::Result<PathBuf> {
    let mut rows: Vec<(String, Option<&Stored>)> = Vec::new();
    for item in items {
        rows.push((item.key.to_string(), item.stored.as_ref()));
        let log = item.rerun.as_ref().and_then(|rerun| match rerun {
            crate::model::types::Rerun::Matched { log, .. }
            | crate::model::types::Rerun::Differed { log, .. }
            | crate::model::types::Rerun::TimedOut { log }
            | crate::model::types::Rerun::NotRun { log, .. } => log.as_ref(),
        });
        if let Some(log) = log {
            rows.push((format!("{} (re-run)", item.key), Some(log)));
        }
    }
    let mut md = format!("# Evidence, round {round}\n\n");
    let _ = writeln!(md, "| Key | Stored copy | Bytes | FNV-1a 64 |");
    let _ = writeln!(md, "|---|---|---:|---|");
    let (mut files, mut total) = (0usize, 0u64);
    for (key, stored) in &rows {
        match stored {
            Some(stored) => {
                files += 1;
                total += stored.bytes;
                let _ = writeln!(
                    md,
                    "| {} | {} | {} | {:016x} |",
                    cell(key),
                    cell(&stored.path.to_string_lossy()),
                    stored.bytes,
                    stored.digest
                );
            }
            None => {
                let _ = writeln!(md, "| {} | (not stored) | | |", cell(key));
            }
        }
    }
    let _ = writeln!(md, "\n{files} files, {total} bytes.");

    let dir = store::round_dir(&store::evidence_dir(reply_dir), round);
    let fd = fsx::open_dir(reply_base, &dir, true)?;
    let tmp = format!(".{MANIFEST_FILE}.{}.tmp", std::process::id());
    let tmp_c = CString::new(tmp.as_bytes()).expect("no NUL");
    let name_c = CString::new(MANIFEST_FILE).expect("no NUL");
    // A temporary name left by an earlier run that stopped half-way.
    // SAFETY: `fd` is an open directory and the name a C string.
    unsafe { libc::unlinkat(fd.as_raw_fd(), tmp_c.as_ptr(), 0) };
    fsx::write_new_at(&fd, OsStr::new(&tmp), md.as_bytes())?;
    // SAFETY: as above; both names are relative to the same descriptor.
    let renamed = unsafe {
        libc::renameat(
            fd.as_raw_fd(),
            tmp_c.as_ptr(),
            fd.as_raw_fd(),
            name_c.as_ptr(),
        )
    };
    if renamed != 0 {
        let err = io::Error::last_os_error();
        // SAFETY: as above.
        unsafe { libc::unlinkat(fd.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(err);
    }
    Ok(dir.join(MANIFEST_FILE))
}

/// The last `lines` lines of a stored copy (a failing log's tail for the
/// judge's brief and 修正を依頼).
pub fn tail(
    reply_base: &Path,
    reply_dir: &Path,
    stored: &Stored,
    lines: usize,
) -> io::Result<String> {
    // A stored log holds at most MAX_LOG_BYTES and a cut mark.
    let (bytes, _) = read_stored(reply_base, reply_dir, stored, MAX_LOG_BYTES + 4096)?;
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    Ok(all[all.len().saturating_sub(lines)..].join("\n"))
}

/// A stored copy's bytes, at most `cap` of them, and whether it was cut.
pub fn read_stored(
    reply_base: &Path,
    reply_dir: &Path,
    stored: &Stored,
    cap: u64,
) -> io::Result<(Vec<u8>, bool)> {
    let (file, _) = open_below(reply_base, reply_dir, &stored.path).map_err(|refused| {
        let kind = match &refused {
            Refused::Missing => io::ErrorKind::NotFound,
            _ => io::ErrorKind::InvalidInput,
        };
        io::Error::new(kind, refused.message(&stored.path))
    })?;
    fsx::read_capped(file, cap)
}

/// 証跡ごと書き出す (L1-8): writes `report` as `<dest>/report.md` and
/// copies every item of `stored` from the reply dir to `<dest>/<its
/// path>`. `dest` must be empty or missing; nothing is overwritten and no
/// link is followed.
///
/// `dest` is the person's choice: made when missing, refused when it is a
/// link, a file or a folder that holds anything. Each copy must still be
/// the size and digest its [`Stored`] records; one that is not stops the
/// export, named (what was written so far stays).
pub fn export_bundle(
    reply_base: &Path,
    reply_dir: &Path,
    dest: &Path,
    report: &str,
    stored: &[&Stored],
) -> Result<(), String> {
    let shown = dest.display();
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!("{shown} はシンボリックリンクです"));
        }
        Ok(meta) if !meta.is_dir() => return Err(format!("{shown} はフォルダではありません")),
        Ok(_) => {
            let mut entries =
                fs::read_dir(dest).map_err(|e| format!("{shown} を読めません: {e}"))?;
            if entries.next().is_some() {
                return Err(format!(
                    "{shown} は空ではありません。空のフォルダを選んでください"
                ));
            }
        }
        Err(_) => fs::create_dir_all(dest).map_err(|e| format!("{shown} を作れません: {e}"))?,
    }
    write_below(dest, dest, Path::new(store::REPORT_FILE), report.as_bytes())
        .map_err(|e| format!("{} を書けません: {e}", store::REPORT_FILE))?;
    let mut done: Vec<&Path> = Vec::new();
    for item in stored {
        if done.contains(&item.path.as_path()) {
            continue;
        }
        let path = item.path.display();
        let (bytes, over) = read_stored(reply_base, reply_dir, item, item.bytes)
            .map_err(|e| format!("証跡 {path} を読めません: {e}"))?;
        if over || bytes.len() as u64 != item.bytes || digest_bytes(&bytes) != item.digest {
            return Err(format!(
                "証跡 {path} が記録と一致しません（保存後に変更されています）"
            ));
        }
        write_below(dest, dest, &item.path, &bytes)
            .map_err(|e| format!("証跡 {path} を書き出せません: {e}"))?;
        done.push(&item.path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::tests::temp;
    use crate::media::tests::fixture;
    use crate::model::types::{EvidenceBody, MAX_IMAGE_BYTES, Rerun};
    use std::os::unix::fs::symlink;

    fn part() -> PartId {
        PartId::new("claude")
    }

    /// A verifier's worktree with a reply's files under
    /// `.noa/verify/ver-1/r1/claude/`, and the target's checkout holding the
    /// judge's reply dir.
    struct Fixture {
        src_base: PathBuf,
        src_dir: PathBuf,
        reply_base: PathBuf,
        reply_dir: PathBuf,
    }

    fn fixture_dirs() -> Fixture {
        let src_base = temp("wt");
        let src_dir = src_base.join(".noa/verify/ver-1/r1/claude");
        fs::create_dir_all(src_dir.join("logs")).unwrap();
        fs::create_dir_all(src_dir.join("shots")).unwrap();
        let reply_base = temp("repo");
        let reply_dir = reply_base.join(".noa/verify/ver-1");
        fs::create_dir_all(&reply_dir).unwrap();
        Fixture {
            src_base,
            src_dir,
            reply_base,
            reply_dir,
        }
    }

    fn item(id: &str, path: &str, cap: u64) -> IngestItem {
        IngestItem {
            key: EvidenceKey::reported(&part(), 1, id),
            path: path.into(),
            cap,
        }
    }

    /// Every file below `dir`, relative, sorted.
    fn files_below(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in fs::read_dir(&at).unwrap().filter_map(Result::ok) {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    stack.push(path);
                } else {
                    out.push(
                        path.strip_prefix(dir)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn ac30_inspect_reports_files_links_fifos_and_a_png_named_mp4() {
        let f = fixture_dirs();
        fs::write(f.src_dir.join("logs/1.log"), "ok\n").unwrap();
        fs::write(f.src_dir.join("shots/tab.png"), fixture(Media::Png)).unwrap();
        fs::write(f.src_dir.join("shots/wrong.mp4"), fixture(Media::Png)).unwrap();
        let outside = temp("outside");
        fs::write(outside.join("secret"), "s").unwrap();
        symlink(outside.join("secret"), f.src_dir.join("logs/link.log")).unwrap();
        symlink(&outside, f.src_dir.join("linked")).unwrap();
        let fifo = CString::new(f.src_dir.join("logs/fifo.log").as_os_str().as_bytes()).unwrap();
        // SAFETY: a C string path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        let paths: Vec<String> = [
            "logs/1.log",
            "shots/tab.png",
            "shots/wrong.mp4",
            "logs/link.log",
            "linked/secret",
            "logs/fifo.log",
            "logs/none.log",
            "../escape.log",
            "logs",
        ]
        .map(String::from)
        .to_vec();
        let facts = inspect(&f.src_base, &f.src_dir, &paths);
        let states: Vec<FileState> = facts.iter().map(|f| f.state).collect();
        assert_eq!(
            states,
            [
                FileState::Regular { bytes: 3 },
                FileState::Regular { bytes: 64 },
                FileState::Regular { bytes: 64 },
                FileState::Link,
                FileState::Link,
                FileState::NotRegular,
                FileState::Missing,
                FileState::Missing,
                FileState::NotRegular,
            ]
        );
        assert_eq!(
            (facts[1].sniffed, facts[1].by_ext),
            (Some(Media::Png), Some(Media::Png))
        );
        assert_eq!(
            (facts[2].sniffed, facts[2].by_ext),
            (Some(Media::Png), Some(Media::Mp4)),
            "a PNG named .mp4 is reported as both, for validation to refuse"
        );
        assert_eq!((facts[0].sniffed, facts[0].by_ext), (None, None));
        // A link between the base and the reply dir: every path is a link.
        let moved = f.src_base.join(".noa/verify/ver-1/r1/real");
        fs::rename(&f.src_dir, &moved).unwrap();
        symlink(&moved, &f.src_dir).unwrap();
        let facts = inspect(&f.src_base, &f.src_dir, &paths[..1]);
        assert_eq!(facts[0].state, FileState::Link);
    }

    #[test]
    fn ac09_ingest_copies_into_the_judges_reply_dir_with_size_and_digest_and_writes_the_manifest() {
        let f = fixture_dirs();
        fs::write(f.src_dir.join("logs/1.log"), "test result: ok\n").unwrap();
        fs::write(f.src_dir.join("shots/tab.png"), fixture(Media::Png)).unwrap();
        let items = [
            item("e1", "logs/1.log", MAX_LOG_BYTES),
            item("e2", "shots/tab.png", MAX_IMAGE_BYTES),
        ];
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &items,
        );
        let stored: Vec<Stored> = out.iter().map(|(_, r)| r.clone().unwrap()).collect();
        assert_eq!(out[0].0, EvidenceKey::reported(&part(), 1, "e1"));
        assert_eq!(
            stored[0],
            Stored {
                path: "evidence/r1/claude/logs/1.log".into(),
                bytes: 16,
                digest: digest_bytes(b"test result: ok\n"),
            }
        );
        assert_eq!(
            stored[1].path,
            PathBuf::from("evidence/r1/claude/shots/tab.png")
        );
        assert_eq!(
            fs::read(f.reply_dir.join(&stored[1].path)).unwrap(),
            fixture(Media::Png)
        );
        // Nothing but the copies was written anywhere.
        assert_eq!(
            files_below(&f.reply_base),
            [
                ".noa/verify/ver-1/evidence/r1/claude/logs/1.log",
                ".noa/verify/ver-1/evidence/r1/claude/shots/tab.png",
            ]
        );
        // Ingest again (a crash after the copy, before `EvidenceIngested`
        // was logged): the copy there is the file, byte for byte, so it is
        // kept and given again.
        let again = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &items,
        );
        let again: Vec<Stored> = again.into_iter().map(|(_, r)| r.unwrap()).collect();
        assert_eq!(again, stored);
        // A copy that is not the file is never written over nor taken: of
        // the same size with other bytes, cut short, or a link (to a file
        // holding the right bytes).
        let copy = f.reply_dir.join(&stored[0].path);
        let elsewhere = temp("elsewhere").join("1.log");
        fs::write(&elsewhere, "test result: ok\n").unwrap();
        for (what, make) in [
            (
                "other bytes",
                &(|| fs::write(&copy, "test result: no\n").unwrap()) as &dyn Fn(),
            ),
            ("cut short", &|| fs::write(&copy, "test result").unwrap()),
            ("a link", &|| symlink(&elsewhere, &copy).unwrap()),
        ] {
            let _ = fs::remove_file(&copy);
            make();
            let before = fs::symlink_metadata(&copy).unwrap();
            let again = ingest(
                &f.src_base,
                &f.src_dir,
                &f.reply_base,
                &f.reply_dir,
                1,
                &part(),
                &items[..1],
            );
            let err = again[0].1.clone().unwrap_err();
            assert!(err.contains("logs/1.log"), "{what}: {err}");
            let after = fs::symlink_metadata(&copy).unwrap();
            assert_eq!(
                (before.len(), before.file_type()),
                (after.len(), after.file_type()),
                "{what}"
            );
        }
        fs::remove_file(&copy).unwrap();
        fs::write(&copy, "test result: ok\n").unwrap();

        let evidence: Vec<Evidence> = stored
            .iter()
            .zip(["e1", "e2"])
            .map(|(stored, id)| Evidence {
                key: EvidenceKey::reported(&part(), 1, id),
                body: EvidenceBody::Observation {
                    text: String::new(),
                },
                stored: Some(stored.clone()),
                rerun: (id == "e1").then(|| Rerun::Matched {
                    exit: 0,
                    log: Some(Stored {
                        path: "evidence/r1/claude.rerun/1.log".into(),
                        bytes: 5,
                        digest: 1,
                    }),
                }),
                attached: None,
            })
            .chain([Evidence {
                key: EvidenceKey::reported(&part(), 1, "e3"),
                body: EvidenceBody::Observation {
                    text: "seen".into(),
                },
                stored: None,
                rerun: None,
                attached: None,
            }])
            .collect();
        let refs: Vec<&Evidence> = evidence.iter().collect();
        let manifest = write_manifest(&f.reply_base, &f.reply_dir, 1, &refs).unwrap();
        assert_eq!(manifest, f.reply_dir.join("evidence/r1/manifest.md"));
        let md = fs::read_to_string(&manifest).unwrap();
        assert!(
            md.contains(&format!(
                "| claude/r1/e1 | evidence/r1/claude/logs/1.log | 16 | {:016x} |",
                digest_bytes(b"test result: ok\n")
            )),
            "{md}"
        );
        assert!(md.contains("| claude/r1/e1 (re-run) | evidence/r1/claude.rerun/1.log | 5 |"));
        assert!(md.contains("| claude/r1/e3 | (not stored) |"));
        assert!(md.contains("3 files, 85 bytes."), "{md}");
        // Written again (a later item), and never through a link.
        let victim = temp("victim").join("v");
        fs::write(&victim, "keep").unwrap();
        fs::remove_file(&manifest).unwrap();
        symlink(&victim, &manifest).unwrap();
        write_manifest(&f.reply_base, &f.reply_dir, 1, &refs[..1]).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert!(
            !fs::symlink_metadata(&manifest)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        assert_eq!(
            tail(&f.reply_base, &f.reply_dir, &stored[0], 40).unwrap(),
            "test result: ok"
        );
    }

    #[test]
    fn ac09_ingest_never_follows_a_link_swapped_in_after_validation() {
        let f = fixture_dirs();
        let outside = temp("outside");
        fs::create_dir_all(outside.join("logs")).unwrap();
        fs::write(outside.join("logs/1.log"), "SECRET").unwrap();
        fs::write(outside.join("2.log"), "SECRET").unwrap();
        fs::create_dir_all(f.src_dir.join("logs/deep")).unwrap();
        fs::write(f.src_dir.join("logs/1.log"), "mine").unwrap();
        fs::write(f.src_dir.join("logs/2.log"), "mine").unwrap();
        fs::write(f.src_dir.join("logs/deep/3.log"), "mine").unwrap();
        fs::write(f.src_dir.join("shots/ok.png"), fixture(Media::Png)).unwrap();
        let items = [
            item("e1", "logs/deep/3.log", MAX_LOG_BYTES),
            item("e2", "logs/2.log", MAX_LOG_BYTES),
            item("e3", "shots/ok.png", MAX_IMAGE_BYTES),
            item("e4", "logs/fifo.log", MAX_LOG_BYTES),
        ];
        let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
        let facts = inspect(&f.src_base, &f.src_dir, &paths[..3]);
        assert!(
            facts
                .iter()
                .all(|f| matches!(f.state, FileState::Regular { .. }))
        );

        // After validation: a parent folder becomes a link to a folder that
        // holds a file of the same name, the file itself becomes a link,
        // and a FIFO appears where a log was cited.
        let real_deep = f.src_dir.join("logs/deep-real");
        fs::rename(f.src_dir.join("logs/deep"), &real_deep).unwrap();
        fs::create_dir_all(outside.join("deep")).unwrap();
        fs::write(outside.join("deep/3.log"), "SECRET").unwrap();
        symlink(outside.join("deep"), f.src_dir.join("logs/deep")).unwrap();
        fs::remove_file(f.src_dir.join("logs/2.log")).unwrap();
        symlink(outside.join("2.log"), f.src_dir.join("logs/2.log")).unwrap();
        let fifo = CString::new(f.src_dir.join("logs/fifo.log").as_os_str().as_bytes()).unwrap();
        // SAFETY: a C string path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);

        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &items,
        );
        let errors: Vec<String> = out
            .iter()
            .filter_map(|(_, r)| r.as_ref().err().cloned())
            .collect();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].contains("logs/deep/3.log") && errors[0].contains("リンク"));
        assert!(errors[1].contains("logs/2.log") && errors[1].contains("リンク"));
        assert!(
            errors[2].contains("通常のファイルではありません"),
            "{}",
            errors[2]
        );
        assert!(out[2].1.is_ok(), "the other items are still copied");
        assert_eq!(
            files_below(&f.reply_dir),
            ["evidence/r1/claude/shots/ok.png"]
        );

        // The reply folder itself swapped for a link: nothing is read.
        let real = f.src_base.join(".noa/verify/ver-1/r1/claude-real");
        fs::rename(&f.src_dir, &real).unwrap();
        symlink(&outside, &f.src_dir).unwrap();
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            2,
            &part(),
            &[item("e1", "logs/1.log", MAX_LOG_BYTES)],
        );
        assert!(out[0].1.as_ref().unwrap_err().contains("リンク"));
        assert!(!f.reply_dir.join("evidence/r2").exists());

        // A link in the judge's reply dir where a copy goes is never written
        // through either.
        fs::rename(&real, &f.src_dir).unwrap_or_else(|_| {
            fs::remove_file(&f.src_dir).unwrap();
            fs::rename(&real, &f.src_dir).unwrap();
        });
        let into = f.reply_dir.join("evidence/r3/claude");
        fs::create_dir_all(f.reply_dir.join("evidence/r3")).unwrap();
        symlink(&outside, &into).unwrap();
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            3,
            &part(),
            &[item("e3", "shots/ok.png", MAX_IMAGE_BYTES)],
        );
        assert!(out[0].1.is_err());
        assert!(!outside.join("shots").exists());
    }

    #[test]
    fn ac09_items_citing_the_same_file_share_one_copy() {
        let f = fixture_dirs();
        fs::write(f.src_dir.join("logs/1.log"), "0123456789").unwrap();
        let items = [
            item("e1", "logs/1.log", MAX_LOG_BYTES),
            item("e2", "logs/./1.log", MAX_LOG_BYTES),
            // Within one item's cap and past another's.
            item("e3", "logs/1.log", 4),
        ];
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &items,
        );
        let first = out[0].1.clone().unwrap();
        assert_eq!(out[1].1, Ok(first.clone()));
        assert_eq!(first.path, PathBuf::from("evidence/r1/claude/logs/1.log"));
        assert!(out[2].1.as_ref().unwrap_err().contains("上限 4"), "{out:?}");
        assert_eq!(
            files_below(&f.reply_base),
            [".noa/verify/ver-1/evidence/r1/claude/logs/1.log"]
        );
        // A path gone for one is gone for all its items, each told so.
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            2,
            &part(),
            &[
                item("e1", "logs/none.log", 8),
                item("e2", "logs/none.log", 8),
            ],
        );
        for (_, result) in out {
            assert!(result.unwrap_err().contains("ありません"));
        }
    }

    #[test]
    fn ac09_a_file_grown_past_its_cap_since_validation_is_refused() {
        let f = fixture_dirs();
        fs::write(f.src_dir.join("logs/1.log"), "0123456789").unwrap();
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &[item("e1", "logs/1.log", 4)],
        );
        assert!(out[0].1.as_ref().unwrap_err().contains("上限"));
    }

    #[test]
    fn ac31_attached_media_is_copied_to_the_next_user_number_without_following_a_link() {
        let f = fixture_dirs();
        let src = temp("person");
        fs::write(src.join("shot one.png"), fixture(Media::Png)).unwrap();
        fs::write(src.join("clip.MOV"), fixture(Media::QuickTime)).unwrap();
        fs::write(src.join("noext"), fixture(Media::Jpeg)).unwrap();
        let first = attach(&f.reply_base, &f.reply_dir, &src.join("shot one.png")).unwrap();
        assert_eq!(first.key, EvidenceKey::user(1));
        assert_eq!(first.media, Media::Png);
        assert_eq!(
            first.stored,
            Stored {
                path: "evidence/user/1.png".into(),
                bytes: 64,
                digest: digest_bytes(&fixture(Media::Png)),
            }
        );
        assert_eq!(
            fs::read(f.reply_dir.join("evidence/user/1.png")).unwrap(),
            fixture(Media::Png)
        );
        let second = attach(&f.reply_base, &f.reply_dir, &src.join("clip.MOV")).unwrap();
        assert_eq!(
            (second.key, second.stored.path),
            (EvidenceKey::user(2), PathBuf::from("evidence/user/2.mov"))
        );
        let third = attach(&f.reply_base, &f.reply_dir, &src.join("noext")).unwrap();
        assert_eq!(third.stored.path, PathBuf::from("evidence/user/3.jpg"));

        // Refused: a link, a mismatch, not media, over the cap, a FIFO.
        symlink(src.join("shot one.png"), src.join("link.png")).unwrap();
        fs::write(src.join("png.mp4"), fixture(Media::Png)).unwrap();
        fs::write(src.join("notes.txt"), "text").unwrap();
        let mut big = fixture(Media::Png);
        big.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        fs::write(src.join("big.png"), big).unwrap();
        let fifo = CString::new(src.join("pipe.png").as_os_str().as_bytes()).unwrap();
        // SAFETY: a C string path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        for (name, problem) in [
            ("link.png", "シンボリックリンク"),
            ("png.mp4", "一致しません"),
            ("notes.txt", "ではありません"),
            ("big.png", "大きすぎます"),
            ("pipe.png", "通常のファイルではありません"),
        ] {
            let err = attach(&f.reply_base, &f.reply_dir, &src.join(name)).unwrap_err();
            assert!(err.contains(problem), "{name}: {err}");
        }
        // The user folder swapped for a link: nothing is written through it.
        let outside = temp("outside");
        let user = f.reply_dir.join("evidence/user");
        fs::rename(&user, f.reply_dir.join("evidence/user-old")).unwrap();
        symlink(&outside, &user).unwrap();
        assert!(attach(&f.reply_base, &f.reply_dir, &src.join("shot one.png")).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn ac33_the_bundle_copies_every_cited_file_at_its_reply_dir_path_and_overwrites_nothing() {
        let f = fixture_dirs();
        fs::write(f.src_dir.join("logs/1.log"), "log\n").unwrap();
        fs::write(f.src_dir.join("shots/tab.png"), fixture(Media::Png)).unwrap();
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &[
                item("e1", "logs/1.log", MAX_LOG_BYTES),
                item("e2", "shots/tab.png", MAX_IMAGE_BYTES),
            ],
        );
        let mut stored: Vec<Stored> = out.into_iter().map(|(_, r)| r.unwrap()).collect();
        let person = temp("person").join("a.png");
        fs::write(&person, fixture(Media::Png)).unwrap();
        stored.push(attach(&f.reply_base, &f.reply_dir, &person).unwrap().stored);
        let refs: Vec<&Stored> = stored.iter().chain(&stored[..1]).collect();

        let dest = temp("export").join("bundle");
        export_bundle(&f.reply_base, &f.reply_dir, &dest, "# 検証結果\n", &refs).unwrap();
        assert_eq!(
            files_below(&dest),
            [
                "evidence/r1/claude/logs/1.log",
                "evidence/r1/claude/shots/tab.png",
                "evidence/user/1.png",
                "report.md",
            ]
        );
        assert_eq!(
            fs::read_to_string(dest.join("report.md")).unwrap(),
            "# 検証結果\n"
        );
        assert_eq!(
            fs::read(dest.join("evidence/user/1.png")).unwrap(),
            fixture(Media::Png)
        );

        // Not empty: refused, nothing overwritten.
        fs::write(dest.join("report.md"), "mine").ok();
        let err = export_bundle(&f.reply_base, &f.reply_dir, &dest, "new", &refs).unwrap_err();
        assert!(err.contains("空ではありません"), "{err}");
        assert_eq!(fs::read_to_string(dest.join("report.md")).unwrap(), "mine");
        // A link as the destination, and a stored copy changed since.
        let link = dest.with_file_name("link");
        symlink(temp("elsewhere"), &link).unwrap();
        assert!(
            export_bundle(&f.reply_base, &f.reply_dir, &link, "x", &refs)
                .unwrap_err()
                .contains("シンボリックリンク")
        );
        fs::write(f.reply_dir.join(&stored[0].path), "edited").unwrap();
        let err = export_bundle(
            &f.reply_base,
            &f.reply_dir,
            &dest.with_file_name("again"),
            "x",
            &refs,
        )
        .unwrap_err();
        assert!(err.contains("記録と一致しません"), "{err}");
    }

    #[test]
    fn a_tail_gives_the_last_lines_of_a_stored_copy() {
        let f = fixture_dirs();
        let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        fs::write(f.src_dir.join("logs/1.log"), &text).unwrap();
        let out = ingest(
            &f.src_base,
            &f.src_dir,
            &f.reply_base,
            &f.reply_dir,
            1,
            &part(),
            &[item("e1", "logs/1.log", MAX_LOG_BYTES)],
        );
        let stored = out[0].1.clone().unwrap();
        let tail = tail(&f.reply_base, &f.reply_dir, &stored, 3).unwrap();
        assert_eq!(tail, "line 48\nline 49\nline 50");
        let (bytes, cut) = read_stored(&f.reply_base, &f.reply_dir, &stored, 7).unwrap();
        assert_eq!((bytes.as_slice(), cut), (&b"line 1\n"[..], true));
        let gone = Stored {
            path: "evidence/r1/claude/logs/none.log".into(),
            ..stored
        };
        assert_eq!(
            read_stored(&f.reply_base, &f.reply_dir, &gone, 7)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
}
