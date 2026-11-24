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

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fsx;
use crate::model::types::{
    FileStatus, Head, Millis, Omitted, Rebuild, SpecFile, Target, TargetFile, TargetKind,
    TargetManifest,
};

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

/// Most bytes a listing (`ls-files`, `diff --raw`) is read to.
const MAX_LISTING_BYTES: u64 = 4 * 1024 * 1024;
/// How often a work-tree capture is taken again when the tree moved under it.
const CAPTURE_TRIES: usize = 3;

const NOT_GIT: &str = "検証は git の作業ツリーでのみ行えます";
const LINK_REASON: &str = "シンボリックリンク（リンク先は取り込みません）";
const SUBMODULE_REASON: &str = "サブモジュール（中身は取り込みません）";

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

/// `rel` as a path with `/` separators.
fn slash_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Writes `bytes` to `root/rel`, `root` being a folder of the snapshot
/// `dest` (made on the way).
fn write_file(dest: &Path, root: &Path, rel: &str, bytes: &[u8]) -> io::Result<()> {
    let Some(rel_path) = safe_relative(rel) else {
        return Err(io::Error::other(format!("unsafe path {rel:?}")));
    };
    let path = root.join(rel_path);
    fsx::create_dirs(dest, path.parent().unwrap_or(root))?;
    fsx::write_replace(dest, &path, bytes)
}

/// The environment variables that pick another repository, index, object
/// store or way of finding them than `-C <dir>` says (`git rev-parse
/// --local-env-vars`, plus `GIT_NAMESPACE` and the two that bound
/// discovery): every git command the widget runs has them cleared, so
/// that, say, a `GIT_INDEX_FILE` the widget was started with is never
/// written.
const REPO_ENV: [&str; 18] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Clears [`REPO_ENV`] from `cmd`'s environment.
pub(crate) fn clear_repo_env(cmd: &mut Command) -> &mut Command {
    for var in REPO_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// Git with every setting that changes the output it is parsed from pinned.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for setting in [
        "core.quotePath=false",
        "color.ui=never",
        "diff.noprefix=false",
        "diff.mnemonicPrefix=false",
        "diff.relative=false",
        "diff.external=",
        "diff.renames=true",
        "diff.algorithm=myers",
        "diff.indentHeuristic=true",
    ] {
        cmd.args(["-c", setting]);
    }
    clear_repo_env(&mut cmd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_EXTERNAL_DIFF");
    cmd
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

/// The base a diff target takes when none is given: the remote's default
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

/// One entry of `git diff --raw -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawEntry {
    status: char,
    old_mode: String,
    new_mode: String,
    new_sha: String,
    /// The old path of a rename or copy.
    from: Option<String>,
    path: String,
}

