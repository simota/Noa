use super::*;
use crate::model::types::{MAX_FILE_BYTES, MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, Phase};
use crate::testkit::{self, Options, Rig, VERIFIERS, all_pass, cmd, facts_for, p, reply_json};

// --------------------------------------------------------------- criteria

fn criteria_problems(text: &str) -> Vec<String> {
    let rig = Rig::new(2);
    parse_criteria(text, rig.engine.session())
        .map(|_| Vec::new())
        .unwrap_or_else(|e| e.problems)
}

fn criterion(id: &str, statement: &str, priority: &str, source: &str, how: &str) -> String {
    format!(
        r#"{{"id": "{id}", "statement": "{statement}", "how": "{how}", "priority": "{priority}", "source": "{source}"}}"#
    )
}

fn criteria_json(items: &[String]) -> String {
    format!(r#"{{"summary": "s", "criteria": [{}]}}"#, items.join(", "))
}

#[test]
fn ac5_the_judges_criteria_parse_and_keep_the_persons_line() {
    let rig = Rig::new(2);
    let out = parse_criteria(testkit::CRITERIA, rig.engine.session()).unwrap();
    assert_eq!(out.criteria.len(), 3);
    assert_eq!(out.criteria[0].statement, testkit::USER_LINE);
    assert_eq!(out.criteria[0].source, CriterionSource::User);
    assert_eq!(out.criteria[2].priority, Priority::Should);
    // One fence around the whole file is tolerated.
    let fenced = format!("```json\n{}\n```", testkit::CRITERIA);
    assert!(parse_criteria(&fenced, rig.engine.session()).is_ok());
    // Unknown fields are refused.
    let extra = testkit::CRITERIA.replace("\"summary\"", "\"note\": 1, \"summary\"");
    let err = parse_criteria(&extra, rig.engine.session()).unwrap_err();
    assert!(err.to_string().contains("unknown field `note`"), "{err}");
}

#[test]
fn ac5_each_criteria_rule_is_its_own_problem_line() {
    let user = criterion("AC-1", testkit::USER_LINE, "must", "user", "pwd");
    let ok = |id: &str| criterion(id, "Something holds", "must", "inferred", "run it");
    let cases: Vec<(String, &str)> = vec![
        (criteria_json(&[]), "criteria: none given; write 1 to 30"),
        (
            criteria_json(
                &std::iter::once(user.clone())
                    .chain((2..=31).map(|n| ok(&format!("AC-{n}"))))
                    .collect::<Vec<_>>(),
            ),
            "criteria: 31 given, at most 30; merge related ones",
        ),
        (
            criteria_json(&[ok("AC-1")]),
            "the person's criteria missing (each once, source user, their wording kept): \"Tabs inherit the focused pane's cwd\"",
        ),
        (
            criteria_json(&[user.clone(), ok("AC-3")]),
            "ids: AC-2 missing; number them AC-1 to AC-2 without gaps",
        ),
        (
            criteria_json(&[user.clone(), ok("AC-1")]),
            "criteria[1].id AC-1 is used twice",
        ),
        (
            criteria_json(&[user.clone(), ok("C2")]),
            "criteria[1].id \"C2\" must be AC-<n> (AC-1, AC-2, …)",
        ),
        (
            criteria_json(&[
                user.clone(),
                criterion("AC-2", "x", "may", "inferred", "run"),
            ]),
            "criteria[1].priority \"may\" must be must or should",
        ),
        (
            criteria_json(&[user.clone(), criterion("AC-2", "x", "must", "guess", "run")]),
            "criteria[1].source \"guess\" must be user, request, inferred or spec:<line>",
        ),
        (
            criteria_json(&[
                user.clone(),
                criterion("AC-2", "x", "must", "spec:3", "run"),
            ]),
            "criteria[1].source spec:3: there is no spec file; use request or inferred",
        ),
        (
            criteria_json(&[
                user.clone(),
                criterion("AC-2", "x", "must", "inferred", " "),
            ]),
            "criteria[1].how is empty; say how to verify it",
        ),
        (
            criteria_json(&[
                user.clone(),
                criterion("AC-2", "Not the line", "must", "user", "run"),
            ]),
            "criteria[1]: source user, but the statement is not one of the person's lines word for word",
        ),
        (
            criteria_json(&[user.clone(), user.replace("AC-1", "AC-2")]),
            "the person's line \"Tabs inherit the focused pane's cwd\" appears 2 times; give it exactly once",
        ),
        (
            criteria_json(&[
                user.clone(),
                criterion("AC-2", &"x".repeat(301), "must", "inferred", "run"),
            ]),
            "criteria[1].statement has 301 characters; at most 300 (put details in how)",
        ),
    ];
    for (text, want) in cases {
        let problems = criteria_problems(&text);
        assert!(
            problems.iter().any(|p| p == want),
            "want {want:?} in {problems:#?}"
        );
    }
    // Several rules broken at once: one line each.
    let problems = criteria_problems(&criteria_json(&[
        criterion("AC-2", "x", "may", "guess", ""),
        ok("AC-2"),
    ]));
    assert!(problems.len() >= 5, "{problems:#?}");
}

// ------------------------------------------------------------------ reply

/// A rig in round 1, criteria accepted.
fn round_one() -> Rig {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig
}

fn reply_problems(rig: &Rig, part: &str, text: &str, files: &[FileFact]) -> Vec<String> {
    let round = rig.round();
    parse_reply(text, rig.engine.session(), &p(part), round, files)
        .map(|_| Vec::new())
        .unwrap_or_else(|e| e.problems)
}

fn fact(path: &str, state: FileState, sniffed: Option<Media>, by_ext: Option<Media>) -> FileFact {
    FileFact {
        path: path.into(),
        state,
        sniffed,
        by_ext,
    }
}

fn regular(bytes: u64) -> FileState {
    FileState::Regular { bytes }
}

#[test]
fn ac8_a_good_reply_maps_to_the_frozen_types() {
    let rig = round_one();
    let text = reply_json(
        &[
            ("AC-1", "fail", &["e1", "e2"]),
            ("AC-2", "blocked", &[]),
            ("AC-3", "not_tested", &[]),
        ],
        &[
            cmd("e1", 101, true),
            r#"{"id": "e2", "kind": "file", "path": "./video/tab.mov", "caption": "cd /tmp, new tab",
                "marks": [{"t": "00:04", "note": "opened"}, {"t": "00:06.5", "note": "shows $HOME"}], "shows": ["AC-1"]}"#
                .to_owned(),
        ],
        "",
    );
    let reply = parse_reply(
        &text,
        rig.engine.session(),
        &p("codex"),
        1,
        &facts_for(&text),
    )
    .unwrap();
    assert_eq!(reply.results[0].outcome, Outcome::Fail);
    assert_eq!(reply.results[0].evidence[0].as_str(), "codex/r1/e1");
    let EvidenceBody::File {
        path, media, marks, ..
    } = &reply.evidence[1].body
    else {
        panic!("{:?}", reply.evidence[1]);
    };
    assert_eq!(path, "video/tab.mov", "a leading ./ is dropped");
    assert_eq!(*media, Some(Media::QuickTime));
    assert_eq!(
        marks.iter().map(|m| m.t_ms).collect::<Vec<_>>(),
        [4_000, 6_500]
    );
    assert_eq!(cited_paths(&text), ["logs/e1.log", "video/tab.mov"]);
}

