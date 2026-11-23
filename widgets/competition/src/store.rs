//! Sessions and artifacts on disk. One directory per competition:
//!
//! ```text
//! <data-dir>/noa/competitions/<session id>/
//!   events.jsonl                  one event batch per line
//!   entries/<part>/...            each contestant's private live output folder
//!   r<N>/<part>.md                contestant submission report for round N
//!   r<N>/judge.json               judge evaluation output for round N
//!   r<N>/artifacts/<part>/...     round-N snapshot of <part>'s deliverables
//!   compare.html                  standalone side-by-side comparison viewer
//! ```

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::model::reduce::{self, Event, ReduceError};
use crate::model::types::{ArtifactFile, ArtifactKind, PartId, RoundNo, Session, SessionId};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const JUDGE_FILE: &str = "judge.json";
pub const VIEWER_FILE: &str = "compare.html";

/// Maximum total bytes copied into one round's artifact snapshot per contestant.
const MAX_SNAPSHOT_BYTES: u64 = 50 * 1024 * 1024;
/// Maximum number of files recorded in one contestant's snapshot.
const MAX_SNAPSHOT_FILES: usize = 200;

/// Where competitions live: `<data-dir>/noa/competitions`, beside Noa's own
/// session state.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("competitions"))
}

pub fn session_dir(root: &Path, id: &SessionId) -> PathBuf {
    root.join(id.as_str())
}

pub fn round_dir(dir: &Path, round: RoundNo) -> PathBuf {
    dir.join(format!("r{round}"))
}

/// Where reply files and contestant output directories go when agents run in
/// `workdir`: `<workdir>/.noa/competitions/<id>`.
pub fn workdir_reply_dir(workdir: &Path, id: &SessionId) -> PathBuf {
    workdir.join(".noa").join("competitions").join(id.as_str())
}

/// Makes `<workdir>/.noa/` with a `.gitignore` holding `*`, so competition
/// artifacts never pollute `git status`.
pub fn prepare_workdir(workdir: &Path) -> io::Result<()> {
    let dir = workdir.join(".noa");
    fs::create_dir_all(&dir)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut file) => io::Write::write_all(&mut file, b"*\n"),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Contestant `part`'s private working output directory.
pub fn entry_dir(base_dir: &Path, part: &PartId) -> PathBuf {
    base_dir.join("entries").join(part.as_str())
}

/// Round-`round` artifact snapshot directory for `part`.
pub fn artifact_snapshot_dir(base_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(base_dir, round)
        .join("artifacts")
        .join(part.as_str())
}

/// `<base>/r<N>/checks/<part>/`: the snapshot check (manifest and
/// viewer-condition screenshots) of `part`'s round-`round` entry.
pub fn checks_dir(base_dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(base_dir, round)
        .join("checks")
        .join(part.as_str())
}

/// Path to the standalone HTML comparison viewer.
pub fn viewer_path(base_dir: &Path) -> PathBuf {
    base_dir.join(VIEWER_FILE)
}

/// The file contestant `part` writes its round-`round` report to.
///
/// Prefixed so a contestant named `claude` or `agents` does not write
/// `claude.md`, which case-insensitive filesystems (macOS) resolve as
/// `CLAUDE.md` and agents then load as project instructions.
pub fn reply_path(dir: &Path, round: RoundNo, part: &PartId) -> PathBuf {
    round_dir(dir, round).join(format!("report-{part}.md"))
}

/// The file the judge writes its round-`round` JSON to.
pub fn judge_reply_path(dir: &Path, round: RoundNo) -> PathBuf {
    round_dir(dir, round).join(JUDGE_FILE)
}

pub fn prepare_reply(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

pub fn clear_reply(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn read_reply(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn ignored_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "dist" | "__pycache__" | ".DS_Store"
    )
}

/// Copies `src_dir` (`entries/<part>`) into `dst_dir` (`r<N>/artifacts/<part>`)
/// and returns the sorted list of captured [`ArtifactFile`]s.
pub fn snapshot_artifacts(src_dir: &Path, dst_dir: &Path) -> io::Result<Vec<ArtifactFile>> {
    if !src_dir.is_dir() {
        return Ok(Vec::new());
    }
    if dst_dir.exists() {
        let _ = fs::remove_dir_all(dst_dir);
    }
    fs::create_dir_all(dst_dir)?;
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    copy_dir_rec(src_dir, src_dir, dst_dir, &mut files, &mut total_bytes)?;
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

fn copy_dir_rec(
    root: &Path,
    cur: &Path,
    dst_root: &Path,
    out: &mut Vec<ArtifactFile>,
    total_bytes: &mut u64,
) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(cur)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_SNAPSHOT_FILES {
            break;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ignored_dir_name(&name_str) || name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir_rec(root, &path, dst_root, out, total_bytes)?;
        } else if meta.is_file() {
            let size = meta.len();
            if total_bytes.saturating_add(size) > MAX_SNAPSHOT_BYTES {
                continue;
            }
            // Joined from its components, never by rewriting separators: a
            // file name may hold `\` (`a\..\..\x` is one name on Unix).
            let rel_path = path.strip_prefix(root).unwrap_or(&path);
            let rel = rel_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let target = dst_root.join(rel_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &target)?;
            *total_bytes += size;
            out.push(ArtifactFile {
                kind: ArtifactKind::from_path(&rel),
                rel_path: rel,
                bytes: size,
            });
        }
    }
    Ok(())
}

