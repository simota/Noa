//! Runs: one agent started on one task (L1-4). The run log
//! (`<data dir>/noa/tasks/<repo-key>/runs.jsonl`, append-only [`RunEvent`]
//! lines), the brief the agent reads (`<workdir>/.noa/tasks/runs/<run-id>/task.md`),
//! where it reports (`…/result.md`, its `NOA_RESULT`), and what is known of
//! a run now ([`RunView`]).

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::model::column::column;
use crate::model::types::{
    Column, Liveness, Millis, PaneFact, Report, ResultFact, Run, RunEvent, RunId, RunView,
    START_TIMEOUT_MS, State, Task, TaskRef,
};

/// Run directories, under the work tree's `.noa/tasks/`.
pub const RUNS_DIR: &str = "runs";
pub const TASK_FILE: &str = "task.md";
pub const RESULT_FILE: &str = "result.md";

/// The pane label of `run`: `task:<run-id>`, how its pane is found again.
pub fn label(run: &RunId) -> String {
    format!("task:{run}")
}

/// `<workdir>/.noa/tasks/runs/<run-id>`.
pub fn run_dir(workdir: &Path, run: &RunId) -> PathBuf {
    crate::local::tasks_dir(workdir)
        .join(RUNS_DIR)
        .join(run.as_str())
}

/// `<workdir>/.noa/tasks/runs/<run-id>/task.md`.
pub fn task_file(workdir: &Path, run: &RunId) -> PathBuf {
    run_dir(workdir, run).join(TASK_FILE)
}

/// `<workdir>/.noa/tasks/runs/<run-id>/result.md`.
pub fn result_file(workdir: &Path, run: &RunId) -> PathBuf {
    run_dir(workdir, run).join(RESULT_FILE)
}

/// A report is cut at this many bytes, as `+ctl board` cuts it.
pub const REPORT_MAX_BYTES: u64 = 16 * 1024;
/// … and at this many lines.
pub const REPORT_MAX_LINES: usize = 200;

/// A run id not in `taken`: `run-<now>`, then `run-<now>-2`, `-3` …
pub fn new_id(now: Millis, taken: &[RunId]) -> RunId {
    (1u32..)
        .map(|n| match n {
            1 => RunId::new(format!("run-{now}")),
            n => RunId::new(format!("run-{now}-{n}")),
        })
        .find(|id| !taken.contains(id))
        .expect("an unbounded range has a free id")
}

/// Holds the repository's run lock (`runs.jsonl.lock` beside `log`) until
/// dropped: every window on the repository takes it around "is a run live
/// on this task? then log RunStarted", so two cannot both start one.
pub fn lock(log: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = log.as_os_str().to_owned();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .open(PathBuf::from(name))?;
    // SAFETY: flock on a descriptor this function owns.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(file)
}

/// Appends one line to the run log. A torn last line (the widget died
/// mid-write) is ended first, so this event is a line of its own.
pub fn append(log: &Path, event: &RunEvent) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(log)?;
    let len = file.metadata()?.len();
    let mut last = [0u8; 1];
    let torn = len > 0 && {
        file.read_exact_at(&mut last, len - 1)?;
        last[0] != b'\n'
    };
    let mut line = if torn { b"\n".to_vec() } else { Vec::new() };
    serde_json::to_writer(&mut line, event).map_err(io::Error::other)?;
    line.push(b'\n');
    // One write of the whole line, so a reader never sees half of it
    // unless the process dies in between.
    (&file).write_all(&line)?;
    file.sync_data()
}

/// The runs the log holds; a torn last line is skipped (as is one torn
/// before a later append ended it).
pub fn replay(log: &Path) -> io::Result<Vec<Run>> {
    let text = match fs::read(log) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let events: Vec<RunEvent> = text
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Ok(fold(&events))
}

