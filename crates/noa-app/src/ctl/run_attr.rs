//! Board Timeline run attribution (`docs/specs/board-timeline.md` L2-6,
//! AC-4): `spawn --task`, `send --new-run` and `retry` register a run with
//! `noa.timeline.registerRun` after the pane exists and before the task is
//! typed, and the run's ids reach the worker in a one-line header typed
//! before the task. No environment variable carries a run id.

use std::borrow::Cow;
use std::time::Duration;

use noa_ipc::timeline::{
    ContextParams, ContextResult, IdKind, PublishOwner, PublishPolicy, RegisterRunParams,
    RegisterRunResult, RunInfo, TimelineErrorKind, valid_id,
};
use noa_ipc::{Client, ClientError, WireId};
use rand::RngCore;

use super::journal;

/// The timeline calls run attribution makes: a [`Client`], or a fake in
/// tests.
pub(crate) trait TimelineApi {
    fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, ClientError>;
    fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError>;
    /// The server advertised `boardTimelineV1`.
    fn supports(&self) -> bool;
}

impl TimelineApi for Client {
    fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, ClientError> {
        self.timeline_register_run(p)
    }

    fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError> {
        self.timeline_context(p)
    }

    fn supports(&self) -> bool {
        self.supports_board_timeline()
    }
}

/// A registered run, as the worker's header and this pane's journal carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunAttr {
    pub board_id: String,
    pub run_id: String,
    pub worker_id: Option<String>,
    pub producer_id: String,
    pub retry_of: Option<String>,
}

/// Why a run is registered.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::enum_variant_names,
    reason = "the variants name the commands (spawn --task, send --new-run, retry) as the \
              frozen contract (board-timeline L2-6) has them"
)]
pub(crate) enum NewRun {
    SpawnTask,
    SendNewRun,
    /// `retry`: the old worker's latest run, when known.
    Retry {
        of_run: Option<String>,
    },
}

/// Registers a run for `worker_pane` on the board of `orchestrator` (D9),
/// sending `request_id` unchanged on every attempt. `Ok(None)`: the server
/// has no timeline (nothing is registered and no header is typed).
/// `Err`: the failure's kind, for the caller's warning.
pub(crate) fn register(
    api: &mut dyn TimelineApi,
    orchestrator: Option<u64>,
    worker_pane: u64,
    new: NewRun,
    request_id: &str,
) -> Result<Option<RunAttr>, String> {
    if !api.supports() {
        return Ok(None);
    }
    let params = RegisterRunParams {
        request_id: request_id.to_owned(),
        orchestrator_pane_id: orchestrator.map(WireId),
        worker_pane_id: Some(WireId(worker_pane)),
        retry_of_run_id: match new {
            NewRun::Retry { of_run } => of_run,
            NewRun::SpawnTask | NewRun::SendNewRun => None,
        },
        publish_policy: PublishPolicy {
            owner: PublishOwner::Worker,
            producer_id: None,
        },
        board_id: None,
        board_name: None,
        label: None,
        role: None,
        task_ref: None,
    };
    let mut delays = [200, 400, 800, 1600].into_iter();
    loop {
        let kind = match api.register_run(&params) {
            Ok(run) => {
                if !valid_id(IdKind::Board, &run.board_id) || !valid_id(IdKind::Run, &run.run_id) {
                    return Err("invalid registration response".into());
                }
                return Ok(Some(RunAttr {
                    board_id: run.board_id,
                    run_id: run.run_id,
                    worker_id: run.worker_id,
                    producer_id: run.producer_id,
                    retry_of: params.retry_of_run_id,
                }));
            }
            Err(ClientError::Rpc { code, .. }) => {
                TimelineErrorKind::from_code(code).ok_or_else(|| format!("rpc error {code}"))?
            }
            Err(ClientError::Timeout(_)) => TimelineErrorKind::OutcomeUnknown,
            Err(_) => return Err("registration was not acknowledged".into()),
        };
        if matches!(
            kind,
            TimelineErrorKind::Busy | TimelineErrorKind::OutcomeUnknown
        ) && let Some(delay) = delays.next()
        {
            std::thread::sleep(Duration::from_millis(delay));
        } else {
            return Err(kind.to_string());
        }
    }
}

