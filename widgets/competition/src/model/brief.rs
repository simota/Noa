//! Situation briefs for contestants and the judge, assembled from state alone.
//!
//! Information isolation is enforced here:
//! - In `RoundKind::Blind`, a contestant brief NEVER includes peer submissions,
//!   peer artifact directories, peer feedback, or peer identities. It does
//!   carry the contestant's own rank and its point gap to the leader (or lead
//!   over the runner-up), to keep the pressure on.
//! - In `RoundKind::Reveal` (every `reveal_interval` completed rounds), the
//!   brief adds a Peer Showcase section containing the prior round's peer
//!   submissions, artifact snapshot paths, and the judge's round overview.

use std::fmt::Write as _;
use std::path::Path;

use super::aggregate::PanelScores;
use super::types::{Brief, MsgKind, PartId, Receipt, RoundKind, RoundNo, Session, Submission};

/// Render one contestant's artifact list directly into `out`.
fn write_artifacts(out: &mut String, submission: &Submission, artifact_dir: &Path) {
    if submission.artifacts.is_empty() {
        out.push_str("  (no files in entry directory yet)\n");
        return;
    }
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
}

fn finish_body(mut body: String) -> String {
    let trimmed = body.trim_end().len();
    body.truncate(trimmed);
    body.push('\n');
    body
}

/// How deliverables reach the comparison viewer. Told to contestants and
/// repeated to the judge, so both work from the same rules.
const VIEWER_RULES: [&str; 3] = [
    "If your deliverable is a UI or visual design, put its entry page at the root of your output directory as `index.html` (or deliver SVG/PNG files) so the comparison viewer can show it.",
    "The comparison viewer opens a copy of your root `index.html` that the competition generates at each snapshot, with the stylesheets, scripts, images and fonts it references by relative path inlined. Reference your files by relative path; do not hand-build separate preview files.",
    "The viewer still frames that copy in an iframe with `sandbox=\"allow-scripts\"` (an opaque origin): popups, same-origin APIs and storage are blocked and clipboard access may fail, so the page must hold up under those limits.",
];

/// Where facts may come from. Told to contestants and repeated to the judge.
const SOURCE_RULE: &str = "Back every fact and number with files under version control in the reference workspace (`git ls-files`), unless the request names other sources. When sources disagree, the one the request calls canonical wins; say which source you used.";

/// Writes the rules both sides share directly into `out`.
fn write_shared_rules(out: &mut String) {
    for rule in VIEWER_RULES {
        out.push_str("- ");
        out.push_str(rule);
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "- What the viewer copy inlines: {}",
        crate::preview::INLINE_RULES
    );
    let _ = writeln!(
        out,
        "- Every submission is checked at snapshot: the viewer copy, screenshots and a manifest (files, sizes, hashes, references that could not be inlined) under `r<N>/checks/<contestant>/`. Screenshot conditions: {}",
        crate::shots::CONDITIONS
    );
    out.push_str("- ");
    out.push_str(SOURCE_RULE);
    out.push('\n');
}

#[cfg(test)]
fn shared_rules() -> Vec<String> {
    let mut rules: Vec<String> = VIEWER_RULES.iter().map(|r| (*r).to_owned()).collect();
    rules.push(format!(
        "What the viewer copy inlines: {}",
        crate::preview::INLINE_RULES
    ));
    rules.push(format!(
        "Every submission is checked at snapshot: the viewer copy, screenshots and a manifest (files, sizes, hashes, references that could not be inlined) under `r<N>/checks/<contestant>/`. Screenshot conditions: {}",
        crate::shots::CONDITIONS
    ));
    rules.push(SOURCE_RULE.to_owned());
    rules
}

/// Writes the command a contestant runs to check its entry before submitting.
fn write_check_command(out: &mut String, entry_dir: &Path) {
    match std::env::current_exe() {
        Ok(exe) => {
            let _ = write!(
                out,
                "\"{}\" --check=\"{}\"",
                exe.display(),
                entry_dir.display()
            );
        }
        Err(_) => {
            let _ = write!(out, "\"competition\" --check=\"{}\"", entry_dir.display());
        }
    }
}

/// Writes the judge's published checklist for `round` directly into `out`.
fn write_checklist_lines(out: &mut String, session: &Session, round: RoundNo) {
    match session.checklist_before(round) {
        Some((from, items)) => {
            let _ = writeln!(
                out,
                "The judge's checklist (published with the R{from} judgment):"
            );
            for item in items {
                out.push_str("- ");
                out.push_str(item);
                out.push('\n');
            }
        }
        None => out.push_str("The judge publishes its checklist with the round-1 judgment.\n"),
    }
}

