//! Captures a review target into its snapshot (`<reply dir>/target/`) and
//! describes it in a [`TargetManifest`]. Runs once, when a review starts:
//! every agent then reads the same bytes, whatever happens to the workspace.
//!
//! Git targets are read with read-only commands only (`rev-parse`,
//! `merge-base`, `diff`, `ls-files`, `cat-file`), with optional locks off, so
//! the index and the work tree are never written. File identity comes from
//! NUL-separated machine output (`diff --raw -z`, `ls-files -z`), never from
//! the patch's headers, and the patch is asked for with fixed prefixes, so
//! names with spaces, tabs or quotes and the user's `diff.*` settings do not
//! change what is captured. Files are read without following symbolic links
//! (see `fsx`) and never past their cap.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fsx;
use crate::model::types::{
    FileStatus, Head, Millis, Omitted, Target, TargetFile, TargetKind, TargetManifest,
};

/// Most files one target may hold.
pub const MAX_TARGET_FILES: usize = 400;
/// Most bytes all captured files may add up to.
pub const MAX_TARGET_BYTES: u64 = 24 * 1024 * 1024;
/// The largest diff a git target may have.
pub const MAX_DIFF_BYTES: u64 = 4 * 1024 * 1024;
/// A larger file is left out of the snapshot.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// The largest document.
pub const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;
/// Most bytes a listing (`ls-files`, `diff --raw`) is read to.
const MAX_LISTING_BYTES: u64 = 4 * 1024 * 1024;
/// How often a work-tree capture is taken again when the tree moved under it.
const CAPTURE_TRIES: usize = 3;

pub const DIFF_FILE: &str = "diff.patch";
pub const FILES_DIR: &str = "files";
pub const DOCUMENT_FILE: &str = "document.md";
pub const MANIFEST_FILE: &str = "manifest.json";

const LINK_REASON: &str = "シンボリックリンク（リンク先は取り込みません）";
const SUBMODULE_REASON: &str = "サブモジュール（中身は取り込みません）";

/// A stable 64-bit FNV-1a digest of `bytes`.
pub fn digest_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Why bytes are not text: a NUL in the first 8 KiB, or not UTF-8.
fn not_text(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    head.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn too_large(bytes: u64) -> String {
    format!("{} KiB（1 MiB 超）", bytes.div_ceil(1024))
}

fn line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
    (newlines + usize::from(!bytes.ends_with(b"\n"))) as u32
}

/// A relative path that stays inside its root, as components.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> io::Result<()> {
    let Some(rel_path) = safe_relative(rel) else {
        return Err(io::Error::other(format!("unsafe path {rel:?}")));
    };
    let path = root.join(rel_path);
    // `root` (the snapshot's files folder) sits in the trusted `dest`.
    if let Some(dest) = root.parent() {
        fsx::create_dirs(dest, path.parent().unwrap_or(root))?;
    }
    fsx::write_replace(root, &path, bytes)
}

/// Git with every setting that changes the output it is parsed from pinned.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    // A widget launched from a hook or another worktree must still read
    // the repository selected by the person, not the inherited Git state.
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
    cmd.arg("-C").arg(dir);
    for setting in [
        "core.quotePath=false",
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "core.bigFileThreshold=512m",
        "color.ui=never",
        "diff.noprefix=false",
        "diff.mnemonicPrefix=false",
        "diff.relative=false",
        "diff.external=",
        "diff.renames=true",
        "diff.renameLimit=1000",
        "diff.algorithm=myers",
        "diff.indentHeuristic=true",
        "diff.interHunkContext=0",
        "diff.suppressBlankEmpty=false",
        "diff.submodule=short",
        "diff.ignoreSubmodules=none",
    ] {
        cmd.args(["-c", setting]);
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    cmd
}

/// Clean/process filters are commands, even during a read-only diff.
/// Disable every configured driver so the patch describes the same raw
/// bytes as the files copied from the worktree.
fn filter_drivers(repo: &Path) -> Result<Vec<String>, String> {
    let mut cmd = git(repo);
    cmd.args(["config", "-z", "--name-only", "--get-regexp", r"^filter\."]);
    let listing = run_capped(cmd, &[0, 1], MAX_LISTING_BYTES).map_err(|e| match e {
        RunError::TooLarge => "git の設定が大きすぎます".to_owned(),
        RunError::Failed(why) => why,
    })?;
    let mut drivers = std::collections::BTreeSet::new();
    for key in nul_fields(&listing)? {
        if let Some((driver, _)) = key.strip_prefix("filter.").and_then(|s| s.rsplit_once('.')) {
            drivers.insert(driver.to_owned());
        }
    }
    Ok(drivers.into_iter().collect())
}

fn git_unfiltered(repo: &Path, drivers: &[String]) -> Command {
    let mut cmd = git(repo);
    // Environment config keeps driver names containing '=' intact; -c
    // would interpret the first '=' as the setting's value separator.
    let settings: Vec<_> = drivers
        .iter()
        .flat_map(|driver| {
            [
                ("clean", ""),
                ("smudge", ""),
                ("process", ""),
                ("required", "false"),
            ]
            .into_iter()
            .map(move |(key, value)| (format!("filter.{driver}.{key}"), value))
        })
        .collect();
    cmd.env("GIT_CONFIG_COUNT", settings.len().to_string());
    for (i, (key, value)) in settings.into_iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), key)
            .env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
    cmd
}

/// Git may refresh index stat information while diffing even with optional
/// locks off. Give those commands a private copy, preserving its mtime so
/// Git still detects entries changed in the same second as the index.
struct IndexCopy(PathBuf);

impl Drop for IndexCopy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut lock = self.0.clone().into_os_string();
        lock.push(".lock");
        let _ = fs::remove_file(lock);
    }
}

impl IndexCopy {
    fn of(repo: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::OpenOptionsExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let index = PathBuf::from(std::ffi::OsString::from_vec(git_line(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?));
        let cannot = |e: io::Error| format!("インデックスの写しを作れません: {e}");
        let (copy, mut file) = loop {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = std::env::temp_dir().join(format!(
                "noa-review-index-{}-{}-{stamp}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::File::options()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => break (Self(path), file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(cannot(e)),
            }
        };
        match fs::File::open(&index) {
            Ok(mut source) => {
                io::copy(&mut source, &mut file).map_err(cannot)?;
                file.set_modified(
                    source
                        .metadata()
                        .and_then(|m| m.modified())
                        .map_err(cannot)?,
                )
                .map_err(cannot)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::remove_file(&copy.0).map_err(cannot)?;
            }
            Err(e) => return Err(cannot(e)),
        }
        Ok(copy)
    }
}

#[derive(Debug)]
enum RunError {
    /// The output passed the cap.
    TooLarge,
    Failed(String),
}

/// Runs `cmd`, reading at most `cap` bytes of its output; `ok_codes` are
/// the exit codes that count as success (`diff --no-index` exits 1 on a
/// difference).
fn run_capped(mut cmd: Command, ok_codes: &[i32], cap: u64) -> Result<Vec<u8>, RunError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Failed(format!("git を起動できません: {e}")))?;
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut text);
        let _ = io::copy(&mut stderr, &mut io::sink());
        text
    });
    let mut out = Vec::new();
    let stdout = child.stdout.take().expect("piped");
    let read = stdout.take(cap + 1).read_to_end(&mut out);
    if out.len() as u64 > cap {
        let _ = child.kill();
        let _ = child.wait();
        let _ = errors.join();
        return Err(RunError::TooLarge);
    }
    read.map_err(|e| RunError::Failed(e.to_string()))?;
    let status = child.wait().map_err(|e| RunError::Failed(e.to_string()))?;
    let errors = errors.join().unwrap_or_default();
    match status.code() {
        Some(code) if ok_codes.contains(&code) => Ok(out),
        _ => Err(RunError::Failed(
            String::from_utf8_lossy(&errors).trim().to_owned(),
        )),
    }
}

fn run(cmd: Command, cap: u64) -> Result<Vec<u8>, String> {
    run_capped(cmd, &[0], cap).map_err(|e| match e {
        RunError::TooLarge => "git の出力が大きすぎます。パスを絞ってください".to_owned(),
        RunError::Failed(why) => why,
    })
}