#[test]
fn ac8_each_reply_rule_is_its_own_problem_line() {
    let mut rig = round_one();
    let passes = |ids: &[&'static str]| -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
        vec![
            ("AC-1", "pass", ids.to_vec()),
            ("AC-2", "pass", ids.to_vec()),
            ("AC-3", "pass", ids.to_vec()),
        ]
    };
    let json = |results: &[(&str, &str, Vec<&str>)], evidence: &[String], extra: &str| {
        let results: Vec<(&str, &str, &[&str])> = results
            .iter()
            .map(|(c, r, ids)| (*c, *r, ids.as_slice()))
            .collect();
        reply_json(&results, evidence, extra)
    };
    let e1 = cmd("e1", 0, true);
    let cases: Vec<(String, Vec<FileFact>, &str)> = vec![
        (
            json(&passes(&["e1"])[..1], std::slice::from_ref(&e1), ""),
            vec![],
            "results: missing AC-2, AC-3 (one result per criterion in round 1)",
        ),
        (
            json(&passes(&[]), &[], ""),
            vec![],
            "results[0] (AC-1): pass needs at least one command or file evidence",
        ),
        (
            json(
                &passes(&["o1"]),
                &[r#"{"id": "o1", "kind": "observation", "text": "looked fine"}"#.to_owned()],
                "",
            ),
            vec![],
            "results[0] (AC-1): pass needs at least one command or file evidence",
        ),
        (
            json(&passes(&["e1"]), &[e1.clone(), cmd("e2", 0, false)], ""),
            vec![],
            "evidence \"e2\" is cited by no result, defect or cross-check; cite it or leave it out",
        ),
        (
            json(&passes(&["e1", "e9"]), std::slice::from_ref(&e1), ""),
            vec![],
            "results[0] (AC-1).evidence: \"e9\" is not an evidence id of this reply; ids: e1",
        ),
        (
            json(
                &passes(&["e1"]),
                &[e1.replace("logs/e1.log", "../e1.log")],
                "",
            ),
            vec![],
            "evidence[0] (e1).log \"../e1.log\" must be a plain relative path, without \"..\"",
        ),
        (
            json(
                &passes(&["e1"]),
                &[e1.replace("logs/e1.log", "/tmp/e1.log")],
                "",
            ),
            vec![],
            "evidence[0] (e1).log \"/tmp/e1.log\" must be relative to your reply dir, not absolute",
        ),
        (
            json(
                &passes(&["e1"]),
                &[e1.replace("logs/e1.log", "rerun/1.log")],
                "",
            ),
            vec![],
            "evidence[0] (e1).log \"rerun/1.log\" is under rerun/, which the widget keeps for its re-runs; save it elsewhere",
        ),
        (
            json(&passes(&["e1"]), std::slice::from_ref(&e1), ""),
            vec![fact("logs/e1.log", FileState::Link, None, None)],
            "evidence[0] (e1): logs/e1.log is a symbolic link (or behind one); save the file itself",
        ),
        (
            json(&passes(&["e1"]), std::slice::from_ref(&e1), ""),
            vec![fact("logs/e1.log", FileState::Missing, None, None)],
            "evidence[0] (e1): logs/e1.log does not exist in your reply dir",
        ),
        (
            json(&passes(&["e1"]), std::slice::from_ref(&e1), ""),
            vec![fact("logs/e1.log", regular(MAX_LOG_BYTES + 1), None, None)],
            "evidence[0] (e1): logs/e1.log is 8388609 bytes; a log may be at most 8 MiB",
        ),
        (
            json(
                &passes(&["f1"]),
                &[r#"{"id": "f1", "kind": "file", "path": "out/dump.bin"}"#.to_owned()],
                "",
            ),
            vec![fact(
                "out/dump.bin",
                regular(MAX_FILE_BYTES + 1),
                None,
                None,
            )],
            "evidence[0] (f1): out/dump.bin is 15728641 bytes; a file may be at most 15 MiB",
        ),
        (
            json(
                &passes(&["f1"]),
                &[r#"{"id": "f1", "kind": "file", "path": "rerun/run.sh"}"#.to_owned()],
                "",
            ),
            vec![],
            "evidence[0] (f1).path \"rerun/run.sh\" is under rerun/, which the widget keeps for its re-runs; save it elsewhere",
        ),
        (
            json(
                &passes(&["e1"]),
                std::slice::from_ref(&e1),
                r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "reproduced", "evidence": ["e1"]}]"#,
            ),
            vec![],
            "cross_checks: not allowed in round 1 (there is nothing to cross-check yet)",
        ),
        (
            json(
                &passes(&["e1"]),
                &[e1.replace("\"exit\": 0", "\"exit\": 4294967296")],
                "",
            ),
            vec![],
            "evidence[0] (e1).exit 4294967296 is not an exit status",
        ),
        (
            json(
                &[
                    ("AC-1", "fail", vec![]),
                    ("AC-2", "blocked", vec![]),
                    ("AC-3", "maybe", vec![]),
                ],
                &[],
                "",
            ),
            vec![],
            "results[0] (AC-1): fail needs evidence",
        ),
        (
            json(
                &[
                    ("AC-1", "pass", vec!["e1"]),
                    ("AC-2", "pass", vec!["e1"]),
                    ("AC-3", "maybe", vec!["e1"]),
                ],
                std::slice::from_ref(&e1),
                "",
            ),
            vec![],
            "results[2] (AC-3).result \"maybe\" must be pass, fail, blocked or not_tested",
        ),
        (
            json(
                &passes(&["e1"]),
                std::slice::from_ref(&e1),
                r#", "defects": [{"id": "1", "claim": "c", "severity": "high", "category": "falsify", "confidence": "high", "repro": "", "evidence": []}]"#,
            ),
            vec![],
            "defects[0].repro is empty; give the steps that show it",
        ),
        (
            json(
                &passes(&["e1"]),
                std::slice::from_ref(&e1),
                r#", "defects": [{"id": "1", "claim": "c", "severity": "high", "category": "falsify", "confidence": "high", "repro": "1. run", "evidence": []}]"#,
            ),
            vec![],
            "defects[0].evidence is empty; cite at least one evidence id",
        ),
    ];
    for (text, files, want) in cases {
        let files = if files.is_empty() {
            facts_for(&text)
        } else {
            files
        };
        let problems = reply_problems(&rig, "claude", &text, &files);
        assert!(
            problems.iter().any(|p| p == want),
            "want {want:?} in {problems:#?}\nfor {text}"
        );
    }
    // A missing answer to an assignment, in round 2.
    rig.deliver();
    let replies = [
        (
            "claude",
            reply_json(
                &[
                    ("AC-1", "fail", &["e1"]),
                    ("AC-2", "pass", &["e1"]),
                    ("AC-3", "pass", &["e1"]),
                ],
                &[cmd("e1", 1, true)],
                "",
            ),
        ),
        ("codex", all_pass()),
        ("claude-2", all_pass()),
    ];
    for (part, text) in &replies {
        rig.reply(part, text);
    }
    rig.ingest();
    rig.rerun_all_matched();
    rig.judge(r#"{"summary": "s", "rulings": [
      {"criterion": "AC-1", "ruling": "failed", "rationale": "r", "evidence": ["claude/r1/e1"]},
      {"criterion": "AC-2", "ruling": "verified", "rationale": "r", "evidence": ["codex/r1/e1"]},
      {"criterion": "AC-3", "ruling": "verified", "rationale": "r", "evidence": ["codex/r1/e1"]}]}"#);
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    let problems = reply_problems(&rig, "codex", r#"{"summary": "Nothing."}"#, &[]);
    assert_eq!(
        problems,
        ["cross_checks: missing claude/AC-1 (one per assignment)"]
    );
    let problems = reply_problems(
        &rig,
        "codex",
        r#"{"summary": "s", "cross_checks": [
          {"subject": "claude/AC-1", "outcome": "not_reproduced"},
          {"subject": "claude-2/AC-3", "outcome": "disputed", "note": "n"}]}"#,
        &[],
    );
    assert!(problems.contains(&"cross_checks[0] (claude/AC-1): give evidence (only disputed with a note may go without)".to_owned()), "{problems:#?}");
    assert!(
        problems.contains(
            &"cross_checks[1].subject claude-2/AC-3 is not one of your assignments: claude/AC-1"
                .to_owned()
        ),
        "{problems:#?}"
    );
}

