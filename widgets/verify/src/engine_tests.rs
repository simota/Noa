use super::*;
use crate::model::reduce::Event;
use crate::model::types::{
    AgentKind, AgentSetup, BuiltinStrategy, CriterionSource, Media, Outcome, Priority, Subject,
};
use crate::testkit::{self, JUDGE_R1, Options, Rig, VERIFIERS, all_pass, cmd, key, p, reply_json};

fn create(panel: &[VerifierChoice], max_rounds: RoundNo) -> Result<Engine, String> {
    Engine::create_in(
        SessionId::new("ver-x"),
        Setup {
            request: "",
            target: testkit::target(),
            manifest: testkit::manifest(),
            rebuild: Rebuild::default(),
            spec: None,
            isolation: Isolation::Worktree,
            user_criteria: Vec::new(),
            confirm: true,
            workdir: PathBuf::from("/repo"),
            main_worktree: PathBuf::from("/repo"),
            max_rounds,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel,
        },
        Path::new("/tmp/ver-x"),
        Config::default(),
        1,
    )
    .map(|(engine, _)| engine)
    .map_err(|e| e.to_string())
}

#[test]
fn the_panel_size_setups_and_custom_strategies_are_checked() {
    let one = vec![testkit::verifier(AgentKind::Claude, BuiltinStrategy::E2e)];
    assert!(create(&one, 2).unwrap_err().contains("2体以上"));
    let seven: Vec<VerifierChoice> = (0..7)
        .map(|_| testkit::verifier(AgentKind::Codex, BuiltinStrategy::E2e))
        .collect();
    assert!(create(&seven, 2).unwrap_err().contains("6体まで"));
    let mut bad = testkit::panel();
    bad[0].agent.setup = AgentSetup {
        model: Some("a b".into()),
        effort: None,
    };
    assert!(create(&bad, 2).unwrap_err().contains("モデル名"));
    let mut custom = testkit::panel();
    custom[2].strategy = StrategyChoice::Custom("   ".into());
    assert!(create(&custom, 2).unwrap_err().contains("空です"));
    custom[2].strategy = StrategyChoice::Custom("あ".repeat(501));
    assert!(create(&custom, 2).unwrap_err().contains("500文字まで"));
    custom[2].strategy = StrategyChoice::Custom("あ".repeat(500));
    assert!(create(&custom, 2).is_ok());
    assert!(create(&testkit::panel(), 0).is_err());
    assert!(create(&testkit::panel(), 7).is_err());
}

#[test]
fn verifier_and_custom_strategy_ids_follow_the_panel() {
    let panel = vec![
        testkit::verifier(AgentKind::Claude, BuiltinStrategy::Conformance),
        VerifierChoice {
            agent: AgentKind::Codex.into(),
            strategy: StrategyChoice::Custom(" Odd locales ".into()),
        },
        VerifierChoice {
            agent: AgentKind::Claude.into(),
            strategy: StrategyChoice::Custom("Slow disks".into()),
        },
    ];
    let (judge, verifiers) = participants(&AgentKind::Agy.into(), &panel);
    assert_eq!(judge.id.as_str(), JUDGE_ID);
    let ids: Vec<&str> = verifiers.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "claude-2"]);
    assert_eq!(
        verifiers[1].strategy,
        Some(Strategy::Custom {
            id: "custom".into(),
            text: "Odd locales".into()
        })
    );
    assert_eq!(verifiers[2].strategy.as_ref().unwrap().id(), "custom-2");
}

#[test]
fn the_timeouts_are_the_specs() {
    let config = Config::default();
    assert_eq!(config.start_timeout, 90_000);
    assert_eq!(config.reply_timeout, 45 * 60_000);
    assert_eq!(config.max_retries, 2);
}

#[test]
fn a_recheck_replays_into_confirmation_without_drafting_or_copying_outcomes() {
    let mut original = Rig::new(2);
    testkit::accept_default_criteria(&mut original);
    let at = original.tick();
    original.feed(Input::CriteriaEdit {
        edit: CriteriaEdit::Edit {
            id: "AC-1".into(),
            statement: None,
            how: Some("Use the previous custom procedure".into()),
            priority: Some(Priority::Should),
        },
        at,
    });
    let previous = crate::model::recheck::Recheck::from_session(original.engine.session()).unwrap();
    let panel = testkit::panel();
    let (engine, output) = Engine::create_in(
        SessionId::new("ver-again"),
        Setup {
            request: "Verify the updated change",
            target: testkit::target(),
            manifest: testkit::manifest(),
            rebuild: Rebuild::default(),
            spec: None,
            isolation: Isolation::Worktree,
            user_criteria: Vec::new(),
            confirm: false,
            workdir: PathBuf::from("/repo"),
            main_worktree: PathBuf::from("/repo"),
            max_rounds: 2,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel: &panel,
        },
        Path::new("/tmp/ver-again"),
        Config::default(),
        100,
    )
    .unwrap();
    let (mut engine, output) = engine.with_recheck(output, previous.clone()).unwrap();
    assert_eq!(output.batches.len(), 1);
    assert_eq!(output.batches[0].len(), 2);
    let json = serde_json::to_string(&output.batches[0]).unwrap();
    let batch: Vec<Event> = serde_json::from_str(&json).unwrap();
    let replayed = reduce::create(&batch).unwrap();
    assert_eq!(&replayed, engine.session());
    assert_eq!(replayed.criteria, previous.criteria);
    assert_eq!(replayed.phase(), Phase::Confirming);
    assert!(replayed.confirm);
    assert!(replayed.criteria_brief.is_none());
    assert!(replayed.results.is_empty());
    assert!(replayed.rulings.is_empty());
    assert!(replayed.evidence.is_empty());
    assert!(replayed.rounds.is_empty());
    assert!(replayed.verdict().is_none());
    let started = engine.handle(Input::Start { now: 101 }).unwrap();
    assert_eq!(started.effects, [Effect::WriteReport]);
    assert!(started.batches.is_empty());
}

