//! Markdown export for clipboard copy, saving reports, and adopting a
//! winning entry into a repository.

use std::fmt::Write as _;
use std::path::Path;

use super::types::{PartId, RoundKind, Session};

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
    let _ = writeln!(
        out,
        "- 審判: {}（{}）・参加者: {}",
        session.judge.id,
        session.judge.kind.command(),
        panel.join(", ")
    );
    if let Some(winner) = &session.winner {
        let _ = writeln!(out, "- 優勝: **{winner}**");
    }

    if let Some(verdict) = session.final_verdict() {
        let _ = writeln!(out, "\n## 最終講評\n\n{}", verdict.trim());
    }

    let _ = writeln!(out, "\n## リーダーボード\n");
    for (idx, part) in session.current_ranking().iter().enumerate() {
        let scores: Vec<String> = session
            .score_history(part)
            .into_iter()
            .map(|(r, s)| format!("R{r}:{s}"))
            .collect();
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
        let _ = writeln!(out, "{}. **{part}**{crown} — {hist}", idx + 1);
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
        for part in session.contestant_ids() {
            if let Some(sub) = round.submissions.get(part) {
                let score = round
                    .outcome
                    .as_ref()
                    .and_then(|o| o.evaluations.get(part))
                    .map(|e| format!(" ({}/100)", e.score))
                    .unwrap_or_default();
                let _ = writeln!(out, "- **{part}**{score}: {}", sub.summary);
            }
        }
    }
    out
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