/// Folds events into runs, in start order.
pub fn fold(events: &[RunEvent]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for event in events {
        match event {
            RunEvent::RunStarted {
                run,
                task,
                agent,
                workdir,
                branch,
                at,
                ..
            } => {
                if runs.iter().any(|r| &r.run == run) {
                    continue;
                }
                runs.push(Run {
                    run: run.clone(),
                    task: task.clone(),
                    agent: *agent,
                    workdir: workdir.clone(),
                    branch: branch.clone(),
                    pane: None,
                    board: None,
                    started: *at,
                    closed: None,
                });
            }
            RunEvent::RunPaneAssigned {
                run, pane, board, ..
            } => {
                if let Some(r) = runs.iter_mut().find(|r| &r.run == run) {
                    r.pane = Some(*pane);
                    r.board = Some(*board);
                }
            }
            RunEvent::RunClosed { run, at } => {
                if let Some(r) = runs.iter_mut().find(|r| &r.run == run) {
                    r.closed.get_or_insert(*at);
                }
            }
        }
    }
    runs
}

/// The brief (`task.md`) of a run on `task` in `workdir`, reporting to `result`.
pub fn brief(task: &Task, workdir: &Path, result: &Path) -> String {
    let mut out = format!("# {}\n\n", task.title);
    match &task.r#ref {
        TaskRef::Local(id) => {
            out.push_str(&format!("- Task: local task `{id}`\n"));
        }
        TaskRef::GitHub { repo, number } => {
            out.push_str(&format!("- Task: GitHub issue {repo}#{number}\n"));
        }
        TaskRef::Code { path, .. } => {
            out.push_str(&format!(
                "- Task: a marker (TODO / FIXME) in the code, in `{path}` (where below)\n"
            ));
        }
    }
    if let Some(url) = &task.url {
        out.push_str(&format!("- URL: {url}\n"));
    }
    if !task.labels.is_empty() {
        out.push_str(&format!("- Labels: {}\n", task.labels.join(", ")));
    }
    out.push_str(&format!("- Work in: {}\n", workdir.display()));
    out.push_str(&format!(
        "- Report to: {} (also in $NOA_RESULT)\n\n",
        result.display()
    ));
    out.push_str("## Task\n\n");
    match task.body.as_deref().map(str::trim_end) {
        Some(body) if !body.is_empty() => {
            out.push_str(body);
            out.push_str("\n\n");
        }
        _ => match (&task.r#ref, &task.body) {
            (TaskRef::GitHub { repo, number }, None) => out.push_str(&format!(
                "(The issue's text was not loaded: read it with \
                 `gh issue view {number} --repo {repo} --json title,body,comments`.)\n\n"
            )),
            _ => out.push_str("(No description beyond the title.)\n\n"),
        },
    }
    out.push_str(&format!(
        "## How to work\n\n\
         - Do the task in {}. If it is a worktree on a branch of its own, commit \
         your work on that branch. Never push, switch branches or merge.\n",
        workdir.display()
    ));
    if let TaskRef::GitHub { number, .. } = &task.r#ref {
        out.push_str(&format!(
            "- Put `Fixes #{number}` in the commit message only when the work fully \
             fixes the issue, so it closes when the change is merged.\n"
        ));
    }
    if let TaskRef::Code { path, .. } = &task.r#ref {
        out.push_str(&format!(
            "- Remove the marker from `{path}` only when the work fully does what it \
             asks: the task leaves the list once the marker is gone.\n"
        ));
    }
    out.push_str(
        "- Something you find outside this task: record it as a new local task \
         (a `t-<millis>.md` file in the main worktree's `.noa/tasks/`, the format \
         the noa-tasks skill describes) and name it in your report, instead of doing it.\n\
         - Do not close this task, local or on GitHub: the person decides that after \
         reading your report.\n\n",
    );
    out.push_str(&format!(
        "## Report\n\n\
         Before you finish, write your report to {} ($NOA_RESULT): what you changed \
         (files, commits), how you checked it (commands and their outcome), and what is left.\n",
        result.display()
    ));
    out
}

/// The agent's prompt: `Read <task.md> and do the task it describes. Write your report to $NOA_RESULT.`
pub fn prompt(task_file: &Path) -> String {
    format!(
        "Read {} and do the task it describes. Write your report to $NOA_RESULT.",
        task_file.display()
    )
}

