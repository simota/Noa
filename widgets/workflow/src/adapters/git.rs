//! A run edits an owned worktree; verification gets separate copies of its
//! exact commit, tracked delta and untracked files.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::model::TargetSnapshot;

use super::fs;

const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Snapshot patches are machine input for `git apply` and evidence hashing.
/// Display preferences must not add ANSI escapes or change their path/hunk syntax.
fn diff(repo: &Path, args: &[&str], limit: usize) -> Result<Vec<u8>, String> {
    let mut options = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--binary",
        "--ignore-submodules=none",
        "--submodule=short",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--output-indicator-new=+",
        "--output-indicator-old=-",
        "--output-indicator-context= ",
    ];
    options.extend_from_slice(args);
    command_with_limit(repo, &options, limit)
}

pub fn command(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    command_with_limit(repo, args, MAX_SNAPSHOT_BYTES)
}

fn command_with_limit(repo: &Path, args: &[&str], limit: usize) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(repo)
        .args(args);
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    super::process::output_with_limit(&mut command, std::time::Duration::from_secs(30), limit)
        .map_err(|message| format!("git {}: {message}", args.first().copied().unwrap_or("")))
}

pub fn text(repo: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(command(repo, args)?)
        // Paths can end in whitespace, including a newline. Remove only the
        // record terminator emitted by Git, never bytes from the actual path.
        .map(|mut s| {
            if s.ends_with('\n') {
                s.pop();
            }
            s
        })
        .map_err(|_| "git の応答がUTF-8ではありません".into())
}

pub fn repository(path: &Path) -> Result<PathBuf, String> {
    let value = text(path, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(value).map_err(|e| e.to_string())
}

pub fn common_dir(path: &Path) -> Result<PathBuf, String> {
    let dir = PathBuf::from(text(path, &["rev-parse", "--git-common-dir"])?);
    std::fs::canonicalize(if dir.is_absolute() {
        dir
    } else {
        path.join(dir)
    })
    .map_err(|e| e.to_string())
}

pub fn head(repo: &Path) -> Result<String, String> {
    let sha = text(repo, &["rev-parse", "--verify", "HEAD"])?;
    if !(sha.len() == 40 || sha.len() == 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("HEADの識別子が不正です".into());
    }
    Ok(sha)
}

fn relative(name: &[u8]) -> Result<PathBuf, String> {
    let value = std::str::from_utf8(name)
        .map_err(|_| "UTF-8でないファイル名は現在のスナップショットに対応していません")?;
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("スナップショットに相対パスでないファイルがあります".into());
    }
    Ok(path)
}

struct Capture {
    patch: Vec<u8>,
    untracked: Vec<(PathBuf, Vec<u8>, u32)>,
    digest: String,
}

fn capture(repo: &Path) -> Result<Capture, String> {
    if !command(repo, &["ls-files", "-z", "--", ".noa/workflow"])?.is_empty() {
        return Err(".noa/workflow は成果物用の予約領域です。追跡済みファイルがあるため対象を省略せず停止しました".into());
    }
    let patch = diff(
        repo,
        &["HEAD", "--", ".", ":(exclude).noa/workflow/**"],
        MAX_SNAPSHOT_BYTES,
    )?;
    if patch.len() > MAX_SNAPSHOT_BYTES {
        return Err("変更スナップショットが64 MiBを超えています".into());
    }
    let mut bytes = patch.clone();
    let names = command(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut untracked = Vec::new();
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let path = relative(name)?;
        if path.starts_with(".noa/workflow") {
            continue;
        }
        let content = fs::read(&repo.join(&path), MAX_SNAPSHOT_BYTES as u64)?
            .ok_or_else(|| format!("スナップショット中に {} が消えました", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(repo.join(&path))
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o777;
        bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&mode.to_le_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&content);
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err("未追跡ファイルを含むスナップショットが64 MiBを超えています".into());
        }
        untracked.push((path, content, mode));
    }
    Ok(Capture {
        patch,
        untracked,
        digest: fs::hash(&bytes),
    })
}

pub fn snapshot(repo: &Path, base_sha: &str) -> Result<TargetSnapshot, String> {
    let before = head(repo)?;
    let capture = capture(repo)?;
    if before != head(repo)? {
        return Err("対象のHEADが読込中に変わりました。もう一度確認してください".into());
    }
    Ok(TargetSnapshot {
        base_sha: base_sha.to_owned(),
        head_sha: before,
        dirty_digest: capture.digest,
    })
}

pub fn same_snapshot(repo: &Path, target: &TargetSnapshot) -> Result<(), String> {
    if snapshot(repo, &target.base_sha)? != *target {
        return Err(
            "対象の変更版が実行時と異なります。旧版の承認・検証結果は使用できません".into(),
        );
    }
    Ok(())
}

pub fn is_clean(target: &TargetSnapshot) -> bool {
    target.dirty_digest == fs::hash(&[])
}

