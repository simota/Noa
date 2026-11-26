//! Starting, focusing and closing a run against the [`Noa`] trait.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tasks::auto_assign::AutoRecord;
use tasks::model::types::{AgentKind, Isolation, Millis, Run, RunEvent, RunId, Task};
use tasks::task_links::{self, LinkEvent};
use tasks::{fsx, runs, shell, store, worktree};

use super::ipc::{Noa, NoaError, PaneSpec, PaneView, Placement, no_board_tab};

/// The variable the run's agent finds its result file in.
pub const RESULT_VAR: &str = "NOA_RESULT";
/// The board tab's label (the server names a board tab itself; kept for
/// the spec's sake).
const BOARD_LABEL: &str = "tasks";

/// What 実行 asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub task: Task,
    pub repo: PathBuf,
    pub agent: AgentKind,
    pub isolation: Isolation,
    /// The auto-assignment the form was filled from, when one was and the
    /// person kept its pick; logged on `RunStarted`.
    pub auto: Option<AutoRecord>,
}

/// `git -C dir <args>`, without the person's `GIT_*` variables; its stdout,
/// or why it failed.
pub fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // As `store::without_git_env` (crate-private to the library): git finds
    // the repository from `dir` alone.
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// The commit `dir`'s work tree is at (`git rev-parse HEAD`).
pub fn head_of(dir: &Path) -> Result<String, String> {
    let out = git(dir, &["rev-parse", "--verify", "HEAD"])?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// The run log's link log (`links.jsonl` in its key dir).
pub fn links_of(log: &Path) -> PathBuf {
    task_links::links_log(log.parent().unwrap_or(Path::new("")))
}

/// Records the commit `run` starts from: `ChangeBase` with `workdir`'s
/// `HEAD`, in `log`'s key dir.
pub fn record_base(log: &Path, run: &RunId, workdir: &Path, now: Millis) -> Result<(), String> {
    let commit = head_of(workdir)?;
    task_links::append(
        &links_of(log),
        &LinkEvent::ChangeBase {
            run: run.to_string(),
            commit,
            at: now,
        },
    )
    .map_err(|e| e.to_string())
}

/// Workdir (worktree::add or repo), prepare_workdir, task.md, RunStarted, the board tab if
/// `board` is None (cwd repo), the float (label, shell::argv, env NOA_RESULT + PROMPT_VAR),
/// RunPaneAssigned. The float is `attended: true` (keyboard focus), as review's fix request
/// (Decision 7). Auto approval is the tab's and a float takes its board tab's, so the board
/// spec is `attended: true` too: ipc.rs then sends the board `autoApprove: false` (still no focus).
/// Refused, nothing written, without boardTab.
///
/// A float refused because its board tab is gone (closed by the person)
/// is tried once more on a new board tab. A run whose float cannot be
/// opened at all is logged closed, so its card does not wait out
/// `START_TIMEOUT_MS` in 実行中.
///
/// Once `RunStarted` is logged, the commit the run starts from is recorded
/// (`ChangeBase`, `docs/specs/task-links.md` L1-3); why it could not be is
/// the second value: the run goes on.
pub fn start<N: Noa>(
    noa: &mut N,
    board: &mut Option<u64>,
    log: &Path,
    req: &RunRequest,
    now: Millis,
) -> Result<(RunId, Option<String>), String> {
    if !noa.supports_board_tab() {
        return Err(no_board_tab(&noa.server_version()));
    }
    let taken: Vec<RunId> = runs::replay(log)
        .map_err(|e| format!("実行の記録を読めません: {e}"))?
        .into_iter()
        .map(|r| r.run)
        .collect();
    let run = runs::new_id(now, &taken);
    let (workdir, branch) = match req.isolation {
        Isolation::Worktree => {
            let (path, branch) = worktree::add(&req.repo, &run)?;
            (path, Some(branch))
        }
        Isolation::Here => (req.repo.clone(), None),
    };
    let task_file = runs::task_file(&workdir, &run);
    let result = runs::result_file(&workdir, &run);
    let label = runs::label(&run);
    let logged = |event: &RunEvent| {
        runs::append(log, event).map_err(|e| format!("実行の記録に書けません: {e}"))
    };
    let prepared = store::prepare_workdir(&workdir)
        .map_err(|e| format!("{e}"))
        .and_then(|()| {
            fsx::create_dirs(&workdir, &runs::run_dir(&workdir, &run))
                .and_then(|()| {
                    fsx::write_new(
                        &workdir,
                        &task_file,
                        runs::brief(&req.task, &workdir, &result).as_bytes(),
                    )
                })
                .map_err(|e| format!("{} を書けません: {e}", task_file.display()))
        })
        .and_then(|()| {
            logged(&RunEvent::RunStarted {
                run: run.clone(),
                task: req.task.r#ref.clone(),
                agent: req.agent,
                workdir: workdir.clone(),
                branch: branch.clone(),
                label: label.clone(),
                at: now,
                auto: req.auto.clone(),
            })
        });
    if let Err(why) = prepared {
        // Not logged, so nothing else would ever name the worktree this
        // run made: take it back.
        if let Some(branch) = &branch
            && let Err(left) = worktree::remove(&req.repo, &workdir, branch)
        {
            return Err(format!("{why}（{left}）"));
        }
        return Err(why);
    }
    let base = record_base(log, &run, &workdir, now).err();
    let spec = PaneSpec {
        argv: shell::argv(req.agent),
        cwd: workdir,
        label,
        attended: true,
        env: vec![
            (RESULT_VAR.to_owned(), result.display().to_string()),
            (shell::PROMPT_VAR.to_owned(), runs::prompt(&task_file)),
        ],
    };
    let spawned = float(noa, board, &req.repo, &spec);
    let (pane, on) = match spawned {
        Ok(spawned) => spawned,
        Err(why) => {
            logged(&RunEvent::RunClosed {
                run: run.clone(),
                at: now,
            })?;
            return Err(format!("エージェントのペインを開けません: {why}"));
        }
    };
    logged(&RunEvent::RunPaneAssigned {
        run: run.clone(),
        pane,
        board: on,
        at: now,
    })?;
    Ok((run, base))
}

/// Spawns `spec` as a float on `board`, opening the board tab first when
/// there is none, and once more on a new one when the board is gone; the
/// float's id and its board's.
fn float<N: Noa>(
    noa: &mut N,
    board: &mut Option<u64>,
    repo: &Path,
    spec: &PaneSpec,
) -> Result<(u64, u64), NoaError> {
    let fresh = board.is_none();
    let on = open_board(noa, board, repo)?;
    match noa.spawn(Placement::Float(on), spec) {
        Ok(pane) => Ok((pane, on)),
        Err(NoaError::PaneGone) if !fresh => {
            *board = None;
            let on = open_board(noa, board, repo)?;
            noa.spawn(Placement::Float(on), spec).map(|pane| (pane, on))
        }
        Err(err) => Err(err),
    }
}

/// `board`, or a new board tab in `repo` (attended: no auto approval).
fn open_board<N: Noa>(noa: &mut N, board: &mut Option<u64>, repo: &Path) -> Result<u64, NoaError> {
    if let Some(on) = *board {
        return Ok(on);
    }
    let spec = PaneSpec {
        argv: Vec::new(),
        cwd: repo.to_owned(),
        label: BOARD_LABEL.to_owned(),
        attended: true,
        env: Vec::new(),
    };
    let on = noa.spawn(Placement::Board, &spec)?;
    *board = Some(on);
    Ok(on)
}

/// The board tab of the repository's runs: a live run pane's parent.
pub fn board_of(panes: &[PaneView], runs: &[Run]) -> Option<u64> {
    runs.iter()
        .rev()
        .filter(|run| run.closed.is_none())
        .find_map(|run| {
            let label = runs::label(&run.run);
            panes
                .iter()
                .find(|p| p.label.as_deref() == Some(label.as_str()) && !p.exited)
                .and_then(|p| p.parent_pane_id)
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::process::{Command, Stdio};

    use tasks::model::types::{State, TaskRef};

    use super::*;
    use crate::live::fake::{FakeNoa, float};

    /// A temp dir removed on drop (the library's testkit is not reachable
    /// from the binary).
    pub(crate) struct Temp(pub PathBuf);

    impl Temp {
        pub(crate) fn new(name: &str) -> Temp {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "noa-tasks-live-{name}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Temp(path.canonicalize().unwrap())
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A git repository with one commit at `dir`.
    pub(crate) fn repo_at(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        ] {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(dir)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (name, _) in std::env::vars_os() {
                if name.as_encoded_bytes().starts_with(b"GIT_") {
                    cmd.env_remove(name);
                }
            }
            assert!(cmd.status().unwrap().success());
        }
    }

    pub(crate) fn local_task(id: &str) -> Task {
        Task {
            r#ref: TaskRef::Local(id.to_owned()),
            title: "フォントを直す".into(),
            body: Some("本文\n".into()),
            state: State::Open,
            labels: Vec::new(),
            updated: 1,
            url: None,
            closed: None,
            problem: None,
        }
    }

    fn request(repo: &Path, isolation: Isolation) -> RunRequest {
        RunRequest {
            task: local_task("t-1"),
            repo: repo.to_owned(),
            agent: AgentKind::Codex,
            isolation,
            auto: None,
        }
    }

    fn env<'a>(spec: &'a PaneSpec, key: &str) -> &'a str {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no {key} in {:?}", spec.env))
    }

    /// A run refused after its worktree was made (here: the repository
    /// tracks a file named `.noa`, so the worktree's `.noa/` cannot be
    /// made) takes its worktree and branch back: nothing is left that no
    /// log line names.
    #[test]
    fn a_run_refused_after_its_worktree_was_made_removes_it_and_its_branch() {
        let tmp = Temp::new("start-wt-refused");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        fs::write(repo.join(".noa"), "x\n").unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{}",
                String::from_utf8_lossy(&ok.stderr)
            );
            String::from_utf8_lossy(&ok.stdout).into_owned()
        };
        git(&["add", "-f", ".noa"]);
        git(&["commit", "-q", "-m", "tracked"]);
        let log = tmp.0.join("data/runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        let err = start(
            &mut noa,
            &mut None,
            &log,
            &request(&repo, Isolation::Worktree),
            500,
        )
        .unwrap_err();
        assert!(!err.is_empty());
        assert!(!tmp.0.join("app-wt/task-run-500").exists(), "{err}");
        assert_eq!(git(&["branch", "--list", "task-run-500"]), "");
        assert!(!git(&["worktree", "list"]).contains("task-run-500"));
        assert!(runs::replay(&log).unwrap_or_default().is_empty());
    }

    #[test]
    fn ac15_a_worktree_run_floats_on_a_new_board_tab_attended_and_logged_around_the_spawn() {
        let tmp = Temp::new("start-wt");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let log = tmp.0.join("data/runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        let mut board = None;
        let run = start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Worktree),
            500,
        )
        .unwrap()
        .0;
        assert_eq!(run, RunId::new("run-500"));
        let workdir = tmp.0.join("app-wt/task-run-500");
        assert!(workdir.is_dir());
        let branch = Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&branch.stdout).trim(),
            "task-run-500"
        );
        assert_eq!(
            fs::read_to_string(workdir.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        let task_file = runs::task_file(&workdir, &run);
        let brief = fs::read_to_string(&task_file).unwrap();
        assert!(
            brief.contains("フォントを直す") && brief.contains("t-1"),
            "{brief}"
        );

        let spawned = noa.with(|s| s.spawned.clone());
        let [
            (Placement::Board, tab, at_tab),
            (Placement::Float(on), float, at_float),
        ] = &spawned[..]
        else {
            panic!("{spawned:?}");
        };
        assert_eq!(board, Some(*on));
        assert!(
            tab.attended,
            "the board tab asks the person (no auto approval)"
        );
        assert_eq!(tab.cwd, repo);
        assert!(float.attended);
        assert_eq!(float.label, "task:run-500");
        assert_eq!(float.argv, shell::argv(AgentKind::Codex));
        assert_eq!(float.cwd, workdir);
        assert_eq!(
            env(float, "NOA_RESULT"),
            runs::result_file(&workdir, &run).display().to_string()
        );
        assert_eq!(env(float, "NOA_TASKS_PROMPT"), runs::prompt(&task_file));
        // RunStarted before the spawns, RunPaneAssigned after.
        for snapshot in [at_tab, at_float] {
            assert!(snapshot.contains("\"RunStarted\""), "{snapshot}");
            assert!(!snapshot.contains("RunPaneAssigned"), "{snapshot}");
        }
        let runs = runs::replay(&log).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].workdir, workdir);
        assert_eq!(runs[0].branch.as_deref(), Some("task-run-500"));
        assert_eq!(runs[0].board, Some(*on));
        assert_eq!(
            runs[0].pane,
            noa.with(|s| s.panes.last().map(|p| p.pane_id))
        );
    }

    #[test]
    fn ac15_here_uses_the_repo_and_two_runs_share_one_board_tab() {
        let tmp = Temp::new("start-here");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let log = tmp.0.join("runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        let mut board = None;
        let first = start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Here),
            9,
        )
        .unwrap()
        .0;
        let second = start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Here),
            9,
        )
        .unwrap()
        .0;
        assert_eq!((first.as_str(), second.as_str()), ("run-9", "run-9-2"));
        let spawned = noa.with(|s| s.spawned.clone());
        let boards = spawned
            .iter()
            .filter(|(p, _, _)| *p == Placement::Board)
            .count();
        assert_eq!(boards, 1, "{spawned:?}");
        let on = board.unwrap();
        for (placement, spec, _) in &spawned[1..] {
            assert_eq!(*placement, Placement::Float(on));
            assert_eq!(spec.cwd, repo);
        }
        let runs = runs::replay(&log).unwrap();
        assert!(runs.iter().all(|r| r.workdir == repo && r.branch.is_none()));
        assert!(runs::task_file(&repo, &second).is_file());
        assert!(!tmp.0.join("app-wt").exists());
    }

    /// Auto-assign L1-7: the request's `AutoRecord` is logged on the run's
    /// `RunStarted`, and a manual run logs none.
    #[test]
    fn run_started_logs_the_requests_auto_record() {
        let tmp = Temp::new("start-auto");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let log = tmp.0.join("runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        let mut board = None;
        let auto = AutoRecord {
            planner: "codex (gpt-6.2-sol · medium)".into(),
            at: 3,
            picks: vec![tasks::auto_assign::Pick {
                slot: "worker".into(),
                agent: "codex".into(),
                model: None,
                effort: None,
                reason: "小さな修正".into(),
            }],
            overridden: Vec::new(),
        };
        let with = RunRequest {
            auto: Some(auto.clone()),
            ..request(&repo, Isolation::Here)
        };
        start(&mut noa, &mut board, &log, &with, 9).unwrap();
        start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Here),
            9,
        )
        .unwrap();
        let text = fs::read_to_string(&log).unwrap();
        let logged: Vec<Option<AutoRecord>> = text
            .lines()
            .filter_map(|l| match serde_json::from_str::<RunEvent>(l).unwrap() {
                RunEvent::RunStarted { auto, .. } => Some(auto),
                _ => None,
            })
            .collect();
        assert_eq!(logged, [Some(auto), None], "{text}");
    }

    #[test]
    fn ac15_a_noa_without_board_tabs_is_refused_with_nothing_written() {
        let tmp = Temp::new("start-old");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let log = tmp.0.join("runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        noa.with(|s| s.no_board_tab = true);
        let mut board = None;
        let err = start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Worktree),
            1,
        )
        .unwrap_err();
        assert!(err.contains("0.9.15"), "{err}");
        assert!(!log.exists());
        assert!(!repo.join(".noa").exists());
        assert!(!tmp.0.join("app-wt").exists());
        assert!(noa.with(|s| s.spawned.is_empty()));
        assert_eq!(board, None);
    }

    #[test]
    fn a_board_tab_closed_since_is_replaced_and_a_float_that_cannot_open_ends_the_run() {
        let tmp = Temp::new("start-gone");
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let log = tmp.0.join("runs.jsonl");
        let mut noa = FakeNoa::new(Some(log.clone()));
        noa.with(|s| s.gone_boards.push(5));
        let mut board = Some(5);
        start(
            &mut noa,
            &mut board,
            &log,
            &request(&repo, Isolation::Here),
            1,
        )
        .unwrap();
        let fresh = board.unwrap();
        assert_ne!(fresh, 5);
        assert_eq!(
            noa.with(|s| s.spawned.len()),
            2,
            "a new board, then the float"
        );

        // Every board refuses: the run is logged closed, not left starting.
        noa.with(|s| s.gone_boards.push(fresh));
        let mut stale = Some(fresh);
        noa.with(|s| s.gone_boards.push(s.next + 1));
        let err = start(
            &mut noa,
            &mut stale,
            &log,
            &request(&repo, Isolation::Here),
            2,
        )
        .unwrap_err();
        assert!(err.contains("ペイン"), "{err}");
        let runs = runs::replay(&log).unwrap();
        assert_eq!(runs.last().unwrap().closed, Some(2));
    }

    #[test]
    fn the_board_is_the_parent_of_a_live_run_pane() {
        let t = TaskRef::Local("t-1".into());
        let run = |id: &str, closed| Run {
            run: RunId::new(id),
            task: t.clone(),
            agent: AgentKind::Claude,
            workdir: PathBuf::from("/w"),
            branch: None,
            pane: Some(1),
            board: Some(2),
            started: 1,
            closed,
        };
        let panes = [float(40, 7, "task:run-1"), float(41, 8, "task:run-2")];
        assert_eq!(board_of(&panes, &[run("run-1", None)]), Some(7));
        assert_eq!(board_of(&panes, &[run("run-1", Some(3))]), None, "closed");
        assert_eq!(board_of(&panes, &[run("run-9", None)]), None, "no pane");
        let mut exited = panes.clone();
        exited[0].exited = true;
        assert_eq!(board_of(&exited, &[run("run-1", None)]), None);
        // The newest live run's.
        assert_eq!(
            board_of(&panes, &[run("run-1", None), run("run-2", None)]),
            Some(8)
        );
    }
}
