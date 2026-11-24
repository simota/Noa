use super::*;
use crate::model::types::{
    AgentKind, AgentSetup, Agreement, BuiltinLens, Decision, FindingStatus, LensChoice,
    ReviewerChoice, StanceKind,
};
use crate::testkit::{self, LEAD_R1, Rig, blind, p, review, through_round_one};

fn create(panel: &[ReviewerChoice], max_rounds: RoundNo) -> Result<Engine, String> {
    Engine::create_in(
        SessionId::new("rev-x"),
        Setup {
            request: "",
            target: testkit::target(),
            manifest: testkit::manifest(),
            workdir: None,
            max_rounds,
            lead: AgentKind::Claude.into(),
            panel,
        },
        Path::new("/tmp/rev-x"),
        Config::default(),
        1,
    )
    .map(|(engine, _)| engine)
    .map_err(|e| e.to_string())
}

#[test]
fn ac6_the_panel_size_setups_custom_lenses_and_an_empty_target_are_checked() {
    let one = vec![testkit::reviewer(AgentKind::Claude, BuiltinLens::Tests)];
    assert!(create(&one, 3).unwrap_err().contains("2体以上"));
    let seven: Vec<ReviewerChoice> = (0..7)
        .map(|_| testkit::reviewer(AgentKind::Codex, BuiltinLens::Tests))
        .collect();
    assert!(create(&seven, 3).unwrap_err().contains("6体まで"));
    let mut bad_model = testkit::panel();
    bad_model[0].agent.setup = AgentSetup {
        model: Some("a b".into()),
        effort: None,
    };
    assert!(create(&bad_model, 3).unwrap_err().contains("モデル名"));
    let mut bad_effort = testkit::panel();
    bad_effort[1].agent.setup = AgentSetup {
        model: None,
        effort: Some("ultra".into()),
    };
    assert!(create(&bad_effort, 3).is_ok(), "codex takes ultra");
    bad_effort[0].agent.setup.effort = Some("ultra".into());
    assert!(create(&bad_effort, 3).unwrap_err().contains("エフォート"));
    let mut empty_lens = testkit::panel();
    empty_lens[2].lens = LensChoice::Custom("   ".into());
    assert!(create(&empty_lens, 3).unwrap_err().contains("空です"));
    let mut long_lens = testkit::panel();
    long_lens[2].lens = LensChoice::Custom("あ".repeat(501));
    assert!(create(&long_lens, 3).unwrap_err().contains("500文字まで"));
    long_lens[2].lens = LensChoice::Custom("あ".repeat(500));
    assert!(create(&long_lens, 3).is_ok());
    assert!(create(&testkit::panel(), 0).is_err());
    assert!(create(&testkit::panel(), 11).is_err());
    let err = Engine::create_in(
        SessionId::new("rev-y"),
        Setup {
            request: "",
            target: testkit::target(),
            manifest: TargetManifest::default(),
            workdir: None,
            max_rounds: 3,
            lead: AgentKind::Claude.into(),
            panel: &testkit::panel(),
        },
        Path::new("/tmp/rev-y"),
        Config::default(),
        1,
    )
    .unwrap_err();
    assert!(err.to_string().contains("空"), "{err}");
}

#[test]
fn ac7_participant_and_custom_lens_ids_follow_the_panel() {
    let panel = vec![
        testkit::reviewer(AgentKind::Claude, BuiltinLens::Correctness),
        ReviewerChoice {
            agent: AgentKind::Codex.into(),
            lens: LensChoice::Custom("i18n".into()),
        },
        ReviewerChoice {
            agent: AgentKind::Claude.into(),
            lens: LensChoice::Custom("API compatibility".into()),
        },
    ];
    let (lead, reviewers) = participants(&AgentKind::Agy.into(), &panel);
    assert_eq!(lead.id.as_str(), "lead");
    assert_eq!(lead.kind, AgentKind::Agy);
    let ids: Vec<&str> = reviewers.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "claude-2"]);
    let lenses: Vec<&str> = reviewers
        .iter()
        .map(|r| r.lens.as_ref().unwrap().id())
        .collect();
    assert_eq!(lenses, ["correctness", "custom", "custom-2"]);
}

