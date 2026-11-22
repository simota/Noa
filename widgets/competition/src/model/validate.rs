//! Validation of external agent outputs: contestant submission Markdown and
//! judge evaluation JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use super::types::{EntryEvaluation, JudgeOutput, PartId, RoundNo, Session};

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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvaluation {
    score: u16,
    #[serde(default)]
    strengths: String,
    feedback: String,
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
        evaluations.insert(
            PartId::new(id),
            EntryEvaluation {
                score: ev.score.min(100) as u8,
                strengths: ev.strengths.trim().to_owned(),
                feedback,
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

    #[test]
    fn submission_parser_extracts_summary_line() {
        let parsed =
            parse_submission("# Summary: Refined hero typography\n\nDetails here.").unwrap();
        assert_eq!(parsed.summary, "Refined hero typography");
        assert!(parse_submission("   \n  ").is_err());
    }
}
