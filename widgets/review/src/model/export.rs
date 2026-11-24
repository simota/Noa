//! The review as a Markdown report, for the clipboard, a saved file and
//! `<session dir>/report.md`.

use std::fmt::Write as _;

use super::types::{Finding, FindingStatus, Phase, Session, TargetKind, short_id};

/// The report's section headings, in order (after the title).
pub const SECTIONS: [&str; 7] = [
    "## 総合判定",
    "## 指摘一覧",
    "## 観点別",
    "## 対立している指摘",
    "## 却下・取り下げ・統合",
    "## カバレッジの欠落",
    "## ラウンド記録",
];

fn location(f: &Finding) -> String {
    let mut out = match &f.location {
        Some(loc) => format!("`{loc}`"),
        None => "（対象全体）".to_owned(),
    };
    if !f.in_target {
        out.push_str("（対象外）");
    } else if f.in_diff == Some(false) {
        out.push_str("（差分外）");
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Files no reviewer reported reviewing.
pub fn unreviewed_files(session: &Session) -> Vec<&str> {
    session
        .manifest
        .files
        .iter()
        .filter(|file| {
            !session.coverage.values().any(|c| {
                c.reviewed
                    .iter()
                    .any(|entry| super::types::covers(entry, &file.path))
            })
        })
        .map(|file| file.path.as_str())
        .collect()
}

/// Reviewers that failed a round, with the round and why.
pub fn failures(session: &Session) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for round in &session.rounds {
        for brief in &round.briefs {
            if let (Some(why), None) = (&brief.receipt.failed, brief.receipt.replied) {
                out.push((round.no, brief.to.to_string(), why.clone()));
            }
        }
    }
    out
}

pub fn report_markdown(session: &Session) -> String {
    let mut out = String::with_capacity(8192);
    // Live findings, ranked (L2-4): the findings list, the lenses and the
    // disputed section all go by them.
    let ranked: Vec<&Finding> = session
        .ranked()
        .into_iter()
        .filter(|f| f.status.is_live())
        .collect();
    write_head(&mut out, session);
    write_verdict(&mut out, session);
    write_findings(&mut out, session, &ranked);
    write_lenses(&mut out, session, &ranked);
    write_disputed(&mut out, session, &ranked);
    write_set_aside(&mut out, session);
    write_gaps(&mut out, session);
    write_rounds(&mut out, session);
    out
}

/// The title, and what was reviewed, by whom and how far.
fn write_head(out: &mut String, session: &Session) {
    let provisional = session.verdict().is_none() && !session.closed;
    let _ = writeln!(
        out,
        "# レビュー結果{}\n",
        if provisional { "（暫定）" } else { "" }
    );

    let m = &session.manifest;
    let _ = writeln!(
        out,
        "- 対象: {}（{}）",
        m.label,
        session.target.kind.label()
    );
    if let Some(repo) = &m.repo {
        let _ = writeln!(out, "- 場所: `{}`", repo.display());
    }
    if session.target.kind == TargetKind::Git {
        let short = |s: &Option<String>| {
            s.as_deref()
                .map_or_else(|| "?".to_owned(), |s| short_id(s, 10))
        };
        let _ = writeln!(
            out,
            "- 比較: {} `{}` … {} `{}`（merge-base `{}`）",
            m.base_ref.as_deref().unwrap_or("?"),
            short(&m.base_sha),
            m.head_ref.as_deref().unwrap_or("作業ツリー"),
            short(&m.head_sha),
            short(&m.merge_base),
        );
    }
    let _ = write!(out, "- ファイル: {}", m.files.len());
    if m.additions() > 0 || m.deletions() > 0 {
        let _ = write!(out, "（+{} −{}）", m.additions(), m.deletions());
    }
    if !session.target.paths.is_empty() {
        let _ = write!(out, "・絞り込み: {}", session.target.paths.join(" "));
    }
    out.push('\n');
    if !session.request.trim().is_empty() {
        let _ = writeln!(out, "- 依頼: {}", one_line(&session.request));
    }
    out.push_str("- パネル:\n");
    for part in &session.reviewers {
        let lens = part.lens.as_ref().map(|l| l.label()).unwrap_or_default();
        let _ = writeln!(
            out,
            "  - {} → {}（{}{}）",
            part.id,
            lens,
            part.kind.command(),
            part.setup
                .label()
                .map(|l| format!(" · {l}"))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(
        out,
        "  - リード: {}{}",
        session.lead.kind.command(),
        session
            .lead
            .setup
            .label()
            .map(|l| format!(" · {l}"))
            .unwrap_or_default()
    );
    let state = if session.closed {
        "終了"
    } else if session.waits_to_continue() {
        "停止中"
    } else {
        "進行中"
    };
    let _ = writeln!(
        out,
        "- ラウンド: {} / {}・{state}\n",
        session.rounds.len(),
        session.max_rounds
    );
}

/// 1. The verdict.
fn write_verdict(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[0]);
    match session.verdict() {
        Some(v) => {
            let _ = writeln!(
                out,
                "**{}**\n\n{}\n",
                v.decision.label(),
                v.rationale.trim()
            );
            if !v.must_fix.is_empty() {
                out.push_str("必ず直すもの:\n");
                for id in &v.must_fix {
                    let claim = session.finding(id).map(|f| f.claim.as_str()).unwrap_or("");
                    let _ = writeln!(out, "- **{id}** {claim}");
                }
                out.push('\n');
            }
        }
        None if session.closed => out.push_str("判定なし（ユーザーが終了）\n\n"),
        None => out.push_str("まだ判定はありません（最終ラウンドで出ます）。\n\n"),
    }
}

/// 2. The live findings, ranked.
fn write_findings(out: &mut String, session: &Session, ranked: &[&Finding]) {
    let _ = writeln!(out, "{}\n", SECTIONS[1]);
    if ranked.is_empty() {
        out.push_str("（指摘はありません）\n\n");
    }
    for f in ranked {
        let tally = session.tally(f);
        let _ = writeln!(out, "### {} [{}] {}\n", f.id, f.severity.label(), f.claim);
        let _ = writeln!(
            out,
            "- {} · {} · {}（{}）· {}",
            session.category_label(&f.category),
            f.status.label(),
            tally.label(),
            tally.agreement.label(),
            f.confidence.label()
        );
        let _ = writeln!(out, "- 場所: {}", location(f));
        let origins: Vec<String> = f.sources.iter().map(|k| k.to_string()).collect();
        let _ = writeln!(out, "- 出典: {}", origins.join(", "));
        let _ = writeln!(out, "- 判断: {}", one_line(&f.rationale));
        let evidence = f
            .sources
            .first()
            .and_then(|k| session.raw_finding(k))
            .map(|r| one_line(&r.evidence));
        if let Some(evidence) = evidence {
            let _ = writeln!(out, "- 根拠: {evidence}");
        }
        if let Some(fix) = f.fix.as_deref().or_else(|| {
            f.sources
                .iter()
                .find_map(|k| session.raw_finding(k).and_then(|r| r.fix.as_deref()))
        }) {
            let _ = writeln!(out, "- 修正案: {}", one_line(fix));
        }
        out.push('\n');
    }
}

/// 3. The findings per lens, with who holds it.
fn write_lenses(out: &mut String, session: &Session, ranked: &[&Finding]) {
    let _ = writeln!(out, "{}\n", SECTIONS[2]);
    let mut lenses: Vec<String> = session
        .panel_lenses()
        .iter()
        .map(|l| l.id().to_owned())
        .collect();
    for f in ranked {
        if !lenses.contains(&f.category) {
            lenses.push(f.category.clone());
        }
    }
    for id in &lenses {
        let holders: Vec<&str> = session
            .reviewers
            .iter()
            .filter(|p| p.lens.as_ref().is_some_and(|l| l.id() == id))
            .map(|p| p.id.as_str())
            .collect();
        let _ = write!(out, "### {}", session.category_label(id));
        if holders.is_empty() {
            out.push_str("（担当なし）");
        } else {
            let _ = write!(out, "（{}）", holders.join(", "));
        }
        out.push_str("\n\n");
        let mine: Vec<&&Finding> = ranked.iter().filter(|f| &f.category == id).collect();
        if mine.is_empty() {
            out.push_str("- 指摘なし\n");
        }
        for f in mine {
            let _ = writeln!(out, "- **{}** [{}] {}", f.id, f.severity.label(), f.claim);
        }
        out.push('\n');
    }
}

/// 4. Each disputed finding with every stance.
fn write_disputed(out: &mut String, session: &Session, ranked: &[&Finding]) {
    let _ = writeln!(out, "{}\n", SECTIONS[3]);
    let disputed: Vec<&&Finding> = ranked
        .iter()
        .filter(|f| f.status == FindingStatus::Disputed)
        .collect();
    if disputed.is_empty() {
        out.push_str("（ありません）\n\n");
    }
    for f in disputed {
        let _ = writeln!(out, "### {} {}\n", f.id, f.claim);
        for key in &f.sources {
            let _ = writeln!(
                out,
                "- {}: 提起（R{}）",
                key.part,
                session.raw_finding(key).map_or(0, |r| r.round)
            );
        }
        for (part, stances) in &f.stances {
            for s in stances {
                let _ = write!(out, "- {part}: {}（R{}）", s.kind.label(), s.round);
                if !s.note.is_empty() {
                    let _ = write!(out, " {}", one_line(&s.note));
                }
                out.push('\n');
            }
        }
        out.push('\n');
    }
}

/// 5. Rejected, withdrawn and merged findings, and dismissed raw ones.
fn write_set_aside(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[4]);
    let mut any = false;
    for f in session.findings.iter().filter(|f| !f.status.is_live()) {
        any = true;
        let _ = write!(out, "- **{}** [{}] {}", f.id, f.status.label(), f.claim);
        match f.status {
            FindingStatus::Merged => {
                if let Some(into) = &f.merged_into {
                    let _ = write!(out, " → {into}");
                }
            }
            FindingStatus::Rejected => {
                let _ = write!(out, " — {}", one_line(&f.rationale));
            }
            _ => {}
        }
        out.push('\n');
    }
    for d in &session.dismissed {
        any = true;
        let claim = session
            .raw_finding(&d.raw)
            .map(|r| r.claim.as_str())
            .unwrap_or("");
        let _ = writeln!(
            out,
            "- {}（R{} 退け）{claim} — {}",
            d.raw,
            d.round,
            one_line(&d.reason)
        );
    }
    if !any {
        out.push_str("（ありません）\n");
    }
    out.push('\n');
}

/// 6. What the review did not cover.
fn write_gaps(out: &mut String, session: &Session) {
    let m = &session.manifest;
    let _ = writeln!(out, "{}\n", SECTIONS[5]);
    let mut gaps = false;
    let unheld = session.unheld_lenses();
    if !unheld.is_empty() {
        gaps = true;
        let names: Vec<&str> = unheld.iter().map(|l| l.label()).collect();
        let _ = writeln!(out, "- 担当のいない観点: {}", names.join("、"));
    }
    let unreviewed = unreviewed_files(session);
    if !unreviewed.is_empty() {
        gaps = true;
        let _ = writeln!(
            out,
            "- どのレビュアーもレビュー済みと報告していないファイル: {}",
            unreviewed.join(", ")
        );
    }
    for o in &m.omitted {
        gaps = true;
        let _ = writeln!(out, "- スナップショット外: {}（{}）", o.path, o.reason);
    }
    for raw in session.unplaced() {
        gaps = true;
        let state = if session.closed {
            "取りまとめられずに終わった指摘"
        } else {
            "取りまとめ待ちの指摘"
        };
        let _ = writeln!(
            out,
            "- {state}: {}（R{}・{}）{}",
            raw.key,
            raw.round,
            raw.severity.label(),
            one_line(&raw.claim)
        );
    }
    for (round, part, why) in failures(session) {
        gaps = true;
        let _ = writeln!(out, "- R{round} で {part} が返答できませんでした: {why}");
    }
    if !gaps {
        out.push_str("（ありません）\n");
    }
    out.push('\n');
}

/// 7. Each round and the lead's summary of it.
fn write_rounds(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[6]);
    for round in &session.rounds {
        let _ = write!(out, "- R{}（{}）: ", round.no, round.kind.label());
        match (&round.outcome, round.phase) {
            (Some(outcome), _) => out.push_str(&one_line(&outcome.summary)),
            (None, Phase::Closed) => out.push_str("（取りまとめなし）"),
            (None, _) if session.closed => out.push_str("（中断）"),
            (None, _) => out.push_str("（進行中）"),
        }
        out.push('\n');
    }
}