/// One line git prints, its bytes as they are: only the newline git ends
/// it with is taken off (a path may begin or end with spaces).
fn git_line(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = git(dir);
    cmd.args(args);
    let mut out = run(cmd, 64 * 1024)?;
    if out.last() == Some(&b'\n') {
        out.pop();
    }
    Ok(out)
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_line(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

fn commit_of(dir: &Path, rev: &str) -> Option<String> {
    git_text(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()
    .filter(|sha| !sha.is_empty())
}

/// The base a git target takes when none is given: the remote's default
/// branch, else `main`, else `master`.
pub fn default_base(repo: &Path) -> Option<String> {
    if let Ok(name) = git_text(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && !name.is_empty()
        && commit_of(repo, &name).is_some()
    {
        return Some(name);
    }
    ["main", "master"]
        .into_iter()
        .find(|name| commit_of(repo, name).is_some())
        .map(str::to_owned)
}

fn nul_fields(bytes: &[u8]) -> Result<Vec<String>, String> {
    bytes
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| {
            String::from_utf8(f.to_vec())
                .map_err(|_| "UTF-8 ではないパスや設定名は取り込めません".to_owned())
        })
        .collect()
}

/// One entry of `git diff --raw -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub status: char,
    pub old_mode: String,
    pub new_mode: String,
    pub new_sha: String,
    /// The old path of a rename or copy.
    pub from: Option<String>,
    pub path: String,
}

/// Reads `git diff --raw -z`: `:om nm osha nsha S\0path\0`, with a second
/// path after a rename or copy.
pub fn parse_raw(bytes: &[u8]) -> Result<Vec<RawEntry>, String> {
    let fields: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let text = |f: &[u8]| {
        String::from_utf8(f.to_vec()).map_err(|_| "UTF-8 ではないパスは取り込めません".to_owned())
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let head = fields[i];
        if head.is_empty() {
            i += 1;
            continue;
        }
        let head = text(head)?;
        let parts: Vec<&str> = head.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, _old_sha, new_sha, status] = parts[..] else {
            return Err(format!("git diff --raw を読めません: {head:?}"));
        };
        let code = status.chars().next().unwrap_or('M');
        let paths = if matches!(code, 'R' | 'C') { 2 } else { 1 };
        if i + paths >= fields.len() {
            return Err("git diff --raw が途中で切れています".to_owned());
        }
        let (from, path) = if paths == 2 {
            (Some(text(fields[i + 1])?), text(fields[i + 2])?)
        } else {
            (None, text(fields[i + 1])?)
        };
        out.push(RawEntry {
            status: code,
            old_mode: old_mode.to_owned(),
            new_mode: new_mode.to_owned(),
            new_sha: new_sha.to_owned(),
            from,
            path,
        });
        i += 1 + paths;
    }
    Ok(out)
}

/// A file type change is one raw entry but two patch sections: the old
/// type's deletion followed by the new type's addition.
fn sections(entry: &RawEntry) -> usize {
    let kind = |mode: &str| u32::from_str_radix(mode, 8).map_or(0, |mode| mode & 0o170000);
    let (old, new) = (kind(&entry.old_mode), kind(&entry.new_mode));
    if old != 0 && new != 0 && old != new {
        2
    } else {
        1
    }
}

/// Per file of a unified diff: additions, deletions and the new side's
/// changed line ranges (added lines), inclusive.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
    pub changed: Vec<(u32, u32)>,
}

/// Reads a unified diff into one [`DiffStat`] per `diff --git` section, in
/// order. Paths are not read from it: [`parse_raw`] names the sections.
pub fn parse_sections(diff: &str) -> Vec<DiffStat> {
    let mut out: Vec<DiffStat> = Vec::new();
    let mut new_line = 0u32;
    let mut in_hunk = false;
    for line in diff.split('\n') {
        if line.starts_with("diff --git ") {
            out.push(DiffStat::default());
            in_hunk = false;
            continue;
        }
        let Some(stat) = out.last_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("@@ ") {
            in_hunk = true;
            // "@@ -a,b +c,d @@"
            new_line = rest
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|c| c.split(',').next())
                .and_then(|c| c.parse().ok())
                .unwrap_or(1);
            continue;
        }
        if !in_hunk {
            continue;
        }
        if line.starts_with('+') {
            stat.additions += 1;
            match stat.changed.last_mut() {
                Some((_, end)) if *end + 1 == new_line => *end = new_line,
                _ => stat.changed.push((new_line, new_line)),
            }
            new_line += 1;
        } else if line.starts_with('-') {
            stat.deletions += 1;
        } else if line.starts_with(' ') {
            new_line += 1;
        }
    }
    out
}

/// Takes captured files in, keeping the caps.
struct Collector<'a> {
    dest: &'a Path,
    files: Vec<TargetFile>,
    omitted: Vec<Omitted>,
    bytes: u64,
}

impl Collector<'_> {
    fn omit(&mut self, path: &str, reason: impl Into<String>) {
        self.omitted.push(Omitted {
            path: path.to_owned(),
            reason: reason.into(),
        });
    }

    /// Takes `bytes` (read to at most [`MAX_FILE_BYTES`], `over` when the
    /// file held more) as `path`'s captured content.
    fn add(
        &mut self,
        path: &str,
        status: FileStatus,
        (bytes, over): (Vec<u8>, bool),
        stat: DiffStat,
    ) -> Result<(), String> {
        if over {
            self.omit(path, too_large(MAX_FILE_BYTES + 1));
            return Ok(());
        }
        if not_text(&bytes) {
            self.omit(path, "バイナリ（テキストではない）");
            return Ok(());
        }
        self.bytes += bytes.len() as u64;
        if self.bytes > MAX_TARGET_BYTES {
            return Err(format!(
                "対象が大きすぎます（{} MiB 超）。パスを絞ってください",
                MAX_TARGET_BYTES / (1024 * 1024)
            ));
        }
        write_file(&self.dest.join(FILES_DIR), path, &bytes)
            .map_err(|e| format!("{path} を書き出せません: {e}"))?;
        self.files.push(TargetFile {
            path: path.to_owned(),
            status,
            lines: line_count(&bytes),
            bytes: bytes.len() as u64,
            additions: stat.additions,
            deletions: stat.deletions,
            changed: stat.changed,
            digest: digest_bytes(&bytes),
        });
        Ok(())
    }
}

fn too_many(n: usize) -> String {
    format!("ファイルが多すぎます（{n} 件、上限 {MAX_TARGET_FILES} 件）。パスを絞ってください")
}

/// Captures `target` into `dest` (created; it must not exist yet).
pub fn capture(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
    if fs::symlink_metadata(dest).is_ok() {
        return Err(format!("{} は既にあります", dest.display()));
    }
    let mut last = String::new();
    for _ in 0..CAPTURE_TRIES {
        fs::create_dir_all(dest).map_err(|e| format!("{} を作れません: {e}", dest.display()))?;
        let result = match target.kind {
            TargetKind::Git => capture_git(target, dest, now),
            TargetKind::Files => capture_files(target, dest, now).map(Some),
            TargetKind::Document => capture_document(target, dest, now).map(Some),
        };
        match result {
            Ok(Some(manifest)) => {
                let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
                fsx::write_replace(dest, &dest.join(MANIFEST_FILE), &json)
                    .map_err(|e| format!("マニフェストを書けません: {e}"))?;
                return Ok(manifest);
            }
            // The work tree moved while it was read: take it again.
            Ok(None) => {
                let _ = fs::remove_dir_all(dest);
                last =
                    "取得中にレビュー対象が変わり続けました。編集が落ち着いてから開始してください"
                        .to_owned();
            }
            Err(why) => {
                let _ = fs::remove_dir_all(dest);
                return Err(why);
            }
        }
    }
    Err(last)
}

/// The top level of the work tree holding `path`.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    git_line(path, &["rev-parse", "--show-toplevel"])
        .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        .map_err(|_| format!("{} は git の作業ツリーではありません", path.display()))
}

/// A git target's diff and the files it names, as one look at the tree.
#[derive(PartialEq, Eq)]
struct DiffView {
    patch: Vec<u8>,
    raw: Vec<RawEntry>,
    untracked: Vec<String>,
    /// Untracked symbolic links: listed, never read.
    links: Vec<String>,
}

fn too_large_diff(bytes: u64) -> String {
    format!(
        "差分が大きすぎます（{} KiB 超、上限 {} MiB）。パスを絞ってください",
        bytes.div_ceil(1024),
        MAX_DIFF_BYTES / (1024 * 1024)
    )
}