#[test]
fn ac8_unknown_fields_and_more_than_eight_reruns_are_refused() {
    let rig = round_one();
    let extra = all_pass().replace("\"summary\"", "\"verifier\": \"claude\", \"summary\"");
    let err = parse_reply(
        &extra,
        rig.engine.session(),
        &p("claude"),
        1,
        &facts_for(&extra),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown field `verifier`"),
        "{err}"
    );
    let ids: Vec<String> = (1..=9).map(|n| format!("e{n}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let text = reply_json(
        &[
            ("AC-1", "pass", &refs),
            ("AC-2", "pass", &refs),
            ("AC-3", "pass", &refs),
        ],
        &ids.iter().map(|id| cmd(id, 0, true)).collect::<Vec<_>>(),
        "",
    );
    let problems = reply_problems(&rig, "claude", &text, &facts_for(&text));
    assert_eq!(
        problems,
        ["evidence: 9 commands marked rerun: true, at most 8; keep the ones that matter most"]
    );
}

// ------------------------------------------------------------------ media

#[test]
fn ac30_each_media_rule_is_its_own_problem_line() {
    let rig = round_one();
    let file = |item: &str| {
        reply_json(
            &[
                ("AC-1", "pass", &["f1"]),
                ("AC-2", "not_tested", &[]),
                ("AC-3", "not_tested", &[]),
            ],
            &[item.to_owned()],
            "",
        )
    };
    let cases: Vec<(String, FileFact, &str)> = vec![
        (
            file(r#"{"id": "f1", "kind": "file", "path": "shots/tab.mp4", "caption": "c"}"#),
            fact(
                "shots/tab.mp4",
                regular(10),
                Some(Media::Png),
                Some(Media::Mp4),
            ),
            "evidence[0] (f1): shots/tab.mp4 is a PNG image but its extension says MP4; name it .png",
        ),
        (
            file(r#"{"id": "f1", "kind": "file", "path": "shots/tab", "caption": "c"}"#),
            fact("shots/tab", regular(10), Some(Media::Jpeg), None),
            "evidence[0] (f1): shots/tab is a JPEG image but its extension names no image or video; name it .jpg",
        ),
        (
            file(r#"{"id": "f1", "kind": "file", "path": "shots/tab.png", "caption": "c"}"#),
            fact("shots/tab.png", regular(10), None, Some(Media::Png)),
            "evidence[0] (f1): shots/tab.png is named like a PNG image but its content is not one",
        ),
        (
            file(r#"{"id": "f1", "kind": "file", "path": "video/a.mp4", "caption": "c"}"#),
            fact(
                "video/a.mp4",
                regular(MAX_VIDEO_BYTES + 1),
                Some(Media::Mp4),
                Some(Media::Mp4),
            ),
            "evidence[0] (f1): video/a.mp4 is 209715201 bytes; a video may be at most 200 MiB",
        ),
        (
            file(r#"{"id": "f1", "kind": "file", "path": "shots/a.png", "caption": "c"}"#),
            fact(
                "shots/a.png",
                regular(MAX_IMAGE_BYTES + 1),
                Some(Media::Png),
                Some(Media::Png),
            ),
            "evidence[0] (f1): shots/a.png is 15728641 bytes; an image may be at most 15 MiB",
        ),
        (
            file(r#"{"id": "f1", "kind": "file", "path": "shots/a.png"}"#),
            fact(
                "shots/a.png",
                regular(10),
                Some(Media::Png),
                Some(Media::Png),
            ),
            "evidence[0] (f1): shots/a.png is media and needs a caption saying what it shows",
        ),
        (
            file(&format!(
                r#"{{"id": "f1", "kind": "file", "path": "shots/a.png", "caption": "{}"}}"#,
                "c".repeat(301)
            )),
            fact(
                "shots/a.png",
                regular(10),
                Some(Media::Png),
                Some(Media::Png),
            ),
            "evidence[0] (f1).caption has 301 characters; at most 300",
        ),
        (
            file(
                r#"{"id": "f1", "kind": "file", "path": "video/a.webm", "caption": "c",
                     "marks": [{"t": "00:06", "note": "b"}, {"t": "00:04", "note": "a"}]}"#,
            ),
            fact(
                "video/a.webm",
                regular(10),
                Some(Media::Webm),
                Some(Media::Webm),
            ),
            "evidence[0] (f1).marks[1].t 00:04 is before the mark above it; keep marks in time order",
        ),
        (
            file(
                r#"{"id": "f1", "kind": "file", "path": "video/a.mov", "caption": "c",
                     "marks": [{"t": "0:4", "note": "a"}]}"#,
            ),
            fact(
                "video/a.mov",
                regular(10),
                Some(Media::QuickTime),
                Some(Media::QuickTime),
            ),
            "evidence[0] (f1).marks[0].t \"0:4\": seconds must be two digits",
        ),
        (
            file(
                r#"{"id": "f1", "kind": "file", "path": "shots/a.gif", "caption": "c",
                     "marks": [{"t": "00:01", "note": "a"}]}"#,
            ),
            fact(
                "shots/a.gif",
                regular(10),
                Some(Media::Gif),
                Some(Media::Gif),
            ),
            "evidence[0] (f1).marks: only a video takes marks",
        ),
        (
            file(
                r#"{"id": "f1", "kind": "file", "path": "shots/a.webp", "caption": "c", "shows": ["AC-9"]}"#,
            ),
            fact(
                "shots/a.webp",
                regular(10),
                Some(Media::Webp),
                Some(Media::Webp),
            ),
            "evidence[0] (f1).shows: \"AC-9\" is not a criterion; criteria: AC-1, AC-2, AC-3",
        ),
    ];
    for (text, fact, want) in cases {
        let problems = reply_problems(&rig, "claude", &text, &[fact]);
        assert_eq!(problems, [want], "for {text}");
    }
    // Every media kind whose bytes and name agree goes through, with its
    // marks in milliseconds.
    for media in Media::ALL {
        let path = format!("m/a.{}", media.extension());
        let marks = if media.is_video() {
            r#", "marks": [{"t": "01:02.25", "note": "n"}]"#
        } else {
            ""
        };
        let text = file(&format!(
            r#"{{"id": "f1", "kind": "file", "path": "{path}", "caption": "c"{marks}}}"#
        ));
        let reply = parse_reply(
            &text,
            rig.engine.session(),
            &p("claude"),
            1,
            &[fact(&path, regular(10), Some(media), Some(media))],
        )
        .unwrap();
        let EvidenceBody::File {
            media: got, marks, ..
        } = &reply.evidence[0].body
        else {
            unreachable!()
        };
        assert_eq!(*got, Some(media));
        if media.is_video() {
            assert_eq!(marks[0].t_ms, 62_250);
        }
    }
}

// ------------------------------------------------------------------ judge

/// Round 1 of `max_rounds` with:
/// - claude: AC-1 pass on e1 (re-run, matched), AC-2 pass on e2 (no
///   re-run) and o1 (an observation), AC-3 pass on e2;
/// - codex: AC-1 fail on e1 (re-run, matched), AC-2 not tested, AC-3 pass
///   on e2 (no re-run);
/// - claude-2: nothing passed but AC-3 on e1 (no re-run), and a high
///   defect `1`.
fn ruled_round(max_rounds: RoundNo) -> Rig {
    let mut rig = Rig::build(Options {
        max_rounds,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    rig.reply(
        "claude",
        &reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e2", "o1"]),
                ("AC-3", "pass", &["e2"]),
            ],
            &[
                cmd("e1", 0, true),
                cmd("e2", 0, false),
                r#"{"id": "o1", "kind": "observation", "text": "The tab looked right."}"#
                    .to_owned(),
            ],
            "",
        ),
    );
    rig.reply(
        "codex",
        &reply_json(
            &[
                ("AC-1", "fail", &["e1"]),
                ("AC-2", "not_tested", &[]),
                ("AC-3", "pass", &["e2"]),
            ],
            &[cmd("e1", 101, true), cmd("e2", 0, false)],
            "",
        ),
    );
    rig.reply(
        "claude-2",
        &reply_json(
            &[("AC-1", "not_tested", &[]), ("AC-2", "not_tested", &[]), ("AC-3", "pass", &["e1"])],
            &[cmd("e1", 0, false)],
            r#", "defects": [{"id": "1", "claim": "The store loses the last line", "severity": "high", "category": "regression",
               "confidence": "high", "location": {"path": "src/store.rs", "line": 11}, "repro": "1. kill -9 2. reopen", "evidence": ["e1"]}]"#,
        ),
    );
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling);
    rig
}

fn ruling(criterion: &str, kind: &str, rationale: &str, evidence: &[&str]) -> String {
    let evidence: Vec<String> = evidence.iter().map(|k| format!("\"{k}\"")).collect();
    format!(
        r#"{{"criterion": "{criterion}", "ruling": "{kind}", "rationale": "{rationale}", "evidence": [{}]}}"#,
        evidence.join(", ")
    )
}

/// A judge output of `rulings` with claude-2's defect added as D1 (`status`).
fn judge_json(rulings: &[String], status: &str, tail: &str) -> String {
    format!(
        r#"{{"summary": "s", "rulings": [{}], "defect_ops": [{{"op": "add", "id": "D1", "from": ["claude-2/1"],
           "claim": "The store loses the last line", "repro": "1. kill -9 2. reopen", "severity": "high", "category": "regression",
           "confidence": "high", "status": "{status}", "rationale": "Seen once."}}]{tail}}}"#,
        rulings.join(", ")
    )
}

