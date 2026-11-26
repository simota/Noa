//! `Noa +ctl retry PANE`: start a worker whose result failed the
//! orchestrator's check again, in its directory under its label, on the
//! same or another tier, with its task and a preface saying the previous
//! attempt's changes are there (`docs/specs/agent-orchestration.md` D19).
//!
//! What to do is decided from this pane's journal and one listing before
//! anything happens ([`plan`], pure); [`steps`] then checks the spawn, closes
//! the old pane and spawns, in that order.

use std::path::Path;

use noa_ipc::{Panel, WireId};

use super::{
    DEFAULT_MAX_CHILDREN, Failure, RetryArgs, SpawnArgs, Task, TaskText, check_send_size, journal,
    worktree,
};

/// How a preface starts, so a retry of a retry sends the task without the
/// earlier preface.
pub(crate) const PREFACE_START: &str = "Retry: a previous worker (pane ";
/// What parts the preface from the task.
pub(crate) const PREFACE_END: &str = "\n\n---\n\n";

/// What `retry` does once its checks passed.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    /// The pane retried.
    pub(crate) old: u64,
    /// The old pane is still listed as this pane's worker: close it first.
    pub(crate) listed: bool,
    /// The spawn of the new worker, with `--task` (the preface and task).
    pub(crate) spawn: SpawnArgs,
    /// Added to the spawn line and its journal event: `retryOf`, and the
    /// old worker's `worktree` and `branch` when it had them.
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

/// Decide `retry`'s spawn from this pane's journal (`workers`) and a
/// listing (`panels`) of `own`'s panes. `given` is the `--task` or
/// `--task-file` text; `report_written` says whether a result file holds a
/// report. `Err` is why nothing is done.
pub(crate) fn plan(
    args: &RetryArgs,
    given: Option<String>,
    workers: &[journal::Worker],
    panels: &[Panel],
    own: u64,
    supports_timeline: bool,
    report_written: impl Fn(&Path) -> bool,
) -> Result<Plan, String> {
    let old = args.pane;
    let worker = workers
        .iter()
        .find(|worker| worker.pane == old)
        .ok_or_else(|| {
            format!(
                "pane {old} is not a worker this pane spawned (by this pane's journal); retry \
                 only starts this pane's own workers again"
            )
        })?;
    // A pane listed under another parent took the id after Noa restarted:
    // the worker is gone, and that pane is not touched.
    let panel = panels
        .iter()
        .find(|panel| panel.pane_id.0 == old && panel.parent_pane_id == Some(WireId(own)));
    if panel
        .and_then(|panel| panel.agent.as_ref())
        .is_some_and(|agent| agent.state.as_deref() == Some("running"))
    {
        return Err(format!(
            "pane {old} is working; wait for its turn to end, or close it first. Nothing was done"
        ));
    }
    // `spawn` refuses `--no-board` from a worker (D10); refused here, the
    // old pane is not closed for a spawn that cannot happen.
    let is_worker = panels
        .iter()
        .any(|panel| panel.pane_id.0 == own && panel.parent_pane_id.is_some());
    super::spawn_opens_board(is_worker, args.board)
        .map_err(|message| format!("{message}. Nothing was done"))?;
    let argv = worker
        .argv
        .as_ref()
        .and_then(strings)
        .or_else(|| Some(panel?.spawn.as_ref()?.argv.clone()))
        .filter(|argv| !argv.is_empty())
        .ok_or_else(|| {
            format!(
                "pane {old} was started without a command: retry starts claude, codex or agy \
                 again"
            )
        })?;
    if !worktree::is_agent_command(&argv) {
        return Err(format!(
            "pane {old} runs {:?}: retry types the task, so it only starts claude, codex or agy \
             again",
            argv[0]
        ));
    }
    let old_tier = worker
        .tier
        .as_deref()
        .map(worktree::Tier::parse)
        .transpose()?;
    let command = match old_tier {
        Some(tier) => worktree::remove_tier(&argv, tier)?,
        None => argv.clone(),
    };
    let tier = args.tier.or(old_tier);
    if tier.is_some() {
        worktree::check_tier_command(&command)?;
    }
    let cwd = worker.cwd.clone().ok_or_else(|| {
        format!("this pane's journal has no directory for pane {old}; spawn it yourself")
    })?;
    let task = match given {
        Some(task) => task,
        None => {
            let sent = worker.task.as_deref().ok_or_else(|| format!(
                "this pane's journal does not hold pane {old}'s last task whole (none was sent, \
                 it was over {} KiB, or an older Noa +ctl kept only its start): pass \
                 --task-file F. Nothing was done",
                journal::FULL_TEXT_BYTES / 1024
            ))?;
            strip_preface(super::run_attr::strip_header(sent)).to_string()
        }
    };
    let outcome = worker
        .last_outcome
        .as_ref()
        .and_then(|outcome| outcome["reason"].as_str());
    let report = worker
        .result
        .as_deref()
        .filter(|path| report_written(Path::new(path)));
    let text = compose(
        &preface(old, &describe(&argv, old_tier), outcome, report),
        &task,
    );
    check_send_size(&super::run_attr::prospective_text(&text, supports_timeline))?;
    let mut extra = serde_json::Map::new();
    extra.insert("retryOf".to_string(), serde_json::json!(WireId(old)));
    if let (Some(path), Some(branch)) = (&worker.worktree, &worker.branch) {
        extra.insert("worktree".to_string(), path.clone().into());
        extra.insert("branch".to_string(), branch.clone().into());
    }
    Ok(Plan {
        old,
        listed: panel.is_some(),
        spawn: SpawnArgs {
            board: args.board,
            cwd: Some(cwd),
            env: Vec::new(),
            label: worker.label.clone(),
            input: None,
            wait_after: true,
            focus: false,
            max_children: DEFAULT_MAX_CHILDREN,
            worktree: false,
            tier,
            ready: args.ready,
            task: Some(Task {
                text: TaskText::Literal(text),
                wait: args.wait.clone(),
            }),
            command,
        },
        extra,
    })
}