/// This evidence is the committed change between fixed object IDs. Local
/// uncommitted bytes remain bound by the separate target snapshot digest.
pub fn committed_diff(repo: &Path, target: &TargetSnapshot) -> Result<Vec<u8>, String> {
    diff(
        repo,
        &[&target.base_sha, &target.head_sha, "--"],
        8 * 1024 * 1024,
    )
}

/// This function never removes an existing worktree or resets its files.
/// The caller must have durably reserved a unique path before calling it.
pub fn prepare(
    source: &Path,
    destination: &Path,
    branch: Option<&str>,
    target: &TargetSnapshot,
) -> Result<(), String> {
    same_snapshot(source, target)?;
    if destination.exists() {
        if common_dir(source)? != common_dir(destination)? {
            return Err("保存済みworktreeが別のリポジトリを指しています".into());
        }
        return same_snapshot(destination, target);
    }
    let parent = destination.parent().ok_or("worktreeの保存先が不正です")?;
    fs::plain_dirs(parent)?;
    let delta = capture(source)?;
    if delta.digest != target.dirty_digest {
        return Err("worktreeを準備する間に対象が変わりました".into());
    }
    let path = destination
        .to_str()
        .ok_or("worktreeパスはUTF-8である必要があります")?;
    match branch {
        Some(branch) => {
            command(
                source,
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    "--",
                    path,
                    &target.head_sha,
                ],
            )?;
        }
        None => {
            command(
                source,
                &["worktree", "add", "--detach", "--", path, &target.head_sha],
            )?;
        }
    }
    if !delta.patch.is_empty() {
        let patch = parent.join(format!(
            ".{}.patch",
            destination
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("snapshot")
        ));
        fs::atomic_write(&patch, &delta.patch)?;
        let result = command(
            destination,
            &[
                "apply",
                "--binary",
                "--",
                patch.to_str().ok_or("patchパスが不正です")?,
            ],
        );
        let _ = std::fs::remove_file(patch);
        result?;
    }
    for (name, content, mode) in delta.untracked {
        let output = destination.join(name);
        if output.exists() {
            return Err("未追跡ファイルがworktree内の既存ファイルと競合しました".into());
        }
        fs::atomic_write(&output, &content)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    same_snapshot(source, target)?;
    same_snapshot(destination, target)
}

