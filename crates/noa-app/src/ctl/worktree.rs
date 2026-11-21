//! `Noa +ctl spawn --worktree`, `spawn --tier` and `close --prune`: the git
//! and model-tier logic, kept apart from `ctl.rs`. Naming, tier flags and
//! porcelain parsing are pure; the functions that run `git` are thin shells
//! around them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How much model a worker gets (`spawn --tier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    Light,
    Standard,
    Deep,
}

impl Tier {
    pub(crate) fn parse(name: &str) -> Result<Self, String> {
        match name {
            "light" => Ok(Self::Light),
            "standard" => Ok(Self::Standard),
            "deep" => Ok(Self::Deep),
            other => Err(format!(
                "unknown --tier {other:?}: use light, standard or deep"
            )),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agent {
    Claude,
    Codex,
    Agy,
}

const TIER_AGENTS: &str = "claude, codex or agy";

fn agent_of(program: &str) -> Option<Agent> {
    match Path::new(program).file_name()?.to_str()? {
        "claude" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "agy" => Some(Agent::Agy),
        _ => None,
    }
}

/// The flags that put `agent` on `tier`.
fn tier_flags(agent: Agent, tier: Tier) -> &'static [&'static str] {
    match (agent, tier) {
        (Agent::Claude, Tier::Light) => &["--model", "sonnet", "--effort", "low"],
        (Agent::Claude, Tier::Standard) => &["--model", "sonnet", "--effort", "medium"],
        (Agent::Claude, Tier::Deep) => &["--model", "opus", "--effort", "high"],
        (Agent::Codex, Tier::Light) => &["-m", "gpt-6-luna", "-c", "model_reasoning_effort=low"],
        (Agent::Codex, Tier::Standard) => {
            &["-m", "gpt-6.2-sol", "-c", "model_reasoning_effort=medium"]
        }
        (Agent::Codex, Tier::Deep) => &["-m", "gpt-6.2-astra", "-c", "model_reasoning_effort=high"],
        (Agent::Agy, Tier::Light) => &["--model", "gemini-4-hydrogen-low"],
        (Agent::Agy, Tier::Standard) => &["--model", "gemini-4-argon-medium"],
        (Agent::Agy, Tier::Deep) => &["--model", "gemini-4-neon-high"],
    }
}

/// Whether `args` already choose a model or effort for `agent`.
fn picks_model(agent: Agent, args: &[String]) -> bool {
    let long = |arg: &str, name: &str| arg == name || arg.starts_with(&format!("{name}="));
    let codex_config =
        |value: &str| value.starts_with("model_reasoning_effort=") || value.starts_with("model=");
    match agent {
        Agent::Claude => args
            .iter()
            .any(|arg| long(arg, "--model") || long(arg, "--effort")),
        Agent::Agy => args.iter().any(|arg| long(arg, "--model")),
        Agent::Codex => {
            let mut iter = args.iter();
            while let Some(arg) = iter.next() {
                // `-m X`, or `-mX` joined.
                if arg.starts_with("-m") || long(arg, "--model") {
                    return true;
                }
                if arg == "-c" || arg == "--config" {
                    if iter.next().is_some_and(|value| codex_config(value)) {
                        return true;
                    }
                } else if let Some(value) = arg
                    .strip_prefix("--config=")
                    .or_else(|| arg.strip_prefix("-c"))
                    && codex_config(value)
                {
                    return true;
                }
            }
            false
        }
    }
}

/// Whether `command` runs claude, codex or agy (the programs `--tier` knows).
pub(crate) fn is_agent_command(command: &[String]) -> bool {
    command
        .first()
        .is_some_and(|program| agent_of(program).is_some())
}

/// Argument check for `--tier`: `command` must be one of the three agents
/// and must not already pick a model.
pub(crate) fn check_tier_command(command: &[String]) -> Result<(), String> {
    let Some((program, rest)) = command.split_first() else {
        return Err(format!("--tier needs a COMMAND after `--`: {TIER_AGENTS}"));
    };
    let Some(agent) = agent_of(program) else {
        return Err(format!(
            "--tier only applies to {TIER_AGENTS}, not {program:?}"
        ));
    };
    if picks_model(agent, rest) {
        return Err(format!(
            "--tier conflicts with the model or effort already chosen in the command \
             ({program}); drop one of them"
        ));
    }
    Ok(())
}

/// `command` with the tier's flags right after the program.
pub(crate) fn apply_tier(command: &[String], tier: Tier) -> Result<Vec<String>, String> {
    check_tier_command(command)?;
    let agent = agent_of(&command[0]).expect("checked above");
    let mut out = Vec::with_capacity(command.len() + 4);
    out.push(command[0].clone());
    out.extend(tier_flags(agent, tier).iter().map(|flag| flag.to_string()));
    out.extend(command[1..].iter().cloned());
    Ok(out)
}

/// A `--worktree` label becomes a directory and a branch name.
pub(crate) fn check_label(label: &str) -> Result<(), String> {
    let ok = !label.is_empty()
        && !label.starts_with(['.', '-'])
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "--worktree needs a --label matching [A-Za-z0-9._-]+ that does not start \
             with '.' or '-', got {label:?}"
        ))
    }
}