#[test]
fn ac6_with_confirmation_on_the_session_holds_after_the_criteria_and_edits_are_events() {
    let mut rig = Rig::new(2);
    assert_eq!(rig.phase(), Phase::Criteria);
    let brief = rig.engine.session().criteria_brief.clone().unwrap();
    assert!(brief.body.contains(testkit::USER_LINE));
    testkit::accept_default_criteria(&mut rig);
    assert_eq!(rig.phase(), Phase::Confirming);
    assert!(rig.engine.session().rounds.is_empty(), "held: no round yet");
    assert!(!rig.asked(&Effect::MakeWorktrees));

    // Edits are events.
    let at = rig.tick();
    rig.feed(Input::CriteriaEdit {
        edit: CriteriaEdit::Edit {
            id: "AC-1".into(),
            statement: None,
            how: Some("Open a tab from /tmp and run pwd".into()),
            priority: None,
        },
        at,
    });
    // A criterion the person gave can be edited but not removed; nor one
    // from the spec or the person's additions.
    let err = rig
        .try_feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Remove { id: "AC-1".into() },
            at,
        })
        .unwrap_err();
    assert!(err.contains("削除できません"), "{err}");
    rig.feed(Input::CriteriaEdit {
        edit: CriteriaEdit::Remove { id: "AC-3".into() },
        at,
    });
    rig.feed(Input::CriteriaEdit {
        edit: CriteriaEdit::Add {
            statement: "Quitting keeps the session".into(),
            how: "⌘Q, reopen".into(),
            priority: Priority::Should,
        },
        at,
    });
    rig.feed(Input::CriteriaEdit {
        edit: CriteriaEdit::Move {
            id: "AC-4".into(),
            to: 0,
        },
        at,
    });
    let s = rig.engine.session();
    let ids: Vec<&str> = s.criteria.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["AC-4", "AC-1", "AC-2"]);
    assert_eq!(s.criteria[0].source, CriterionSource::User);
    assert_eq!(s.criteria_edits.len(), 4);
    assert!(
        rig.log
            .iter()
            .flatten()
            .filter(|e| matches!(e, Event::CriteriaEdited { .. }))
            .count()
            == 4
    );
    let err = rig
        .try_feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Remove { id: "AC-4".into() },
            at,
        })
        .unwrap_err();
    assert!(err.contains("削除できません"), "{err}");

    // 検証を開始 freezes them; the worktrees come first, then round 1.
    let at = rig.tick();
    rig.feed(Input::UserStartVerify { edits: Vec::new(), at });
    assert_eq!(rig.engine.session().frozen_at, Some(at));
    assert!(rig.asked(&Effect::MakeWorktrees));
    assert!(rig.engine.session().rounds.is_empty());
    let err = rig
        .try_feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Move {
                id: "AC-1".into(),
                to: 0,
            },
            at,
        })
        .unwrap_err();
    assert!(err.contains("確認待ち"), "{err}");
    testkit::make_worktrees(&mut rig);
    assert_eq!(rig.round(), 1);
    assert_eq!(rig.phase(), Phase::Briefing);
    let brief = rig.brief("claude", BriefKind::Work);
    assert!(
        brief.contains("AC-4") && brief.contains("Open a tab from /tmp"),
        "{brief}"
    );
}

#[test]
fn saving_and_starting_commits_all_edits_and_freezing_in_one_replayable_batch() {
    for isolation in [Isolation::Worktree, Isolation::Shared] {
        let mut rig = Rig::build(Options { isolation, ..Options::default() });
        testkit::accept_default_criteria(&mut rig);
        // Priority menus save immediately, before the text fields do.
        let at = rig.tick();
        rig.feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Edit {
                id: "AC-1".into(),
                statement: None,
                how: None,
                priority: Some(Priority::Should),
            },
            at,
        });
        let before = rig.engine.session().clone();
        let at = rig.tick();
        let out = rig.feed(Input::UserStartVerify {
            edits: vec![
                CriteriaEdit::Edit {
                    id: "AC-1".into(),
                    statement: Some("  New tabs keep the focused cwd  ".into()),
                    how: None,
                    priority: None,
                },
                CriteriaEdit::Edit {
                    id: "AC-2".into(),
                    statement: None,
                    how: Some("  Interrupt a write, then reopen the store  ".into()),
                    priority: None,
                },
                CriteriaEdit::Add {
                    statement: "Closing one tab leaves the others open".into(),
                    how: "Open two tabs and close one".into(),
                    priority: Priority::Should,
                },
            ],
            at,
        });
        assert_eq!(out.batches[0].len(), 4);
        assert!(out.batches[0][..3].iter().all(|e| matches!(e, Event::CriteriaEdited { .. })));
        assert_eq!(out.batches[0][3], Event::CriteriaFrozen { at });
        let mut boundary = before;
        reduce::apply(&mut boundary, &out.batches[0]).unwrap();
        assert_eq!(boundary.frozen_at, Some(at));
        assert_eq!(boundary.criteria.len(), 4);
        assert_eq!(boundary.criteria[0].statement, "New tabs keep the focused cwd");
        assert_eq!(boundary.criteria[0].priority, Priority::Should);
        assert_eq!(boundary.criteria[1].how, "Interrupt a write, then reopen the store");
        if isolation == Isolation::Worktree {
            assert!(out.effects.contains(&Effect::MakeWorktrees));
            testkit::make_worktrees(&mut rig);
        }
        let session = rig.engine.session();
        let round = session.current_round().unwrap();
        assert_eq!(round.no, 1);
        for brief in &round.briefs {
            assert_eq!(brief.base_rev, session.rev);
            assert!(brief.body.contains("New tabs keep the focused cwd"));
            assert!(brief.body.contains("Interrupt a write, then reopen the store"));
            assert!(brief.body.contains("Closing one tab leaves the others open"));
        }
        assert_eq!(reduce::replay(&rig.log).unwrap(), *session);
    }
}

#[test]
fn a_refused_pending_edit_leaves_every_criterion_unfrozen_and_unchanged() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let before = rig.engine.session().clone();
    let log = rig.log.clone();
    let effects = rig.effects.clone();
    let at = rig.tick();
    let edit = CriteriaEdit::Edit {
        id: "AC-1".into(),
        statement: Some("This change must not be partially saved".into()),
        how: None,
        priority: None,
    };
    let error = rig.try_feed(Input::UserStartVerify {
        edits: vec![edit.clone(), CriteriaEdit::Add {
            statement: "An unfinished addition".into(),
            how: " ".into(),
            priority: Priority::Must,
        }],
        at,
    }).unwrap_err();
    assert!(error.contains(HOW_EMPTY), "{error}");
    assert_eq!(*rig.engine.session(), before);
    assert_eq!(rig.log, log);
    assert_eq!(rig.effects, effects);
    rig.feed(Input::UserStartVerify { edits: vec![edit], at });
    assert_eq!(rig.engine.session().frozen_at, Some(at));
}

#[test]
fn pending_additions_are_validated_against_each_other_before_any_is_saved() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let before = rig.engine.session().clone();
    let edits = (before.criteria.len()..=MAX_CRITERIA).map(|n| CriteriaEdit::Add {
        statement: format!("Check {n}"),
        how: "Run the check".into(),
        priority: Priority::Should,
    }).collect();
    let error = rig.try_feed(Input::UserStartVerify { edits, at: 99 }).unwrap_err();
    assert!(error.contains(&format!("{MAX_CRITERIA}件まで")), "{error}");
    assert_eq!(*rig.engine.session(), before);
    assert_eq!(rig.phase(), Phase::Confirming);
}

