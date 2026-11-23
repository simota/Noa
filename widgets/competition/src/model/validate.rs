//! Validation of external agent outputs: contestant submission Markdown and
//! judge evaluation JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use super::types::{
    Amendment, AxisScore, EntryEvaluation, Evidence, EvidenceBasis, JudgeOutput, PartId, RoundNo,
    Session,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub problems: Vec<String>,
}

impl ValidationError {
    fn one(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, problem) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

/// Validated contestant report: a short one-line summary plus the full body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubmission {
    pub summary: String,
    pub body: String,
}

/// Validates a contestant's Markdown submission report.
pub fn parse_submission(text: &str) -> Result<ParsedSubmission, ValidationError> {
    let body = text.trim();
    if body.is_empty() {
        return Err(ValidationError::one("submission report is empty"));
    }
    let first_non_empty = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let cleaned = first_non_empty
        .trim_start_matches('#')
        .trim()
        .strip_prefix("Summary:")
        .or_else(|| {
            first_non_empty
                .trim_start_matches('#')
                .trim()
                .strip_prefix("要約:")
        })
        .unwrap_or_else(|| first_non_empty.trim_start_matches('#').trim())
        .trim();
    let summary = if cleaned.is_empty() {
        "提出レポート".to_owned()
    } else {
        cleaned.chars().take(120).collect()
    };
    Ok(ParsedSubmission {
        summary,
        body: body.to_owned(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJudgeOutput {
    overview: String,
    evaluations: BTreeMap<String, RawEvaluation>,
    ranking: Vec<String>,
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    final_verdict: Option<String>,
    #[serde(default)]
    close: bool,
    #[serde(default)]
    amendments: Vec<RawAmendment>,
    #[serde(default)]
    checklist: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvaluation {
    score: u16,
    #[serde(default)]
    strengths: String,
    feedback: String,
    #[serde(default)]
    evidence: Vec<RawEvidence>,
    #[serde(default)]
    breakdown: Vec<RawAxis>,
}

/// Most criteria one evaluation's breakdown may list.
const MAX_AXES: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAxis {
    axis: String,
    score: u16,
    max: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidence {
    claim: String,
    basis: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAmendment {
    round: RoundNo,
    contestant: String,
    score: u16,
    reason: String,
}

fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some((_, inner)) = rest.split_once('\n') else {
        return trimmed;
    };
    inner.trim_end().strip_suffix("```").unwrap_or(inner).trim()
}

/// Checks the judge's JSON for `round` against `session`.
pub fn parse_judge(
    text: &str,
    session: &Session,
    round: RoundNo,
) -> Result<JudgeOutput, ValidationError> {
    let raw: RawJudgeOutput = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid judge JSON: {e}")))?;
    if session.round(round).is_none() {
        return Err(ValidationError::one(format!("there is no round {round}")));
    }

    let mut problems = Vec::new();
    let overview = raw.overview.trim().to_owned();
    if overview.is_empty() {
        problems.push("\"overview\" is empty".to_owned());
    }

    let expected: BTreeSet<&str> = session.contestant_ids().map(|p| p.as_str()).collect();
    let mut evaluations = BTreeMap::new();
    for (id, ev) in raw.evaluations {
        if !expected.contains(id.as_str()) {
            problems.push(format!("evaluations.{id}: unknown contestant"));
            continue;
        }
        if ev.score > 100 {
            problems.push(format!("evaluations.{id}.score: {} exceeds 100", ev.score));
        }
        let feedback = ev.feedback.trim().to_owned();
        if feedback.is_empty() {
            problems.push(format!("evaluations.{id}.feedback is empty"));
        }
        let mut evidence = Vec::new();
        for (i, item) in ev.evidence.into_iter().enumerate() {
            let claim = item.claim.trim().to_owned();
            if claim.is_empty() {
                problems.push(format!("evaluations.{id}.evidence[{i}].claim is empty"));
            }
            match EvidenceBasis::ALL
                .into_iter()
                .find(|b| b.as_str() == item.basis.trim())
            {
                Some(basis) => evidence.push(Evidence { claim, basis }),
                None => problems.push(format!(
                    "evaluations.{id}.evidence[{i}].basis {:?} must be one of measured, self_reported, unverified",
                    item.basis
                )),
            }
        }
        if ev.breakdown.len() > MAX_AXES {
            problems.push(format!(
                "evaluations.{id}.breakdown has {} entries; at most {MAX_AXES}",
                ev.breakdown.len()
            ));
        }
        let mut breakdown = Vec::new();
        for (i, axis) in ev.breakdown.iter().take(MAX_AXES).enumerate() {
            let at = format!("evaluations.{id}.breakdown[{i}]");
            if axis.axis.trim().is_empty() {
                problems.push(format!("{at}.axis is empty"));
            }
            if axis.max == 0 || axis.max > 100 {
                problems.push(format!("{at}.max must be 1..=100"));
            }
            if axis.score > axis.max {
                problems.push(format!(
                    "{at}.score {} exceeds its max {}",
                    axis.score, axis.max
                ));
            }
            breakdown.push(AxisScore {
                axis: axis.axis.trim().to_owned(),
                score: axis.score.min(100) as u8,
                max: axis.max.min(100) as u8,
            });
        }
        if !breakdown.is_empty() {
            let sum: u32 = ev.breakdown.iter().map(|a| u32::from(a.score)).sum();
            if sum != u32::from(ev.score) {
                problems.push(format!(
                    "evaluations.{id}.breakdown scores add up to {sum}, not the score {}",
                    ev.score
                ));
            }
        }
        evaluations.insert(
            PartId::new(id),
            EntryEvaluation {
                score: ev.score.min(100) as u8,
                strengths: ev.strengths.trim().to_owned(),
                feedback,
                evidence,
                amended: None,
                breakdown,
            },
        );
    }
    for part in &expected {
        if !evaluations.contains_key(&PartId::new(*part)) {
            problems.push(format!("evaluations is missing contestant {part:?}"));
        }
    }

    let mut ranking = Vec::new();
    let mut seen_rank = BTreeSet::new();
    for id in raw.ranking {
        if !expected.contains(id.as_str()) {
            problems.push(format!("ranking contains unknown contestant {id:?}"));
        } else if !seen_rank.insert(id.clone()) {
            problems.push(format!("ranking lists contestant {id:?} more than once"));
        } else {
            ranking.push(PartId::new(id));
        }
    }
    if seen_rank.len() != expected.len() {
        problems.push("ranking must include every contestant exactly once".to_owned());
    }

    let is_last = session.max_rounds.is_some_and(|max| round >= max);
    let winner = raw
        .winner
        .map(|w| w.trim().to_owned())
        .filter(|w| !w.is_empty());
    let final_verdict = raw
        .final_verdict
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());

    let winner_id = match winner {
        Some(w) => {
            if !expected.contains(w.as_str()) {
                problems.push(format!("winner {w:?} is not a contestant"));
                None
            } else {
                Some(PartId::new(w))
            }
        }
        None => None,
    };

    if is_last || raw.close {
        if winner_id.is_none() {
            problems.push("\"winner\" is required when closing or on the final round".to_owned());
        }
        if final_verdict.is_none() {
            problems.push(
                "\"final_verdict\" is required when closing or on the final round".to_owned(),
            );
        }
    } else if winner_id.is_some() && final_verdict.is_none() {
        problems.push("\"final_verdict\" is required whenever \"winner\" is set".to_owned());
    }

    let mut amendments = Vec::new();
    for (i, raw) in raw.amendments.into_iter().enumerate() {
        let at = format!("amendments[{i}]");
        let judged = raw.round < round
            && session
                .round(raw.round)
                .and_then(|r| r.outcome.as_ref())
                .is_some_and(|o| {
                    o.evaluations
                        .contains_key(&PartId::new(raw.contestant.clone()))
                });
        if !judged {
            problems.push(format!(
                "{at}: round {} has no judged evaluation of {:?} (only earlier, judged rounds can be amended)",
                raw.round, raw.contestant
            ));
        }
        if raw.score > 100 {
            problems.push(format!("{at}.score: {} exceeds 100", raw.score));
        }
        let reason = raw.reason.trim().to_owned();
        if reason.is_empty() {
            problems.push(format!("{at}.reason is empty"));
        }
        amendments.push(Amendment {
            round: raw.round,
            part: PartId::new(raw.contestant),
            score: raw.score.min(100) as u8,
            reason,
        });
    }

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }

    Ok(JudgeOutput {
        overview,
        evaluations,
        ranking,
        winner: winner_id,
        final_verdict,
        close: raw.close || is_last,
        amendments,
        checklist: raw
            .checklist
            .into_iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, JUDGE_ID, Participant, Phase, Role, Round, RoundKind, SessionId,
    };

    fn session(max_rounds: Option<RoundNo>) -> Session {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
        };
        Session {
            id: SessionId::new("comp-1"),
            request: "Build a widget".into(),
            workdir: None,
            max_rounds,
            reveal_interval: 5,
            rev: 1,
            created_at: 1,
            judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
            contestants: vec![
                p("claude", AgentKind::Claude, Role::Contestant),
                p("codex", AgentKind::Codex, Role::Contestant),
            ],
            messages: vec![],
            rounds: vec![Round {
                no: 1,
                kind: RoundKind::Blind,
                phase: Phase::Judging,
                started_at: 1,
                briefs: vec![],
                submissions: BTreeMap::new(),
                judge_brief: None,
                outcome: None,
            }],
            winner: None,
            closed: false,
        }
    }

    #[test]
    fn valid_judge_json_parses_even_with_code_fence() {
        let s = session(Some(10));
        let raw = r#"```json
{
  "overview": "Solid start from both entries.",
  "evaluations": {
    "claude": { "score": 82, "strengths": "Clean layout", "feedback": "Improve contrast" },
    "codex": { "score": 88, "strengths": "Great interaction", "feedback": "Polish mobile view" }
  },
  "ranking": ["codex", "claude"],
  "winner": null,
  "final_verdict": null,
  "close": false
}
```"#;
        let out = parse_judge(raw, &s, 1).unwrap();
        assert_eq!(
            out.ranking,
            vec![PartId::new("codex"), PartId::new("claude")]
        );
        assert_eq!(out.evaluations[&PartId::new("codex")].score, 88);
        assert!(!out.close);
    }

    #[test]
    fn final_round_requires_winner_and_verdict() {
        let s = session(Some(1));
        let raw = r#"{
  "overview": "Done",
  "evaluations": {
    "claude": { "score": 80, "strengths": "ok", "feedback": "ok" },
    "codex": { "score": 85, "strengths": "ok", "feedback": "ok" }
  },
  "ranking": ["codex", "claude"]
}"#;
        let err = parse_judge(raw, &s, 1).unwrap_err();
        assert!(err.to_string().contains("\"winner\" is required"));
        assert!(err.to_string().contains("\"final_verdict\" is required"));
    }

    const ROUND_ONE: &str = r#"{
  "overview": "R1",
  "evaluations": {
    "claude": { "score": 70, "feedback": "a" },
    "codex": { "score": 80, "feedback": "b" }
  },
  "ranking": ["codex", "claude"]
}"#;

    /// A session whose round 1 is judged and round 2 is being judged.
    fn judged_round_one() -> Session {
        let mut s = session(Some(10));
        let first = parse_judge(ROUND_ONE, &s, 1).unwrap();
        s.rounds[0].outcome = Some(first);
        let mut second = s.rounds[0].clone();
        second.no = 2;
        second.outcome = None;
        s.rounds.push(second);
        s
    }

    #[test]
    fn evidence_carries_its_basis_and_rejects_unknown_ones() {
        let s = session(Some(10));
        let raw = r#"{
  "overview": "R1",
  "evaluations": {
    "claude": { "score": 70, "feedback": "a", "evidence": [
      { "claim": "index.html is 19 KB", "basis": "measured" },
      { "claim": "0 axe violations", "basis": "self_reported" }
    ] },
    "codex": { "score": 80, "feedback": "b", "evidence": [ { "claim": "x", "basis": "guessed" } ] }
  },
  "ranking": ["codex", "claude"]
}"#;
        let err = parse_judge(raw, &s, 1).unwrap_err().to_string();
        assert!(err.contains("evaluations.codex.evidence[0].basis"), "{err}");

        let ok = raw.replace("\"guessed\"", "\"unverified\"");
        let out = parse_judge(&ok, &s, 1).unwrap();
        let claude = &out.evaluations[&PartId::new("claude")];
        assert_eq!(claude.evidence.len(), 2);
        assert_eq!(claude.evidence[1].basis, EvidenceBasis::SelfReported);
    }

    #[test]
    fn amendments_correct_an_earlier_judged_round() {
        let mut s = judged_round_one();
        let raw = r#"{
  "overview": "R2",
  "evaluations": {
    "claude": { "score": 75, "feedback": "a" },
    "codex": { "score": 82, "feedback": "b" }
  },
  "ranking": ["codex", "claude"],
  "amendments": [ { "round": 1, "contestant": "claude", "score": 78, "reason": "R1 was judged without its stylesheet" } ]
}"#;
        let out = parse_judge(raw, &s, 2).unwrap();
        assert_eq!(out.amendments[0].score, 78);

        crate::model::reduce::apply(
            &mut s,
            &[crate::model::reduce::Event::JudgeAccepted {
                round: 2,
                output: out,
            }],
        )
        .unwrap();
        let claude = PartId::new("claude");
        let fixed = &s.round(1).unwrap().outcome.as_ref().unwrap().evaluations[&claude];
        assert_eq!(fixed.score, 78);
        let note = fixed.amended.as_ref().unwrap();
        assert_eq!((note.from, note.by_round), (70, 2));
        assert_eq!(s.score_history(&claude), vec![(1, 78), (2, 75)]);
    }

    #[test]
    fn amendments_only_reach_earlier_judged_rounds() {
        let s = judged_round_one();
        let raw = r#"{
  "overview": "R2",
  "evaluations": {
    "claude": { "score": 75, "feedback": "a" },
    "codex": { "score": 82, "feedback": "b" }
  },
  "ranking": ["codex", "claude"],
  "amendments": [
    { "round": 2, "contestant": "claude", "score": 1, "reason": "self" },
    { "round": 1, "contestant": "nobody", "score": 1, "reason": "x" },
    { "round": 1, "contestant": "codex", "score": 101, "reason": " " }
  ]
}"#;
        let err = parse_judge(raw, &s, 2).unwrap_err().to_string();
        assert!(err.contains("amendments[0]: round 2"), "{err}");
        assert!(
            err.contains("amendments[1]: round 1 has no judged evaluation of \"nobody\""),
            "{err}"
        );
        assert!(
            err.contains("amendments[2].score: 101 exceeds 100"),
            "{err}"
        );
        assert!(err.contains("amendments[2].reason is empty"), "{err}");
    }

    #[test]
    fn breakdown_must_add_up_to_the_score_and_checklist_is_kept() {
        let s = session(Some(10));
        let raw = r#"{
  "overview": "R1",
  "evaluations": {
    "claude": { "score": 70, "feedback": "a", "breakdown": [
      { "axis": "impact", "score": 40, "max": 50 }, { "axis": "craft", "score": 31, "max": 30 }
    ] },
    "codex": { "score": 80, "feedback": "b", "breakdown": [
      { "axis": "impact", "score": 45, "max": 50 }, { "axis": "craft", "score": 25, "max": 30 }
    ] }
  },
  "ranking": ["codex", "claude"],
  "checklist": [" 320x568: no horizontal scroll ", ""]
}"#;
        let err = parse_judge(raw, &s, 1).unwrap_err().to_string();
        assert!(
            err.contains("evaluations.claude.breakdown[1].score 31 exceeds its max 30"),
            "{err}"
        );
        assert!(
            err.contains("evaluations.claude.breakdown scores add up to 71, not the score 70"),
            "{err}"
        );
        assert!(
            err.contains("evaluations.codex.breakdown scores add up to 70, not the score 80"),
            "{err}"
        );

        let ok = raw
            .replace(r#""score": 31, "max": 30"#, r#""score": 30, "max": 30"#)
            .replace(r#""score": 45, "max": 50"#, r#""score": 50, "max": 50"#)
            .replace(r#""score": 25, "max": 30"#, r#""score": 30, "max": 30"#);
        let out = parse_judge(&ok, &s, 1).unwrap();
        assert_eq!(
            out.checklist,
            vec!["320x568: no horizontal scroll".to_owned()]
        );
        assert_eq!(out.evaluations[&PartId::new("codex")].breakdown.len(), 2);
    }

    #[test]
    fn an_amendment_drops_the_breakdown_it_no_longer_matches() {
        let mut s = session(Some(10));
        let first = r#"{
  "overview": "R1",
  "evaluations": {
    "claude": { "score": 70, "feedback": "a", "breakdown": [
      { "axis": "impact", "score": 50, "max": 60 }, { "axis": "craft", "score": 20, "max": 40 } ] },
    "codex": { "score": 80, "feedback": "b" }
  },
  "ranking": ["codex", "claude"]
}"#;
        s.rounds[0].outcome = Some(parse_judge(first, &s, 1).unwrap());
        let mut second = s.rounds[0].clone();
        second.no = 2;
        second.outcome = None;
        s.rounds.push(second);
        let raw = r#"{
  "overview": "R2",
  "evaluations": {
    "claude": { "score": 75, "feedback": "a" },
    "codex": { "score": 82, "feedback": "b" }
  },
  "ranking": ["codex", "claude"],
  "amendments": [ { "round": 1, "contestant": "claude", "score": 90, "reason": "misread" } ]
}"#;
        let out = parse_judge(raw, &s, 2).unwrap();
        crate::model::reduce::apply(
            &mut s,
            &[crate::model::reduce::Event::JudgeAccepted {
                round: 2,
                output: out,
            }],
        )
        .unwrap();
        let fixed =
            &s.round(1).unwrap().outcome.as_ref().unwrap().evaluations[&PartId::new("claude")];
        assert_eq!(fixed.score, 90);
        let sum: u32 = fixed.breakdown.iter().map(|a| u32::from(a.score)).sum();
        assert!(
            fixed.breakdown.is_empty() || sum == 90,
            "{:?}",
            fixed.breakdown
        );
    }

    #[test]
    fn an_oversized_breakdown_is_a_validation_error_not_a_panic() {
        let s = session(Some(10));
        let raw = r#"{
  "overview": "R1",
  "evaluations": {
    "claude": { "score": 64, "feedback": "a", "breakdown": [
      { "axis": "a", "score": 65535, "max": 100 }, { "axis": "b", "score": 1, "max": 100 } ] },
    "codex": { "score": 80, "feedback": "b" }
  },
  "ranking": ["codex", "claude"]
}"#;
        let result = std::panic::catch_unwind(|| parse_judge(raw, &s, 1));
        let err = result
            .expect("validation panicked")
            .unwrap_err()
            .to_string();
        assert!(err.contains("breakdown"), "{err}");
    }

    #[test]
    fn submission_parser_extracts_summary_line() {
        let parsed =
            parse_submission("# Summary: Refined hero typography\n\nDetails here.").unwrap();
        assert_eq!(parsed.summary, "Refined hero typography");
        assert!(parse_submission("   \n  ").is_err());
    }
}
