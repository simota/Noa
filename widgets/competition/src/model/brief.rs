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
    if let Some((ev_round, ev)) = session.latest_evaluation_before(to, round) {
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
                        let _ = write!(body, " — score {}/100", ev.score);
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

/// Build the brief for the judge to evaluate all submissions in `round`.
pub fn judge_brief(
    session: &Session,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
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
    if round <= 1 {
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
         - List in each evaluation's `evidence` the concrete claims your score rests on, each with `basis`: \"measured\" (you checked it in the files or screenshots), \"self_reported\" (taken from the report without re-checking), or \"unverified\". Never present a self-reported number as measured.\n\
         - If you find that an earlier round's score rested on a wrong basis (a misread rule, the wrong rendering), correct it in `amendments` with the round, contestant, corrected score and reason, instead of only mentioning it in prose.\n",
    );
    if is_last {
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
    body.push_str(
        "{\n  \"overview\": \"1-3 sentence round overview comparing progress\",\n  \"evaluations\": {\n    \"<contestant_id>\": {\n      \"score\": 85,\n      \"strengths\": \"Key strengths of this entry\",\n      \"feedback\": \"Concrete private feedback for next round, based only on this entry vs the request (no peer ideas)\",\n      \"breakdown\": [\n        { \"axis\": \"<criterion>\", \"score\": 50, \"max\": 60 },\n        { \"axis\": \"<criterion>\", \"score\": 35, \"max\": 40 }\n      ],\n      \"evidence\": [\n        { \"claim\": \"index.html is 19,018 bytes\", \"basis\": \"measured\" },\n        { \"claim\": \"0 axe violations\", \"basis\": \"self_reported\" }\n      ]\n    }\n  },\n  \"ranking\": [\"<best_id>\", \"<second_id>\"],\n  \"checklist\": [\"<check>\"],\n  \"amendments\": [],\n  \"winner\": null,\n  \"final_verdict\": null,\n  \"close\": false\n}\n\nAn amendment looks like: { \"round\": 2, \"contestant\": \"<id>\", \"score\": 80, \"reason\": \"why the R2 score was wrong\" }\n",
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
}
