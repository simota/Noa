//! The review as a Markdown report, for the clipboard, a saved file and
//! `<session dir>/report.md`.

use std::fmt::Write as _;

use super::types::{Finding, FindingStatus, Phase, Session, TargetKind};

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
                .map(|s| &s[..s.len().min(10)])
                .unwrap_or("?")
                .to_owned()
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

    // 1. Verdict.
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

    // 2. Ranked findings.
    let _ = writeln!(out, "{}\n", SECTIONS[1]);
    let ranked: Vec<&Finding> = session
        .ranked()
        .into_iter()
        .filter(|f| f.status.is_live())
        .collect();
    if ranked.is_empty() {
        out.push_str("（指摘はありません）\n\n");
    }
    for f in &ranked {
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

    // 3. Per lens.
    let _ = writeln!(out, "{}\n", SECTIONS[2]);
    let mut lenses: Vec<String> = session
        .panel_lenses()
        .iter()
        .map(|l| l.id().to_owned())
        .collect();
    for f in &ranked {
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

    // 4. Disputed.
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

    // 5. Rejected, withdrawn, merged, dismissed.
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

    // 6. Coverage gaps.
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
    for (round, part, why) in failures(session) {
        gaps = true;
        let _ = writeln!(out, "- R{round} で {part} が返答できませんでした: {why}");
    }
    if !gaps {
        out.push_str("（ありません）\n");
    }
    out.push('\n');

    // 7. Rounds.
    let _ = writeln!(out, "{}\n", SECTIONS[6]);
    for round in &session.rounds {
        let _ = write!(out, "- R{}（{}）: ", round.no, round.kind.label());
        match (&round.outcome, round.phase) {
            (Some(outcome), _) => out.push_str(&one_line(&outcome.summary)),
            (None, Phase::Closed) => out.push_str("（取りまとめなし）"),
            (None, _) => out.push_str("（進行中）"),
        }
        out.push('\n');
    }
    out
}