/// The rulings that meet the standard in [`ruled_round`].
fn good_rulings() -> Vec<String> {
    vec![
        ruling("AC-1", "failed", "codex's test fails", &["codex/r1/e1"]),
        ruling("AC-2", "inconclusive", "one pass, one observation", &[]),
        ruling(
            "AC-3",
            "verified",
            "two passes",
            &["claude/r1/e2", "codex/r1/e2"],
        ),
    ]
}

fn judge_problems(rig: &Rig, text: &str) -> Vec<String> {
    parse_judge(text, rig.engine.session(), rig.round())
        .map(|_| Vec::new())
        .unwrap_or_else(|e| e.problems)
}

#[test]
fn ac13_each_evidence_standard_rule_is_its_own_problem_line() {
    let rig = ruled_round(2);
    assert_eq!(
        judge_problems(&rig, &judge_json(&good_rulings(), "open", "")),
        Vec::<String>::new()
    );
    let with = |i: usize, replacement: String| {
        let mut rulings = good_rulings();
        rulings[i] = replacement;
        judge_json(&rulings, "open", "")
    };
    let cases: Vec<(String, &str)> = vec![
        (
            with(2, ruling("AC-3", "verified", "one pass", &["claude/r1/e2"])),
            "AC-3: verified needs a matched re-run or two independent passes",
        ),
        (
            with(0, ruling("AC-1", "verified", "matched", &["claude/r1/e1"])),
            "AC-1: verified despite codex's fail, which no not_reproduced cross-check has answered",
        ),
        (
            with(1, ruling("AC-2", "verified", "seen", &["claude/r1/o1"])),
            "AC-2: an observation alone never makes a criterion verified; cite a command or a file",
        ),
        (
            with(
                0,
                ruling("AC-1", "failed", "claude says so", &["claude/r1/e1"]),
            ),
            "AC-1: failed needs a cited fail result or reproduced evidence",
        ),
        (
            with(1, ruling("AC-2", "inconclusive", " ", &[])),
            "AC-2: inconclusive needs a rationale",
        ),
        (
            with(1, ruling("AC-2", "not_applicable", "", &[])),
            "AC-2: not_applicable needs a rationale",
        ),
        (
            with(
                2,
                ruling("AC-3", "verified", "two", &["claude/r1/e2", "nobody/r1/e9"]),
            ),
            "AC-3: cited evidence \"nobody/r1/e9\" does not exist (cite keys as <verifier>/r<N>/<id> or user/<n>)",
        ),
        (
            with(2, ruling("AC-3", "same", "", &[])),
            "rulings[2] (AC-3): \"same\" needs an earlier ruling, and AC-3 has none",
        ),
        (
            judge_json(&good_rulings()[..2], "open", ""),
            "rulings: missing AC-3 (one per criterion each round; \"same\" carries one over)",
        ),
        (
            format!(
                r#"{{"summary": "s", "rulings": [{}]}}"#,
                good_rulings().join(", ")
            ),
            "unplaced raw defects: claude-2/1 (add, merge or dismiss each one)",
        ),
        (
            judge_json(
                &good_rulings(),
                "open",
                r#", "assign": {"codex": ["codex/AC-1"]}"#,
            ),
            "assign.codex[0]: codex/AC-1 is codex's own result; a verifier is never assigned its own result",
        ),
    ];
    for (text, want) in cases {
        let problems = judge_problems(&rig, &text);
        assert!(
            problems.iter().any(|p| p == want),
            "want {want:?} in {problems:#?}"
        );
    }
}