/// Reads `git diff --raw -z`: `:om nm osha nsha S\0path\0`, with a second
/// path after a rename or copy.
fn parse_raw(bytes: &[u8]) -> Result<Vec<RawEntry>, String> {
    let fields: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let text = |f: &[u8]| String::from_utf8_lossy(f).into_owned();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let head = fields[i];
        if head.is_empty() {
            i += 1;
            continue;
        }
        let head = text(head);
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
            (Some(text(fields[i + 1])), text(fields[i + 2]))
        } else {
            (None, text(fields[i + 1]))
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

/// How many `diff --git` sections git's patch gives `entry`: two for a
/// change of file type (a file that became a link, a link or a submodule
/// that became a file, …), which git shows as a deletion and then an
/// addition of the path; one otherwise.
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
struct DiffStat {
    additions: u32,
    deletions: u32,
    changed: Vec<(u32, u32)>,
}

/// Reads a unified diff into one [`DiffStat`] per `diff --git` section, in
/// order. Paths are not read from it: [`parse_raw`] names the sections.
fn parse_sections(diff: &str) -> Vec<DiffStat> {
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
            return Err(too_many_bytes());
        }
        write_file(self.dest, &self.dest.join(FILES_DIR), path, &bytes)
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

fn too_many_bytes() -> String {
    format!(
        "対象が大きすぎます（{} MiB 超）。パスを絞ってください",
        MAX_TARGET_BYTES / (1024 * 1024)
    )
}

/// Captures `target` into `dest` (created; it must not exist yet): the
/// review-compatible snapshot, `worktree.patch` and `untracked/` in
/// work-tree mode, and `spec.md`. Refuses a non-git directory, an empty
/// diff (diff target), an untracked binary over
/// [`MAX_UNTRACKED_BINARY_BYTES`] and every cap, with the reason.
pub fn capture(target: &Target, dest: &Path, now: Millis) -> Result<Captured, String> {
    if fs::symlink_metadata(dest).is_ok() {
        return Err(format!("{} は既にあります", dest.display()));
    }
    let mut last = String::new();
    for _ in 0..CAPTURE_TRIES {
        fs::create_dir_all(dest).map_err(|e| format!("{} を作れません: {e}", dest.display()))?;
        match capture_once(target, dest, now) {
            Ok(Some(captured)) => {
                let json =
                    serde_json::to_vec_pretty(&captured.manifest).map_err(|e| e.to_string())?;
                if let Err(e) = fsx::write_replace(dest, &dest.join(MANIFEST_FILE), &json) {
                    let _ = fs::remove_dir_all(dest);
                    return Err(format!("マニフェストを書けません: {e}"));
                }
                return Ok(captured);
            }
            // The work tree moved while it was read: take it again.
            Ok(None) => {
                let _ = fs::remove_dir_all(dest);
                last = "取得中に検証対象が変わり続けました。編集が落ち着いてから開始してください"
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

/// The top level of the work tree holding `path`; a non-git directory is
/// refused with 「検証は git の作業ツリーでのみ行えます」.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    git_line(path, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        .ok_or_else(|| NOT_GIT.to_owned())
}

/// A target's diff and the files it names, as one look at the tree.
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

const DIFF_COMMON: [&str; 5] = [
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-relative",
    "--find-renames",
];

/// Whether `path` lies in a `.noa/` folder, which holds the widgets' own
/// files (this snapshot among them): never part of a target.
fn in_noa(path: &str) -> bool {
    path.split('/').any(|part| part == ".noa")
}

/// Untracked, non-ignored paths under `paths` (all, when empty).
fn untracked_paths(repo: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    let mut cmd = git(repo);
    cmd.args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
        .args(paths);
    let mut untracked = nul_fields(&run(cmd, MAX_LISTING_BYTES)?);
    untracked.retain(|p| !in_noa(p));
    Ok(untracked)
}

fn is_link(repo: &Path, path: &str) -> bool {
    safe_relative(path.trim_end_matches('/')).is_some_and(|rel| {
        fs::symlink_metadata(repo.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
    })
}

fn diff_view(
    repo: &Path,
    target: &Target,
    range: &[String],
    untracked: bool,
) -> Result<DiffView, String> {
    let mut cmd = git(repo);
    cmd.arg("diff")
        .args(DIFF_COMMON)
        .args(["--unified=3", "--src-prefix=a/", "--dst-prefix=b/"])
        .args(range)
        .arg("--")
        .args(&target.paths);
    let mut patch = match run_capped(cmd, &[0], MAX_DIFF_BYTES) {
        Ok(patch) => patch,
        Err(RunError::TooLarge) => return Err(too_large_diff(MAX_DIFF_BYTES)),
        Err(RunError::Failed(why)) => return Err(why),
    };
    let mut cmd = git(repo);
    cmd.arg("diff")
        .args(DIFF_COMMON)
        .args(["--raw", "-z", "--no-abbrev"])
        .args(range)
        .arg("--")
        .args(&target.paths);
    let raw = parse_raw(&run(cmd, MAX_LISTING_BYTES)?)?;
    let found = if untracked {
        untracked_paths(repo, &target.paths)?
    } else {
        Vec::new()
    };
    // An untracked link has no patch here (a link to a folder gives none
    // at all): it is listed as omitted.
    let (links, untracked): (Vec<String>, Vec<String>) =
        found.into_iter().partition(|p| is_link(repo, p));
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
        let mut cmd = git(repo);
        cmd.args(["diff", "--no-index"]).args(DIFF_COMMON).args([
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

/// The bytes of a work-tree file under `repo`, read to at most `cap`:
/// `Err` gives the omission reason (a link, a submodule, unreadable).
fn read_worktree(repo: &Path, path: &str, cap: u64) -> Result<(Vec<u8>, bool), String> {
    read_worktree_file(repo, path, cap).map(|(bytes, over, _)| (bytes, over))
}

/// Whether `file` is executable as git counts it (the owner's bit).
pub(crate) fn is_executable(file: &fs::File) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(file.metadata()?.permissions().mode() & 0o100 != 0)
}

/// Gives `file` the execute bits matching its read bits (`0644` → `0755`,
/// `0600` → `0700`), as git checks out an executable file.
pub(crate) fn make_executable(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode() & 0o7777;
    file.set_permissions(fs::Permissions::from_mode(mode | ((mode & 0o444) >> 2)))
}

/// As [`read_worktree`], with whether the file is executable.
fn read_worktree_file(repo: &Path, path: &str, cap: u64) -> Result<(Vec<u8>, bool, bool), String> {
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
        .and_then(|file| {
            let exec = is_executable(&file)?;
            let (bytes, over) = fsx::read_capped(file, cap)?;
            Ok((bytes, over, exec))
        })
        .map_err(|e| {
            if fsx::is_link_error(&e) {
                LINK_REASON.to_owned()
            } else {
                format!("読めません: {e}")
            }
        })
}

/// What a work-tree head's verifiers rebuild it from (L1-1): the binary
/// patch of every uncommitted change against the head commit, and every
/// untracked, non-ignored path (the whole tree, whatever the pathspec, so
/// the worktree is the person's tree).
#[derive(PartialEq, Eq)]
struct RecipeView {
    patch: Vec<u8>,
    untracked: Vec<String>,
}

fn recipe_view(repo: &Path, head_sha: &str) -> Result<RecipeView, String> {
    let mut cmd = git(repo);
    cmd.arg("diff").args([
        "--binary",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-relative",
        "--no-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        head_sha,
        "--",
    ]);
    let patch = match run_capped(cmd, &[0], MAX_WORKTREE_PATCH_BYTES) {
        Ok(patch) => patch,
        Err(RunError::TooLarge) => {
            return Err(format!(
                "作業ツリーの変更が大きすぎます（上限 {} MiB）",
                MAX_WORKTREE_PATCH_BYTES / (1024 * 1024)
            ));
        }
        Err(RunError::Failed(why)) => return Err(why),
    };
    Ok(RecipeView {
        patch,
        untracked: untracked_paths(repo, &[])?,
    })
}

/// Writes the rebuild recipe of `view` into `dest`: `worktree.patch`, and
/// each untracked file copied as is into `untracked/<path>`, an executable
/// one kept executable (the copy's mode is what the rebuild gives the
/// worktree's file). Untracked links and nested repositories are not
/// copied (a worktree gets no link
/// the person's tree pointed elsewhere with); a file over
/// [`MAX_UNTRACKED_BINARY_BYTES`] or one that cannot be read refuses the
/// start, named, since the worktree could not be rebuilt without it.
fn write_recipe(
    repo: &Path,
    dest: &Path,
    head_sha: &str,
    view: &RecipeView,
) -> Result<Rebuild, String> {
    fsx::write_replace(dest, &dest.join(WORKTREE_PATCH), &view.patch)
        .map_err(|e| format!("作業ツリーの差分を書けません: {e}"))?;
    let files: Vec<&String> = view
        .untracked
        .iter()
        .filter(|p| !p.ends_with('/') && !is_link(repo, p))
        .collect();
    if files.len() > MAX_TARGET_FILES {
        return Err(format!(
            "未追跡のファイルが多すぎます（{} 件、上限 {MAX_TARGET_FILES} 件）",
            files.len()
        ));
    }
    let mut total = 0u64;
    let mut untracked = Vec::new();
    for path in files {
        let (bytes, over, exec) = match read_worktree_file(repo, path, MAX_UNTRACKED_BINARY_BYTES) {
            Ok(read) => read,
            Err(reason) if reason == LINK_REASON || reason == SUBMODULE_REASON => continue,
            Err(reason) => return Err(format!("未追跡のファイル {path} を{reason}")),
        };
        if over {
            let kind = if not_text(&bytes) {
                "バイナリ"
            } else {
                "テキスト"
            };
            return Err(format!(
                "未追跡の{kind}ファイル {path} が大きすぎます（1 MiB 超）。検証者の worktree に写せません"
            ));
        }
        total += bytes.len() as u64;
        if total > MAX_TARGET_BYTES {
            return Err(too_many_bytes());
        }
        let root = dest.join(UNTRACKED_DIR);
        write_file(dest, &root, path, &bytes)
            .and_then(|()| {
                if !exec {
                    return Ok(());
                }
                let copy = root.join(safe_relative(path).expect("written above"));
                make_executable(&fsx::open_read(dest, &copy)?)
            })
            .map_err(|e| format!("{path} を書き出せません: {e}"))?;
        untracked.push((path.clone(), digest_bytes(&bytes)));
    }
    Ok(Rebuild {
        head_sha: head_sha.to_owned(),
        worktree_patch: Some(digest_bytes(&view.patch)),
        untracked,
    })
}

/// Copies the spec file `asked` (relative to `repo`, or an absolute path
/// inside it) into `dest/spec.md`.
fn capture_spec(repo: &Path, asked: &str, dest: &Path) -> Result<SpecFile, String> {
    let asked = asked.trim();
    let given = Path::new(asked);
    let relative = if given.is_absolute() {
        let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        let parent = given.parent().map(canonical).unwrap_or_default();
        let full = parent.join(given.file_name().unwrap_or_default());
        full.strip_prefix(canonical(repo))
            .ok()
            .and_then(|rel| safe_relative(&rel.to_string_lossy()))
    } else {
        safe_relative(asked)
    };
    let Some(rel) = relative else {
        return Err(format!(
            "仕様ファイル {asked} はリポジトリの中のファイルを指定してください"
        ));
    };
    let path = slash_path(&rel);
    let full = repo.join(&rel);
    let (bytes, over) = match fs::symlink_metadata(&full) {
        Err(_) => return Err(format!("仕様ファイル {path} が見つかりません")),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!("仕様ファイル {path} はシンボリックリンクです"));
        }
        Ok(_) => fsx::open_read(repo, &full)
            .and_then(|file| fsx::read_capped(file, MAX_SPEC_BYTES))
            .map_err(|e| format!("仕様ファイル {path} を読めません: {e}"))?,
    };
    if over {
        return Err(format!(
            "仕様ファイル {path} が大きすぎます（上限 {} MiB）",
            MAX_SPEC_BYTES / (1024 * 1024)
        ));
    }
    if not_text(&bytes) {
        return Err(format!(
            "仕様ファイル {path} がテキストではありません（NUL を含むか UTF-8 ではありません）"
        ));
    }
    fsx::write_replace(dest, &dest.join(SPEC_FILE), &bytes)
        .map_err(|e| format!("仕様ファイルを書けません: {e}"))?;
    Ok(SpecFile {
        path,
        bytes: bytes.len() as u64,
        lines: line_count(&bytes),
        digest: digest_bytes(&bytes),
    })
}

/// Captures `target` once; `Ok(None)` when a work tree moved while it was
/// read (the caller tries again).
fn capture_once(target: &Target, dest: &Path, now: Millis) -> Result<Option<Captured>, String> {
    let repo = repo_root(&target.repo)?;
    let (head_rev, head_ref) = match &target.head {
        Head::Worktree => ("HEAD".to_owned(), None),
        Head::Ref(r) => (r.trim().to_owned(), Some(r.trim().to_owned())),
    };
    let head_sha = commit_of(&repo, &head_rev)
        .ok_or_else(|| format!("比較先 {head_rev:?} が見つかりません"))?;
    let worktree = target.head == Head::Worktree;
    let recipe = if worktree {
        Some(recipe_view(&repo, &head_sha)?)
    } else {
        None
    };

    let (mut manifest, view) = match target.kind {
        TargetKind::Diff => capture_diff(&repo, target, dest, &head_sha, head_ref.clone())?,
        TargetKind::State => capture_state(&repo, target, dest, &head_sha, head_ref.clone())?,
    };
    manifest.captured_at = now;
    let spec = target
        .spec
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|asked| capture_spec(&repo, asked, dest))
        .transpose()?;
    let rebuild = match &recipe {
        Some(recipe) => write_recipe(&repo, dest, &head_sha, recipe)?,
        None => Rebuild {
            head_sha: head_sha.clone(),
            worktree_patch: None,
            untracked: Vec::new(),
        },
    };
    // A work tree is read more than once over (diff, files, recipe): when
    // it moved in between, they may be different versions.
    if worktree {
        let range = match target.kind {
            TargetKind::Diff => vec![manifest.merge_base.clone().unwrap_or_default()],
            TargetKind::State => vec![head_sha.clone()],
        };
        if diff_view(&repo, target, &range, true)? != view
            || recipe_view(&repo, &head_sha)? != recipe.expect("work-tree mode")
            || commit_of(&repo, "HEAD").as_deref() != Some(head_sha.as_str())
        {
            return Ok(None);
        }
    }
    Ok(Some(Captured {
        manifest,
        rebuild,
        spec,
    }))
}

fn head_label(repo: &Path, head: &Head) -> String {
    match head {
        Head::Worktree => {
            let branch = git_text(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap_or_else(|_| "HEAD".to_owned());
            format!("{branch}（作業ツリー）")
        }
        Head::Ref(r) => r.trim().to_owned(),
    }
}

/// A diff target (L1-1 item 1), as review's git capture.
fn capture_diff(
    repo: &Path,
    target: &Target,
    dest: &Path,
    head_sha: &str,
    head_ref: Option<String>,
) -> Result<(TargetManifest, DiffView), String> {
    let base_ref = match target
        .base
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(base) => base.to_owned(),
        None => default_base(repo).ok_or(
            "比較元のブランチが見つかりません（origin の既定ブランチ・main・master がありません）",
        )?,
    };
    let base_sha = commit_of(repo, &base_ref)
        .ok_or_else(|| format!("比較元 {base_ref:?} が見つかりません"))?;
    let merge_base = git_text(repo, &["merge-base", &base_sha, head_sha])
        .map_err(|e| format!("共通の祖先が見つかりません: {e}"))?;
    let range: Vec<String> = match &target.head {
        Head::Worktree => vec![merge_base.clone()],
        Head::Ref(_) => vec![merge_base.clone(), head_sha.to_owned()],
    };

    let view = diff_view(repo, target, &range, target.head == Head::Worktree)?;
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
            // The deletion of the old type, then the addition of the new.
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
            // Deleted from the index only (`git rm --cached`) and still in
            // the work tree as an untracked file: one path, listed once, as
            // the untracked file it now is (below), which is also what a
            // verifier's worktree is rebuilt with.
            if view.untracked.contains(&entry.path) {
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
            Head::Worktree => read_worktree(repo, &entry.path, MAX_FILE_BYTES),
            Head::Ref(_) => {
                let mut cmd = git(repo);
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
        match read_worktree(repo, path, MAX_FILE_BYTES) {
            Ok(read) => collector.add(path, FileStatus::Untracked, read, stat)?,
            Err(reason) => collector.omit(path, reason),
        }
    }
    for path in &view.links {
        collector.omit(path.trim_end_matches('/'), LINK_REASON);
    }

    let n = collector.files.len() + collector.omitted.len();
    let manifest = TargetManifest {
        label: format!(
            "{} … {base_ref}（{n} ファイル）",
            head_label(repo, &target.head)
        ),
        repo: Some(repo.to_owned()),
        base_ref: Some(base_ref),
        base_sha: Some(base_sha),
        head_ref,
        head_sha: Some(head_sha.to_owned()),
        merge_base: Some(merge_base),
        files: collector.files,
        omitted: collector.omitted,
        diff_bytes: view.patch.len() as u64,
        diff_digest: Some(digest_bytes(&view.patch)),
        captured_at: 0,
    };
    Ok((manifest, view))
}

/// A state target (L1-1 item 2): no base and no changed files; the diff
/// holds the uncommitted changes against `HEAD` in work-tree mode and is
/// empty for a ref.
fn capture_state(
    repo: &Path,
    target: &Target,
    dest: &Path,
    head_sha: &str,
    head_ref: Option<String>,
) -> Result<(TargetManifest, DiffView), String> {
    let view = match &target.head {
        Head::Worktree => diff_view(repo, target, &[head_sha.to_owned()], true)?,
        Head::Ref(_) => DiffView {
            patch: Vec::new(),
            raw: Vec::new(),
            untracked: Vec::new(),
            links: Vec::new(),
        },
    };
    fsx::write_replace(dest, &dest.join(DIFF_FILE), &view.patch)
        .map_err(|e| format!("差分を書けません: {e}"))?;
    let manifest = TargetManifest {
        label: format!("{}（現在の状態）", head_label(repo, &target.head)),
        repo: Some(repo.to_owned()),
        head_ref,
        head_sha: Some(head_sha.to_owned()),
        diff_bytes: view.patch.len() as u64,
        diff_digest: Some(digest_bytes(&view.patch)),
        ..TargetManifest::default()
    };
    Ok((manifest, view))
}

/// Whether the snapshot in `dir` is still what `manifest`, `rebuild` and
/// `spec` describe: the manifest, the diff, every captured file, the
/// worktree patch, every untracked file and `spec.md` (when `spec` is
/// given; with `None` there must be no `spec.md`) hash the same. Nothing
/// is read through a link.
pub fn verify(
    dir: &Path,
    manifest: &TargetManifest,
    rebuild: &Rebuild,
    spec: Option<&SpecFile>,
) -> Result<(), String> {
    let changed = || "検証対象のスナップショットが変更されています".to_owned();
    let meta = fs::symlink_metadata(dir)
        .map_err(|_| "検証対象のスナップショットがありません".to_owned())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(changed());
    }
    let read = |path: &Path| -> Result<Vec<u8>, String> {
        let file = fsx::open_read(dir, path).map_err(|_| changed())?;
        Ok(fsx::read_capped(file, MAX_TARGET_BYTES)
            .map_err(|_| changed())?
            .0)
    };
    let absent = |path: &Path| fs::symlink_metadata(path).is_err();
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
        let path = snapshot_path(dir, &file.path).ok_or_else(changed)?;
        if digest_bytes(&read(&path)?) != file.digest {
            return Err(changed());
        }
    }
    match rebuild.worktree_patch {
        Some(want) if digest_bytes(&read(&dir.join(WORKTREE_PATCH))?) != want => {
            return Err(changed());
        }
        None if !absent(&dir.join(WORKTREE_PATCH)) => return Err(changed()),
        _ => {}
    }
    for (path, want) in &rebuild.untracked {
        let rel = safe_relative(path).ok_or_else(changed)?;
        if digest_bytes(&read(&dir.join(UNTRACKED_DIR).join(rel))?) != *want {
            return Err(changed());
        }
    }
    let spec_changed = || "仕様ファイルの写しが変更されています".to_owned();
    match spec {
        Some(spec) => {
            let file = fsx::open_read(dir, &dir.join(SPEC_FILE)).map_err(|_| spec_changed())?;
            let bytes = fsx::read_capped(file, MAX_SPEC_BYTES)
                .map_err(|_| spec_changed())?
                .0;
            if digest_bytes(&bytes) != spec.digest || bytes.len() as u64 != spec.bytes {
                return Err(spec_changed());
            }
        }
        None if !absent(&dir.join(SPEC_FILE)) => return Err(spec_changed()),
        None => {}
    }
    Ok(())
}

/// Where a captured file (post-image) lives in the snapshot `dir`; `None`
/// for a path that is not plainly relative.
pub fn snapshot_path(dir: &Path, path: &str) -> Option<PathBuf> {
    Some(dir.join(FILES_DIR).join(safe_relative(path)?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) fn temp(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-verify-io-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Canonical, so paths compare with what git prints (`/private/var`
        // on macOS).
        fs::canonicalize(&dir).unwrap()
    }

    pub(crate) fn sh(dir: &Path, args: &[&str]) -> String {
        let mut git = Command::new("git");
        let out = clear_repo_env(&mut git)
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

    /// A repository on `main` with two files, and a branch `feat` (checked
    /// out) that changes one, adds one and deletes one.
    pub(crate) fn repo() -> PathBuf {
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

    pub(crate) fn diff_target(repo: &Path, head: Head) -> Target {
        Target {
            kind: TargetKind::Diff,
            repo: repo.to_owned(),
            base: Some("main".into()),
            head,
            paths: Vec::new(),
            spec: None,
        }
    }

    /// Uncommitted, staged and untracked changes (a binary one among
    /// them) on top of `feat`.
    pub(crate) fn dirty(repo: &Path) {
        fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
        fs::write(repo.join("staged.txt"), "staged\n").unwrap();
        sh(repo, &["add", "staged.txt"]);
        fs::create_dir_all(repo.join("notes")).unwrap();
        fs::write(repo.join("notes/draft.txt"), "wip\nmore\n").unwrap();
        fs::write(repo.join("blob.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
        fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(repo.join("ignored.txt"), "never\n").unwrap();
        fs::create_dir_all(repo.join(".noa/verify/ver-1")).unwrap();
        fs::write(repo.join(".noa/verify/ver-1/x.txt"), "own\n").unwrap();
    }

    fn status_and_index(repo: &Path) -> (String, Vec<u8>) {
        (
            sh(repo, &["status", "--porcelain", "--untracked-files=all"]),
            fs::read(repo.join(".git/index")).unwrap(),
        )
    }

    #[test]
    fn ac01_a_ref_head_gives_the_review_snapshot_and_a_recipe_of_its_commit() {
        let repo = repo();
        let before = status_and_index(&repo);
        let dest = temp("dest").join("target");
        let captured = capture(&diff_target(&repo, Head::Ref("feat".into())), &dest, 7).unwrap();
        let manifest = &captured.manifest;
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
        let feat = sh(&repo, &["rev-parse", "feat"]).trim().to_owned();
        assert_eq!(manifest.head_sha.as_deref(), Some(feat.as_str()));
        assert_eq!(manifest.captured_at, 7);
        let a = manifest.file("a.txt").unwrap();
        assert_eq!((a.lines, a.additions, a.deletions), (4, 2, 1));
        assert_eq!(a.changed, vec![(2, 2), (4, 4)]);
        assert_eq!(manifest.file("new.txt").unwrap().status, FileStatus::Added);
        assert_eq!(
            manifest.file("gone.txt").unwrap().status,
            FileStatus::Deleted
        );
        assert_eq!(
            captured.rebuild,
            Rebuild {
                head_sha: feat,
                worktree_patch: None,
                untracked: Vec::new(),
            }
        );
        assert!(!dest.join(WORKTREE_PATCH).exists());
        assert!(!dest.join(UNTRACKED_DIR).exists());
        assert_eq!(status_and_index(&repo), before);
        verify(&dest, manifest, &captured.rebuild, None).unwrap();
        fs::write(dest.join(FILES_DIR).join("a.txt"), "edited").unwrap();
        assert!(verify(&dest, manifest, &captured.rebuild, None).is_err());
    }

    #[test]
    fn ac01_the_work_tree_mode_adds_the_worktree_patch_and_untracked_files_and_writes_nothing_of_the_persons()
     {
        let repo = repo();
        dirty(&repo);
        let before = status_and_index(&repo);
        let dest = temp("dest").join("target");
        let captured = capture(&diff_target(&repo, Head::Worktree), &dest, 1).unwrap();
        assert_eq!(status_and_index(&repo), before, "status or index changed");

        let head = sh(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
        assert_eq!(captured.rebuild.head_sha, head);
        let patch = fs::read(dest.join(WORKTREE_PATCH)).unwrap();
        assert_eq!(captured.rebuild.worktree_patch, Some(digest_bytes(&patch)));
        let text = String::from_utf8_lossy(&patch);
        assert!(text.contains("+five") && text.contains("+staged"), "{text}");
        let untracked: Vec<&str> = captured
            .rebuild
            .untracked
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(untracked, [".gitignore", "blob.bin", "notes/draft.txt"]);
        assert_eq!(
            fs::read(dest.join(UNTRACKED_DIR).join("blob.bin")).unwrap(),
            [0u8, 159, 146, 150, 0, 1],
            "a binary untracked file is copied as is"
        );
        for (path, digest) in &captured.rebuild.untracked {
            let bytes = fs::read(dest.join(UNTRACKED_DIR).join(path)).unwrap();
            assert_eq!(digest_bytes(&bytes), *digest, "{path}");
        }
        assert!(!dest.join(UNTRACKED_DIR).join("ignored.txt").exists());
        assert!(!dest.join(UNTRACKED_DIR).join(".noa").exists());
        // The review-compatible half: the post-images and the diff.
        let draft = captured.manifest.file("notes/draft.txt").unwrap();
        assert_eq!(draft.status, FileStatus::Untracked);
        assert!(captured.manifest.is_omitted("blob.bin"));
        assert_eq!(captured.manifest.file("a.txt").unwrap().lines, 5);
        assert!(
            captured.manifest.label.contains("feat（作業ツリー）"),
            "{}",
            captured.manifest.label
        );
        verify(&dest, &captured.manifest, &captured.rebuild, None).unwrap();
        fs::write(dest.join(UNTRACKED_DIR).join("notes/draft.txt"), "x").unwrap();
        assert!(verify(&dest, &captured.manifest, &captured.rebuild, None).is_err());
    }

    #[test]
    fn ac01_a_state_target_lists_no_files_and_holds_only_uncommitted_changes() {
        let repo = repo();
        let state = |head| Target {
            kind: TargetKind::State,
            base: None,
            ..diff_target(&repo, head)
        };
        // A clean tree at a ref: an empty diff is fine for a state target.
        let dest = temp("state").join("target");
        let clean = capture(&state(Head::Ref("feat".into())), &dest, 0).unwrap();
        assert!(clean.manifest.files.is_empty());
        assert_eq!(clean.manifest.base_sha, None);
        assert_eq!(fs::read(dest.join(DIFF_FILE)).unwrap(), b"");
        assert!(clean.manifest.label.ends_with("（現在の状態）"));

        dirty(&repo);
        let dest = temp("state-wt").join("target");
        let captured = capture(&state(Head::Worktree), &dest, 0).unwrap();
        assert!(captured.manifest.files.is_empty());
        let diff = fs::read_to_string(dest.join(DIFF_FILE)).unwrap();
        assert!(diff.contains("+five") && diff.contains("+wip"), "{diff}");
        assert!(!diff.contains("+fresh"), "committed changes are not in it");
        assert!(captured.rebuild.worktree_patch.is_some());
        verify(&dest, &captured.manifest, &captured.rebuild, None).unwrap();
    }

    #[test]
    fn ac01_the_spec_file_is_copied_and_checked() {
        let repo = repo();
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/spec.md"), "# Spec\n\n1. tabs\n").unwrap();
        let target = Target {
            spec: Some("docs/spec.md".into()),
            ..diff_target(&repo, Head::Ref("feat".into()))
        };
        let dest = temp("spec").join("target");
        let captured = capture(&target, &dest, 0).unwrap();
        let spec = captured.spec.clone().unwrap();
        assert_eq!(
            (spec.path.as_str(), spec.lines, spec.bytes),
            ("docs/spec.md", 3, 16)
        );
        assert_eq!(
            fs::read_to_string(dest.join(SPEC_FILE)).unwrap(),
            "# Spec\n\n1. tabs\n"
        );
        let (manifest, rebuild) = (&captured.manifest, &captured.rebuild);
        verify(&dest, manifest, rebuild, Some(&spec)).unwrap();
        assert!(verify(&dest, manifest, rebuild, None).is_err());
        fs::write(dest.join(SPEC_FILE), "# Spec\n\n1. other\n").unwrap();
        assert!(
            verify(&dest, manifest, rebuild, Some(&spec))
                .unwrap_err()
                .contains("仕様ファイル")
        );
        // An absolute path inside the repository is taken as its relative.
        let absolute = Target {
            spec: Some(repo.join("docs/spec.md").to_string_lossy().into_owned()),
            ..target.clone()
        };
        let again = capture(&absolute, &temp("spec-abs").join("target"), 0).unwrap();
        assert_eq!(again.spec.unwrap().path, "docs/spec.md");
        for (asked, problem) in [
            ("../outside.md", "リポジトリの中"),
            ("docs/none.md", "見つかりません"),
        ] {
            let t = Target {
                spec: Some(asked.into()),
                ..target.clone()
            };
            let err = capture(&t, &temp("spec-bad").join("target"), 0).unwrap_err();
            assert!(err.contains(problem), "{asked}: {err}");
        }
        fs::write(
            repo.join("docs/big.md"),
            "x".repeat(MAX_SPEC_BYTES as usize + 1),
        )
        .unwrap();
        let big = Target {
            spec: Some("docs/big.md".into()),
            ..target
        };
        assert!(
            capture(&big, &temp("spec-big").join("target"), 0)
                .unwrap_err()
                .contains("大きすぎます")
        );
    }

    #[test]
    fn ac02_a_non_git_directory_an_empty_diff_a_big_untracked_binary_and_the_caps_refuse_the_start()
    {
        let dest = |n: &str| temp(n).join("target");
        let plain = temp("plain");
        let not_git = Target {
            repo: plain.clone(),
            ..diff_target(&plain, Head::Worktree)
        };
        let target = dest("plain");
        assert_eq!(capture(&not_git, &target, 0).unwrap_err(), NOT_GIT);
        assert!(!target.exists(), "a refused capture left its directory");
        assert_eq!(repo_root(&plain).unwrap_err(), NOT_GIT);

        let repo = repo();
        let same = Target {
            base: Some("feat".into()),
            ..diff_target(&repo, Head::Ref("feat".into()))
        };
        assert_eq!(
            capture(&same, &dest("empty"), 0).unwrap_err(),
            "差分がありません"
        );
        let missing = Target {
            base: Some("nope".into()),
            ..diff_target(&repo, Head::Worktree)
        };
        assert!(
            capture(&missing, &dest("base"), 0)
                .unwrap_err()
                .contains("比較元")
        );

        // An untracked binary over 1 MiB, named.
        let mut big = vec![0u8; MAX_UNTRACKED_BINARY_BYTES as usize + 1];
        big[1] = 0xff;
        fs::write(repo.join("model.bin"), &big).unwrap();
        let err = capture(&diff_target(&repo, Head::Worktree), &dest("bin"), 0).unwrap_err();
        assert!(
            err.contains("model.bin") && err.contains("バイナリ") && err.contains("1 MiB"),
            "{err}"
        );
        fs::remove_file(repo.join("model.bin")).unwrap();

        // Too many files.
        let many = repo.join("many");
        fs::create_dir_all(&many).unwrap();
        for i in 0..=MAX_TARGET_FILES {
            fs::write(many.join(format!("f{i:03}.txt")), "x\n").unwrap();
        }
        assert!(
            capture(&diff_target(&repo, Head::Worktree), &dest("many"), 0)
                .unwrap_err()
                .contains("多すぎます")
        );
        fs::remove_dir_all(&many).unwrap();

        // A diff over 4 MiB.
        let huge = repo.join("huge");
        fs::create_dir_all(&huge).unwrap();
        for i in 0..5 {
            fs::write(huge.join(format!("h{i}.txt")), "line\n".repeat(190_000)).unwrap();
        }
        assert!(
            capture(&diff_target(&repo, Head::Worktree), &dest("huge"), 0)
                .unwrap_err()
                .contains("差分が大きすぎます")
        );
        fs::remove_dir_all(&huge).unwrap();

        // Too many bytes: a one-line change to each of 25 files of just
        // under 1 MiB (a small diff, large post-images).
        sh(&repo, &["checkout", "-q", "-b", "bulk-base", "main"]);
        let chunk = "line\n".repeat((1024 * 1024 - 16) / 5);
        for i in 0..25 {
            fs::write(repo.join(format!("b{i}.txt")), &chunk).unwrap();
        }
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "bulk"]);
        sh(&repo, &["checkout", "-q", "-b", "bulk"]);
        for i in 0..25 {
            fs::write(repo.join(format!("b{i}.txt")), format!("x{chunk}")).unwrap();
        }
        sh(&repo, &["commit", "-q", "-am", "touch"]);
        let bulk = Target {
            base: Some("bulk-base".into()),
            ..diff_target(&repo, Head::Ref("bulk".into()))
        };
        assert_eq!(
            capture(&bulk, &dest("bytes"), 0).unwrap_err(),
            too_many_bytes()
        );
    }

    #[test]
    fn ac02_a_worktree_patch_over_16_mib_refuses_the_start() {
        let repo = repo();
        // An uncommitted change whose `git diff --binary HEAD` passes the
        // cap; checked before the 4 MiB diff cap is reached.
        let line = "a line of the work-tree change\n";
        let big = line.repeat(MAX_WORKTREE_PATCH_BYTES as usize / line.len() + 1);
        fs::write(repo.join("a.txt"), big).unwrap();
        let dest = temp("patch-cap").join("target");
        let err = capture(&diff_target(&repo, Head::Worktree), &dest, 0).unwrap_err();
        assert_eq!(err, "作業ツリーの変更が大きすぎます（上限 16 MiB）");
        assert!(!dest.exists(), "a refused capture left its directory");
    }

    #[test]
    fn ac01_a_file_that_became_a_link_and_a_link_that_became_a_file_are_read() {
        let repo = temp("typechange");
        sh(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("to-link.txt"), "was a file\n").unwrap();
        std::os::unix::fs::symlink("to-link.txt", repo.join("to-file.txt")).unwrap();
        fs::write(repo.join("z.txt"), "one\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "base"]);
        sh(&repo, &["checkout", "-q", "-b", "feat"]);
        fs::remove_file(repo.join("to-link.txt")).unwrap();
        std::os::unix::fs::symlink("z.txt", repo.join("to-link.txt")).unwrap();
        fs::remove_file(repo.join("to-file.txt")).unwrap();
        fs::write(repo.join("to-file.txt"), "now\na file\n").unwrap();
        fs::write(repo.join("z.txt"), "one\ntwo\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "types"]);
        // One raw entry each, two patch sections each.
        let raw = sh(&repo, &["diff", "--raw", "main", "feat"]);
        assert_eq!(raw.matches(" T\t").count(), 2, "{raw}");

        for head in [Head::Ref("feat".into()), Head::Worktree] {
            let dest = temp("typechange-dest").join("target");
            let c = capture(&diff_target(&repo, head.clone()), &dest, 0).unwrap();
            let m = &c.manifest;
            assert!(m.is_omitted("to-link.txt"), "{head:?}: {m:?}");
            let file = m.file("to-file.txt").unwrap();
            assert_eq!(
                (file.lines, file.additions, file.changed.clone()),
                (2, 2, vec![(1, 2)]),
                "{head:?}"
            );
            // The sections after them still go to the right files.
            let z = m.file("z.txt").unwrap();
            assert_eq!(
                (z.additions, z.deletions, z.changed.clone()),
                (1, 0, vec![(2, 2)])
            );
            verify(&dest, m, &c.rebuild, None).unwrap();
        }
    }

    #[test]
    fn verify_notices_a_tampered_patch_a_missing_untracked_file_and_a_missing_snapshot() {
        let repo = repo();
        dirty(&repo);
        let dest = temp("verify").join("target");
        let c = capture(&diff_target(&repo, Head::Worktree), &dest, 0).unwrap();
        verify(&dest, &c.manifest, &c.rebuild, None).unwrap();
        let patch = fs::read(dest.join(WORKTREE_PATCH)).unwrap();
        fs::write(dest.join(WORKTREE_PATCH), b"tampered").unwrap();
        assert!(verify(&dest, &c.manifest, &c.rebuild, None).is_err());
        fs::write(dest.join(WORKTREE_PATCH), &patch).unwrap();
        fs::remove_file(dest.join(UNTRACKED_DIR).join("blob.bin")).unwrap();
        assert!(verify(&dest, &c.manifest, &c.rebuild, None).is_err());
        fs::remove_dir_all(&dest).unwrap();
        assert!(
            verify(&dest, &c.manifest, &c.rebuild, None)
                .unwrap_err()
                .contains("ありません")
        );
    }

    #[test]
    fn default_base_prefers_main_then_master() {
        let repo = repo();
        assert_eq!(default_base(&repo).as_deref(), Some("main"));
        let other = temp("master");
        sh(&other, &["init", "-q", "-b", "master"]);
        fs::write(other.join("x"), "x").unwrap();
        sh(&other, &["add", "."]);
        sh(&other, &["commit", "-q", "-m", "x"]);
        assert_eq!(default_base(&other).as_deref(), Some("master"));
    }

    #[test]
    fn snapshot_paths_stay_inside_the_files_folder() {
        let dir = Path::new("/s");
        assert_eq!(
            snapshot_path(dir, "a/b.txt"),
            Some(PathBuf::from("/s/files/a/b.txt"))
        );
        assert_eq!(snapshot_path(dir, "../x"), None);
        assert_eq!(snapshot_path(dir, "/etc/passwd"), None);
    }
}
