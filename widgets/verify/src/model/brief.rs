//! The briefs agents get (L2-4), in English with constant section
//! headings, and the note that sends a turned-down file back.
//!
//! Information isolation is enforced here: a round-1 (blind) verifier brief
//! never names another verifier, a peer worktree or a peer reply path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::model::types::{
    Brief, BriefKind, CrossCheck, EvidenceBody, EvidenceKey, Isolation, JUDGE_ID, MAX_CAPTION,
    MAX_CMD, MAX_CRITERIA, MAX_DEFECTS, MAX_EVIDENCE_ITEMS, MAX_LOG_BYTES, MAX_MARKS, MAX_RERUNS,
    MAX_STATEMENT, MsgKind, PartId, Receipt, Rerun, RoundKind, RoundNo, Session, Strategy, Subject,
    TargetKind, short_id,
};
use crate::model::validate::{MAX_CLAIM, MAX_ID_LEN, RESERVED_DIR};

/// The headings of the judge's criteria brief, in order.
pub const CRITERIA_SECTIONS: [&str; 6] = [
    "## 1. Verification request",
    "## 2. Target",
    "## 3. Spec",
    "## 4. Criteria the person gave",
    "## 5. How to write criteria",
    "## 6. Your output",
];

/// The headings of a round-1 verifier brief, in order.
pub const BLIND_SECTIONS: [&str; 8] = [
    "## 1. Request and round",
    "## 2. Target and your worktree",
    "## 3. Criteria",
    "## 4. Your strategy",
    "## 5. Evidence",
    "## 6. Rules",
    "## 7. User interventions",
    "## 8. How to reply",
];

/// The headings of a cross-check (round ≥ 2) verifier brief, in order.
pub const CROSS_SECTIONS: [&str; 9] = [
    "## 1. Request and round",
    "## 2. Target and your worktree",
    "## 3. Criteria",
    "## 4. Your strategy",
    "## 5. Rulings and results so far",
    "## 6. Your assignments",
    "## 7. Evidence",
    "## 8. User interventions",
    "## 9. How to reply",
];

/// The headings of the judge's ruling brief, in order.
pub const RULING_SECTIONS: [&str; 9] = [
    "## 1. Request and round",
    "## 2. Criteria",
    "## 3. Panel and receipts",
    "## 4. Results this round",
    "## 5. Cross-checks",
    "## 6. Defects",
    "## 7. Evidence standard",
    "## 8. This round",
    "## 9. Your output",
];

/// The only heading of a re-run brief.
pub const RERUN_SECTIONS: [&str; 1] = ["## Re-run"];

/// The evidence standard, as the ruling brief states it (L1-6 rules).
pub const EVIDENCE_STANDARD: [&str; 10] = [
    "Every criterion gets one ruling per round; \"ruling\": \"same\" carries the previous one over unchanged. Cited evidence keys exist.",
    "Each rule reads the criterion's whole history: every result on it from every verifier and round, every cross-check on one of those results, every cross-check on a defect, and the evidence they cite.",
    "A cross-check of round N on <verifier>/<criterion> is about that verifier's latest result on the criterion as of round N-1, the one its brief named; once the verifier has reported a later result, the later checks on that subject are about the later result.",
    "Evidence is for a criterion when a result on it cites it, or a cross-check about a result on it, or it is media whose shows names it (media with shows is evidence only for the criteria it names). It counts when its file was stored (an observation has no file and counts as text); a command or file missing at ingest counts for nothing. Evidence cited for verified or failed must be evidence for that criterion.",
    "A pass of any round counts when the ruling cites a command or file of it that counts, unless it is contested. A pass is contested once a not_reproduced cross-check about that very result cites evidence that counts, until a later pass on the criterion (of a round after that check, from any verifier, its own included) that has a command or file that counts for the criterion and is not itself contested (media whose shows leaves the criterion out does not lift it); a contested pass is neither one of two sources nor the pass whose matched re-run is cited. A disputed check contests nothing. Each verifier is one source however many of its passes count; media the person attached (user/<n>) whose shows names the criterion is one more source, so it counts only together with a verifier's pass. A cross-check is no pass, whatever its outcome; only a not_reproduced that contests a pass takes one away.",
    "A fail of any round is open until a not_reproduced cross-check about that very result cites evidence that counts. Nothing else answers it: not a later blocked, not_tested or pass result of the same verifier, not a disputed or reproduced check, not a not_reproduced about another result of that verifier; a fail its verifier replaced before anyone checked it stays open. A fail that a reproduced cross-check about it reproduced is confirmed, whatever comes later.",
    "verified needs a cited command or file (an observation alone never satisfies it), no open and no confirmed fail on the criterion, and either a cited command of a counting pass whose re-run matched (the matched re-run of a fail's command is not one) or two sources (cite one item of each); a contested pass gives neither.",
    "failed needs cited evidence that counts and that a fail result on the criterion cites, or a reproduced cross-check about a fail result on it, or a reproduced cross-check on a defect (also one merged since); a cross-check about a pass backs no failure.",
    "inconclusive and not_applicable need a rationale.",
    "Defect ops: every raw defect of this round goes into exactly one add or merge \"from\", or one dismiss.",
];