#[test]
fn ac13_a_same_ruling_carries_the_previous_one_over_and_is_held_to_the_standard_again() {
    let mut rig = ruled_round(3);
    rig.judge(&judge_json(&good_rulings(), "open", ""));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    // codex refutes nothing and reproduces nothing; claude now fails AC-3.
    let assigned = rig
        .engine
        .session()
        .current_round()
        .unwrap()
        .assignments
        .clone();
    for part in VERIFIERS {
        let checks: Vec<String> = assigned
            .get(&p(part))
            .into_iter()
            .flatten()
            .map(|s| {
                format!(r#"{{"subject": "{s}", "outcome": "disputed", "note": "not looked at"}}"#)
            })
            .collect();
        let results = if part == "claude" {
            r#""results": [{"criterion": "AC-3", "result": "fail", "note": "slow now", "evidence": ["e1"]}], "evidence": [{"id": "e1", "kind": "command", "cmd": "hyperfine ./app", "cwd": ".", "exit": 1, "log": "logs/1.log", "rerun": false}], "#
        } else {
            ""
        };
        let text = format!(
            r#"{{"summary": "s", {results}"cross_checks": [{}]}}"#,
            checks.join(", ")
        );
        rig.reply(part, &text);
    }
    rig.ingest();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude"));
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    let text = format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        same("AC-1"),
        same("AC-2"),
        same("AC-3")
    );
    let problems = judge_problems(&rig, &text);
    assert_eq!(
        problems,
        ["AC-3: verified despite claude's fail, which no not_reproduced cross-check has answered"]
    );
    let text = text.replace(
        &same("AC-3"),
        &ruling("AC-3", "failed", "slow now", &["claude/r2/e1"]),
    );
    let out = parse_judge(&text, rig.engine.session(), 2).unwrap();
    assert_eq!(out.rulings["AC-1"].round, 2);
    assert_eq!(out.rulings["AC-1"].kind, RulingKind::Failed);
    assert_eq!(out.rulings["AC-1"].evidence[0].as_str(), "codex/r1/e1");
}