/// The old worker's latest run id for `retry`: from this pane's journal,
/// else from `context(old pane)` only while it is listed as our worker.
pub(crate) fn retry_of(
    api: &mut dyn TimelineApi,
    workers: &[journal::Worker],
    old_pane: u64,
    listed: bool,
) -> Option<String> {
    if let Some(run) = workers
        .iter()
        .find(|w| w.pane == old_pane)
        .and_then(|w| w.run_id.clone())
    {
        return Some(run);
    }
    if !listed || !api.supports() {
        return None;
    }
    let me = api
        .context(&ContextParams {
            pane_id: Some(WireId(old_pane)),
            board_id: None,
        })
        .ok()?
        .me?;
    if me.pane_id != WireId(old_pane) || me.role != "worker" {
        return None;
    }
    me.runs.last().map(|run| run.run_id.clone())
}

/// `ctl-` + 16 random lowercase hex digits, minted once per command.
pub(crate) fn new_request_id() -> String {
    format!("ctl-{:016x}", rand::thread_rng().next_u64())
}

/// The one-line header typed before a registered run's task.
pub(crate) fn header(attr: &RunAttr) -> String {
    header_line(&attr.board_id, &attr.run_id)
}

fn header_line(board: &str, run: &str) -> String {
    format!(
        "[noa-timeline board={board} run={run}] When done, publish a summary: \
         \"$NOA_BIN\" +ctl timeline post --run {run} --file <summary.json>",
    )
}

/// `text` with the header and a blank line before it; `text` unchanged
/// without a registered run.
pub(crate) fn with_header(attr: Option<&RunAttr>, text: &str) -> String {
    match attr {
        Some(attr) => format!("{}\n\n{text}", header(attr)),
        None => text.to_string(),
    }
}

/// `text` under a header as long as any registered run's (board and run
/// ids are fixed-length), for checking a send's size before the run exists.
pub(crate) fn with_longest_header(text: &str) -> String {
    let attr = RunAttr {
        board_id: format!("bd-{:016x}", 0),
        run_id: format!("rn-{:016x}", 0),
        worker_id: None,
        producer_id: String::new(),
        retry_of: None,
    };
    with_header(Some(&attr), text)
}

