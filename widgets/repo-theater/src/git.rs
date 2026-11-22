//! Local git inspection for live worktrees discovered via `noa-ipc`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{ChangedFile, DiffMode};

#[derive(Debug, Clone)]
pub struct WorktreeGitInfo {
    pub repo_name: String,
    pub repo_root: String,
    pub worktree_root: String,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<ChangedFile>,
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn canonical_or_raw(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub fn inspect_worktree(cwd: &str, mode: DiffMode) -> Option<WorktreeGitInfo> {
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return None;
    }
    let toplevel = run_git(dir, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_owned();
    if toplevel.is_empty() {
        return None;
    }
    let common_raw = run_git(dir, &["rev-parse", "--git-common-dir"])
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| ".git".into());
    let common_path = if Path::new(&common_raw).is_absolute() {
        PathBuf::from(&common_raw)
    } else {
        Path::new(&toplevel).join(&common_raw)
    };
    let repo_root = common_path
        .parent()
        .map(canonical_or_raw)
        .unwrap_or_else(|| canonical_or_raw(Path::new(&toplevel)));
    let repo_name = Path::new(&repo_root)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_owned();

    let status_out = run_git(dir, &["status", "--porcelain=v2", "--branch"]).unwrap_or_default();
    let PorcelainStatus {
        mut branch,
        ahead,
        behind,
        files: mut status_map,
    } = parse_porcelain_v2(&status_out);
    if branch.is_empty() {
        branch = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|| "HEAD".into());
    }

    let numstat_args: &[&str] = match mode {
        DiffMode::Unstaged => &["diff", "--numstat"],
        DiffMode::Staged => &["diff", "--cached", "--numstat"],
        DiffMode::VsMain => &["diff", "--numstat", "main...HEAD"],
    };
    let numstat_out = run_git(dir, numstat_args).unwrap_or_default();
    let mut stats = parse_numstat(&numstat_out);

    let mut files = Vec::new();
    match mode {
        DiffMode::VsMain => {
            for (path, (added, deleted)) in stats.drain() {
                files.push(ChangedFile {
                    path,
                    status: "M".into(),
                    added,
                    deleted,
                    diff: String::new(),
                    conflict_worktrees: Vec::new(),
                });
            }
        }
        DiffMode::Staged | DiffMode::Unstaged => {
            for (path, code) in status_map.drain(..) {
                let want = match mode {
                    DiffMode::Staged => code.0 != '.' && code.0 != '?',
                    DiffMode::Unstaged => code.1 != '.' || code.0 == '?',
                    DiffMode::VsMain => true,
                };
                if !want {
                    continue;
                }
                let (added, deleted) = stats.remove(&path).unwrap_or((0, 0));
                let status = if code.0 == '?' {
                    "?".to_owned()
                } else if mode == DiffMode::Staged {
                    code.0.to_string()
                } else {
                    code.1.to_string()
                };
                files.push(ChangedFile {
                    path,
                    status,
                    added,
                    deleted,
                    diff: String::new(),
                    conflict_worktrees: Vec::new(),
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Some(WorktreeGitInfo {
        repo_name,
        repo_root,
        worktree_root: canonical_or_raw(Path::new(&toplevel)),
        branch,
        ahead,
        behind,
        files,
    })
}

pub fn file_diff(cwd: &str, path: &str, mode: DiffMode) -> String {
    let dir = Path::new(cwd);
    let args: Vec<&str> = match mode {
        DiffMode::Unstaged => vec!["diff", "--", path],
        DiffMode::Staged => vec!["diff", "--cached", "--", path],
        DiffMode::VsMain => vec!["diff", "main...HEAD", "--", path],
    };
    let out = run_git(dir, &args).unwrap_or_default();
    if !out.trim().is_empty() {
        return out;
    }
    // Untracked file fallback: show contents as added lines.
    if mode == DiffMode::Unstaged {
        let full = dir.join(path);
        if let Ok(content) = std::fs::read_to_string(&full) {
            let mut buf = format!(
                "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
                content.lines().count()
            );
            for line in content.lines().take(500) {
                buf.push('+');
                buf.push_str(line);
                buf.push('\n');
            }
            return buf;
        }
    }
    format!("(差分なし: {path})")
}

struct PorcelainStatus {
    branch: String,
    ahead: usize,
    behind: usize,
    files: Vec<(String, (char, char))>,
}

fn parse_porcelain_v2(text: &str) -> PorcelainStatus {
    let mut branch = String::new();
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() >= 9 {
                let mut chars = parts[1].chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                let path_part = parts[8];
                let path = if line.starts_with("2 ") {
                    path_part.split('\t').next().unwrap_or(path_part).to_owned()
                } else {
                    path_part.to_owned()
                };
                files.push((path, (x, y)));
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            files.push((path.to_owned(), ('?', '?')));
        }
    }
    PorcelainStatus {
        branch,
        ahead,
        behind,
        files,
    }
}

fn parse_numstat(text: &str) -> std::collections::HashMap<String, (usize, usize)> {
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let Some(a) = parts.next() else { continue };
        let Some(d) = parts.next() else { continue };
        let Some(p) = parts.next() else { continue };
        let added = a.parse().unwrap_or(0);
        let deleted = d.parse().unwrap_or(0);
        map.insert(p.to_owned(), (added, deleted));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2_branch_and_files() {
        let sample = "\
# branch.oid 123456
# branch.head feat/test
# branch.ab +2 -1
1 .M N... 100644 100644 100644 a b src/main.rs
1 A. N... 000000 100644 100644 0 c src/new.rs
? notes.txt
";
        let parsed = parse_porcelain_v2(sample);
        assert_eq!(parsed.branch, "feat/test");
        assert_eq!(parsed.ahead, 2);
        assert_eq!(parsed.behind, 1);
        assert_eq!(parsed.files.len(), 3);
        assert_eq!(parsed.files[0], ("src/main.rs".into(), ('.', 'M')));
        assert_eq!(parsed.files[1], ("src/new.rs".into(), ('A', '.')));
        assert_eq!(parsed.files[2], ("notes.txt".into(), ('?', '?')));
    }
}
