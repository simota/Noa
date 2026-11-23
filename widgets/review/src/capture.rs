//! Captures a review target into its snapshot (`<reply dir>/target/`) and
//! describes it in a [`TargetManifest`]. Runs once, when a review starts:
//! every agent then reads the same bytes, whatever happens to the workspace.
//!
//! Git targets are read with read-only commands only (`rev-parse`,
//! `merge-base`, `diff`, `ls-files`, `show`), with optional locks off, so the
//! index and the work tree are never written.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

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

pub const DIFF_FILE: &str = "diff.patch";
pub const FILES_DIR: &str = "files";
pub const DOCUMENT_FILE: &str = "document.md";
pub const MANIFEST_FILE: &str = "manifest.json";

/// A stable 64-bit FNV-1a digest of `bytes`.
pub fn digest_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Why a file is not captured: binary, or too large. `None` keeps it.
fn unfit(bytes: &[u8]) -> Option<String> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Some(format!("{} KiB（1 MiB 超）", bytes.len().div_ceil(1024)));
    }
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return Some("バイナリ（テキストではない）".to_owned());
    }
    None
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=never"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    cmd
}

/// Runs git and returns stdout; `ok_codes` are the exit codes that count
/// as success (`git diff --no-index` exits 1 on a difference).
fn run(mut cmd: Command, ok_codes: &[i32]) -> Result<Vec<u8>, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    match out.status.code() {
        Some(code) if ok_codes.contains(&code) => Ok(out.stdout),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
    }
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = git(dir);
    cmd.args(args);
    run(cmd, &[0]).map(|out| String::from_utf8_lossy(&out).trim().to_owned())
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

fn nul_fields(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

/// `git diff --name-status -z`: `(status, old path, new path)`.
fn parse_name_status(bytes: &[u8]) -> Vec<(char, Option<String>, String)> {
    let fields = nul_fields(bytes);
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let code = fields[i].chars().next().unwrap_or('M');
        if matches!(code, 'R' | 'C') && i + 2 < fields.len() {
            out.push((code, Some(fields[i + 1].clone()), fields[i + 2].clone()));
            i += 3;
        } else if i + 1 < fields.len() {
            out.push((code, None, fields[i + 1].clone()));
            i += 2;
        } else {
            break;
        }
    }
    out
}

/// Per new-side path of a unified diff: additions, deletions and the new
/// side's changed line ranges (added lines), inclusive.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
    pub changed: Vec<(u32, u32)>,
}

/// Reads a unified diff into a [`DiffStat`] per file, keyed by the path
/// after `+++ b/` (or the `--- a/` path for a deletion).
pub fn parse_diff(diff: &str) -> Vec<(String, DiffStat)> {
    let mut out: Vec<(String, DiffStat)> = Vec::new();
    let mut old_path: Option<String> = None;
    let mut new_line = 0u32;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
            old_path = None;
            continue;
        }
        if !in_hunk {
            if let Some(rest) = line.strip_prefix("--- ") {
                old_path = rest.strip_prefix("a/").map(str::to_owned);
                continue;
            }
            if let Some(rest) = line.strip_prefix("+++ ") {
                let path = match rest.strip_prefix("b/") {
                    Some(path) => path.to_owned(),
                    None => old_path.clone().unwrap_or_default(),
                };
                out.push((path, DiffStat::default()));
                continue;
            }
        }
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
        let Some((_, stat)) = out.last_mut() else {
            continue;
        };
        if line.starts_with('+') {
            stat.additions += 1;
            match stat.changed.last_mut() {
                Some((_, end)) if *end + 1 == new_line => *end = new_line,
                _ => stat.changed.push((new_line, new_line)),
            }
            new_line += 1;
        } else if line.starts_with('-') {
            stat.deletions += 1;
        } else if line.starts_with('\\') {
            // "\ No newline at end of file"
        } else {
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
    fn add(
        &mut self,
        path: &str,
        status: FileStatus,
        bytes: &[u8],
        stat: DiffStat,
    ) -> Result<(), String> {
        if let Some(reason) = unfit(bytes) {
            self.omitted.push(Omitted {
                path: path.to_owned(),
                reason,
            });
            return Ok(());
        }
        self.bytes += bytes.len() as u64;
        if self.bytes > MAX_TARGET_BYTES {
            return Err(format!(
                "対象が大きすぎます（{} MiB 超）。パスを絞ってください",
                MAX_TARGET_BYTES / (1024 * 1024)
            ));
        }
        write_file(&self.dest.join(FILES_DIR), path, bytes)
            .map_err(|e| format!("{path} を書き出せません: {e}"))?;
        self.files.push(TargetFile {
            path: path.to_owned(),
            status,
            lines: line_count(bytes),
            bytes: bytes.len() as u64,
            additions: stat.additions,
            deletions: stat.deletions,
            changed: stat.changed,
            digest: digest_bytes(bytes),
        });
        Ok(())
    }
}