fn diff_view(repo: &Path, target: &Target, range: &[String]) -> Result<DiffView, String> {
    let drivers = filter_drivers(repo)?;
    let index = IndexCopy::of(repo)?;
    let common = [
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-relative",
        "--find-renames",
    ];
    let mut cmd = git_unfiltered(repo, &drivers);
    cmd.env("GIT_INDEX_FILE", &index.0);
    cmd.arg("diff")
        .args(common)
        .args(["--unified=3", "--src-prefix=a/", "--dst-prefix=b/"])
        .args(range)
        .arg("--")
        .args(&target.paths);
    let mut patch = match run_capped(cmd, &[0], MAX_DIFF_BYTES) {
        Ok(patch) => patch,
        Err(RunError::TooLarge) => return Err(too_large_diff(MAX_DIFF_BYTES)),
        Err(RunError::Failed(why)) => return Err(why),
    };
    let mut cmd = git_unfiltered(repo, &drivers);
    cmd.env("GIT_INDEX_FILE", &index.0);
    cmd.arg("diff")
        .args(common)
        .args(["--raw", "-z", "--no-abbrev"])
        .args(range)
        .arg("--")
        .args(&target.paths);
    let raw = parse_raw(&run(cmd, MAX_LISTING_BYTES)?)?;
    let mut untracked = Vec::new();
    if target.head == Head::Worktree {
        let mut cmd = git_unfiltered(repo, &drivers);
        cmd.env("GIT_INDEX_FILE", &index.0);
        cmd.args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
            .args(&target.paths);
        untracked = nul_fields(&run(cmd, MAX_LISTING_BYTES)?)?;
        untracked.retain(|p| !p.split('/').any(|part| part == ".noa"));
    }
    // An untracked link has no patch here (a link to a folder gives none
    // at all): it is listed as omitted.
    let (links, untracked): (Vec<String>, Vec<String>) = untracked.into_iter().partition(|p| {
        safe_relative(p.trim_end_matches('/')).is_some_and(|rel| {
            fs::symlink_metadata(repo.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
        })
    });
    let listed = raw.len() + untracked.len() + links.len();
    if listed > MAX_TARGET_FILES {
        return Err(too_many(listed));
    }
    for path in &untracked {
        // A nested repository is listed as `dir/`: it has no patch.
        if path.ends_with('/') {
            continue;
        }
        let left = MAX_DIFF_BYTES - patch.len() as u64;
        let mut cmd = git_unfiltered(repo, &drivers);
        cmd.args(["diff", "--no-index"]).args(common).args([
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            "/dev/null",
            path,
        ]);
        match run_capped(cmd, &[0, 1], left) {
            Ok(more) => patch.extend(more),
            Err(RunError::TooLarge) => return Err(too_large_diff(MAX_DIFF_BYTES)),
            Err(RunError::Failed(why)) => return Err(why),
        }
    }
    Ok(DiffView {
        patch,
        raw,
        untracked,
        links,
    })
}

/// The bytes of a work-tree file under `repo`: link text for a symbolic
/// link, else the file read to its cap; `Err` gives the omission reason.
fn read_worktree(repo: &Path, path: &str) -> Result<(Vec<u8>, bool), String> {
    let Some(rel) = safe_relative(path) else {
        return Err("パスがルートの外を指しています".to_owned());
    };
    let full = repo.join(rel);
    match fs::symlink_metadata(&full) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(LINK_REASON.to_owned());
        }
        Ok(meta) if meta.is_dir() => return Err(SUBMODULE_REASON.to_owned()),
        _ => {}
    }
    fsx::open_read(repo, &full)
        .and_then(|file| fsx::read_capped(file, MAX_FILE_BYTES))
        .map_err(|e| {
            if fsx::is_link_error(&e) {
                LINK_REASON.to_owned()
            } else {
                format!("読めません: {e}")
            }
        })
}

/// Captures a git target; `Ok(None)` when a work tree moved while it was
/// read (the caller tries again).
fn capture_git(
    target: &Target,
    dest: &Path,
    now: Millis,
) -> Result<Option<TargetManifest>, String> {
    let Some(path) = &target.path else {
        return Err("リポジトリを指定してください".to_owned());
    };
    let repo = repo_root(path)?;
    let base_ref = match target
        .base
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(base) => base.to_owned(),
        None => default_base(&repo).ok_or(
            "比較元のブランチが見つかりません（origin の既定ブランチ・main・master がありません）",
        )?,
    };
    let base_sha = commit_of(&repo, &base_ref)
        .ok_or_else(|| format!("比較元 {base_ref:?} が見つかりません"))?;
    let (head_rev, head_ref) = match &target.head {
        Head::Worktree => ("HEAD".to_owned(), None),
        Head::Ref(r) => (r.trim().to_owned(), Some(r.trim().to_owned())),
    };
    let head_sha = commit_of(&repo, &head_rev)
        .ok_or_else(|| format!("比較先 {head_rev:?} が見つかりません"))?;
    let merge_base = git_text(&repo, &["merge-base", &base_sha, &head_sha])
        .map_err(|e| format!("共通の祖先が見つかりません: {e}"))?;
    let range: Vec<String> = match &target.head {
        Head::Worktree => vec![merge_base.clone()],
        Head::Ref(_) => vec![merge_base.clone(), head_sha.clone()],
    };

    let view = diff_view(&repo, target, &range)?;
    if view.raw.is_empty() && view.untracked.is_empty() && view.links.is_empty() {
        return Err("差分がありません".to_owned());
    }
    let stats = parse_sections(&String::from_utf8_lossy(&view.patch));
    let patched = view.raw.iter().map(sections).sum::<usize>()
        + view.untracked.iter().filter(|p| !p.ends_with('/')).count();
    if stats.len() != patched {
        return Err(format!(
            "差分を読み取れません（ファイル {patched} 件に対してパッチ {} 件）",
            stats.len()
        ));
    }
    fsx::write_replace(dest, &dest.join(DIFF_FILE), &view.patch)
        .map_err(|e| format!("差分を書けません: {e}"))?;

    let mut collector = Collector {
        dest,
        files: Vec::new(),
        omitted: Vec::new(),
        bytes: 0,
    };
    let mut stats = stats.into_iter();
    for entry in &view.raw {
        let mut stat = stats.next().unwrap_or_default();
        if sections(entry) == 2 {
            let added = stats.next().unwrap_or_default();
            stat = DiffStat {
                additions: stat.additions + added.additions,
                deletions: stat.deletions + added.deletions,
                changed: added.changed,
            };
        }
        let status = match entry.status {
            'A' => FileStatus::Added,
            'D' => FileStatus::Deleted,
            'R' | 'C' => FileStatus::Renamed {
                from: entry.from.clone().unwrap_or_default(),
            },
            _ => FileStatus::Modified,
        };
        if entry.new_mode == "160000" || entry.old_mode == "160000" {
            collector.omit(&entry.path, SUBMODULE_REASON);
            continue;
        }
        if status == FileStatus::Deleted {
            // `git rm --cached` leaves the same path untracked: the
            // post-image below is the one manifest entry for that path.
            if view.untracked.contains(&entry.path) || view.links.contains(&entry.path) {
                continue;
            }
            collector.files.push(TargetFile {
                path: entry.path.clone(),
                status,
                lines: 0,
                bytes: 0,
                additions: 0,
                deletions: stat.deletions,
                changed: Vec::new(),
                digest: 0,
            });
            continue;
        }
        if entry.new_mode == "120000" {
            collector.omit(&entry.path, LINK_REASON);
            continue;
        }
        let read = match &target.head {
            Head::Worktree => read_worktree(&repo, &entry.path),
            Head::Ref(_) => {
                let mut cmd = git(&repo);
                cmd.args(["cat-file", "blob", &entry.new_sha]);
                match run_capped(cmd, &[0], MAX_FILE_BYTES) {
                    Ok(bytes) => Ok((bytes, false)),
                    Err(RunError::TooLarge) => Ok((Vec::new(), true)),
                    Err(RunError::Failed(why)) => Err(format!("読めません: {why}")),
                }
            }
        };
        match read {
            Ok(read) => collector.add(&entry.path, status, read, stat)?,
            Err(reason) => collector.omit(&entry.path, reason),
        }
    }
    for path in &view.untracked {
        if path.ends_with('/') {
            collector.omit(path.trim_end_matches('/'), SUBMODULE_REASON);
            continue;
        }
        let stat = stats.next().unwrap_or_default();
        match read_worktree(&repo, path) {
            Ok(read) => collector.add(path, FileStatus::Untracked, read, stat)?,
            Err(reason) => collector.omit(path, reason),
        }
    }
    for path in &view.links {
        collector.omit(path.trim_end_matches('/'), LINK_REASON);
    }
    // A work tree is read twice over (diff, then files): when it moved in
    // between, the patch and the files may be different versions.
    if target.head == Head::Worktree && diff_view(&repo, target, &range)? != view {
        return Ok(None);
    }
    // Whether the snapshot holds changes not in `head_sha`, read before a
    // last look at HEAD: a commit since `head_sha` was read takes it again.
    let uncommitted = target.head == Head::Worktree
        && (!view.untracked.is_empty()
            || !view.links.is_empty()
            || differs(&repo, &head_sha, &target.paths)?);
    if target.head == Head::Worktree && commit_of(&repo, "HEAD").as_ref() != Some(&head_sha) {
        return Ok(None);
    }

    let head_label = match &target.head {
        Head::Worktree => {
            let branch = git_text(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap_or_else(|_| "HEAD".to_owned());
            format!("{branch}（作業ツリー）")
        }
        Head::Ref(r) => r.trim().to_owned(),
    };
    let n = collector.files.len() + collector.omitted.len();
    Ok(Some(TargetManifest {
        label: format!("{head_label} … {base_ref}（{n} ファイル）"),
        repo: Some(repo.clone()),
        base_ref: Some(base_ref),
        base_sha: Some(base_sha),
        head_ref,
        head_sha: Some(head_sha),
        merge_base: Some(merge_base),
        document_name: None,
        files: collector.files,
        omitted: collector.omitted,
        diff_bytes: view.patch.len() as u64,
        diff_digest: Some(digest_bytes(&view.patch)),
        uncommitted,
        captured_at: now,
    }))
}