/// The result file, `lstat`'ed; a link counts as Missing.
pub fn result_fact(path: &Path) -> ResultFact {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => ResultFact::Written {
            bytes: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as Millis),
        },
        _ => ResultFact::Missing,
    }
}

/// The result file's text, capped at 16 KiB / 200 lines.
pub fn read_report(path: &Path) -> Option<Report> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(REPORT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut cut = bytes.len() as u64 > REPORT_MAX_BYTES;
    bytes.truncate(REPORT_MAX_BYTES as usize);
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: up to the last whole one.
        Err(e) if cut && e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).ok()?
        }
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    if let Some((at, _)) = text.match_indices('\n').nth(REPORT_MAX_LINES - 1)
        && at + 1 < text.len()
    {
        text.truncate(at + 1);
        cut = true;
    }
    Some(Report { text, cut })
}

/// What is known of `run` now, from the panes Noa lists.
pub fn view(
    run: &Run,
    panes: &[PaneFact],
    result: ResultFact,
    report: Option<Report>,
    now: Millis,
) -> RunView {
    let label = label(&run.run);
    let pane = panes
        .iter()
        .filter(|p| p.label.as_deref() == Some(label.as_str()))
        // Its own pane first, should a label be listed twice.
        .max_by_key(|p| Some(p.pane_id) == run.pane);
    let liveness = match pane {
        _ if run.closed.is_some() => Liveness::Gone,
        Some(pane) if pane.exited => Liveness::Gone,
        Some(pane) => Liveness::Alive { agent: pane.agent },
        None if run.pane.is_none() && now < run.started.saturating_add(START_TIMEOUT_MS) => {
            Liveness::Starting
        }
        None => Liveness::Gone,
    };
    RunView {
        run: run.clone(),
        liveness,
        result,
        report,
    }
}

