//! Briefs for reviewers and the lead, assembled from state alone.
//!
//! Information isolation is enforced here: a round-1 (blind) reviewer brief
//! never names another reviewer, a peer reply path, or a peer finding.

use std::fmt::Write as _;
use std::path::Path;

use super::types::{Brief, Location};
use super::types::{
    Finding, LEAD_ID, Lens, MsgKind, PartId, Receipt, RoundKind, RoundNo, Session, TargetKind,
};
use super::validate::{MAX_CLAIM, MAX_FINDINGS};

/// The headings of a round-1 reviewer brief, in order.
pub const BLIND_SECTIONS: [&str; 6] = [
    "## 1. Review request and round",
    "## 2. Target",
    "## 3. Your lens",
    "## 4. Rules",
    "## 5. User interventions",
    "## 6. How to reply",
];

/// The headings of a cross-examination reviewer brief, in order.
pub const CROSS_SECTIONS: [&str; 7] = [
    "## 1. Review request and round",
    "## 2. Target",
    "## 3. Your lens",
    "## 4. Findings so far",
    "## 5. The lead's summary and your question",
    "## 6. User interventions",
    "## 7. How to reply",
];

/// The headings of the lead's brief, in order.
pub const LEAD_SECTIONS: [&str; 8] = [
    "## 1. Review request and round",
    "## 2. Target",
    "## 3. Panel",
    "## 4. Raw findings this round",
    "## 5. Findings so far",
    "## 6. User interventions",
    "## 7. This round",
    "## 8. Your output",
];

/// The question a reviewer gets in a cross round when the lead gave none.
pub const DEFAULT_QUESTION: &str = "Check the findings above against the target: confirm \
     what holds, dispute what does not with evidence, and add what everyone missed.";

fn finish_body(mut body: String) -> String {
    let trimmed = body.trim_end().len();
    body.truncate(trimmed);
    body.push('\n');
    body
}

fn section_request(out: &mut String, heading: &str, session: &Session, round: RoundNo, who: &str) {
    let _ = writeln!(out, "{heading}");
    let kind = session
        .round(round)
        .map(|r| r.kind)
        .unwrap_or_else(|| RoundKind::for_round(round));
    let mode = match kind {
        RoundKind::Blind => "BLIND independent review (you see no other reviewer's work)",
        RoundKind::Cross => "CROSS-EXAMINATION (every finding so far is shown to every reviewer)",
    };
    let _ = writeln!(out, "You are: {who}");
    let _ = writeln!(out, "Round: {round} of {} — {mode}", session.max_rounds);
    let left = session.max_rounds.saturating_sub(round);
    if left == 0 {
        out.push_str("This is the FINAL round.\n");
    } else {
        let _ = writeln!(out, "Rounds left after this one: {left}");
    }
    let request = session.request.trim();
    if request.is_empty() {
        out.push_str("What the user asked for: a thorough review of the target below.\n");
    } else {
        let _ = writeln!(out, "What the user asked for:\n{request}");
    }
    out.push('\n');
}