/// A journal `argv`: an array of strings.
fn strings(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// Which worker it was: its program, and its tier or else the model it
/// named (`claude, tier light`, `codex, model gpt-6-luna`, `agy`).
pub(crate) fn describe(argv: &[String], tier: Option<worktree::Tier>) -> String {
    let program = argv.first().map_or("", |program| {
        Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program)
    });
    match (tier, worktree::model_of(argv)) {
        (Some(tier), _) => format!("{program}, tier {}", tier.name()),
        (None, Some(model)) => format!("{program}, model {model}"),
        (None, None) => program.to_string(),
    }
}

/// What the new worker is told before the task: which worker was here, how
/// it stopped, that its changes are here to build on, and its report.
pub(crate) fn preface(
    old: u64,
    worker: &str,
    outcome: Option<&str>,
    report: Option<&str>,
) -> String {
    let stopped = match outcome {
        Some(reason) => format!("stopped (its last outcome: {reason})"),
        None => "stopped".to_string(),
    };
    let mut text = format!(
        "{PREFACE_START}{old}; {worker}) worked on this task in this directory and {stopped}. \
         Its changes, committed or not, are still here: check `git status` and `git log` \
         first and build on them rather than starting over."
    );
    if let Some(report) = report {
        text.push_str(&format!(" Its report: {report}"));
    }
    text
}

/// The text the new worker gets: the preface, then the task.
pub(crate) fn compose(preface: &str, task: &str) -> String {
    format!("{preface}{PREFACE_END}{task}")
}

/// Recover the task from its retry preface, after removing any timeline header.
pub(crate) fn strip_preface(text: &str) -> &str {
    if text.starts_with(PREFACE_START)
        && let Some((_, task)) = text.split_once(PREFACE_END)
    {
        return task;
    }
    text
}

