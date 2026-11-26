//! The review's Board Timeline adapter (`docs/specs/board-timeline.md`
//! AC-5): candidates come from confirmed lead outputs only, their keys
//! survive a replay, a restart re-sends a staged envelope exactly once, and
//! a failed post leaves the review's log alone.

// Under `timeline::` so `cargo test timeline` (the unit's check) selects them.
mod timeline {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use noa_ipc::ClientError;
    use noa_ipc::timeline::{
        CheckResult, MAX_POST_PARAMS_BYTES, Outcome, PostKind, PostParams, PostResult,
        PublishOwner, RegisterRunParams, RegisterRunResult, Seq, TimelineErrorKind, canon_bytes,
    };
    use noa_ipc::timeline_outbox::PostSink;
    use review::model::reduce::{self, Event};
    use review::model::types::{
        AgentKind, Confidence, Decision, FindingStatus, Head, LeadOp, LeadOutput, Location, Millis,
        PartId, Participant, Phase, Role, RoundKind, Session, SessionId, Severity, Target,
        TargetKind, TargetManifest, Verdict,
    };
    use review::store::Store;
    use review::timeline::{self as tl, Publisher, RunSpec, Status, TimelineApi};

    const SESSION: &str = "rev-42";
    const BOARD_PANE: u64 = 7;

    /// What the fake answers for its `seq`-th stored post.
    fn published(seq: u64) -> Status {
        Status::Published {
            seq,
            post_id: format!("po-{seq:016x}"),
        }
    }

    fn created(max_rounds: u32) -> Vec<Event> {
        let p = |id: &str, role| Participant {
            id: PartId::new(id),
            kind: AgentKind::Claude,
            role,
            pane_id: None,
            setup: Default::default(),
            lens: None,
        };
        vec![Event::Created {
            id: SessionId::new(SESSION),
            request: String::new(),
            target: Box::new(Target {
                kind: TargetKind::Document,
                path: None,
                base: None,
                head: Head::Worktree,
                paths: Vec::new(),
                text: Some("doc".into()),
                workdir: None,
            }),
            manifest: Box::new(TargetManifest {
                label: "設計メモ".into(),
                ..TargetManifest::default()
            }),
            workdir: None,
            max_rounds,
            lead: p("lead", Role::Lead),
            reviewers: vec![p("claude", Role::Reviewer)],
            at: 1,
        }]
    }

    fn lead_batch(round: u32, verdict: Option<Decision>, ops: Vec<LeadOp>) -> Vec<Event> {
        let accepted = Event::LeadAccepted {
            round,
            output: LeadOutput {
                summary: format!("round {round} consolidated"),
                ops,
                questions: Default::default(),
                verdict: verdict.map(|decision| Verdict {
                    decision,
                    rationale: "because".into(),
                    must_fix: Vec::new(),
                }),
                close: verdict.is_some(),
            },
        };
        let mut batch = vec![
            accepted,
            Event::PhaseChanged {
                round,
                phase: Phase::Closed,
            },
        ];
        if verdict.is_some() {
            batch.push(Event::SessionClosed);
        }
        batch
    }

    fn add(id: &str, status: FindingStatus) -> LeadOp {
        LeadOp::Add {
            id: id.into(),
            from: Vec::new(),
            claim: format!("claim {id}"),
            location: None,
            severity: Severity::High,
            category: "bug".into(),
            confidence: Confidence::High,
            status,
            rationale: "r".into(),
            fix: None,
        }
    }

    fn round(no: u32) -> Event {
        Event::RoundStarted {
            no,
            kind: if no == 1 {
                RoundKind::Blind
            } else {
                RoundKind::Cross
            },
            at: Millis::from(no) * 10,
        }
    }