#[test]
fn ac6_with_confirmation_off_round_one_starts_as_soon_as_the_criteria_are_accepted() {
    let mut rig = Rig::build(Options {
        confirm: false,
        isolation: Isolation::Shared,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    let s = rig.engine.session();
    assert!(s.frozen_at.is_some());
    assert_eq!(rig.round(), 1, "round 1 started at once");
    assert_eq!(s.current_round().unwrap().briefs.len(), 3);
    assert!(!rig.asked(&Effect::MakeWorktrees), "shared mode makes none");
    let err = rig.try_feed(Input::UserStartVerify { edits: Vec::new(), at: 99 }).unwrap_err();
    assert!(err.contains("確認待ち"), "{err}");
}

#[test]
fn a_worktree_that_fails_refuses_round_one_until_retried() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let at = rig.tick();
    rig.feed(Input::UserStartVerify { edits: Vec::new(), at });
    rig.feed(Input::WorktreeFailed {
        part: p("codex"),
        path: PathBuf::from("/repo-wt/verify-ver-1-codex"),
        error: "src/tab.rs differs from the manifest".into(),
    });
    let now = rig.tick();
    rig.feed(Input::Tick { now });
    assert!(rig.engine.session().rounds.is_empty(), "round 1 refused");
    rig.effects.clear();
    rig.feed(Input::UserRetry);
    assert!(rig.asked(&Effect::MakeWorktrees));
    testkit::make_worktrees(&mut rig);
    assert_eq!(rig.round(), 1);
}

#[test]
fn ac16_the_round_runs_briefing_verifying_ingesting_ruling_and_the_judge_waits_for_every_rerun() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    assert_eq!(rig.phase(), Phase::Briefing);
    rig.deliver();
    assert_eq!(rig.phase(), Phase::Verifying);
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    assert_eq!(rig.phase(), Phase::Ingesting);
    assert!(rig.asked(&Effect::Ingest { round: 1 }));
    rig.ingest();
    for part in VERIFIERS {
        assert!(rig.asked(&Effect::Rerun {
            round: 1,
            part: p(part)
        }));
    }
    let stored = rig
        .engine
        .session()
        .evidence_item(&key("codex/r1/e1"))
        .and_then(|e| e.stored.clone())
        .unwrap();
    assert_eq!(stored.path, Path::new("evidence/r1/codex/logs/e1.log"));
    rig.rerun("claude", |_, exit| Rerun::Matched { exit, log: None });
    rig.rerun("codex", |_, _| Rerun::Differed {
        exit: Some(1),
        log: None,
    });
    let s = rig.engine.session();
    assert_eq!(
        rig.phase(),
        Phase::Ingesting,
        "claude-2's re-run is pending"
    );
    assert!(s.current_round().unwrap().judge_brief.is_none());
    let claude = s
        .current_round()
        .unwrap()
        .brief(&p("claude"), BriefKind::Rerun)
        .unwrap();
    assert!(
        claude.receipt.replied.is_some(),
        "every item has its outcome"
    );
    assert!(
        claude.body.contains("run_in_background option set") && claude.body.contains("    sh '"),
        "{}",
        claude.body
    );
    rig.rerun("claude-2", |_, _| Rerun::TimedOut { log: None });
    assert_eq!(rig.phase(), Phase::Ruling);
    let brief = rig.brief("judge", BriefKind::Work);
    assert!(brief.contains("re-run matched (exit 0)"), "{brief}");
    assert!(brief.contains("re-run differed (exit 1)"), "{brief}");
    assert!(brief.contains("re-run timed out"), "{brief}");
}

#[test]
fn ac16_a_verifier_that_fails_the_round_does_not_block_it() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    for part in ["claude", "claude-2"] {
        let at = rig.tick();
        rig.feed(Input::Sent {
            round: 1,
            part: p(part),
            kind: BriefKind::Work,
            at,
        });
    }
    for _ in 0..3 {
        rig.feed(Input::SendFailed {
            round: 1,
            part: p("codex"),
            kind: BriefKind::Work,
            error: "pane gone".into(),
        });
    }
    let codex = rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("codex"), BriefKind::Work)
        .unwrap()
        .receipt
        .clone();
    assert!(codex.failed.is_some());
    rig.reply("claude", &all_pass());
    rig.reply("claude-2", &all_pass());
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling);
    let brief = rig.brief("judge", BriefKind::Work);
    assert!(
        brief.contains("- codex (codex): no reply: send failed: pane gone"),
        "{brief}"
    );
}