/// `<top>-wt/<label>`.
pub(crate) fn worktree_path(top: &Path, label: &str) -> PathBuf {
    let mut dir: OsString = top.as_os_str().to_owned();
    dir.push("-wt");
    PathBuf::from(dir).join(label)
}

/// `orch/<label>`.
pub(crate) fn branch_name(label: &str) -> String {
    format!("orch/{label}")
}

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeEntry {
    pub(crate) path: PathBuf,
    /// The full ref (`refs/heads/...`); `None` for a detached HEAD.
    pub(crate) branch: Option<String>,
}

pub(crate) fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    for block in porcelain.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("branch ") {
                branch = Some(value.to_string());
            }
        }
        if let Some(path) = path {
            entries.push(WorktreeEntry { path, branch });
        }
    }
    entries
}

/// The worktree `spawn --worktree` made for `label`, if it is still there.
pub(crate) fn find_worktree<'a>(
    entries: &'a [WorktreeEntry],
    top: &Path,
    label: &str,
) -> Option<&'a WorktreeEntry> {
    let path = worktree_path(top, label);
    let branch = format!("refs/heads/{}", branch_name(label));
    entries
        .iter()
        .find(|entry| entry.path == path && entry.branch.as_deref() == Some(branch.as_str()))
}

// ---------------------------------------------------------------------------
// Running git

/// Run `git args` in `cwd`; `Err` carries git's stderr (or why it did not run).
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| format!("cannot run git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git {} failed ({})", args.join(" "), output.status)
        } else {
            stderr
        })
    }
}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or("").to_string()
}

/// The repository's main worktree, seen from `cwd`: the first entry of
/// `git worktree list`, the same from any of its worktrees or subdirectories.
pub(crate) fn main_worktree(cwd: &Path) -> Result<PathBuf, String> {
    let entries = parse_worktree_list(&git(cwd, &["worktree", "list", "--porcelain"])?);
    entries
        .into_iter()
        .next()
        .map(|entry| entry.path)
        .ok_or_else(|| "git worktree list printed no worktree".to_string())
}

/// A worktree made for a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Made {
    pub(crate) path: PathBuf,
    pub(crate) branch: String,
}

/// `git worktree add -b orch/<label> <main>-wt/<label> HEAD`, run in `cwd`
/// (so the branch starts at `cwd`'s HEAD), where `<main>` is the main
/// worktree: run from another worktree, the new one still goes next to the
/// repository, not inside that worktree's `-wt`. The `Err` is git's stderr.
/// Warns on stderr when `cwd`'s checkout has changes the worktree does not
/// have.
pub(crate) fn add(cwd: &Path, label: &str) -> Result<Made, String> {
    let top = main_worktree(cwd)?;
    let path = worktree_path(&top, label);
    let branch = branch_name(label);
    let path_arg = path.to_string_lossy().into_owned();
    git(cwd, &["worktree", "add", "-b", &branch, &path_arg, "HEAD"])?;
    if let Ok(status) = git(cwd, &["status", "--porcelain", "--untracked-files=all"]) {
        for warning in uncommitted_warnings(&status) {
            eprintln!("Noa +ctl: warning: {warning}");
        }
    }
    Ok(Made { path, branch })
}