    /// Round 1 has no verdict, round 2 closes the session, and round 3
    /// closes it again after an extension.
    fn batches() -> Vec<Vec<Event>> {
        vec![
            created(2),
            vec![round(1)],
            lead_batch(
                1,
                None,
                vec![
                    add("F1", FindingStatus::Open),
                    add("F2", FindingStatus::Rejected),
                ],
            ),
            vec![round(2)],
            lead_batch(
                2,
                Some(Decision::RequestChanges),
                vec![add("F3", FindingStatus::Agreed)],
            ),
            vec![Event::RoundsExtended {
                max_rounds: 3,
                at: 30,
            }],
            vec![round(3)],
            lead_batch(3, Some(Decision::Approve), Vec::new()),
        ]
    }

    fn session_of(batches: &[Vec<Event>]) -> Session {
        let mut session = reduce::create(&batches[0]).unwrap();
        for batch in &batches[1..] {
            reduce::apply(&mut session, batch).unwrap();
        }
        session
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "review-timeline-{name}-{}-{:?}",
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
        let all = batches();
        // Before any verdict (a consolidation without one included): nothing.
        assert!(tl::candidates(&session_of(&all[..4])).is_empty());
        let first = tl::candidates(&session_of(&all[..5]));
        assert_eq!(first.len(), 1);
        let c = &first[0];
        assert_eq!(c.event_id, "verdict-r2");
        assert!(!c.supersedes_previous);
        assert_eq!(c.post.kind, PostKind::Summary);
        assert_eq!(c.post.outcome, Outcome::Succeeded);
        assert_eq!(c.post.summary, "round 2 consolidated");
        assert_eq!(c.post.checks.len(), 1);
        assert_eq!(c.post.checks[0].result, CheckResult::Failed);
        let issues: Vec<_> = c.post.open_issues.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(issues, ["F1: claim F1", "F3: claim F3"]);
        assert_eq!(c.post.source_ref.namespace, "review");
        assert_eq!(c.post.source_ref.session_id, SESSION);
        assert_eq!(c.post.source_ref.event_id, "verdict-r2");
        assert!(c.post.title.contains("要修正"));
        c.post.validate().unwrap();

        let both = tl::candidates(&session_of(&all));
        assert_eq!(
            both.iter().map(|c| c.event_id.as_str()).collect::<Vec<_>>(),
            ["verdict-r2", "verdict-r3"]
        );
        assert!(both[1].supersedes_previous);
        assert_eq!(both[1].post.checks[0].result, CheckResult::Passed);
    }