/// Whether a new run may start on `task`: refused on a task file that does
/// not parse (or is a link), and while its last run is live (starting, or
/// its agent not finished or in error), whatever the task's state: one
/// live run per task (L1-4), a closed task's included.
pub fn may_start(task: &Task, last: Option<&RunView>) -> Result<(), String> {
    if let Some(problem) = &task.problem {
        return Err(format!("「{}」は実行できません（{problem}）", task.title));
    }
    // The column the task would be in open: 実行中 while the run is live.
    let open = Task {
        state: State::Open,
        ..task.clone()
    };
    match column(&open, last) {
        Column::Running => Err(format!(
            "「{}」は実行中です（詳細の「ペインを開く」でペインへ）",
            task.title
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runs_files_are_under_its_run_dir_and_its_label_names_it() {
        let run = RunId::new("run-5");
        assert_eq!(label(&run), "task:run-5");
        let dir = Path::new("/w/.noa/tasks/runs/run-5");
        assert_eq!(run_dir(Path::new("/w"), &run), dir);
        assert_eq!(task_file(Path::new("/w"), &run), dir.join("task.md"));
        assert_eq!(result_file(Path::new("/w"), &run), dir.join("result.md"));
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use crate::model::types::{AgentKind, AgentState, State};
    use crate::testkit::{TempDir, issue, labelled, local, run as run_on};

    fn started(run: &str, task: TaskRef, at: Millis) -> RunEvent {
        RunEvent::RunStarted {
            run: RunId::new(run),
            task,
            agent: AgentKind::Codex,
            workdir: PathBuf::from("/w"),
            branch: Some(format!("task-{run}")),
            label: format!("task:{run}"),
            at,
        }
    }

    /// The run lock refuses a link where its file goes (never taking a
    /// lock elsewhere), and holds others off until dropped.
    #[test]
    fn the_run_lock_refuses_a_link_and_is_exclusive() {
        let tmp = crate::testkit::TempDir::new("runs-lock");
        let log = tmp.path().join("data/runs.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        let elsewhere = tmp.path().join("elsewhere.lock");
        std::os::unix::fs::symlink(&elsewhere, tmp.path().join("data/runs.jsonl.lock")).unwrap();
        assert!(lock(&log).is_err());
        assert!(!elsewhere.exists(), "the link was followed");
        fs::remove_file(tmp.path().join("data/runs.jsonl.lock")).unwrap();

        let held = lock(&log).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = {
            let log = log.clone();
            std::thread::spawn(move || {
                let _second = lock(&log).unwrap();
                tx.send(()).unwrap();
            })
        };
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "a second holder got in while the first held it"
        );
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the second holder gets it once the first lets go");
        waiting.join().unwrap();
    }

    #[test]
    fn a_markers_brief_names_its_file_and_when_to_remove_it() {
        let task = crate::code::parse(b"src/a.rs\x007\x00// TODO: speed up\n", |_| 1).remove(0);
        let text = brief(&task, Path::new("/w"), Path::new("/w/r.md"));
        assert!(text.contains("in `src/a.rs`"), "{text}");
        assert!(text.contains("src/a.rs:7"), "{text}");
        assert!(text.contains("Remove the marker from `src/a.rs`"), "{text}");
        assert!(!text.contains("Fixes #"), "{text}");
    }

    #[test]
    fn run_ids_take_the_next_free_suffix() {
        assert_eq!(new_id(5, &[]), RunId::new("run-5"));
        let taken = [RunId::new("run-5"), RunId::new("run-5-2")];
        assert_eq!(new_id(5, &taken), RunId::new("run-5-3"));
    }

    #[test]
    fn ac12_append_and_replay_fold_the_three_events() {
        let tmp = TempDir::new("runs-log");
        let log = tmp.path().join("key/runs.jsonl");
        assert!(replay(&log).unwrap().is_empty(), "no log yet");
        let t = TaskRef::Local("t-1".into());
        for event in [
            started("run-1", t.clone(), 10),
            started("run-2", t.clone(), 20),
            RunEvent::RunPaneAssigned {
                run: RunId::new("run-1"),
                pane: 41,
                board: 7,
                at: 11,
            },
            RunEvent::RunClosed {
                run: RunId::new("run-1"),
                at: 30,
            },
        ] {
            append(&log, &event).unwrap();
        }
        let runs = replay(&log).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0],
            Run {
                run: RunId::new("run-1"),
                task: t.clone(),
                agent: AgentKind::Codex,
                workdir: PathBuf::from("/w"),
                branch: Some("task-run-1".into()),
                pane: Some(41),
                board: Some(7),
                started: 10,
                closed: Some(30),
            }
        );
        assert_eq!((runs[1].pane, runs[1].closed), (None, None));
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 4);
    }

    #[test]
    fn ac12_a_torn_last_line_is_skipped_and_the_next_append_is_a_line_of_its_own() {
        let tmp = TempDir::new("runs-torn");
        let log = tmp.path().join("runs.jsonl");
        append(&log, &started("run-1", TaskRef::Local("t-1".into()), 1)).unwrap();
        let mut text = fs::read(&log).unwrap();
        text.extend_from_slice(br#"{"event":"RunStarted","run":"run-2","ta"#);
        fs::write(&log, &text).unwrap();
        let runs = replay(&log).unwrap();
        assert_eq!(
            runs.iter().map(|r| r.run.as_str()).collect::<Vec<_>>(),
            ["run-1"]
        );
        append(
            &log,
            &RunEvent::RunClosed {
                run: RunId::new("run-1"),
                at: 9,
            },
        )
        .unwrap();
        let runs = replay(&log).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].closed, Some(9));
    }

    fn pane(id: u64, label: &str, agent: Option<AgentState>, exited: bool) -> PaneFact {
        PaneFact {
            pane_id: id,
            parent: Some(7),
            label: Some(label.to_owned()),
            agent,
            exited,
        }
    }

    #[test]
    fn ac13_liveness_from_the_labelled_pane_the_clock_and_run_closed() {
        let t = TaskRef::Local("t-1".into());
        let mut run = run_on(&t, 1_000);
        let label = label(&run.run);
        let v = |run: &Run, panes: &[PaneFact], now| {
            view(run, panes, ResultFact::Missing, None, now).liveness
        };
        // No pane yet: Starting until 90 s, then Gone.
        assert_eq!(v(&run, &[], 1_000), Liveness::Starting);
        assert_eq!(
            v(&run, &[], 1_000 + START_TIMEOUT_MS - 1),
            Liveness::Starting
        );
        assert_eq!(v(&run, &[], 1_000 + START_TIMEOUT_MS), Liveness::Gone);
        // Its labelled pane: Alive with the agent's state; another label is not it.
        let others = [pane(3, "task:run-other", Some(AgentState::Running), false)];
        assert_eq!(v(&run, &others, 1_000 + START_TIMEOUT_MS), Liveness::Gone);
        run.pane = Some(41);
        for agent in [None, Some(AgentState::Input), Some(AgentState::Finished)] {
            let panes = [others[0].clone(), pane(41, &label, agent, false)];
            assert_eq!(v(&run, &panes, 5_000), Liveness::Alive { agent });
        }
        // A pane assigned but no longer listed, exited, or the run closed: Gone.
        assert_eq!(v(&run, &others, 5_000), Liveness::Gone);
        let exited = [pane(41, &label, Some(AgentState::Finished), true)];
        assert_eq!(v(&run, &exited, 5_000), Liveness::Gone);
        run.closed = Some(6_000);
        let alive = [pane(41, &label, None, false)];
        assert_eq!(v(&run, &alive, 7_000), Liveness::Gone);
    }

    #[test]
    fn ac13_the_report_is_capped_at_16_kib_and_200_lines() {
        let tmp = TempDir::new("runs-report");
        let path = tmp.path().join("result.md");
        assert_eq!(result_fact(&path), ResultFact::Missing);
        assert_eq!(read_report(&path), None);
        fs::write(&path, "done\n").unwrap();
        assert!(matches!(
            result_fact(&path),
            ResultFact::Written { bytes: 5, .. }
        ));
        assert_eq!(
            read_report(&path),
            Some(Report {
                text: "done\n".into(),
                cut: false
            })
        );
        // 200 lines exactly: whole; 201: cut to 200.
        let lines: String = (1..=200).map(|n| format!("{n}\n")).collect();
        fs::write(&path, &lines).unwrap();
        assert!(!read_report(&path).unwrap().cut);
        fs::write(&path, format!("{lines}201\n")).unwrap();
        let report = read_report(&path).unwrap();
        assert!(report.cut);
        assert_eq!(report.text, lines);
        // Over 16 KiB, cut on a character boundary.
        let big = "あ".repeat(6_000);
        fs::write(&path, &big).unwrap();
        let report = read_report(&path).unwrap();
        assert!(report.cut);
        assert!(report.text.len() <= 16 * 1024 && report.text.len() > 16 * 1024 - 3);
        assert!(big.starts_with(&report.text));
        // A link counts as missing and is not read.
        let link = tmp.path().join("link.md");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(result_fact(&link), ResultFact::Missing);
        assert_eq!(read_report(&link), None);
    }

    /// An issue whose text was not loaded: the command the brief gives
    /// reads it from the repository its ref names, not gh's default.
    #[test]
    fn an_unloaded_issues_brief_reads_it_from_its_own_repository() {
        let task = Task {
            r#ref: TaskRef::GitHub {
                repo: "a/one".into(),
                number: 12,
            },
            title: "x".into(),
            body: None,
            state: State::Open,
            labels: Vec::new(),
            updated: 0,
            url: None,
            closed: None,
            problem: None,
        };
        let text = brief(&task, Path::new("/w"), Path::new("/w/r.md"));
        assert!(
            text.contains("gh issue view 12 --repo a/one --json title,body,comments"),
            "{text}"
        );
    }

    #[test]
    fn ac14_the_brief_names_the_task_where_to_work_and_where_to_report() {
        let workdir = Path::new("/src/noa-wt/task-run-5");
        let result = Path::new("/src/noa-wt/task-run-5/.noa/tasks/runs/run-5/result.md");
        let local_task = labelled(local("t-1", "フォントを直す"), &["bug"]);
        let brief_local = brief(&local_task, workdir, result);
        for part in [
            "# フォントを直す",
            "t-1",
            "フォントを直す の本文",
            "bug",
            "/src/noa-wt/task-run-5",
            result.to_str().unwrap(),
        ] {
            assert!(brief_local.contains(part), "{part:?} in\n{brief_local}");
        }
        assert!(!brief_local.contains("Fixes #"), "{brief_local}");
        assert!(!brief_local.contains("URL:"), "{brief_local}");

        let mut gh = issue(12, "Add search");
        gh.body = Some("Search the scrollback.".into());
        let brief_gh = brief(&gh, workdir, result);
        for part in [
            "# Add search",
            "noa-dev/noa#12",
            "https://github.com/noa-dev/noa/issues/12",
            "Search the scrollback.",
            "Fixes #12",
            result.to_str().unwrap(),
        ] {
            assert!(brief_gh.contains(part), "{part:?} in\n{brief_gh}");
        }
        // An issue whose body is not loaded points at gh.
        assert!(brief(&issue(12, "x"), workdir, result).contains("gh issue view 12"));
        assert_eq!(
            prompt(Path::new("/w/.noa/tasks/runs/run-5/task.md")),
            "Read /w/.noa/tasks/runs/run-5/task.md and do the task it describes. Write your report to $NOA_RESULT."
        );
    }

    #[test]
    fn ac16_a_run_is_refused_while_running_and_allowed_as_a_retry() {
        use crate::testkit::run_view;
        let task = local("t-1", "a");
        assert!(may_start(&task, None).is_ok());
        for liveness in [
            Liveness::Starting,
            Liveness::Alive { agent: None },
            Liveness::Alive {
                agent: Some(AgentState::Permission),
            },
        ] {
            let last = run_view(&task.r#ref, liveness, ResultFact::Missing);
            assert!(
                may_start(&task, Some(&last))
                    .unwrap_err()
                    .contains("実行中")
            );
        }
        for liveness in [
            Liveness::Gone,
            Liveness::Alive {
                agent: Some(AgentState::Finished),
            },
        ] {
            let last = run_view(&task.r#ref, liveness, ResultFact::Missing);
            assert!(may_start(&task, Some(&last)).is_ok(), "確認待ち: a retry");
        }
        let mut done = task.clone();
        done.state = State::Closed;
        let last = run_view(&task.r#ref, Liveness::Gone, ResultFact::Missing);
        assert!(may_start(&done, Some(&last)).is_ok());
    }

    #[test]
    fn ac16_one_live_run_per_task_also_once_it_is_closed() {
        use crate::testkit::{closed, run_view};
        // Closed (`d`, or on GitHub) while its agent still works.
        for task in [closed(local("t-1", "a")), closed(issue(4, "b"))] {
            for liveness in [
                Liveness::Starting,
                Liveness::Alive { agent: None },
                Liveness::Alive {
                    agent: Some(AgentState::Running),
                },
                Liveness::Alive {
                    agent: Some(AgentState::Input),
                },
            ] {
                let last = run_view(&task.r#ref, liveness.clone(), ResultFact::Missing);
                let refused = may_start(&task, Some(&last));
                assert!(
                    refused.as_ref().is_err_and(|e| e.contains("実行中")),
                    "{liveness:?}: {refused:?}"
                );
            }
            let gone = run_view(&task.r#ref, Liveness::Gone, ResultFact::Missing);
            assert!(may_start(&task, Some(&gone)).is_ok(), "reopened by a run");
        }
    }

    #[test]
    fn ac16_a_task_file_that_does_not_parse_is_never_run() {
        let mut broken = local("t-3", "t-3");
        broken.problem = Some("読めません: 1 行目が --- ではありません".into());
        let refused = may_start(&broken, None).unwrap_err();
        assert!(
            refused.contains("t-3") && refused.contains("読めません"),
            "{refused}"
        );
    }
}
