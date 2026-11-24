use super::*;
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
    rig.feed(Input::UserStartVerify { at });
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
    let err = rig.try_feed(Input::UserStartVerify { at: 99 }).unwrap_err();
    assert!(err.contains("確認待ち"), "{err}");
}

#[test]
fn a_worktree_that_fails_refuses_round_one_until_retried() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    let at = rig.tick();
    rig.feed(Input::UserStartVerify { at });
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
    assert!(claude.body.contains("nohup sh '"), "{}", claude.body);
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
    let judge = format!("{}{CLOSE_PASS}", JUDGE_R1.trim_end().trim_end_matches('}'));
    rig.judge(&judge);
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
    let judge = JUDGE_R1.replace("codex/r1/e1", "claude/r1/e1");
    let judge = format!("{}{CLOSE_PASS}", judge.trim_end().trim_end_matches('}'));
    rig.judge(&judge);
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
    assert_eq!(rig.round(), 1);
    rig.feed(Input::UserContinue { at: 900 });
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