#[test]
fn a_round_briefs_every_reviewer_and_waits_for_all_replies_before_the_lead() {
    let mut rig = Rig::new(3);
    let round = rig.engine.session().current_round().unwrap();
    assert_eq!(round.briefs.len(), 3);
    assert_eq!(round.phase, Phase::Briefing);
    rig.deliver();
    assert_eq!(
        rig.engine.session().current_round().unwrap().phase,
        Phase::Reviewing
    );
    rig.reply(
        "claude",
        &blind(&[("1", "src/store.rs", 128, "high", "correctness")]),
    );
    rig.reply("codex", &blind(&[]));
    assert_eq!(
        rig.engine.session().current_round().unwrap().phase,
        Phase::Reviewing
    );
    let out = rig.reply("agy", &blind(&[]));
    let round = rig.engine.session().current_round().unwrap();
    assert_eq!(round.phase, Phase::Consolidating);
    let lead = Rig::sent_to(&out, "lead").expect("the lead is briefed");
    assert!(lead.contains("### claude/1"), "{lead}");
    assert_eq!(rig.engine.session().raw.len(), 1);
}

#[test]
fn ac11_a_rejected_reply_is_cleared_corrected_logged_and_fails_after_the_retries() {
    let mut rig = Rig::new(3);
    rig.deliver();
    let out = rig.reply("claude", "{ not json");
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::ClearReply { .. }))
    );
    let note = Rig::sent_to(&out, "claude").unwrap();
    assert!(
        note.starts_with("Your review JSON could not be used:"),
        "{note}"
    );
    assert!(note.contains("not valid review JSON"), "{note}");
    let receipt = &rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("claude"))
        .unwrap()
        .receipt;
    assert_eq!(receipt.rejected_digest, Some(store::digest("{ not json")));
    for _ in 0..2 {
        let at = rig.tick();
        rig.feed(Input::Sent {
            round: 1,
            part: p("claude"),
            at,
        });
        rig.reply("claude", r#"{"summary": ""}"#);
    }
    let receipt = &rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .brief(&p("claude"))
        .unwrap()
        .receipt;
    assert!(
        receipt
            .failed
            .as_deref()
            .unwrap()
            .contains("invalid output after retries")
    );
    // The failed reviewer does not hold the round.
    rig.reply("codex", &blind(&[]));
    let out = rig.reply("agy", &blind(&[]));
    let lead = Rig::sent_to(&out, "lead").unwrap();
    assert!(
        lead.contains("- claude (claude): no reply: invalid output after retries"),
        "{lead}"
    );
}

#[test]
fn a_brief_not_started_in_time_is_sent_again_then_failed() {
    let mut rig = Rig::new(3);
    rig.deliver();
    let base = rig.now;
    let out = rig.feed(Input::Tick { now: base + 90_000 });
    assert_eq!(
        out.effects
            .iter()
            .filter(|e| matches!(e, Effect::SendBrief { .. }))
            .count(),
        3
    );
    let receipt = &rig.engine.session().current_round().unwrap().briefs[0].receipt;
    assert_eq!(receipt.retries, 1);
    assert!(receipt.sent.is_none());
}

#[test]
fn ac12_every_raw_finding_is_placed_or_dismissed_after_consolidation() {
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    let s = rig.engine.session();
    for raw in &s.raw {
        assert!(s.placement(&raw.key).is_some(), "{} unplaced", raw.key);
    }
    let f1 = s.finding("F1").unwrap();
    assert_eq!(f1.sources.len(), 2);
    assert_eq!(f1.in_diff, Some(true));
    assert_eq!(s.current_round().unwrap().no, 2, "round 2 started");
    assert!(
        rig.brief("codex")
            .contains("Your question: Does F1 matter for security?")
    );
}

