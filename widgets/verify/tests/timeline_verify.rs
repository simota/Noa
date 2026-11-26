//! The verification's Board Timeline adapter
//! (`docs/specs/board-timeline.md` AC-5): candidates come from confirmed
//! judge outputs only, their keys survive a replay, a restart re-sends a
//! staged envelope exactly once, and a failed post leaves the verification
//! alone.

#[allow(dead_code)]
#[path = "golden/scenario.rs"]
mod scenario;

// Under `timeline::` so `cargo test timeline` (the unit's check) selects them.
mod timeline {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use noa_ipc::ClientError;
    use noa_ipc::timeline::{
        CheckResult, MAX_POST_PARAMS_BYTES, Outcome, PostKind, PostParams, PostResult,
        RegisterRunParams, RegisterRunResult, Seq, TimelineErrorKind, canon_bytes,
    };
    use noa_ipc::timeline_outbox::PostSink;
    use verify::model::reduce;
    use verify::model::types::{Decision, DefectOp, Event, JudgeOutput, Phase, RoundKind, Session};
    use verify::timeline::{self as tl, Publisher, RunSpec, Status, TimelineApi};

    const BOARD_PANE: u64 = 7;
    const SESSION: &str = "ver-golden";

    /// What the fake answers for its `seq`-th stored post.
    fn published(seq: u64) -> Status {
        Status::Published {
            seq,
            post_id: format!("po-{seq:016x}"),
        }
    }

    fn finished() -> Session {
        super::scenario::run().1
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "verify-timeline-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What the fake does with the next post.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Next {
        Answer,
        /// Stores the post, then loses the acknowledgement.
        TimeOutAfterStoring,
        /// Refuses it (`io_error`), storing nothing.
        Refuse,
    }

    /// A Noa with the timeline: key A and key B as the store keeps them.
    struct FakeNoa {
        supports: bool,
        next: Next,
        sent: Vec<PostParams>,
        stored: Vec<(PostParams, String)>,
        by_key: HashMap<(String, String, String), usize>,
        registered: Vec<RegisterRunParams>,
    }

    impl FakeNoa {
        fn new() -> Self {
            Self {
                supports: true,
                next: Next::Answer,
                sent: Vec::new(),
                stored: Vec::new(),
                by_key: HashMap::new(),
                registered: Vec::new(),
            }
        }
    }