/// What `git status --porcelain` says the new worktree lacks: changes to
/// tracked files (which the agent may need committed first) and untracked
/// files (often the orchestrator's own scratch files), each said apart.
pub(crate) fn uncommitted_warnings(porcelain: &str) -> Vec<String> {
    let (mut tracked, mut untracked) = (0, 0);
    for line in porcelain.lines().filter(|line| !line.is_empty()) {
        if line.starts_with("??") {
            untracked += 1;
        } else {
            tracked += 1;
        }
    }
    let count = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut warnings = Vec::new();
    if tracked > 0 {
        warnings.push(format!(
            "this checkout has uncommitted changes to {}; they are not in the worktree",
            count(tracked, "tracked file", "tracked files")
        ));
    }
    if untracked > 0 {
        warnings.push(format!(
            "{} not in the worktree",
            count(untracked, "untracked file is", "untracked files are")
        ));
    }
    warnings
}

/// What a removal did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removal {
    pub(crate) removed: bool,
    pub(crate) branch_deleted: bool,
    /// git's first stderr line, when it refused something.
    pub(crate) kept: Option<String>,
}

/// `git worktree remove <path>` then `git branch -d <branch>`; never forced,
/// and the branch is only touched once the worktree is gone.
pub(crate) fn remove(cwd: &Path, path: &Path, branch: &str) -> Removal {
    let path_arg = path.to_string_lossy().into_owned();
    if let Err(message) = git(cwd, &["worktree", "remove", &path_arg]) {
        return Removal {
            removed: false,
            branch_deleted: false,
            kept: Some(first_line(&message)),
        };
    }
    match git(cwd, &["branch", "-d", branch]) {
        Ok(_) => Removal {
            removed: true,
            branch_deleted: true,
            kept: None,
        },
        Err(message) => Removal {
            removed: true,
            branch_deleted: false,
            kept: Some(first_line(&message)),
        },
    }
}