#[test]
fn ac13_merge_moves_sources_and_stances_and_marks_the_merged_finding() {
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("codex", &review(&[], &[("F2", "dispute", "Not layering.")]));
    rig.reply(
        "agy",
        &review(
            &[("2", "src/store.rs", 129, "high", "correctness")],
            &[("F1", "confirm", "")],
        ),
    );
    rig.lead(
        r#"{"summary": "Merged.", "ops": [
          {"op": "merge", "id": "F1", "from": ["agy/2", "F2"]}
        ]}"#,
    );
    let s = rig.engine.session();
    let f2 = s.finding("F2").unwrap();
    assert_eq!(f2.status, FindingStatus::Merged);
    assert_eq!(f2.merged_into.as_deref(), Some("F1"));
    assert!(f2.sources.is_empty() && f2.stances.is_empty());
    let f1 = s.finding("F1").unwrap();
    let sources: Vec<String> = f1.sources.iter().map(|k| k.to_string()).collect();
    assert_eq!(sources, ["claude/1", "codex/1", "agy/2", "agy/1"]);
    assert_eq!(
        f1.latest_stance(&p("codex")).unwrap().kind,
        StanceKind::Dispute
    );
    assert!(!f2.status.is_live());
}

#[test]
fn ac14_a_finding_every_origin_withdrew_is_withdrawn() {
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply(
        "agy",
        &review(&[], &[("F1", "unsure", ""), ("F2", "withdraw", "")]),
    );
    let s = rig.engine.session();
    assert_eq!(s.finding("F2").unwrap().status, FindingStatus::Withdrawn);
    rig.reply("claude", &review(&[], &[("F1", "withdraw", "")]));
    assert_eq!(
        rig.engine.session().finding("F1").unwrap().status,
        FindingStatus::Open,
        "codex still stands behind F1"
    );
}

#[test]
fn ac15_agreement_and_ranking_follow_the_stances() {
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "dispute", "No.")]));
    rig.reply("codex", &review(&[], &[("F2", "unsure", "")]));
    rig.reply("agy", &review(&[], &[("F1", "confirm", "")]));
    let s = rig.engine.session();
    let f1 = s.tally(s.finding("F1").unwrap());
    assert_eq!(f1.agreement, Agreement::Unanimous);
    assert_eq!(f1.label(), "支持 3");
    let f2 = s.tally(s.finding("F2").unwrap());
    assert_eq!(f2.agreement, Agreement::Split);
    assert_eq!(f2.label(), "支持 1 · 反論 1 · 未表明 1");
    let ranked: Vec<&str> = s.ranked().iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ranked, ["F1", "F2"]);

    // A lone supporter with everyone else silent is single; one dispute
    // against two supporters is a majority.
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "unsure", "")]));
    let s = rig.engine.session();
    assert_eq!(
        s.tally(s.finding("F2").unwrap()).agreement,
        Agreement::Single
    );
    rig.reply("agy", &review(&[], &[("F1", "dispute", "Disagree.")]));
    let s = rig.engine.session();
    assert_eq!(
        s.tally(s.finding("F1").unwrap()).agreement,
        Agreement::Majority
    );
}

fn finish_round(rig: &mut Rig, lead: &str) -> Output {
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("codex", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("agy", &review(&[], &[("F1", "confirm", "")]));
    rig.lead(lead)
}

const VERDICT: &str = r#"{"summary": "Done.", "ops": [
  {"op": "update", "id": "F1", "status": "agreed"},
  {"op": "update", "id": "F2", "status": "rejected", "rationale": "Fine as is."}
 ], "verdict": {"decision": "request_changes", "rationale": "F1 loses data.", "must_fix": ["F1"]}}"#;