fn section_target(out: &mut String, heading: &str, session: &Session, reply_dir: &Path) {
    let m = &session.manifest;
    let target_dir = crate::store::target_dir(reply_dir);
    let _ = writeln!(out, "{heading}");
    let _ = writeln!(
        out,
        "Kind: {}",
        match session.target.kind {
            TargetKind::Git => "git diff",
            TargetKind::Files => "files",
            TargetKind::Document => "document",
        }
    );
    let _ = writeln!(out, "Label: {}", m.label);
    if let Some(repo) = &m.repo {
        let _ = writeln!(out, "Source: {}", repo.display());
    }
    if let (Some(base), Some(base_sha)) = (&m.base_ref, &m.base_sha) {
        let _ = writeln!(out, "Base: {base} ({base_sha})");
    }
    if let Some(head) = &m.head_sha {
        let name = m
            .head_ref
            .as_deref()
            .unwrap_or("work tree (HEAD plus uncommitted and untracked changes)");
        let _ = writeln!(out, "Head: {name} ({head})");
    }
    if let Some(mb) = &m.merge_base {
        let _ = writeln!(out, "Merge base: {mb}");
    }
    if let Some(name) = &m.document_name {
        let _ = writeln!(out, "Original file: {name}");
    }
    let _ = writeln!(
        out,
        "Snapshot (review THIS, read-only): {}",
        target_dir.display()
    );
    match session.target.kind {
        TargetKind::Git => {
            let _ = writeln!(
                out,
                "- {}: the diff from the merge base\n- {}/<path>: the new side of every changed text file",
                target_dir.join(crate::capture::DIFF_FILE).display(),
                target_dir.join(crate::capture::FILES_DIR).display()
            );
        }
        TargetKind::Files => {
            let _ = writeln!(
                out,
                "- {}/<path>: every file under review",
                target_dir.join(crate::capture::FILES_DIR).display()
            );
        }
        TargetKind::Document => {
            let _ = writeln!(
                out,
                "- {}: the document under review",
                target_dir.join(crate::capture::DOCUMENT_FILE).display()
            );
        }
    }
    if let Some(workdir) = &session.workdir {
        let _ = writeln!(
            out,
            "Workspace (read-only context; it may have changed since the capture): {}",
            workdir.display()
        );
    }
    out.push_str("Line numbers in findings refer to the snapshot files (the new side).\n");
    let _ = writeln!(out, "Files ({}):", m.files.len());
    for f in &m.files {
        let _ = write!(out, "- {} {} ({} lines", f.status.code(), f.path, f.lines);
        if f.additions > 0 || f.deletions > 0 {
            let _ = write!(out, ", +{} -{}", f.additions, f.deletions);
        }
        out.push(')');
        if let super::types::FileStatus::Renamed { from } = &f.status {
            let _ = write!(out, " from {from}");
        }
        if !f.changed.is_empty() {
            out.push_str(" changed:");
            for (i, (a, b)) in f.changed.iter().enumerate() {
                out.push_str(if i == 0 { " " } else { ", " });
                if a == b {
                    let _ = write!(out, "{a}");
                } else {
                    let _ = write!(out, "{a}-{b}");
                }
            }
        }
        out.push('\n');
    }
    if !m.omitted.is_empty() {
        out.push_str("Left out of the snapshot (not reviewable here):\n");
        for o in &m.omitted {
            let _ = writeln!(out, "- {}: {}", o.path, o.reason);
        }
    }
    out.push('\n');
}

fn lens_text(lens: &Lens) -> String {
    match lens {
        Lens::Builtin { id } => format!("{} ({}): {}", id.id(), id.label(), id.checklist()),
        Lens::Custom { id, text } => format!("{id} (custom): {}", text.trim()),
    }
}

fn section_lens(out: &mut String, heading: &str, session: &Session, to: &PartId) {
    let _ = writeln!(out, "{heading}");
    if let Some(lens) = session.participant(to).and_then(|p| p.lens.as_ref()) {
        let _ = writeln!(
            out,
            "Review from this angle first and most thoroughly:\n{}",
            lens_text(lens)
        );
    }
    out.push_str(
        "Report anything serious you notice outside it too, with the matching category (or `other`).\n\n",
    );
}

fn write_categories(out: &mut String, session: &Session) {
    out.push_str(&session.categories().join(", "));
}

fn section_interventions(
    out: &mut String,
    heading: &str,
    session: &Session,
    to: &PartId,
    round: RoundNo,
) {
    let _ = writeln!(out, "{heading}");
    let mut any = false;
    for msg in session.messages.iter().filter(|m| {
        m.kind == MsgKind::Intervention
            && m.round <= round
            && (m.to.is_empty() || m.to.contains(to) || to.as_str() == LEAD_ID)
    }) {
        any = true;
        let _ = write!(out, "- [{} R{}", msg.id, msg.round);
        if to.as_str() == LEAD_ID {
            out.push_str(" to ");
            if msg.to.is_empty() {
                out.push_str("everyone");
            } else {
                let names: Vec<&str> = msg.to.iter().map(PartId::as_str).collect();
                out.push_str(&names.join(", "));
            }
        }
        let _ = writeln!(out, "] {}", msg.body.trim());
    }
    if !any {
        out.push_str("(none)\n");
    }
    out.push('\n');
}