/// Whether the work tree's tracked files under `paths` (all when empty)
/// differ from `head`: the work tree against the commit, as the snapshot is
/// read, so a change only staged and undone in the work tree is none.
fn differs(repo: &Path, head: &str, paths: &[String]) -> Result<bool, String> {
    let drivers = filter_drivers(repo)?;
    let index = IndexCopy::of(repo)?;
    let output = git_unfiltered(repo, &drivers)
        .env("GIT_INDEX_FILE", &index.0)
        .args([
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            head,
            "--",
        ])
        .args(paths)
        .output()
        .map_err(|e| format!("git diff: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(format!(
            "git diff: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// Skipped by a walk outside git: hidden entries, build and package dirs.
fn walk_skips(name: &str) -> bool {
    name.starts_with('.') || name == "target" || name == "node_modules"
}

fn push_unique(paths: &mut Vec<String>, path: String) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn utf8_name(name: std::ffi::OsString) -> io::Result<String> {
    name.into_string().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "UTF-8 ではないパスは取り込めません",
        )
    })
}

/// Lists the files under `rel` (below `root`), never following a link:
/// links are listed in `links` instead. De-duplicate while collecting so
/// overlapping selections cannot spend the cap before later files are read.
fn walk(root: &Path, rel: &Path, out: &mut Vec<String>, links: &mut Vec<String>) -> io::Result<()> {
    let mut entries = fs::read_dir(root.join(rel))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = utf8_name(entry.file_name())?;
        if walk_skips(&name) {
            continue;
        }
        let kind = entry.file_type()?;
        let child = rel.join(&name);
        let child_text = child.to_str().expect("UTF-8 path components").to_owned();
        if kind.is_symlink() {
            push_unique(links, child_text);
        } else if kind.is_dir() {
            walk(root, &child, out, links)?;
        } else if kind.is_file() {
            push_unique(out, child_text);
        }
        if out.len() + links.len() > MAX_TARGET_FILES {
            return Ok(());
        }
    }
    Ok(())
}