#[test]
fn ac11_in_shared_mode_no_rerun_script_or_brief_is_made_and_every_item_is_not_run() {
    let mut rig = Rig::build(Options {
        isolation: Isolation::Shared,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    let s = rig.engine.session();
    assert!(
        !rig.effects
            .iter()
            .any(|e| matches!(e, Effect::Rerun { .. })),
        "no re-run script is asked for"
    );
    let round = s.current_round().unwrap();
    assert!(round.rerun_briefs.is_empty() && round.reruns.is_empty());
    for part in VERIFIERS {
        let item = s.evidence_item(&key(&format!("{part}/r1/e1"))).unwrap();
        assert_eq!(
            item.rerun,
            Some(Rerun::NotRun {
                why: SHARED_NOT_RUN.into(),
                log: None
            })
        );
    }
    assert_eq!(rig.phase(), Phase::Ruling);
    // A re-run script reported anyway is ignored.
    let out = rig.feed(Input::RerunWritten {
        round: 1,
        part: p("claude"),
        request: RerunRequest {
            nonce: "0".repeat(32),
            items: Vec::new(),
            script_digest: 0,
        },
        script: PathBuf::from("/repo/run.sh"),
    });
    assert!(out.batches.is_empty() && out.effects.is_empty());
}

#[test]
fn a_shared_brief_names_the_checkout_and_the_commit_its_head_must_be() {
    let mut rig = Rig::build(Options {
        isolation: Isolation::Shared,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    // The target is the ref feat/cwd: nothing says /repo has it out.
    let brief = rig.brief("codex", BriefKind::Work);
    let sha = testkit::HEAD_SHA;
    for line in [
        "Where you run: /repo — the person's own checkout".to_owned(),
        format!(
            "Its HEAD must be {sha}, the commit of feat/cwd you are verifying; the widget did not record which commit this checkout has out.\n"
        ),
        format!(
            "Before anything else, run `git rev-parse HEAD` there. If it does not print {sha}, this checkout is not the target: run nothing else, and report every criterion as blocked with a note naming the commit you saw.\n"
        ),
    ] {
        assert!(brief.contains(&line), "{line:?} in\n{brief}");
    }
    // A work-tree target: the checkout had that commit out at capture.
    let mut session = rig.engine.session().clone();
    session.manifest.head_ref = None;
    session.target.head = crate::model::types::Head::Worktree;
    let reply_dir = crate::store::verifier_reply_dir(&session, &p("codex"));
    let brief = crate::model::brief::verifier_brief(
        &session,
        &p("codex"),
        1,
        &reply_dir,
        &crate::store::reply_path(&reply_dir, 1, &p("codex")),
        &session.workdir,
    )
    .body;
    assert!(
        brief.contains(&format!(
            "Its HEAD must be {sha}: the commit it had out when the target was captured"
        )),
        "{brief}"
    );
    assert!(
        brief.contains("report every criterion as blocked"),
        "{brief}"
    );
}

#[test]
fn a_rerun_brief_that_never_starts_ends_its_items_not_run() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun("claude", |_, exit| Rerun::Matched { exit, log: None });
    rig.rerun("codex", |_, exit| Rerun::Matched { exit, log: None });
    let reply_dir = crate::store::verifier_reply_dir(rig.engine.session(), &p("claude-2"));
    rig.feed(Input::RerunWritten {
        round: 1,
        part: p("claude-2"),
        request: RerunRequest {
            nonce: "1".repeat(32),
            items: vec![crate::model::types::RerunItem {
                key: key("claude-2/r1/e1"),
                n: 1,
                cmd_digest: 3,
            }],
            script_digest: 4,
        },
        script: crate::store::rerun_dir(&reply_dir, 1, &p("claude-2")).join("run.sh"),
    });
    let sent = rig.tick();
    rig.feed(Input::Sent {
        round: 1,
        part: p("claude-2"),
        kind: BriefKind::Rerun,
        at: sent,
    });
    // Working on the brief is not starting the script.
    rig.feed(Input::AgentStateChanged {
        part: p("claude-2"),
        state: AgentState::Running,
        at: sent + 1,
    });
    let mut now = sent;
    for _ in 0..3 {
        now += 90_000;
        rig.feed(Input::Tick { now });
        rig.feed(Input::Sent {
            round: 1,
            part: p("claude-2"),
            kind: BriefKind::Rerun,
            at: now,
        });
    }
    now += 90_000;
    rig.feed(Input::Tick { now });
    let item = rig
        .engine
        .session()
        .evidence_item(&key("claude-2/r1/e1"))
        .unwrap()
        .clone();
    match item.rerun {
        Some(Rerun::NotRun { why, .. }) => {
            assert!(why.starts_with("re-run did not start"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.phase(), Phase::Ruling);
}

/// Round 1 where AC-1 fails for claude (passes for the others), AC-2
/// passes for codex alone, and AC-3 passes for everyone.
fn mixed_round_one(rig: &mut Rig) {
    testkit::accept_default_criteria(rig);
    testkit::start_round_one(rig);
    rig.deliver();
    rig.reply(
        "claude",
        &reply_json(
            &[
                ("AC-1", "fail", &["e1"]),
                ("AC-2", "not_tested", &[]),
                ("AC-3", "pass", &["e2"]),
            ],
            &[cmd("e1", 101, true), cmd("e2", 0, true)],
            "",
        ),
    );
    rig.reply(
        "codex",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[cmd("e1", 0, true)],
            "",
        ),
    );
    rig.reply(
        "claude-2",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "blocked", &[]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[cmd("e1", 0, true)],
            "",
        ),
    );
    rig.ingest();
    rig.rerun_all_matched();
}

const JUDGE_MIXED: &str = r#"{"summary": "AC-1 fails for claude.",
 "rulings": [
  {"criterion": "AC-1", "ruling": "failed", "rationale": "claude's test fails.", "evidence": ["claude/r1/e1"]},
  {"criterion": "AC-2", "ruling": "inconclusive", "rationale": "Only codex ran it."},
  {"criterion": "AC-3", "ruling": "verified", "rationale": "Matched.", "evidence": ["codex/r1/e1"]}
 ]}"#;

#[test]
fn ac15_without_assignments_every_fail_and_single_pass_goes_to_another_verifier_round_robin() {
    let mut rig = Rig::new(2);
    mixed_round_one(&mut rig);
    rig.judge(JUDGE_MIXED);
    assert_eq!(rig.round(), 2);
    let s = rig.engine.session();
    let assigned = &s.current_round().unwrap().assignments;
    let result = |part: &str, criterion: &str| Subject::Result {
        part: p(part),
        criterion: criterion.into(),
    };
    assert_eq!(
        *assigned,
        BTreeMap::from([
            (p("codex"), vec![result("claude", "AC-1")]),
            (p("claude-2"), vec![result("codex", "AC-2")]),
        ])
    );
    for (who, subjects) in assigned {
        for subject in subjects {
            assert!(
                !matches!(subject, Subject::Result { part, .. } if part == who),
                "{who} got its own result"
            );
        }
    }
    let brief = rig.brief("codex", BriefKind::Work);
    assert!(brief.contains("## 6. Your assignments\nReproduce or refute each of these in your own worktree, with your own evidence:\n- claude/AC-1"), "{brief}");
    // The round-robin wraps past the reporter: with every verifier failing
    // AC-1, each fail goes to the next verifier.
    let mut session = s.clone();
    session.results.retain(|r| r.round == 1);
    for r in &mut session.results {
        if r.criterion == "AC-1" {
            r.outcome = Outcome::Fail;
        } else {
            r.outcome = Outcome::NotTested;
        }
    }
    let wrap = rules::default_assignments(&session, 2);
    assert_eq!(
        wrap,
        BTreeMap::from([
            (p("codex"), vec![result("claude", "AC-1")]),
            (p("claude-2"), vec![result("codex", "AC-1")]),
            (p("claude"), vec![result("claude-2", "AC-1")]),
        ])
    );
}

#[test]
fn ac15_the_judges_assignments_replace_the_default() {
    let mut rig = Rig::new(2);
    mixed_round_one(&mut rig);
    rig.judge(&JUDGE_MIXED.replace(
        "\"rulings\"",
        "\"assign\": {\"claude\": [\"codex/AC-2\"]}, \"rulings\"",
    ));
    let assigned = &rig.engine.session().current_round().unwrap().assignments;
    assert_eq!(
        *assigned,
        BTreeMap::from([(
            p("claude"),
            vec![Subject::Result {
                part: p("codex"),
                criterion: "AC-2".into()
            }]
        )])
    );
}

/// Round 1's rulings, each citing the matched commands of two verifiers;
/// without its closing brace, for [`CLOSE_PASS`].
const JUDGE_R1_TWO: &str = r#"{"summary": "All three hold.",
 "rulings": [
  {"criterion": "AC-1", "ruling": "verified", "rationale": "Re-runs matched.", "evidence": ["claude/r1/e1", "codex/r1/e1"]},
  {"criterion": "AC-2", "ruling": "verified", "rationale": "Re-runs matched.", "evidence": ["codex/r1/e1", "claude-2/r1/e1"]},
  {"criterion": "AC-3", "ruling": "verified", "rationale": "Re-runs matched.", "evidence": ["claude-2/r1/e1", "claude/r1/e1"]}
 ]"#;

const CLOSE_PASS: &str = r#", "verdict": {"decision": "pass", "rationale": "Everything holds.", "blocking": []}, "close": true}"#;

#[test]
fn ac17_round_one_closes_early_when_every_criterion_has_two_matched_passes() {
    let mut rig = Rig::new(3);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun_all_matched();
    // A ruling citing one verifier per criterion meets the standard (one
    // matched re-run is enough for verified) but rests on one verifier:
    // no early close.
    let one = format!("{}{CLOSE_PASS}", JUDGE_R1.trim_end().trim_end_matches('}'));
    let session = rig.engine.session().clone();
    let why = crate::model::validate::parse_judge(&one, &session, 1).unwrap_err();
    assert_eq!(
        why.problems,
        [
            "close: not allowed in round 1 unless every criterion is verified by two verifiers with matched re-runs, its ruling citing each one's matched command"
        ],
        "{why}"
    );
    rig.judge(&format!("{JUDGE_R1_TWO}{CLOSE_PASS}"));
    let s = rig.engine.session();
    assert!(s.closed, "{:?}", rig.rejected("judge"));
    assert_eq!(
        s.verdict().map(|v| v.decision),
        Some(crate::model::types::Decision::Pass)
    );
    assert!(rig.asked(&Effect::RemoveWorktrees));
}

#[test]
fn ac17_round_one_may_not_close_with_a_criterion_on_one_matched_pass() {
    let mut rig = Rig::new(3);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &all_pass());
    // codex's re-run differs, claude-2 did not test AC-3.
    rig.reply("codex", &all_pass());
    rig.reply(
        "claude-2",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "not_tested", &[]),
            ],
            &[cmd("e1", 0, true)],
            "",
        ),
    );
    rig.ingest();
    rig.rerun("claude", |_, exit| Rerun::Matched { exit, log: None });
    rig.rerun("codex", |_, _| Rerun::Differed {
        exit: Some(2),
        log: None,
    });
    rig.rerun("claude-2", |_, exit| Rerun::Matched { exit, log: None });
    // Every ruling meets the standard (claude's matched e1 backs each
    // criterion); only the early close is refused.
    let judge = JUDGE_R1
        .replace("codex/r1/e1", "claude/r1/e1")
        .replace("claude-2/r1/e1", "claude/r1/e1");
    let judge = format!("{}{CLOSE_PASS}", judge.trim_end().trim_end_matches('}'));
    rig.judge(&judge);
    assert!(!rig.engine.session().closed);
    let why = rig.rejected("judge").unwrap();
    assert!(why.contains("close: not allowed in round 1"), "{why}");
}

