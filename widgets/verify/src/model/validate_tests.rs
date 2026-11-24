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
    ruled_round_with(max_rounds, &[])
}

/// [`ruled_round`] with the items keyed in `gone` gone before ingest.
fn ruled_round_with(max_rounds: RoundNo, gone: &[&str]) -> Rig {
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
    rig.ingest_except(gone, &Default::default());
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
            "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered",
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
        [
            "AC-3: verified despite claude's R2 fail, which no not_reproduced cross-check has answered"
        ]
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

#[test]
fn ac5_a_line_the_person_gave_twice_is_two_criteria() {
    let line = testkit::USER_LINE;
    let rig = Rig::build(Options {
        user_criteria: vec![line.to_owned(), line.to_owned()],
        ..Options::default()
    });
    let session = rig.engine.session();
    let user = |id: &str| criterion(id, line, "must", "user", "pwd");
    let problems = |text: &str| {
        parse_criteria(text, session)
            .map(|_| Vec::new())
            .unwrap_or_else(|e| e.problems)
    };
    // Each line kept, under its own id: accepted.
    assert_eq!(
        problems(&criteria_json(&[user("AC-1"), user("AC-2")])),
        Vec::<String>::new()
    );
    // One of the two dropped: refused.
    assert_eq!(
        problems(&criteria_json(&[user("AC-1")])),
        [format!(
            "the person's criteria missing (each once, source user, their wording kept): {line:?} (given 2 times, 1 here)"
        )]
    );
    // One more than given: refused.
    assert_eq!(
        problems(&criteria_json(&[user("AC-1"), user("AC-2"), user("AC-3")])),
        [format!(
            "the person's line {line:?} appears 3 times; give it exactly 2 times, as the person gave it"
        )]
    );
}

#[test]
fn ac13_cited_evidence_must_be_evidence_for_the_criterion_it_rules_on() {
    let rig = ruled_round(2);
    let with = |i: usize, replacement: String| {
        let mut rulings = good_rulings();
        rulings[i] = replacement;
        judge_json(&rulings, "open", "")
    };
    // codex/r1/e1 is codex's AC-1 failure, re-run matched: no evidence for
    // AC-2 or AC-3, whichever way it is cited.
    for (i, id) in [(1, "AC-2"), (2, "AC-3")] {
        let problems = judge_problems(
            &rig,
            &with(i, ruling(id, "verified", "matched", &["codex/r1/e1"])),
        );
        assert_eq!(
            problems,
            [format!(
                "{id}: cited evidence codex/r1/e1 is not evidence for {id} (cite what a result or a cross-check on {id} cites, or media whose shows names it)"
            )]
        );
    }
    let problems = judge_problems(
        &rig,
        &with(2, ruling("AC-3", "failed", "fails", &["codex/r1/e1"])),
    );
    assert!(
        problems[0].starts_with("AC-3: cited evidence codex/r1/e1 is not evidence for AC-3"),
        "{problems:?}"
    );
}

#[test]
fn ac13_evidence_gone_before_ingest_counts_for_nothing() {
    let rig = ruled_round_with(2, &["codex/r1/e2"]);
    // claude's and codex's passes on AC-3 would be two, but codex's log was
    // never stored.
    let problems = judge_problems(&rig, &judge_json(&good_rulings(), "open", ""));
    assert_eq!(
        problems,
        [
            "AC-3: verified needs a matched re-run or two independent passes; codex/r1/e2 was not stored (gone before ingest) and counts for nothing"
        ]
    );
    // Nor does a matched re-run whose log is gone.
    let rig = all_passed_with_a_defect_except(&["claude/r1/e1"]);
    let pass = verdict_json("pass", "");
    let rulings = |ac1: &str| {
        vec![
            ruling("AC-1", "verified", "matched", &[ac1]),
            ruling("AC-2", "verified", "matched", &["codex/r1/e1"]),
            ruling("AC-3", "verified", "matched", &["claude-2/r1/e1"]),
        ]
    };
    assert_eq!(
        judge_problems(
            &rig,
            &judge_json(&rulings("claude/r1/e1"), "rejected", &pass)
        ),
        [
            "AC-1: verified needs a matched re-run or two independent passes; claude/r1/e1 was not stored (gone before ingest) and counts for nothing"
        ]
    );
    assert_eq!(
        judge_problems(
            &rig,
            &judge_json(&rulings("codex/r1/e1"), "rejected", &pass)
        ),
        Vec::<String>::new()
    );
}

#[test]
fn ac13_a_reproduced_pass_backs_no_failure_and_a_reproduced_fail_does() {
    let mut rig = ruled_round(3);
    rig.judge(&judge_json(
        &good_rulings(),
        "open",
        r#", "assign": {"codex": ["claude/AC-2"], "claude-2": ["codex/AC-1"]}"#,
    ));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
    rig.reply(
        "codex",
        &reply_json(
            &[],
            &[cmd("e1", 0, false)],
            r#", "cross_checks": [{"subject": "claude/AC-2", "outcome": "reproduced", "note": "passes here too", "evidence": ["e1"]}]"#,
        ),
    );
    rig.reply(
        "claude-2",
        &reply_json(
            &[],
            &[cmd("e1", 101, false)],
            r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "reproduced", "note": "fails here too", "evidence": ["e1"]}]"#,
        ),
    );
    rig.ingest();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    let text = |ac1: String, ac2: String, ac3: String| {
        format!(r#"{{"summary": "s", "rulings": [{ac1}, {ac2}, {ac3}]}}"#)
    };
    // codex reproduced claude's AC-2 pass: no ground for AC-2 failing, and
    // no evidence at all for AC-3.
    let problems = judge_problems(
        &rig,
        &text(
            same("AC-1"),
            ruling("AC-2", "failed", "codex says", &["codex/r2/e1"]),
            same("AC-3"),
        ),
    );
    assert_eq!(
        problems,
        ["AC-2: failed needs a cited fail result or reproduced evidence"]
    );
    let problems = judge_problems(
        &rig,
        &text(
            same("AC-1"),
            same("AC-2"),
            ruling("AC-3", "failed", "codex says", &["codex/r2/e1"]),
        ),
    );
    assert!(
        problems[0].starts_with("AC-3: cited evidence codex/r2/e1 is not evidence for AC-3"),
        "{problems:?}"
    );
    // claude-2 reproduced codex's AC-1 fail: that backs AC-1 failing.
    let problems = judge_problems(
        &rig,
        &text(
            ruling("AC-1", "failed", "reproduced", &["claude-2/r2/e1"]),
            same("AC-2"),
            same("AC-3"),
        ),
    );
    assert_eq!(problems, Vec::<String>::new());
}