/// The findings of `ids` that `session` has, ranked (L2-4); ids it does
/// not know are left out.
pub fn fix_findings<'a>(session: &'a Session, ids: &[String]) -> Vec<&'a Finding> {
    session
        .ranked()
        .into_iter()
        .filter(|f| ids.contains(&f.id))
        .collect()
}

/// What an agent asked to fix findings is given: how to work, what was
/// reviewed, then each finding of `ids` (ranked) with its location,
/// evidence, the suggested fix and any dispute. `snapshot` is where the
/// reviewed copy of the target lies.
pub fn fix_task(session: &Session, ids: &[String], snapshot: &std::path::Path) -> String {
    let findings = fix_findings(session, ids);
    let m = &session.manifest;
    let mut out = String::with_capacity(4096);
    let _ = writeln!(
        out,
        "# Fix request from review {}\n\n\
         A panel of coding agents reviewed this repository ({}), and a lead \
         consolidated their findings. Fix the {} finding{} below in this \
         working tree.\n",
        session.id,
        m.label,
        findings.len(),
        if findings.len() == 1 { "" } else { "s" },
    );
    out.push_str("## What was reviewed\n\n");
    if session.target.kind == TargetKind::Git {
        let short = |s: &Option<String>| {
            s.as_deref()
                .map_or_else(|| "?".to_owned(), |s| short_id(s, 10))
        };
        let _ = writeln!(
            out,
            "- The diff from {} `{}` (merge base `{}`) to {}.",
            m.base_ref.as_deref().unwrap_or("?"),
            short(&m.base_sha),
            short(&m.merge_base),
            match &m.head_ref {
                Some(head) => format!("{head} `{}`", short(&m.head_sha)),
                None => format!(
                    "the work tree (on `{}`, with its uncommitted and untracked changes)",
                    short(&m.head_sha)
                ),
            },
        );
        match &m.head_ref {
            Some(head) => {
                let _ = writeln!(
                    out,
                    "- The review read {head} at `{}`. If this working tree is not on that \
                     commit (`git rev-parse HEAD`), stop and ask before changing anything.",
                    short(&m.head_sha)
                );
            }
            None => out.push_str(
                "- The review read the work tree as it was when the review started; it may \
                 have changed since.\n",
            ),
        }
    } else {
        let _ = writeln!(out, "- {} files of this folder.", m.files.len());
    }
    let _ = writeln!(
        out,
        "- Line numbers refer to the reviewed copy in {}; check the current file before \
         you edit.\n",
        snapshot.display()
    );
    out.push_str(
        "## How to work\n\n\
         - Fix only the findings listed here.\n\
         - A finding marked disputed or open may not hold: check it first, and leave it \
         unfixed if it does not, saying why.\n\
         - Follow the repository's own rules (AGENTS.md, CLAUDE.md, CONTRIBUTING) and run \
         the checks that cover what you change.\n\
         - Do not push, publish, or open pull requests.\n\
         - End with a report: for each finding id, what you changed, or why you left it.\n\n",
    );
    if let Some(v) = session.verdict() {
        let _ = writeln!(
            out,
            "## The lead's verdict: {}\n\n{}\n",
            v.decision.name(),
            one_line(&v.rationale)
        );
        if !v.must_fix.is_empty() {
            let _ = writeln!(out, "Must fix: {}\n", v.must_fix.join(", "));
        }
    }
    out.push_str("## Findings\n\n");
    for f in &findings {
        let _ = writeln!(out, "### {} [{}] {}\n", f.id, f.severity.name(), f.claim);
        let tally = session.tally(f);
        let _ = writeln!(
            out,
            "- Lens: {} · status: {} · confidence: {} · supported by {}, disputed by {}",
            f.category,
            f.status.name(),
            f.confidence.name(),
            tally.supporters.len(),
            tally.disputers.len(),
        );
        let location = match &f.location {
            Some(loc) if !f.in_target => format!("`{loc}` (outside the reviewed target)"),
            Some(loc) if f.in_diff == Some(false) => format!("`{loc}` (outside the diff)"),
            Some(loc) => format!("`{loc}`"),
            None => "the target as a whole".to_owned(),
        };
        let _ = writeln!(out, "- Location: {location}");
        if !f.rationale.trim().is_empty() {
            let _ = writeln!(out, "- Lead: {}", one_line(&f.rationale));
        }
        for key in &f.sources {
            if let Some(raw) = session.raw_finding(key) {
                let _ = writeln!(out, "- Evidence ({key}): {}", one_line(&raw.evidence));
            }
        }
        if let Some(fix) = f.fix.as_deref().or_else(|| {
            f.sources
                .iter()
                .find_map(|k| session.raw_finding(k).and_then(|r| r.fix.as_deref()))
        }) {
            let _ = writeln!(out, "- Suggested fix: {}", one_line(fix));
        }
        for part in &tally.disputers {
            if let Some(s) = f.latest_stance(part) {
                let _ = writeln!(
                    out,
                    "- Disputed by {part} (R{}): {}",
                    s.round,
                    one_line(&s.note)
                );
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Input;
    use crate::model::types::{AgentKind, BuiltinLens};
    use crate::testkit::{self, Rig, blind, review, through_round_one};

    /// The body of `section` (up to the next heading) in `report`.
    fn section<'a>(report: &'a str, section: &str) -> &'a str {
        let start = report.find(section).unwrap() + section.len();
        let rest = &report[start..];
        rest[..rest.find("\n## ").unwrap_or(rest.len())].trim()
    }

    fn assert_sections_in_order(report: &str) {
        let mut at = 0;
        for heading in SECTIONS {
            let found = report[at..]
                .find(heading)
                .unwrap_or_else(|| panic!("{heading} missing"));
            at += found + heading.len();
        }
    }

    #[test]
    fn ac22_a_review_ended_by_the_user_before_any_finding() {
        let mut rig = Rig::new(3);
        rig.feed(Input::UserClose);
        let report = report_markdown(rig.engine.session());
        assert_sections_in_order(&report);
        assert!(report.starts_with("# レビュー結果\n"), "{report}");
        assert_eq!(section(&report, SECTIONS[0]), "判定なし（ユーザーが終了）");
        assert_eq!(section(&report, SECTIONS[1]), "（指摘はありません）");
        for empty in [SECTIONS[3], SECTIONS[4]] {
            assert_eq!(section(&report, empty), "（ありません）", "{empty}");
        }
        assert_eq!(
            section(&report, SECTIONS[6]),
            "- R1（独立レビュー）: （中断）"
        );
    }

    #[test]
    fn ac22_a_review_with_every_lens_held_and_every_file_covered_has_no_gaps() {
        let panel: Vec<_> = BuiltinLens::ALL
            .into_iter()
            .map(|lens| testkit::reviewer(AgentKind::Claude, lens))
            .collect();
        let mut rig = Rig::with(1, &panel);
        rig.deliver();
        let ids: Vec<String> = rig
            .engine
            .session()
            .reviewer_ids()
            .map(|p| p.to_string())
            .collect();
        for id in &ids {
            rig.reply(id, &blind(&[]));
        }
        rig.lead(r#"{"summary": "Nothing.", "verdict": {"decision": "approve", "rationale": "Clean.", "must_fix": []}}"#);
        let mut session = rig.engine.session().clone();
        assert!(session.closed && session.verdict().is_some());
        session.manifest.omitted.clear();
        let report = report_markdown(&session);
        assert_sections_in_order(&report);
        assert_eq!(section(&report, SECTIONS[5]), "（ありません）");
        assert!(section(&report, SECTIONS[0]).starts_with("**"), "{report}");
    }

    #[test]
    fn raw_findings_a_review_ended_on_before_the_lead_consolidated_them_are_reported() {
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
        rig.reply("agy", &blind(&[]));
        rig.feed(Input::UserClose);
        let s = rig.engine.session();
        assert_eq!(s.unplaced().len(), 2);
        let gaps = section(&report_markdown(s), SECTIONS[5]).to_owned();
        assert!(
            gaps.contains("- 取りまとめられずに終わった指摘: claude/1（R1・"),
            "{gaps}"
        );
        assert!(
            gaps.contains("- 取りまとめられずに終わった指摘: codex/1（R1・"),
            "{gaps}"
        );
    }

    #[test]
    fn ac22_an_extended_review_reports_as_provisional_again() {
        let mut rig = Rig::new(2);
        through_round_one(&mut rig);
        rig.deliver();
        for part in ["claude", "codex"] {
            rig.reply(part, &review(&[], &[("F2", "confirm", "")]));
        }
        rig.reply("agy", &review(&[], &[("F1", "confirm", "")]));
        rig.lead(
            r#"{"summary": "Done.", "ops": [
              {"op": "update", "id": "F1", "status": "agreed"},
              {"op": "update", "id": "F2", "status": "agreed"}
            ], "verdict": {"decision": "request_changes", "rationale": "F1.", "must_fix": ["F1"]}}"#,
        );
        assert!(report_markdown(rig.engine.session()).starts_with("# レビュー結果\n"));
        rig.feed(Input::UserExtend { by: 1, at: 500 });
        let report = report_markdown(rig.engine.session());
        assert!(report.starts_with("# レビュー結果（暫定）"), "{report}");
        assert_eq!(
            section(&report, SECTIONS[0]),
            "まだ判定はありません（最終ラウンドで出ます）。"
        );
        assert!(section(&report, SECTIONS[6]).contains("- R3（"), "{report}");
    }
}