fn finish_body(mut body: String) -> String {
    let trimmed = body.trim_end().len();
    body.truncate(trimmed);
    body.push('\n');
    body
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn kind_of(session: &Session, round: RoundNo) -> RoundKind {
    session
        .round(round)
        .map(|r| r.kind)
        .unwrap_or_else(|| RoundKind::for_round(round))
}

fn write_request(out: &mut String, session: &Session) {
    let request = session.request.trim();
    if request.is_empty() {
        out.push_str("What the person asked for: verify that the change does what it claims.\n");
    } else {
        let _ = writeln!(out, "What the person asked for:\n{request}");
    }
}

fn section_request(out: &mut String, heading: &str, session: &Session, round: RoundNo, who: &str) {
    let _ = writeln!(out, "{heading}");
    let judge = who.starts_with("the judge");
    let mode = match (kind_of(session, round), judge) {
        (RoundKind::Blind, false) => "INDEPENDENT verification (you see no other verifier's work)",
        (RoundKind::Blind, true) => "ruling on the independent round (each verifier worked alone)",
        (RoundKind::Cross, false) => {
            "CROSS-CHECK (every result so far is shown; reproduce or refute what you are assigned)"
        }
        (RoundKind::Cross, true) => "ruling on a cross-check round",
    };
    let _ = writeln!(out, "You are: {who}");
    let _ = writeln!(out, "Round: {round} of {} — {mode}", session.max_rounds);
    let left = session.max_rounds.saturating_sub(round);
    if left == 0 {
        out.push_str("This is the FINAL round.\n");
    } else {
        let _ = writeln!(out, "Rounds left after this one: {left}");
    }
    write_request(out, session);
    out.push('\n');
}

/// What was captured, for every brief: the kind, the commits, the files.
fn write_target(out: &mut String, session: &Session, reply_dir: &Path) {
    let m = &session.manifest;
    let target_dir = crate::store::target_dir(reply_dir);
    let _ = writeln!(
        out,
        "Kind: {}",
        match session.target.kind {
            TargetKind::Diff => "git diff (a change: base … head)",
            TargetKind::State => "current state (the tree as it is; no base)",
        }
    );
    let _ = writeln!(out, "Label: {}", m.label);
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
    let _ = writeln!(out, "Snapshot (read-only): {}", target_dir.display());
    let _ = writeln!(
        out,
        "- {}: {}",
        target_dir.join(crate::capture::DIFF_FILE).display(),
        match session.target.kind {
            TargetKind::Diff => "the diff from the merge base",
            TargetKind::State => "the uncommitted changes against HEAD (empty for a ref)",
        }
    );
    if session.spec.is_some() {
        let _ = writeln!(
            out,
            "- {}: the spec the criteria come from",
            target_dir.join(crate::capture::SPEC_FILE).display()
        );
    }
    if !m.files.is_empty() {
        let _ = writeln!(out, "Changed files ({}):", m.files.len());
        for f in &m.files {
            let _ = write!(out, "- {} {} ({} lines", f.status.code(), f.path, f.lines);
            if f.additions > 0 || f.deletions > 0 {
                let _ = write!(out, ", +{} -{}", f.additions, f.deletions);
            }
            out.push(')');
            if let crate::model::types::FileStatus::Renamed { from } = &f.status {
                let _ = write!(out, " from {from}");
            }
            out.push('\n');
        }
    }
    if !m.omitted.is_empty() {
        out.push_str("Left out of the snapshot:\n");
        for o in &m.omitted {
            let _ = writeln!(out, "- {}: {}", o.path, o.reason);
        }
    }
}

/// The criteria, frozen, with their priority, source and how.
fn write_criteria(out: &mut String, session: &Session) {
    for c in &session.criteria {
        let _ = writeln!(
            out,
            "- {} [{}] ({}) {}\n  How: {}",
            c.id,
            c.priority.name(),
            c.source,
            c.statement,
            if c.how.trim().is_empty() {
                "(not given; choose a way that shows it)"
            } else {
                c.how.trim()
            }
        );
    }
}

fn strategy_text(strategy: &Strategy) -> String {
    match strategy {
        Strategy::Builtin { id } => format!("{} ({}): {}", id.id(), id.label(), id.checklist()),
        Strategy::Custom { id, text } => format!("{id} (custom): {}", text.trim()),
    }
}

fn section_interventions(
    out: &mut String,
    heading: &str,
    session: &Session,
    to: &PartId,
    round: RoundNo,
) {
    let _ = writeln!(out, "{heading}");
    let judge = to.as_str() == JUDGE_ID;
    let mut any = false;
    for msg in session.messages.iter().filter(|m| {
        m.kind == MsgKind::Intervention
            && m.round <= round
            && (m.to.is_empty() || m.to.contains(to) || judge)
    }) {
        any = true;
        let _ = write!(out, "- [{} R{}", msg.id, msg.round);
        if judge {
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

/// The judge's criteria brief; it writes `reply_path` (`c/criteria.json`
/// under `reply_dir`, the judge's reply dir).
pub fn criteria_brief(session: &Session, reply_dir: &Path, reply_path: &Path) -> Brief {
    let mut body = String::with_capacity(8192);
    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[0]);
    body.push_str(
        "You are: the judge of a multi-agent verification. First you write the acceptance criteria the change is verified against; verifiers then run checks in their own copies, and you rule on every criterion from the evidence they record. You run nothing yourself.\n",
    );
    write_request(&mut body, session);
    body.push('\n');

    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[1]);
    write_target(&mut body, session, reply_dir);
    let _ = writeln!(
        body,
        "Workspace (read-only context): {}\n",
        session.workdir.display()
    );

    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[2]);
    match &session.spec {
        Some(spec) => {
            let _ = writeln!(
                body,
                "{} ({} lines), copied to {}. It is the main source of criteria: cite a criterion taken from it as source \"spec:<line>\".\n",
                spec.path,
                spec.lines,
                crate::store::target_dir(reply_dir)
                    .join(crate::capture::SPEC_FILE)
                    .display()
            );
        }
        None => {
            body.push_str("(no spec file; take the criteria from the request and the target)\n\n")
        }
    }

    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[3]);
    if session.user_criteria.is_empty() {
        body.push_str("(none)\n");
    } else {
        body.push_str("Each line below must appear as exactly one criterion with source \"user\" and this wording kept as its statement (you may add how):\n");
        for line in &session.user_criteria {
            let _ = writeln!(body, "- {line}");
        }
    }
    body.push('\n');

    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[4]);
    body.push_str(
        "- Each criterion is one checkable behaviour, stated so that a run can pass or fail it.\n",
    );
    body.push_str("- \"how\" is concrete enough to run: the command, the steps in the app, the input and what to look for.\n");
    body.push_str("- must: the change does not work without it; should: expected, but not blocking on its own.\n");
    body.push_str("- source: user (a line of section 4), spec:<line> (a line of the spec), request (the request text), inferred (what the change implies).\n");
    body.push_str("- Cover what the change claims; do not add wishes it never claimed.\n\n");

    let _ = writeln!(body, "{}", CRITERIA_SECTIONS[5]);
    let _ = write!(
        body,
        "Write ONLY this JSON to the file:\n{}\n\
         Write the whole file in one go when you are done: the file appearing is how your reply is collected. Do not answer in the terminal.\n\n\
         {{ \"summary\": \"What the change claims and how the criteria cover it.\",\n\
         \x20 \"criteria\": [\n\
         \x20   {{\"id\": \"AC-1\", \"statement\": \"Opening a tab inherits the focused pane's cwd.\",\n\
         \x20    \"how\": \"Run the app, cd /tmp in a pane, open a tab, check pwd.\",\n\
         \x20    \"priority\": \"must\", \"source\": \"user\"}}\n\
         \x20 ] }}\n\nRules:\n",
        reply_path.display()
    );
    let _ = writeln!(
        body,
        "- 1 to {MAX_CRITERIA} criteria; ids AC-1, AC-2, … from 1 without gaps"
    );
    let _ = writeln!(
        body,
        "- statement: one line, at most {MAX_STATEMENT} characters; how: not empty"
    );
    body.push_str("- priority: must or should; source: user, request, inferred or spec:<line>\n");
    body.push_str(
        "- every line of section 4 appears once, with source user and its exact wording\n",
    );
    let judge = session.judge.id.clone();
    Brief {
        to: judge,
        kind: BriefKind::Work,
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// Where a verifier runs, for its brief.
fn write_worktree(out: &mut String, session: &Session, worktree: &Path, reply_dir: &Path) {
    match session.isolation {
        Isolation::Worktree => {
            let _ = writeln!(
                out,
                "Your worktree: {}\n\
                 It is a detached git worktree of your own, built from {}{}. Run everything here. You may edit any file inside it (throwaway tests, debug prints); edit nothing anywhere else. It has its own build cache (target/ or the language's equivalent): the first build starts cold.",
                worktree.display(),
                short_id(&session.rebuild.head_sha, 12),
                if session.rebuild.worktree_patch.is_some() || !session.rebuild.untracked.is_empty()
                {
                    " plus the person's uncommitted and untracked changes"
                } else {
                    ""
                }
            );
        }
        Isolation::Shared => {
            let _ = writeln!(
                out,
                "Where you run: {} — the person's own checkout, shared with the other verifiers. Do NOT edit, create or delete any tracked file, and run only commands that do not write tracked files. Re-runs are off in this mode.",
                worktree.display()
            );
            // Nothing made this checkout the target: a ref head names a
            // commit the person may not have out (L1-3).
            let sha = &session.rebuild.head_sha;
            match &session.manifest.head_ref {
                Some(head) => {
                    let _ = writeln!(
                        out,
                        "Its HEAD must be {sha}, the commit of {head} you are verifying; the widget did not record which commit this checkout has out."
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "Its HEAD must be {sha}: the commit it had out when the target was captured, which the target is with the person's uncommitted and untracked changes."
                    );
                }
            }
            let _ = writeln!(
                out,
                "Before anything else, run `git rev-parse HEAD` there. If it does not print {sha}, this checkout is not the target: run nothing else, and report every criterion as blocked with a note naming the commit you saw."
            );
        }
    }
    let _ = writeln!(
        out,
        "Your reply dir: {} (everything you save goes under its r<N>/<you>/ folder).",
        reply_dir.display()
    );
}

fn section_target(
    out: &mut String,
    heading: &str,
    session: &Session,
    worktree: &Path,
    reply_dir: &Path,
) {
    let _ = writeln!(out, "{heading}");
    write_target(out, session, &crate::store::reply_dir(session));
    write_worktree(out, session, worktree, reply_dir);
    out.push('\n');
}

fn section_strategy(out: &mut String, heading: &str, session: &Session, to: &PartId) {
    let _ = writeln!(out, "{heading}");
    if let Some(strategy) = session.participant(to).and_then(|p| p.strategy.as_ref()) {
        let _ = writeln!(
            out,
            "Verify this way first and most thoroughly:\n{}",
            strategy_text(strategy)
        );
    }
    out.push_str(
        "Whatever your strategy, report a result for every criterion you are asked about.\n\n",
    );
}

/// Where a verifier saves evidence and how it reports it (L1-5, L1-5a).
fn section_evidence(out: &mut String, heading: &str, session: &Session, part_dir: &Path) {
    let _ = writeln!(out, "{heading}");
    let _ = writeln!(
        out,
        "Save every file you cite under {} and cite it by its path relative to that folder:",
        part_dir.display()
    );
    out.push_str("- Command output under logs/, stdout and stderr together: `cmd > logs/3.log 2>&1; echo $?`. Report the exit status you saw.\n");
    out.push_str("- Screenshots under shots/ and screen recordings under video/, made with the platform's own capture tools (on macOS: `screencapture -x shots/1.png`, `screencapture -v video/1.mov`). Every picture or recording needs a caption saying what it shows; a recording may list marks (moments to look at).\n");
    out.push_str("- When capturing the screen is not allowed (on macOS, a screen-recording permission your terminal lacks), report the criterion as blocked with that reason; do not describe the screen as an observation instead.\n");
    out.push_str("- An observation (text only) is shown as unbacked and never makes a criterion verified on its own.\n");
    let _ = writeln!(
        out,
        "- {RESERVED_DIR}/ is the widget's: save nothing there and cite nothing from it."
    );
    match session.isolation {
        Isolation::Worktree => {
            let _ = writeln!(
                out,
                "- Mark `rerun: true` on commands that should give the same exit status when run again without your session's state (at most {MAX_RERUNS}). After your reply is accepted the widget may send you a short re-run brief: it runs those commands again in your pane and compares the exit status. Do not touch the worktree after writing your reply until the next brief."
            );
        }
        Isolation::Shared => out.push_str(
            "- Re-runs are off in shared mode; `rerun` is recorded but nothing is run again.\n",
        ),
    }
    let _ = writeln!(
        out,
        "- Limits: logs at most {} MiB, images and other files {} MiB, videos {} MiB; at most {MAX_EVIDENCE_ITEMS} items per reply.\n",
        mib(MAX_LOG_BYTES),
        mib(crate::model::types::MAX_IMAGE_BYTES),
        mib(crate::model::types::MAX_VIDEO_BYTES)
    );
}

const VERIFIER_RULES: [&str; 5] = [
    "Run things: a result rests on what you executed and saved, not on reading the code.",
    "Do not contact or type into other panes, and do not open other verifiers' worktrees or replies.",
    "Do not push, publish, open pull requests or change anything outside your worktree.",
    "Mark anything you could not verify as not_tested or blocked with the reason; never guess a pass.",
    "Write the reply in the language of the person's request (English when there is none); keep ids, field names and enum values as given.",
];

fn write_reply_format(out: &mut String, session: &Session, cross: bool, reply_path: &Path) {
    let _ = write!(
        out,
        "Write ONLY this JSON to the file:\n{}\n\
         Write the whole file in one go when you are done: the file appearing is how your reply is collected. Do not answer in the terminal.\n\n\
         {{ \"summary\": \"What I ran and what I found.\",\n\
         \x20 \"environment\": \"macOS 26.1, rustc 1.92, debug build\",\n\
         \x20 \"results\": [{{\"criterion\": \"AC-1\", \"result\": \"fail\", \"note\": \"New tab opens in $HOME after `cd /tmp`.\", \"evidence\": [\"e1\", \"e2\", \"e3\"]}}],\n\
         \x20 \"evidence\": [\n\
         \x20   {{\"id\": \"e1\", \"kind\": \"command\", \"cmd\": \"cargo test -p app cwd_inherit\", \"cwd\": \".\", \"exit\": 101, \"log\": \"logs/1.log\", \"rerun\": true}},\n\
         \x20   {{\"id\": \"e2\", \"kind\": \"file\", \"path\": \"shots/tab.png\", \"caption\": \"pwd in the new tab\"}},\n\
         \x20   {{\"id\": \"e3\", \"kind\": \"file\", \"path\": \"video/new-tab.mov\", \"caption\": \"cd /tmp, new tab, pwd\",\n\
         \x20    \"marks\": [{{\"t\": \"00:04\", \"note\": \"new tab opened\"}}, {{\"t\": \"00:06.5\", \"note\": \"it shows $HOME\"}}]}},\n\
         \x20   {{\"id\": \"e4\", \"kind\": \"observation\", \"text\": \"The tab bar flickers once.\"}}],\n\
         \x20 \"defects\": [{{\"id\": \"1\", \"claim\": \"one line\", \"severity\": \"high\", \"category\": \"falsify\", \"confidence\": \"high\",\n\
         \x20               \"location\": {{\"path\": \"src/a.rs\", \"line\": 40}}, \"repro\": \"1. … 2. …\", \"evidence\": [\"e4\"]}}]",
        reply_path.display()
    );
    if cross {
        out.push_str(
            ",\n  \"cross_checks\": [{\"subject\": \"codex/AC-2\", \"outcome\": \"not_reproduced\", \"note\": \"…\", \"evidence\": [\"e1\"]}]",
        );
    }
    out.push_str(" }\n\nRules:\n");
    if cross {
        out.push_str("- results: optional in this round; give one only to update your own result on a criterion, with new evidence\n");
    } else {
        out.push_str("- results: exactly one per criterion of section 3\n");
    }
    out.push_str("- result: pass (needs at least one command or file evidence), fail (needs evidence and a note), blocked or not_tested (need a note saying why)\n");
    let _ = writeln!(
        out,
        "- evidence ids: 1-{MAX_ID_LEN} letters, digits, '-' or '_', unique in the reply; every id cited exists and every item is cited at least once"
    );
    let _ = writeln!(
        out,
        "- command: cmd 1-{MAX_CMD} characters (several lines allowed), cwd relative to your worktree without \"..\", exit an integer, log the saved output; rerun true or false"
    );
    let _ = writeln!(
        out,
        "- paths (log, file): relative to your r<N>/<you>/ folder, no \"..\", no leading \"/\", not under {RESERVED_DIR}/; a regular file, not a link"
    );
    let _ = writeln!(
        out,
        "- file: an image (PNG, JPEG, GIF, WebP) or video (MP4, MOV, WebM) must have an extension matching its content and a caption of 1-{MAX_CAPTION} characters; marks only on a video, at most {MAX_MARKS}, t as mm:ss[.f] or hh:mm:ss[.f] in time order; shows (optional) lists the criteria a picture shows"
    );
    let _ = write!(
        out,
        "- defects (optional, at most {MAX_DEFECTS}): problems no criterion covers; each id new, claim one line at most {MAX_CLAIM} characters, severity critical/high/medium/low/info, confidence high/medium/low, repro required, evidence at least one id; category: "
    );
    out.push_str(&session.categories().join(", "));
    out.push('\n');
    if cross {
        out.push_str("- cross_checks: exactly one per assignment of section 6; outcome reproduced, not_reproduced or disputed; evidence from your own runs, required except for disputed with a note\n");
    }
}

/// Verifier `to`'s brief for `round`: blind in round 1, cross-checks
/// after. `reply_dir` is the verifier's reply dir, `worktree` where it
/// runs ([`crate::store::verifier_root`]).
pub fn verifier_brief(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
    worktree: &Path,
) -> Brief {
    let mut body = String::with_capacity(8192);
    let who = format!("verifier {to}");
    let part_dir = crate::store::part_dir(reply_dir, round, to);
    match kind_of(session, round) {
        RoundKind::Blind => {
            section_request(&mut body, BLIND_SECTIONS[0], session, round, &who);
            section_target(&mut body, BLIND_SECTIONS[1], session, worktree, reply_dir);
            let _ = writeln!(body, "{}", BLIND_SECTIONS[2]);
            write_criteria(&mut body, session);
            body.push('\n');
            section_strategy(&mut body, BLIND_SECTIONS[3], session, to);
            section_evidence(&mut body, BLIND_SECTIONS[4], session, &part_dir);
            let _ = writeln!(body, "{}", BLIND_SECTIONS[5]);
            body.push_str("- INDEPENDENT ROUND: verify on your own. You will see the other verifiers' results next round.\n");
            for rule in VERIFIER_RULES {
                let _ = writeln!(body, "- {rule}");
            }
            body.push('\n');
            section_interventions(&mut body, BLIND_SECTIONS[6], session, to, round);
            let _ = writeln!(body, "{}", BLIND_SECTIONS[7]);
            write_reply_format(&mut body, session, false, reply_path);
        }
        RoundKind::Cross => {
            section_request(&mut body, CROSS_SECTIONS[0], session, round, &who);
            section_target(&mut body, CROSS_SECTIONS[1], session, worktree, reply_dir);
            let _ = writeln!(body, "{}", CROSS_SECTIONS[2]);
            write_criteria(&mut body, session);
            body.push('\n');
            section_strategy(&mut body, CROSS_SECTIONS[3], session, to);
            let _ = writeln!(body, "{}", CROSS_SECTIONS[4]);
            write_standing(&mut body, session, round);
            let _ = writeln!(body, "{}", CROSS_SECTIONS[5]);
            let assigned = session
                .round(round)
                .and_then(|r| r.assignments.get(to))
                .cloned()
                .unwrap_or_default();
            if assigned.is_empty() {
                body.push_str(
                    "(none this round; you may still update your own results with new evidence)\n",
                );
            } else {
                body.push_str("Reproduce or refute each of these in your own worktree, with your own evidence:\n");
                for subject in &assigned {
                    write_assignment(&mut body, session, subject, round);
                }
            }
            body.push('\n');
            section_evidence(&mut body, CROSS_SECTIONS[6], session, &part_dir);
            body.push_str("Rules:\n");
            for rule in VERIFIER_RULES {
                let _ = writeln!(body, "- {rule}");
            }
            body.push('\n');
            section_interventions(&mut body, CROSS_SECTIONS[7], session, to, round);
            let _ = writeln!(body, "{}", CROSS_SECTIONS[8]);
            write_reply_format(&mut body, session, true, reply_path);
        }
    }
    Brief {
        to: to.clone(),
        kind: BriefKind::Work,
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// One assignment of a cross-check brief: the result with its note, or
/// the defect with its claim and repro steps.
fn write_assignment(out: &mut String, session: &Session, subject: &Subject, round: RoundNo) {
    let _ = write!(out, "- {subject}");
    match subject {
        Subject::Result { part, criterion } => {
            if let Some(r) = session.result_as_of(part, criterion, round.saturating_sub(1)) {
                let _ = write!(out, ": {} (R{})", r.outcome.name(), r.round);
                if !r.note.is_empty() {
                    let _ = write!(out, " — {}", r.note.trim());
                }
            }
            out.push('\n');
        }
        Subject::Defect { id } => match session.defect(id) {
            Some(d) => {
                let _ = writeln!(
                    out,
                    " [{}] {}\n  Repro: {}",
                    d.severity.name(),
                    d.claim,
                    d.repro.trim()
                );
            }
            None => out.push('\n'),
        },
    }
}

/// One evidence item as briefs list it: what it is, where its stored copy
/// lies, its digest and its re-run.
fn write_item(out: &mut String, session: &Session, key: &EvidenceKey, reply_dir: &Path) {
    let Some(item) = session.evidence_item(key) else {
        let _ = writeln!(out, "  - {key}: (unknown)");
        return;
    };
    let _ = write!(out, "  - {key}: ");
    match &item.body {
        EvidenceBody::Command {
            cmd,
            cwd,
            exit,
            rerun,
            ..
        } => {
            let lines = cmd.lines().count();
            if lines > 1 {
                let _ = write!(out, "command of {lines} lines (cwd {cwd}) exit {exit}");
            } else {
                let one = cmd.lines().next().unwrap_or_default();
                let _ = write!(out, "command `{one}` (cwd {cwd}) exit {exit}");
            }
            match &item.rerun {
                Some(r) => {
                    let _ = write!(out, "; re-run {}", rerun_text(r));
                }
                None if *rerun => out.push_str("; re-run pending"),
                None => {}
            }
            // A command of several lines is shown whole: its first line
            // alone may not say what it runs (`set -e`).
            if lines > 1 {
                out.push_str(&fenced(cmd, "    "));
            }
        }
        EvidenceBody::File {
            path,
            caption,
            media,
            marks,
            ..
        } => {
            match media {
                Some(m) => {
                    let _ = write!(
                        out,
                        "{} {} {path}",
                        m.name(),
                        if m.is_video() { "video" } else { "image" }
                    );
                }
                None => {
                    let _ = write!(out, "file {path}");
                }
            }
            if let Some(caption) = caption {
                let _ = write!(out, " — {caption:?}");
            }
            for mark in marks {
                let _ = write!(out, " [{} {}]", mark_text(mark.t_ms), mark.note);
            }
        }
        EvidenceBody::Observation { text } => {
            let _ = write!(out, "observation (unbacked): {}", text.trim());
        }
    }
    match &item.stored {
        Some(stored) => {
            let _ = write!(
                out,
                "\n    stored: {} ({} bytes, digest {:016x})",
                reply_dir.join(&stored.path).display(),
                stored.bytes,
                stored.digest
            );
        }
        None if item.file_path().is_some() => {
            out.push_str("\n    stored: missing (gone before ingest)")
        }
        None => {}
    }
    out.push('\n');
}

/// `text` as a fenced code block, each line after `indent`, starting with
/// a newline. The fence is longer than any run of backticks in `text`, so
/// no line of it ends the block early.
pub(crate) fn fenced(text: &str, indent: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let mut out = format!("\n{indent}{fence}");
    for line in text.lines() {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(indent);
            out.push_str(line);
        }
    }
    let _ = write!(out, "\n{indent}{fence}");
    out
}

/// `mm:ss.f` of a mark.
pub fn mark_text(t_ms: u64) -> String {
    let (s, ms) = (t_ms / 1000, t_ms % 1000);
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    let mut out = if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    };
    if ms > 0 {
        let frac = format!("{ms:03}");
        let _ = write!(out, ".{}", frac.trim_end_matches('0'));
    }
    out
}

fn rerun_text(rerun: &Rerun) -> String {
    match rerun {
        Rerun::Matched { exit, .. } => format!("matched (exit {exit})"),
        Rerun::Differed { exit: Some(e), .. } => format!("differed (exit {e})"),
        Rerun::Differed { exit: None, .. } => "differed (no exit status)".to_owned(),
        Rerun::TimedOut { .. } => "timed out".to_owned(),
        Rerun::NotRun { why, .. } => format!("not run: {why}"),
    }
}

/// Rulings and every result so far, with identities (round ≥ 2 briefs):
/// per criterion its rulings, every result and every cross-check on one,
/// each with the evidence it cites, and what the rulings cite besides (the
/// person's media, say); then each defect with its sources' evidence and
/// the cross-checks on it (L1-4 item 4; media wherever cited, L1-5a).
fn write_standing(out: &mut String, session: &Session, round: RoundNo) {
    let reply_dir = crate::store::reply_dir(session);
    if let Some((no, summary)) = session.summary_before(round) {
        let _ = writeln!(out, "The judge's summary of R{no}:\n{}\n", summary.trim());
    }
    for c in &session.criteria {
        let rulings: Vec<_> = session
            .rulings
            .get(&c.id)
            .into_iter()
            .flatten()
            .filter(|r| r.round < round)
            .collect();
        let ruling = rulings
            .last()
            .map(|r| format!("{} (R{}): {}", r.kind.name(), r.round, r.rationale.trim()))
            .unwrap_or_else(|| "not ruled".to_owned());
        let _ = writeln!(out, "### {} — {ruling}", c.id);
        // The earlier rulings: a later one does not take their reasons away.
        for r in &rulings[..rulings.len().saturating_sub(1)] {
            let _ = write!(out, "- judge R{}: {}", r.round, r.kind.name());
            if !r.rationale.trim().is_empty() {
                let _ = write!(out, " — {}", r.rationale.trim());
            }
            out.push('\n');
        }
        let mut listed: BTreeSet<&EvidenceKey> = BTreeSet::new();
        // Every result, not just the latest: an update leaves the earlier
        // one and its evidence readable (L1-4 item 4).
        for part in session.verifier_ids() {
            for r in session
                .results
                .iter()
                .filter(|r| &r.part == part && r.criterion == c.id && r.round < round)
            {
                let _ = write!(out, "- {part} R{}: {}", r.round, r.outcome.name());
                if !r.note.is_empty() {
                    let _ = write!(out, " — {}", r.note.trim());
                }
                out.push('\n');
                for key in &r.evidence {
                    write_item(out, session, key, &reply_dir);
                    listed.insert(key);
                }
            }
        }
        for check in session.cross_checks.iter().filter(|x| {
            x.round < round
                && matches!(&x.subject, Subject::Result { criterion, .. } if *criterion == c.id)
        }) {
            write_check(out, session, check, &reply_dir);
            listed.extend(&check.evidence);
        }
        let cited: Vec<&EvidenceKey> = rulings
            .iter()
            .flat_map(|r| &r.evidence)
            .filter(|k| listed.insert(k))
            .collect();
        if !cited.is_empty() {
            out.push_str("- also cited by the judge:\n");
            for key in cited {
                write_item(out, session, key, &reply_dir);
            }
        }
    }
    for d in &session.defects {
        let _ = writeln!(
            out,
            "### {} — {} · {} · {}: {}\nRepro: {}",
            d.id,
            d.status.name(),
            d.severity.name(),
            d.category,
            d.claim,
            d.repro.trim()
        );
        for raw in d.sources.iter().filter_map(|k| session.raw_defect(k)) {
            let _ = writeln!(out, "- {} (R{}): {}", raw.key, raw.round, raw.claim);
            for key in &raw.evidence {
                write_item(out, session, key, &reply_dir);
            }
        }
        for check in session.cross_checks.iter().filter(|x| {
            x.round < round && matches!(&x.subject, Subject::Defect { id } if *id == d.id)
        }) {
            write_check(out, session, check, &reply_dir);
        }
    }
    out.push('\n');
}

/// One earlier cross-check, with the evidence it cites.
fn write_check(out: &mut String, session: &Session, check: &CrossCheck, reply_dir: &Path) {
    let _ = write!(
        out,
        "- {} R{} cross-check on {}: {}",
        check.by,
        check.round,
        check.subject,
        check.outcome.name()
    );
    if !check.note.is_empty() {
        let _ = write!(out, " — {}", check.note.trim());
    }
    out.push('\n');
    for key in &check.evidence {
        write_item(out, session, key, reply_dir);
    }
}

/// The defects section of the ruling brief: this round's raw defects (each
/// with its evidence items, and the failing log tails they cite not shown
/// yet, `tailed`) and the defects so far.
fn section_defects<'a>(
    out: &mut String,
    heading: &str,
    session: &'a Session,
    round: RoundNo,
    reply_dir: &Path,
    tails: &BTreeMap<EvidenceKey, String>,
    tailed: &mut BTreeSet<&'a EvidenceKey>,
) {
    let _ = writeln!(out, "{heading}");
    let raws: Vec<_> = session
        .raw_defects
        .iter()
        .filter(|r| r.round == round && session.placement(&r.key).is_none())
        .collect();
    out.push_str("Raw defects this round (place each one):\n");
    if raws.is_empty() {
        out.push_str("(none)\n");
    }
    for raw in raws {
        let _ = write!(
            out,
            "### {} · {} · {} · {} · ",
            raw.key,
            raw.severity.name(),
            raw.category,
            raw.confidence.name()
        );
        match &raw.location {
            Some(loc) => {
                let _ = write!(out, "{loc}");
            }
            None => out.push_str("(whole target)"),
        }
        let _ = writeln!(
            out,
            "\nClaim: {}\nRepro: {}\nEvidence: {}",
            raw.claim,
            raw.repro.trim(),
            raw.evidence
                .iter()
                .map(EvidenceKey::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(fix) = &raw.fix {
            let _ = writeln!(out, "Fix: {}", fix.trim());
        }
        // Every item, whatever else cites it: media only a defect cites
        // reach the judge with their stored path and caption (L1-5a).
        for key in &raw.evidence {
            write_item(out, session, key, reply_dir);
            write_tail(out, key, tails, tailed);
        }
    }
    out.push_str("\nDefects so far:\n");
    if session.defects.is_empty() {
        out.push_str("(none)\n");
    }
    for d in &session.defects {
        let tally = session.tally(d);
        let _ = writeln!(
            out,
            "- {} [{}] {} · {} — {} (supported by {}, disputed by {})",
            d.id,
            d.status.name(),
            d.severity.name(),
            d.category,
            d.claim,
            tally.supporters.len(),
            tally.disputers.len()
        );
        // The earlier rounds' cross-checks on it, with their evidence: a
        // reproduction may back a failure (L1-6 rule 6) that a judge in a
        // new pane rules on. This round's are in the cross-checks section.
        for check in session.cross_checks.iter().filter(|x| {
            x.round < round && matches!(&x.subject, Subject::Defect { id } if *id == d.id)
        }) {
            write_check(out, session, check, reply_dir);
        }
    }
    out.push('\n');
}

/// `key`'s failing log tail, unless already shown.
fn write_tail<'a>(
    out: &mut String,
    key: &'a EvidenceKey,
    tails: &BTreeMap<EvidenceKey, String>,
    tailed: &mut BTreeSet<&'a EvidenceKey>,
) {
    if let Some(tail) = tails.get(key)
        && tailed.insert(key)
    {
        let _ = writeln!(out, "    log tail:\n```\n{}\n```", tail.trim_end());
    }
}

/// The judge's ruling brief for `round`. `tails` holds the last 40 lines
/// of each failing command's stored log, read by the runner.
pub fn judge_brief(
    session: &Session,
    round: RoundNo,
    reply_dir: &Path,
    reply_path: &Path,
    tails: &BTreeMap<EvidenceKey, String>,
) -> Brief {
    let mut body = String::with_capacity(16384);
    section_request(
        &mut body,
        RULING_SECTIONS[0],
        session,
        round,
        "the judge (you rule from the recorded evidence; you run nothing yourself)",
    );

    let _ = writeln!(body, "{}", RULING_SECTIONS[1]);
    write_criteria(&mut body, session);
    body.push('\n');

    let _ = writeln!(body, "{}", RULING_SECTIONS[2]);
    let _ = writeln!(
        body,
        "Isolation: {}",
        match session.isolation {
            Isolation::Worktree =>
                "one detached worktree per verifier; re-runs in each verifier's own pane",
            Isolation::Shared => "shared read-only checkout (isolation not enforced; no re-runs)",
        }
    );
    let this = session.round(round);
    for part in &session.verifiers {
        let strategy = part
            .strategy
            .as_ref()
            .map(strategy_text)
            .unwrap_or_default();
        let _ = write!(body, "- {} ({}): ", part.id, part.kind.command());
        match this
            .and_then(|r| r.brief(&part.id, BriefKind::Work))
            .map(|b| &b.receipt)
        {
            Some(r) if r.replied.is_some() => body.push_str("replied"),
            Some(r) => match &r.failed {
                Some(why) => {
                    let _ = write!(body, "no reply: {why}");
                }
                None => body.push_str("no reply"),
            },
            None => body.push_str("not briefed"),
        }
        let _ = writeln!(body, "\n  Strategy: {strategy}");
        if let Some(note) = this.and_then(|r| r.replies.get(&part.id)) {
            let _ = writeln!(body, "  Summary: {}", note.summary.trim());
            if !note.environment.is_empty() {
                let _ = writeln!(body, "  Environment: {}", note.environment.trim());
            }
        }
    }
    body.push('\n');

    let _ = writeln!(body, "{}", RULING_SECTIONS[3]);
    // Each failing log's tail once, under the first item citing it.
    let mut tailed: BTreeSet<&EvidenceKey> = BTreeSet::new();
    for c in &session.criteria {
        let _ = writeln!(body, "### {} [{}] {}", c.id, c.priority.name(), c.statement);
        let mut any = false;
        for r in session
            .results
            .iter()
            .filter(|r| r.round == round && r.criterion == c.id)
        {
            any = true;
            let _ = write!(body, "- {}: {}", r.part, r.outcome.name());
            if !r.note.is_empty() {
                let _ = write!(body, " — {}", r.note.trim());
            }
            body.push('\n');
            for key in &r.evidence {
                write_item(&mut body, session, key, reply_dir);
                write_tail(&mut body, key, tails, &mut tailed);
            }
        }
        if !any {
            body.push_str("- (no new result this round)\n");
        }
        // The criterion's history before this round (L1-6 rules 1–3): a
        // judge in a new pane rules from it, a ruling resting on an earlier
        // pass included.
        let earlier: Vec<_> = session
            .verifier_ids()
            .flat_map(|part| {
                session
                    .results
                    .iter()
                    .filter(move |r| &r.part == part && r.criterion == c.id && r.round < round)
            })
            .collect();
        let earlier_checks: Vec<_> = session
            .cross_checks
            .iter()
            .filter(|x| {
                x.round < round
                    && matches!(&x.subject, Subject::Result { criterion, .. } if *criterion == c.id)
            })
            .collect();
        if !earlier.is_empty() || !earlier_checks.is_empty() {
            body.push_str(
                "Earlier rounds on this criterion (the evidence standard reads these too):\n",
            );
        }
        for r in earlier {
            let _ = write!(body, "- {} R{}: {}", r.part, r.round, r.outcome.name());
            if !r.note.is_empty() {
                let _ = write!(body, " — {}", r.note.trim());
            }
            body.push('\n');
            for key in &r.evidence {
                write_item(&mut body, session, key, reply_dir);
            }
        }
        for check in earlier_checks {
            write_check(&mut body, session, check, reply_dir);
        }
        if let Some(prev) = session.ruling(&c.id) {
            let _ = writeln!(
                body,
                "Your ruling so far (R{}): {} — {}",
                prev.round,
                prev.kind.name(),
                prev.rationale.trim()
            );
        }
    }
    let attached: Vec<_> = session
        .evidence
        .iter()
        .filter(|e| e.attached.is_some())
        .collect();
    if !attached.is_empty() {
        body.push_str("\nMedia the person attached (cite as user/<n>):\n");
        for item in attached {
            write_item(&mut body, session, &item.key, reply_dir);
            if let EvidenceBody::File { shows, .. } = &item.body
                && !shows.is_empty()
            {
                let _ = writeln!(body, "    shows: {}", shows.join(", "));
            }
        }
    }
    body.push_str("\nYou may open images by their stored path when your CLI can read images. You cannot watch videos: a video's claim rests on its caption and marks. When a verified or failed ruling rests on video evidence alone (no command, image or other file among the stored evidence it cites), its rationale must name the caption and marks it relied on; an empty rationale there is refused.\n\n");

    let _ = writeln!(body, "{}", RULING_SECTIONS[4]);
    let checks: Vec<_> = session
        .cross_checks
        .iter()
        .filter(|c| c.round == round)
        .collect();
    if checks.is_empty() {
        body.push_str("(none this round)\n");
    }
    for c in checks {
        let _ = write!(body, "- {} on {}: {}", c.by, c.subject, c.outcome.name());
        if !c.note.is_empty() {
            let _ = write!(body, " — {}", c.note.trim());
        }
        body.push('\n');
        for key in &c.evidence {
            write_item(&mut body, session, key, reply_dir);
            write_tail(&mut body, key, tails, &mut tailed);
        }
    }
    body.push('\n');

    section_defects(
        &mut body,
        RULING_SECTIONS[5],
        session,
        round,
        reply_dir,
        tails,
        &mut tailed,
    );

    let _ = writeln!(body, "{}", RULING_SECTIONS[6]);
    for rule in EVIDENCE_STANDARD {
        let _ = writeln!(body, "- {rule}");
    }
    body.push_str("The widget enforces this standard: a ruling past it is turned down.\n\n");

    let _ = writeln!(body, "{}", RULING_SECTIONS[7]);
    section_interventions(
        &mut body,
        "User interventions:",
        session,
        &session.judge.id,
        round,
    );
    let last = session.is_last(round);
    if last {
        body.push_str(
            "This is the LAST round: \"verdict\" is required and no criterion may stay unruled.\n",
        );
    } else {
        let left = session.max_rounds - round;
        let _ = writeln!(
            body,
            "{left} round(s) left after this one. In \"assign\", give each verifier the results (<verifier>/<criterion>, never its own) and defects (D<n>) to reproduce or refute next round; with none, every fail and every pass resting on a single verifier is assigned round-robin. Leave \"verdict\" out unless you close."
        );
        if round >= 2 || session.max_rounds <= 1 {
            body.push_str(
                "You may set \"close\": true with a \"verdict\" if nothing is left to settle.\n",
            );
        } else {
            body.push_str("\"close\" is allowed in round 1 only when every criterion is verified by two or more verifiers with re-runs that matched, its ruling citing each one's matched command.\n");
        }
    }
    body.push('\n');

    let _ = writeln!(body, "{}", RULING_SECTIONS[8]);
    let next = session.next_defect_id();
    // "same" needs an earlier ruling: none exists in round 1.
    let second = if session.rulings.is_empty() {
        "{\"criterion\": \"AC-2\", \"ruling\": \"verified\", \"rationale\": \"…\", \"evidence\": [\"codex/r1/e2\", \"agy/r1/e1\"]}"
    } else {
        "{\"criterion\": \"AC-2\", \"ruling\": \"same\"}"
    };
    let verdict = if last {
        "  \"verdict\": {\"decision\": \"fail\", \"rationale\": \"…\", \"blocking\": [\"AC-1\", \"D1\"]},\n"
    } else {
        ""
    };
    let _ = write!(
        body,
        "Write ONLY this JSON to the file:\n{}\n\
         Write the whole file in one go when you are done. Do not answer in the terminal.\n\n\
         {{ \"summary\": \"what this round showed and what is still open\",\n\
         \x20 \"rulings\": [{{\"criterion\": \"AC-1\", \"ruling\": \"failed\", \"rationale\": \"…\", \"evidence\": [\"claude/r1/e1\"]}},\n\
         \x20              {second}],\n\
         \x20 \"defect_ops\": [{{\"op\": \"add\", \"id\": \"{next}\", \"from\": [\"claude/1\"], \"claim\": \"one line\", \"repro\": \"1. … 2. …\",\n\
         \x20                \"location\": {{\"path\": \"src/a.rs\", \"line\": 40}}, \"severity\": \"high\", \"category\": \"falsify\",\n\
         \x20                \"confidence\": \"high\", \"status\": \"open\", \"rationale\": \"…\", \"fix\": \"…\"}},\n\
         \x20               {{\"op\": \"merge\", \"id\": \"D1\", \"from\": [\"codex/2\"]}},\n\
         \x20               {{\"op\": \"update\", \"id\": \"D2\", \"severity\": \"medium\", \"status\": \"agreed\", \"rationale\": \"…\"}},\n\
         \x20               {{\"op\": \"dismiss\", \"raw\": \"agy/5\", \"reason\": \"…\"}}],\n\
         \x20 \"assign\": {{\"codex\": [\"claude/AC-1\"], \"claude\": [\"D1\"]}},\n\
         {verdict}\
         \x20 \"close\": false }}\n\nRules:\n",
        reply_path.display()
    );
    body.push_str("- rulings: one per criterion: verified, failed, inconclusive, not_applicable, or same (the previous one unchanged); evidence lists the keys it rests on\n");
    let _ = writeln!(
        body,
        "- defect ops: every raw defect of section 6 in exactly one add or merge \"from\", or one dismiss with a reason; new ids are free (the next is {next}); status open, agreed, disputed or rejected (rejected needs a rationale)"
    );
    body.push_str(
        "- severity: critical, high, medium, low, info; confidence: high, medium, low; category: ",
    );
    body.push_str(&session.categories().join(", "));
    body.push('\n');
    body.push_str("- assign: verifier ids: ");
    body.push_str(
        &session
            .verifier_ids()
            .map(PartId::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    );
    body.push_str("; values <verifier>/<criterion> of another verifier, or a live defect id\n");
    body.push_str("- verdict.decision: pass (every must verified, no live critical or high defect, no blocking), pass_with_issues (every must verified, or ruled not_applicable and listed in blocking; blocking lists every should criterion not verified, every must ruled not_applicable and every live defect), fail (some must failed; blocking non-empty), inconclusive (some must inconclusive, none failed); a should criterion ruled not_applicable holds nothing back\n");
    body.push_str("- verdict: only in the last round or with \"close\": true\n");

    let judge = session.judge.id.clone();
    Brief {
        to: judge,
        kind: BriefKind::Work,
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// Verifier `to`'s re-run brief for `round`: the one command line that
/// starts `script` (absolute) detached or in the foreground
/// ([`crate::rerun::detached`]), the reminder not to edit `rerun_dir` or
/// the worktree, and "reply nothing".
pub fn rerun_brief(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    script: &Path,
    rerun_dir: &Path,
) -> Brief {
    let detached = session
        .participant(to)
        .is_none_or(|p| crate::rerun::detached(p.kind));
    let quoted = shell_quote(&script.display().to_string());
    let mut body = String::with_capacity(1024);
    let _ = writeln!(body, "{}", RERUN_SECTIONS[0]);
    let _ = writeln!(
        body,
        "Round {round}: the widget runs the commands you marked rerun: true again, in this pane."
    );
    if detached {
        let _ = writeln!(
            body,
            "Run exactly this with your shell tool, detached so your tool's own time limit does not cut it short, and do nothing else until the next brief:\n\n    nohup sh {quoted} >/dev/null 2>&1 &\n"
        );
    } else {
        let _ = writeln!(
            body,
            "Run exactly this with your shell tool's longest time limit, and do nothing else until the next brief:\n\n    sh {quoted}\n"
        );
    }
    let _ = writeln!(
        body,
        "Do not edit any file under {} and do not touch your worktree. Reply nothing.",
        rerun_dir.display()
    );
    Brief {
        to: to.clone(),
        kind: BriefKind::Rerun,
        base_rev: session.rev,
        body: finish_body(body),
        reply_path: rerun_dir.to_owned(),
        receipt: Receipt::default(),
    }
}

/// `text` in single quotes for `sh`, a `'` inside written as `'\''`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// What an agent whose file was turned down is sent: the problems and
/// where to write the whole file again.
pub fn correction_note(to: &PartId, error: &str, reply_path: &Path) -> String {
    let what = if to.as_str() != JUDGE_ID {
        "reply JSON"
    } else if reply_path
        .ends_with(Path::new(crate::store::CRITERIA_DIR).join(crate::store::CRITERIA_FILE))
    {
        "criteria JSON"
    } else {
        "ruling JSON"
    };
    format!(
        "Your {what} could not be used:\n{error}\n\nFix it and write the whole file again at:\n{}\n",
        reply_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::Phase;
    use crate::testkit::{self, Rig, VERIFIERS, all_pass, reply_json};

    #[test]
    fn ac13_a_ruling_brief_lists_an_earlier_rounds_defect_reproduction_with_its_evidence() {
        let mut rig = Rig::new(3);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply(
            "claude",
            &format!(
                r#"{}, "defects": [{{"id": "1", "claim": "The store loses the last line", "severity": "high",
                   "category": "regression", "confidence": "high", "repro": "1. kill -9 mid-write\n2. reopen the store",
                   "evidence": ["e1"]}}]}}"#,
                all_pass().trim_end_matches('}')
            ),
        );
        rig.reply("codex", &all_pass());
        rig.reply("claude-2", &all_pass());
        rig.ingest();
        rig.rerun_all_matched();
        rig.judge(&format!(
            r#"{}, "defect_ops": [{{"op": "add", "id": "D1", "from": ["claude/1"], "claim": "The store loses the last line",
                 "repro": "1. kill -9 mid-write\n2. reopen the store", "severity": "high", "category": "regression",
                 "confidence": "high", "status": "open", "rationale": "Seen once."}}], "assign": {{"codex": ["D1"]}}}}"#,
            testkit::JUDGE_R1.trim_end().trim_end_matches('}')
        ));
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        // R2: only codex's reproduction of D1 cites the picture of AC-2.
        rig.deliver();
        rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
        rig.reply(
            "codex",
            &reply_json(
                &[],
                &[r#"{"id": "p1", "kind": "file", "path": "shots/empty.png", "caption": "the store is empty after reopen", "shows": ["AC-2"]}"#
                    .to_owned()],
                r#", "cross_checks": [{"subject": "D1", "outcome": "reproduced", "note": "lost here too", "evidence": ["p1"]}]"#,
            ),
        );
        rig.reply("claude-2", r#"{"summary": "Nothing assigned."}"#);
        rig.ingest();
        assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
        // A rationale that names neither the check nor the key.
        rig.judge(
            r#"{"summary": "s", "rulings": [
              {"criterion": "AC-1", "ruling": "same"},
              {"criterion": "AC-2", "ruling": "failed", "rationale": "The store is empty.", "evidence": ["codex/r2/p1"]},
              {"criterion": "AC-3", "ruling": "same"}]}"#,
        );
        assert_eq!(rig.round(), 3, "{:?}", rig.rejected("judge"));
        // R3 brings nothing new; the judge starts in a new pane.
        rig.deliver();
        for part in VERIFIERS {
            rig.reply(part, r#"{"summary": "Nothing assigned."}"#);
        }
        rig.ingest();
        assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("codex"));
        let brief = rig.brief("judge", BriefKind::Work);
        let defects = &brief[brief.find(RULING_SECTIONS[5]).unwrap()..];
        let defects = &defects[..defects.find(RULING_SECTIONS[6]).unwrap()];
        for line in [
            "- codex R2 cross-check on D1: reproduced — lost here too\n",
            "  - codex/r2/p1: PNG image shots/empty.png — \"the store is empty after reopen\"\n",
            "evidence/r2/codex/shots/empty.png (100 bytes, digest ",
        ] {
            assert!(defects.contains(line), "{line:?} in\n{defects}");
        }
    }
}
