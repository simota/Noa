//! The tasks widget's Board Timeline adapter
//! (`docs/specs/board-timeline.md` AC-5): a run is registered when its
//! pane is up and posted only when the person shares it, its keys come
//! from the run log alone, a restart re-sends a staged share exactly once,
//! and a failed share leaves the run's files alone.

// Under `timeline::` so `cargo test timeline` (the unit's check) selects them.
mod timeline {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use noa_ipc::ClientError;
    use noa_ipc::timeline::{
        MAX_BODY_BYTES, Outcome, PostKind, PostParams, PostResult, PublishOwner, RegisterRunParams,
        RegisterRunResult, Seq, TimelineErrorKind, canon_bytes, valid_token,
    };
    use noa_ipc::timeline_outbox::PostSink;
    use tasks::model::types::{AgentKind, Run, RunEvent, RunId, TaskRef};
    use tasks::runs;
    use tasks::timeline::{self as tl, Publisher, RunSpec, Share, Status, TimelineApi};

    const KEY: &str = "app-00000000000000ff";
    const BOARD_PANE: u64 = 7;
    const RUN_PANE: u64 = 9;

    /// What the fake answers for its `seq`-th stored post.
    fn published(seq: u64) -> Status {
        Status::Published {
            seq,
            post_id: format!("po-{seq:016x}"),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tasks-timeline-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A run started and given its pane, logged in `log`, read back.
    fn logged_run(log: &Path, workdir: &Path) -> Run {
        let run = RunId::new("run-1000");
        for event in [
            RunEvent::RunStarted {
                run: run.clone(),
                task: TaskRef::GitHub {
                    repo: "o/n".into(),
                    number: 12,
                },
                agent: AgentKind::Claude,
                workdir: workdir.to_owned(),
                branch: None,
                label: runs::label(&run),
                at: 1000,
            },
            RunEvent::RunPaneAssigned {
                run: run.clone(),
                pane: RUN_PANE,
                board: BOARD_PANE,
                at: 1001,
            },
        ] {
            runs::append(log, &event).unwrap();
        }
        runs::replay(log).unwrap().remove(0)
    }

    fn spec(run: &Run) -> RunSpec {
        RunSpec {
            worker_pane: run.pane,
            task_ref: Some(tl::task_ref(&run.task)),
        }
    }

    fn share<'a>(run: &'a Run, path: &'a Path, text: &'a str) -> Share<'a> {
        Share {
            run: &run.run,
            task: &run.task,
            title: Some("フォントを直す"),
            result_path: path,
            result: text,
            longer: false,
            outcome: Outcome::Unknown,
        }
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
        let root = temp("confirmed");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let mut noa = FakeNoa::new();
        let mut publisher =
            Publisher::open(&root.join("outbox"), KEY, run.run.as_str(), spec(&run)).unwrap();
        // Starting a run registers it and posts nothing.
        assert_eq!(publisher.register(&mut noa, run.board), Status::Idle);
        assert!(noa.sent.is_empty());
        assert_eq!(publisher.shared(), None, "never shared");
        let reg = &noa.registered[0];
        assert_eq!(reg.request_id, "tasks-app-00000000000000ff-run-run-1000");
        assert_eq!(reg.orchestrator_pane_id.map(|p| p.0), Some(BOARD_PANE));
        assert_eq!(reg.worker_pane_id.map(|p| p.0), Some(RUN_PANE));
        assert_eq!(reg.publish_policy.owner, PublishOwner::Runner);
        assert_eq!(
            reg.publish_policy.producer_id.as_deref(),
            Some("tasks:app-00000000000000ff")
        );
        let task = reg.task_ref.as_ref().unwrap();
        assert_eq!((task.kind.as_str(), task.id.as_str()), ("github", "o/n#12"));

        // The share is the confirmed point: the result file's text, the
        // outcome the person picked.
        let path = runs::result_file(&root, &run.run);
        let mut picked = share(&run, &path, "## 結果\n\n直しました。\n");
        picked.outcome = Outcome::Succeeded;
        let c = tl::candidate(KEY, &picked);
        assert_eq!(c.event_id, "result-run-1000");
        assert_eq!(c.post.kind, PostKind::Summary);
        assert_eq!(c.post.outcome, Outcome::Succeeded);
        assert_eq!(c.post.title, "タスク結果: フォントを直す");
        assert_eq!(c.post.summary, "結果");
        assert_eq!(c.post.body_markdown, "## 結果\n\n直しました。\n");
        assert_eq!(c.post.source_ref.namespace, "tasks");
        assert_eq!(c.post.source_ref.session_id, KEY);
        assert_eq!(c.post.source_ref.run_id.as_deref(), Some("run-1000"));
        assert_eq!(c.post.artifacts[0].r#ref, path.display().to_string());
        c.post.validate().unwrap();
        let status = publisher.sync(&mut noa, run.board, &[c]);
        assert_eq!(status, published(1));
        assert_eq!(noa.registered.len(), 1, "registered once");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_request_ids_stable_across_replay() {
        let root = temp("replay");
        let log = root.join("runs.jsonl");
        let run = logged_run(&log, &root);
        let again = runs::replay(&log).unwrap().remove(0);
        let path = runs::result_file(&root, &run.run);
        let first = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let second = tl::candidate(KEY, &share(&again, &path, "結果\n"));
        assert_eq!(first, second);
        assert_eq!(
            tl::request_id(KEY, &first.event_id),
            "tasks-app-00000000000000ff-result-run-1000"
        );
        assert_eq!(tl::task_ref(&run.task), tl::task_ref(&again.task));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_restart_resends_exactly_once() {
        let root = temp("restart");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let c = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let mut noa = FakeNoa::new();
        noa.next = Next::TimeOutAfterStoring;
        let outbox = root.join("outbox");
        let mut first = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert!(
            first
                .sync(&mut noa, run.board, std::slice::from_ref(&c))
                .failed()
        );
        drop(first);
        let mut again = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(
            again.shared(),
            Some(Status::Failed {
                message: "timed out waiting for the post acknowledgement".into()
            })
        );
        // The pane is gone after the restart: no board, no worker pane.
        let status = again.sync(&mut noa, None, std::slice::from_ref(&c));
        assert_eq!(status, published(1));
        assert_eq!(noa.stored.len(), 1, "stored once");
        assert_eq!(noa.sent.len(), 2);
        assert_eq!(noa.sent[0], noa.sent[1], "the same envelope both times");
        assert_eq!(again.shared(), Some(published(1)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_failure_keeps_source_result() {
        let root = temp("failure");
        let log = root.join("runs.jsonl");
        let run = logged_run(&log, &root);
        let path = runs::result_file(&root, &run.run);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "結果\n").unwrap();
        let (log_bytes, result_bytes) =
            (std::fs::read(&log).unwrap(), std::fs::read(&path).unwrap());
        let (text, longer) = tl::read_result(&root, &path).unwrap();
        let mut noa = FakeNoa::new();
        noa.next = Next::Refuse;
        let mut publisher =
            Publisher::open(&root.join("outbox"), KEY, run.run.as_str(), spec(&run)).unwrap();
        let mut picked = share(&run, &path, &text);
        picked.longer = longer;
        let status = publisher.sync(&mut noa, run.board, &[tl::candidate(KEY, &picked)]);
        assert!(status.failed());
        assert_eq!(std::fs::read(&log).unwrap(), log_bytes);
        assert_eq!(std::fs::read(&path).unwrap(), result_bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ac5_state_visible() {
        let root = temp("state");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let c = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let mut noa = FakeNoa::new();
        let mut publisher =
            Publisher::open(&root.join("outbox"), KEY, run.run.as_str(), spec(&run)).unwrap();
        noa.next = Next::Refuse;
        let failed = publisher.sync(&mut noa, run.board, std::slice::from_ref(&c));
        assert_eq!(
            failed.label().as_deref(),
            Some("タイムライン: 投稿失敗（再試行）")
        );
        assert_eq!(publisher.shared().map(|s| s.failed()), Some(true));
        let done = publisher.sync(&mut noa, run.board, std::slice::from_ref(&c));
        assert_eq!(done.label().as_deref(), Some("タイムライン: 投稿済み #1"));
        assert_eq!(
            Status::Pending.label().as_deref(),
            Some("タイムライン: 投稿待ち")
        );
        let mut old = FakeNoa::new();
        old.supports = false;
        let mut other =
            Publisher::open(&root.join("old"), KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(
            other.sync(&mut old, run.board, std::slice::from_ref(&c)),
            Status::Idle
        );
        assert!(old.sent.is_empty() && old.registered.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_a_long_result_is_cut_with_a_note_and_links_its_file() {
        let root = temp("long");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "あ".repeat(MAX_BODY_BYTES)).unwrap();
        let (text, longer) = tl::read_result(&root, &path).unwrap();
        assert!(longer);
        let mut picked = share(&run, &path, &text);
        picked.longer = longer;
        let c = tl::candidate(KEY, &picked);
        assert!(c.post.body_markdown.len() <= MAX_BODY_BYTES);
        assert!(c.post.body_markdown.ends_with("（全文は result.md）"));
        assert!(c.post.body_markdown.starts_with("あああ"));
        c.post.validate().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_a_result_that_is_a_link_or_a_fifo_is_refused_without_waiting() {
        let root = temp("special");
        let target = root.join("elsewhere.md");
        std::fs::write(&target, "x").unwrap();
        let link = root.join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(tl::read_result(&root, &link).is_err());
        let fifo = root.join("fifo.md");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(tl::read_result(&root, &fifo).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_session_key_is_a_token() {
        for key in [
            "my app-0123456789abcdef",
            "日本語-0123456789abcdef",
            "a-b-0123456789abcdef",
        ] {
            let token = tl::session_key(key);
            assert!(token.ends_with("-0123456789abcdef"), "{token}");
            assert!(
                valid_token(&tl::run_request_id(&token, "run-1000-2")),
                "{token}"
            );
        }
        assert_eq!(tl::session_key("app-00ff"), "app-00ff");
    }

    // Review defect 1: a result folder that is a link is not read through,
    // so another run's result is never shared as this one's.
    #[test]
    fn timeline_a_result_reached_through_a_linked_folder_is_refused() {
        let root = temp("linked-dir");
        let work = root.join("work");
        let run = logged_run(&root.join("runs.jsonl"), &work);
        let path = runs::result_file(&work, &run.run);
        let other = root.join("other-run");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("result.md"), "別の run の結果\n").unwrap();
        // The run's own folder is a link to another run's.
        std::fs::create_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&other, path.parent().unwrap()).unwrap();
        let err = tl::read_result(&work, &path).unwrap_err();
        assert!(tasks::fsx::is_link_error(&err), "{err}");
        // So is a link higher up (`.noa`).
        let work2 = root.join("work2");
        std::fs::create_dir_all(&work2).unwrap();
        std::os::unix::fs::symlink(work.join(".noa"), work2.join(".noa")).unwrap();
        let path2 = runs::result_file(&work2, &run.run);
        assert!(tl::read_result(&work2, &path2).is_err());
        // The same file in a real folder is read.
        std::fs::remove_file(path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "結果\n").unwrap();
        assert_eq!(
            tl::read_result(&work, &path).unwrap(),
            ("結果\n".into(), false)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 2: a run id that is not one plain path component never
    // names an outbox, and nothing outside the root is created or changed.
    #[test]
    fn timeline_a_run_id_that_is_a_path_opens_no_outbox() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("path-id");
        let outbox = root.join("outbox");
        // A folder whose path is all token characters, as one in a log can be.
        let elsewhere =
            std::env::temp_dir().join(format!("tasks-tl-elsewhere-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = elsewhere.display().to_string();
        assert!(
            valid_token(&tl::run_request_id(KEY, &abs)),
            "the old check let it through"
        );
        for id in [abs.as_str(), "..", ".", "a/b", "run-1/", ""] {
            assert_eq!(tl::outbox_dir(&outbox, KEY, id), None, "{id:?}");
            assert!(
                Publisher::open(&outbox, KEY, id, RunSpec::default()).is_err(),
                "{id:?}"
            );
        }
        let mode = std::fs::metadata(&elsewhere).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "left as it was");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert!(!outbox.exists(), "nothing made");
        assert_eq!(
            tl::outbox_dir(&outbox, KEY, "run-1000"),
            Some(outbox.join("tasks").join(KEY).join("run-1000"))
        );
        let _ = std::fs::remove_dir_all(elsewhere);
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 12: a cut that falls inside a character still says the
    // file goes on.
    #[test]
    fn timeline_a_long_japanese_result_cut_inside_a_character_keeps_the_note() {
        let root = temp("cut-in-char");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{}日付", "a".repeat(MAX_BODY_BYTES))).unwrap();
        let (text, longer) = tl::read_result(&root, &path).unwrap();
        assert!(longer);
        assert_eq!(
            text.len(),
            MAX_BODY_BYTES,
            "read up to the whole characters"
        );
        let mut picked = share(&run, &path, &text);
        picked.longer = longer;
        let c = tl::candidate(KEY, &picked);
        assert!(c.post.body_markdown.len() <= MAX_BODY_BYTES);
        assert!(
            c.post.body_markdown.ends_with("（全文は result.md）"),
            "{:?}",
            &c.post.body_markdown[c.post.body_markdown.len() - 40..]
        );
        c.post.validate().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 5: what was shared is saved before anything is sent and
    // read back as it was, whatever the result file says later.
    #[test]
    fn ac5_a_share_is_saved_before_it_is_sent() {
        let root = temp("saved");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let outbox = root.join("outbox");
        let mut picked = share(&run, &path, "最初の結果\n");
        picked.outcome = Outcome::Succeeded;
        let c = tl::candidate(KEY, &picked);
        let publisher = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(publisher.saved_share().unwrap(), None);
        assert_eq!(publisher.shared(), None, "never shared");
        publisher.save_share(&c).unwrap();
        drop(publisher);
        // A restart: nothing was sent, the share is there as it was made.
        let again = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(again.saved_share().unwrap(), Some(c.clone()));
        assert_eq!(
            again.shared(),
            Some(Status::Waiting {
                why: tl::NOT_REGISTERED.into()
            }),
            "saved, waiting for its run"
        );
        let mut noa = FakeNoa::new();
        let mut again = again;
        assert_eq!(
            again.sync(&mut noa, run.board, std::slice::from_ref(&c)),
            published(1)
        );
        let sent = &noa.sent[0].post;
        assert_eq!(sent.body_markdown, "最初の結果\n");
        assert_eq!(sent.outcome, Outcome::Succeeded);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = tl::outbox_dir(&outbox, KEY, run.run.as_str())
                .unwrap()
                .join("share.json");
            let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 8: never registered without the run's pane on its
    // board: no rootless board.
    #[test]
    fn ac5_no_run_is_registered_without_a_board_pane() {
        let root = temp("no-board");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let c = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let mut noa = FakeNoa::new();
        let gone = RunSpec {
            worker_pane: None,
            ..spec(&run)
        };
        let mut publisher =
            Publisher::open(&root.join("outbox"), KEY, run.run.as_str(), gone).unwrap();
        let waits = Status::Waiting {
            why: tl::NOT_REGISTERED.into(),
        };
        assert_eq!(publisher.register(&mut noa, None), waits);
        assert_eq!(
            publisher.sync(&mut noa, None, std::slice::from_ref(&c)),
            waits
        );
        assert_eq!(waits.label().as_deref(), Some("タイムライン: 投稿待ち"));
        assert!(noa.registered.is_empty() && noa.sent.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 10: a share the outbox could not stage fails with the
    // reason, and nothing is sent.
    #[test]
    fn ac5_a_staging_failure_shows_its_reason_and_sends_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("stage-fails");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let c = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let outbox = root.join("outbox");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(publisher.register(&mut noa, run.board), Status::Idle);
        let dir = tl::outbox_dir(&outbox, KEY, run.run.as_str()).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let status = publisher.sync(&mut noa, run.board, std::slice::from_ref(&c));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let Status::Failed { message } = &status else {
            panic!("a staging failure shows as {status:?}");
        };
        assert!(message.contains("outbox"), "{message}");
        assert_eq!(publisher.shared(), Some(status.clone()));
        assert!(noa.sent.is_empty());
        assert_eq!(publisher.sync(&mut noa, run.board, &[c]), published(1));
        let _ = std::fs::remove_dir_all(root);
    }

    // Review defect 11: why it failed and which post it is, also after a
    // restart.
    #[test]
    fn ac5_state_says_why_and_which_post() {
        let root = temp("detail");
        let run = logged_run(&root.join("runs.jsonl"), &root);
        let path = runs::result_file(&root, &run.run);
        let c = tl::candidate(KEY, &share(&run, &path, "結果\n"));
        let outbox = root.join("outbox");
        let mut noa = FakeNoa::new();
        let mut publisher = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        noa.next = Next::Refuse;
        let failed = publisher.sync(&mut noa, run.board, std::slice::from_ref(&c));
        assert_eq!(failed.detail().as_deref(), Some("disk full"));
        drop(publisher);
        let again = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(
            again.shared().and_then(|s| s.detail()).as_deref(),
            Some("disk full")
        );
        let mut again = again;
        assert_eq!(again.sync(&mut noa, run.board, &[c]), published(1));
        let reopened = Publisher::open(&outbox, KEY, run.run.as_str(), spec(&run)).unwrap();
        assert_eq!(
            reopened.shared().and_then(|s| s.detail()).as_deref(),
            Some("postId po-0000000000000001")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
