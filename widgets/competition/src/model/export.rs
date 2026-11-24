//! Markdown export for clipboard copy, saving reports, and adopting a
//! winning entry into a repository. A judge panel's report gives means,
//! each judge's score and the spread; a single judge's is as it always was.

use std::fmt::Write as _;
use std::path::Path;

use super::aggregate::{PanelScores, SPREAD_WARN};
use super::types::{PartId, Participant, RoundKind, RoundNo, Session};

pub fn result_markdown(session: &Session) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# コンペティション結果\n");
    let _ = writeln!(out, "## 依頼\n\n{}\n", session.request.trim());

    let rounds = session.rounds.len();
    let limit = session
        .max_rounds
        .map(|max| format!("（上限 {max}）"))
        .unwrap_or_default();
    let state = if session.closed {
        "終了"
    } else {
        "進行中"
    };
    let reveal = if session.reveal_interval == 0 {
        "完全ブラインド".to_owned()
    } else {
        format!("{}ラウンド毎に相互閲覧", session.reveal_interval)
    };
    let panel: Vec<&str> = session.contestant_ids().map(|p| p.as_str()).collect();
    let _ = writeln!(out, "- ラウンド: {rounds}{limit}・{state}・{reveal}");
    if session.is_panel() {
        let judges: Vec<String> = session
            .judges()
            .enumerate()
            .map(|(seat, judge)| judge_label(judge, seat == 0))
            .collect();
        let _ = writeln!(
            out,
            "- 審判: {}・参加者: {}",
            judges.join(", "),
            panel.join(", ")
        );
    } else {
        let _ = writeln!(
            out,
            "- 審判: {}（{}）・参加者: {}",
            session.judge.id,
            session.judge.kind.command(),
            panel.join(", ")
        );
    }
    if let Some(winner) = &session.winner {
        let _ = writeln!(out, "- 優勝: **{winner}**");
    }

    if let Some(verdict) = session.final_verdict() {
        let _ = writeln!(out, "\n## 最終講評\n\n{}", verdict.trim());
    }

    let _ = writeln!(out, "\n## リーダーボード\n");
    for (idx, part) in session.current_ranking().iter().enumerate() {
        let history = session.score_history(part);
        let scores: Vec<String> = if session.is_panel() {
            history
                .iter()
                .map(|(r, s)| {
                    let label = session
                        .score_label(*r, part)
                        .unwrap_or_else(|| s.to_string());
                    format!("R{r}:{label}")
                })
                .collect()
        } else {
            history.iter().map(|(r, s)| format!("R{r}:{s}")).collect()
        };
        let crown = if session.winner.as_ref() == Some(part) {
            " 🏆"
        } else {
            ""
        };
        let hist = if scores.is_empty() {
            "未採点".to_owned()
        } else {
            scores.join(" → ")
        };
        // A panel's last scored round, judge by judge.
        let last = history
            .last()
            .and_then(|(r, _)| Some((*r, session.panel_scores(*r, part)?)))
            .map(|(r, panel)| format!("（R{r}: {}）", judges_text(&panel)))
            .unwrap_or_default();
        let _ = writeln!(out, "{}. **{part}**{crown} — {hist}{last}", idx + 1);
    }

    let _ = writeln!(out, "\n## ラウンド記録");
    for round in &session.rounds {
        let kind = match round.kind {
            RoundKind::Blind => "ブラインド",
            RoundKind::Reveal => "相互閲覧",
        };
        let _ = writeln!(out, "\n### Round {} ({kind})", round.no);
        if let Some(outcome) = &round.outcome {
            let _ = writeln!(out, "\n**審判総評:** {}\n", outcome.overview.trim());
        }
        if round.outcome.is_some() && !round.scorecards.is_empty() {
            let _ = writeln!(out, "**審判別総評:**\n");
            for (seat, judge) in session.judges().enumerate() {
                if let Some(card) = round.scorecards.get(&judge.id) {
                    let _ = writeln!(
                        out,
                        "- {}: {}",
                        judge_label(judge, seat == 0),
                        card.overview.trim()
                    );
                }
            }
            out.push('\n');
        }
        for part in session.contestant_ids() {
            if let Some(sub) = round.submissions.get(part) {
                let score = match panel_score(session, round.no, part) {
                    Some(panel) => format!(" ({panel})"),
                    None => round
                        .outcome
                        .as_ref()
                        .and_then(|o| o.evaluations.get(part))
                        .map(|e| format!(" ({}/100)", e.score))
                        .unwrap_or_default(),
                };
                let _ = writeln!(out, "- **{part}**{score}: {}", sub.summary);
            }
        }
    }
    out
}