/// `close --prune` for one closed pane labelled `label`: remove its worktree
/// `<main>-wt/<label>` and branch `orch/<label>` when the pane's own cwd
/// (`pane_cwd`) is in that worktree. The repository is the pane's, found
/// from `pane_cwd`, not the caller's cwd, and git runs in its main worktree.
/// `None` only for a label `spawn --worktree` could not have given; for any
/// other the result says what was done, or why nothing was.
///
/// The worktree's result files are first moved into `<main>/.noa/`
/// ([`super::result::move_out`]); `"result"` is where the pane's own
/// (`result`, its path in the worktree) went, and `"moved"` lists every
/// file moved (`from`, `to`), with the `paneId` of the other worker in
/// `others` (pane, known result path) whose file it was. When a file cannot
/// be moved, the worktree is kept and `"kept"` says why.
pub(crate) fn prune(
    label: &str,
    pane_cwd: &Path,
    result: Option<&Path>,
    others: &[(u64, PathBuf)],
) -> Option<serde_json::Value> {
    check_label(label).ok()?;
    let branch = branch_name(label);
    let kept = |reason: String| {
        serde_json::json!({
            "removed": false,
            "branchDeleted": false,
            "kept": reason,
        })
    };
    // The listed cwd can be a directory the worker has since removed.
    let from = pane_cwd
        .ancestors()
        .find(|dir| dir.is_dir())
        .unwrap_or(pane_cwd);
    let entries = match git(from, &["worktree", "list", "--porcelain"]) {
        Ok(porcelain) => parse_worktree_list(&porcelain),
        Err(message) => {
            return Some(kept(format!(
                "no {branch} worktree found from the pane's cwd {}: {}",
                pane_cwd.display(),
                first_line(&message)
            )));
        }
    };
    let main = entries.first()?.path.clone();
    let Some(entry) = find_worktree(&entries, &main, label) else {
        return Some(kept(format!(
            "no worktree {} on {branch} in this repository",
            worktree_path(&main, label).display()
        )));
    };
    // Labels are not unique: only the pane that worked in the worktree
    // prunes it, not another pane that happens to share the label.
    if !within(pane_cwd, &entry.path) {
        return Some(serde_json::json!({
            "path": entry.path.to_string_lossy(),
            "removed": false,
            "branchDeleted": false,
            "kept": NOT_IN_WORKTREE,
        }));
    }
    // A worker's file, by name, when it is in this worktree's `.noa/`
    // (compared resolved, before the move takes it away).
    let noa_dir = entry.path.join(".noa").canonicalize().ok();
    let name_here = |result: &Path| {
        let dir = result.parent()?;
        (dir.canonicalize().ok()? == *noa_dir.as_ref()?)
            .then(|| result.file_name())
            .flatten()
            .map(std::ffi::OsStr::to_os_string)
    };
    let own_name = result.and_then(name_here);
    let other_names: Vec<(u64, std::ffi::OsString)> = others
        .iter()
        .filter_map(|(pane, path)| Some((*pane, name_here(path)?)))
        .collect();
    let mut moved = Vec::new();
    let not_moved = super::result::move_out(&entry.path, &main, &mut moved).err();
    let own_result = own_name.and_then(|name| {
        moved
            .iter()
            .find(|(from, _)| from.file_name() == Some(name.as_os_str()))
            .map(|(_, to)| to.to_string_lossy().into_owned())
    });
    let moves: Vec<serde_json::Value> = moved
        .iter()
        .map(|(from, to)| {
            let mut line = serde_json::json!({
                "from": from.to_string_lossy(),
                "to": to.to_string_lossy(),
            });
            if let Some((pane, _)) = other_names
                .iter()
                .find(|(_, name)| from.file_name() == Some(name.as_os_str()))
            {
                line["paneId"] = serde_json::json!(noa_ipc::WireId(*pane));
            }
            line
        })
        .collect();
    let removal = match not_moved {
        Some(message) => Removal {
            removed: false,
            branch_deleted: false,
            kept: Some(message),
        },
        None => remove(&main, &entry.path, &branch),
    };
    let mut info = serde_json::json!({
        "path": entry.path.to_string_lossy(),
        "removed": removal.removed,
        "branchDeleted": removal.branch_deleted,
    });
    if let Some(kept) = removal.kept {
        info["kept"] = kept.into();
    }
    if let Some(result) = own_result {
        info["result"] = result.into();
    }
    if !moves.is_empty() {
        info["moved"] = moves.into();
    }
    Some(info)
}

