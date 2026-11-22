//! Situation briefs for contestants and the judge, assembled from state alone.
//!
//! Information isolation is enforced here:
//! - In `RoundKind::Blind`, a contestant brief NEVER includes peer submissions,
//!   peer artifact directories, peer feedback, or peer scores.
//! - In `RoundKind::Reveal` (every `reveal_interval` completed rounds), the
//!   brief adds a Peer Showcase section containing the prior round's peer
//!   submissions, artifact snapshot paths, and the judge's round overview.

use std::fmt::Write as _;
use std::path::Path;

use super::types::{
    Brief, Message, MsgKind, PartId, Receipt, RoundKind, RoundNo, Session, Submission,
};

/// Render one contestant's artifact list for a brief.
fn format_artifacts(submission: &Submission, artifact_dir: &Path) -> String {
    if submission.artifacts.is_empty() {
        return "  (no files in entry directory yet)\n".to_owned();
    }
    let mut out = String::new();
    let _ = writeln!(out, "  Directory: {}", artifact_dir.display());
    for file in &submission.artifacts {
        let _ = writeln!(
            out,
            "  - {} ({}, {} bytes)",
            file.rel_path,
            file.kind.label(),
            file.bytes
        );
    }
    out
}

fn user_interventions_for<'a>(
    session: &'a Session,
    to: &PartId,
    round: RoundNo,
) -> Vec<&'a Message> {
    session
        .messages
        .iter()
        .filter(|m| {
            m.kind == MsgKind::Intervention
                && m.round <= round
                && (m.to.is_empty() || m.to.contains(to))
        })
        .collect()
}

