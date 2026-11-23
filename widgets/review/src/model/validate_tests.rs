use super::*;
use crate::testkit::{Rig, blind, p, review, through_round_one};

/// The problems `text` gets as `part`'s reply in the rig's current round.
fn review_problems(rig: &Rig, part: &str, text: &str) -> Vec<String> {
    let s = rig.engine.session();
    parse_review(text, s, &p(part), s.current_round().unwrap().no)
        .err()
        .map(|e| e.problems)
        .unwrap_or_default()
}

fn lead_problems(rig: &Rig, text: &str) -> Vec<String> {
    let s = rig.engine.session();
    parse_lead(text, s, s.current_round().unwrap().no)
        .err()
        .map(|e| e.problems)
        .unwrap_or_default()
}

fn finding(fields: &str) -> String {
    format!(r#"{{"summary": "s", "findings": [{{{fields}}}]}}"#)
}

const OK_FIELDS: &str = r#""id": "1", "location": {"path": "src/store.rs", "line": 5}, "severity": "high", "category": "correctness", "confidence": "high", "claim": "c", "evidence": "e""#;

const OK_PAIRS: [(&str, &str); 7] = [
    ("id", r#""1""#),
    ("location", r#"{"path": "src/store.rs", "line": 5}"#),
    ("severity", r#""high""#),
    ("category", r#""correctness""#),
    ("confidence", r#""high""#),
    ("claim", r#""c""#),
    ("evidence", r#""e""#),
];

/// A one-finding reply with `field` set to `value`.
fn with(field: &str, value: &str) -> String {
    let fields: Vec<String> = OK_PAIRS
        .iter()
        .map(|(name, v)| format!("\"{name}\": {}", if *name == field { value } else { v }))
        .collect();
    finding(&fields.join(", "))
}

#[test]
fn ac9_each_reviewer_rule_is_its_own_problem_line() {
    let rig = Rig::new(3);
    assert!(review_problems(&rig, "claude", &finding(OK_FIELDS)).is_empty());
    let cases: Vec<(String, &str)> = vec![
        ("not json".into(), "not valid review JSON"),
        (r#"{"summary": " "}"#.into(), "\"summary\" is empty"),
        (r#"{"summary": "s", "extra": 1}"#.into(), "unknown field"),
        (r#"{"summary": "s", "reviewer": "codex"}"#.into(), "is not you"),
        (with("id", r#""a b""#), "findings[0].id"),
        (with("location", r#"{"path": "/etc/passwd", "line": 1}"#), "must be relative"),
        (with("location", r#"{"path": "src/../x", "line": 1}"#), "without \"..\""),
        (with("location", r#"{"path": "src/store.rs", "line": 201}"#), "past the end of src/store.rs (200 lines"),
        (with("location", r#"{"path": "src/store.rs", "line": 9, "end_line": 4}"#), "end_line 4 is before line 9"),
        (with("location", r#"{"path": "src/store.rs", "line": 0}"#), "line must be 1 or more"),
        (with("severity", r#""urgent""#), "severity \"urgent\""),
        (with("category", r#""style""#), "category \"style\" must be one of correctness, security, performance, design, tests, ux_docs, other"),
        (with("confidence", r#""sure""#), "confidence \"sure\""),
        (with("claim", r#""""#), "claim is empty"),
        (with("claim", r#""two\nlines""#), "claim must be one line"),
        (with("claim", &format!("\"{}\"", "x".repeat(301))), "at most 300"),
        (with("evidence", r#"" ""#), "evidence is empty"),
        (
            r#"{"summary": "s", "findings": [], "stances": [{"finding": "F1", "stance": "confirm"}]}"#.into(),
            "stances: not allowed in the blind round",
        ),
        (
            r#"{"summary": "s", "coverage": {"reviewed": ["lib/"]}}"#.into(),
            "coverage.reviewed[0] \"lib/\" is not a target file",
        ),
    ];
    for (text, want) in &cases {
        let problems = review_problems(&rig, "claude", text);
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "{want:?} not among {problems:?} for {text}"
        );
    }
    // Two problems in one reply come back as two lines.
    let both = finding(
        &OK_FIELDS
            .replace(r#""high", "category""#, r#""nope", "category""#)
            .replace(r#""claim": "c""#, r#""claim": """#),
    );
    assert_eq!(review_problems(&rig, "claude", &both).len(), 2);
    // Over fifty findings.
    let many: Vec<String> = (0..51)
        .map(|i| {
            format!(
                "{{{}}}",
                OK_FIELDS.replace(r#""id": "1""#, &format!(r#""id": "{i}""#))
            )
        })
        .collect();
    let text = format!(r#"{{"summary": "s", "findings": [{}]}}"#, many.join(","));
    assert!(
        review_problems(&rig, "claude", &text)
            .iter()
            .any(|p| p.contains("at most 50"))
    );
    // A location outside the target is accepted as context; an omitted file
    // takes no line check.
    let ok = with("location", r#"{"path": "src/caller.rs", "line": 900}"#);
    assert!(review_problems(&rig, "claude", &ok).is_empty());
    let ok = with("location", r#"{"path": "assets/logo.png", "line": 3}"#);
    assert!(review_problems(&rig, "claude", &ok).is_empty());
    let numeric_id = finding(&OK_FIELDS.replace(r#""id": "1""#, r#""id": 7"#));
    assert!(review_problems(&rig, "claude", &numeric_id).is_empty());
}

#[test]
fn ac9_stance_rules_and_the_missing_list_in_cross_rounds() {
    let mut rig = Rig::new(3);
    through_round_one(&mut rig);
    // claude raised F1, so it owes a stance on F2 only.
    let missing = review_problems(&rig, "claude", &review(&[], &[]));
    assert_eq!(
        missing,
        ["stances: missing F2 (give confirm, dispute, refine or unsure)"]
    );
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "claude",
            review(&[], &[("F2", "confirm", ""), ("F1", "confirm", "")]),
            "you raised F1; give refine or withdraw",
        ),
        (
            "codex",
            review(&[], &[("F2", "withdraw", "")]),
            "only a reviewer who raised F2 can withdraw it",
        ),
        (
            "codex",
            review(&[], &[("F2", "dispute", "")]),
            "note is required for dispute",
        ),
        (
            "codex",
            review(&[], &[("F2", "refine", "")]),
            "note is required for refine",
        ),
        (
            "codex",
            review(&[], &[("F2", "maybe", "x")]),
            "stance \"maybe\"",
        ),
        (
            "codex",
            review(&[], &[("F9", "confirm", "")]),
            "\"F9\" is not a finding",
        ),
        (
            "codex",
            review(&[], &[("F2", "confirm", ""), ("F2", "unsure", "")]),
            "a second stance on F2",
        ),
        (
            "claude",
            review(
                &[("1", "src/store.rs", 3, "low", "tests")],
                &[("F2", "confirm", "")],
            ),
            "was used in an earlier round",
        ),
        (
            "claude",
            review(
                &[
                    ("9", "src/store.rs", 3, "low", "tests"),
                    ("9", "src/store.rs", 4, "low", "tests"),
                ],
                &[("F2", "confirm", "")],
            ),
            "used twice in this reply",
        ),
    ];
    for (part, text, want) in &cases {
        let problems = review_problems(&rig, part, text);
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "{want:?} not among {problems:?}"
        );
    }
    let ok = review(
        &[],
        &[("F2", "dispute", "No."), ("F1", "refine", "Also at 140.")],
    );
    assert!(
        review_problems(&rig, "claude", &ok)
            .iter()
            .all(|p| !p.contains("F1")),
        "an origin may refine"
    );
}

#[test]
fn ac10_each_lead_rule_is_its_own_problem_line() {
    let mut rig = Rig::new(3);
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
    let add = |id: &str, from: &str, extra: &str| {
        format!(
            r#"{{"op": "add", "id": "{id}", "from": [{from}], "claim": "c", "severity": "high", "category": "correctness", "confidence": "high", "status": "open", "rationale": "r"{extra}}}"#
        )
    };
    let all = format!(
        "{}, {}",
        add("F1", r#""claude/1", "codex/1""#, ""),
        r#"{"op": "dismiss", "raw": "agy/1", "reason": "fine"}"#
    );
    let ok = format!(r#"{{"summary": "s", "ops": [{all}]}}"#);
    assert!(
        lead_problems(&rig, &ok).is_empty(),
        "{:?}",
        lead_problems(&rig, &ok)
    );
    let cases: Vec<(String, &str)> = vec![
        (
            format!(
                r#"{{"summary": "s", "ops": [{}]}}"#,
                add("F1", r#""claude/1""#, "")
            ),
            "unassigned raw findings: codex/1, agy/1",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {}]}}"#,
                add("F2", r#""claude/1""#, "")
            ),
            "raw finding claude/1 is already placed",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {}]}}"#,
                add("F1", r#""x/9""#, "")
            ),
            "finding F1 already exists",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {}]}}"#,
                add("F 3", r#""claude/1""#, "")
            ),
            "id \"F 3\" must be",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {}]}}"#,
                add("F4", r#""nobody/1""#, "")
            ),
            "unknown raw finding \"nobody/1\"",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {{"op": "update", "id": "F1", "status": "rejected"}}]}}"#
            ),
            "rejecting F1 needs a \"rationale\"",
        ),
        (
            format!(r#"{{"summary": "s", "ops": [{all}, {{"op": "update", "id": "F1"}}]}}"#),
            "changes nothing",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {{"op": "update", "id": "F7", "status": "agreed"}}]}}"#
            ),
            "unknown finding \"F7\"",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}], "verdict": {{"decision": "approve", "rationale": "r"}}}}"#
            ),
            "verdict: only in the last round",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}], "close": true, "verdict": {{"decision": "approve", "rationale": "r"}}}}"#
            ),
            "close: not allowed in round 1",
        ),
        (
            format!(r#"{{"summary": "s", "ops": [{all}], "questions": {{"lead": "q"}}}}"#),
            "questions: unknown reviewer \"lead\"",
        ),
        (
            format!(r#"{{"summary": "s", "ops": [{all}], "questions": {{"codex": " "}}}}"#),
            "the question for codex is empty",
        ),
        (
            format!(r#"{{"summary": "", "ops": [{all}]}}"#),
            "\"summary\" is empty",
        ),
        (
            format!(
                r#"{{"summary": "s", "ops": [{all}, {{"op": "dismiss", "raw": "agy", "reason": "x"}}]}}"#
            ),
            "must be a raw finding as reviewer/id",
        ),
    ];
    for (text, want) in &cases {
        let problems = lead_problems(&rig, text);
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "{want:?} not among {problems:?}"
        );
    }
}

#[test]
fn ac10_verdict_rules_in_the_last_round() {
    let mut rig = Rig::new(2);
    through_round_one(&mut rig);
    rig.deliver();
    rig.reply("claude", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("codex", &review(&[], &[("F2", "confirm", "")]));
    rig.reply("agy", &review(&[], &[("F1", "confirm", "")]));
    let settle = r#"{"op": "update", "id": "F1", "status": "agreed"}, {"op": "update", "id": "F2", "status": "disputed"}"#;
    let verdict = |decision: &str, must: &str| {
        format!(
            r#"{{"summary": "s", "ops": [{settle}], "verdict": {{"decision": "{decision}", "rationale": "r", "must_fix": [{must}]}}}}"#
        )
    };
    assert!(lead_problems(&rig, &verdict("request_changes", r#""F1""#)).is_empty());
    assert!(lead_problems(&rig, &verdict("reject", "")).is_empty());
    let cases: Vec<(String, &str)> = vec![
        (r#"{"summary": "s"}"#.to_owned(), "verdict: required, this is the last round"),
        (verdict("approve", r#""F1""#), "approve takes no must_fix"),
        (verdict("request_changes", ""), "request_changes needs at least one finding"),
        (verdict("approve_with_fixes", ""), "approve_with_fixes needs at least one finding"),
        (verdict("ship", ""), "verdict.decision \"ship\""),
        (verdict("request_changes", r#""F9""#), "must_fix: \"F9\" is not a live finding"),
        (
            r#"{"summary": "s", "ops": [{"op": "update", "id": "F2", "status": "agreed"}], "verdict": {"decision": "reject", "rationale": "r"}}"#.to_owned(),
            "F1 still open; set agreed, disputed or rejected",
        ),
    ];
    for (text, want) in &cases {
        let problems = lead_problems(&rig, text);
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "{want:?} not among {problems:?}"
        );
    }
}

#[test]
fn a_fenced_reply_is_taken_as_its_content() {
    let rig = Rig::new(3);
    let fenced = format!("```json\n{}\n```", blind(&[]));
    assert!(review_problems(&rig, "claude", &fenced).is_empty());
}

#[test]
fn locations_know_whether_they_touch_the_diff() {
    let m = crate::testkit::manifest();
    let loc = |path: &str, line| Location {
        path: path.into(),
        line,
        end_line: None,
    };
    assert_eq!(
        locate(&m, &loc("src/store.rs", Some(125))),
        (true, Some(true))
    );
    assert_eq!(
        locate(&m, &loc("src/store.rs", Some(3))),
        (true, Some(false))
    );
    assert_eq!(locate(&m, &loc("assets/logo.png", None)), (true, None));
    assert_eq!(locate(&m, &loc("lib/other.rs", Some(3))), (false, None));
}