fn write_location(out: &mut String, loc: Option<&Location>) {
    match loc {
        Some(loc) => {
            let _ = write!(out, "{loc}");
        }
        None => out.push_str("(whole target)"),
    }
}

/// One finding with its sources, status and stances, for briefs.
fn write_finding(out: &mut String, session: &Session, f: &Finding, full: bool) {
    let _ = write!(
        out,
        "### {} [{}] {} · {} · {} · ",
        f.id,
        f.status.name(),
        f.severity.name(),
        f.category,
        f.confidence.name()
    );
    write_location(out, f.location.as_ref());
    out.push('\n');
    let _ = writeln!(out, "Claim: {}", f.claim);
    if !full {
        return;
    }
    let _ = writeln!(out, "Lead's rationale: {}", f.rationale);
    if let Some(fix) = &f.fix {
        let _ = writeln!(out, "Suggested fix: {fix}");
    }
    out.push_str("Sources:\n");
    for key in &f.sources {
        if let Some(raw) = session.raw_finding(key) {
            let _ = write!(
                out,
                "- {key} (R{}, {}, {}): {}\n  Evidence: {}",
                raw.round,
                raw.severity.name(),
                raw.confidence.name(),
                raw.claim,
                raw.evidence.trim()
            );
            if let Some(fix) = &raw.fix {
                let _ = write!(out, "\n  Fix: {}", fix.trim());
            }
            out.push('\n');
        }
    }
    if f.stances.is_empty() {
        out.push_str("Stances: (none yet)\n");
    } else {
        out.push_str("Stances:\n");
        for (part, stances) in &f.stances {
            for s in stances {
                let _ = write!(out, "- {part} R{}: {}", s.round, s.kind.name());
                if let Some(sev) = s.severity {
                    let _ = write!(out, " (severity {})", sev.name());
                }
                if !s.note.is_empty() {
                    let _ = write!(out, " — {}", s.note);
                }
                if let Some(e) = &s.evidence {
                    let _ = write!(out, "\n  Evidence: {}", e.trim());
                }
                out.push('\n');
            }
        }
    }
}

fn section_findings(out: &mut String, heading: &str, session: &Session) {
    let _ = writeln!(out, "{heading}");
    let live: Vec<&Finding> = session
        .findings
        .iter()
        .filter(|f| f.status.is_live())
        .collect();
    if live.is_empty() {
        out.push_str("(no live findings)\n");
    }
    for f in &live {
        write_finding(out, session, f, true);
        out.push('\n');
    }
    let settled: Vec<&Finding> = session
        .findings
        .iter()
        .filter(|f| !f.status.is_live())
        .collect();
    if !settled.is_empty() {
        out.push_str("Settled (rejected, withdrawn or merged; shown for reference):\n");
        for f in settled {
            let _ = write!(out, "- {} [{}", f.id, f.status.name());
            if let Some(into) = &f.merged_into {
                let _ = write!(out, " into {into}");
            }
            let _ = writeln!(out, "] {}", f.claim);
        }
    }
    out.push('\n');
}

const REVIEWER_RULES: [&str; 5] = [
    "Review the snapshot. Do NOT modify any file in the workspace or the snapshot: you review, you do not fix. Write only your reply file.",
    "Back every finding with evidence from the files (a quote, a trace through the code, a reproduction). Mark anything you could not verify as unverified in the evidence.",
    "Prefer a few precise, well-evidenced findings over many vague ones; say so when the target looks sound.",
    "Do not contact or type into other panes.",
    "Write the reply in the language of the user's request (English when there is none); keep ids, field names and enum values as given.",
];