/// Round 1 of a one-round verification where every verifier passed every
/// criterion on a matched re-run and claude-2 reported a high defect.
fn all_passed_with_a_defect() -> Rig {
    all_passed_with_a_defect_except(&[])
}

/// [`all_passed_with_a_defect`] with the items keyed in `gone` gone before
/// ingest.
fn all_passed_with_a_defect_except(gone: &[&str]) -> Rig {
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
    rig.ingest_except(gone, &Default::default());
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    rig
}

fn verdict_json(decision: &str, blocking: &str) -> String {
    format!(
        r#", "verdict": {{"decision": "{decision}", "rationale": "r", "blocking": [{blocking}]}}"#
    )
}

#[test]
fn ac14_a_must_criterion_ruled_not_applicable_refuses_pass_and_needs_blocking() {
    let rig = all_passed_with_a_defect();
    let rulings = vec![
        ruling("AC-1", "not_applicable", "Tabs are not part of it.", &[]),
        ruling("AC-2", "verified", "matched", &["codex/r1/e1"]),
        ruling("AC-3", "verified", "matched", &["claude-2/r1/e1"]),
    ];
    // The defect is rejected: only AC-1 holds the change back.
    let problems = |decision: &str, blocking: &str| {
        judge_problems(
            &rig,
            &judge_json(&rulings, "rejected", &verdict_json(decision, blocking)),
        )
    };
    assert_eq!(
        problems("pass", ""),
        [
            "verdict: pass needs every must criterion verified; AC-1 (must) was ruled not_applicable; use pass_with_issues with it in blocking"
        ]
    );
    assert_eq!(
        problems("pass_with_issues", ""),
        ["verdict: pass_with_issues needs blocking to list what holds it back: AC-1"]
    );
    assert_eq!(
        problems("pass_with_issues", "\"AC-1\""),
        Vec::<String>::new()
    );
}