/// The payload to validate before registration. Only a timeline-capable
/// server can add the fixed-length header.
pub(crate) fn prospective_text(text: &str, supports_timeline: bool) -> Cow<'_, str> {
    if supports_timeline {
        Cow::Owned(with_longest_header(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// `text` without a leading header (and its blank line), for showing a
/// task; `retry::strip_preface` goes after it.
pub(crate) fn strip_header(text: &str) -> &str {
    if let Some((line, task)) = text.split_once("\n\n")
        && let Some(fields) = line.strip_prefix("[noa-timeline board=")
        && let Some((board, tail)) = fields.split_once(" run=")
        && let Some((run, _)) = tail.split_once(']')
        && valid_id(IdKind::Board, board)
        && valid_id(IdKind::Run, run)
        && line == header_line(board, run)
    {
        return task;
    }
    text
}

/// The run's ids for this pane's journal: `{"boardId","runId","workerId"}`.
pub(crate) fn journal_fields(attr: &RunAttr) -> serde_json::Value {
    serde_json::json!({
        "boardId": attr.board_id,
        "runId": attr.run_id,
        "workerId": attr.worker_id,
    })
}

/// The run `timeline post` posts under: `explicit`, else the one run in
/// `runs`; none or several is refused with what to do.
pub(crate) fn choose_run<'a>(
    runs: &'a [RunInfo],
    explicit: Option<&'a str>,
) -> Result<&'a str, String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    match runs {
        [run] => Ok(&run.run_id),
        [] => Err("no registered run (pass --run, or --from-result with --take-over from the orchestrator)".into()),
        many => {
            let ids: Vec<_> = many.iter().map(|run| run.run_id.as_str()).collect();
            Err(format!("multiple registered runs on this pane; pass --run with one of: {}", ids.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl::{
        Command, Failure, SendArgs, SendTarget, new_run_text, parse_args, send_steps,
    };
    use noa_ipc::Panel;
    use noa_ipc::timeline::{ContextSelf, Seq};
    use std::collections::VecDeque;

    fn attr() -> RunAttr {
        RunAttr {
            board_id: "bd-0123456789abcdef".to_string(),
            run_id: "rn-0123456789abcdef".to_string(),
            worker_id: Some("wk-0123456789abcdef".to_string()),
            producer_id: "wk-0123456789abcdef".to_string(),
            retry_of: None,
        }
    }

    #[test]
    fn header_is_the_spec_line() {
        assert_eq!(
            header(&attr()),
            "[noa-timeline board=bd-0123456789abcdef run=rn-0123456789abcdef] When done, \
             publish a summary: \"$NOA_BIN\" +ctl timeline post --run rn-0123456789abcdef \
             --file <summary.json>"
        );
        assert_eq!(
            with_header(Some(&attr()), "do it"),
            format!("{}\n\ndo it", header(&attr()))
        );
    }

    #[test]
    fn the_longest_header_is_as_long_as_a_run_header() {
        assert_eq!(
            with_longest_header("do it").len(),
            with_header(Some(&attr()), "do it").len()
        );
    }

    #[test]
    fn no_run_types_the_text_unchanged() {
        assert_eq!(with_header(None, "do it\n"), "do it\n");
    }

    #[test]
    fn request_ids_are_ctl_and_16_hex() {
        let id = new_request_id();
        let hex = id.strip_prefix("ctl-").expect("ctl- prefix");
        assert_eq!(hex.len(), 16);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert!(noa_ipc::timeline::valid_token(&id));
    }

    #[test]
    fn journal_fields_name_board_run_and_worker() {
        assert_eq!(
            journal_fields(&attr()),
            serde_json::json!({
                "boardId": "bd-0123456789abcdef",
                "runId": "rn-0123456789abcdef",
                "workerId": "wk-0123456789abcdef",
            })
        );
    }

    #[derive(Default)]
    struct FakeApi {
        unsupported: bool,
        calls: Vec<&'static str>,
        registrations: Vec<RegisterRunParams>,
        errors: VecDeque<TimelineErrorKind>,
        me: Option<ContextSelf>,
        typed: Vec<String>,
    }

    impl TimelineApi for FakeApi {
        fn supports(&self) -> bool {
            !self.unsupported
        }

        fn register_run(
            &mut self,
            p: &RegisterRunParams,
        ) -> Result<RegisterRunResult, ClientError> {
            self.calls.push("registerRun");
            self.registrations.push(p.clone());
            if let Some(kind) = self.errors.pop_front() {
                return Err(ClientError::Rpc {
                    code: kind.code().code(),
                    message: String::new(),
                });
            }
            let run = attr();
            Ok(RegisterRunResult {
                board_id: run.board_id,
                run_id: run.run_id,
                worker_id: run.worker_id,
                producer_id: run.producer_id,
                created: self.registrations.len() == 1,
            })
        }

        fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError> {
            self.calls.push("context");
            assert_eq!(p.pane_id, Some(WireId(12)));
            assert_eq!(p.board_id, None);
            Ok(ContextResult {
                board_id: attr().board_id,
                me: self.me.clone(),
                archived: false,
                head_seq: Seq(0),
                name: String::new(),
                server_instance_id: String::new(),
                workers: vec![],
            })
        }
    }

    impl SendTarget for FakeApi {
        fn panel(&mut self, pane: u64) -> Result<Option<Panel>, Failure> {
            Ok(Some(
                serde_json::from_value(serde_json::json!({
                    "paneId": WireId(pane), "windowId": "1", "windowGroupId": "1",
                    "name": "worker", "cwd": "/", "attention": false, "busy": false, "preview": []
                }))
                .unwrap(),
            ))
        }

        fn subscribe(&mut self, _: u64) -> Result<(), Failure> {
            Ok(())
        }
        fn pause(&mut self, _: Duration) {}
        fn paste_ready(&mut self, _: u64) -> Result<bool, Failure> {
            Ok(true)
        }
        fn type_text(&mut self, _: u64, text: &str, _: bool) -> Result<(), Failure> {
            self.calls.push("sendText");
            self.typed.push(text.to_owned());
            Ok(())
        }
    }

    fn send(flags: &[&str]) -> SendArgs {
        let args = ["send", "8"]
            .into_iter()
            .chain(flags.iter().copied())
            .chain(["task"])
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let Command::Send(args) = parse_args(&args).unwrap() else {
            unreachable!()
        };
        args
    }

    fn register_worker(api: &mut FakeApi, new: NewRun) -> Result<Option<RunAttr>, String> {
        register(api, Some(4), 8, new, "ctl-0123456789abcdef")
    }

    fn run(id: &str) -> RunInfo {
        RunInfo {
            run_id: id.into(),
            board_id: attr().board_id,
            worker_id: attr().worker_id,
            created_at: "2026-11-26T00:00:00Z".into(),
            retry_of_run_id: None,
            summary_post_id: None,
            task_ref: None,
            publish_policy: PublishPolicy {
                owner: PublishOwner::Worker,
                producer_id: None,
            },
        }
    }

    #[test]
    fn ac4_spawn_task_registers_before_typing() {
        let mut api = FakeApi::default();
        let registered = register_worker(&mut api, NewRun::SpawnTask).unwrap();
        assert_eq!(registered, Some(attr()));
        let text = with_header(registered.as_ref(), "task");
        send_steps(&mut api, &send(&[]), &text).unwrap();
        assert_eq!(api.calls, ["registerRun", "sendText"]);
        assert_eq!(api.typed, [with_header(Some(&attr()), "task")]);
        let p = &api.registrations[0];
        assert_eq!(p.orchestrator_pane_id, Some(WireId(4)));
        assert_eq!(p.worker_pane_id, Some(WireId(8)));
        assert_eq!(p.publish_policy.owner, PublishOwner::Worker);
        assert_eq!(p.publish_policy.producer_id, None);
        assert_eq!(p.retry_of_run_id, None);
        assert_eq!(p.board_id, None);
    }

    #[test]
    fn ac4_send_new_run_registers() {
        let args = send(&["--new-run"]);
        assert!(args.new_run);
        let mut api = FakeApi::default();
        let text = new_run_text(&mut api, &args, "task", true, |api| {
            register_worker(api, NewRun::SendNewRun).unwrap()
        })
        .unwrap();
        send_steps(&mut api, &args, &text).unwrap();
        assert_eq!(api.calls, ["registerRun", "sendText"]);
        assert_eq!(api.typed, [with_header(Some(&attr()), "task")]);
    }

    #[test]
    fn ac4_answer_and_follow_up_do_not_register() {
        for flags in [&[][..], &["--answer"], &["--queue"], &["--replace"]] {
            let args = send(flags);
            assert!(!args.new_run);
            let mut api = FakeApi::default();
            send_steps(&mut api, &args, "task").unwrap();
            assert!(!api.calls.contains(&"registerRun"));
            assert!(api.typed.iter().any(|s| s == "task"));
        }
    }

    #[test]
    fn ac4_old_server_accepts_a_new_run_body_at_the_send_limit() {
        let mut api = FakeApi {
            unsupported: true,
            ..Default::default()
        };
        let body = "\n".repeat(noa_ipc::MAX_SEND_TEXT_BYTES / 2 - 1);
        let args = send(&["--new-run"]);
        let text = new_run_text(&mut api, &args, &body, false, |_| {
            panic!("a server without the capability cannot register a run")
        })
        .unwrap();
        send_steps(&mut api, &args, &text).unwrap();
        assert_eq!(api.typed, [body]);
        assert_eq!(api.calls, ["sendText"]);
    }

    #[test]
    fn ac4_old_server_unchanged() {
        let mut api = FakeApi {
            unsupported: true,
            ..Default::default()
        };
        let task = "\n  original\n作業\n";
        let of_run = retry_of(&mut api, &[], 12, true);
        for new in [
            NewRun::SpawnTask,
            NewRun::SendNewRun,
            NewRun::Retry { of_run },
        ] {
            let registered = register_worker(&mut api, new).unwrap();
            let text = with_header(registered.as_ref(), task);
            send_steps(&mut api, &send(&[]), &text).unwrap();
        }
        assert_eq!(api.typed, [task; 3]);
        assert_eq!(api.calls, ["sendText"; 3]);
    }

    #[test]
    fn ac4_request_id_stable_across_retries() {
        let mut api = FakeApi {
            errors: [TimelineErrorKind::OutcomeUnknown, TimelineErrorKind::Busy].into(),
            ..Default::default()
        };
        let previous = Some("rn-0000000000000001".to_owned());
        let registered = register_worker(
            &mut api,
            NewRun::Retry {
                of_run: previous.clone(),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(registered.retry_of, previous);
        let first = &api.registrations[0];
        assert_eq!(first.request_id, "ctl-0123456789abcdef");
        assert_eq!(first.retry_of_run_id, previous);
        assert_eq!(api.registrations, vec![first.clone(); 3]);
    }

    #[test]
    fn ac4_registration_stops_at_five_attempts_or_a_terminal_error() {
        for (kind, count) in [
            (TimelineErrorKind::Busy, 5),
            (TimelineErrorKind::Conflict, 1),
        ] {
            let mut api = FakeApi {
                errors: vec![kind; count].into(),
                ..Default::default()
            };
            assert_eq!(
                register_worker(&mut api, NewRun::SpawnTask),
                Err(kind.to_string())
            );
            assert_eq!(api.registrations.len(), count);
        }
    }

    #[test]
    fn ac4_retry_of_a_reused_pane_uses_only_the_journal() {
        let mut worker = journal::Worker {
            pane: 12,
            argv: Some(serde_json::json!(["claude"])),
            cwd: Some("/repo".into()),
            ..Default::default()
        };
        let Command::Retry(args) = parse_args(&["retry".into(), "12".into()]).unwrap() else {
            unreachable!()
        };
        let mut api = FakeApi {
            me: Some(ContextSelf {
                pane_id: WireId(12),
                role: "worker".into(),
                worker_id: attr().worker_id,
                runs: vec![run("rn-0000000000000099")],
            }),
            ..Default::default()
        };
        let mut other = SendTarget::panel(&mut api, 12).unwrap().unwrap();
        other.parent_pane_id = Some(WireId(99));
        for id in [None, Some("rn-0000000000000001")] {
            worker.run_id = id.map(str::to_owned);
            let workers = std::slice::from_ref(&worker);
            let plan = crate::ctl::retry::plan(
                &args,
                Some("task".into()),
                workers,
                std::slice::from_ref(&other),
                4,
                true,
                |_| false,
            )
            .unwrap();
            assert!(!plan.listed);
            assert_eq!(
                retry_of(&mut api, workers, plan.old, plan.listed).as_deref(),
                id
            );
            assert!(api.calls.is_empty(), "a reused pane must never be queried");
        }
    }

    #[test]
    fn ac4_retry_prefers_journal_then_old_pane_context() {
        let old = "rn-0000000000000001";
        let latest = "rn-0000000000000002";
        let mut api = FakeApi {
            me: Some(ContextSelf {
                pane_id: WireId(12),
                role: "worker".into(),
                worker_id: attr().worker_id,
                runs: vec![run(old), run(latest)],
            }),
            ..Default::default()
        };
        let worker = journal::Worker {
            pane: 12,
            run_id: Some(old.into()),
            ..Default::default()
        };
        assert_eq!(
            retry_of(&mut api, &[worker], 12, true).as_deref(),
            Some(old)
        );
        assert!(api.calls.is_empty());
        assert_eq!(retry_of(&mut api, &[], 12, true).as_deref(), Some(latest));
        api.me = None;
        assert_eq!(retry_of(&mut api, &[], 12, true), None);
        assert_eq!(api.calls, ["context", "context"]);
    }

    #[test]
    fn ac4_header_round_trip_preserves_task_bytes_and_lookalikes() {
        let task = "\n\n実装\n";
        assert_eq!(strip_header(&with_header(Some(&attr()), task)), task);
        let line = header(&attr());
        for text in [
            format!("instructions\n{line}\n\n{task}"),
            format!("{line} extra\n\n{task}"),
            format!("[noa-timeline board=mine run=one]\n\n{task}"),
            line,
        ] {
            assert_eq!(strip_header(&text), text);
        }
    }

    #[test]
    fn ac4_post_without_run_ambiguous_refused() {
        let a = "rn-0000000000000001";
        let b = "rn-0000000000000002";
        assert_eq!(
            choose_run(&[], None).unwrap_err(),
            "no registered run (pass --run, or --from-result with --take-over from the orchestrator)"
        );
        let runs = [run(a), run(b)];
        assert_eq!(choose_run(&runs[..1], None), Ok(a));
        let message = choose_run(&runs, None).unwrap_err();
        for hint in [a, b, "--run"] {
            assert!(message.contains(hint), "{message}");
        }
        assert_eq!(choose_run(&runs, Some(b)), Ok(b));
        assert_eq!(choose_run(&[], Some(a)), Ok(a));
    }
}