#[test]
fn ac16_the_last_round_needs_a_verdict_and_then_the_review_closes() {
    let mut rig = Rig::new(2);
    through_round_one(&mut rig);
    finish_round(&mut rig, r#"{"summary": "No verdict."}"#);
    assert!(
        rig.rejected("lead")
            .unwrap()
            .contains("verdict: required, this is the last round")
    );
    assert!(!rig.engine.session().closed);
    let out = rig.reply("lead", VERDICT);
    assert!(out.effects.contains(&Effect::WriteReport));
    let s = rig.engine.session();
    assert!(s.closed);
    let verdict = s.verdict().unwrap();
    assert_eq!(verdict.decision, Decision::RequestChanges);
    assert_eq!(verdict.must_fix, ["F1"]);
    assert_eq!(s.finding("F2").unwrap().status, FindingStatus::Rejected);
}

#[test]
fn ac17_a_held_review_waits_after_consolidation_and_carries_interventions() {
    let mut rig = Rig::new(3);
    let err = rig.try_feed(Input::UserContinue { at: 1 }).unwrap_err();
    assert!(err.contains("停止していません"), "{err}");
    rig.feed(Input::UserSetHold { hold: true, at: 11 });
    rig.deliver();
    rig.reply(
        "claude",
        &blind(&[("1", "src/store.rs", 128, "high", "correctness")]),
    );
    rig.reply(
        "codex",
        &blind(&[("1", "src/store.rs", 130, "medium", "security")]),
    );
    rig.reply(
        "agy",
        &blind(&[("1", "src/engine.rs", 11, "low", "design")]),
    );
    rig.lead(LEAD_R1);
    let s = rig.engine.session();
    assert_eq!(s.rounds.len(), 1, "held: no round 2 yet");
    assert!(s.waits_to_continue());
    rig.feed(Input::UserIntervention {
        to: vec![p("codex")],
        body: "セッション固定化も見て".into(),
        at: 100,
    });
    let out = rig.feed(Input::UserContinue { at: 101 });
    assert_eq!(rig.engine.session().rounds.len(), 2);
    assert!(
        Rig::sent_to(&out, "codex")
            .unwrap()
            .contains("セッション固定化も見て")
    );
    assert!(
        !Rig::sent_to(&out, "agy")
            .unwrap()
            .contains("セッション固定化も見て")
    );

    // Letting go of the hold while a round waits starts it.
    let mut rig = Rig::new(3);
    rig.feed(Input::UserSetHold { hold: true, at: 11 });
    through_round_one(&mut rig);
    assert!(rig.engine.session().waits_to_continue());
    rig.feed(Input::UserSetHold {
        hold: false,
        at: 200,
    });
    assert_eq!(rig.engine.session().rounds.len(), 2);
}

#[test]
fn ac18_rounds_are_added_before_the_last_consolidation_and_after_a_full_run() {
    // Running, before the last round consolidates.
    let mut rig = Rig::new(2);
    through_round_one(&mut rig);
    rig.feed(Input::UserExtend { by: 1, at: 50 });
    assert_eq!(rig.engine.session().max_rounds, 3);

    // Refused while the last round consolidates.
    let mut rig = Rig::new(2);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("codex", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("agy", &review(&[], &[("F1", "confirm", "")]));
    assert_eq!(
        rig.engine.session().current_round().unwrap().phase,
        Phase::Consolidating
    );
    let err = rig
        .try_feed(Input::UserExtend { by: 1, at: 60 })
        .unwrap_err();
    assert!(err.contains("取りまとめ中"), "{err}");
    let at = rig.tick();
    rig.feed(Input::Sent {
        round: 2,
        part: p("lead"),
        at,
    });
    rig.reply("lead", VERDICT);
    assert!(rig.engine.session().closed);

    // After running all its rounds: open again, the old verdict stays with
    // its round, and none is the result until the new last round.
    let out = rig.feed(Input::UserExtend { by: 1, at: 70 });
    let s = rig.engine.session();
    assert!(!s.closed);
    assert_eq!((s.max_rounds, s.rounds.len()), (3, 3));
    assert!(s.verdict().is_none());
    assert!(
        s.round(2)
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .verdict
            .is_some()
    );
    assert!(
        Rig::sent_to(&out, "claude")
            .unwrap()
            .contains("Round: 3 of 3")
    );
    assert!(rig.try_feed(Input::UserExtend { by: 0, at: 71 }).is_err());

    // Refused after an early close by the lead, and after the user's end.
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    finish_round(
        &mut rig,
        &VERDICT.replace(r#""verdict""#, r#""close": true, "verdict""#),
    );
    assert!(rig.engine.session().closed);
    let err = rig
        .try_feed(Input::UserExtend { by: 1, at: 80 })
        .unwrap_err();
    assert!(err.contains("上限の前に終了"), "{err}");
    let mut rig = Rig::new(3);
    rig.feed(Input::UserClose);
    let err = rig
        .try_feed(Input::UserExtend { by: 1, at: 80 })
        .unwrap_err();
    assert!(err.contains("上限の前に終了"), "{err}");
    assert!(rig.engine.session().ended_by_user());
}

#[test]
fn a_user_retry_resets_failed_briefs_and_sends_them_again() {
    let mut rig = Rig::new(3);
    rig.deliver();
    for _ in 0..3 {
        let at = rig.tick();
        rig.feed(Input::AgentStateChanged {
            part: p("agy"),
            state: AgentState::Running,
            at,
        });
        rig.feed(Input::AgentStateChanged {
            part: p("agy"),
            state: AgentState::Error,
            at,
        });
        let at = rig.tick();
        rig.feed(Input::Sent {
            round: 1,
            part: p("agy"),
            at,
        });
    }
    let failed = |rig: &Rig| {
        rig.engine
            .session()
            .current_round()
            .unwrap()
            .brief(&p("agy"))
            .unwrap()
            .receipt
            .failed
            .clone()
    };
    assert!(failed(&rig).is_some());
    let out = rig.feed(Input::UserRetry);
    assert!(failed(&rig).is_none());
    assert!(
        Rig::sent_to(&out, "agy")
            .unwrap()
            .starts_with("## 1. Review request and round")
    );
    assert!(rig.try_feed(Input::UserRetry).is_err());
}

#[test]
fn a_closed_review_refuses_interventions_and_hold() {
    let mut rig = Rig::new(3);
    rig.feed(Input::UserClose);
    for input in [
        Input::UserIntervention {
            to: vec![],
            body: "x".into(),
            at: 1,
        },
        Input::UserSetHold { hold: true, at: 1 },
        Input::UserRetry,
    ] {
        assert!(rig.try_feed(input).unwrap_err().contains("終了"));
    }
}

#[test]
fn a_resumed_review_watches_and_resends_what_was_open() {
    let mut rig = Rig::new(3);
    rig.deliver();
    rig.reply("claude", &blind(&[]));
    let session = rig.engine.session().clone();
    let mut resumed = Engine::resume(session, Path::new("/data/reviews/rev-1"), Config::default());
    let out = resumed.handle(Input::Start { now: 5_000 }).unwrap();
    let watched: Vec<&str> = out
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::WatchReply { part, .. } => Some(part.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(watched, ["codex", "agy"]);
    assert!(
        out.effects
            .iter()
            .all(|e| !matches!(e, Effect::SendBrief { .. })),
        "already sent"
    );
}

#[test]
fn ac18_a_held_review_extended_after_a_full_run_has_no_verdict_until_its_new_last_round() {
    let mut rig = Rig::new(2);
    rig.feed(Input::UserSetHold { hold: true, at: 11 });
    through_round_one(&mut rig);
    rig.feed(Input::UserContinue { at: 100 });
    finish_round(&mut rig, VERDICT);
    assert!(rig.engine.session().verdict().is_some());

    // Held: the extension opens the review but starts no round.
    let out = rig.feed(Input::UserExtend { by: 1, at: 200 });
    assert!(
        out.effects.contains(&Effect::WriteReport),
        "the stored report keeps its old verdict"
    );
    let s = rig.engine.session();
    assert!(!s.closed && s.waits_to_continue());
    assert_eq!((s.max_rounds, s.rounds.len()), (3, 2));
    assert!(s.verdict().is_none(), "the old verdict is still the result");

    // Ended by the user before the new round: no verdict, the user's end.
    rig.feed(Input::UserClose);
    let s = rig.engine.session();
    assert!(s.closed && s.verdict().is_none() && s.ended_by_user());
}

/// Fails `part`'s brief of the current round (its retries used up).
fn fail(rig: &mut Rig, part: &str) {
    let round = rig.round();
    for _ in 0..10 {
        let brief = rig
            .engine
            .session()
            .current_round()
            .unwrap()
            .brief(&p(part))
            .unwrap();
        if brief.receipt.failed.is_some() {
            return;
        }
        rig.feed(Input::SendFailed {
            round,
            part: p(part),
            error: "gone".into(),
        });
    }
    panic!("{part} never failed");
}

#[test]
fn ac15_a_reviewer_failed_in_the_latest_round_is_not_counted_even_where_it_spoke_before() {
    // codex raised F1 (with claude), then fails round 2.
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("agy", &review(&[], &[("F1", "dispute", "Not torn.")]));
    fail(&mut rig, "codex");
    let s = rig.engine.session();
    let f1 = s.tally(s.finding("F1").unwrap());
    assert_eq!(
        (f1.supporters.clone(), f1.disputers.clone()),
        (vec![p("claude")], vec![p("agy")])
    );
    assert_eq!(
        f1.agreement,
        Agreement::Split,
        "codex, failed, still counted"
    );
    let f2 = s.tally(s.finding("F2").unwrap());
    assert!(!f2.silent.contains(&p("codex")) && !f2.supporters.contains(&p("codex")));
    assert_eq!(f2.agreement, Agreement::Unanimous);
}

#[test]
fn ac15_findings_alike_in_every_way_rank_by_id() {
    let mut rig = Rig::new(3);
    rig.deliver();
    rig.reply(
        "claude",
        &blind(&[("1", "src/store.rs", 128, "high", "correctness")]),
    );
    rig.reply(
        "codex",
        &blind(&[("1", "src/store.rs", 130, "high", "correctness")]),
    );
    rig.reply(
        "agy",
        &blind(&[("1", "src/engine.rs", 11, "high", "correctness")]),
    );
    let add = |id: &str, from: &str| {
        format!(
            r#"{{"op": "add", "id": "{id}", "from": ["{from}"], "claim": "Claim {id}", "location": null, "severity": "high", "category": "correctness", "confidence": "high", "status": "open", "rationale": "Seen."}}"#
        )
    };
    rig.lead(&format!(
        r#"{{"summary": "Three.", "ops": [{}, {}, {}]}}"#,
        add("F10", "claude/1"),
        add("F2", "codex/1"),
        add("F1", "agy/1")
    ));
    let s = rig.engine.session();
    assert_eq!(s.findings.len(), 3);
    let ranked: Vec<&str> = s.ranked().iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ranked, ["F1", "F2", "F10"]);
}

#[test]
fn ac12_after_every_lead_output_each_raw_finding_is_in_exactly_one_place() {
    let mut rig = Rig::new(3);
    let check = |rig: &Rig| {
        let s = rig.engine.session();
        let consolidated: Vec<RoundNo> = s
            .rounds
            .iter()
            .filter(|r| r.outcome.is_some())
            .map(|r| r.no)
            .collect();
        for raw in s.raw.iter().filter(|r| consolidated.contains(&r.round)) {
            let in_findings = s
                .findings
                .iter()
                .filter(|f| f.sources.contains(&raw.key))
                .count();
            let dismissed = s.dismissed.iter().filter(|d| d.raw == raw.key).count();
            assert_eq!(
                in_findings + dismissed,
                1,
                "{} is in {in_findings} findings and {dismissed} dismissals",
                raw.key
            );
        }
    };
    through_round_one(&mut rig);
    check(&rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("codex", &review(&[], &[("F2", "dispute", "Not layering.")]));
    rig.reply(
        "agy",
        &review(
            &[
                ("2", "src/store.rs", 129, "high", "correctness"),
                ("3", "src/store.rs", 1, "low", "design"),
            ],
            &[("F1", "confirm", "")],
        ),
    );
    rig.lead(
        r#"{"summary": "Merged.", "ops": [
          {"op": "merge", "id": "F1", "from": ["agy/2", "F2"]},
          {"op": "dismiss", "raw": "agy/3", "reason": "Style only."}
        ]}"#,
    );
    assert!(rig.engine.session().round(2).unwrap().outcome.is_some());
    check(&rig);
}