/// Build the brief for contestant `to` in `round`.
///
/// `entry_dir` is `to`'s private working output directory (`entries/<to>/`).
/// `reply_dir` is the root directory holding `r<N>/artifacts/<part>/` snapshots.
pub fn contestant_brief(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    entry_dir: &Path,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
    let kind = session
        .round(round)
        .map(|r| r.kind)
        .unwrap_or_else(|| RoundKind::for_round(round, session.reveal_interval));
    let of = session
        .max_rounds
        .map(|max| format!(" of {max}"))
        .unwrap_or_default();
    let mode_label = match kind {
        RoundKind::Blind => "BLIND (isolated — peers' work is hidden)",
        RoundKind::Reveal => "PEER REVEAL (showcase of peers' work from the previous round)",
    };

    let mut body = String::new();
    let _ = writeln!(body, "## 1. Competition request and round");
    let _ = writeln!(body, "Request:\n{}\n", session.request);
    let _ = writeln!(body, "Contestant: {to}");
    let _ = writeln!(body, "Round: {round}{of} — {mode_label}");
    if let Some(max) = session.max_rounds {
        let left = max.saturating_sub(round);
        if left == 0 {
            let _ = writeln!(
                body,
                "Notice: This is the FINAL round. Deliver your strongest, polished work."
            );
        } else {
            let _ = writeln!(body, "Rounds remaining after this one: {left}");
        }
    }
    if let Some(until) = session.rounds_until_reveal(round) {
        let _ = writeln!(
            body,
            "Next peer reveal: in {until} round(s) (every {} rounds).",
            session.reveal_interval
        );
    }
    if let Some(workdir) = &session.workdir {
        let _ = writeln!(
            body,
            "Reference workspace (read-only context): {}",
            workdir.display()
        );
    }
    let _ = writeln!(
        body,
        "Your private output directory: {}",
        entry_dir.display()
    );
    body.push('\n');

    let _ = writeln!(body, "## 2. Competition rules");
    let _ = writeln!(
        body,
        "- Create or refine all your deliverables (designs, HTML/CSS/JS, SVG, images, code, docs) ONLY inside your private output directory: {}",
        entry_dir.display()
    );
    let _ = writeln!(
        body,
        "- If your deliverable is a UI or visual design, include a self-contained `index.html` (or SVG/PNG files) in your output directory so it can be previewed directly in the comparison viewer."
    );
    match kind {
        RoundKind::Blind => {
            let _ = writeln!(
                body,
                "- BLIND ROUND: Do NOT inspect, read, or access any other contestant's directory. Compete strictly on your own ideas and the judge's feedback to you."
            );
        }
        RoundKind::Reveal => {
            let _ = writeln!(
                body,
                "- PEER REVEAL ROUND: You may inspect the other contestants' round-{} snapshots listed below to understand their approaches, strengths, and gaps. Do NOT copy their files verbatim; synthesize better ideas and surpass them in your own output directory.",
                round.saturating_sub(1)
            );
        }
    }
    body.push('\n');

    let _ = writeln!(body, "## 3. Your previous work and private judge feedback");
    if let Some((prev_round, prev_sub)) = session.latest_submission_before(to, round) {
        let _ = writeln!(
            body,
            "Your last submission (R{prev_round}): {}",
            prev_sub.summary
        );
        let snap = reply_dir
            .join(format!("r{prev_round}"))
            .join("artifacts")
            .join(to.as_str());
        body.push_str(&format_artifacts(prev_sub, &snap));
    } else {
        let _ = writeln!(body, "(this is your first round; start fresh)");
    }
    if let Some((ev_round, ev)) = session.latest_evaluation_before(to, round) {
        let _ = writeln!(body, "\nJudge evaluation of your R{ev_round} entry:");
        let _ = writeln!(body, "- Score: {}/100", ev.score);
        if !ev.strengths.is_empty() {
            let _ = writeln!(body, "- Strengths: {}", ev.strengths);
        }
        let _ = writeln!(body, "- Feedback to address: {}", ev.feedback);
    }
    body.push('\n');

    if kind == RoundKind::Reveal {
        let prev = round.saturating_sub(1);
        let _ = writeln!(
            body,
            "## 4. Peer showcase (round {prev} — visible once every {} rounds)",
            session.reveal_interval
        );
        if let Some(prev_round) = session.round(prev) {
            if let Some(outcome) = &prev_round.outcome {
                let _ = writeln!(body, "Judge overview of R{prev}:\n{}\n", outcome.overview);
                let ranking: Vec<&str> = outcome.ranking.iter().map(|p| p.as_str()).collect();
                let _ = writeln!(body, "R{prev} ranking: {}\n", ranking.join(" > "));
            }
            let mut any_peer = false;
            for peer in session.contestant_ids().filter(|p| *p != to) {
                if let Some(sub) = prev_round.submissions.get(peer) {
                    any_peer = true;
                    let score = prev_round
                        .outcome
                        .as_ref()
                        .and_then(|o| o.evaluations.get(peer))
                        .map(|e| format!(" — score {}/100", e.score))
                        .unwrap_or_default();
                    let _ = writeln!(body, "### Peer: {peer} (R{prev}{score})");
                    let _ = writeln!(body, "Summary: {}", sub.summary);
                    let snap = reply_dir
                        .join(format!("r{prev}"))
                        .join("artifacts")
                        .join(peer.as_str());
                    body.push_str(&format_artifacts(sub, &snap));
                    let _ = writeln!(body, "Report:\n{}\n", sub.body.trim());
                }
            }
            if !any_peer {
                let _ = writeln!(body, "(no peer submissions were recorded in R{prev})\n");
            }
        } else {
            let _ = writeln!(body, "(previous round not found)\n");
        }
    }

    let interventions = user_interventions_for(session, to, round);
    let _ = writeln!(body, "## 5. User instructions / interventions");
    if interventions.is_empty() {
        let _ = writeln!(body, "(none)");
    } else {
        for msg in interventions {
            let _ = writeln!(body, "- [{} R{}] {}", msg.id, msg.round, msg.body.trim());
        }
    }
    body.push('\n');

    let _ = writeln!(body, "## 6. How to submit");
    let _ = writeln!(
        body,
        "1. Write or update your deliverables in: {}",
        entry_dir.display()
    );
    let _ = writeln!(
        body,
        "2. Write your Markdown submission report to: {}",
        reply_path.display()
    );
    let _ = writeln!(
        body,
        "   Start the report with a one-line summary (e.g. `# Summary: ...`), then describe what you built or improved this round, design/technical choices, and list key files in your output directory."
    );
    let _ = writeln!(
        body,
        "   Write the file only after all deliverable files are saved."
    );

    Brief {
        to: to.clone(),
        base_rev: session.rev,
        body: body.trim_end().to_owned() + "\n",
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// Build the brief for the judge to evaluate all submissions in `round`.
pub fn judge_brief(
    session: &Session,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
    let of = session
        .max_rounds
        .map(|max| format!(" of {max}"))
        .unwrap_or_default();
    let is_last = session.max_rounds.is_some_and(|max| round >= max);
    let contestants: Vec<&str> = session.contestant_ids().map(|p| p.as_str()).collect();

    let mut body = String::new();
    let _ = writeln!(body, "## 1. Competition judging — Round {round}{of}");
    let _ = writeln!(body, "Request:\n{}\n", session.request);
    let _ = writeln!(body, "Contestants: {}", contestants.join(", "));
    let _ = writeln!(
        body,
        "Peer-reveal schedule: every {} rounds (0 = disabled)",
        session.reveal_interval
    );
    body.push('\n');

    let _ = writeln!(body, "## 2. Round {round} submissions and artifacts");
    if let Some(r) = session.round(round) {
        for part in session.contestant_ids() {
            let _ = writeln!(body, "### Contestant: {part}");
            if let Some(sub) = r.submissions.get(part) {
                let _ = writeln!(body, "Summary: {}", sub.summary);
                let snap = reply_dir
                    .join(format!("r{round}"))
                    .join("artifacts")
                    .join(part.as_str());
                body.push_str(&format_artifacts(sub, &snap));
                let _ = writeln!(body, "Report:\n{}\n", sub.body.trim());
            } else {
                let _ = writeln!(body, "(did not submit in R{round})\n");
            }
        }
    }

    let interventions: Vec<&Message> = session
        .messages
        .iter()
        .filter(|m| m.kind == MsgKind::Intervention && m.round <= round)
        .collect();
    let _ = writeln!(body, "## 3. User interventions");
    if interventions.is_empty() {
        let _ = writeln!(body, "(none)");
    } else {
        for msg in interventions {
            let target = if msg.to.is_empty() {
                "everyone".to_owned()
            } else {
                msg.to
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                body,
                "- [{} R{} to {target}] {}",
                msg.id,
                msg.round,
                msg.body.trim()
            );
        }
    }
    body.push('\n');

    let _ = writeln!(body, "## 4. Judging rules");
    let _ = writeln!(
        body,
        "- Inspect the actual artifact files in each contestant's snapshot directory above, not just their self-report."
    );
    let _ = writeln!(
        body,
        "- Score every contestant on 0..100 based on how well their deliverable satisfies the user's request (quality, completeness, aesthetics/craft, usability)."
    );
    let _ = writeln!(
        body,
        "- IMPORTANT (blind rule): Each contestant's `feedback` is delivered privately. Do NOT reveal other contestants' ideas, scores, or identities inside `feedback`."
    );
    if is_last {
        let _ = writeln!(
            body,
            "- This is the FINAL round: you MUST set \"winner\" to the winning contestant id, provide \"final_verdict\", and set \"close\": true."
        );
    } else {
        let _ = writeln!(
            body,
            "- Leave \"winner\" and \"final_verdict\" null and \"close\": false unless the competition should end early."
        );
    }
    body.push('\n');

    let _ = writeln!(body, "## 5. Your JSON output");
    let _ = writeln!(
        body,
        "Write ONLY valid JSON (no prose around it) to: {}\n",
        reply_path.display()
    );
    let _ = writeln!(
        body,
        "{{\n  \"overview\": \"1-3 sentence round overview comparing progress\",\n  \"evaluations\": {{\n    \"<contestant_id>\": {{\n      \"score\": 85,\n      \"strengths\": \"Key strengths of this entry\",\n      \"feedback\": \"Concrete private feedback for next round (no peer spoilers)\"\n    }}\n  }},\n  \"ranking\": [\"<best_id>\", \"<second_id>\"],\n  \"winner\": null,\n  \"final_verdict\": null,\n  \"close\": false\n}}"
    );

    Brief {
        to: session.judge.id.clone(),
        base_rev: session.rev,
        body: body.trim_end().to_owned() + "\n",
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// Short message sent back to an agent when its reply file failed validation.
pub fn correction_note(to: &PartId, error: &str, reply_path: &Path) -> String {
    format!(
        "Your previous output ({to}) could not be accepted:\n{error}\n\nPlease fix these issues and rewrite: {}\n",
        reply_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, ArtifactFile, ArtifactKind, EntryEvaluation, JUDGE_ID, JudgeOutput, Participant,
        Phase, Role, Round, SessionId,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sample_session() -> Session {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
        };
        let mut s = Session {
            id: SessionId::new("comp-1"),
            request: "Design a landing page for a coffee roaster".into(),
            workdir: None,
            max_rounds: Some(10),
            reveal_interval: 5,
            rev: 10,
            created_at: 1,
            judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
            contestants: vec![
                p("claude", AgentKind::Claude, Role::Contestant),
                p("codex", AgentKind::Codex, Role::Contestant),
            ],
            messages: vec![],
            rounds: vec![],
            winner: None,
            closed: false,
        };
        for no in 1..=5 {
            let mut submissions = BTreeMap::new();
            submissions.insert(
                PartId::new("claude"),
                Submission {
                    part: PartId::new("claude"),
                    summary: format!("Claude R{no} minimalist warm design"),
                    body: format!("Claude secret strategy R{no}"),
                    artifacts: vec![ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 1200,
                    }],
                    at: no as u64 * 10,
                },
            );
            submissions.insert(
                PartId::new("codex"),
                Submission {
                    part: PartId::new("codex"),
                    summary: format!("Codex R{no} editorial typography"),
                    body: format!("Codex secret strategy R{no}"),
                    artifacts: vec![ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 1500,
                    }],
                    at: no as u64 * 10 + 1,
                },
            );
            let mut evaluations = BTreeMap::new();
            evaluations.insert(
                PartId::new("claude"),
                EntryEvaluation {
                    score: 70 + no as u8,
                    strengths: "Warm palette".into(),
                    feedback: "Tighten hero spacing".into(),
                },
            );
            evaluations.insert(
                PartId::new("codex"),
                EntryEvaluation {
                    score: 72 + no as u8,
                    strengths: "Crisp typography".into(),
                    feedback: "Add product cards".into(),
                },
            );
            s.rounds.push(Round {
                no,
                kind: RoundKind::Blind,
                phase: Phase::Closed,
                started_at: no as u64,
                briefs: vec![],
                submissions,
                judge_brief: None,
                outcome: Some(JudgeOutput {
                    overview: format!("R{no} overview: Codex leads slightly"),
                    evaluations,
                    ranking: vec![PartId::new("codex"), PartId::new("claude")],
                    winner: None,
                    final_verdict: None,
                    close: false,
                }),
            });
        }
        s
    }

    #[test]
    fn blind_round_brief_hides_peer_work() {
        let s = sample_session();
        let brief = contestant_brief(
            &s,
            &PartId::new("claude"),
            4,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/claude.md"),
        );
        assert!(brief.body.contains("BLIND"));
        assert!(!brief.body.contains("Codex secret strategy"));
        assert!(!brief.body.contains("Peer showcase"));
        assert!(brief.body.contains("Tighten hero spacing"));
        assert!(!brief.body.contains("Add product cards"));
    }

    #[test]
    fn reveal_round_brief_exposes_prior_round_peer_work() {
        let s = sample_session();
        let brief = contestant_brief(
            &s,
            &PartId::new("claude"),
            6,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r6/claude.md"),
        );
        assert!(brief.body.contains("PEER REVEAL"));
        assert!(brief.body.contains("Peer showcase (round 5"));
        assert!(brief.body.contains("Codex secret strategy R5"));
        assert!(brief.body.contains("R5 ranking: codex > claude"));
    }
}