#[test]
fn ac14_pass_with_issues_lists_everything_that_holds_the_change_back() {
    let rig = all_passed_with_a_defect();
    // Every must verified, AC-3 (should) inconclusive, D1 high and open.
    let rulings = vec![
        ruling("AC-1", "verified", "matched", &["claude/r1/e1"]),
        ruling("AC-2", "verified", "matched", &["codex/r1/e1"]),
        ruling("AC-3", "inconclusive", "not measured", &[]),
    ];
    let problems = |blocking: &str| {
        judge_problems(
            &rig,
            &judge_json(
                &rulings,
                "open",
                &verdict_json("pass_with_issues", blocking),
            ),
        )
    };
    assert_eq!(
        problems("\"AC-3\""),
        [
            "verdict.blocking: missing D1; pass_with_issues lists everything that holds the change back"
        ]
    );
    assert_eq!(
        problems("\"D1\""),
        [
            "verdict.blocking: missing AC-3; pass_with_issues lists everything that holds the change back"
        ]
    );
    assert_eq!(problems("\"AC-3\", \"D1\""), Vec::<String>::new());
}

#[test]
fn assignments_name_a_result_the_verifier_has() {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    for part in ["claude", "claude-2"] {
        let at = rig.tick();
        rig.feed(crate::engine::Input::Sent {
            round: 1,
            part: p(part),
            kind: crate::model::types::BriefKind::Work,
            at,
        });
    }
    // codex fails before replying: it has no result to cross-check.
    for _ in 0..3 {
        rig.feed(crate::engine::Input::SendFailed {
            round: 1,
            part: p("codex"),
            kind: crate::model::types::BriefKind::Work,
            error: "pane gone".into(),
        });
    }
    rig.reply("claude", &all_pass());
    rig.reply("claude-2", &all_pass());
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling);
    let rulings = format!(
        r#""rulings": [{}, {}, {}]"#,
        ruling(
            "AC-1",
            "verified",
            "two",
            &["claude/r1/e1", "claude-2/r1/e1"]
        ),
        ruling(
            "AC-2",
            "verified",
            "two",
            &["claude/r1/e1", "claude-2/r1/e1"]
        ),
        ruling(
            "AC-3",
            "verified",
            "two",
            &["claude/r1/e1", "claude-2/r1/e1"]
        ),
    );
    let text = |assign: &str| format!(r#"{{"summary": "s", {rulings}, "assign": {assign}}}"#);
    assert_eq!(
        judge_problems(&rig, &text(r#"{"claude": ["codex/AC-1"]}"#)),
        ["assign.claude[0]: codex has no result on AC-1 to reproduce or refute"]
    );
    assert_eq!(
        judge_problems(&rig, &text(r#"{"claude": ["claude-2/AC-1"]}"#)),
        Vec::<String>::new()
    );
}

#[test]
fn the_cross_brief_lists_every_result_so_far_not_only_the_latest() {
    let mut rig = ruled_round(3);
    rig.judge(&judge_json(&good_rulings(), "open", ""));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    // codex turns its AC-1 fail into a pass.
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
        let results = if part == "codex" {
            r#""results": [{"criterion": "AC-1", "result": "pass", "note": "fixed by a rebuild", "evidence": ["e1"]}], "evidence": [{"id": "e1", "kind": "command", "cmd": "cargo test cwd_after_rebuild", "cwd": ".", "exit": 0, "log": "logs/1.log", "rerun": false}], "#
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
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    rig.judge(&format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        ruling("AC-1", "inconclusive", "codex changed its mind", &[]),
        same("AC-2"),
        same("AC-3")
    ));
    assert_eq!(rig.round(), 3, "{:?}", rig.rejected("judge"));
    let brief = rig.brief("claude", crate::model::types::BriefKind::Work);
    let so_far = &brief[brief.find("## 5. Rulings and results so far").unwrap()..];
    let so_far = &so_far[..so_far.find("## 6.").unwrap()];
    let ac1 = &so_far[so_far.find("### AC-1").unwrap()..so_far.find("### AC-2").unwrap()];
    for line in [
        "- codex R1: fail — Saw it.",
        "  - codex/r1/e1: command `cargo test e1` (cwd .) exit 101",
        "- codex R2: pass — fixed by a rebuild",
        "  - codex/r2/e1: command `cargo test cwd_after_rebuild` (cwd .) exit 0",
    ] {
        assert!(ac1.contains(line), "{line:?} in\n{ac1}");
    }
}

/// [`ruled_round`] ruled once with claude-2 assigned codex's AC-1 fail,
/// and round 2 with claude-2's `not_reproduced` on it citing `e1`; the
/// items keyed in `gone` gone before round 2's ingest.
fn fail_answered_with(gone: &[&str]) -> Rig {
    let mut rig = ruled_round(3);
    rig.judge(&judge_json(
        &good_rulings(),
        "open",
        r#", "assign": {"claude-2": ["codex/AC-1"]}"#,
    ));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
    rig.reply("codex", r#"{"summary": "Nothing assigned."}"#);
    rig.reply(
        "claude-2",
        &reply_json(
            &[],
            &[cmd("e1", 0, false)],
            r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "not_reproduced", "note": "passes here", "evidence": ["e1"]}]"#,
        ),
    );
    rig.ingest_except(gone, &Default::default());
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    rig
}

#[test]
fn ac13_a_not_reproduced_whose_evidence_was_not_stored_answers_no_fail() {
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    let text = format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        ruling("AC-1", "verified", "matched", &["claude/r1/e1"]),
        same("AC-2"),
        same("AC-3")
    );
    let rig = fail_answered_with(&["claude-2/r2/e1"]);
    assert_eq!(
        judge_problems(&rig, &text),
        [
            "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered; claude-2/r2/e1 was not stored (gone before ingest) and counts for nothing"
        ]
    );
    // Stored, the same answer lets AC-1 be verified.
    let rig = fail_answered_with(&[]);
    assert_eq!(judge_problems(&rig, &text), Vec::<String>::new());
}

/// [`ruled_round`] ruled once with claude-2 assigned codex's AC-1 fail,
/// then round 2 with `codex` and `claude_2` as those verifiers' replies
/// (claude has nothing assigned), everything stored.
fn second_round(codex: &str, claude_2: &str) -> Rig {
    let mut rig = ruled_round(3);
    rig.judge(&judge_json(
        &good_rulings(),
        "open",
        r#", "assign": {"claude-2": ["codex/AC-1"]}"#,
    ));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
    rig.reply("codex", codex);
    rig.reply("claude-2", claude_2);
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(
        rig.phase(),
        Phase::Ruling,
        "codex: {:?}, claude-2: {:?}",
        rig.rejected("codex"),
        rig.rejected("claude-2")
    );
    rig
}

/// Round 2's ruling: AC-1 verified on claude's matched re-run, the others
/// carried over.
fn ac1_verified_on_claudes_rerun() -> String {
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        ruling("AC-1", "verified", "matched", &["claude/r1/e1"]),
        same("AC-2"),
        same("AC-3")
    )
}

#[test]
fn ac13_a_not_reproduced_on_media_for_another_criterion_answers_no_fail() {
    let answer = |shows: &str| {
        reply_json(
            &[],
            &[format!(
                r#"{{"id": "f1", "kind": "file", "path": "shots/store.png", "caption": "the store after reopen", "shows": ["{shows}"]}}"#
            )],
            r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "not_reproduced", "note": "passes here", "evidence": ["f1"]}]"#,
        )
    };
    let nothing = r#"{"summary": "Nothing assigned."}"#;
    // The picture is stored but shows AC-2 only: it is no evidence for
    // AC-1, so codex's AC-1 fail stays open.
    let rig = second_round(nothing, &answer("AC-2"));
    assert_eq!(
        judge_problems(&rig, &ac1_verified_on_claudes_rerun()),
        [
            "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered; claude-2/r2/f1 is not evidence for AC-1 (its shows leaves AC-1 out)"
        ]
    );
    // Showing AC-1, the same answer lets AC-1 be verified.
    let rig = second_round(nothing, &answer("AC-1"));
    assert_eq!(
        judge_problems(&rig, &ac1_verified_on_claudes_rerun()),
        Vec::<String>::new()
    );
}