#[test]
fn ac17_round_one_may_not_close_on_matched_passes_its_rulings_do_not_cite() {
    let mut rig = Rig::new(3);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    // The person's screenshot shows every criterion.
    rig.feed(Input::AttachEvidence {
        key: EvidenceKey::user(1),
        body: EvidenceBody::File {
            path: "all.png".into(),
            caption: Some("every criterion holds".into()),
            media: Some(Media::Png),
            marks: Vec::new(),
            shows: Vec::new(),
        },
        stored: Stored {
            path: PathBuf::from("evidence/user/1.png"),
            bytes: 2048,
            digest: 77,
        },
        shows: vec!["AC-1".into(), "AC-2".into(), "AC-3".into()],
        at: 60,
    });
    rig.deliver();
    // claude and codex pass everything on matched re-runs; claude-2
    // passes everything on a command it does not re-run.
    rig.reply("claude", &all_pass());
    rig.reply("codex", &all_pass());
    rig.reply(
        "claude-2",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[cmd("e1", 0, false)],
            "",
        ),
    );
    rig.ingest();
    rig.rerun_all_matched();
    // Each ruling rests on claude-2 and the person, two sources: verified
    // holds, but no verifier it rests on has a matched re-run.
    let ruling = |criterion: &str| {
        format!(
            r#"{{"criterion": "{criterion}", "ruling": "verified", "rationale": "claude-2's test and the screenshot.", "evidence": ["claude-2/r1/e1", "user/1"]}}"#
        )
    };
    let rulings = format!(
        r#"{{"summary": "All three hold.", "rulings": [{}, {}, {}]"#,
        ruling("AC-1"),
        ruling("AC-2"),
        ruling("AC-3"),
    );
    let session = rig.engine.session().clone();
    let open = format!("{rulings}}}");
    assert!(
        crate::model::validate::parse_judge(&open, &session, 1).is_ok(),
        "the rulings alone meet the standard"
    );
    rig.judge(&format!("{rulings}{CLOSE_PASS}"));
    assert!(!rig.engine.session().closed);
    let why = rig.rejected("judge").unwrap();
    assert!(why.contains("close: not allowed in round 1"), "{why}");
}

#[test]
fn ac31_person_attached_media_is_logged_listed_for_the_judge_and_counts_only_beside_a_pass() {
    let shot = |n: u32| {
        (
            EvidenceKey::user(n),
            EvidenceBody::File {
                path: format!("tab-{n}.png"),
                caption: Some("pwd in the new tab".into()),
                media: Some(Media::Png),
                marks: Vec::new(),
                shows: Vec::new(),
            },
            Stored {
                path: PathBuf::from(format!("evidence/user/{n}.png")),
                bytes: 2048,
                digest: 77,
            },
        )
    };
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let (k, body, stored) = shot(1);
    let early = rig.try_feed(Input::AttachEvidence {
        key: k.clone(),
        body: body.clone(),
        stored: stored.clone(),
        shows: vec!["AC-1".into()],
        at: 50,
    });
    assert!(
        early.unwrap_err().contains("開始後"),
        "refused before round 1"
    );
    testkit::start_round_one(&mut rig);
    rig.feed(Input::AttachEvidence {
        key: k.clone(),
        body,
        stored,
        shows: vec!["AC-1".into()],
        at: 60,
    });
    let item = rig.engine.session().evidence_item(&k).unwrap().clone();
    assert_eq!(
        item.attached,
        Some(crate::model::types::Attachment { round: 1, at: 60 })
    );
    assert!(matches!(&item.body, EvidenceBody::File { shows, .. } if shows == &["AC-1"]));
    assert!(
        rig.log
            .iter()
            .flatten()
            .any(|e| matches!(e, Event::EvidenceAttached { .. }))
    );
    // claude passes AC-1 on a command it does not re-run; nobody else
    // passes it.
    rig.deliver();
    rig.reply(
        "claude",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "not_tested", &[]),
                ("AC-3", "not_tested", &[]),
            ],
            &[cmd("e1", 0, false)],
            "",
        ),
    );
    for part in ["codex", "claude-2"] {
        rig.reply(
            part,
            &reply_json(
                &[
                    ("AC-1", "not_tested", &[]),
                    ("AC-2", "not_tested", &[]),
                    ("AC-3", "not_tested", &[]),
                ],
                &[],
                "",
            ),
        );
    }
    rig.ingest();
    assert_eq!(rig.phase(), Phase::Ruling);
    let brief = rig.brief("judge", BriefKind::Work);
    assert!(
        brief.contains(
            "Media the person attached (cite as user/<n>):\n  - user/1: PNG image tab-1.png"
        ),
        "{brief}"
    );
    let ruling = |evidence: &str| {
        format!(
            r#"{{"summary": "s", "rulings": [
              {{"criterion": "AC-1", "ruling": "verified", "rationale": "r", "evidence": [{evidence}]}},
              {{"criterion": "AC-2", "ruling": "inconclusive", "rationale": "untested"}},
              {{"criterion": "AC-3", "ruling": "inconclusive", "rationale": "untested"}}]}}"#
        )
    };
    let session = rig.engine.session().clone();
    let alone =
        crate::model::validate::parse_judge(&ruling(r#""user/1""#), &session, 1).unwrap_err();
    assert!(
        alone.problems.contains(
            &"AC-1: verified needs a matched re-run or two independent passes".to_owned()
        ),
        "{alone}"
    );
    let one_pass =
        crate::model::validate::parse_judge(&ruling(r#""claude/r1/e1""#), &session, 1).unwrap_err();
    assert!(
        one_pass.to_string().contains("two independent passes"),
        "{one_pass}"
    );
    rig.judge(&ruling(r#""claude/r1/e1", "user/1""#));
    assert!(
        rig.engine.session().round(1).unwrap().outcome.is_some(),
        "{:?}",
        rig.rejected("judge")
    );
    // Closed: no more attachments.
    rig.feed(Input::UserClose);
    let (k, body, stored) = shot(2);
    let late = rig.try_feed(Input::AttachEvidence {
        key: k,
        body,
        stored,
        shows: vec!["AC-1".into()],
        at: 999,
    });
    assert!(late.unwrap_err().contains("終了"));
}

#[test]
fn ac31_person_attached_marks_are_held_to_the_verifiers_rules() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    let mark = |t_ms: u64, note: &str| crate::model::types::Mark {
        t_ms,
        note: note.into(),
    };
    let attach = |rig: &mut Rig, n: u32, media: Media, marks: Vec<crate::model::types::Mark>| {
        rig.try_feed(Input::AttachEvidence {
            key: EvidenceKey::user(n),
            body: EvidenceBody::File {
                path: format!("clip-{n}.{}", media.extension()),
                caption: Some("cd /tmp, ⌘T, pwd".into()),
                media: Some(media),
                marks,
                shows: Vec::new(),
            },
            stored: Stored {
                path: PathBuf::from(format!("evidence/user/{n}.{}", media.extension())),
                bytes: 2048,
                digest: 7,
            },
            shows: vec!["AC-1".into()],
            at: 70,
        })
    };
    let refused = |result: Result<Output, String>| result.map(|_| ()).unwrap_err();
    assert_eq!(
        refused(attach(&mut rig, 1, Media::Png, vec![mark(0, "the tab")])),
        "マークを付けられるのは動画だけです"
    );
    let in_order = |n: u64| (0..n).map(|i| mark(i * 1000, "a moment")).collect();
    assert_eq!(
        refused(attach(&mut rig, 1, Media::QuickTime, in_order(21))),
        "マークは20件までです"
    );
    assert_eq!(
        refused(attach(&mut rig, 1, Media::QuickTime, vec![mark(0, " ")])),
        "マークごとに何が見えるかを書いてください"
    );
    assert!(rig.engine.session().evidence.is_empty(), "nothing logged");
    attach(&mut rig, 1, Media::QuickTime, in_order(20)).unwrap();
    let item = rig.engine.session().evidence_item(&EvidenceKey::user(1));
    assert!(
        matches!(item.map(|e| &e.body), Some(EvidenceBody::File { marks, .. }) if marks.len() == 20)
    );
}

#[test]
fn a_rejected_reply_is_cleared_corrected_and_fails_after_the_retries() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    let bad = r#"{"summary": "", "results": []}"#;
    let out = rig.reply("claude", bad);
    let reply_path = rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("claude"), BriefKind::Work)
        .unwrap()
        .reply_path
        .clone();
    assert!(
        out.effects
            .contains(&Effect::ClearReply { path: reply_path })
    );
    let note = out
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::SendBrief { body, .. } => Some(body.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        note.starts_with("Your reply JSON could not be used:"),
        "{note}"
    );
    assert!(note.contains("- \"summary\" is empty"), "{note}");
    assert!(
        note.contains("- results: missing AC-1, AC-2, AC-3"),
        "{note}"
    );
    for _ in 0..2 {
        rig.reply("claude", bad);
    }
    let receipt = rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("claude"), BriefKind::Work)
        .unwrap()
        .receipt
        .clone();
    assert!(
        receipt
            .failed
            .unwrap()
            .starts_with("invalid output after retries")
    );
}

#[test]
fn the_last_round_needs_a_verdict_then_the_verification_closes_and_removes_its_worktrees() {
    let mut rig = Rig::new(1);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun_all_matched();
    rig.judge(JUDGE_R1);
    assert!(
        rig.rejected("judge")
            .unwrap()
            .contains("verdict: required, this is the last round")
    );
    let judge = format!(
        "{}{}",
        JUDGE_R1.trim_end().trim_end_matches('}'),
        r#", "verdict": {"decision": "pass", "rationale": "Holds.", "blocking": []}}"#
    );
    rig.judge(&judge);
    let s = rig.engine.session();
    assert!(s.closed);
    assert!(rig.asked(&Effect::RemoveWorktrees));
    assert!(rig.asked(&Effect::WriteReport));
    // Removal is logged after the close.
    for part in VERIFIERS {
        rig.feed(Input::WorktreeRemoved { part: p(part) });
    }
    assert!(
        rig.engine
            .session()
            .worktrees
            .values()
            .all(|t| t.state == WorktreeState::Removed)
    );
}

#[test]
fn a_held_verification_waits_after_a_ruling_and_an_extension_rebuilds_removed_worktrees() {
    let mut rig = Rig::new(2);
    rig.feed(Input::UserSetHold { hold: true, at: 11 });
    testkit::through_round_one(&mut rig);
    assert!(rig.engine.session().waits_to_continue());
    assert_eq!(rig.round(), 1, "held after the ruling");
    let at = rig.tick();
    rig.feed(Input::UserContinue { at });
    assert_eq!(rig.round(), 2);
    // Round 2 (the last) ends in a verdict; the worktrees are removed.
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, r#"{"summary": "Nothing more."}"#);
    }
    rig.ingest();
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    rig.judge(&format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}], "verdict": {{"decision": "pass", "rationale": "Holds.", "blocking": []}}}}"#,
        same("AC-1"),
        same("AC-2"),
        same("AC-3")
    ));
    assert!(rig.engine.session().closed, "{:?}", rig.rejected("judge"));
    for part in VERIFIERS {
        rig.feed(Input::WorktreeRemoved { part: p(part) });
    }
    // +1: open again and, being held, waiting; 次のラウンドへ makes the
    // removed worktrees anew first, then the round starts.
    let made_before = rig
        .effects
        .iter()
        .filter(|e| **e == Effect::MakeWorktrees)
        .count();
    let at = rig.tick();
    rig.feed(Input::UserExtend { by: 1, at });
    let s = rig.engine.session();
    assert!(!s.closed && s.max_rounds == 3 && s.verdict().is_none());
    assert!(s.waits_to_continue(), "held after the extension too");
    let at = rig.tick();
    rig.feed(Input::UserContinue { at });
    assert_eq!(
        rig.effects
            .iter()
            .filter(|e| **e == Effect::MakeWorktrees)
            .count(),
        made_before + 1,
        "the removed worktrees are rebuilt"
    );
    assert_eq!(rig.round(), 2, "no round before its worktrees exist");
    testkit::make_worktrees(&mut rig);
    let s = rig.engine.session();
    assert!(s.worktrees.values().all(|t| t.state == WorktreeState::Made));
    assert_eq!(rig.round(), 3);
    assert_eq!(rig.phase(), Phase::Briefing);
}