/// `status`: how many commits `branch` has that `here`'s HEAD lacks (`git
/// rev-list --count HEAD..branch`); `None` when git cannot say (`here` is
/// outside the repository, or the branch is gone).
pub(crate) fn ahead(here: &Path, branch: &str) -> Option<u64> {
    let range = format!("HEAD..{branch}");
    git(here, &["rev-list", "--count", &range, "--"])
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// `status`: whether the checkout at `dir` has changes, untracked files
/// included; `None` when git cannot say.
pub(crate) fn dirty(dir: &Path) -> Option<bool> {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .ok()
        .map(|status| !status.trim().is_empty())
}

/// Why `close --prune` kept the worktree of a pane's label: the pane did not
/// work in it.
pub(crate) const NOT_IN_WORKTREE: &str = "the pane's cwd is not in this worktree";

/// Whether `path` is `dir` or inside it, compared resolved when they exist
/// (`/var` and `/private/var` are one directory on macOS).
fn within(path: &Path, dir: &Path) -> bool {
    let resolve = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    resolve(path).starts_with(resolve(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn labels_are_plain_names() {
        for good in ["a", "fix-1", "v1.2_x", "A.b"] {
            assert!(check_label(good).is_ok(), "{good}");
        }
        for bad in ["", ".hidden", "-x", "a/b", "a b", "ü", "a:b"] {
            assert!(check_label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn path_and_branch_are_named_after_the_label() {
        assert_eq!(
            worktree_path(Path::new("/src/noa"), "ctl"),
            PathBuf::from("/src/noa-wt/ctl")
        );
        assert_eq!(branch_name("ctl"), "orch/ctl");
    }

    #[test]
    fn tiers_parse_by_name() {
        assert_eq!(Tier::parse("deep"), Ok(Tier::Deep));
        assert_eq!(Tier::Standard.name(), "standard");
        assert!(Tier::parse("max").is_err());
    }

    #[test]
    fn tier_flags_go_right_after_the_program() {
        let cases = [
            (
                "claude",
                Tier::Light,
                &["--model", "sonnet", "--effort", "low"][..],
            ),
            (
                "/usr/bin/claude",
                Tier::Standard,
                &["--model", "sonnet", "--effort", "medium"],
            ),
            (
                "claude",
                Tier::Deep,
                &["--model", "opus", "--effort", "high"],
            ),
            (
                "codex",
                Tier::Light,
                &["-m", "gpt-6-luna", "-c", "model_reasoning_effort=low"],
            ),
            (
                "codex",
                Tier::Standard,
                &["-m", "gpt-6.2-sol", "-c", "model_reasoning_effort=medium"],
            ),
            (
                "codex",
                Tier::Deep,
                &["-m", "gpt-6.2-astra", "-c", "model_reasoning_effort=high"],
            ),
            ("agy", Tier::Light, &["--model", "gemini-4-hydrogen-low"]),
            ("agy", Tier::Standard, &["--model", "gemini-4-argon-medium"]),
            ("agy", Tier::Deep, &["--model", "gemini-4-neon-high"]),
        ];
        for (program, tier, flags) in cases {
            let command = strings(&[program, "--flag", "task"]);
            let got = apply_tier(&command, tier).unwrap();
            let mut want = vec![program.to_string()];
            want.extend(flags.iter().map(|f| f.to_string()));
            want.extend(strings(&["--flag", "task"]));
            assert_eq!(got, want, "{program} {tier:?}");
        }
    }

    #[test]
    fn tier_needs_one_of_the_three_agents() {
        let none = check_tier_command(&[]).unwrap_err();
        assert!(none.contains("claude, codex or agy"), "{none}");
        let other = check_tier_command(&strings(&["vim"])).unwrap_err();
        assert!(other.contains("claude, codex or agy"), "{other}");
    }

    #[test]
    fn tier_refuses_a_command_that_already_picks_a_model() {
        for command in [
            &["claude", "--model", "opus"][..],
            &["claude", "--model=opus"],
            &["claude", "--effort", "low"],
            &["codex", "-m", "x"],
            // Review finding: the joined short form was let through, and
            // Codex then exits on the model given twice.
            &["codex", "-mgpt-6.2-sol"],
            &["codex", "--model", "x"],
            &["codex", "-c", "model_reasoning_effort=low"],
            &["codex", "-cmodel_reasoning_effort=low"],
            &["codex", "--config=model_reasoning_effort=low"],
            &["agy", "--model", "x"],
        ] {
            assert!(
                check_tier_command(&strings(command)).is_err(),
                "{command:?} must be refused"
            );
        }
        // Unrelated config and other agents' flags are fine.
        for command in [
            &["codex", "-c", "sandbox_mode=read-only"][..],
            &["codex", "exec", "task"],
            &["agy", "--effort", "low"],
        ] {
            assert!(
                check_tier_command(&strings(command)).is_ok(),
                "{command:?} must be accepted"
            );
        }
    }

    #[test]
    fn porcelain_lists_each_worktree_with_its_branch() {
        let porcelain = "\
worktree /src/noa
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /src/noa-wt/ctl
HEAD 2222222222222222222222222222222222222222
branch refs/heads/orch/ctl

worktree /src/noa-wt/loose
HEAD 3333333333333333333333333333333333333333
detached
";
        let entries = parse_worktree_list(porcelain);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(entries[2].branch, None);

        let top = Path::new("/src/noa");
        assert_eq!(
            find_worktree(&entries, top, "ctl").map(|e| e.path.clone()),
            Some(PathBuf::from("/src/noa-wt/ctl"))
        );
        // Detached, or on another branch, is not ours.
        assert!(find_worktree(&entries, top, "loose").is_none());
        assert!(find_worktree(&entries, top, "missing").is_none());
    }

    // --- real git ---------------------------------------------------------

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repo with one commit at `<tmp>/repo`; the temp dir is canonical so
    /// it compares equal to what git prints.
    fn repo(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("noa-wt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let base = {
            std::fs::create_dir_all(&base).unwrap();
            base.canonicalize().unwrap()
        };
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        (base, repo)
    }

    #[test]
    fn add_then_prune_removes_a_clean_worktree_and_its_branch() {
        let (base, repo) = repo("clean");
        let made = add(&repo, "job").unwrap();
        assert_eq!(made.path, base.join("repo-wt").join("job"));
        assert_eq!(made.branch, "orch/job");
        assert!(made.path.join("a.txt").is_file());

        let info = prune("job", &made.path, None, &[]).unwrap();
        assert_eq!(info["removed"], true);
        assert_eq!(info["branchDeleted"], true);
        assert!(info.get("kept").is_none());
        assert!(!made.path.exists());
        assert!(
            run_git(&repo, &["branch", "--list", "orch/job"])
                .trim()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // Review finding #2: `close --prune` removed the worker's report with
    // its worktree, so `result PANE` found nothing.
    #[test]
    fn prune_moves_the_result_files_into_the_main_worktree_first() {
        let (base, repo) = repo("result");
        let made = add(&repo, "job").unwrap();
        let own = super::super::result::prepare(&made.path, Some("job")).unwrap();
        std::fs::write(&own, "report\n").unwrap();
        let other = made.path.join(".noa").join("result-0000abcd.md");
        std::fs::write(&other, "other\n").unwrap();
        // The main worktree already has a file of that name: not overwritten.
        let taken = super::super::result::prepare(&repo, Some("job")).unwrap();
        std::fs::write(&taken, "main's own\n").unwrap();

        let info = prune("job", &made.path, Some(&own), &[]).unwrap();
        assert_eq!(info["removed"], true, "{info}");
        assert!(!made.path.exists());
        let moved = PathBuf::from(info["result"].as_str().unwrap());
        assert_eq!(moved.parent(), Some(repo.join(".noa").as_path()));
        assert_ne!(moved, taken);
        let name = moved.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("result-job-") && name.ends_with(".md"),
            "{name}"
        );
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "report\n");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "main's own\n");
        let kept = repo.join(".noa").join("result-0000abcd.md");
        assert_eq!(std::fs::read_to_string(kept).unwrap(), "other\n");
        // Still ignored in the main worktree.
        assert!(run_git(&repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    // Review finding: B worked in A's worktree (`--cwd`); `close --prune A`
    // moved B's report too, but only A's new path was returned and
    // journaled, so `result B` read a path in the removed worktree.
    #[test]
    fn prune_journals_where_every_workers_result_went() {
        let (base, repo) = repo("shared");
        let made = add(&repo, "job").unwrap();
        let own = super::super::result::prepare(&made.path, Some("job")).unwrap();
        std::fs::write(&own, "a's report\n").unwrap();
        let b = super::super::result::prepare(&made.path, Some("b")).unwrap();
        std::fs::write(&b, "b's report\n").unwrap();

        let info = prune("job", &made.path, Some(&own), &[(9, b.clone())]).unwrap();
        assert_eq!(info["removed"], true, "{info}");
        assert_eq!(info["moved"].as_array().map(Vec::len), Some(2), "{info}");
        let events: Vec<serde_json::Value> = vec![
            serde_json::json!({"ev":"spawn","pane":4,"label":"job","result":own.to_string_lossy()}),
            serde_json::json!({"ev":"spawn","pane":9,"label":"b","result":b.to_string_lossy()}),
            serde_json::json!({"ev":"close","pane":9,"worktree":null}),
            serde_json::json!({"ev":"close","pane":4,"worktree":info}),
        ];
        let workers = super::super::journal::workers(&events);
        for (worker, text) in workers.iter().zip(["a's report\n", "b's report\n"]) {
            let path = worker.result.as_deref().expect("a result path");
            assert_eq!(
                std::fs::read_to_string(path).ok().as_deref(),
                Some(text),
                "pane {}: {path}",
                worker.pane
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prune_keeps_the_worktree_when_its_result_cannot_be_moved() {
        let (base, repo) = repo("unmovable");
        let made = add(&repo, "job").unwrap();
        let own = super::super::result::prepare(&made.path, Some("job")).unwrap();
        std::fs::write(&own, "report\n").unwrap();
        // `<main>/.noa` is a file, so nothing can be moved there.
        std::fs::write(repo.join(".noa"), "").unwrap();
        run_git(&repo, &["add", ".noa"]);
        run_git(&repo, &["commit", "-q", "-m", "noa file"]);

        let info = prune("job", &made.path, Some(&own), &[]).unwrap();
        assert_eq!(info["removed"], false);
        assert_eq!(info["branchDeleted"], false);
        let kept = info["kept"].as_str().unwrap();
        assert!(kept.contains(".noa"), "{kept}");
        assert!(info.get("result").is_none());
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "report\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn add_fails_with_gits_message_and_leaves_nothing() {
        let (base, repo) = repo("dup");
        add(&repo, "job").unwrap();
        let err = add(&repo, "job").unwrap_err();
        assert!(err.contains("orch/job") || err.contains("already"), "{err}");

        let outside = base.join("plain");
        std::fs::create_dir_all(&outside).unwrap();
        // Not a repo (an enclosing repo under the temp dir would be a bug here).
        if main_worktree(&outside).is_err() {
            assert!(add(&outside, "x").is_err());
            assert!(!base.join("plain-wt").exists());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // Audit finding (AU-04): `status` says how far a worktree worker got.
    #[test]
    fn ahead_counts_the_workers_commits_and_dirty_its_changes() {
        let (base, repo) = repo("ahead");
        let made = add(&repo, "job").unwrap();
        assert_eq!(ahead(&repo, "orch/job"), Some(0));
        assert_eq!(dirty(&made.path), Some(false));
        std::fs::write(made.path.join("b.txt"), "b\n").unwrap();
        assert_eq!(dirty(&made.path), Some(true));
        run_git(&made.path, &["add", "."]);
        run_git(&made.path, &["commit", "-q", "-m", "b"]);
        assert_eq!(dirty(&made.path), Some(false));
        assert_eq!(ahead(&repo, "orch/job"), Some(1));
        assert_eq!(ahead(&repo, "orch/missing"), None);
        assert_eq!(ahead(&base, "orch/job"), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prune_keeps_a_worktree_with_uncommitted_changes() {
        let (base, repo) = repo("dirty");
        let made = add(&repo, "job").unwrap();
        std::fs::write(made.path.join("new.txt"), "x").unwrap();

        let info = prune("job", &made.path, None, &[]).unwrap();
        assert_eq!(info["removed"], false);
        assert_eq!(info["branchDeleted"], false);
        assert!(info["kept"].as_str().is_some_and(|k| !k.is_empty()));
        assert!(made.path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prune_removes_the_worktree_but_keeps_an_unmerged_branch() {
        let (base, repo) = repo("unmerged");
        let made = add(&repo, "job").unwrap();
        std::fs::write(made.path.join("work.txt"), "w").unwrap();
        run_git(&made.path, &["add", "."]);
        run_git(&made.path, &["commit", "-q", "-m", "work"]);

        let info = prune("job", &made.path, None, &[]).unwrap();
        assert_eq!(info["removed"], true);
        assert_eq!(info["branchDeleted"], false);
        assert!(info["kept"].as_str().is_some_and(|k| !k.is_empty()));
        assert!(!made.path.exists());
        assert!(
            !run_git(&repo, &["branch", "--list", "orch/job"])
                .trim()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // Audit finding (REL-08, AU-07): a label without a matching worktree
    // printed no "worktree" key, so a worktree that was looked for in the
    // wrong repository could not be told from none at all.
    #[test]
    fn prune_says_why_when_no_worktree_matches_the_label() {
        let (base, repo) = repo("none");
        let missing = prune("nothing", &repo, None, &[]).unwrap();
        assert_eq!(missing["removed"], false);
        assert_eq!(missing["branchDeleted"], false);
        let reason = missing["kept"].as_str().unwrap();
        assert!(
            reason.contains("orch/nothing") && reason.contains("repo-wt/nothing"),
            "{reason}"
        );
        // Not a label `spawn --worktree` could have given.
        assert!(prune("../escape", &repo, None, &[]).is_none());
        // A plain branch of the same name at the same path is not ours.
        let path = worktree_path(&repo, "other");
        run_git(
            &repo,
            &["worktree", "add", "-b", "feature", path.to_str().unwrap()],
        );
        let other = prune("other", &path, None, &[]).unwrap();
        assert_eq!(other["removed"], false);
        assert!(path.exists());
        // A pane whose cwd is in no repository.
        let outside = base.join("plain");
        std::fs::create_dir_all(&outside).unwrap();
        if main_worktree(&outside).is_err() {
            let lost = prune("job", &outside, None, &[]).unwrap();
            assert!(
                lost["kept"]
                    .as_str()
                    .is_some_and(|kept| kept.contains("orch/job")),
                "{lost}"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // Audit finding (REL-08, AU-07): `add` and `prune` worked from the
    // caller's `--show-toplevel`, so from inside a worker's worktree `add`
    // nested the new one under `<repo>-wt/A-wt/` and `prune` found nothing.
    #[test]
    fn add_and_prune_use_the_main_worktree_from_anywhere_in_the_repository() {
        let (base, repo) = repo("main-base");
        let first = add(&repo, "a").unwrap();
        let sub = first.path.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let second = add(&sub, "b").unwrap();
        assert_eq!(second.path, base.join("repo-wt").join("b"));

        // The pane's cwd finds the worktree whatever the caller's cwd is.
        let info = prune("b", &second.path.join("."), None, &[]).unwrap();
        assert_eq!(info["removed"], true, "{info}");
        assert_eq!(info["branchDeleted"], true, "{info}");
        assert!(!second.path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn uncommitted_changes_and_untracked_files_are_warned_apart() {
        assert!(uncommitted_warnings("").is_empty());
        assert_eq!(
            uncommitted_warnings("?? task-1.txt\n?? result-1.json\n"),
            ["2 untracked files are not in the worktree"]
        );
        assert_eq!(
            uncommitted_warnings(" M src/a.rs\n?? task.txt\n"),
            [
                "this checkout has uncommitted changes to 1 tracked file; they are not in the \
                 worktree",
                "1 untracked file is not in the worktree",
            ]
        );
        assert_eq!(
            uncommitted_warnings("M  a\nR  b -> c\n"),
            [
                "this checkout has uncommitted changes to 2 tracked files; they are not in the \
              worktree"
            ]
        );
    }

    #[test]
    fn agent_commands_are_claude_codex_or_agy() {
        assert!(is_agent_command(&strings(&["/opt/bin/codex", "exec"])));
        assert!(is_agent_command(&strings(&["claude"])));
        assert!(!is_agent_command(&strings(&["zsh"])));
        assert!(!is_agent_command(&[]));
    }

    // Review finding: labels are not unique, so closing another pane that
    // had the same label pruned the worktree a running worker still used.
    #[test]
    fn prune_leaves_a_worktree_the_closed_pane_was_not_in() {
        let (base, repo) = repo("shared-label");
        let made = add(&repo, "job").unwrap();

        // Review finding: a skipped prune printed nothing, so the caller
        // could not tell it from a label without a worktree.
        let kept = prune("job", &repo, None, &[]).unwrap();
        assert_eq!(
            kept,
            serde_json::json!({
                "path": made.path.to_string_lossy(),
                "removed": false,
                "branchDeleted": false,
                "kept": NOT_IN_WORKTREE,
            })
        );
        assert!(made.path.exists());
        assert!(
            !run_git(&repo, &["branch", "--list", "orch/job"])
                .trim()
                .is_empty()
        );
        let removed = prune("job", &made.path.join("src"), None, &[]).unwrap();
        assert_eq!(removed["removed"], true);
        let _ = std::fs::remove_dir_all(&base);
    }
}