    #[test]
    fn ac5_unstaged_verdict_keeps_its_accepted_round() {
        let mut session = reduce::create(&created(1)).unwrap();
        reduce::apply(&mut session, &[round(1)]).unwrap();
        let batch = lead_batch(
            1,
            Some(Decision::RequestChanges),
            vec![add("F1", FindingStatus::Agreed)],
        );
        reduce::apply(&mut session, &batch).unwrap();
        let first_report = review::model::export::report_markdown(&session);
        reduce::apply(
            &mut session,
            &[
                Event::RoundsExtended {
                    max_rounds: 2,
                    at: 30,
                },
                round(2),
            ],
        )
        .unwrap();
        let batch = lead_batch(
            2,
            Some(Decision::ApproveWithFixes),
            vec![add("F2", FindingStatus::Agreed)],
        );
        reduce::apply(&mut session, &batch).unwrap();

        let root = temp("unstaged");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &tl::candidates(&session)),
            published(2)
        );
        let first = &noa.sent[0].post;
        assert_eq!(
            first.open_issues.len(),
            1,
            "later finding leaked into round 1"
        );
        assert_eq!(first.body_markdown, first_report);
        assert_eq!(noa.sent[1].post.open_issues.len(), 2);
        assert_eq!(
            noa.sent[1].post.body_markdown,
            review::model::export::report_markdown(&session)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_request_ids_stable_across_replay() {
        let root = temp("replay");
        let dir = root.join(SESSION);
        let all = batches();
        let mut store = Store::create(&dir, &all[0]).unwrap();
        store.append_all(&all[1..]).unwrap();
        drop(store);
        let (_store, replayed) = Store::open(&dir).unwrap();
        let live = session_of(&all);
        let round = serde_json::to_value(live.round(2).unwrap()).unwrap();
        assert!(round.get("verdict_post").is_none());
        let ids = |s: &Session| {
            tl::candidates(s)
                .into_iter()
                .map(|c| tl::request_id(s.id.as_str(), &c.event_id))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&live),
            ["review-rev-42-verdict-r2", "review-rev-42-verdict-r3"]
        );
        assert_eq!(ids(&replayed), ids(&live));
        assert_eq!(tl::candidates(&replayed), tl::candidates(&live));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_restart_resends_exactly_once() {
        let root = temp("restart");
        let session = session_of(&batches()[..5]);
        let candidates = tl::candidates(&session);
        let mut noa = FakeNoa::new();
        noa.next = Next::TimeOutAfterStoring;
        let mut first = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        let status = first.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert!(status.failed(), "{status:?}");
        drop(first);

        // A restart: a new publisher on the same outbox re-sends the stored
        // envelope, which the server recognises.
        let mut again = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(
            again.binding().map(|b| b.run_id.as_str()),
            Some("rn-00000000000000bb"),
            "the run is saved before the first post"
        );
        let status = again.sync(&mut noa, Some(BOARD_PANE + 1), &candidates);
        assert_eq!(status, published(1));
        assert_eq!(noa.stored.len(), 1, "stored once");
        assert_eq!(noa.sent.len(), 2);
        assert_eq!(noa.sent[0], noa.sent[1], "the same envelope both times");
        // The restart registered again with the saved board, from the new
        // board pane, under the same request id.
        assert_eq!(noa.registered.len(), 2);
        assert_eq!(noa.registered[1].request_id, "review-rev-42-run");
        assert_eq!(
            noa.registered[1].board_id.as_deref(),
            Some("bd-00000000000000aa")
        );
        assert_eq!(noa.registered[1].publish_policy.owner, PublishOwner::Runner);
        assert_eq!(
            noa.registered[1].publish_policy.producer_id.as_deref(),
            Some("review:rev-42")
        );

        // Published stays published: nothing is sent again.
        let status = again.sync(&mut noa, Some(BOARD_PANE + 1), &candidates);
        assert_eq!(status, published(1));
        assert_eq!(noa.sent.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_later_verdict_supersedes_the_published_one_and_waits_for_it() {
        let root = temp("supersede");
        let candidates = tl::candidates(&session_of(&batches()));
        let mut noa = FakeNoa::new();
        noa.next = Next::Refuse;
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert!(
            publisher
                .sync(&mut noa, Some(BOARD_PANE), &candidates)
                .failed()
        );
        assert_eq!(noa.sent.len(), 1, "the second waits behind the first");
        let status = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(status, published(2));
        assert_eq!(noa.stored.len(), 2);
        assert_eq!(noa.stored[0].0.post.supersedes_post_id, None);
        assert_eq!(
            noa.stored[1].0.post.supersedes_post_id.as_deref(),
            Some(noa.stored[0].1.as_str())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_failure_keeps_source_result() {
        let root = temp("failure");
        let dir = root.join("reviews").join(SESSION);
        let all = batches();
        let mut store = Store::create(&dir, &all[0]).unwrap();
        store.append_all(&all[1..]).unwrap();
        let log = std::fs::read(dir.join("events.jsonl")).unwrap();
        let session = session_of(&all);
        let mut noa = FakeNoa::new();
        noa.next = Next::Refuse;
        let mut publisher =
            Publisher::open(&root.join("outbox"), SESSION, RunSpec::default()).unwrap();
        let status = publisher.sync(&mut noa, Some(BOARD_PANE), &tl::candidates(&session));
        assert!(status.failed());
        assert_eq!(std::fs::read(dir.join("events.jsonl")).unwrap(), log);
        assert_eq!(session, session_of(&all), "the session is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_state_visible() {
        let root = temp("state");
        let candidates = tl::candidates(&session_of(&batches()[..5]));
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
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
        assert_eq!(Status::Idle.label(), None);
        // An older Noa: nothing is sent and nothing registered.
        let mut old = FakeNoa::new();
        old.supports = false;
        let mut other = Publisher::open(&root.join("old"), SESSION, RunSpec::default()).unwrap();
        other.sync(&mut old, Some(BOARD_PANE), &candidates);
        assert!(old.sent.is_empty() && old.registered.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 11: the label goes with why it failed or which post the
    // server stored.
    #[test]
    fn ac5_state_says_why_and_which_post() {
        let root = temp("detail");
        let candidates = tl::candidates(&session_of(&batches()[..5]));
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        noa.next = Next::Refuse;
        let failed = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(failed.detail().as_deref(), Some("disk full"));
        let done = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        assert_eq!(done, published(1));
        assert_eq!(done.detail().as_deref(), Some("postId po-0000000000000001"));
        // Reopened, the outbox still says which post it is.
        let again = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(again.status(&candidates), published(1));
        assert_eq!(Status::Pending.detail(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 10: a post the outbox could not stage fails with the
    // reason, and nothing is sent.
    #[test]
    fn ac5_a_staging_failure_shows_its_reason_and_sends_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("stage-fails");
        let candidates = tl::candidates(&session_of(&batches()[..5]));
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(publisher.register(&mut noa, Some(BOARD_PANE)), Status::Idle);
        let dir = tl::outbox_dir(&root, SESSION).unwrap();
        // No room for the envelope: nothing can be written there.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let status = publisher.sync(&mut noa, Some(BOARD_PANE), &candidates);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let Status::Failed { message } = &status else {
            panic!("a staging failure shows as {status:?}");
        };
        assert!(message.contains("outbox"), "{message}");
        assert_eq!(
            status.label().as_deref(),
            Some("タイムライン: 投稿失敗（再試行）")
        );
        assert_eq!(
            publisher.status(&candidates),
            status,
            "it stays until retried"
        );
        assert!(noa.sent.is_empty(), "nothing sent before it is staged");
        // 再試行 once there is room stages and sends it.
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &candidates),
            published(1)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 8: a run is registered from the board pane at the
    // session's start, never without one (that would make a board with no
    // root), and its later posts use the run saved then.
    #[test]
    fn ac5_no_run_is_registered_without_a_board_pane() {
        let root = temp("no-board");
        let candidates = tl::candidates(&session_of(&batches()[..5]));
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        let waits = Status::Waiting {
            why: tl::NOT_REGISTERED.into(),
        };
        assert_eq!(publisher.register(&mut noa, None), waits);
        assert_eq!(publisher.sync(&mut noa, None, &candidates), waits);
        assert_eq!(waits.label().as_deref(), Some("タイムライン: 投稿待ち"));
        assert!(noa.registered.is_empty() && noa.sent.is_empty());
        // At the session's start: no verdict yet, the run is registered.
        let mut started =
            Publisher::open(&root.join("started"), SESSION, RunSpec::default()).unwrap();
        assert_eq!(started.register(&mut noa, Some(BOARD_PANE)), Status::Idle);
        assert_eq!(noa.registered.len(), 1);
        assert_eq!(
            noa.registered[0].orchestrator_pane_id.map(|p| p.0),
            Some(BOARD_PANE)
        );
        assert!(noa.sent.is_empty());
        // A restart with the board tab gone posts under the saved run.
        let mut again =
            Publisher::open(&root.join("started"), SESSION, RunSpec::default()).unwrap();
        assert_eq!(again.sync(&mut noa, None, &candidates), published(1));
        assert_eq!(noa.registered.len(), 1, "no board pane, no registration");
        assert_eq!(noa.sent[0].run_id.as_deref(), Some("rn-00000000000000bb"));
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 3: a review that finished while its last post was
    // unanswered is opened again with no runner: its outbox says where the
    // post stands, and 再試行 sends the stored envelope.
    #[test]
    fn ac5_a_finished_review_reopened_resends_its_verdict() {
        let root = temp("reopened");
        let session = session_of(&batches()[..5]);
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
        assert_eq!(
            stored,
            Status::Failed {
                message: "timed out waiting for the post acknowledgement".into()
            }
        );
        assert!(stored.unsent());
        assert_eq!(tl::resend_stored(&root, &session, &mut noa), published(1));
        assert_eq!(noa.stored.len(), 1, "stored once");
        assert_eq!(noa.sent.len(), 2);
        assert_eq!(noa.sent[0], noa.sent[1], "the stored envelope");
        assert_eq!(noa.registered.len(), 1, "no board pane, no registration");
        assert_eq!(tl::stored_status(&root, &session), published(1));

        // A review that never tried the timeline shows and creates nothing.
        let other = temp("never");
        assert_eq!(tl::stored_status(&other, &session), Status::Idle);
        assert_eq!(tl::resend_stored(&other, &session, &mut noa), Status::Idle);
        assert!(!tl::outbox_dir(&other, SESSION).unwrap().exists());
        assert_eq!(noa.sent.len(), 2);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other);
    }

    // Review defect 1: a stored review whose session id is a path is
    // opened from the list without its outbox: nothing outside the root is
    // looked at, created or changed.
    #[test]
    fn ac5_a_session_id_that_is_a_path_opens_no_outbox() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("path-id");
        let outbox = root.join("outbox");
        // A folder whose path is all token characters, as one in a log can be.
        let elsewhere =
            std::env::temp_dir().join(format!("review-tl-elsewhere-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = elsewhere.display().to_string();
        assert!(
            noa_ipc::timeline::valid_token(&tl::run_request_id(&abs)),
            "the request-id check alone let it through"
        );
        let mut noa = FakeNoa::new();
        for id in [abs.as_str(), "..", ".", "a/b", "rev-1/", ""] {
            assert_eq!(tl::outbox_dir(&outbox, id), None, "{id:?}");
            assert!(
                Publisher::open(&outbox, id, RunSpec::default()).is_err(),
                "{id:?}"
            );
            assert!(
                matches!(Publisher::existing(&outbox, id), Ok(None)),
                "{id:?}"
            );
            let mut session = session_of(&batches()[..5]);
            session.id = SessionId::new(id);
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
            Some(outbox.join("review").join(SESSION))
        );
        let _ = std::fs::remove_dir_all(elsewhere);
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 7: open findings alone over the 64 KiB request cap.
    #[test]
    fn ac5_a_post_of_a_hundred_long_findings_fits_the_request_cap() {
        let mut all = batches();
        all.truncate(4);
        let ops = (0..100)
            .map(|n| LeadOp::Add {
                id: format!("F{}", n + 100),
                from: Vec::new(),
                claim: "長".repeat(200),
                location: Some(Location {
                    path: format!("src/{}.rs", "p".repeat(75)),
                    line: None,
                    end_line: None,
                }),
                severity: Severity::High,
                category: "bug".into(),
                confidence: Confidence::High,
                status: FindingStatus::Open,
                rationale: "r".into(),
                fix: None,
            })
            .collect();
        all.push(lead_batch(2, Some(Decision::RequestChanges), ops));
        let session = session_of(&all);
        assert_eq!(
            session
                .findings
                .last()
                .unwrap()
                .location
                .as_ref()
                .unwrap()
                .path
                .len(),
            82
        );
        let c = tl::candidates(&session).remove(0);
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
        assert!(!c.post.open_issues.is_empty() && c.post.open_issues.len() < 100);
        assert!(
            c.post.body_markdown.contains("report.md"),
            "says what was left out"
        );
        // The server takes it, so later rounds are not held behind it.
        let root = temp("long-findings");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&root, SESSION, RunSpec::default()).unwrap();
        assert_eq!(
            publisher.sync(&mut noa, Some(BOARD_PANE), &[c]),
            published(1)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