/// `retry`'s steps once [`plan`] passed: `check` the spawn (its directory,
/// its program on `PATH`) before anything is closed, `close` the old pane
/// when it is still listed, then `spawn` the new worker. A failed step
/// stops the rest.
pub(crate) fn steps<S>(
    session: &mut S,
    plan: Plan,
    check: impl FnOnce(&mut S, &SpawnArgs) -> Result<(), Failure>,
    close: impl FnOnce(&mut S, u64) -> Result<(), Failure>,
    spawn: impl FnOnce(
        &mut S,
        SpawnArgs,
        serde_json::Map<String, serde_json::Value>,
    ) -> Result<i32, Failure>,
) -> Result<i32, Failure> {
    check(session, &plan.spawn)?;
    if plan.listed {
        close(session, plan.old)?;
    }
    spawn(session, plan.spawn, plan.extra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl::{EXIT_ERROR, EXIT_OK, WaitOptions, parse_args};
    use noa_ipc::{PanelAgent, PanelSpawn};
    use worktree::Tier;

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn retry_args(items: &[&str]) -> RetryArgs {
        let mut full = vec!["retry"];
        full.extend(items);
        match parse_args(&strs(&full)).unwrap() {
            super::super::Command::Retry(args) => args,
            other => panic!("retry expected, got {other:?}"),
        }
    }

    /// A light claude worker in a worktree, sent a task, that finished.
    fn worker(pane: u64) -> journal::Worker {
        journal::Worker {
            pane,
            label: Some("fix".to_string()),
            argv: Some(serde_json::json!(apply(
                &["claude", "--permission-mode", "auto"],
                Tier::Light
            ))),
            tier: Some("light".to_string()),
            cwd: Some("/r-wt/fix".to_string()),
            worktree: Some("/r-wt/fix".to_string()),
            branch: Some("orch/fix".to_string()),
            result: Some("/r-wt/fix/.noa/result-fix.md".to_string()),
            task: Some("fix the test".to_string()),
            last_outcome: Some(serde_json::json!({ "reason": "finished", "seq": 2, "ts": 9 })),
            ..journal::Worker::default()
        }
    }

    fn apply(command: &[&str], tier: Tier) -> Vec<String> {
        worktree::apply_tier(&strs(command), tier).unwrap()
    }

    fn listed(pane: u64, parent: u64, state: Option<&str>) -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(1),
            pane_id: WireId(pane),
            name: "claude".to_string(),
            cwd: "/r-wt/fix".to_string(),
            branch: None,
            process: Some("claude".to_string()),
            busy: false,
            attention: false,
            attachable: true,
            preview: Vec::new(),
            surface_id: None,
            agent: Some(PanelAgent {
                kind: Some("claude".to_string()),
                state: state.map(str::to_string),
                detail: None,
                seq: 2,
                blocked: None,
            }),
            exit: None,
            parent_pane_id: Some(WireId(parent)),
            label: Some("fix".to_string()),
            spawn: None,
        }
    }

    fn plan_of(
        args: &[&str],
        given: Option<&str>,
        workers: &[journal::Worker],
        panels: &[Panel],
    ) -> Result<Plan, String> {
        let mut items = vec!["12"];
        items.extend(args);
        plan(
            &retry_args(&items),
            given.map(str::to_string),
            workers,
            panels,
            1,
            true,
            |_| true,
        )
    }

    fn task_text(plan: &Plan) -> &str {
        match &plan.spawn.task.as_ref().unwrap().text {
            TaskText::Literal(text) => text,
            other => panic!("a literal task expected, got {other:?}"),
        }
    }

    #[test]
    fn retry_parses_its_pane_tier_task_and_the_tasks_wait_options() {
        let args = retry_args(&[
            "12",
            "--tier",
            "deep",
            "--task-file",
            "t.md",
            "--ready",
            "40",
            "--timeout",
            "600",
            "--settle",
            "5",
            "--for",
            "finished,error",
            "--no-board",
        ]);
        assert_eq!(args.pane, 12);
        assert_eq!(args.tier, Some(Tier::Deep));
        assert_eq!(args.task, Some(TaskText::File("t.md".to_string())));
        assert_eq!(args.ready, Some(std::time::Duration::from_secs(40)));
        assert_eq!(args.wait.timeout, Some(std::time::Duration::from_secs(600)));
        assert_eq!(args.wait.settle, Some(std::time::Duration::from_secs(5)));
        assert_eq!(args.wait.reasons.len(), 2);
        assert!(!args.board);

        let bare = retry_args(&["12"]);
        assert_eq!(bare.tier, None);
        assert_eq!(bare.task, None);
        assert_eq!(bare.ready, None);
        assert_eq!(bare.wait, WaitOptions::default());
        assert!(bare.board);
        assert_eq!(
            retry_args(&["12", "--task", "again"]).task,
            Some(TaskText::Literal("again".to_string()))
        );

        for bad in [
            &["retry"][..],
            &["retry", "12", "13"],
            &["retry", "x"],
            &["retry", "12", "--tier", "max"],
            &["retry", "12", "--task", "a", "--task-file", "f"],
            &["retry", "12", "--ready", "soon"],
            &["retry", "12", "--cwd", "/tmp"],
            &["retry", "12", "--label", "other"],
            &["retry", "12", "--for", "stalled"],
            &["retry", "12", "--", "claude"],
        ] {
            assert!(parse_args(&strs(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_retry_runs_the_old_command_in_its_directory_under_its_label() {
        let plan = plan_of(&[], None, &[worker(12)], &[listed(12, 1, Some("finished"))]).unwrap();
        assert_eq!(plan.old, 12);
        assert!(plan.listed);
        let spawn = &plan.spawn;
        // The tier's flags come off; the spawn puts the tier's on again.
        assert_eq!(
            spawn.command,
            strs(&["claude", "--permission-mode", "auto"])
        );
        assert_eq!(spawn.tier, Some(Tier::Light));
        assert_eq!(spawn.cwd.as_deref(), Some("/r-wt/fix"));
        assert_eq!(spawn.label.as_deref(), Some("fix"));
        // No new worktree: the old one is the cwd.
        assert!(!spawn.worktree);
        assert!(spawn.board);
        assert_eq!(plan.extra["retryOf"], "12");
        assert_eq!(plan.extra["worktree"], "/r-wt/fix");
        assert_eq!(plan.extra["branch"], "orch/fix");
        assert!(task_text(&plan).ends_with(&format!("{PREFACE_END}fix the test")));

        // A worker already gone is retried from the journal, and not closed.
        let gone = plan_of(&[], None, &[worker(12)], &[]).unwrap();
        assert!(!gone.listed);
        assert_eq!(gone.spawn.command, plan.spawn.command);
        // Neither is a pane another orchestrator holds under that id now.
        let taken = plan_of(&[], None, &[worker(12)], &[listed(12, 5, Some("running"))]).unwrap();
        assert!(!taken.listed);

        // A plain worker: no worktree or branch to record.
        let mut plain = worker(12);
        plain.worktree = None;
        plain.branch = None;
        let plan = plan_of(&[], None, &[plain], &[]).unwrap();
        assert!(plan.extra.get("worktree").is_none());
        assert!(plan.extra.get("branch").is_none());
    }

    #[test]
    fn a_retry_moves_to_the_tier_given_for_each_agent() {
        for program in ["claude", "codex", "agy"] {
            for old in [Tier::Light, Tier::Standard, Tier::Deep] {
                for new in [Tier::Light, Tier::Standard, Tier::Deep] {
                    let mut journaled = worker(12);
                    journaled.argv = Some(serde_json::json!(apply(&[program, "-x"], old)));
                    journaled.tier = Some(old.name().to_string());
                    let plan = plan_of(&["--tier", new.name()], None, &[journaled], &[]).unwrap();
                    assert_eq!(plan.spawn.command, strs(&[program, "-x"]));
                    assert_eq!(plan.spawn.tier, Some(new));
                }
            }
        }

        // A worker spawned without a tier gets one only when the command
        // picks no model of its own, as spawn --tier checks.
        let mut untiered = worker(12);
        untiered.tier = None;
        untiered.argv = Some(serde_json::json!(["codex", "--full-auto"]));
        let plan = plan_of(&["--tier", "standard"], None, &[untiered.clone()], &[]).unwrap();
        assert_eq!(plan.spawn.command, strs(&["codex", "--full-auto"]));
        assert_eq!(plan.spawn.tier, Some(Tier::Standard));
        let plan = plan_of(&[], None, &[untiered.clone()], &[]).unwrap();
        assert_eq!(plan.spawn.tier, None);

        untiered.argv = Some(serde_json::json!(["claude", "--model", "opus"]));
        let refused = plan_of(&["--tier", "deep"], None, &[untiered.clone()], &[]).unwrap_err();
        assert!(refused.contains("--tier conflicts"), "{refused}");
        // Without --tier, its own model stays.
        let plan = plan_of(&[], None, &[untiered], &[]).unwrap();
        assert_eq!(plan.spawn.command, strs(&["claude", "--model", "opus"]));
    }

    #[test]
    fn the_command_comes_from_the_panel_when_the_journal_has_none() {
        let mut bare = worker(12);
        bare.argv = None;
        bare.tier = None;
        let mut panel = listed(12, 1, Some("finished"));
        panel.spawn = Some(PanelSpawn {
            argv: strs(&["agy", "--yolo"]),
            started_at: 1,
        });
        let plan = plan_of(&[], None, &[bare.clone()], &[panel]).unwrap();
        assert_eq!(plan.spawn.command, strs(&["agy", "--yolo"]));

        // Neither the journal nor a listing knows one.
        let refused = plan_of(&[], None, &[bare], &[listed(12, 1, None)]).unwrap_err();
        assert!(refused.contains("without a command"), "{refused}");
    }

    #[test]
    fn a_worker_cannot_retry_its_own_worker_without_the_board() {
        // This pane (1) is itself a worker of pane 9.
        let panels = [listed(12, 1, Some("finished")), listed(1, 9, None)];
        let error = plan_of(&["--no-board"], None, &[worker(12)], &panels).unwrap_err();
        assert!(error.contains("--no-board"), "{error}");
        assert!(error.contains("Nothing was done"), "{error}");
        // With the board, or from an orchestrator, it plans as usual.
        assert!(plan_of(&[], None, &[worker(12)], &panels).is_ok());
        let alone = [listed(12, 1, Some("finished"))];
        assert!(plan_of(&["--no-board"], None, &[worker(12)], &alone).is_ok());
    }

    #[test]
    fn retry_refuses_what_it_cannot_start_again() {
        // Not a worker of this pane by the journal, listed or not.
        let stranger = plan(
            &retry_args(&["13"]),
            None,
            &[worker(12)],
            &[listed(13, 1, None)],
            1,
            true,
            |_| true,
        );
        let message = stranger.map(|_| ()).unwrap_err();
        assert!(message.contains("pane 13 is not a worker"), "{message}");
        let message = plan_of(&[], None, &[], &[]).unwrap_err();
        assert!(message.starts_with("pane 12 is not a worker"), "{message}");

        // Still working.
        let message =
            plan_of(&[], None, &[worker(12)], &[listed(12, 1, Some("running"))]).unwrap_err();
        assert!(message.contains("is working"), "{message}");
        assert!(message.contains("close it first"), "{message}");
        // Between turns, waiting, or stopped by an error: retried.
        for state in [
            None,
            Some("finished"),
            Some("permission"),
            Some("input"),
            Some("error"),
        ] {
            assert!(plan_of(&[], None, &[worker(12)], &[listed(12, 1, state)]).is_ok());
        }

        // Not an agent.
        let mut command = worker(12);
        command.tier = None;
        command.argv = Some(serde_json::json!(["cargo", "test"]));
        let message = plan_of(&[], None, &[command], &[]).unwrap_err();
        assert!(
            message.contains("only starts claude, codex or agy"),
            "{message}"
        );

        // The tier's flags are not where spawn --tier put them.
        let mut moved = worker(12);
        moved.argv = Some(serde_json::json!(["claude", "-p"]));
        assert!(plan_of(&[], None, &[moved], &[]).is_err());

        // No directory known.
        let mut nowhere = worker(12);
        nowhere.cwd = None;
        assert!(plan_of(&[], None, &[nowhere], &[]).is_err());
    }

    #[test]
    fn ac4_retry_checks_the_encoded_header_and_preface_while_planning() {
        let workers = [worker(12)];
        let empty = plan_of(&[], Some(""), &workers, &[]).unwrap();
        let wire = crate::ctl::run_attr::with_longest_header(task_text(&empty));
        let overhead = serde_json::to_string(&wire).unwrap().len();
        let body = "x".repeat(noa_ipc::MAX_SEND_TEXT_BYTES - overhead);
        assert!(plan_of(&[], Some(&body), &workers, &[]).is_ok());
        let over = body + "x";
        let error = plan_of(&[], Some(&over), &workers, &[]).unwrap_err();
        assert!(error.contains("KiB"), "{error}");
        let args = retry_args(&["12"]);
        assert!(plan(&args, Some(over), &workers, &[], 1, false, |_| true).is_ok());
    }

    #[test]
    fn a_task_not_kept_whole_needs_a_task_file() {
        let mut cut = worker(12);
        cut.task = None;
        let message = plan_of(&[], None, &[cut.clone()], &[]).unwrap_err();
        assert!(message.contains("--task-file"), "{message}");
        assert!(message.contains("64 KiB"), "{message}");

        // Given, it is used, preface and all.
        let plan = plan_of(&[], Some("the whole task"), &[cut], &[]).unwrap();
        assert!(task_text(&plan).starts_with(PREFACE_START));
        assert!(task_text(&plan).ends_with("the whole task"));

        // Too large for one send, even before anything is closed.
        let huge = "\n".repeat(noa_ipc::MAX_SEND_TEXT_BYTES / 2);
        let message = plan_of(&[], Some(&huge), &[worker(12)], &[]).unwrap_err();
        assert!(message.contains("Nothing was sent"), "{message}");
    }

    #[test]
    fn the_preface_names_the_worker_its_outcome_and_its_report() {
        assert_eq!(
            preface(
                12,
                "claude, tier light",
                Some("finished"),
                Some("/w/.noa/result-fix.md")
            ),
            "Retry: a previous worker (pane 12; claude, tier light) worked on this task in this \
             directory and stopped (its last outcome: finished). Its changes, committed or not, \
             are still here: check `git status` and `git log` first and build on them rather \
             than starting over. Its report: /w/.noa/result-fix.md"
        );
        let bare = preface(3, "agy", None, None);
        assert!(bare.contains("(pane 3; agy) worked"), "{bare}");
        assert!(bare.contains("and stopped. Its changes"), "{bare}");
        assert!(!bare.contains("report"), "{bare}");

        assert_eq!(
            describe(&apply(&["claude"], Tier::Deep), Some(Tier::Deep)),
            "claude, tier deep"
        );
        assert_eq!(
            describe(&strs(&["/usr/local/bin/codex", "-m", "o9"]), None),
            "codex, model o9"
        );
        assert_eq!(describe(&strs(&["agy"]), None), "agy");
    }

    #[test]
    fn the_report_is_named_only_when_written_and_the_outcome_when_known() {
        let args = retry_args(&["12"]);
        let with = |written: bool, journaled: journal::Worker| {
            let plan = plan(&args, None, &[journaled], &[], 1, true, |path| {
                assert_eq!(path, Path::new("/r-wt/fix/.noa/result-fix.md"));
                written
            })
            .unwrap();
            task_text(&plan).to_string()
        };
        assert!(with(true, worker(12)).contains("Its report: /r-wt/fix/.noa/result-fix.md"));
        assert!(!with(false, worker(12)).contains("Its report"));
        let mut unknown = worker(12);
        unknown.last_outcome = None;
        assert!(with(true, unknown).contains("and stopped. Its"));
        assert!(with(true, worker(12)).contains("(its last outcome: finished)"));
    }

    // A retry of a retry sends the task once, under the newest preface.
    #[test]
    fn a_retried_task_loses_its_earlier_preface() {
        let first = compose(
            &preface(9, "claude, tier light", None, None),
            "fix the test",
        );
        assert_eq!(strip_preface(&first), "fix the test");
        let mut again = worker(12);
        again.task = Some(first);
        let plan = plan_of(&[], None, &[again], &[]).map(|plan| task_text(&plan).to_string());
        let text = plan.unwrap();
        assert!(
            text.starts_with("Retry: a previous worker (pane 12;"),
            "{text}"
        );
        assert_eq!(text.matches(PREFACE_START).count(), 1, "{text}");
        assert!(text.ends_with(&format!("{PREFACE_END}fix the test")));

        // A task that merely contains the separator keeps it.
        let task = "a\n\n---\n\nb";
        assert_eq!(strip_preface(task), task);
    }

    #[test]
    fn ac4_strip_header_then_preface_on_retry() {
        let task = "fix the test\n\n---\n\nkeep this part";
        let mut again = worker(12);
        again.task = Some(crate::ctl::run_attr::with_longest_header(&compose(
            &preface(9, "claude", None, None),
            task,
        )));
        let plan = plan_of(&[], None, &[again], &[]).unwrap();
        let text = task_text(&plan);
        assert_eq!(text.matches(PREFACE_START).count(), 1);
        assert!(!text.contains("[noa-timeline"));
        assert_eq!(strip_preface(text), task);
    }

    /// [`steps`] with each step logged; `fail` names the step that fails.
    fn run_steps(listed: bool, fail: Option<&str>) -> (Result<i32, Failure>, Vec<String>) {
        let mut plan = plan_of(&[], None, &[worker(12)], &[]).unwrap();
        plan.listed = listed;
        let mut log: Vec<String> = Vec::new();
        let failing = |step: &str| fail == Some(step);
        let result = steps(
            &mut log,
            plan,
            |log, spawn| {
                log.push(format!("check {}", spawn.cwd.as_deref().unwrap()));
                if failing("check") {
                    return Err(Failure::error("--cwd \"/r-wt/fix\" is not a directory"));
                }
                Ok(())
            },
            |log, old| {
                log.push(format!("close {old}"));
                if failing("close") {
                    return Err(Failure::error("pane 12 was not closed"));
                }
                Ok(())
            },
            |log, spawn, extra| {
                log.push(format!(
                    "spawn {:?} retryOf={}",
                    spawn.command, extra["retryOf"]
                ));
                Ok(EXIT_OK)
            },
        );
        (result, log)
    }

    #[test]
    fn the_old_pane_closes_after_the_checks_and_before_the_spawn() {
        let (result, log) = run_steps(true, None);
        assert_eq!(result.ok(), Some(EXIT_OK));
        assert_eq!(
            log,
            [
                "check /r-wt/fix",
                "close 12",
                "spawn [\"claude\", \"--permission-mode\", \"auto\"] retryOf=\"12\"",
            ]
        );

        // Already gone: nothing to close.
        let (_, log) = run_steps(false, None);
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|step| !step.starts_with("close")));

        // A failed check closes nothing; a failed close spawns nothing.
        let (result, log) = run_steps(true, Some("check"));
        assert_eq!(result.err().map(|failure| failure.code), Some(EXIT_ERROR));
        assert_eq!(log, ["check /r-wt/fix"]);
        let (result, log) = run_steps(true, Some("close"));
        assert!(result.is_err());
        assert_eq!(log, ["check /r-wt/fix", "close 12"]);
    }
}