fn write_reply_format(out: &mut String, session: &Session, cross: bool, reply_path: &Path) {
    let _ = write!(
        out,
        "Write ONLY this JSON to the file:\n{}\n\
         Write the whole file in one go when you are done: the file appearing is how your reply is collected. Do not answer in the terminal.\n\n\
         {{ \"summary\": \"what you reviewed and your overall assessment\",\n\
         \x20 \"findings\": [{{\"id\": \"1\", \"location\": {{\"path\": \"src/a.rs\", \"line\": 10, \"end_line\": 14}},\n\
         \x20               \"severity\": \"high\", \"category\": \"correctness\", \"confidence\": \"medium\",\n\
         \x20               \"claim\": \"one line\", \"evidence\": \"…\", \"fix\": \"…\"}}],\n",
        reply_path.display()
    );
    if cross {
        out.push_str(
            "  \"stances\": [{\"finding\": \"F1\", \"stance\": \"dispute\", \"note\": \"…\", \"evidence\": \"…\", \"severity\": \"low\"}],\n",
        );
    }
    out.push_str(
        "  \"coverage\": {\"reviewed\": [\"src/a.rs\", \"src/ui/\"], \"skipped\": [{\"path\": \"docs/\", \"reason\": \"…\"}]} }\n\nRules:\n",
    );
    let _ = writeln!(
        out,
        "- findings: at most {MAX_FINDINGS}; each id 1-16 letters, digits, '-' or '_', new (not one you used in an earlier round)"
    );
    out.push_str("- location: null for the target as a whole, else a path from the file list; line and end_line (optional) within that file in the snapshot\n");
    out.push_str(
        "- severity: critical, high, medium, low or info; confidence: high, medium or low\n",
    );
    out.push_str("- category: ");
    write_categories(out, session);
    out.push('\n');
    let _ = writeln!(
        out,
        "- claim: one line, at most {MAX_CLAIM} characters; evidence: required; fix: optional"
    );
    if cross {
        out.push_str("- stance: confirm, dispute, refine or unsure on findings you did not raise; refine or withdraw on your own; note required for dispute and refine; severity is an optional proposal\n");
    }
    out.push_str("- coverage (optional): what you reviewed and what you skipped, as files, directories ending in '/', or \"*\"\n");
}