/// Writes the comparison viewer HTML file and returns its path.
pub fn write_viewer(base_dir: &Path, session: &Session) -> io::Result<PathBuf> {
    fs::create_dir_all(base_dir)?;
    let path = viewer_path(base_dir);
    let html = crate::model::viewer::render_html(session, base_dir);
    fs::write(&path, html)?;
    Ok(path)
}

/// Like [`write_viewer`], reusing cached artifact previews across refreshes.
pub fn write_viewer_cached(
    base_dir: &Path,
    session: &Session,
    cache: &mut crate::model::viewer::ViewerCache,
) -> io::Result<PathBuf> {
    fs::create_dir_all(base_dir)?;
    let path = viewer_path(base_dir);
    let html = crate::model::viewer::render_html_cached(session, base_dir, cache);
    fs::write(&path, html)?;
    Ok(path)
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt { line: usize, error: String },
    Reduce(ReduceError),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Corrupt { line, error } => {
                write!(f, "{EVENTS_FILE} line {line} is damaged: {error}")
            }
            StoreError::Reduce(e) => write!(f, "{EVENTS_FILE} does not fold: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    file: File,
}

fn line(batch: &[Event]) -> io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(batch).map_err(io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

impl Store {
    pub fn create(dir: &Path, first: &[Event]) -> Result<Store, StoreError> {
        reduce::create(first).map_err(StoreError::Reduce)?;
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(dir)?;
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join(EVENTS_FILE))?;
        let mut store = Store {
            dir: dir.to_owned(),
            file,
        };
        store.append(first)?;
        Ok(store)
    }

    pub fn open(dir: &Path) -> Result<(Store, Session), StoreError> {
        let path = dir.join(EVENTS_FILE);
        let text = fs::read(&path)?;
        let mut batches = Vec::new();
        let mut kept = 0;
        let mut offset = 0;
        let mut torn = false;
        for (n, raw) in text.split_inclusive(|&b| b == b'\n').enumerate() {
            let complete = raw.ends_with(b"\n");
            offset += raw.len();
            if raw.trim_ascii().is_empty() {
                kept = offset;
                continue;
            }
            match serde_json::from_slice::<Vec<Event>>(raw) {
                Ok(batch) => {
                    batches.push(batch);
                    kept = offset;
                    torn = !complete;
                }
                Err(_) if !complete => torn = true,
                Err(error) => {
                    return Err(StoreError::Corrupt {
                        line: n + 1,
                        error: error.to_string(),
                    });
                }
            }
        }
        let session = reduce::replay(&batches).map_err(StoreError::Reduce)?;
        let mut file = OpenOptions::new().append(true).open(&path)?;
        if torn {
            if kept < text.len() {
                file.set_len(kept as u64)?;
            } else {
                file.write_all(b"\n")?;
            }
        }
        Ok((
            Store {
                dir: dir.to_owned(),
                file,
            },
            session,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn append(&mut self, batch: &[Event]) -> io::Result<()> {
        self.file.write_all(&line(batch)?)
    }

    pub fn append_all(&mut self, batches: &[Vec<Event>]) -> io::Result<()> {
        for batch in batches {
            self.append(batch)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-competition-store-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn snapshot_artifacts_copies_deliverables_and_skips_hidden_dirs() {
        let root = temp_root("snap");
        let src = entry_dir(&root, &PartId::new("claude"));
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("index.html"), "<h1>Hi</h1>").unwrap();
        fs::write(src.join("assets/logo.svg"), "<svg/>").unwrap();
        fs::write(src.join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        let dst = artifact_snapshot_dir(&root, 1, &PartId::new("claude"));
        let files = snapshot_artifacts(&src, &dst).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rel_path, "assets/logo.svg");
        assert_eq!(files[0].kind, ArtifactKind::Svg);
        assert_eq!(files[1].rel_path, "index.html");
        assert_eq!(files[1].kind, ArtifactKind::Html);
        assert!(dst.join("index.html").is_file());
        assert!(!dst.join(".git").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_report_file_never_collides_with_agent_instruction_files() {
        let path = reply_path(Path::new("/c"), 3, &PartId::new("claude"));
        assert_eq!(path.file_name().unwrap(), "report-claude.md");
        let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(name != "claude.md" && name != "agents.md");
    }

    #[test]
    fn a_backslash_in_a_file_name_does_not_reach_outside_the_snapshot() {
        let root = temp_root("escape");
        let src = entry_dir(&root, &PartId::new("claude"));
        fs::create_dir_all(&src).unwrap();
        let name = r"asset\..\..\..\judge.json";
        fs::write(src.join(name), "{}").unwrap();

        let dst = artifact_snapshot_dir(&root, 1, &PartId::new("claude"));
        let files = snapshot_artifacts(&src, &dst).unwrap();
        assert!(!judge_reply_path(&root, 1).exists());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel_path, name);
        assert!(dst.join(name).is_file());
        fs::remove_dir_all(&root).unwrap();
    }
}