#[test]
fn a_hold_set_while_an_extension_rebuilds_the_worktrees_stops_before_its_round() {
    let mut rig = Rig::new(2);
    testkit::through_round_one(&mut rig);
    assert_eq!(rig.round(), 2);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, r#"{"summary": "Nothing more."}"#);
    }
    rig.ingest();
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    rig.judge(&format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}], "verdict": {{"decision": "pass", "rationale": "Holds.", "blocking": []}}}}"#,
        same("AC-1"),
        same("AC-2"),
        same("AC-3")
    ));
    assert!(rig.engine.session().closed, "{:?}", rig.rejected("judge"));
    for part in VERIFIERS {
        rig.feed(Input::WorktreeRemoved { part: p(part) });
    }
    // +1 while not held: the worktrees are made anew for round 3.
    let at = rig.tick();
    rig.feed(Input::UserExtend { by: 1, at });
    assert_eq!(rig.effects.last(), Some(&Effect::MakeWorktrees));
    // ラウンド後に停止 turned on while they are made.
    let at = rig.tick();
    rig.feed(Input::UserSetHold { hold: true, at });
    let sends = |rig: &Rig| {
        rig.effects
            .iter()
            .filter(|e| matches!(e, Effect::SendBrief { .. }))
            .count()
    };
    let sent = sends(&rig);
    testkit::make_worktrees(&mut rig);
    assert_eq!(rig.round(), 2, "held: round 3 waits for 次のラウンドへ");
    assert_eq!(sends(&rig), sent, "no brief before 次のラウンドへ");
    assert!(rig.engine.session().waits_to_continue());
    let at = rig.tick();
    rig.feed(Input::UserContinue { at });
    assert_eq!(rig.round(), 3);
    assert_eq!(sends(&rig), sent + VERIFIERS.len());
}

#[test]
fn a_verification_ended_by_hand_cannot_be_extended() {
    let mut rig = Rig::new(2);
    testkit::through_round_one(&mut rig);
    assert_eq!(rig.round(), 2);
    rig.feed(Input::UserClose);
    for part in VERIFIERS {
        rig.feed(Input::WorktreeRemoved { part: p(part) });
    }
    let err = rig
        .try_feed(Input::UserExtend { by: 1, at: 950 })
        .unwrap_err();
    assert!(err.contains("ラウンド上限の前に終了"), "{err}");
}

/// The engine resumed from `rig`'s log cut right after the first batch
/// holding an event `cut` matches, and its output on `Start`.
fn resumed_after(rig: &Rig, cut: impl Fn(&Event) -> bool) -> (Engine, Output) {
    let end = rig
        .log
        .iter()
        .position(|batch| batch.iter().any(&cut))
        .expect("the log holds the batch to cut after");
    let session = reduce::replay(&rig.log[..=end]).unwrap();
    let mut engine = Engine::resume(
        session,
        Path::new("/data/verifications/ver-1"),
        Config::default(),
    );
    let out = engine.handle(Input::Start { now: 5_000 }).unwrap();
    (engine, out)
}