/// The brief for reviewer `to` in `round`.
pub fn reviewer_brief(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
) -> Brief {
    let kind = session
        .round(round)
        .map(|r| r.kind)
        .unwrap_or_else(|| RoundKind::for_round(round));
    let mut body = String::with_capacity(8192);
    let who = format!("reviewer {to}");
    match kind {
        RoundKind::Blind => {
            section_request(&mut body, BLIND_SECTIONS[0], session, round, &who);
            section_target(&mut body, BLIND_SECTIONS[1], session, reply_dir);
            section_lens(&mut body, BLIND_SECTIONS[2], session, to);
            body.push_str(BLIND_SECTIONS[3]);
            body.push('\n');
            body.push_str("- BLIND ROUND: review on your own. Do not open other reply files in the round directories; you will see the other reviewers' findings next round.\n");
            for rule in REVIEWER_RULES {
                let _ = writeln!(body, "- {rule}");
            }
            body.push('\n');
            section_interventions(&mut body, BLIND_SECTIONS[4], session, to, round);
            body.push_str(BLIND_SECTIONS[5]);
            body.push('\n');
            write_reply_format(&mut body, session, false, reply_path);
        }
        RoundKind::Cross => {
            section_request(&mut body, CROSS_SECTIONS[0], session, round, &who);
            section_target(&mut body, CROSS_SECTIONS[1], session, reply_dir);
            section_lens(&mut body, CROSS_SECTIONS[2], session, to);
            section_findings(&mut body, CROSS_SECTIONS[3], session);
            body.push_str(CROSS_SECTIONS[4]);
            body.push('\n');
            match session.summary_before(round) {
                Some((no, summary)) => {
                    let _ = writeln!(body, "Lead's summary of R{no}:\n{}\n", summary.trim());
                }
                None => body.push_str("(no summary yet)\n\n"),
            }
            let question = session.question_for(to, round).unwrap_or(DEFAULT_QUESTION);
            let _ = writeln!(body, "Your question: {question}\n");
            for rule in REVIEWER_RULES {
                let _ = writeln!(body, "- {rule}");
            }
            body.push('\n');
            section_interventions(&mut body, CROSS_SECTIONS[5], session, to, round);
            body.push_str(CROSS_SECTIONS[6]);
            body.push('\n');
            let required = session.required_stances(to);
            if required.is_empty() {
                body.push_str(
                    "Stances required on: (none; you may still revise earlier stances)\n",
                );
            } else {
                let _ = writeln!(body, "Stances required on: {}", required.join(", "));
            }
            write_reply_format(&mut body, session, true, reply_path);
        }
    }
    Brief {
        to: to.clone(),
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// The lead's consolidation brief for `round`.
pub fn lead_brief(session: &Session, round: RoundNo, reply_dir: &Path, reply_path: &Path) -> Brief {
    let mut body = String::with_capacity(16384);
    section_request(
        &mut body,
        LEAD_SECTIONS[0],
        session,
        round,
        "the lead (you consolidate; you write no findings of your own)",
    );
    section_target(&mut body, LEAD_SECTIONS[1], session, reply_dir);

    let _ = writeln!(body, "{}", LEAD_SECTIONS[2]);
    let this = session.round(round);
    for part in &session.reviewers {
        let lens = part.lens.as_ref().map(lens_text).unwrap_or_default();
        let _ = write!(body, "- {} ({}): ", part.id, part.kind.command());
        let receipt = this.and_then(|r| r.brief(&part.id)).map(|b| &b.receipt);
        match receipt {
            Some(r) if r.replied.is_some() => body.push_str("replied"),
            Some(r) => match &r.failed {
                Some(why) => {
                    let _ = write!(body, "no reply: {why}");
                }
                None => body.push_str("no reply"),
            },
            None => body.push_str("not briefed"),
        }
        let _ = writeln!(body, "\n  Lens: {lens}");
        if let Some(note) = this.and_then(|r| r.reviews.get(&part.id)) {
            let _ = writeln!(body, "  Summary: {}", note.summary.trim());
        }
    }
    body.push('\n');

    let _ = writeln!(body, "{}", LEAD_SECTIONS[3]);
    let raws: Vec<_> = session
        .raw
        .iter()
        .filter(|r| r.round == round && session.placement(&r.key).is_none())
        .collect();
    if raws.is_empty() {
        body.push_str("(none)\n");
    }
    for raw in raws {
        let _ = write!(
            body,
            "### {} · {} · {} · {} · ",
            raw.key,
            raw.severity.name(),
            raw.category,
            raw.confidence.name()
        );
        write_location(&mut body, raw.location.as_ref());
        if !raw.in_target {
            body.push_str(" (outside the target)");
        } else if raw.in_diff == Some(false) {
            body.push_str(" (outside the changed lines)");
        }
        let _ = writeln!(
            body,
            "\nClaim: {}\nEvidence: {}",
            raw.claim,
            raw.evidence.trim()
        );
        if let Some(fix) = &raw.fix {
            let _ = writeln!(body, "Fix: {}", fix.trim());
        }
        body.push('\n');
    }

    section_findings(&mut body, LEAD_SECTIONS[4], session);
    section_interventions(
        &mut body,
        LEAD_SECTIONS[5],
        session,
        &session.lead.id,
        round,
    );

    let _ = writeln!(body, "{}", LEAD_SECTIONS[6]);
    let last = session.is_last(round);
    if last {
        body.push_str("This is the LAST round: \"verdict\" is required, and no finding may stay open: settle each one as agreed, disputed or rejected. Weigh the evidence yourself where the reviewers disagree; you may read the snapshot to decide.\n");
    } else {
        let left = session.max_rounds - round;
        let _ = writeln!(
            body,
            "{left} round(s) left after this one. Ask each reviewer the question that will settle what is still open or disputed. Leave \"verdict\" out unless you close the review."
        );
        if super::validate::may_close(session, round) {
            body.push_str(
                "You may set \"close\": true with a \"verdict\" if nothing is left to settle.\n",
            );
        } else {
            body.push_str(
                "\"close\" is not allowed in round 1: the findings must be cross-examined first.\n",
            );
        }
    }
    body.push('\n');

    let _ = writeln!(body, "{}", LEAD_SECTIONS[7]);
    let next = session.next_finding_id();
    let verdict = if last {
        "  \"verdict\": {\"decision\": \"request_changes\", \"rationale\": \"…\", \"must_fix\": [\"F1\"]},\n"
    } else {
        ""
    };
    let _ = write!(
        body,
        "Write ONLY this JSON to the file:\n{}\n\
         Write the whole file in one go when you are done. Do not answer in the terminal.\n\n\
         {{ \"summary\": \"what this round found and what is still in dispute\",\n\
         \x20 \"ops\": [{{\"op\": \"add\", \"id\": \"{next}\", \"from\": [\"claude/1\", \"codex/2\"], \"claim\": \"one line\",\n\
         \x20          \"location\": {{\"path\": \"src/a.rs\", \"line\": 10}}, \"severity\": \"high\", \"category\": \"correctness\",\n\
         \x20          \"confidence\": \"high\", \"status\": \"open\", \"rationale\": \"…\", \"fix\": \"…\"}},\n\
         \x20         {{\"op\": \"merge\", \"id\": \"F1\", \"from\": [\"agy/4\"]}},\n\
         \x20         {{\"op\": \"update\", \"id\": \"F2\", \"severity\": \"medium\", \"status\": \"disputed\", \"rationale\": \"…\"}},\n\
         \x20         {{\"op\": \"dismiss\", \"raw\": \"agy/5\", \"reason\": \"…\"}}],\n\
         \x20 \"questions\": {{\"codex\": \"…\"}},\n\
         {verdict}\
         \x20 \"close\": false }}\n\nRules:\n",
        reply_path.display()
    );
    body.push_str("- every raw finding in section 4 goes into exactly one add or merge \"from\", or one dismiss with a reason; none may be left out\n");
    let _ = writeln!(
        body,
        "- new finding ids: 1-16 letters, digits, '-' or '_', not taken; the next free one is {next}"
    );
    body.push_str(
        "- merge \"from\" may also name other live findings, which become merged into \"id\"\n",
    );
    body.push_str("- update changes at least one of claim, location, severity, category, confidence, status, rationale, fix\n");
    body.push_str("- status: open, agreed, disputed or rejected (rejected needs a rationale)\n");
    body.push_str(
        "- severity: critical, high, medium, low, info; confidence: high, medium, low; category: ",
    );
    write_categories(&mut body, session);
    body.push('\n');
    body.push_str("- questions: reviewer ids: ");
    let ids: Vec<&str> = session.reviewer_ids().map(PartId::as_str).collect();
    body.push_str(&ids.join(", "));
    body.push_str("; a reviewer with no question gets a default one\n");
    body.push_str("- verdict.decision: approve (no must_fix), approve_with_fixes or request_changes (at least one must_fix), reject; must_fix lists live finding ids\n");
    body.push_str("- verdict: only in the last round or with \"close\": true\n");

    let lead = session.lead.id.clone();
    Brief {
        to: lead,
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// What an agent is sent when its output file was turned down.
pub fn correction_note(to: &PartId, error: &str, reply_path: &Path) -> String {
    let what = if to.as_str() == LEAD_ID {
        "lead JSON"
    } else {
        "review JSON"
    };
    format!(
        "Your {what} could not be used:\n{error}\n\nFix it and write the whole file again at:\n{}\n",
        reply_path.display()
    )
}