/// Report and improvement rules every contestant gets, repeated to the judge.
const CONTESTANT_RULES: [&str; 5] = [
    "The judge's feedback is the floor, not the goal: points go to what raises the deliverable's impact for the request, not to applying the feedback alone.",
    "Every round, make at least one improvement nobody asked for and name it in a `## Beyond feedback` section of the report.",
    "In peer-reveal rounds, open at least one peer's deliverable and add a `## Peer review` section listing each idea adopted or not adopted, with the reason.",
    "Start the report with a one-line summary, describe what changed and why, and list the key files.",
    "Write the report in the language of the request and mark anything not verified as unverified.",
];

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
    let mode_label = match kind {
        RoundKind::Blind => "BLIND (isolated — peers' work is hidden)",
        RoundKind::Reveal => "PEER REVEAL (showcase of peers' work from the previous round)",
    };

    let mut body = String::with_capacity(4096);
    let _ = writeln!(body, "## 1. Competition request and round");
    let _ = writeln!(body, "Request:\n{}\n", session.request);
    let _ = writeln!(body, "Contestant: {to}");
    let _ = write!(body, "Round: {round}");
    if let Some(max) = session.max_rounds {
        let _ = write!(body, " of {max}");
    }
    let _ = writeln!(body, " — {mode_label}");
    if let Some(max) = session.max_rounds {
        let left = max.saturating_sub(round);
        if left == 0 {
            body.push_str(
                "Notice: This is the FINAL round. Deliver your strongest, polished work.\n",
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
        body.push_str(
            "  Do not write build outputs or temporary files there; use your output directory or $TMPDIR.\n",
        );
    }
    let _ = writeln!(
        body,
        "Your private output directory: {}",
        entry_dir.display()
    );
    body.push('\n');

    body.push_str("## 2. Competition rules\n");
    let _ = writeln!(
        body,
        "- Create or refine all your deliverables (designs, HTML/CSS/JS, SVG, images, code, docs) ONLY inside your private output directory: {}",
        entry_dir.display()
    );
    write_shared_rules(&mut body);
    body.push_str("- Before you submit, run the same check on your output directory: ");
    write_check_command(&mut body, entry_dir);
    body.push_str(". It prints the viewer copy, the screenshots and the manifest, and lists what could not be inlined. If it cannot run in your environment, say so in the report and mark what it would have shown as unverified.\n\n");
    write_checklist_lines(&mut body, session, round);
    match kind {
        RoundKind::Blind => {
            body.push_str(
                "- BLIND ROUND: Do NOT inspect, read, or access any other contestant's directory. Compete strictly on your own ideas and the judge's feedback to you.\n",
            );
        }
        RoundKind::Reveal => {
            let _ = writeln!(
                body,
                "- PEER REVEAL ROUND: You may inspect the other contestants' round-{} snapshots listed below to understand their approaches, strengths, and gaps. Do NOT copy their files verbatim; synthesize better ideas and surpass them in your own output directory.",
                round.saturating_sub(1)
            );
            body.push_str(
                "- Open at least one peer's deliverable (its `index.html` or images), not only the reports quoted below. In your report, add a `## Peer review` section with one line per idea you adopted and one per idea you chose not to adopt, each with the reason.\n",
            );
        }
    }
    body.push('\n');

    body.push_str("## 3. Your previous work and private judge feedback\n");
    if let Some((prev_round, prev_sub)) = session.latest_submission_before(to, round) {
        let _ = writeln!(
            body,
            "Your last submission (R{prev_round}): {}",
            prev_sub.summary
        );
        let prev_round_dir = reply_dir.join(format!("r{prev_round}"));
        let snap = prev_round_dir.join("artifacts").join(to.as_str());
        write_artifacts(&mut body, prev_sub, &snap);
        let checks = prev_round_dir.join("checks").join(to.as_str());
        let _ = writeln!(
            body,
            "  What the judge saw (viewer-copy screenshots and manifest): {}",
            checks.display()
        );
    } else {
        body.push_str("(this is your first round; start fresh)\n");
    }
    let evaluation = session.latest_evaluation_before(to, round);
    let panel_scores =
        evaluation.and_then(|(ev_round, _)| Some((ev_round, session.panel_scores(ev_round, to)?)));
    if let Some((ev_round, scores)) = panel_scores {
        write_panel_evaluation(&mut body, session, to, ev_round, &scores);
    } else if let Some((ev_round, ev)) = evaluation {
        let _ = writeln!(body, "\nJudge evaluation of your R{ev_round} entry:");
        let _ = writeln!(body, "- Score: {}/100", ev.score);
        for axis in &ev.breakdown {
            let _ = writeln!(body, "  - {}: {}/{}", axis.axis, axis.score, axis.max);
        }
        if !ev.strengths.is_empty() {
            let _ = writeln!(body, "- Strengths: {}", ev.strengths);
        }
        let _ = writeln!(body, "- Feedback to address: {}", ev.feedback);
        if let Some(standing) = standing_line(session, to, ev_round) {
            let _ = writeln!(body, "- {standing}");
        }
    }
    // Corrections the last judgment made to any of `to`'s scores, whether or
    // not `to` took part in that round.
    let last_judged = round.saturating_sub(1);
    for r in session.rounds.iter().filter(|r| r.no < round) {
        if !r.scorecards.is_empty() {
            write_panel_corrections(&mut body, session, to, r.no, last_judged);
            continue;
        }
        if let Some(ev) = r.outcome.as_ref().and_then(|o| o.evaluations.get(to))
            && let Some(note) = ev.amended.as_ref().filter(|n| n.by_round == last_judged)
        {
            let _ = writeln!(
                body,
                "- Score correction: your R{} score went from {}/100 to {}/100 ({}).",
                r.no, note.from, ev.score, note.reason
            );
        }
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
                let _ = write!(body, "R{prev} ranking: ");
                for (i, part) in outcome.ranking.iter().enumerate() {
                    if i > 0 {
                        body.push_str(" > ");
                    }
                    body.push_str(part.as_str());
                }
                body.push_str("\n\n");
            }
            let mut any_peer = false;
            let round_dir = reply_dir.join(format!("r{prev}"));
            let checks_root = round_dir.join("checks");
            let artifacts_root = round_dir.join("artifacts");
            for peer in session.contestant_ids().filter(|p| *p != to) {
                if let Some(sub) = prev_round.submissions.get(peer) {
                    any_peer = true;
                    let _ = write!(body, "### Peer: {peer} (R{prev}");
                    if let Some(ev) = prev_round
                        .outcome
                        .as_ref()
                        .and_then(|o| o.evaluations.get(peer))
                    {
                        match session.panel_scores(prev, peer) {
                            Some(scores) => {
                                let _ = write!(body, " — score {}/100", scores.mean_label());
                            }
                            None => {
                                let _ = write!(body, " — score {}/100", ev.score);
                            }
                        }
                    }
                    body.push_str(")\n");
                    let _ = writeln!(
                        body,
                        "Screenshots and manifest (start here): {}",
                        checks_root.join(peer.as_str()).display()
                    );
                    let _ = writeln!(body, "Summary: {}", sub.summary);
                    write_artifacts(&mut body, sub, &artifacts_root.join(peer.as_str()));
                    let _ = writeln!(
                        body,
                        "Full report: {}\n",
                        crate::store::reply_path(reply_dir, prev, peer).display()
                    );
                }
            }
            if !any_peer {
                let _ = writeln!(body, "(no peer submissions were recorded in R{prev})\n");
            }
        } else {
            body.push_str("(previous round not found)\n\n");
        }
    }

    body.push_str("## 5. User instructions / interventions\n");
    let mut any_intervention = false;
    for msg in session.messages.iter().filter(|m| {
        m.kind == MsgKind::Intervention
            && m.round <= round
            && (m.to.is_empty() || m.to.contains(to))
    }) {
        any_intervention = true;
        let _ = writeln!(body, "- [{} R{}] {}", msg.id, msg.round, msg.body.trim());
    }
    if !any_intervention {
        body.push_str("(none)\n");
    }
    body.push('\n');

    let is_last = session.max_rounds.is_some_and(|max| round >= max);
    body.push_str("## 6. How to compete\n");
    body.push_str(
        "- The judge's feedback is the floor, not the goal: points go to what raises the deliverable's impact for the request, not to applying the feedback alone.\n",
    );
    if is_last {
        body.push_str(
            "- FINAL ROUND: finish rather than experiment. Close every open feedback item, verify everything you can, and leave no regression; a new idea is welcome only if you can verify it fully this round.\n",
        );
        body.push_str(
            "- In your report, add a `## Beyond feedback` section naming the finishing work nobody asked for and why it raises the final result.\n",
        );
    } else {
        body.push_str(
            "- Every round, also make at least one improvement nobody asked for: the change you expect to raise your score most. Prefer one bold, verified change over many small fixes.\n",
        );
        body.push_str(
            "- In your report, add a `## Beyond feedback` section naming that change and why you expect it to win points.\n",
        );
    }
    body.push('\n');

    body.push_str("## 7. How to submit\n");
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
    body.push_str(
        "   Start the report with a one-line summary (e.g. `# Summary: ...`), then describe what you built or improved this round, design/technical choices, and list key files in your output directory.\n\
         \x20  Write the file only after all deliverable files are saved.\n\
         \x20  Write the report in the language of the request, and mark anything you could not verify in your environment as unverified.\n",
    );

    Brief {
        to: to.clone(),
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// `to`'s rank after round `round`, and the gap to the leader or the lead over
/// the runner-up. Peers stay anonymous: only positions and point gaps.
fn standing_line(session: &Session, to: &PartId, round: RoundNo) -> Option<String> {
    let outcome = session.round(round)?.outcome.as_ref()?;
    let pos = outcome.ranking.iter().position(|p| p == to)?;
    let score_at = |i: usize| {
        outcome
            .ranking
            .get(i)
            .and_then(|p| outcome.evaluations.get(p))
            .map(|e| i32::from(e.score))
    };
    let mine = score_at(pos)?;
    let n = outcome.ranking.len();
    let gap = if pos == 0 {
        match score_at(1) {
            Some(next) => format!("you lead by {} point(s)", mine - next),
            None => "you lead".to_owned(),
        }
    } else {
        format!("{} point(s) behind the leader", score_at(0)? - mine)
    };
    Some(format!(
        "Standing after R{round}: rank {} of {n}, {gap}.",
        pos + 1
    ))
}

/// `to`'s evaluation of panel round `no`: the mean and each judge's score,
/// then each judge's own strengths, feedback and breakdown, labelled by
/// judge, and the standing on means. Only `to`'s own: the blind rule
/// between contestants is the single judge's.
fn write_panel_evaluation(
    body: &mut String,
    session: &Session,
    to: &PartId,
    no: RoundNo,
    scores: &PanelScores,
) {
    let _ = writeln!(
        body,
        "\nJudges' evaluation of your R{no} entry (mean of {} judges):",
        scores.scores.len()
    );
    let _ = write!(body, "- Score: {}/100 (", scores.mean_label());
    for (i, (judge, score)) in scores.scores.iter().enumerate() {
        if i > 0 {
            body.push_str(", ");
        }
        let _ = write!(body, "{judge} {score}");
    }
    body.push_str(")\n");
    if let Some(round) = session.round(no) {
        for judge in session.judges() {
            let Some(ev) = round
                .scorecards
                .get(&judge.id)
                .and_then(|card| card.evaluations.get(to))
            else {
                continue;
            };
            let _ = write!(body, "- {}: ", judge.id);
            if !ev.strengths.is_empty() {
                let _ = write!(body, "Strengths: {} / ", ev.strengths);
            }
            let _ = writeln!(body, "Feedback to address: {}", ev.feedback);
            for axis in &ev.breakdown {
                let _ = writeln!(body, "  - {}: {}/{}", axis.axis, axis.score, axis.max);
            }
        }
    }
    if let Some(standing) = panel_standing_line(session, to, no) {
        let _ = writeln!(body, "- {standing}");
    }
}

/// [`standing_line`] on means, to one decimal: the combined ranking of
/// panel round `round`, and the gap of exact means.
fn panel_standing_line(session: &Session, to: &PartId, round: RoundNo) -> Option<String> {
    let outcome = session.round(round)?.outcome.as_ref()?;
    let pos = outcome.ranking.iter().position(|p| p == to)?;
    let scores_at = |i: usize| {
        outcome
            .ranking
            .get(i)
            .and_then(|p| session.panel_scores(round, p))
    };
    let mine = scores_at(pos)?;
    let judges = mine.scores.len().max(1) as u32;
    // Every judge scores every entry, so the gap of means is the gap of
    // sums over the judge count; rounded half up to one decimal.
    let gap = |high: u32, low: u32| {
        let x10 = (20 * high.saturating_sub(low) + judges) / (2 * judges);
        format!("{}.{}", x10 / 10, x10 % 10)
    };
    let n = outcome.ranking.len();
    let gap = if pos == 0 {
        match scores_at(1) {
            Some(next) => format!("you lead by {} point(s)", gap(mine.sum(), next.sum())),
            None => "you lead".to_owned(),
        }
    } else {
        format!(
            "{} point(s) behind the leader",
            gap(scores_at(0)?.sum(), mine.sum())
        )
    };
    Some(format!(
        "Standing after R{round}: rank {} of {n}, {gap}.",
        pos + 1
    ))
}

/// The corrections the judgment of round `by_round` made to `to`'s scores
/// in panel round `no`: one line per judge that amended its own card,
/// with the round's mean after the correction.
fn write_panel_corrections(
    body: &mut String,
    session: &Session,
    to: &PartId,
    no: RoundNo,
    by_round: RoundNo,
) {
    let Some(round) = session.round(no) else {
        return;
    };
    for judge in session.judges() {
        let Some(ev) = round
            .scorecards
            .get(&judge.id)
            .and_then(|card| card.evaluations.get(to))
        else {
            continue;
        };
        let Some(note) = ev.amended.as_ref().filter(|n| n.by_round == by_round) else {
            continue;
        };
        let _ = write!(
            body,
            "- Score correction: {} changed your R{no} score from {} to {} ({})",
            judge.id, note.from, ev.score, note.reason
        );
        if let Some(scores) = session.panel_scores(no, to) {
            let _ = write!(body, "; R{no} mean is now {}", scores.mean_label());
        }
        body.push_str(".\n");
    }
}

/// Build the brief for the judge to evaluate all submissions in `round`:
/// the single judge's, or the lead's scorecard brief in a panel.
pub fn judge_brief(
    session: &Session,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
    scorecard_brief(session, round, &session.judge.id, reply_dir, reply_path)
}

/// Build `judge`'s brief to score every submission in `round`. A single
/// judge's is today's brief, byte for byte. In a panel each judge gets its
/// own, naming the panel and its seat, its own reply path only, and
/// nothing another judge wrote: no score, overview, feedback or file.
pub fn scorecard_brief(
    session: &Session,
    round: RoundNo,
    judge: &PartId,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
    let panel = session.is_panel();
    let lead = &session.judge.id;
    let co_judge = panel && judge != lead;
    let is_last = session.max_rounds.is_some_and(|max| round >= max);
    let this_round = session.round(round);
    let sub_bytes: usize = this_round.map_or(0, |r| {
        r.submissions
            .values()
            .map(|s| s.body.len() + s.summary.len() + s.artifacts.len() * 64 + 512)
            .sum()
    });

    let mut body = String::with_capacity(sub_bytes + 4096);
    let _ = write!(body, "## 1. Competition judging — Round {round}");
    if let Some(max) = session.max_rounds {
        let _ = write!(body, " of {max}");
    }
    body.push('\n');
    let _ = writeln!(body, "Request:\n{}\n", session.request);
    body.push_str("Contestants: ");
    for (i, part) in session.contestant_ids().enumerate() {
        if i > 0 {
            body.push_str(", ");
        }
        body.push_str(part.as_str());
    }
    body.push('\n');
    if panel {
        body.push_str("Judging panel: ");
        for (i, seat) in session.judges().enumerate() {
            if i > 0 {
                body.push_str(", ");
            }
            body.push_str(seat.id.as_str());
            if seat.id == *lead {
                body.push_str(" (lead)");
            }
        }
        let _ = writeln!(body, ". You are {judge}.");
        body.push_str("An entry's round score is the mean of every judge's score.\n");
    }
    let _ = writeln!(
        body,
        "Peer-reveal schedule: every {} rounds (0 = disabled)",
        session.reveal_interval
    );
    body.push('\n');

    let _ = writeln!(body, "## 2. Round {round} submissions and artifacts");
    if let Some(r) = this_round {
        let round_dir = reply_dir.join(format!("r{round}"));
        let artifacts_root = round_dir.join("artifacts");
        let checks_root = round_dir.join("checks");
        for part in session.contestant_ids() {
            let _ = writeln!(body, "### Contestant: {part}");
            if let Some(sub) = r.submissions.get(part) {
                let _ = writeln!(body, "Summary: {}", sub.summary);
                let snap = artifacts_root.join(part.as_str());
                write_artifacts(&mut body, sub, &snap);
                if sub.artifacts.iter().any(|f| f.rel_path == "index.html") {
                    let shots = checks_root.join(part.as_str());
                    let _ = write!(
                        body,
                        "  Viewer copy: {}\n  Screenshots of the viewer copy (",
                        snap.join(".preview").join("index.html").display()
                    );
                    for (i, &v) in crate::shots::VIEWPORTS.iter().enumerate() {
                        if i > 0 {
                            body.push_str(", ");
                        }
                        body.push_str(&crate::shots::file_name(v));
                    }
                    let _ = writeln!(body, ") and manifest.md: {}", shots.display());
                }
                let _ = writeln!(body, "Report:\n{}\n", sub.body.trim());
            } else {
                let _ = writeln!(body, "(did not submit in R{round})\n");
            }
        }
    }

    body.push_str("## 3. User interventions\n");
    let mut any_intervention = false;
    for msg in session
        .messages
        .iter()
        .filter(|m| m.kind == MsgKind::Intervention && m.round <= round)
    {
        any_intervention = true;
        let _ = write!(body, "- [{} R{} to ", msg.id, msg.round);
        if msg.to.is_empty() {
            body.push_str("everyone");
        } else {
            for (i, part) in msg.to.iter().enumerate() {
                if i > 0 {
                    body.push_str(", ");
                }
                body.push_str(part.as_str());
            }
        }
        let _ = writeln!(body, "] {}", msg.body.trim());
    }
    if !any_intervention {
        body.push_str("(none)\n");
    }
    body.push('\n');

    body.push_str("## 4. Rules the contestants were given\n");
    write_shared_rules(&mut body);
    for rule in CONTESTANT_RULES {
        body.push_str("- ");
        body.push_str(rule);
        body.push('\n');
    }
    body.push_str(
        "- Contestants can run the same snapshot check on their entry before submitting (`--check`).\n",
    );
    write_checklist_lines(&mut body, session, round);
    body.push('\n');

    body.push_str("## 5. Judging rules\n");
    body.push_str(
        "- Judge visual entries from the viewer copy and its screenshots listed above: they show what the comparison viewer shows. If the screenshots are missing, capture the viewer copy yourself under the conditions in section 4. Do not penalize what the rules in section 4 allow. Results in a contestant's check output or in `r<N>/checks/` count as measured.\n",
    );
    if panel {
        let _ = writeln!(
            body,
            "- Score independently: do not open other judges' reply files in r{round}/ (judge.json, judge-2.json, …, verdict.json), and do not read from or type into any other pane."
        );
    }
    if co_judge {
        if round <= 1 {
            body.push_str(
                "- The lead judge publishes the checklist with this round's judgment; leave \"checklist\" empty.\n",
            );
        } else {
            body.push_str(
                "- Apply the published checklist in section 4. Leave \"checklist\" empty: only the lead judge changes it.\n",
            );
        }
    } else if round <= 1 {
        body.push_str(
            "- Publish your checklist in `checklist`: the concrete checks you will apply for the rest of the competition (viewports, what counts as the first view, thresholds such as minimum text size, accessibility bar). Contestants see it from round 2.\n",
        );
    } else {
        body.push_str(
            "- Apply the published checklist in section 4. Do not score a check that is not on it. To add or change a check, send the full new list in `checklist` and name the change in `overview`; it applies from the next round.\n",
        );
    }
    body.push_str(
        "- When the request weights its criteria, fill each evaluation's `breakdown` with one entry per criterion (its points and maximum); the points must add up to `score`. The contestant sees its own breakdown.\n\
         - Inspect the actual artifact files in each contestant's snapshot directory above, not just their self-report.\n\
         - Score every contestant on 0..100 based on how well their deliverable satisfies the user's request (quality, completeness, aesthetics/craft, usability).\n\
         - IMPORTANT (blind rule): Each contestant's `strengths` and `feedback` are delivered privately to that contestant. Do NOT reveal other contestants' ideas, scores, or identities inside them.\n\
         - Derive each contestant's `feedback` only from gaps between their own deliverable and the user's request. Do NOT suggest a feature, approach, layout, wording, or fix because you saw it in another contestant's work (this round or earlier), even unnamed or paraphrased; ideas are shared only through peer-reveal rounds. If a gap is evident only by comparison with a peer, leave it out.\n\
         - Score against an absolute bar for the request, not against the previous round: an entry that only applies last round's feedback without raising its impact should not gain points. Reward bold, verified improvements the contestant initiated (see its `## Beyond feedback` section).\n\
         - Write `feedback` as at most 3 items, highest score impact first, each with a check the contestant can run (e.g. \"at 320×568, the install button and two queue items are visible\").\n\
         - Do not ask for checks the contestants' environment cannot run (e.g. a physical phone when only a headless browser is available); ask them to state the item as unverified instead.\n\
         - Compare each report's claims with the files, and with the claims of that contestant's previous report; name any claim the files contradict in that contestant's `feedback`.\n\
         - List in each evaluation's `evidence` the concrete claims your score rests on, each with `basis`: \"measured\" (you checked it in the files or screenshots), \"self_reported\" (taken from the report without re-checking), or \"unverified\". Never present a self-reported number as measured.\n",
    );
    if panel {
        body.push_str(
            "- If you find that one of your own earlier scores rested on a wrong basis (a misread rule, the wrong rendering), correct it in `amendments` with the round, contestant, corrected score and reason, instead of only mentioning it in prose. Amend only your own earlier scores; each judge corrects its own.\n",
        );
    } else {
        body.push_str(
            "- If you find that an earlier round's score rested on a wrong basis (a misread rule, the wrong rendering), correct it in `amendments` with the round, contestant, corrected score and reason, instead of only mentioning it in prose.\n",
        );
    }
    if co_judge {
        body.push_str(
            "- Leave \"winner\" and \"final_verdict\" null and \"close\": false: the lead judge decides early close and the verdict; you only score.\n",
        );
    } else if panel && is_last {
        body.push_str(
            "- This is the FINAL round: score as usual and set \"close\": true. Leave \"winner\" and \"final_verdict\" null: once every judge has scored you will get the combined scores and be asked for the winner and the final verdict.\n",
        );
    } else if panel {
        body.push_str(
            "- Leave \"winner\" and \"final_verdict\" null. Set \"close\": true only if the competition should end early; you will then be asked for the verdict once every judge has scored.\n",
        );
    } else if is_last {
        body.push_str(
            "- This is the FINAL round: you MUST set \"winner\" to the winning contestant id, provide \"final_verdict\", and set \"close\": true.\n",
        );
    } else {
        body.push_str(
            "- Leave \"winner\" and \"final_verdict\" null and \"close\": false unless the competition should end early.\n",
        );
    }
    body.push('\n');

    body.push_str("## 6. Your JSON output\n");
    let _ = writeln!(
        body,
        "Write ONLY valid JSON (no prose around it) to: {}\n",
        reply_path.display()
    );
    let template = "{\n  \"overview\": \"1-3 sentence round overview comparing progress\",\n  \"evaluations\": {\n    \"<contestant_id>\": {\n      \"score\": 85,\n      \"strengths\": \"Key strengths of this entry\",\n      \"feedback\": \"Concrete private feedback for next round, based only on this entry vs the request (no peer ideas)\",\n      \"breakdown\": [\n        { \"axis\": \"<criterion>\", \"score\": 50, \"max\": 60 },\n        { \"axis\": \"<criterion>\", \"score\": 35, \"max\": 40 }\n      ],\n      \"evidence\": [\n        { \"claim\": \"index.html is 19,018 bytes\", \"basis\": \"measured\" },\n        { \"claim\": \"0 axe violations\", \"basis\": \"self_reported\" }\n      ]\n    }\n  },\n  \"ranking\": [\"<best_id>\", \"<second_id>\"],\n  \"checklist\": [\"<check>\"],\n  \"amendments\": [],\n  \"winner\": null,\n  \"final_verdict\": null,\n  \"close\": false\n}\n\nAn amendment looks like: { \"round\": 2, \"contestant\": \"<id>\", \"score\": 80, \"reason\": \"why the R2 score was wrong\" }\n";
    if co_judge {
        body.push_str(&template.replace("\"checklist\": [\"<check>\"]", "\"checklist\": []"));
    } else {
        body.push_str(template);
    }

    Brief {
        to: judge.clone(),
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// Build the panel lead's verdict brief for deciding round `round`, once
/// every judge's scorecard is in and combined: the combined scores (each
/// entry's mean, every judge's score and the spread, in combined order),
/// the entries with the highest mean, every judge's overview and the
/// user's interventions. Self-contained, so a lead whose pane restarted
/// since its scorecard can still write the verdict.
pub fn verdict_brief(session: &Session, round: RoundNo, reply_path: &Path) -> Brief {
    let this_round = session.round(round);
    let outcome = this_round.and_then(|r| r.outcome.as_ref());
    let judges = session.judges().count();
    let is_last = session.max_rounds.is_some_and(|max| round >= max);

    let mut body = String::with_capacity(4096);
    let _ = write!(body, "## 1. Final verdict — Round {round}");
    if let Some(max) = session.max_rounds {
        let _ = write!(body, " of {max}");
    }
    body.push('\n');
    let _ = writeln!(body, "Request:\n{}\n", session.request);
    let _ = writeln!(
        body,
        "Every judge ({judges}) has scored round {round}, and the scores are combined below."
    );
    if is_last {
        body.push_str("This is the FINAL round: the competition ends with your verdict.\n");
    } else {
        body.push_str("You asked to end the competition early: it ends with your verdict.\n");
    }
    body.push_str(
        "As the lead judge, name the winner and write the final verdict. The scores stand as they are: this reply changes none of them.\n\n",
    );

    let _ = writeln!(body, "## 2. Combined scores (mean of {judges} judges)");
    let top = super::aggregate::top_set(session, round);
    if let Some(outcome) = outcome {
        for (i, part) in outcome.ranking.iter().enumerate() {
            let Some(scores) = session.panel_scores(round, part) else {
                continue;
            };
            let _ = write!(body, "{}. {part} — mean {} (", i + 1, scores.mean_label());
            for (j, (judge, score)) in scores.scores.iter().enumerate() {
                if j > 0 {
                    body.push_str(", ");
                }
                let _ = write!(body, "{judge} {score}");
            }
            let _ = writeln!(body, "; spread {})", scores.spread());
        }
    }
    let top_list = top
        .iter()
        .map(PartId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if top.len() > 1 {
        let _ = writeln!(body, "Tied for the highest mean: {top_list}");
    } else {
        let _ = writeln!(body, "Highest mean: {top_list}");
    }
    body.push('\n');

    body.push_str("## 3. Each judge's overview\n");
    if let Some(r) = this_round {
        for judge in session.judges() {
            let Some(card) = r.scorecards.get(&judge.id) else {
                continue;
            };
            let _ = write!(body, "### {}", judge.id);
            if judge.id == session.judge.id {
                body.push_str(" (lead, you)");
            }
            let _ = writeln!(body, "\n{}\n", card.overview.trim());
        }
    }

    body.push_str("## 4. User interventions\n");
    let mut any_intervention = false;
    for msg in session
        .messages
        .iter()
        .filter(|m| m.kind == MsgKind::Intervention && m.round <= round)
    {
        any_intervention = true;
        let _ = write!(body, "- [{} R{} to ", msg.id, msg.round);
        if msg.to.is_empty() {
            body.push_str("everyone");
        } else {
            for (i, part) in msg.to.iter().enumerate() {
                if i > 0 {
                    body.push_str(", ");
                }
                body.push_str(part.as_str());
            }
        }
        let _ = writeln!(body, "] {}", msg.body.trim());
    }
    if !any_intervention {
        body.push_str("(none)\n");
    }
    body.push('\n');

    body.push_str("## 5. Your JSON output\n");
    let _ = writeln!(
        body,
        "Write ONLY valid JSON (no prose around it) to: {}\n",
        reply_path.display()
    );
    let _ = writeln!(
        body,
        "{{\"winner\": \"<one of: {top_list}>\", \"final_verdict\": \"Why the winner won, citing the judges' scores and where they disagreed\"}}\n"
    );
    body.push_str(
        "- \"winner\" must be an entry with the highest mean; when several tie, you choose among them. You cannot name another entry.\n\
         - \"final_verdict\" must not be empty.\n",
    );

    Brief {
        to: session.judge.id.clone(),
        base_rev: session.rev,
        body: finish_body(body),
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
        AgentKind, AmendNote, ArtifactFile, ArtifactKind, AxisScore, EntryEvaluation, JUDGE_ID,
        JudgeOutput, Message, Participant, Phase, Role, Round, SessionId,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sample_session() -> Session {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
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
            co_judges: Vec::new(),
            contestants: vec![
                p("claude", AgentKind::Claude, Role::Contestant),
                p("codex", AgentKind::Codex, Role::Contestant),
            ],
            messages: vec![],
            rounds: vec![],
            winner: None,
            closed: false,
            hold: false,
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
                    evidence: Vec::new(),
                    amended: None,
                    breakdown: Vec::new(),
                },
            );
            evaluations.insert(
                PartId::new("codex"),
                EntryEvaluation {
                    score: 72 + no as u8,
                    strengths: "Crisp typography".into(),
                    feedback: "Add product cards".into(),
                    evidence: Vec::new(),
                    amended: None,
                    breakdown: Vec::new(),
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
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
                outcome: Some(JudgeOutput {
                    overview: format!("R{no} overview: Codex leads slightly"),
                    evaluations,
                    ranking: vec![PartId::new("codex"), PartId::new("claude")],
                    winner: None,
                    final_verdict: None,
                    close: false,
                    amendments: Vec::new(),
                    checklist: Vec::new(),
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
        assert!(
            brief
                .body
                .contains("Standing after R3: rank 2 of 2, 2 point(s) behind the leader.")
        );
        assert!(brief.body.contains("## Beyond feedback"));
        let leader = contestant_brief(
            &s,
            &PartId::new("codex"),
            4,
            &PathBuf::from("/tmp/entries/codex"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/report-codex.md"),
        );
        assert!(leader.body.contains("rank 1 of 2, you lead by 2 point(s)"));
    }

    #[test]
    fn judge_brief_forbids_peer_ideas_in_feedback() {
        let s = sample_session();
        let brief = judge_brief(
            &s,
            4,
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/judge.json"),
        );
        assert!(
            brief
                .body
                .contains("`strengths` and `feedback` are delivered privately")
        );
        assert!(
            brief
                .body
                .contains("because you saw it in another contestant's work")
        );
        assert!(
            brief
                .body
                .contains("based only on this entry vs the request")
        );
        assert!(brief.body.contains("at most 3 items"));
        assert!(brief.body.contains("absolute bar for the request"));
        assert!(brief.body.contains("environment cannot run"));
        assert!(
            brief
                .body
                .contains("claims of that contestant's previous report")
        );
    }

    #[test]
    fn judge_brief_repeats_contestant_rules_and_points_at_viewer_screenshots() {
        let s = sample_session();
        let brief = judge_brief(
            &s,
            4,
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/judge.json"),
        );
        assert!(
            brief
                .body
                .contains("## 4. Rules the contestants were given")
        );
        for rule in shared_rules()
            .iter()
            .map(String::as_str)
            .chain(CONTESTANT_RULES)
        {
            assert!(brief.body.contains(rule), "missing rule: {rule}");
        }
        assert!(
            brief
                .body
                .contains("Viewer copy: /tmp/comp/r4/artifacts/claude/.preview/index.html")
        );
        assert!(brief.body.contains(
            "1440x900.png, 768x1024.png, 320x568.png) and manifest.md: /tmp/comp/r4/checks/claude"
        ));
        assert!(brief.body.contains("\"basis\": \"self_reported\""));
        assert!(brief.body.contains("`amendments`"));
    }

    #[test]
    fn contestant_brief_carries_checklist_breakdown_correction_and_check_command() {
        let mut s = sample_session();
        let claude = PartId::new("claude");
        let first = s.round_mut(1).unwrap().outcome.as_mut().unwrap();
        first.checklist = vec!["320x568: no horizontal scroll".into()];
        first.evaluations.get_mut(&claude).unwrap().amended = Some(AmendNote {
            from: 60,
            by_round: 3,
            reason: "judged without its stylesheet".into(),
        });
        let third = s.round_mut(3).unwrap().outcome.as_mut().unwrap();
        third.evaluations.get_mut(&claude).unwrap().breakdown = vec![AxisScore {
            axis: "accuracy".into(),
            score: 20,
            max: 25,
        }];
        let brief = contestant_brief(
            &s,
            &claude,
            4,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/report-claude.md"),
        );
        let body = &brief.body;
        assert!(
            body.contains(
                "checklist (published with the R1 judgment):\n- 320x568: no horizontal scroll"
            ),
            "{body}"
        );
        assert!(body.contains("  - accuracy: 20/25"), "{body}");
        assert!(
            body.contains(
                "your R1 score went from 60/100 to 71/100 (judged without its stylesheet)"
            ),
            "{body}"
        );
        assert!(body.contains("--check=\"/tmp/entries/claude\""), "{body}");
        assert!(body.contains(
            "What the judge saw (viewer-copy screenshots and manifest): /tmp/comp/r3/checks/claude"
        ));
        assert!(!body.contains("FINAL ROUND"));

        s.max_rounds = Some(4);
        let last = contestant_brief(
            &s,
            &claude,
            4,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/report-claude.md"),
        );
        assert!(
            last.body
                .contains("FINAL ROUND: finish rather than experiment")
        );
        assert!(!last.body.contains("Prefer one bold, verified change"));
    }

    #[test]
    fn a_correction_reaches_a_contestant_that_skipped_the_amending_round() {
        let mut s = sample_session();
        let claude = PartId::new("claude");
        s.round_mut(1)
            .unwrap()
            .outcome
            .as_mut()
            .unwrap()
            .evaluations
            .get_mut(&claude)
            .unwrap()
            .amended = Some(AmendNote {
            from: 60,
            by_round: 3,
            reason: "judged without its stylesheet".into(),
        });
        // claude did not take part in R3.
        let third = s.round_mut(3).unwrap();
        third.submissions.remove(&claude);
        third.outcome.as_mut().unwrap().evaluations.remove(&claude);
        let brief = contestant_brief(
            &s,
            &claude,
            4,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/report-claude.md"),
        );
        assert!(
            brief
                .body
                .contains("your R1 score went from 60/100 to 71/100"),
            "{}",
            brief.body
        );
    }

    #[test]
    fn judge_publishes_its_checklist_in_round_one_and_keeps_it_after() {
        let s = sample_session();
        let first = judge_brief(
            &s,
            1,
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r1/judge.json"),
        );
        assert!(first.body.contains("Publish your checklist in `checklist`"));
        let later = judge_brief(
            &s,
            4,
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/judge.json"),
        );
        assert!(
            later
                .body
                .contains("Do not score a check that is not on it")
        );
        assert!(later.body.contains("`breakdown`"));
    }

    #[test]
    fn contestant_briefs_carry_viewer_language_and_peer_review_rules() {
        let s = sample_session();
        let blind = contestant_brief(
            &s,
            &PartId::new("claude"),
            4,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r4/report-claude.md"),
        );
        assert!(blind.body.contains(r#"sandbox="allow-scripts""#));
        assert!(blind.body.contains("language of the request"));
        assert!(!blind.body.contains("## Peer review"));
        let reveal = contestant_brief(
            &s,
            &PartId::new("claude"),
            6,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r6/report-claude.md"),
        );
        assert!(reveal.body.contains("## Peer review"));
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
        // Peers' screenshots come first; the full report is a file to open.
        let shots = brief
            .body
            .find("Screenshots and manifest (start here): /tmp/comp/r5/checks/codex")
            .unwrap();
        let report = brief
            .body
            .find("Full report: /tmp/comp/r5/report-codex.md")
            .unwrap();
        assert!(shots < report);
        assert!(!brief.body.contains("Codex secret strategy R5"));
        assert!(brief.body.contains("R5 ranking: codex > claude"));
    }

    #[test]
    fn brief_formatting_preserves_trailing_newline_and_all_sections() {
        let mut s = sample_session();
        s.messages.push(Message {
            id: crate::model::types::MessageId::nth(1),
            round: 4,
            author: crate::model::types::Author::User,
            kind: MsgKind::Intervention,
            to: vec![PartId::new("claude"), PartId::new("codex")],
            body: "Keep contrast high.".into(),
            at: 100,
        });
        let cb = contestant_brief(
            &s,
            &PartId::new("claude"),
            6,
            &PathBuf::from("/tmp/entries/claude"),
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r6/claude.md"),
        );
        assert!(cb.body.ends_with('\n'));
        assert!(!cb.body.ends_with("\n\n"));
        assert!(cb.body.contains("## 1. Competition request and round"));
        assert!(cb.body.contains("## 4. Peer showcase (round 5"));
        assert!(cb.body.contains("- [m1 R4] Keep contrast high."));
        assert!(cb.body.contains("  - index.html (HTML, 1200 bytes)"));

        let jb = judge_brief(
            &s,
            5,
            &PathBuf::from("/tmp/comp"),
            &PathBuf::from("/tmp/comp/r5/judge.json"),
        );
        assert!(jb.body.ends_with('\n'));
        assert!(!jb.body.ends_with("\n\n"));
        assert!(jb.body.contains("Contestants: claude, codex"));
        assert!(
            jb.body
                .contains("- [m1 R4 to claude, codex] Keep contrast high.")
        );
        assert!(
            jb.body
                .contains("Viewer copy: /tmp/comp/r5/artifacts/claude/.preview/index.html")
        );
        assert!(jb.body.contains("Report:\nClaude secret strategy R5\n"));
    }
    // ---- A judge panel (agy leads; claude is judge-2, codex judge-3). ----

    use crate::model::aggregate::tests::{cards, judge_next, panel_session, settle};
    use crate::model::types::{Amendment, judge_id};

    const DIR: &str = "/tmp/comp";

    fn scorecard_of(session: &Session, round: RoundNo, seat: usize) -> Brief {
        let judge = judge_id(seat);
        let path = crate::store::judge_reply_path(Path::new(DIR), round, &judge);
        scorecard_brief(session, round, &judge, Path::new(DIR), &path)
    }

    #[test]
    fn co_judge_briefs_say_they_only_score() {
        let mut s = panel_session(Some(3));
        let lead = scorecard_of(&s, 1, 0).body;
        let second = scorecard_of(&s, 1, 1).body;
        for (body, me) in [(&lead, "judge"), (&second, "judge-2")] {
            assert!(body.contains(&format!(
                "Judging panel: judge (lead), judge-2, judge-3. You are {me}.\nAn entry's round score is the mean of every judge's score.\n"
            )));
            assert!(body.contains(
                "- Score independently: do not open other judges' reply files in r1/ (judge.json, judge-2.json, …, verdict.json), and do not read from or type into any other pane."
            ));
            assert!(body.contains("Amend only your own earlier scores"));
            assert!(!body.contains("an earlier round's score rested"));
        }
        // The lead publishes the checklist and may ask to close.
        assert!(lead.contains("Publish your checklist in `checklist`"));
        assert!(lead.contains(r#""checklist": ["<check>"]"#));
        assert!(lead.contains(r#"Set "close": true only if the competition should end early"#));
        assert!(!lead.contains("you only score"));
        // A co-judge only scores.
        assert!(second.contains(
            r#"- The lead judge publishes the checklist with this round's judgment; leave "checklist" empty."#
        ));
        assert!(second.contains(r#""checklist": []"#));
        assert!(!second.contains("Publish your checklist"));
        assert!(second.contains(
            r#"- Leave "winner" and "final_verdict" null and "close": false: the lead judge decides early close and the verdict; you only score."#
        ));
        assert!(!second.contains("FINAL"));
        assert!(!second.contains(r#"Set "close": true"#));

        settle(
            &mut s,
            1,
            &cards(
                &[("codex", 90), ("claude", 70)],
                &[("codex", 85), ("claude", 75)],
                &[("codex", 80), ("claude", 79)],
            ),
        )
        .unwrap();
        judge_next(&mut s, 2);
        judge_next(&mut s, 3);
        let third = scorecard_of(&s, 3, 2).body;
        assert!(third.contains(
            r#"- Apply the published checklist in section 4. Leave "checklist" empty: only the lead judge changes it."#
        ));
        assert!(third.contains("you only score"));
        assert!(!third.contains("FINAL"));
        let last = scorecard_of(&s, 3, 0).body;
        assert!(last.contains(
            r#"- This is the FINAL round: score as usual and set "close": true. Leave "winner" and "final_verdict" null: once every judge has scored you will get the combined scores and be asked for the winner and the final verdict."#
        ));
        assert!(!last.contains("you MUST set \"winner\""));
    }

    #[test]
    fn a_judge_brief_holds_nothing_from_another_judge() {
        let mut s = panel_session(Some(3));
        let mut r1 = cards(
            &[("codex", 90), ("claude", 70)],
            &[("codex", 85), ("claude", 75)],
            &[("codex", 80), ("claude", 79)],
        );
        r1.get_mut(&judge_id(0)).unwrap().checklist = vec!["320x568: no horizontal scroll".into()];
        settle(&mut s, 1, &r1).unwrap();
        judge_next(&mut s, 2);
        let paths: Vec<String> = (0..3)
            .map(|seat| {
                crate::store::judge_reply_path(Path::new(DIR), 2, &judge_id(seat))
                    .display()
                    .to_string()
            })
            .collect();
        let verdict = crate::store::verdict_reply_path(Path::new(DIR), 2);
        for seat in 0..3 {
            let body = scorecard_of(&s, 2, seat).body;
            // No judge's R1 overview, strengths or feedback, its own neither.
            for who in ["judge", "judge-2", "judge-3"] {
                for text in [
                    format!("{who} overview"),
                    format!("{who} strengths of"),
                    format!("{who} feedback to"),
                ] {
                    assert!(!body.contains(&text), "{} holds {text:?}", judge_id(seat));
                }
            }
            for (other, path) in paths.iter().enumerate() {
                assert_eq!(body.contains(path.as_str()), other == seat, "{path}");
            }
            assert!(!body.contains(&verdict.display().to_string()));
            // No score at all: the R1 scores (90, 85, 80, 79, 75, 70) are
            // nowhere outside the JSON template's example.
            for score in [90, 80, 79, 75, 70] {
                assert!(!body.contains(&format!("{score}/100")), "{score}");
            }
            // The published checklist is the lead's, and every judge applies it.
            assert!(body.contains(
                "The judge's checklist (published with the R1 judgment):\n- 320x568: no horizontal scroll"
            ));
        }
    }

    #[test]
    fn contestant_brief_carries_the_mean_and_each_judges_feedback() {
        let mut s = panel_session(Some(3));
        settle(
            &mut s,
            1,
            &cards(
                &[("codex", 90), ("claude", 70)],
                &[("codex", 85), ("claude", 75)],
                &[("codex", 80), ("claude", 79)],
            ),
        )
        .unwrap();
        judge_next(&mut s, 2);
        let mut r2 = cards(
            &[("codex", 88), ("claude", 80)],
            &[("claude", 84), ("codex", 83)],
            &[("codex", 82), ("claude", 81)],
        );
        r2.get_mut(&judge_id(1)).unwrap().amendments = vec![Amendment {
            round: 1,
            part: PartId::new("claude"),
            score: 82,
            reason: "R1 was judged without its stylesheet".into(),
        }];
        settle(&mut s, 2, &r2).unwrap();
        let claude = PartId::new("claude");
        let brief = contestant_brief(
            &s,
            &claude,
            3,
            Path::new("/tmp/entries/claude"),
            Path::new(DIR),
            Path::new("/tmp/comp/r3/report-claude.md"),
        );
        let body = &brief.body;
        // claude R2: 80, 84, 81 → 81.7; codex 88, 83, 82 → 84.3.
        for line in [
            "Judges' evaluation of your R2 entry (mean of 3 judges):\n- Score: 81.7/100 (judge 80, judge-2 84, judge-3 81)\n",
            "- judge: Strengths: judge strengths of claude / Feedback to address: judge feedback to claude\n  - all: 80/100\n",
            "- judge-2: Strengths: judge-2 strengths of claude / Feedback to address: judge-2 feedback to claude\n  - all: 84/100\n",
            "- judge-3: Strengths: judge-3 strengths of claude / Feedback to address: judge-3 feedback to claude\n  - all: 81/100\n",
            "- Standing after R2: rank 2 of 2, 2.7 point(s) behind the leader.\n",
            // R1: 70, 82, 79 → 77.0.
            "- Score correction: judge-2 changed your R1 score from 75 to 82 (R1 was judged without its stylesheet); R1 mean is now 77.0.\n",
        ] {
            assert!(body.contains(line), "missing {line:?} in\n{body}");
        }
        // Only its own: nothing about codex, and no single-judge lines.
        assert!(
            !body.contains("of codex") && !body.contains("to codex"),
            "{body}"
        );
        assert!(!body.contains("Judge evaluation of your"));
        assert!(!body.contains("went from"));

        let leader = contestant_brief(
            &s,
            &PartId::new("codex"),
            3,
            Path::new("/tmp/entries/codex"),
            Path::new(DIR),
            Path::new("/tmp/comp/r3/report-codex.md"),
        );
        assert!(
            leader
                .body
                .contains("Standing after R2: rank 1 of 2, you lead by 2.7 point(s).")
        );
        assert!(!leader.body.contains("Score correction"));
    }

    #[test]
    fn verdict_brief_lists_combined_scores_and_the_top_set() {
        let mut s = panel_session(Some(1));
        s.messages.push(Message {
            id: crate::model::types::MessageId::nth(1),
            round: 1,
            author: crate::model::types::Author::User,
            kind: MsgKind::Intervention,
            to: vec![judge_id(0)],
            body: "Weigh accessibility.".into(),
            at: 5,
        });
        settle(
            &mut s,
            1,
            &cards(
                &[("codex", 90), ("claude", 70)],
                &[("codex", 81), ("claude", 80)],
                &[("codex", 80), ("claude", 75)],
            ),
        )
        .unwrap();
        let path = crate::store::verdict_reply_path(Path::new(DIR), 1);
        let brief = verdict_brief(&s, 1, &path);
        assert_eq!(brief.to, judge_id(0));
        assert_eq!(brief.reply_path, path);
        let body = &brief.body;
        for line in [
            "## 1. Final verdict — Round 1 of 1\n",
            "This is the FINAL round: the competition ends with your verdict.\n",
            "## 2. Combined scores (mean of 3 judges)\n1. codex — mean 83.7 (judge 90, judge-2 81, judge-3 80; spread 10)\n2. claude — mean 75.0 (judge 70, judge-2 80, judge-3 75; spread 10)\nHighest mean: codex\n",
            "## 3. Each judge's overview\n### judge (lead, you)\njudge overview\n\n### judge-2\njudge-2 overview\n\n### judge-3\njudge-3 overview\n",
            "## 4. User interventions\n- [m1 R1 to judge] Weigh accessibility.\n",
            "Write ONLY valid JSON (no prose around it) to: /tmp/comp/r1/verdict.json\n",
            r#"{"winner": "<one of: codex>", "final_verdict": "#,
        ] {
            assert!(body.contains(line), "missing {line:?} in\n{body}");
        }
        assert!(body.ends_with('\n') && !body.ends_with("\n\n"));

        // A tie at the top, after an early close.
        let mut s = panel_session(Some(3));
        let mut tied = cards(
            &[("claude", 81), ("codex", 80)],
            &[("codex", 82), ("claude", 80)],
            &[("codex", 80), ("claude", 81)],
        );
        tied.get_mut(&judge_id(0)).unwrap().close = true;
        settle(&mut s, 1, &tied).unwrap();
        let body = verdict_brief(&s, 1, &path).body;
        assert!(
            body.contains("You asked to end the competition early"),
            "{body}"
        );
        assert!(
            body.contains("Tied for the highest mean: claude, codex\n"),
            "{body}"
        );
        assert!(body.contains(r#""<one of: claude, codex>""#), "{body}");
    }
}