#[test]
fn ac13_a_later_blocked_result_of_the_same_verifier_answers_no_fail() {
    // codex turns its AC-1 fail into blocked; claude-2 only disputes it.
    let rig = second_round(
        r#"{"summary": "s", "results": [{"criterion": "AC-1", "result": "blocked", "note": "the tool is not available"}]}"#,
        r#"{"summary": "s", "cross_checks": [{"subject": "codex/AC-1", "outcome": "disputed", "note": "not looked at"}]}"#,
    );
    assert_eq!(
        judge_problems(&rig, &ac1_verified_on_claudes_rerun()),
        [
            "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered"
        ]
    );
}

#[test]
fn ac13_a_not_reproduced_about_a_later_pass_answers_no_earlier_fail() {
    // R1: codex fails AC-1, claude passes it on a matched re-run. Nobody is
    // assigned codex's fail in R2, where codex turns it into a pass.
    let mut rig = ruled_round(4);
    rig.judge(&judge_json(
        &good_rulings(),
        "open",
        r#", "assign": {"claude": ["codex/AC-3"]}"#,
    ));
    assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
    rig.deliver();
    rig.reply(
        "claude",
        r#"{"summary": "s", "cross_checks": [{"subject": "codex/AC-3", "outcome": "disputed", "note": "not looked at"}]}"#,
    );
    rig.reply(
        "codex",
        &reply_json(&[("AC-1", "pass", &["e1"])], &[cmd("e1", 0, false)], ""),
    );
    rig.reply("claude-2", r#"{"summary": "Nothing assigned."}"#);
    rig.ingest();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    rig.judge(&format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}], "assign": {{"claude-2": ["codex/AC-1"]}}}}"#,
        ruling("AC-1", "inconclusive", "codex changed its mind", &[]),
        same("AC-2"),
        same("AC-3")
    ));
    assert_eq!(rig.round(), 3, "{:?}", rig.rejected("judge"));
    // R3: claude-2 is asked about codex's R2 pass, and cannot reproduce it.
    let brief = rig.brief("claude-2", crate::model::types::BriefKind::Work);
    assert!(brief.contains("- codex/AC-1: pass (R2)"), "{brief}");
    rig.deliver();
    rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
    rig.reply("codex", r#"{"summary": "Nothing assigned."}"#);
    rig.reply(
        "claude-2",
        &reply_json(
            &[],
            &[cmd("e1", 1, false)],
            r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "not_reproduced", "note": "fails here", "evidence": ["e1"]}]"#,
        ),
    );
    rig.ingest();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude-2"));
    // That answers the R2 pass, not the R1 fail: no check was ever about
    // the fail, so another matched re-run does not make AC-1 verified.
    let text = format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        ruling("AC-1", "verified", "matched", &["claude/r1/e1"]),
        same("AC-2"),
        same("AC-3")
    );
    assert_eq!(
        judge_problems(&rig, &text),
        [
            "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered; claude-2's R3 not_reproduced is about codex's R2 pass"
        ]
    );
}