#[test]
fn ac11_a_shared_log_cut_between_ingest_and_not_run_resumes_without_a_rerun_script() {
    let mut rig = Rig::build(Options {
        isolation: Isolation::Shared,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    // Cut after the last ingest (codex's: replies are ingested in id
    // order), before its not_run batch.
    let (engine, out) = resumed_after(
        &rig,
        |e| matches!(e, Event::EvidenceIngested { part, .. } if part.as_str() == "codex"),
    );
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, Effect::Rerun { .. })),
        "no re-run script is asked for in 共有 mode: {:?}",
        out.effects
    );
    let s = engine.session();
    assert_eq!(
        s.evidence_item(&key("codex/r1/e1")).unwrap().rerun,
        Some(Rerun::NotRun {
            why: SHARED_NOT_RUN.into(),
            log: None
        })
    );
    assert_eq!(s.phase(), Phase::Ruling, "the judge is briefed");
}

#[test]
fn ac17_an_early_close_cut_before_its_session_closed_stays_closed_on_resume() {
    let mut rig = Rig::new(3);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun_all_matched();
    rig.judge(&format!("{JUDGE_R1_TWO}{CLOSE_PASS}"));
    assert!(rig.engine.session().closed, "{:?}", rig.rejected("judge"));
    let (engine, out) = resumed_after(&rig, |e| matches!(e, Event::JudgeAccepted { .. }));
    let s = engine.session();
    assert!(s.closed, "the verdict ends it");
    assert_eq!(s.rounds.len(), 1, "no round 2");
    assert_eq!(
        s.verdict().map(|v| v.decision),
        Some(crate::model::types::Decision::Pass)
    );
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, Effect::SendBrief { .. })),
        "{:?}",
        out.effects
    );
    assert!(out.effects.contains(&Effect::RemoveWorktrees));
    assert!(out.effects.contains(&Effect::WriteReport));
}

#[test]
fn a_started_rerun_script_is_never_sent_again() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun("claude", |_, exit| Rerun::Matched { exit, log: None });
    rig.rerun("codex", |_, exit| Rerun::Matched { exit, log: None });
    let part = p("claude-2");
    let reply_dir = crate::store::verifier_reply_dir(rig.engine.session(), &part);
    rig.feed(Input::RerunWritten {
        round: 1,
        part: part.clone(),
        request: RerunRequest {
            nonce: "1".repeat(32),
            items: vec![crate::model::types::RerunItem {
                key: key("claude-2/r1/e1"),
                n: 1,
                cmd_digest: 3,
            }],
            script_digest: 4,
        },
        script: crate::store::rerun_dir(&reply_dir, 1, &part).join("run.sh"),
    });
    let at = rig.tick();
    rig.feed(Input::Sent {
        round: 1,
        part: part.clone(),
        kind: BriefKind::Rerun,
        at,
    });
    let at = rig.tick();
    rig.feed(Input::RerunStarted {
        round: 1,
        part: part.clone(),
        at,
    });
    // The pane errs, a send fails late, the pane is lost: the detached
    // script runs on, nothing is sent again.
    let at = rig.tick();
    for input in [
        Input::AgentStateChanged {
            part: part.clone(),
            state: AgentState::Error,
            at,
        },
        Input::SendFailed {
            round: 1,
            part: part.clone(),
            kind: BriefKind::Rerun,
            error: "pane gone".into(),
        },
        Input::Redeliver {
            round: 1,
            part: part.clone(),
            kind: BriefKind::Rerun,
        },
        Input::Tick {
            now: at + 3_600_000,
        },
    ] {
        let out = rig.feed(input);
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::SendBrief { .. })),
            "{:?}",
            out.effects
        );
    }
    let receipt = rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&part, BriefKind::Rerun)
        .unwrap()
        .receipt
        .clone();
    assert_eq!((receipt.retries, receipt.failed), (0, None));
    // The watch takes it to its outcome.
    let at = rig.tick();
    rig.feed(Input::RerunReported {
        round: 1,
        key: key("claude-2/r1/e1"),
        rerun: Rerun::Matched { exit: 0, log: None },
        at,
    });
    assert_eq!(rig.phase(), Phase::Ruling);
}

#[test]
fn a_criterion_cannot_be_left_without_how_to_verify_it() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let at = rig.tick();
    let err = rig
        .try_feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Edit {
                id: "AC-1".into(),
                statement: None,
                how: Some("  ".into()),
                priority: None,
            },
            at,
        })
        .unwrap_err();
    assert_eq!(err, HOW_EMPTY);
    let err = rig
        .try_feed(Input::CriteriaEdit {
            edit: CriteriaEdit::Add {
                statement: "Quitting keeps the session".into(),
                how: " \n ".into(),
                priority: Priority::Should,
            },
            at,
        })
        .unwrap_err();
    assert_eq!(err, HOW_EMPTY);
    let s = rig.engine.session();
    assert!(s.criteria_edits.is_empty());
    assert!(s.criteria.iter().all(|c| !c.how.trim().is_empty()));
}

#[test]
fn the_ruling_brief_shows_the_tail_of_a_failing_log_only_a_defect_cites() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &all_pass());
    rig.reply("codex", &all_pass());
    rig.reply(
        "claude-2",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[cmd("e1", 0, true), cmd("e2", 1, false)],
            r#", "defects": [{"id": "1", "claim": "The store loses the last line", "severity": "high", "category": "regression",
               "confidence": "high", "repro": "1. kill -9 2. reopen", "evidence": ["e2"]}]"#,
        ),
    );
    let tails = std::collections::BTreeMap::from([(
        key("claude-2/r1/e2"),
        "write store\npanicked: lost line\n".to_owned(),
    )]);
    rig.ingest_except(&[], &tails);
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    let brief = rig.brief("judge", BriefKind::Work);
    let defects = &brief[brief.find("## 6. Defects").unwrap()..brief.find("## 7.").unwrap()];
    assert!(
        defects.contains("  - claude-2/r1/e2: command `cargo test e2` (cwd .) exit 1"),
        "{defects}"
    );
    assert!(
        defects.contains("    log tail:\n```\nwrite store\npanicked: lost line\n```"),
        "{defects}"
    );
}

#[test]
fn a_log_whose_reply_paths_were_edited_is_not_its_own() {
    let mut rig = Rig::new(2);
    testkit::through_round_one(&mut rig);
    let mut session = rig.engine.session().clone();
    let dir = Path::new("/data/verifications/ver-1");
    assert!(check_reply_paths(&session).is_ok());
    session.rounds[0].briefs[0].reply_path = PathBuf::from("/elsewhere/reply.json");
    let err = check_reply_paths(&session).unwrap_err();
    assert!(err.contains("R1 の claude の返答パス"), "{err}");
    let err = check_store_paths(&session, &dir.join("..").join("ver-2")).unwrap_err();
    assert!(err.contains("フォルダ名と一致しません"), "{err}");
}