    impl PostSink for FakeNoa {
        fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
            p.post.validate().map_err(|e| ClientError::Rpc {
                code: e.code().code(),
                message: e.message,
            })?;
            self.sent.push(p.clone());
            let next = std::mem::replace(&mut self.next, Next::Answer);
            if next == Next::Refuse {
                return Err(ClientError::Rpc {
                    code: TimelineErrorKind::IoError.code().code(),
                    message: "disk full".into(),
                });
            }
            let key = (
                p.board_id.clone(),
                p.producer_id.clone(),
                p.request_id.clone(),
            );
            let canon =
                |q: &PostParams| canon_bytes(q.run_id.as_deref(), Some(&q.producer_id), &q.post);
            let (index, duplicate) = match self.by_key.get(&key) {
                Some(&index) if canon(&self.stored[index].0) == canon(p) => (index, true),
                Some(_) => {
                    return Err(ClientError::Rpc {
                        code: TimelineErrorKind::Conflict.code().code(),
                        message: "conflict".into(),
                    });
                }
                None => {
                    let post_id = format!("po-{:016x}", self.stored.len() + 1);
                    self.stored.push((p.clone(), post_id));
                    self.by_key.insert(key, self.stored.len() - 1);
                    (self.stored.len() - 1, false)
                }
            };
            if next == Next::TimeOutAfterStoring {
                return Err(ClientError::Timeout("noa.timeline.post".into()));
            }
            Ok(PostResult {
                created_at: "2026-11-26T00:00:00Z".into(),
                duplicate,
                post_id: self.stored[index].1.clone(),
                seq: Seq(index as u64 + 1),
            })
        }
    }

    impl TimelineApi for FakeNoa {
        fn supports(&self) -> bool {
            self.supports
        }

        fn register_run(
            &mut self,
            p: &RegisterRunParams,
        ) -> Result<RegisterRunResult, ClientError> {
            self.registered.push(p.clone());
            Ok(RegisterRunResult {
                board_id: "bd-00000000000000aa".into(),
                created: self.registered.len() == 1,
                producer_id: p.publish_policy.producer_id.clone().unwrap(),
                run_id: "rn-00000000000000bb".into(),
                worker_id: None,
            })
        }
    }

    #[test]
    fn ac5_candidates_from_confirmed_events_only() {
        let session = finished();
        let candidates = tl::candidates(&session);
        // Round 1 was ruled without a verdict; round 2 gave one.
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.event_id.as_str())
                .collect::<Vec<_>>(),
            ["verdict-r2"]
        );
        let c = &candidates[0];
        assert!(!c.supersedes_previous);
        assert_eq!(c.post.kind, PostKind::Summary);
        assert_eq!(c.post.outcome, Outcome::Succeeded, "pass_with_issues");
        let round = session.round(2).unwrap().outcome.as_ref().unwrap();
        assert_eq!(c.post.checks.len(), 1 + round.rulings.len());
        assert!(
            c.post
                .checks
                .iter()
                .any(|k| k.result == CheckResult::Failed)
        );
        assert!(
            c.post
                .checks
                .iter()
                .all(|k| k.reported_by == "verify:ver-golden")
        );
        let issues: Vec<_> = c
            .post
            .open_issues
            .iter()
            .map(|i| i.text.split(':').next().unwrap().to_owned())
            .collect();
        assert_eq!(issues, ["AC-3", "D1"]);
        assert_eq!(c.post.source_ref.namespace, "verify");
        assert_eq!(c.post.source_ref.event_id, "verdict-r2");
        c.post.validate().unwrap();

        // The first round had rulings but no verdict.
        let mut unruled = session;
        unruled.rounds.truncate(1);
        assert!(tl::candidates(&unruled).is_empty());
        assert_eq!(tl::confirmed(&unruled), 0);
    }

    fn accept_third_round(session: &mut Session, output: JudgeOutput) {
        reduce::apply(
            session,
            &[
                Event::RoundsExtended {
                    max_rounds: 3,
                    at: 100,
                },
                Event::RoundStarted {
                    no: 3,
                    kind: RoundKind::Cross,
                    assignments: Default::default(),
                    at: 101,
                },
                Event::JudgeAccepted { round: 3, output },
                Event::PhaseChanged {
                    round: 3,
                    phase: Phase::Closed,
                },
            ],
        )
        .unwrap();
        reduce::apply(session, &[Event::SessionClosed]).unwrap();
    }

    #[test]
    fn ac5_unstaged_verdict_keeps_its_accepted_round() {
        let mut session = finished();
        let first_report = verify::model::export::report_markdown(&session, None);
        let first = tl::candidates(&session).remove(0).post;
        let mut output = session.round(2).unwrap().outcome.clone().unwrap();
        output.defect_ops = vec![DefectOp::Update {
            id: "D1".into(),
            claim: Some("revised defect".into()),
            repro: None,
            location: None,
            severity: None,
            category: None,
            confidence: None,
            status: None,
            rationale: None,
            fix: None,
        }];
        output.verdict.as_mut().unwrap().decision = Decision::Fail;
        accept_third_round(&mut session, output);

        let root = temp("unstaged");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &tl::candidates(&session)),
            published(2)
        );
        assert_eq!(
            noa.sent[0].post.open_issues, first.open_issues,
            "later defect text leaked into round 2"
        );
        assert_eq!(noa.sent[0].post.body_markdown, first_report);
        assert!(
            noa.sent[1]
                .post
                .open_issues
                .iter()
                .any(|i| i.text.contains("revised defect"))
        );
        assert_eq!(
            noa.sent[1].post.body_markdown,
            verify::model::export::report_markdown(&session, None)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_accepted_verdict_succeeds_and_records_the_decision_in_checks() {
        for (decision, result) in [
            (Decision::Fail, CheckResult::Failed),
            (Decision::PassWithIssues, CheckResult::Passed),
            (Decision::Inconclusive, CheckResult::Inconclusive),
            (Decision::Pass, CheckResult::Passed),
        ] {
            let mut session = finished();
            let mut output = session.round(2).unwrap().outcome.clone().unwrap();
            output.defect_ops.clear();
            output.verdict.as_mut().unwrap().decision = decision;
            accept_third_round(&mut session, output);
            let post = tl::candidates(&session).pop().unwrap().post;
            assert_eq!(
                post.outcome,
                Outcome::Succeeded,
                "{decision:?} is an accepted verdict"
            );
            assert!(post.checks.iter().any(|check| check.name
                == format!("verify verdict: {}", decision.name())
                && check.result == result));
        }
    }

    #[test]
    fn ac5_request_ids_stable_across_replay() {
        let run = super::scenario::play();
        let live = run.engine.session();
        let round = serde_json::to_value(live.round(2).unwrap()).unwrap();
        assert!(round.get("verdict_post").is_none());
        let close = run
            .batches
            .iter()
            .position(|batch| batch.contains(&Event::SessionClosed))
            .unwrap();
        let mut replayed = reduce::replay(&run.batches[..close]).unwrap();
        assert!(replayed.verdict().is_some());
        assert_eq!(
            tl::confirmed(&replayed),
            0,
            "a log cut before close has no post yet"
        );
        for batch in &run.batches[close..] {
            reduce::apply(&mut replayed, batch).unwrap();
        }
        let ids = |s: &Session| {
            tl::candidates(s)
                .into_iter()
                .map(|c| tl::request_id(s.id.as_str(), &c.event_id))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(live), ["verify-ver-golden-verdict-r2"]);
        assert_eq!(ids(&replayed), ids(live));
        assert_eq!(tl::candidates(&replayed), tl::candidates(live));
    }

    #[test]
    fn ac5_restart_resends_exactly_once() {
        let root = temp("restart");
        let candidates = tl::candidates(&finished());
        let mut noa = FakeNoa::new();
        noa.next = Next::TimeOutAfterStoring;
        let mut first = Publisher::open(&root, "ver-golden", RunSpec::default()).unwrap();
        assert!(first.sync(&mut noa, Some(BOARD_PANE), &candidates).failed());
        drop(first);
        let mut again = Publisher::open(&root, "ver-golden", RunSpec::default()).unwrap();
        let status = again.sync(&mut noa, Some(BOARD_PANE + 1), &candidates);
        assert_eq!(status, published(1));
        assert_eq!(noa.stored.len(), 1, "stored once");
        assert_eq!(noa.sent.len(), 2);
        assert_eq!(noa.sent[0], noa.sent[1], "the same envelope both times");
        assert_eq!(noa.registered[1].request_id, "verify-ver-golden-run");
        assert_eq!(
            noa.registered[1].board_id.as_deref(),
            Some("bd-00000000000000aa")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_failure_keeps_source_result() {
        let root = temp("failure");
        let session = finished();
        let log = root.join("verifications").join("events.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, serde_json::to_vec(&session).unwrap()).unwrap();
        let before = std::fs::read(&log).unwrap();
        let mut noa = FakeNoa::new();
        noa.next = Next::Refuse;
        let mut publisher =
            Publisher::open(&root.join("outbox"), "ver-golden", RunSpec::default()).unwrap();
        let status = publisher.sync(&mut noa, Some(BOARD_PANE), &tl::candidates(&session));
        assert!(status.failed());
        assert_eq!(std::fs::read(&log).unwrap(), before);
        assert_eq!(session, finished(), "the session is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_state_visible() {
        let root = temp("state");
        let candidates = tl::candidates(&finished());
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, "ver-golden", RunSpec::default()).unwrap();
        assert_eq!(
            publisher.status(&candidates).label().as_deref(),
            Some("タイムライン: 投稿待ち")
        );
        noa.next = Next::Refuse;
        let failed = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(
            failed.label().as_deref(),
            Some("タイムライン: 投稿失敗（再試行）")
        );
        let done = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(done.label().as_deref(), Some("タイムライン: 投稿済み #1"));
        let mut old = FakeNoa::new();
        old.supports = false;
        let mut other =
            Publisher::open(&root.join("old"), "ver-golden", RunSpec::default()).unwrap();
        assert_eq!(
            other.sync(&mut old, Some(BOARD_PANE), &candidates),
            Status::Idle
        );
        assert!(old.sent.is_empty() && old.registered.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 11: the label goes with why it failed or which post the
    // server stored.
    #[test]
    fn ac5_state_says_why_and_which_post() {
        let root = temp("detail");
        let candidates = tl::candidates(&finished());
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        noa.next = Next::Refuse;
        let failed = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(failed.detail().as_deref(), Some("disk full"));
        let done = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(done, published(1));
        assert_eq!(done.detail().as_deref(), Some("postId po-0000000000000001"));
        let again = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(again.status(&candidates), published(1));
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 10: a post the outbox could not stage fails with the
    // reason, and nothing is sent.
    #[test]
    fn ac5_a_staging_failure_shows_its_reason_and_sends_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("stage-fails");
        let candidates = tl::candidates(&finished());
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(publisher.register(&mut noa, Some(BOARD_PANE)), Status::Idle);
        let dir = tl::outbox_dir(&root, SESSION).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let status = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let Status::Failed { message } = &status else {
            panic!("a staging failure shows as {status:?}");
        };
        assert!(message.contains("outbox"), "{message}");
        assert_eq!(publisher.status(&candidates), status);
        assert!(noa.sent.is_empty(), "nothing sent before it is staged");
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &candidates),
            published(1)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 8: never registered without a board pane; registered
    // from one before the first verdict.
    #[test]
    fn ac5_no_run_is_registered_without_a_board_pane() {
        let root = temp("no-board");
        let candidates = tl::candidates(&finished());
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        let waits = Status::Waiting {
            why: tl::NOT_REGISTERED.into(),
        };
        assert_eq!(publisher.register(&mut noa, None), waits);
        assert_eq!(publisher.sync(&mut noa, None, &candidates), waits);
        assert!(noa.registered.is_empty() && noa.sent.is_empty());
        assert_eq!(publisher.register(&mut noa, Some(BOARD_PANE)), Status::Idle);
        assert_eq!(
            noa.registered[0].orchestrator_pane_id.map(|p| p.0),
            Some(BOARD_PANE)
        );
        let mut again = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(again.sync(&mut noa, None, &candidates), published(1));
        assert_eq!(noa.registered.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 3: reopened with no runner, the stored state shows and
    // 再試行 sends the stored envelope.
    #[test]
    fn ac5_a_finished_verification_reopened_resends_its_verdict() {
        let root = temp("reopened");
        let session = finished();
        let candidates = tl::candidates(&session);
        let mut noa = FakeNoa::new();
        noa.next = Next::TimeOutAfterStoring;
        let mut runner_side = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert!(
            runner_side
                .sync(&mut noa, Some(BOARD_PANE), &candidates)
                .failed()
        );
        drop(runner_side);
        let stored = tl::stored_status(&root, &session);
        assert!(stored.failed() && stored.unsent(), "{stored:?}");
        assert_eq!(tl::resend_stored(&root, &session, &mut noa), published(1));
        assert_eq!(noa.stored.len(), 1, "stored once");
        assert_eq!(noa.sent[0], noa.sent[1], "the stored envelope");
        assert_eq!(noa.registered.len(), 1, "no board pane, no registration");
        assert_eq!(tl::stored_status(&root, &session), published(1));
        let other = temp("never");
        assert_eq!(tl::stored_status(&other, &session), Status::Idle);
        assert_eq!(tl::resend_stored(&other, &session, &mut noa), Status::Idle);
        assert!(!tl::outbox_dir(&other, SESSION).unwrap().exists());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other);
    }

    // Review defect 1: a stored verification whose session id is a path is
    // opened from the list without its outbox: nothing outside the root is
    // looked at, created or changed.
    #[test]
    fn ac5_a_session_id_that_is_a_path_opens_no_outbox() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("path-id");
        let outbox = root.join("outbox");
        // A folder whose path is all token characters, as one in a log can be.
        let elsewhere =
            std::env::temp_dir().join(format!("verify-tl-elsewhere-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = elsewhere.display().to_string();
        assert!(
            noa_ipc::timeline::valid_token(&tl::run_request_id(&abs)),
            "the request-id check alone let it through"
        );
        let mut noa = FakeNoa::new();
        for id in [abs.as_str(), "..", ".", "a/b", "ver-1/", ""] {
            assert_eq!(tl::outbox_dir(&outbox, id), None, "{id:?}");
            assert!(
                Publisher::open(&outbox, id, RunSpec::default()).is_err(),
                "{id:?}"
            );
            assert!(
                matches!(Publisher::existing(&outbox, id), Ok(None)),
                "{id:?}"
            );
            let mut session = finished();
            session.id = verify::model::types::SessionId::new(id);
            assert_eq!(tl::stored_status(&outbox, &session), Status::Idle, "{id:?}");
            assert_eq!(
                tl::resend_stored(&outbox, &session, &mut noa),
                Status::Idle,
                "{id:?}"
            );
        }
        let mode = std::fs::metadata(&elsewhere).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "left as it was");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert!(!outbox.exists(), "nothing made");
        assert!(noa.sent.is_empty());
        assert_eq!(
            tl::outbox_dir(&outbox, SESSION),
            Some(outbox.join("verify").join(SESSION))
        );
        let _ = std::fs::remove_dir_all(elsewhere);
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 7: 30 rulings and 80 blocking items, each 200
    // Japanese characters, over the 64 KiB request cap on their own.
    #[test]
    fn ac5_a_post_of_many_long_rulings_and_blockers_fits_the_request_cap() {
        let mut session = finished();
        let criterion = session.criteria[0].clone();
        let defect = session.defects[0].clone();
        let mut output = session.round(2).unwrap().outcome.clone().unwrap();
        output.defect_ops.clear();
        let ruling = output.rulings.values().next().unwrap().clone();
        output.rulings.clear();
        for n in 0..30 {
            let id = format!("AC-{}", 100 + n);
            output.rulings.insert(id.clone(), ruling.clone());
            session.criteria.push(verify::model::types::Criterion {
                id,
                statement: "基".repeat(200),
                ..criterion.clone()
            });
        }
        let verdict = output.verdict.as_mut().unwrap();
        verdict.blocking.clear();
        for n in 0..80 {
            let id = format!("D{}", 100 + n);
            verdict.blocking.push(id.clone());
            session.defects.push(verify::model::types::Defect {
                id,
                claim: "欠".repeat(200),
                ..defect.clone()
            });
        }
        accept_third_round(&mut session, output);
        let candidates = tl::candidates(&session);
        let c = candidates.last().unwrap();
        c.post.validate().unwrap();
        let params = PostParams {
            board_id: "bd-00000000000000aa".into(),
            post: noa_ipc::timeline::PostInput {
                supersedes_post_id: Some("po-00000000000000ff".into()),
                ..c.post.clone()
            },
            producer_id: tl::producer_id(SESSION),
            request_id: tl::request_id(SESSION, &c.event_id),
            run_id: Some("rn-00000000000000bb".into()),
        };
        let bytes = serde_json::to_vec(&params).unwrap().len();
        assert!(bytes <= MAX_POST_PARAMS_BYTES, "{bytes} bytes");
        assert!(
            c.post.open_issues.len() + c.post.checks.len() < 110,
            "items left out"
        );
        assert!(!c.post.open_issues.is_empty() && !c.post.checks.is_empty());
        assert!(
            c.post.body_markdown.contains("report.md"),
            "says what was left out"
        );
        let root = temp("long-items");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &candidates),
            published(2)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