#[test]
fn ac13_passes_of_every_round_whose_evidence_is_stored_count() {
    // Round 1 verified AC-3 on claude's and codex's passes; codex passes it
    // again on another log in round 2. The round-1 passes still count.
    let rig = second_round(
        &reply_json(&[("AC-3", "pass", &["e1"])], &[cmd("e1", 0, false)], ""),
        r#"{"summary": "s", "cross_checks": [{"subject": "codex/AC-1", "outcome": "disputed", "note": "not looked at"}]}"#,
    );
    let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
    let text = format!(
        r#"{{"summary": "s", "rulings": [{}, {}, {}]}}"#,
        same("AC-1"),
        same("AC-2"),
        same("AC-3")
    );
    assert_eq!(judge_problems(&rig, &text), Vec::<String>::new());
    let out = parse_judge(&text, rig.engine.session(), 2).unwrap();
    assert_eq!(
        out.rulings["AC-3"].evidence,
        [testkit::key("claude/r1/e2"), testkit::key("codex/r1/e2")]
    );
}

#[test]
fn ac30_only_media_take_shows() {
    let rig = round_one();
    let text = reply_json(
        &[
            ("AC-1", "pass", &["e1", "f1"]),
            ("AC-2", "not_tested", &[]),
            ("AC-3", "not_tested", &[]),
        ],
        &[
            cmd("e1", 0, false),
            r#"{"id": "f1", "kind": "file", "path": "notes.txt", "shows": ["AC-1"]}"#.to_owned(),
        ],
        "",
    );
    let files = facts_for(&text);
    assert_eq!(
        reply_problems(&rig, "claude", &text, &files),
        ["evidence[1] (f1).shows: notes.txt is not an image or a video; only media take shows"]
    );
    // The same file without shows goes through.
    let plain = text.replace(r#", "shows": ["AC-1"]"#, "");
    assert_eq!(
        reply_problems(&rig, "claude", &plain, &files),
        Vec::<String>::new()
    );
}

#[test]
fn ac30_an_empty_shows_is_refused_rather_than_read_as_left_out() {
    let rig = round_one();
    let text = reply_json(
        &[
            ("AC-1", "pass", &["f1"]),
            ("AC-2", "not_tested", &[]),
            ("AC-3", "not_tested", &[]),
        ],
        &[
            r#"{"id": "f1", "kind": "file", "path": "shots/tab.png", "caption": "pwd in the new tab", "shows": []}"#
                .to_owned(),
        ],
        "",
    );
    let files = facts_for(&text);
    assert_eq!(
        reply_problems(&rig, "claude", &text, &files),
        ["evidence[0] (f1).shows is empty; name the criteria it shows, or leave the field out"]
    );
    // Left out, the picture is evidence for what cites it.
    let left_out = text.replace(r#", "shows": []"#, "");
    assert_eq!(
        reply_problems(&rig, "claude", &left_out, &files),
        Vec::<String>::new()
    );
}

/// Round 1 where codex fails AC-1 on a stored video `v1` alone, and claude
/// and claude-2 pass AC-3 each on a stored video `v1` of their own; every
/// other result passes on a matched re-run of `e1`.
fn ruled_on_videos() -> Rig {
    let mut rig = Rig::new(2);
    testkit::accept_default_criteria(&mut rig);
    testkit::start_round_one(&mut rig);
    rig.deliver();
    let video = r#"{"id": "v1", "kind": "file", "path": "video/tab.mov", "caption": "cd /tmp, ⌘T, pwd",
        "marks": [{"t": "00:06.5", "note": "the new tab shows $HOME"}]}"#;
    let ac3_on_video = reply_json(
        &[
            ("AC-1", "pass", &["e1"]),
            ("AC-2", "pass", &["e1"]),
            ("AC-3", "pass", &["v1"]),
        ],
        &[cmd("e1", 0, true), video.to_owned()],
        "",
    );
    rig.reply("claude", &ac3_on_video);
    rig.reply(
        "codex",
        &reply_json(
            &[
                ("AC-1", "fail", &["v1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "pass", &["e1"]),
            ],
            &[cmd("e1", 0, true), video.to_owned()],
            "",
        ),
    );
    rig.reply("claude-2", &ac3_on_video);
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
    rig
}

#[test]
fn l1_5a_a_ruling_on_video_alone_must_say_what_its_caption_and_marks_show() {
    let rig = ruled_on_videos();
    let text = |ac1: String, ac3: String| {
        format!(
            r#"{{"summary": "s", "rulings": [{ac1}, {}, {ac3}]}}"#,
            ruling("AC-2", "verified", "matched", &["claude/r1/e1"])
        )
    };
    let ac1_failed =
        |rationale: &str, evidence: &[&str]| ruling("AC-1", "failed", rationale, evidence);
    let ac3_verified = |rationale: &str| {
        ruling(
            "AC-3",
            "verified",
            rationale,
            &["claude/r1/v1", "claude-2/r1/v1"],
        )
    };
    assert_eq!(
        judge_problems(
            &rig,
            &text(ac1_failed("", &["codex/r1/v1"]), ac3_verified("two passes"))
        ),
        [
            "AC-1: failed rests on video evidence alone (codex/r1/v1); its rationale must name the caption and marks it relied on"
        ]
    );
    assert_eq!(
        judge_problems(
            &rig,
            &text(
                ac1_failed(
                    "the caption says pwd at 00:06.5 shows $HOME",
                    &["codex/r1/v1"]
                ),
                ac3_verified("")
            )
        ),
        [
            "AC-3: verified rests on video evidence alone (claude/r1/v1, claude-2/r1/v1); its rationale must name the caption and marks it relied on"
        ]
    );
    // Beside a command, a video is not alone.
    assert_eq!(
        judge_problems(
            &rig,
            &text(
                ac1_failed("", &["codex/r1/v1", "claude/r1/e1"]),
                ac3_verified("both recordings end under 200 ms")
            )
        ),
        Vec::<String>::new()
    );
    let brief = rig.brief("judge", crate::model::types::BriefKind::Work);
    assert!(
        brief.contains("When a verified or failed ruling rests on video evidence alone (no command, image or other file among the stored evidence it cites), its rationale must name the caption and marks it relied on; an empty rationale there is refused."),
        "{brief}"
    );
}