#[test]
fn ac14_the_verdict_must_follow_from_the_rulings_and_the_defects() {
    let rig = ruled_round(1);
    let verdict = |decision: &str, blocking: &str| {
        format!(
            r#", "verdict": {{"decision": "{decision}", "rationale": "r", "blocking": [{blocking}]}}"#
        )
    };
    // AC-1 (must) failed, AC-2 (must) inconclusive, D1 high and live.
    let cases: Vec<(String, &str)> = vec![
        (
            judge_json(&good_rulings(), "open", &verdict("pass", "")),
            "verdict: pass needs every must criterion verified; AC-1, AC-2 are not",
        ),
        (
            judge_json(
                &good_rulings(),
                "open",
                &verdict("inconclusive", "\"AC-2\""),
            ),
            "verdict: AC-1 (must) failed; the decision is fail",
        ),
        (
            judge_json(&good_rulings(), "open", &verdict("fail", "")),
            "verdict: fail needs blocking to list what fails: AC-1",
        ),
        (
            judge_json(&good_rulings(), "open", &verdict("fail", "\"AC-9\"")),
            "verdict.blocking: \"AC-9\" is neither a criterion nor a live defect",
        ),
        (
            judge_json(
                &[
                    ruling("AC-1", "inconclusive", "r", &[]),
                    ruling("AC-2", "inconclusive", "r", &[]),
                    good_rulings()[2].clone(),
                ],
                "open",
                &verdict("fail", "\"AC-1\""),
            ),
            "verdict: fail needs a must criterion ruled failed",
        ),
        (
            judge_json(&good_rulings(), "open", ""),
            "verdict: required, this is the last round",
        ),
    ];
    for (text, want) in cases {
        let problems = judge_problems(&rig, &text);
        assert!(
            problems.iter().any(|p| p == want),
            "want {want:?} in {problems:#?}"
        );
    }
    assert_eq!(
        judge_problems(
            &rig,
            &judge_json(
                &good_rulings(),
                "open",
                &verdict("fail", "\"AC-1\", \"D1\"")
            )
        ),
        Vec::<String>::new()
    );

    // Every must verified: a live high defect stops pass, not
    // pass_with_issues; a rejected one stops nothing.
    let mut rig = Rig::build(Options {
        max_rounds: 1,
        ..Options::default()
    });
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    let defect = r#", "defects": [{"id": "1", "claim": "The store loses the last line", "severity": "high", "category": "regression",
       "confidence": "high", "repro": "1. kill -9 2. reopen", "evidence": ["e1"]}]"#;
    rig.reply("claude", &all_pass());
    rig.reply("codex", &all_pass());
    rig.reply(
        "claude-2",
        &format!("{}{defect}}}", all_pass().trim_end_matches('}')),
    );
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    let verified = vec![
        ruling("AC-1", "verified", "matched", &["claude/r1/e1"]),
        ruling("AC-2", "verified", "matched", &["codex/r1/e1"]),
        ruling("AC-3", "verified", "matched", &["claude-2/r1/e1"]),
    ];
    let problems = judge_problems(&rig, &judge_json(&verified, "agreed", &verdict("pass", "")));
    assert_eq!(
        problems,
        ["verdict: pass with live critical or high defect D1; use pass_with_issues or fail"]
    );
    assert_eq!(
        judge_problems(
            &rig,
            &judge_json(&verified, "agreed", &verdict("pass_with_issues", "\"D1\""))
        ),
        Vec::<String>::new()
    );
    let rejected = judge_json(&verified, "rejected", &verdict("pass", ""));
    assert_eq!(judge_problems(&rig, &rejected), Vec::<String>::new());
    assert_eq!(
        judge_problems(
            &rig,
            &judge_json(
                &verified,
                "rejected",
                &verdict("pass_with_issues", "\"AC-1\"")
            )
        ),
        [
            "verdict: nothing holds the change back (every should criterion verified, no live defect); use pass"
        ]
    );
}