#[test]
fn a_rerun_script_that_started_before_its_send_was_logged_is_not_sent_again_on_resume() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun("claude", |_, exit| Rerun::Matched { exit, log: None });
    rig.rerun("codex", |_, exit| Rerun::Matched { exit, log: None });
    let part = p("claude-2");
    let reply_dir = crate::store::verifier_reply_dir(rig.engine.session(), &part);
    rig.feed(Input::RerunWritten {
        round: 1,
        part: part.clone(),
        request: RerunRequest {
            nonce: "1".repeat(32),
            items: vec![crate::model::types::RerunItem {
                key: key("claude-2/r1/e1"),
                n: 1,
                cmd_digest: 3,
            }],
            script_digest: 4,
        },
        script: crate::store::rerun_dir(&reply_dir, 1, &part).join("run.sh"),
    });
    // The first `.pid` is seen before `Sent` is logged; the widget stops.
    let at = rig.tick();
    rig.feed(Input::RerunStarted {
        round: 1,
        part: part.clone(),
        at,
    });
    let (mut engine, out) = resumed_after(
        &rig,
        |e| matches!(e, Event::BriefStarted { to, kind: BriefKind::Rerun, .. } if *to == part),
    );
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, Effect::SendBrief { .. })),
        "{:?}",
        out.effects
    );
    // The watch takes it to its outcome.
    engine
        .handle(Input::RerunReported {
            round: 1,
            key: key("claude-2/r1/e1"),
            rerun: Rerun::Matched { exit: 0, log: None },
            at: 5_010,
        })
        .unwrap();
    assert_eq!(engine.session().phase(), Phase::Ruling);
}

#[test]
fn an_extension_waits_until_the_ended_verifications_worktrees_are_removed() {
    let mut rig = Rig::new(1);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun_all_matched();
    rig.judge(&format!(
        "{}{}",
        JUDGE_R1.trim_end().trim_end_matches('}'),
        r#", "verdict": {"decision": "pass", "rationale": "Holds.", "blocking": []}}"#
    ));
    assert!(rig.engine.session().closed, "{:?}", rig.rejected("judge"));
    assert!(rig.asked(&Effect::RemoveWorktrees));
    let made_before = rig
        .effects
        .iter()
        .filter(|e| **e == Effect::MakeWorktrees)
        .count();
    // Before the removal is done, and while one worktree is still left.
    for removed in [0, 2] {
        for part in &VERIFIERS[..removed] {
            rig.feed(Input::WorktreeRemoved { part: p(part) });
        }
        let at = rig.tick();
        let err = rig.try_feed(Input::UserExtend { by: 1, at }).unwrap_err();
        assert!(
            err.contains("ワークツリーの削除が終わっていません"),
            "{err}"
        );
        let s = rig.engine.session();
        assert!(s.closed && s.max_rounds == 1 && s.rounds.len() == 1);
    }
    rig.feed(Input::WorktreeRemoved {
        part: p("claude-2"),
    });
    let at = rig.tick();
    let out = rig.feed(Input::UserExtend { by: 1, at });
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, Effect::SendBrief { .. })),
        "no brief before the worktrees are made again: {:?}",
        out.effects
    );
    assert_eq!(
        rig.effects
            .iter()
            .filter(|e| **e == Effect::MakeWorktrees)
            .count(),
        made_before + 1
    );
    testkit::make_worktrees(&mut rig);
    let s = rig.engine.session();
    assert!(s.worktrees.values().all(|t| t.state == WorktreeState::Made));
    assert_eq!((rig.round(), rig.phase()), (2, Phase::Briefing));
}

#[test]
fn accepted_criteria_are_reported_while_they_wait_for_confirmation() {
    let mut rig = Rig::new(2);
    assert!(!rig.asked(&Effect::WriteReport));
    testkit::accept_default_criteria(&mut rig);
    assert_eq!(rig.phase(), Phase::Confirming);
    assert!(rig.asked(&Effect::WriteReport));
}

#[test]
fn the_ruling_brief_lists_the_media_only_a_defect_cites() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &all_pass());
    rig.reply("codex", &all_pass());
    rig.reply(
        "claude-2",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[
                cmd("e1", 0, true),
                r#"{"id": "p1", "kind": "file", "path": "shots/lost.png", "caption": "the last line is gone after reopening"}"#
                    .to_owned(),
            ],
            r#", "defects": [{"id": "1", "claim": "The store loses the last line", "severity": "high", "category": "regression",
               "confidence": "high", "repro": "1. kill -9 2. reopen", "evidence": ["p1"]}]"#,
        ),
    );
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    let brief = rig.brief("judge", BriefKind::Work);
    let defects = &brief[brief.find("## 6. Defects").unwrap()..brief.find("## 7.").unwrap()];
    assert!(
        defects.contains(
            "  - claude-2/r1/p1: PNG image shots/lost.png — \"the last line is gone after reopening\"\n    stored: /repo/.noa/verify/ver-1/evidence/r1/claude-2/shots/lost.png (100 bytes, digest "
        ),
        "{defects}"
    );
}

#[test]
fn a_corrected_brief_redelivered_to_a_new_pane_goes_out_in_full() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    // claude's reply is turned down and cleared; the correction goes out,
    // then the verification stops.
    rig.reply("claude", r#"{"summary": "", "results": []}"#);
    let at = rig.tick();
    rig.feed(Input::Sent {
        round: 1,
        part: p("claude"),
        kind: BriefKind::Work,
        at,
    });
    // Resumed with claude's pane gone: a new pane gets the brief again.
    let mut engine = Engine::resume(
        reduce::replay(&rig.log).unwrap(),
        Path::new("/data/verifications/ver-1"),
        Config::default(),
    );
    engine.handle(Input::Start { now: 5_000 }).unwrap();
    engine
        .handle(Input::PaneSpawned {
            part: p("claude"),
            pane_id: 9,
        })
        .unwrap();
    let out = engine
        .handle(Input::Redeliver {
            round: 1,
            part: p("claude"),
            kind: BriefKind::Work,
        })
        .unwrap();
    let brief = engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("claude"), BriefKind::Work)
        .unwrap();
    assert!(brief.receipt.rejected.is_some());
    assert_eq!(
        out.effects,
        [Effect::SendBrief {
            round: 1,
            part: p("claude"),
            kind: BriefKind::Work,
            body: brief.body.clone(),
        }]
    );
    for want in ["Tabs inherit the focused pane's cwd", "## "] {
        assert!(brief.body.contains(want), "{want:?} in\n{}", brief.body);
    }
}

#[test]
fn a_worktree_made_after_the_verification_closed_is_recorded_and_removed() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let at = rig.tick();
    rig.feed(Input::UserStartVerify { edits: Vec::new(), at });
    assert!(rig.asked(&Effect::MakeWorktrees));
    // 終了 before the runner reports any worktree: none is recorded yet.
    let out = rig.feed(Input::UserClose);
    assert_eq!(out.effects, [Effect::WriteReport]);
    let session = rig.engine.session().clone();
    for part in session.verifier_ids() {
        let path = crate::worktree::path_for(&session.main_worktree, &session.id, part);
        let out = rig.feed(Input::WorktreeMade {
            part: part.clone(),
            path: path.clone(),
        });
        assert_eq!(out.effects, [Effect::RemoveWorktrees]);
        assert_eq!(
            rig.engine
                .session()
                .worktrees
                .get(part)
                .map(|t| (&t.path, &t.state)),
            Some((&path, &WorktreeState::Made))
        );
    }
    for part in VERIFIERS {
        rig.feed(Input::WorktreeRemoved { part: p(part) });
    }
    assert!(
        rig.engine
            .session()
            .worktrees
            .values()
            .all(|t| t.state == WorktreeState::Removed)
    );
    assert_eq!(rig.round(), 0, "no round started");
}