fn capture_files(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
    let Some(root) = &target.path else {
        return Err("ルートのフォルダを指定してください".to_owned());
    };
    // The root is the person's choice; what lies below it is not.
    let root =
        fs::canonicalize(root).map_err(|e| format!("{} を開けません: {e}", root.display()))?;
    if !root.is_dir() {
        return Err(format!("{} はフォルダではありません", root.display()));
    }
    let in_git =
        git_text(&root, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true");
    let mut paths = Vec::new();
    let mut links = Vec::new();
    if in_git {
        let mut cmd = git(&root);
        cmd.args(["ls-files", "-co", "--exclude-standard", "-z", "--"])
            .args(&target.paths);
        paths = nul_fields(&run(cmd, MAX_LISTING_BYTES)?)?;
        // `.noa/` holds review snapshots of its own; never review them.
        paths.retain(|p| !p.split('/').any(|part| part == ".noa"));
        paths.sort();
        paths.dedup();
    } else if target.paths.is_empty() {
        walk(&root, Path::new(""), &mut paths, &mut links).map_err(|e| e.to_string())?;
    } else {
        for rel in &target.paths {
            let Some(safe) = safe_relative(rel) else {
                return Err(format!("パス {rel:?} はルートの外を指しています"));
            };
            let full = root.join(&safe);
            let text = safe.to_str().expect("path from a String").to_owned();
            if fsx::check_no_links(&root, &full).is_err() {
                push_unique(&mut links, text);
                continue;
            }
            match fs::symlink_metadata(&full) {
                Ok(meta) if meta.is_dir() => {
                    walk(&root, &safe, &mut paths, &mut links).map_err(|e| e.to_string())?;
                }
                Ok(meta) if meta.is_file() => push_unique(&mut paths, text),
                Ok(_) => push_unique(&mut links, text),
                Err(_) => return Err(format!("{rel} が見つかりません")),
            }
        }
        paths.sort();
        paths.dedup();
    }
    links.sort();
    links.dedup();
    if paths.len() + links.len() > MAX_TARGET_FILES {
        return Err(too_many(paths.len() + links.len()));
    }
    let mut collector = Collector {
        dest,
        files: Vec::new(),
        omitted: Vec::new(),
        bytes: 0,
    };
    for link in &links {
        collector.omit(link, LINK_REASON);
    }
    for path in &paths {
        match read_worktree(&root, path) {
            Ok(read) => collector.add(path, FileStatus::Plain, read, DiffStat::default())?,
            // A tracked file deleted in the work tree is not there to review.
            Err(reason)
                if in_git && !root.join(path).exists() && reason.starts_with("読めません") => {
            }
            Err(reason) => collector.omit(path, reason),
        }
    }
    if collector.files.is_empty() && collector.omitted.is_empty() {
        return Err("対象のファイルがありません".to_owned());
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let n = collector.files.len() + collector.omitted.len();
    Ok(TargetManifest {
        label: format!("{name}（{n} ファイル）"),
        repo: Some(root.clone()),
        files: collector.files,
        omitted: collector.omitted,
        captured_at: now,
        ..TargetManifest::default()
    })
}

fn capture_document(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
    let typed = target.text.as_deref().filter(|t| !t.trim().is_empty());
    let (bytes, name) = match (typed, &target.path) {
        // Kept byte for byte: line numbers must match what was pasted.
        (Some(text), _) => (text.as_bytes().to_vec(), None),
        (None, Some(path)) => {
            let file = fs::File::open(path)
                .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
            let (bytes, over) = fsx::read_capped(file, MAX_DOCUMENT_BYTES)
                .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
            if over {
                return Err(format!(
                    "文書が大きすぎます（{} KiB 超、上限 1 MiB）",
                    MAX_DOCUMENT_BYTES.div_ceil(1024)
                ));
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            (bytes, name)
        }
        (None, None) => {
            return Err("レビューする文書を入力するかファイルを指定してください".to_owned());
        }
    };
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "文書が大きすぎます（{} KiB、上限 1 MiB）",
            bytes.len().div_ceil(1024)
        ));
    }
    if not_text(&bytes) {
        return Err("文書がテキストではありません（NUL を含むか UTF-8 ではありません）".to_owned());
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.trim().is_empty() {
        return Err("文書が空です".to_owned());
    }
    fsx::write_replace(dest, &dest.join(DOCUMENT_FILE), &bytes)
        .map_err(|e| format!("文書を書けません: {e}"))?;
    let label = match &name {
        Some(name) => name.clone(),
        None => {
            let first = text
                .lines()
                .map(|l| l.trim().trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("文書");
            let mut label: String = first.chars().take(40).collect();
            if first.chars().count() > 40 {
                label.push('…');
            }
            label
        }
    };
    Ok(TargetManifest {
        label,
        document_name: name,
        files: vec![TargetFile {
            path: DOCUMENT_FILE.to_owned(),
            status: FileStatus::Plain,
            lines: line_count(&bytes),
            bytes: bytes.len() as u64,
            additions: 0,
            deletions: 0,
            changed: Vec::new(),
            digest: digest_bytes(&bytes),
        }],
        captured_at: now,
        ..TargetManifest::default()
    })
}

/// Whether the snapshot in `dir` is still what `manifest` describes: the
/// directory and its manifest are there and match, and the diff and every
/// captured file hash the same. Nothing is read through a link.
pub fn verify(dir: &Path, manifest: &TargetManifest) -> Result<(), String> {
    let changed = || "レビュー対象のスナップショットが変更されています".to_owned();
    let meta = fs::symlink_metadata(dir)
        .map_err(|_| "レビュー対象のスナップショットがありません".to_owned())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(changed());
    }
    let read = |path: &Path| -> Result<Vec<u8>, String> {
        let file = fsx::open_read(dir, path).map_err(|_| changed())?;
        Ok(fsx::read_capped(file, MAX_TARGET_BYTES)
            .map_err(|_| changed())?
            .0)
    };
    let stored: TargetManifest =
        serde_json::from_slice(&read(&dir.join(MANIFEST_FILE))?).map_err(|_| changed())?;
    if stored != *manifest {
        return Err(changed());
    }
    if let Some(want) = manifest.diff_digest
        && digest_bytes(&read(&dir.join(DIFF_FILE))?) != want
    {
        return Err(changed());
    }
    for file in &manifest.files {
        if file.status == FileStatus::Deleted {
            continue;
        }
        let path = snapshot_path(dir, manifest, &file.path).ok_or_else(changed)?;
        if digest_bytes(&read(&path)?) != file.digest {
            return Err(changed());
        }
    }
    Ok(())
}

/// Where a captured file lives in the snapshot `dir`.
pub fn snapshot_path(dir: &Path, manifest: &TargetManifest, path: &str) -> Option<PathBuf> {
    if manifest.repo.is_none() && path == DOCUMENT_FILE {
        return Some(dir.join(DOCUMENT_FILE));
    }
    Some(dir.join(FILES_DIR).join(safe_relative(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-review-capture-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repository on `main` with two files, and a branch `feat` that
    /// changes one, adds one and deletes one.
    fn repo() -> PathBuf {
        let dir = temp("repo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        fs::write(dir.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        fs::remove_file(dir.join("gone.txt")).unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "feat"]);
        dir
    }

    fn git_target(repo: &Path, head: Head) -> Target {
        Target {
            kind: TargetKind::Git,
            path: Some(repo.to_owned()),
            base: Some("main".into()),
            head,
            paths: Vec::new(),
            text: None,
            workdir: None,
        }
    }

    #[test]
    fn a_branch_is_captured_from_its_merge_base_without_touching_the_repository() {
        let repo = repo();
        let status_before = sh(&repo, &["status", "--porcelain"]);
        let dest = temp("dest").join("target");
        let manifest = capture(&git_target(&repo, Head::Ref("feat".into())), &dest, 7).unwrap();

        let mb = sh(&repo, &["merge-base", "main", "feat"]).trim().to_owned();
        let want = sh(
            &repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--find-renames",
                &mb,
                "feat",
                "--",
            ],
        );
        assert_eq!(fs::read_to_string(dest.join(DIFF_FILE)).unwrap(), want);
        assert_eq!(manifest.merge_base.as_deref(), Some(mb.as_str()));
        assert_eq!(manifest.base_ref.as_deref(), Some("main"));
        assert_eq!(
            manifest.head_sha.as_deref(),
            Some(sh(&repo, &["rev-parse", "feat"]).trim())
        );
        let a = manifest.file("a.txt").unwrap();
        assert_eq!(a.status, FileStatus::Modified);
        assert_eq!((a.lines, a.additions, a.deletions), (4, 2, 1));
        assert_eq!(a.changed, vec![(2, 2), (4, 4)]);
        assert_eq!(
            fs::read_to_string(dest.join(FILES_DIR).join("a.txt")).unwrap(),
            "one\nTWO\nthree\nfour\n"
        );
        assert_eq!(manifest.file("new.txt").unwrap().status, FileStatus::Added);
        let gone = manifest.file("gone.txt").unwrap();
        assert_eq!((gone.status.clone(), gone.lines), (FileStatus::Deleted, 0));
        assert!(!dest.join(FILES_DIR).join("gone.txt").exists());
        assert_eq!(sh(&repo, &["status", "--porcelain"]), status_before);
        assert!(dest.join(MANIFEST_FILE).is_file());
        verify(&dest, &manifest).unwrap();
        fs::write(dest.join(FILES_DIR).join("a.txt"), "edited").unwrap();
        assert!(verify(&dest, &manifest).is_err());
    }

    /// `uncommitted` says whether the captured work tree differs from the
    /// captured HEAD: a change staged and then undone in the work tree is
    /// not one; an edit or an untracked file is.
    #[test]
    fn the_work_tree_mode_marks_uncommitted_by_the_work_tree_against_head() {
        let take = |repo: &Path| {
            let dest = temp("dest").join("target");
            capture(&git_target(repo, Head::Worktree), &dest, 7)
                .unwrap()
                .uncommitted
        };
        let repo = repo();
        assert!(!take(&repo), "clean");
        fs::write(repo.join("a.txt"), "staged\n").unwrap();
        sh(&repo, &["add", "a.txt"]);
        fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        assert_eq!(sh(&repo, &["diff", "HEAD", "--"]), "");
        assert!(!take(&repo), "staged, then undone in the work tree");
        sh(&repo, &["reset", "-q"]);
        fs::write(repo.join("a.txt"), "edited\n").unwrap();
        assert!(take(&repo), "an edit");
        sh(&repo, &["checkout", "--", "a.txt"]);
        fs::write(repo.join("draft.txt"), "wip\n").unwrap();
        assert!(take(&repo), "an untracked file");
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn the_work_tree_mode_takes_uncommitted_and_untracked_changes_and_leaves_the_index_alone() {
        let repo = repo();
        fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
        fs::write(repo.join("draft.txt"), "wip\nmore\n").unwrap();
        let index_before = fs::read(repo.join(".git/index")).unwrap();
        assert_eq!(sh(&repo, &["diff", "--cached"]), "");
        let dest = temp("dest").join("target");
        let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 7).unwrap();
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index_before);
        assert_eq!(sh(&repo, &["diff", "--cached"]), "");
        assert_eq!(manifest.file("a.txt").unwrap().lines, 5);
        let draft = manifest.file("draft.txt").unwrap();
        assert_eq!(draft.status, FileStatus::Untracked);
        assert_eq!((draft.additions, draft.changed.clone()), (2, vec![(1, 2)]));
        let diff = fs::read_to_string(dest.join(DIFF_FILE)).unwrap();
        assert!(diff.contains("+five"), "{diff}");
        assert!(diff.contains("+wip"), "{diff}");
        assert!(
            manifest.label.contains("feat（作業ツリー）"),
            "{}",
            manifest.label
        );
    }

    #[test]
    fn an_empty_diff_a_missing_base_and_the_caps_refuse_the_start() {
        let repo = repo();
        let dest = |n: &str| temp(n).join("target");
        let same = Target {
            base: Some("feat".into()),
            ..git_target(&repo, Head::Ref("feat".into()))
        };
        let empty = dest("empty");
        assert_eq!(capture(&same, &empty, 0).unwrap_err(), "差分がありません");
        assert!(
            !empty.exists(),
            "a refused capture left its directory behind"
        );
        let missing = Target {
            base: Some("nope".into()),
            ..git_target(&repo, Head::Worktree)
        };
        assert!(
            capture(&missing, &dest("base"), 0)
                .unwrap_err()
                .contains("比較元")
        );

        // Too many files.
        let many = temp("many");
        for i in 0..=MAX_TARGET_FILES {
            fs::write(many.join(format!("f{i:03}.txt")), "x\n").unwrap();
        }
        let files = Target {
            kind: TargetKind::Files,
            path: Some(many.clone()),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        assert!(
            capture(&files, &dest("many"), 0)
                .unwrap_err()
                .contains("ファイルが多すぎます")
        );

        // Too many bytes: 25 files of just under 1 MiB.
        let big = temp("big");
        let chunk = "a".repeat(1024 * 1024 - 8);
        for i in 0..25 {
            fs::write(big.join(format!("b{i}.txt")), &chunk).unwrap();
        }
        let big_target = Target {
            path: Some(big),
            ..files.clone()
        };
        assert!(
            capture(&big_target, &dest("big"), 0)
                .unwrap_err()
                .contains("大きすぎます")
        );

        // A diff over 4 MiB.
        let huge = repo.join("huge");
        fs::create_dir_all(&huge).unwrap();
        for i in 0..5 {
            fs::write(huge.join(format!("h{i}.txt")), "line\n".repeat(190_000)).unwrap();
        }
        assert!(
            capture(&git_target(&repo, Head::Worktree), &dest("huge"), 0)
                .unwrap_err()
                .contains("差分が大きすぎます")
        );

        // A document over 1 MiB.
        let doc = Target {
            kind: TargetKind::Document,
            text: Some("x".repeat(1024 * 1024 + 1)),
            path: None,
            ..files
        };
        assert!(
            capture(&doc, &dest("doc"), 0)
                .unwrap_err()
                .contains("文書が大きすぎます")
        );
    }

    #[test]
    fn binary_and_oversized_files_are_listed_as_omitted() {
        let dir = temp("omit");
        fs::write(dir.join("ok.txt"), "fine\n").unwrap();
        fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        fs::write(dir.join("big.txt"), "z".repeat(MAX_FILE_BYTES as usize + 1)).unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(dir),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let dest = temp("omit-dest").join("target");
        let manifest = capture(&target, &dest, 0).unwrap();
        assert_eq!(manifest.files.len(), 1);
        let omitted: Vec<&str> = manifest.omitted.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(omitted, vec!["big.txt", "blob.bin"]);
        assert!(manifest.omitted[1].reason.contains("バイナリ"));
        assert!(manifest.omitted[0].reason.contains("1 MiB"));
    }

    #[test]
    fn files_in_git_follow_gitignore_and_never_take_noa_snapshots() {
        let repo = repo();
        fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(repo.join("ignored.txt"), "no\n").unwrap();
        fs::write(repo.join("loose.txt"), "yes\n").unwrap();
        fs::create_dir_all(repo.join(".noa/reviews/rev-1/target")).unwrap();
        fs::write(repo.join(".noa/reviews/rev-1/target/x.txt"), "old\n").unwrap();
        // No `.noa/.gitignore` here: git lists the snapshot as untracked,
        // and only the capture's own rule keeps it out.
        let target = Target {
            kind: TargetKind::Files,
            path: Some(repo.clone()),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let manifest = capture(&target, &temp("gi").join("target"), 0).unwrap();
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec![".gitignore", "a.txt", "loose.txt", "new.txt"]);

        // Outside git: hidden dirs, target and node_modules are skipped.
        let plain = temp("plain");
        for (p, body) in [
            ("src/main.rs", "fn main() {}\n"),
            (".hidden/x.txt", "h\n"),
            ("target/out.txt", "t\n"),
            ("node_modules/m.js", "m\n"),
        ] {
            let full = plain.join(p);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
        let manifest = capture(
            &Target {
                path: Some(plain),
                ..target
            },
            &temp("plain-dest").join("target"),
            0,
        )
        .unwrap();
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/main.rs"]);
    }

    #[test]
    fn a_document_is_captured_from_typed_text_and_from_a_file() {
        let typed = Target {
            kind: TargetKind::Document,
            path: None,
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: Some("\n\n# 移行計画\n\n    code\n1. 段階的に\n".into()),
            workdir: None,
        };
        let dest = temp("doc").join("target");
        let manifest = capture(&typed, &dest, 0).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join(DOCUMENT_FILE)).unwrap(),
            "\n\n# 移行計画\n\n    code\n1. 段階的に\n",
            "a pasted document is kept byte for byte"
        );
        assert_eq!(manifest.files[0].lines, 6);
        assert_eq!(manifest.label, "移行計画");
        assert_eq!(manifest.files[0].path, DOCUMENT_FILE);
        verify(&dest, &manifest).unwrap();

        let src = temp("doc-src").join("plan.md");
        fs::write(&src, "plan body\n").unwrap();
        let from_file = Target {
            text: None,
            path: Some(src),
            ..typed
        };
        let dest = temp("doc2").join("target");
        let manifest = capture(&from_file, &dest, 0).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join(DOCUMENT_FILE)).unwrap(),
            "plan body\n"
        );
        assert_eq!(manifest.document_name.as_deref(), Some("plan.md"));
        assert_eq!(manifest.files[0].lines, 1);
    }

    #[test]
    fn a_repository_whose_name_ends_in_a_space_is_not_taken_for_its_neighbour() {
        let parent = temp("space");
        for (name, file) in [("repo", "neighbour.txt"), ("repo ", "mine.txt")] {
            let dir = parent.join(name);
            fs::create_dir_all(&dir).unwrap();
            sh(&dir, &["init", "-q", "-b", "main"]);
            fs::write(dir.join(file), "x\n").unwrap();
            sh(&dir, &["add", "."]);
            sh(&dir, &["commit", "-q", "-m", "c"]);
            fs::write(dir.join(file), "y\n").unwrap();
        }
        let mine = parent.join("repo ");
        let top = repo_root(&mine).unwrap();
        assert_eq!(
            fs::canonicalize(&top).unwrap(),
            fs::canonicalize(&mine).unwrap()
        );
        let target = Target {
            kind: TargetKind::Git,
            path: Some(mine.clone()),
            base: Some("main".into()),
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let manifest = capture(&target, &temp("space-out").join("t"), 1).unwrap();
        let files: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["mine.txt"]);
    }

    #[test]
    fn hunks_give_the_new_sides_changed_ranges() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@\n a\n-b\n+B\n+C\n c\n@@ -10,2 +11,3 @@\n k\n+l\n m\n";
        let stats = parse_sections(diff);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].changed, vec![(2, 3), (12, 12)]);
        assert_eq!((stats[0].additions, stats[0].deletions), (3, 1));
    }

    fn branch_adding(repo: &Path, files: &[(&str, &str)]) {
        sh(repo, &["checkout", "-q", "-b", "extra", "main"]);
        for (name, body) in files {
            fs::write(repo.join(name), body).unwrap();
        }
        sh(repo, &["add", "-A"]);
        sh(repo, &["commit", "-q", "-m", "extra"]);
    }

    #[test]
    fn names_with_spaces_tabs_and_quotes_keep_their_changed_lines() {
        let repo = repo();
        branch_adding(
            &repo,
            &[
                ("sp ace.txt", "a\nb\n"),
                ("tab\tname.txt", "a\nb\n"),
                ("quo\"te.txt", "a\nb\n"),
                ("ユニ.txt", "a\nb\n"),
            ],
        );
        let manifest = capture(
            &git_target(&repo, Head::Ref("extra".into())),
            &temp("names").join("target"),
            0,
        )
        .unwrap();
        for name in ["sp ace.txt", "tab\tname.txt", "quo\"te.txt", "ユニ.txt"] {
            let f = manifest
                .file(name)
                .unwrap_or_else(|| panic!("{name:?} missing"));
            assert_eq!(
                (f.additions, f.changed.clone()),
                (2, vec![(1, 2)]),
                "{name:?}"
            );
        }
    }

    #[test]
    fn the_users_diff_settings_do_not_change_the_capture() {
        let repo = repo();
        for (key, value) in [
            ("diff.noprefix", "true"),
            ("diff.mnemonicPrefix", "true"),
            ("diff.algorithm", "histogram"),
            ("diff.context", "8"),
            ("diff.relative", "true"),
        ] {
            sh(&repo, &["config", key, value]);
        }
        fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
        let manifest = capture(
            &git_target(&repo, Head::Worktree),
            &temp("config").join("target"),
            0,
        )
        .unwrap();
        let a = manifest.file("a.txt").unwrap();
        assert_eq!((a.additions, a.changed.clone()), (3, vec![(2, 2), (4, 5)]));
    }

    #[test]
    fn a_diverged_base_and_a_rename_come_from_the_merge_base() {
        let repo = repo();
        // main moves on after feat branched: none of it is in the review.
        sh(&repo, &["checkout", "-q", "main"]);
        fs::write(repo.join("main-only.txt"), "later\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "main moves"]);
        sh(&repo, &["checkout", "-q", "feat"]);
        sh(&repo, &["mv", "a.txt", "renamed.txt"]);
        sh(&repo, &["commit", "-q", "-m", "rename"]);
        let manifest = capture(
            &git_target(&repo, Head::Ref("feat".into())),
            &temp("diverged").join("target"),
            0,
        )
        .unwrap();
        assert!(manifest.file("main-only.txt").is_none());
        assert_eq!(
            manifest.file("renamed.txt").unwrap().status,
            FileStatus::Renamed {
                from: "a.txt".into()
            }
        );
    }

    #[test]
    fn symbolic_links_are_listed_never_followed() {
        use std::os::unix::fs::symlink;
        let secret = temp("secret");
        fs::write(secret.join("key"), "topsecret").unwrap();

        // A committed link in the work tree, and an untracked one.
        let repo = repo();
        symlink(secret.join("key"), repo.join("link.txt")).unwrap();
        sh(&repo, &["add", "link.txt"]);
        symlink(&secret, repo.join("loose")).unwrap();
        let dest = temp("links").join("target");
        let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 0).unwrap();
        assert!(manifest.file("link.txt").is_none());
        assert!(
            manifest
                .omitted
                .iter()
                .any(|o| o.path == "link.txt" && o.reason == LINK_REASON)
        );
        let copied = walk_all(&dest);
        assert!(!copied.contains("topsecret"), "the link target was copied");

        // Files outside git: explicit file and directory links, and a walk.
        let root = temp("plain-links");
        fs::write(root.join("ok.txt"), "fine\n").unwrap();
        symlink(secret.join("key"), root.join("k")).unwrap();
        symlink(&secret, root.join("sdir")).unwrap();
        let files = Target {
            kind: TargetKind::Files,
            path: Some(root.clone()),
            base: None,
            head: Head::Worktree,
            paths: vec!["k".into(), "sdir".into(), "ok.txt".into()],
            text: None,
            workdir: None,
        };
        let dest = temp("plain-links-dest").join("target");
        let manifest = capture(&files, &dest, 0).unwrap();
        let names: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["ok.txt"]);
        let omitted: Vec<&str> = manifest.omitted.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(omitted, ["k", "sdir"]);
        let walked = capture(
            &Target {
                paths: Vec::new(),
                ..files
            },
            &temp("plain-walk-dest").join("target"),
            0,
        )
        .unwrap();
        assert_eq!(walked.files.len(), 1);
        assert_eq!(walked.omitted.len(), 2);
        assert!(!walk_all(&dest).contains("topsecret"));
    }

    /// Every file's text under `dir`, concatenated.
    fn walk_all(dir: &Path) -> String {
        let mut out = String::new();
        for entry in fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                out.push_str(&walk_all(&path));
            } else if kind.is_file() {
                out.push_str(&fs::read_to_string(&path).unwrap_or_default());
            }
        }
        out
    }

    #[test]
    fn a_submodule_change_is_listed_and_does_not_refuse_the_start() {
        let inner = repo();
        let repo = repo();
        sh(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                inner.to_str().unwrap(),
                "sm",
            ],
        );
        sh(&repo, &["commit", "-q", "-m", "sub"]);
        for head in [Head::Ref("feat".into()), Head::Worktree] {
            let manifest =
                capture(&git_target(&repo, head), &temp("sub").join("target"), 0).unwrap();
            assert!(
                manifest
                    .omitted
                    .iter()
                    .any(|o| o.path == "sm" && o.reason == SUBMODULE_REASON),
                "{:?}",
                manifest.omitted
            );
            assert!(manifest.file(".gitmodules").is_some());
        }
    }

    #[test]
    fn an_unreadable_file_is_listed_with_why() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("unreadable");
        fs::write(root.join("ok.txt"), "fine\n").unwrap();
        fs::write(root.join("locked.txt"), "no\n").unwrap();
        fs::set_permissions(root.join("locked.txt"), fs::Permissions::from_mode(0o000)).unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(root.clone()),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let manifest = capture(&target, &temp("unreadable-dest").join("target"), 0).unwrap();
        fs::set_permissions(root.join("locked.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(manifest.files.len(), 1);
        let locked = manifest
            .omitted
            .iter()
            .find(|o| o.path == "locked.txt")
            .unwrap();
        assert!(locked.reason.starts_with("読めません"), "{}", locked.reason);
    }

    #[test]
    fn a_document_that_is_not_text_is_refused() {
        let target = Target {
            kind: TargetKind::Document,
            path: None,
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: Some("plan\u{0}hidden".into()),
            workdir: None,
        };
        assert!(
            capture(&target, &temp("nul").join("target"), 0)
                .unwrap_err()
                .contains("テキストではありません")
        );
    }

    #[test]
    fn verify_notices_a_missing_or_changed_manifest_and_a_missing_snapshot() {
        let root = temp("verify");
        fs::write(root.join("a.txt"), "a\n").unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(root),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let dest = temp("verify-dest").join("target");
        let manifest = capture(&target, &dest, 0).unwrap();
        verify(&dest, &manifest).unwrap();
        let mut other = manifest.clone();
        other.label.push('x');
        fs::write(
            dest.join(MANIFEST_FILE),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        assert!(verify(&dest, &manifest).is_err());
        fs::remove_file(dest.join(MANIFEST_FILE)).unwrap();
        assert!(verify(&dest, &manifest).is_err());
        fs::remove_dir_all(&dest).unwrap();
        assert!(verify(&dest, &manifest).unwrap_err().contains("ありません"));
    }

    #[test]
    fn raw_entries_name_renames_and_modes() {
        let raw = b":100644 100644 aaaa bbbb M\0a b.txt\0:100644 100644 cccc dddd R087\0old\0new\0:160000 160000 e f M\0sm\0";
        let entries = parse_raw(raw).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "a b.txt");
        assert_eq!(
            (entries[1].from.as_deref(), entries[1].path.as_str()),
            (Some("old"), "new")
        );
        assert_eq!(entries[2].new_mode, "160000");
    }

    #[test]
    fn inherited_git_environment_does_not_change_the_target() {
        const CHILD: &str = "NOA_REVIEW_CAPTURE_GIT_ENV_TEST";
        if let Some(path) = std::env::var_os(CHILD) {
            let repo = PathBuf::from(path);
            let dest = temp("git-env-child").join("target");
            let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 0).unwrap();
            assert_eq!(manifest.repo, Some(fs::canonicalize(&repo).unwrap()));
            assert_eq!(
                fs::read_to_string(dest.join(FILES_DIR).join("a.txt")).unwrap(),
                "one\nTWO\nthree\nfour\n"
            );
            return;
        }
        let selected = repo();
        let other = repo();
        fs::write(other.join("a.txt"), "wrong repository\n").unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capture::tests::inherited_git_environment_does_not_change_the_target",
                "--nocapture",
            ])
            .env(CHILD, &selected)
            .env("GIT_DIR", other.join(".git"))
            .env("GIT_WORK_TREE", &other)
            .env("GIT_INDEX_FILE", other.join(".git/index"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "diff.suppressBlankEmpty")
            .env("GIT_CONFIG_VALUE_0", "true")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        fs::remove_dir_all(selected).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn worktree_capture_runs_no_filter_and_preserves_the_index() {
        let repo = repo();
        sh(
            &repo,
            &[
                "config",
                "filter.up.clean",
                "printf ran > clean-ran; tr a-z A-Z",
            ],
        );
        sh(&repo, &["config", "filter.up.required", "true"]);
        fs::write(
            repo.join(".gitattributes"),
            "up.txt filter=up\nnew-up.txt filter=up\n",
        )
        .unwrap();
        fs::write(repo.join("up.txt"), "hello\n").unwrap();
        sh(&repo, &["add", ".gitattributes", "up.txt"]);
        sh(&repo, &["commit", "-q", "-m", "filtered"]);
        fs::write(repo.join("up.txt"), "hello\nworld\n").unwrap();
        fs::write(repo.join("new-up.txt"), "fresh\n").unwrap();
        // Refreshing a file's stat cache must only affect the private index.
        fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        fs::remove_file(repo.join("clean-ran")).unwrap();
        let before = fs::read(repo.join(".git/index")).unwrap();

        let dest = temp("no-filter").join("target");
        let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 0).unwrap();
        assert!(!repo.join("clean-ran").exists());
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), before);
        assert_eq!(
            manifest.file("up.txt").unwrap().digest,
            digest_bytes(b"hello\nworld\n")
        );
        let diff = fs::read_to_string(dest.join(DIFF_FILE)).unwrap();
        assert!(
            diff.contains("+world\n") && diff.contains("+fresh\n"),
            "{diff}"
        );
        verify(&dest, &manifest).unwrap();
    }

    #[test]
    fn a_temporary_index_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let repo = repo();
        let copy = IndexCopy::of(&repo).unwrap();
        assert_eq!(
            fs::metadata(&copy.0).unwrap().permissions().mode() & 0o077,
            0,
            "the index contains private repository paths and staged object IDs"
        );
        assert_eq!(
            fs::read(&copy.0).unwrap(),
            fs::read(repo.join(".git/index")).unwrap()
        );
        let path = copy.0.clone();
        drop(copy);
        assert!(!path.exists());
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn type_changes_keep_following_files_aligned_with_their_patch_sections() {
        use std::os::unix::fs::symlink;
        let repo = temp("type-changes");
        sh(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("to-link.txt"), "was a file\n").unwrap();
        symlink("to-link.txt", repo.join("to-file.txt")).unwrap();
        fs::write(repo.join("z.txt"), "one\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "base"]);
        sh(&repo, &["checkout", "-q", "-b", "feat"]);
        fs::remove_file(repo.join("to-link.txt")).unwrap();
        symlink("z.txt", repo.join("to-link.txt")).unwrap();
        fs::remove_file(repo.join("to-file.txt")).unwrap();
        fs::write(repo.join("to-file.txt"), "now\na file\n").unwrap();
        fs::write(repo.join("z.txt"), "one\ntwo\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "types"]);

        for head in [Head::Ref("feat".into()), Head::Worktree] {
            let dest = temp("type-changes-dest").join("target");
            let manifest = capture(&git_target(&repo, head), &dest, 0).unwrap();
            assert!(manifest.omitted.iter().any(|o| o.path == "to-link.txt"));
            let file = manifest.file("to-file.txt").unwrap();
            assert_eq!(
                (file.additions, file.deletions, file.changed.clone()),
                (2, 1, vec![(1, 2)])
            );
            let z = manifest.file("z.txt").unwrap();
            assert_eq!(
                (z.additions, z.deletions, z.changed.clone()),
                (1, 0, vec![(2, 2)])
            );
            verify(&dest, &manifest).unwrap();
        }
    }

    #[test]
    fn a_file_removed_only_from_the_index_has_one_live_manifest_entry() {
        let repo = repo();
        sh(&repo, &["rm", "--cached", "a.txt"]);
        let dest = temp("untracked-again").join("target");
        let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 0).unwrap();
        assert_eq!(
            manifest.files.iter().filter(|f| f.path == "a.txt").count(),
            1
        );
        let file = manifest.file("a.txt").unwrap();
        assert_eq!(file.status, FileStatus::Untracked);
        assert_eq!(file.lines, 4);
        verify(&dest, &manifest).unwrap();
    }

    #[test]
    fn blank_context_lines_keep_their_line_numbers_despite_git_settings() {
        let repo = repo();
        sh(&repo, &["checkout", "-q", "main"]);
        fs::write(repo.join("a.txt"), "one\n\ntwo\n\nthree\n").unwrap();
        sh(&repo, &["commit", "-q", "-am", "blank base"]);
        fs::write(repo.join("a.txt"), "one\n\ntwo\n\nTHREE\n").unwrap();
        sh(&repo, &["config", "diff.suppressBlankEmpty", "true"]);
        let dest = temp("blank-lines").join("target");
        let manifest = capture(&git_target(&repo, Head::Worktree), &dest, 0).unwrap();
        assert_eq!(manifest.file("a.txt").unwrap().changed, vec![(5, 5)]);
    }

    #[test]
    fn plain_files_keep_literal_backslashes_and_spaces_in_their_names() {
        let root = temp("backslash");
        fs::write(root.join("a\\b.txt"), "literal backslash\n").unwrap();
        fs::write(root.join(" a.txt "), "spaces\n").unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(root),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        for paths in [Vec::new(), vec!["a\\b.txt".into(), " a.txt ".into()]] {
            let dest = temp("backslash-dest").join("target");
            let manifest = capture(
                &Target {
                    paths,
                    ..target.clone()
                },
                &dest,
                0,
            )
            .unwrap();
            assert_eq!(manifest.files.len(), 2);
            assert!(manifest.file("a\\b.txt").is_some());
            assert!(manifest.file(" a.txt ").is_some());
            assert_eq!(
                fs::read_to_string(dest.join(FILES_DIR).join("a\\b.txt")).unwrap(),
                "literal backslash\n"
            );
            assert_eq!(
                fs::read_to_string(dest.join(FILES_DIR).join(" a.txt ")).unwrap(),
                "spaces\n"
            );
            verify(&dest, &manifest).unwrap();
        }
    }

    #[test]
    fn overlapping_file_selections_count_unique_paths_before_enforcing_the_cap() {
        use std::os::unix::fs::symlink;
        let root = temp("overlapping-paths");
        for (folder, count) in [("a", 300), ("b", 50)] {
            fs::create_dir_all(root.join("folder").join(folder)).unwrap();
            for n in 0..count {
                fs::write(root.join(format!("folder/{folder}/{n:03}.txt")), "x\n").unwrap();
            }
        }
        symlink("missing", root.join("folder/link")).unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(root.clone()),
            base: None,
            head: Head::Worktree,
            paths: vec![
                "folder/a".into(),
                "folder".into(),
                "folder/a/000.txt".into(),
                "folder/link".into(),
                "folder".into(),
            ],
            text: None,
            workdir: None,
        };
        let dest = temp("overlapping-paths-dest").join("target");
        let manifest = capture(&target, &dest, 0).unwrap();
        assert_eq!(manifest.files.len(), 350);
        assert!(manifest.file("folder/b/049.txt").is_some());
        assert_eq!(manifest.omitted.len(), 1);
        assert_eq!(manifest.omitted[0].path, "folder/link");
        verify(&dest, &manifest).unwrap();

        // More than 400 distinct entries is still refused, despite overlaps.
        for n in 50..101 {
            fs::write(root.join(format!("folder/b/{n:03}.txt")), "x\n").unwrap();
        }
        let refused = temp("overlapping-paths-over-cap").join("target");
        assert!(
            capture(&target, &refused, 0)
                .unwrap_err()
                .contains("ファイルが多すぎます")
        );
        assert!(!refused.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn non_utf8_paths_are_rejected_instead_of_aliasing_another_file() {
        use std::os::unix::ffi::OsStringExt;
        assert!(parse_raw(b":100644 100644 a b M\0bad-\xff\0").is_err());
        assert!(nul_fields(b"bad-\xff\0").is_err());
        // APFS rejects this name before a directory entry can be created.
        // Exercise the walk's exact conversion on the original bytes on
        // every platform, without relying on the filesystem accepting them.
        let invalid = std::ffi::OsString::from_vec(b"bad-\xff".to_vec());
        let error = utf8_name(invalid).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("UTF-8"));
        assert_eq!(utf8_name("bad-�".into()).unwrap(), "bad-�");
        let root = temp("non-utf8");
        fs::write(root.join("bad-�"), "different file\n").unwrap();
        let target = Target {
            kind: TargetKind::Files,
            path: Some(root),
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        let dest = temp("non-utf8-dest").join("target");
        let manifest = capture(&target, &dest, 0).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "bad-�");
        assert_eq!(
            fs::read_to_string(dest.join(FILES_DIR).join("bad-�")).unwrap(),
            "different file\n"
        );
        verify(&dest, &manifest).unwrap();
    }
}