/// `judge（claude · opus, リード）`, `judge-2（codex）`.
fn judge_label(judge: &Participant, lead: bool) -> String {
    let mut about = judge.kind.command().to_owned();
    if let Some(setup) = judge.setup.label() {
        about.push_str(" · ");
        about.push_str(&setup);
    }
    if lead {
        about.push_str(", リード");
    }
    format!("{}（{about}）", judge.id)
}

/// `judge 80 / judge-2 83 / judge-3 81 · 差 3`; a spread from
/// [`SPREAD_WARN`] up is marked as the leaderboard marks it (`⚠ 差 18`).
fn judges_text(panel: &PanelScores) -> String {
    let scores: Vec<String> = panel
        .scores
        .iter()
        .map(|(judge, score)| format!("{judge} {score}"))
        .collect();
    let spread = panel.spread();
    let warn = if spread >= SPREAD_WARN { "⚠ " } else { "" };
    format!("{} · {warn}差 {spread}", scores.join(" / "))
}

/// `81.3/100 · judge 80 / judge-2 83 / judge-3 81 · 差 3`, in a panel
/// round.
fn panel_score(session: &Session, round: RoundNo, part: &PartId) -> Option<String> {
    let panel = session.panel_scores(round, part)?;
    Some(format!(
        "{}/100 · {}",
        panel.mean_label(),
        judges_text(&panel)
    ))
}

/// Prompt written for an agent started to integrate the winning entry into `workdir`.
pub fn adopt_task(
    session: &Session,
    winner: &PartId,
    entry_dir: &Path,
    task_path: &Path,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Adopt winning competition entry ({winner}) from {}\n\n\
         This file ({}) summarizes a multi-agent competition. Integrate the winning \
         deliverable from `{}` into this repository cleanly.\n\n\
         - Review the winning artifacts in `{}` and adapt them to the project's conventions.\n\
         - Run the repository's tests/checks after integration.\n\
         - Do not push or publish anything.\n",
        session.id,
        task_path.display(),
        entry_dir.display(),
        entry_dir.display(),
    );
    out.push('\n');
    out.push_str(&result_markdown(session));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::aggregate::tests::{cards, panel_session, settle};
    use crate::model::types::Submission;

    /// A panel's spread from `SPREAD_WARN` up is marked as the leaderboard
    /// marks it, in the leaderboard lines and in the round record.
    #[test]
    fn a_panel_report_marks_a_spread_over_the_warning_line() {
        let mut session = panel_session(Some(3));
        for part in ["claude", "codex"] {
            session.rounds[0].submissions.insert(
                PartId::new(part),
                Submission {
                    part: PartId::new(part),
                    summary: format!("{part} entry"),
                    body: String::new(),
                    artifacts: Vec::new(),
                    at: 3,
                },
            );
        }
        let all = cards(
            &[("codex", 85), ("claude", 60)],
            &[("codex", 80), ("claude", 78)],
            &[("codex", 88), ("claude", 62)],
        );
        settle(&mut session, 1, &all).unwrap();
        let report = result_markdown(&session);
        for line in [
            "（R1: judge 60 / judge-2 78 / judge-3 62 · ⚠ 差 18）",
            "- **claude** (66.7/100 · judge 60 / judge-2 78 / judge-3 62 · ⚠ 差 18): claude entry",
            "（R1: judge 85 / judge-2 80 / judge-3 88 · 差 8）",
            "- **codex** (84.3/100 · judge 85 / judge-2 80 / judge-3 88 · 差 8): codex entry",
        ] {
            assert!(report.contains(line), "missing {line:?} in\n{report}");
        }
    }
}