fn too_many(n: usize) -> String {
    format!("ファイルが多すぎます（{n} 件、上限 {MAX_TARGET_FILES} 件）。パスを絞ってください")
}

/// Captures `target` into `dest` (created; it must not exist yet).
pub fn capture(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
    if dest.exists() {
        return Err(format!("{} は既にあります", dest.display()));
    }
    fs::create_dir_all(dest).map_err(|e| format!("{} を作れません: {e}", dest.display()))?;
    let result = match target.kind {
        TargetKind::Git => capture_git(target, dest, now),
        TargetKind::Files => capture_files(target, dest, now),
        TargetKind::Document => capture_document(target, dest, now),
    };
    match result {
        Ok(manifest) => {
            let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
            fs::write(dest.join(MANIFEST_FILE), json)
                .map_err(|e| format!("マニフェストを書けません: {e}"))?;
            Ok(manifest)
        }
        Err(why) => {
            let _ = fs::remove_dir_all(dest);
            Err(why)
        }
    }
}

/// The top level of the work tree holding `path`.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    git_text(path, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| format!("{} は git の作業ツリーではありません", path.display()))
}

fn capture_git(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
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
    let diff_args = |extra: &[&str]| {
        let mut cmd = git(&repo);
        cmd.args(["diff", "--no-ext-diff", "--no-color", "--find-renames"])
            .args(extra)
            .args(&range)
            .arg("--")
            .args(&target.paths);
        cmd
    };
    let mut diff = run(diff_args(&[]), &[0])?;
    let names = parse_name_status(&run(diff_args(&["--name-status", "-z"]), &[0])?);
    let mut untracked = Vec::new();
    if target.head == Head::Worktree {
        let mut cmd = git(&repo);
        cmd.args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
            .args(&target.paths);
        untracked = nul_fields(&run(cmd, &[0])?);
    }
    if names.len() + untracked.len() > MAX_TARGET_FILES {
        return Err(too_many(names.len() + untracked.len()));
    }
    for path in &untracked {
        let mut cmd = git(&repo);
        cmd.args([
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-color",
            "--",
            "/dev/null",
            path,
        ]);
        diff.extend(run(cmd, &[0, 1])?);
    }
    if names.is_empty() && untracked.is_empty() {
        return Err("差分がありません".to_owned());
    }
    if diff.len() as u64 > MAX_DIFF_BYTES {
        return Err(format!(
            "差分が大きすぎます（{} KiB、上限 {} MiB）。パスを絞ってください",
            diff.len().div_ceil(1024),
            MAX_DIFF_BYTES / (1024 * 1024)
        ));
    }
    let diff_text = String::from_utf8_lossy(&diff).into_owned();
    fs::write(dest.join(DIFF_FILE), &diff).map_err(|e| format!("差分を書けません: {e}"))?;
    let stats = parse_diff(&diff_text);
    let stat_of = |path: &str| {
        stats
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    };

    let mut collector = Collector {
        dest,
        files: Vec::new(),
        omitted: Vec::new(),
        bytes: 0,
    };
    let read_new = |path: &str| -> Result<Vec<u8>, String> {
        match &target.head {
            Head::Worktree => {
                fs::read(repo.join(path)).map_err(|e| format!("{path} を読めません: {e}"))
            }
            Head::Ref(_) => {
                let mut cmd = git(&repo);
                cmd.args(["show", &format!("{head_sha}:{path}")]);
                run(cmd, &[0])
            }
        }
    };
    for (code, old, path) in &names {
        let status = match code {
            'A' => FileStatus::Added,
            'D' => FileStatus::Deleted,
            'R' | 'C' => FileStatus::Renamed {
                from: old.clone().unwrap_or_default(),
            },
            _ => FileStatus::Modified,
        };
        if status == FileStatus::Deleted {
            let stat = stat_of(path);
            collector.files.push(TargetFile {
                path: path.clone(),
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
        let bytes = read_new(path)?;
        collector.add(path, status, &bytes, stat_of(path))?;
    }
    for path in &untracked {
        let bytes = fs::read(repo.join(path)).map_err(|e| format!("{path} を読めません: {e}"))?;
        collector.add(path, FileStatus::Untracked, &bytes, stat_of(path))?;
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
    Ok(TargetManifest {
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
        diff_bytes: diff.len() as u64,
        diff_digest: Some(digest_bytes(&diff)),
        captured_at: now,
    })
}

/// Skipped by a walk outside git: hidden entries, build and package dirs.
fn walk_skips(name: &str) -> bool {
    name.starts_with('.') || name == "target" || name == "node_modules"
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<String>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(root.join(rel))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if walk_skips(&name) {
            continue;
        }
        let kind = entry.file_type()?;
        let child = rel.join(&name);
        if kind.is_dir() {
            walk(root, &child, out)?;
        } else if kind.is_file() {
            out.push(child.to_string_lossy().replace('\\', "/"));
        }
        if out.len() > MAX_TARGET_FILES {
            return Ok(());
        }
    }
    Ok(())
}

fn capture_files(target: &Target, dest: &Path, now: Millis) -> Result<TargetManifest, String> {
    let Some(root) = &target.path else {
        return Err("ルートのフォルダを指定してください".to_owned());
    };
    if !root.is_dir() {
        return Err(format!("{} はフォルダではありません", root.display()));
    }
    let in_git = git_text(root, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true");
    let mut paths = Vec::new();
    if in_git {
        let mut cmd = git(root);
        cmd.args(["ls-files", "-co", "--exclude-standard", "-z", "--"])
            .args(&target.paths);
        paths = nul_fields(&run(cmd, &[0])?);
        // `.noa/` holds review snapshots of its own; never review them.
        paths.retain(|p| !p.split('/').any(|part| part == ".noa"));
        paths.sort();
        paths.dedup();
    } else if target.paths.is_empty() {
        walk(root, Path::new(""), &mut paths).map_err(|e| e.to_string())?;
    } else {
        for rel in &target.paths {
            let Some(safe) = safe_relative(rel.trim()) else {
                return Err(format!("パス {rel:?} はルートの外を指しています"));
            };
            let full = root.join(&safe);
            if full.is_dir() {
                walk(root, &safe, &mut paths).map_err(|e| e.to_string())?;
            } else if full.is_file() {
                paths.push(safe.to_string_lossy().replace('\\', "/"));
            } else {
                return Err(format!("{rel} が見つかりません"));
            }
        }
        paths.sort();
        paths.dedup();
    }
    if paths.len() > MAX_TARGET_FILES {
        return Err(too_many(paths.len()));
    }
    if paths.is_empty() {
        return Err("対象のファイルがありません".to_owned());
    }
    let mut collector = Collector {
        dest,
        files: Vec::new(),
        omitted: Vec::new(),
        bytes: 0,
    };
    for path in &paths {
        let full = root.join(path);
        // A tracked file deleted in the work tree is not there to review.
        let Ok(bytes) = fs::read(&full) else {
            continue;
        };
        collector.add(path, FileStatus::Plain, &bytes, DiffStat::default())?;
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
    let typed = target
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let (bytes, name) = match (typed, &target.path) {
        (Some(text), _) => (text.as_bytes().to_vec(), None),
        (None, Some(path)) => {
            let bytes =
                fs::read(path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
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
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err("文書が UTF-8 のテキストではありません".to_owned());
    };
    if text.trim().is_empty() {
        return Err("文書が空です".to_owned());
    }
    fs::write(dest.join(DOCUMENT_FILE), &bytes).map_err(|e| format!("文書を書けません: {e}"))?;
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
/// diff and every captured file hash the same.
pub fn verify(dir: &Path, manifest: &TargetManifest) -> Result<(), String> {
    let changed = || "レビュー対象のスナップショットが変更されています".to_owned();
    if let Some(want) = manifest.diff_digest {
        let diff = fs::read(dir.join(DIFF_FILE)).map_err(|_| changed())?;
        if digest_bytes(&diff) != want {
            return Err(changed());
        }
    }
    for file in &manifest.files {
        if file.status == FileStatus::Deleted {
            continue;
        }
        let path = if file.path == DOCUMENT_FILE && manifest.repo.is_none() {
            dir.join(DOCUMENT_FILE)
        } else {
            let Some(rel) = safe_relative(&file.path) else {
                return Err(changed());
            };
            dir.join(FILES_DIR).join(rel)
        };
        let bytes = fs::read(path).map_err(|_| changed())?;
        if digest_bytes(&bytes) != file.digest {
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
        assert_eq!(
            capture(&same, &dest("empty"), 0).unwrap_err(),
            "差分がありません"
        );
        assert!(!dest("x").exists());
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
        fs::write(repo.join(".noa/.gitignore"), "*\n").unwrap();
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
            text: Some("# 移行計画\n\n1. 段階的に\n".into()),
            workdir: None,
        };
        let dest = temp("doc").join("target");
        let manifest = capture(&typed, &dest, 0).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join(DOCUMENT_FILE)).unwrap(),
            "# 移行計画\n\n1. 段階的に"
        );
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
    fn hunks_give_the_new_sides_changed_ranges() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@\n a\n-b\n+B\n+C\n c\n@@ -10,2 +11,3 @@\n k\n+l\n m\n";
        let stats = parse_diff(diff);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "x");
        assert_eq!(stats[0].1.changed, vec![(2, 3), (12, 12)]);
        assert_eq!((stats[0].1.additions, stats[0].1.deletions), (3, 1));
    }
}