pub fn branch(repo: &Path) -> Result<String, String> {
    let value = text(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if !value.starts_with("noa-workflow/") {
        return Err("Workflowが作成したブランチだけを公開できます".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untracked_paths_cannot_escape_the_snapshot() {
        for name in [b"../secret".as_slice(), b"/secret", b"a/../b"] {
            assert!(relative(name).is_err());
        }
        assert_eq!(
            relative(b"src/main.rs").unwrap(),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn repository_paths_keep_trailing_whitespace_instead_of_selecting_a_sibling() {
        let directory = tempfile::tempdir().unwrap();
        let directory = std::fs::canonicalize(directory.path()).unwrap();
        for suffix in [" ", "\n"] {
            let source = directory.join(format!("source{suffix}"));
            let sibling = directory.join("source");
            let metadata = if suffix == "\n" {
                source.join(".git")
            } else {
                directory.join(format!("metadata{suffix}"))
            };
            std::fs::create_dir(&source).unwrap();
            std::fs::create_dir_all(&sibling).unwrap();
            command(&sibling, &["init", "-q"]).unwrap();
            if suffix == "\n" {
                command(&source, &["init", "-q"]).unwrap();
            } else {
                command(
                    &source,
                    &[
                        "init",
                        "-q",
                        "--separate-git-dir",
                        metadata.to_str().unwrap(),
                    ],
                )
                .unwrap();
            }

            assert_eq!(repository(&source).unwrap(), source);
            assert_eq!(common_dir(&source).unwrap(), metadata);
        }
    }

    #[test]
    fn display_preferences_cannot_change_snapshot_patches_or_committed_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let directory = std::fs::canonicalize(directory.path()).unwrap();
        let source = directory.join("source");
        std::fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]).unwrap();
        command(&source, &["config", "user.name", "Workflow Test"]).unwrap();
        command(
            &source,
            &["config", "user.email", "workflow@example.invalid"],
        )
        .unwrap();
        std::fs::write(source.join("tracked.txt"), "before\ncontext\n").unwrap();
        command(&source, &["add", "tracked.txt"]).unwrap();
        command(&source, &["commit", "-qm", "initial"]).unwrap();
        let base = head(&source).unwrap();
        std::fs::write(source.join("tracked.txt"), "committed\ncontext\n").unwrap();
        command(&source, &["commit", "-qam", "change"]).unwrap();
        std::fs::write(source.join("tracked.txt"), "uncommitted\ncontext\n").unwrap();
        let target = snapshot(&source, &base).unwrap();
        let evidence = committed_diff(&source, &target).unwrap();

        for (key, value) in [
            ("color.ui", "always"),
            ("diff.noprefix", "true"),
            ("diff.mnemonicprefix", "true"),
            ("diff.srcPrefix", "custom/old/"),
            ("diff.dstPrefix", "custom/new/"),
            ("diff.outputIndicatorNew", ">"),
            ("diff.outputIndicatorOld", "<"),
            ("diff.outputIndicatorContext", "_"),
        ] {
            command(&source, &["config", key, value]).unwrap();
        }
        assert_eq!(snapshot(&source, &base).unwrap(), target);
        assert_eq!(committed_diff(&source, &target).unwrap(), evidence);
        assert!(!evidence.contains(&0x1b));

        let isolated = directory.join("isolated");
        prepare(&source, &isolated, None, &target).unwrap();
        assert_eq!(snapshot(&isolated, &base).unwrap(), target);
        assert_eq!(
            std::fs::read(isolated.join("tracked.txt")).unwrap(),
            b"uncommitted\ncontext\n"
        );
    }

    #[test]
    fn submodule_preferences_cannot_hide_target_changes_or_rewrite_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let source = std::fs::canonicalize(directory.path()).unwrap();
        let submodule = source.join("submodule");
        std::fs::create_dir(&submodule).unwrap();
        for repository in [&source, &submodule] {
            command(repository, &["init", "-q"]).unwrap();
            command(repository, &["config", "user.name", "Workflow Test"]).unwrap();
            command(
                repository,
                &["config", "user.email", "workflow@example.invalid"],
            )
            .unwrap();
        }
        std::fs::write(submodule.join("tracked.txt"), "before\n").unwrap();
        command(&submodule, &["add", "tracked.txt"]).unwrap();
        command(&submodule, &["commit", "-qm", "initial"]).unwrap();
        command(&source, &["add", "submodule"]).unwrap();
        command(&source, &["commit", "-qm", "add gitlink"]).unwrap();
        let base = head(&source).unwrap();
        let clean = snapshot(&source, &base).unwrap();
        command(&source, &["config", "diff.ignoreSubmodules", "all"]).unwrap();
        command(&source, &["config", "diff.submodule", "log"]).unwrap();

        std::fs::write(submodule.join("tracked.txt"), "changed\n").unwrap();
        let dirty = snapshot(&source, &base).unwrap();
        assert_ne!(dirty, clean);
        assert!(!is_clean(&dirty));
        command(&submodule, &["commit", "-qam", "change"]).unwrap();
        let updated = snapshot(&source, &base).unwrap();
        assert_ne!(updated, dirty);
        assert!(!is_clean(&updated));
        command(&source, &["add", "submodule"]).unwrap();
        command(&source, &["commit", "-qm", "update gitlink"]).unwrap();
        let target = snapshot(&source, &base).unwrap();
        let evidence = committed_diff(&source, &target).unwrap();
        let evidence = String::from_utf8(evidence).unwrap();
        assert!(evidence.starts_with("diff --git a/submodule b/submodule\n"));
        assert!(evidence.contains(&format!("+Subproject commit {}", head(&submodule).unwrap())));
    }

    #[test]
    fn isolated_snapshot_keeps_dirty_and_untracked_bytes_without_touching_source() {
        let directory = tempfile::tempdir().unwrap();
        let directory = std::fs::canonicalize(directory.path()).unwrap();
        let source = directory.join("source");
        std::fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]).unwrap();
        command(&source, &["config", "user.name", "Workflow Test"]).unwrap();
        command(
            &source,
            &["config", "user.email", "workflow@example.invalid"],
        )
        .unwrap();
        std::fs::write(source.join("tracked.txt"), "initial\n").unwrap();
        command(&source, &["add", "tracked.txt"]).unwrap();
        command(&source, &["commit", "-qm", "initial"]).unwrap();
        let base = head(&source).unwrap();
        std::fs::write(source.join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(source.join("untracked.txt"), "keep me\n").unwrap();
        std::fs::create_dir(source.join(".noa")).unwrap();
        std::fs::write(source.join(".noa/project-config.json"), "{\"keep\":true}\n").unwrap();
        // Canonical snapshots must not invoke configured diff transforms.
        command(&source, &["config", "diff.external", "/bin/false"]).unwrap();
        let target = snapshot(&source, &base).unwrap();
        let isolated = directory.join("isolated");
        prepare(&source, &isolated, Some("noa-workflow/test"), &target).unwrap();
        assert_eq!(snapshot(&isolated, &base).unwrap(), target);
        assert_eq!(
            std::fs::read_to_string(isolated.join("untracked.txt")).unwrap(),
            "keep me\n"
        );
        assert_eq!(
            std::fs::read_to_string(isolated.join(".noa/project-config.json")).unwrap(),
            "{\"keep\":true}\n"
        );
        std::fs::write(isolated.join("tracked.txt"), "isolated edit\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("tracked.txt")).unwrap(),
            "changed\n"
        );
        assert!(same_snapshot(&isolated, &target).is_err());
        assert!(prepare(&source, &isolated, None, &target).is_err());
        std::fs::create_dir_all(source.join(".noa/workflow")).unwrap();
        std::fs::write(source.join(".noa/workflow/project-owned.txt"), "tracked\n").unwrap();
        command(&source, &["add", ".noa/workflow/project-owned.txt"]).unwrap();
        assert!(snapshot(&source, &base).is_err());
    }
}
