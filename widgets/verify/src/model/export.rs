//! The verification as a Markdown report (L1-8), for the clipboard, a
//! saved file and `<session dir>/report.md`, and the task 修正を依頼 hands
//! an agent (L1-7 item 3).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::model::rules::History;
use crate::model::types::{
    CRITERIA_ROUND, CriteriaEdit, CrossOutcome, Defect, Evidence, EvidenceBody, EvidenceKey,
    Isolation, Outcome, Phase, Rerun, RulingKind, Session, Subject, TargetKind, short_id,
};

/// The report's section headings, in order (after the title).
pub const SECTIONS: [&str; 8] = [
    "## 総合判定",
    "## 受け入れ基準",
    "## 不具合",
    "## 追試",
    "## 証跡一覧",
    "## 基準の変更",
    "## 検証の欠落",
    "## ラウンド記録",
];

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text for a Markdown table cell.
fn cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// `path` percent-encoded per component, `/` kept (L1-8): the bytes of a
/// component outside `A-Z a-z 0-9 - . _ ~` become `%XX`.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Text for a Markdown link label (`[label](…)`): `\`, `[` and `]`
/// escaped, so a caption holding a bracket does not end the label early.
fn label(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Where a stored copy is linked: its absolute path into `base` (the
/// judge's reply dir), or its path relative to the reply dir.
fn link(base: Option<&Path>, stored: &Path) -> String {
    match base {
        Some(base) => encode_path(&base.join(stored).to_string_lossy()),
        None => encode_path(&stored.to_string_lossy()),
    }
}

fn mark_text(t_ms: u64) -> String {
    crate::model::brief::mark_text(t_ms)
}

/// Every brief that gave up: `(round, participant, why)`.
pub fn failures(session: &Session) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    if let Some(brief) = &session.criteria_brief
        && let (Some(why), None) = (&brief.receipt.failed, brief.receipt.replied)
    {
        out.push((CRITERIA_ROUND, brief.to.to_string(), why.clone()));
    }
    for round in &session.rounds {
        for brief in round
            .briefs
            .iter()
            .chain(&round.rerun_briefs)
            .chain(&round.judge_brief)
        {
            if let (Some(why), None) = (&brief.receipt.failed, brief.receipt.replied) {
                out.push((round.no, brief.to.to_string(), why.clone()));
            }
        }
    }
    out
}

/// The report. Media are linked by absolute path into `base` (the judge's
/// reply dir) when given, by their path relative to it when `None` (証跡ごと
/// 書き出す).
pub fn report_markdown(session: &Session, base: Option<&Path>) -> String {
    let mut out = String::with_capacity(16384);
    if let Some(base) = base {
        let _ = writeln!(
            out,
            "> 証跡のリンク先: `{}`（このマシン上で、このフォルダがある間だけ開けます）\n",
            base.display()
        );
    }
    write_head(&mut out, session);
    write_verdict(&mut out, session);
    write_criteria(&mut out, session, base);
    write_defects(&mut out, session, base);
    write_cross_checks(&mut out, session);
    write_evidence(&mut out, session);
    write_changes(&mut out, session);
    write_gaps(&mut out, session);
    write_rounds(&mut out, session);
    out
}

fn write_head(out: &mut String, session: &Session) {
    let provisional = session.verdict().is_none() && !session.closed;
    let _ = writeln!(
        out,
        "# 検証結果{}\n",
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
    let short = |s: &Option<String>| {
        s.as_deref()
            .map_or_else(|| "?".to_owned(), |s| short_id(s, 10))
    };
    match session.target.kind {
        TargetKind::Diff => {
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
        TargetKind::State => {
            let _ = writeln!(
                out,
                "- 状態: {} `{}`",
                m.head_ref.as_deref().unwrap_or("作業ツリー"),
                short(&m.head_sha)
            );
        }
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
    if let Some(spec) = &session.spec {
        let _ = writeln!(out, "- 仕様: `{}`（{} 行）", spec.path, spec.lines);
    }
    let _ = write!(out, "- 隔離: {}", session.isolation.label());
    if session.isolation == Isolation::Shared {
        out.push_str("（隔離は強制されず、再実行もしていません）");
    }
    out.push('\n');
    out.push_str("- パネル:\n");
    for part in &session.verifiers {
        let strategy = part
            .strategy
            .as_ref()
            .map(|s| s.label())
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  - {} → {}（{}{}）",
            part.id,
            strategy,
            part.kind.command(),
            part.setup
                .label()
                .map(|l| format!(" · {l}"))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(
        out,
        "  - 判定役: {}{}",
        session.judge.kind.command(),
        session
            .judge
            .setup
            .label()
            .map(|l| format!(" · {l}"))
            .unwrap_or_default()
    );
    let state = if session.closed {
        "終了"
    } else if session.waits_to_continue() {
        "停止中"
    } else if session.phase() == Phase::Closed {
        // A ruled round of a verification that goes on (one extended past
        // its limit, say): the next round is being prepared, its worktrees
        // made or rebuilt, as before round 1.
        Phase::Briefing.label()
    } else {
        session.phase().label()
    };
    let _ = writeln!(
        out,
        "- ラウンド: {} / {}・{state}\n",
        session.rounds.len(),
        session.max_rounds
    );
}

/// What a blocking id names: a criterion's statement or a defect's claim.
fn named(session: &Session, id: &str) -> String {
    session
        .criterion(id)
        .map(|c| c.statement.clone())
        .or_else(|| session.defect(id).map(|d| d.claim.clone()))
        .unwrap_or_default()
}

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
            if !v.blocking.is_empty() {
                out.push_str("止めているもの:\n");
                for id in &v.blocking {
                    let _ = writeln!(out, "- **{id}** {}", named(session, id));
                }
                out.push('\n');
            }
        }
        None if session.closed => out.push_str("判定なし（ユーザーが終了）\n\n"),
        None => out.push_str("まだ判定はありません（最終ラウンドで出ます）。\n\n"),
    }
}

/// The re-run mark of a result: ↻ when a command it cites matched, ≠ when
/// one differed (or timed out), nothing otherwise.
fn rerun_mark(session: &Session, evidence: &[EvidenceKey]) -> &'static str {
    let reruns: Vec<&Rerun> = evidence
        .iter()
        .filter_map(|k| session.evidence_item(k)?.rerun.as_ref())
        .collect();
    if reruns
        .iter()
        .any(|r| matches!(r, Rerun::Differed { .. } | Rerun::TimedOut { .. }))
    {
        "≠"
    } else if reruns.iter().any(|r| matches!(r, Rerun::Matched { .. })) {
        "↻"
    } else {
        ""
    }
}

/// The media shown under `criterion`: those its ruling, its results or the
/// cross-checks on its results cite (unless their `shows` leaves it out)
/// and those the person attached for it, each once, in evidence order.
fn media_for<'a>(session: &'a Session, criterion: &str) -> Vec<&'a Evidence> {
    let mut cited: Vec<&EvidenceKey> = session
        .results
        .iter()
        .filter(|r| r.criterion == criterion)
        .flat_map(|r| &r.evidence)
        .collect();
    cited.extend(
        session
            .cross_checks
            .iter()
            .filter(
                |c| matches!(&c.subject, Subject::Result { criterion: on, .. } if on == criterion),
            )
            .flat_map(|c| &c.evidence),
    );
    if let Some(rulings) = session.rulings.get(criterion) {
        cited.extend(rulings.iter().flat_map(|r| &r.evidence));
    }
    session
        .evidence
        .iter()
        .filter(|e| e.media().is_some() && e.stored.is_some())
        .filter(|e| {
            let EvidenceBody::File { shows, .. } = &e.body else {
                return false;
            };
            let named = shows.iter().any(|s| s == criterion);
            if e.attached.is_some() {
                named
            } else {
                cited.contains(&&e.key) && (shows.is_empty() || named)
            }
        })
        .collect()
}

/// The media shown under defect `defect`: those its raw defects and the
/// cross-checks on it or on a defect merged into it cite (a merge moves
/// the raw defects, and the checks on them count with them, as in
/// [`Session::tally`]), stored, each once, in evidence order. A defect's
/// evidence is its own whatever criteria a `shows` names.
fn media_for_defect<'a>(session: &'a Session, defect: &Defect) -> Vec<&'a Evidence> {
    let ids = session.merged_ids(defect);
    let mut cited: Vec<&EvidenceKey> = defect
        .sources
        .iter()
        .filter_map(|k| session.raw_defect(k))
        .flat_map(|r| &r.evidence)
        .collect();
    cited.extend(
        session
            .cross_checks
            .iter()
            .filter(|c| matches!(&c.subject, Subject::Defect { id } if ids.contains(id.as_str())))
            .flat_map(|c| &c.evidence),
    );
    session
        .evidence
        .iter()
        .filter(|e| e.media().is_some() && e.stored.is_some() && cited.contains(&&e.key))
        .collect()
}

/// `media` as a nested `- スクショ・動画:` list (images embedded, videos
/// linked with their marks).
fn write_media_list(out: &mut String, media: &[&Evidence], base: Option<&Path>) {
    if media.is_empty() {
        return;
    }
    out.push_str("- スクショ・動画:\n");
    let mut nested = String::new();
    for item in media {
        write_media(&mut nested, item, base);
    }
    for line in nested.lines() {
        let _ = writeln!(out, "  {line}");
    }
}

/// One evidence item as the text of a list entry whose marker sits after
/// `indent`: on its first line what it is, its exit status and re-run,
/// digest, size and stored path; a command of several lines follows whole,
/// as a code block inside the entry.
fn item_entry(item: &Evidence, indent: &str) -> String {
    let mut out = format!("`{}` ", item.key);
    let mut block = None;
    match &item.body {
        EvidenceBody::Command { cmd, cwd, exit, .. } => {
            let lines = cmd.lines().count();
            if lines > 1 {
                let _ = write!(out, "コマンド {lines} 行（{cwd}） · 終了コード {exit}");
                // Its first line alone may not say what it runs (`set -e`).
                block = Some(crate::model::brief::fenced(cmd, &format!("{indent}  ")));
            } else {
                let one = cmd.lines().next().unwrap_or_default();
                let _ = write!(out, "コマンド `{one}`（{cwd}） · 終了コード {exit}");
            }
            if let Some(r) = &item.rerun {
                let _ = write!(out, " · 再実行 {}", rerun_label(r));
            }
        }
        EvidenceBody::File {
            path,
            caption,
            media,
            ..
        } => {
            match media {
                Some(m) => {
                    let _ = write!(out, "{}（{}）`{path}`", m.label(), m.name());
                }
                None => {
                    let _ = write!(out, "ファイル `{path}`");
                }
            }
            if let Some(caption) = caption {
                let _ = write!(out, "「{}」", one_line(caption));
            }
        }
        EvidenceBody::Observation { text } => {
            let _ = write!(out, "観察（未裏付け）: {}", one_line(text));
        }
    }
    if item.attached.is_some() {
        out.push_str("（あなたが添付）");
    }
    match &item.stored {
        Some(s) => {
            let _ = write!(
                out,
                " · digest `{:016x}` · {} バイト · `{}`",
                s.digest,
                s.bytes,
                s.path.display()
            );
        }
        None if item.file_path().is_some() => out.push_str(" · 保存なし"),
        None => {}
    }
    if let Some(block) = block {
        out.push_str(&block);
    }
    out
}

fn rerun_label(rerun: &Rerun) -> String {
    match rerun {
        Rerun::Matched { exit, .. } => format!("一致（{exit}）"),
        Rerun::Differed { exit: Some(e), .. } => format!("不一致（{e}）"),
        Rerun::Differed { exit: None, .. } => "不一致（終了コードなし）".to_owned(),
        Rerun::TimedOut { .. } => "時間切れ".to_owned(),
        Rerun::NotRun { why, .. } => format!("未実行（{}）", one_line(why)),
    }
}

fn write_media(out: &mut String, item: &Evidence, base: Option<&Path>) {
    let (Some(media), Some(stored), EvidenceBody::File { caption, marks, .. }) =
        (item.media(), &item.stored, &item.body)
    else {
        return;
    };
    let caption = label(&caption.as_deref().map(one_line).unwrap_or_default());
    let who = if item.attached.is_some() {
        "（あなたが添付）"
    } else {
        ""
    };
    let target = link(base, &stored.path);
    if media.is_video() {
        let _ = writeln!(out, "- ▶ [{caption}]({target}){who}");
        for mark in marks {
            let _ = writeln!(out, "  - {} {}", mark_text(mark.t_ms), one_line(&mark.note));
        }
    } else {
        let _ = writeln!(out, "- ![{caption}]({target}){who}");
    }
}

fn write_criteria(out: &mut String, session: &Session, base: Option<&Path>) {
    let _ = writeln!(out, "{}\n", SECTIONS[1]);
    if session.criteria.is_empty() {
        out.push_str("（基準はまだありません）\n\n");
        return;
    }
    let _ = write!(out, "| ID | 優先度 | 基準 | 判定 |");
    for part in session.verifier_ids() {
        let _ = write!(out, " {part} |");
    }
    out.push_str("\n|---|---|---|---|");
    for _ in session.verifier_ids() {
        out.push_str("---|");
    }
    out.push('\n');
    for c in &session.criteria {
        let ruling = session
            .ruling(&c.id)
            .map(|r| r.kind.label())
            .unwrap_or("未判定");
        let _ = write!(
            out,
            "| {} | {} | {} | {ruling} |",
            c.id,
            c.priority.label(),
            cell(&c.statement)
        );
        for part in session.verifier_ids() {
            match session.latest_result(part, &c.id) {
                Some(r) => {
                    let _ = write!(
                        out,
                        " {}{} |",
                        r.outcome.mark(),
                        rerun_mark(session, &r.evidence)
                    );
                }
                None => out.push_str("  |"),
            }
        }
        out.push('\n');
    }
    out.push('\n');
    for c in &session.criteria {
        let _ = writeln!(out, "### {} {}\n", c.id, c.statement);
        match session.ruling(&c.id) {
            Some(r) => {
                let _ = writeln!(
                    out,
                    "- 判定: {}（R{}）{}",
                    r.kind.label(),
                    r.round,
                    if r.rationale.trim().is_empty() {
                        String::new()
                    } else {
                        format!("— {}", one_line(&r.rationale))
                    }
                );
                for key in &r.evidence {
                    if let Some(item) = session.evidence_item(key) {
                        let _ = writeln!(out, "  - {}", item_entry(item, "  "));
                    }
                }
            }
            None => out.push_str("- 判定: 未判定\n"),
        }
        for part in session.verifier_ids() {
            if let Some(r) = session.latest_result(part, &c.id) {
                let _ = write!(out, "- {part}（R{}）: {}", r.round, r.outcome.label());
                if !r.note.is_empty() {
                    let _ = write!(out, " — {}", one_line(&r.note));
                }
                out.push('\n');
            }
        }
        write_media_list(out, &media_for(session, &c.id), base);
        out.push('\n');
    }
}

fn write_defects(out: &mut String, session: &Session, base: Option<&Path>) {
    let _ = writeln!(out, "{}\n", SECTIONS[2]);
    let live: Vec<&Defect> = session
        .ranked()
        .into_iter()
        .filter(|d| d.status.is_live())
        .collect();
    if live.is_empty() {
        out.push_str("（不具合はありません）\n\n");
    }
    for d in live {
        let tally = session.tally(d);
        let _ = writeln!(out, "### {} [{}] {}\n", d.id, d.severity.label(), d.claim);
        let _ = writeln!(
            out,
            "- {} · {} · {}（{}）· {}",
            session.category_label(&d.category),
            d.status.label(),
            tally.label(),
            tally.agreement.label(),
            d.confidence.label()
        );
        let location = match &d.location {
            Some(loc) if !d.in_target => format!("`{loc}`（対象外）"),
            Some(loc) if d.in_diff == Some(false) => format!("`{loc}`（差分外）"),
            Some(loc) => format!("`{loc}`"),
            None => "（対象全体）".to_owned(),
        };
        let _ = writeln!(out, "- 場所: {location}");
        let origins: Vec<String> = d.sources.iter().map(|k| k.to_string()).collect();
        let _ = writeln!(out, "- 出典: {}", origins.join(", "));
        let _ = writeln!(out, "- 判断: {}", one_line(&d.rationale));
        let _ = writeln!(out, "- 再現手順: {}", one_line(&d.repro));
        let evidence: Vec<&str> = d
            .sources
            .iter()
            .filter_map(|k| session.raw_defect(k))
            .flat_map(|r| r.evidence.iter().map(EvidenceKey::as_str))
            .collect();
        if !evidence.is_empty() {
            let _ = writeln!(out, "- 証跡: {}", evidence.join(", "));
        }
        if let Some(fix) = d.fix.as_deref().or_else(|| {
            d.sources
                .iter()
                .find_map(|k| session.raw_defect(k).and_then(|r| r.fix.as_deref()))
        }) {
            let _ = writeln!(out, "- 修正案: {}", one_line(fix));
        }
        write_media_list(out, &media_for_defect(session, d), base);
        out.push('\n');
    }
}

fn write_cross_checks(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[3]);
    if session.cross_checks.is_empty() {
        out.push_str("（ありません）\n");
    }
    for c in &session.cross_checks {
        let _ = write!(
            out,
            "- R{} {} → {}: {}",
            c.round,
            c.by,
            c.subject,
            c.outcome.label()
        );
        if !c.note.is_empty() {
            let _ = write!(out, " — {}", one_line(&c.note));
        }
        out.push('\n');
    }
    out.push('\n');
}

fn write_evidence(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[4]);
    if session.evidence.is_empty() {
        out.push_str("（ありません）\n\n");
        return;
    }
    let flagged = |e: &&Evidence| {
        matches!(
            e.rerun,
            Some(Rerun::Differed { .. } | Rerun::TimedOut { .. })
        )
    };
    // Where the re-runs below ran, said once (L1-5 Trust), and only for
    // re-runs that did run: an item marked for one may have none yet, or
    // never get one.
    let marked: Vec<&Evidence> = session
        .evidence
        .iter()
        .filter(|e| matches!(e.body, EvidenceBody::Command { rerun: true, .. }))
        .collect();
    let ran = marked.iter().any(|e| {
        matches!(
            e.rerun,
            Some(Rerun::Matched { .. } | Rerun::Differed { .. } | Rerun::TimedOut { .. })
        )
    });
    let pending = !session.closed && marked.iter().any(|e| e.rerun.is_none());
    if !marked.is_empty() {
        out.push_str(match session.isolation {
            Isolation::Worktree if ran => {
                "再実行は各検証者自身のペインで、その検証者のエージェントがウィジェットの書いたスクリプトを起動して行いました。同じ環境で同じ終了コードが出るかの確認で、別の検証者による独立した追試ではありません（追試は「## 追試」）。\n\n"
            }
            Isolation::Worktree if pending => {
                "再実行はまだ一つも行われていません（各検証者自身のペインで、その検証者のエージェントが起動します）。\n\n"
            }
            Isolation::Worktree => {
                "再実行は一つも行われませんでした（`rerun: true` の項目はすべて未実行、理由は「## 検証の欠落」）。\n\n"
            }
            Isolation::Shared => {
                "共有（読み取りのみ）モードのため、再実行は一つも行っていません（`rerun: true` の項目はすべて未実行）。\n\n"
            }
        });
    }
    let (first, rest): (Vec<&Evidence>, Vec<&Evidence>) =
        session.evidence.iter().partition(flagged);
    for item in first.into_iter().chain(rest) {
        let _ = writeln!(out, "- {}", item_entry(item, ""));
        if let Some(log) = item.rerun.as_ref().and_then(rerun_log) {
            let _ = writeln!(
                out,
                "  - 再実行のログ: digest `{:016x}` · {} バイト · `{}`",
                log.digest,
                log.bytes,
                log.path.display()
            );
        }
    }
    out.push('\n');
}

fn rerun_log(rerun: &Rerun) -> Option<&crate::model::types::Stored> {
    match rerun {
        Rerun::Matched { log, .. }
        | Rerun::Differed { log, .. }
        | Rerun::TimedOut { log }
        | Rerun::NotRun { log, .. } => log.as_ref(),
    }
}

fn write_changes(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[5]);
    let mut any = false;
    // An added criterion gets one past every number used (`reduce`), so the
    // person's additions hold the highest numbers.
    let adds = session
        .criteria_edits
        .iter()
        .filter(|(e, _)| matches!(e, CriteriaEdit::Add { .. }))
        .count();
    let mut ids: Vec<&str> = session
        .criteria
        .iter()
        .chain(&session.removed_criteria)
        .map(|c| c.id.as_str())
        .collect();
    ids.sort_by(|a, b| crate::model::types::natural_cmp(b, a));
    let added: Vec<&str> = ids.into_iter().take(adds).collect();
    for c in &session.criteria {
        let what = if added.contains(&c.id.as_str()) {
            "追加"
        } else if c.edited {
            "編集"
        } else {
            continue;
        };
        any = true;
        let _ = writeln!(out, "- {what}: {} {}", c.id, c.statement);
    }
    for c in &session.removed_criteria {
        any = true;
        let _ = writeln!(out, "- 削除: {} {}（{}）", c.id, c.statement, c.source);
    }
    if session
        .criteria_edits
        .iter()
        .any(|(e, _)| matches!(e, CriteriaEdit::Move { .. }))
    {
        any = true;
        let order: Vec<&str> = session.criteria.iter().map(|c| c.id.as_str()).collect();
        let _ = writeln!(out, "- 並べ替え: {}", order.join(", "));
    }
    for c in &session.criteria {
        if let Some(r) = session.ruling(&c.id)
            && r.kind == RulingKind::NotApplicable
        {
            any = true;
            let _ = writeln!(
                out,
                "- 対象外と判定: {} {} — {}",
                c.id,
                c.statement,
                one_line(&r.rationale)
            );
        }
    }
    if !any {
        out.push_str("（ありません）\n");
    }
    out.push('\n');
}

fn write_gaps(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[6]);
    let mut gaps = false;
    // Over every round, as the evidence standard reads a criterion (L1-6):
    // a later blocked or not_tested result leaves an earlier pass or fail
    // in its history. The matrix above shows only the latest.
    let untested: Vec<&str> = session
        .criteria
        .iter()
        .filter(|c| {
            !History::of(session, &c.id)
                .results
                .iter()
                .any(|r| matches!(r.outcome, Outcome::Pass | Outcome::Fail))
        })
        .map(|c| c.id.as_str())
        .collect();
    // While the criteria wait to be confirmed no result is due yet; once
    // the verification is over (also when it ended there) none will come.
    if !untested.is_empty() && (session.frozen_at.is_some() || session.closed) {
        gaps = true;
        let _ = writeln!(out, "- 成功・失敗の結果がない基準: {}", untested.join(", "));
    }
    for (round, part, why) in failures(session) {
        gaps = true;
        let when = if round == CRITERIA_ROUND {
            "基準策定".to_owned()
        } else {
            format!("R{round}")
        };
        let _ = writeln!(
            out,
            "- {when} で {part} が返答できませんでした: {}",
            one_line(&why)
        );
    }
    // Every round's, not just the latest: a later update leaves the reason
    // it could not run before (L1-8 item 8).
    for part in session.verifier_ids() {
        for c in &session.criteria {
            for r in session
                .results
                .iter()
                .filter(|r| &r.part == part && r.criterion == c.id && r.outcome == Outcome::Blocked)
            {
                gaps = true;
                let _ = writeln!(
                    out,
                    "- {} を {part} が実行できませんでした（R{}）: {}",
                    c.id,
                    r.round,
                    one_line(&r.note)
                );
            }
        }
    }
    for item in &session.evidence {
        if let Some(Rerun::NotRun { why, .. }) = &item.rerun {
            gaps = true;
            let _ = writeln!(
                out,
                "- 再実行されなかったコマンド: `{}` — {}",
                item.key,
                one_line(why)
            );
        }
        // Marked for a re-run that the close came before.
        if session.closed
            && item.rerun.is_none()
            && matches!(item.body, EvidenceBody::Command { rerun: true, .. })
        {
            gaps = true;
            let _ = writeln!(
                out,
                "- 再実行されなかったコマンド: `{}` — 再実行の前に検証が終了しました",
                item.key
            );
        }
        if item.stored.is_none()
            && item.file_path().is_some()
            && item.attached.is_none()
            && item
                .key
                .split()
                .and_then(|(p, r, _)| session.round(r).map(|round| round.ingested.contains(&p)))
                .unwrap_or(false)
        {
            gaps = true;
            let _ = writeln!(
                out,
                "- 取り込み前に消えた証跡: `{}`（{}）",
                item.key,
                item.file_path().unwrap_or_default()
            );
        }
    }
    if session.isolation == Isolation::Shared {
        gaps = true;
        out.push_str("- 共有（読み取りのみ）モード: 隔離は強制されず、再実行もしていません\n");
    }
    for r in &session.results {
        let only_observed =
            !r.evidence.is_empty() && r.evidence.iter().all(|k| session.is_observation(k));
        if only_observed {
            gaps = true;
            let _ = writeln!(
                out,
                "- 観察だけを根拠にした結果: {}（{} R{}・{}）",
                r.criterion,
                r.part,
                r.round,
                r.outcome.label()
            );
        }
    }
    for raw in session.unplaced() {
        gaps = true;
        let state = if session.closed {
            "判定されずに終わった不具合"
        } else {
            "判定待ちの不具合"
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
    if !gaps {
        out.push_str("（ありません）\n");
    }
    out.push('\n');
}

fn write_rounds(out: &mut String, session: &Session) {
    let _ = writeln!(out, "{}\n", SECTIONS[7]);
    if session.rounds.is_empty() {
        out.push_str("（まだありません）\n");
    }
    for round in &session.rounds {
        let _ = write!(out, "- R{}（{}）: ", round.no, round.kind.label());
        match (&round.outcome, round.phase) {
            (Some(outcome), _) => out.push_str(&one_line(&outcome.summary)),
            (None, Phase::Closed) => out.push_str("（判定なし）"),
            (None, _) if session.closed => out.push_str("（中断）"),
            (None, _) => out.push_str("（進行中）"),
        }
        out.push('\n');
    }
}

/// How many trailing lines of a cited log the fix task shows (L1-7 item 3).
pub const FIX_TAIL_LINES: usize = 80;

/// The task for an agent asked to fix `criteria` (failed criterion ids)
/// and `defects` (defect ids): each failed criterion with its failing
/// evidence and `tails` (the last 80 lines of each cited stored log, read
/// by the runner), each defect with its repro steps.
pub fn fix_task(
    session: &Session,
    criteria: &[String],
    defects: &[String],
    tails: &BTreeMap<EvidenceKey, String>,
) -> String {
    let m = &session.manifest;
    let criteria: Vec<_> = session
        .criteria
        .iter()
        .filter(|c| criteria.contains(&c.id))
        .collect();
    let defects: Vec<&Defect> = session
        .ranked()
        .into_iter()
        .filter(|d| defects.contains(&d.id))
        .collect();
    let mut out = String::with_capacity(4096);
    let _ = writeln!(
        out,
        "# Fix request from verification {}\n\n\
         A panel of coding agents verified a change in this repository ({}) by running it \
         against acceptance criteria, and a judge ruled on each one from recorded evidence. \
         Fix the {} failed criteri{} and {} defect{} below in this working tree.\n",
        session.id,
        m.label,
        criteria.len(),
        if criteria.len() == 1 { "on" } else { "a" },
        defects.len(),
        if defects.len() == 1 { "" } else { "s" },
    );
    out.push_str("## What was verified\n\n");
    let short = |s: &Option<String>| {
        s.as_deref()
            .map_or_else(|| "?".to_owned(), |s| short_id(s, 10))
    };
    match session.target.kind {
        TargetKind::Diff => {
            let _ = writeln!(
                out,
                "- The change from {} `{}` (merge base `{}`) to {}.",
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
        }
        TargetKind::State => {
            let _ = writeln!(
                out,
                "- The tree as it was at {} `{}`.",
                m.head_ref.as_deref().unwrap_or("the work tree on"),
                short(&m.head_sha)
            );
        }
    }
    // A ref: the tab opens in the target's checkout, which may have
    // another commit out (review L1-9).
    if let Some(head) = &m.head_ref {
        let _ = writeln!(
            out,
            "- The verifiers checked {head} at `{}`. If this working tree is not on that \
             commit (`git rev-parse HEAD`), stop and ask before changing anything.",
            m.head_sha.as_deref().unwrap_or("?")
        );
    }
    out.push_str(match session.isolation {
        Isolation::Worktree => {
            "- The verifiers ran in copies of it; the stored logs below are what they saw. The \
             work tree may have changed since.\n\n"
        }
        Isolation::Shared => {
            "- The verifiers all ran in this working tree itself (shared mode: told not to edit \
             it, which nothing enforced); the stored logs below are what they saw. The work tree \
             may have changed since.\n\n"
        }
    });
    out.push_str(
        "## How to work\n\n\
         - Fix only what is listed here.\n\
         - Reproduce each failure first with the command or steps given; if it does not \
         reproduce, leave it and say why.\n\
         - Follow the repository's own rules (AGENTS.md, CLAUDE.md, CONTRIBUTING) and run \
         the checks that cover what you change.\n\
         - Do not push, publish, or open pull requests.\n\
         - End with a report: for each criterion and defect id, what you changed, or why you \
         left it.\n\n",
    );
    if let Some(v) = session.verdict() {
        let _ = writeln!(
            out,
            "## The judge's verdict: {}\n\n{}\n",
            v.decision.name(),
            one_line(&v.rationale)
        );
        if !v.blocking.is_empty() {
            let _ = writeln!(out, "Blocking: {}\n", v.blocking.join(", "));
        }
    }
    if !criteria.is_empty() {
        out.push_str("## Failed criteria\n\n");
    }
    for c in criteria {
        let _ = writeln!(
            out,
            "### {} [{}] {}\n",
            c.id,
            c.priority.name(),
            c.statement
        );
        if !c.how.trim().is_empty() {
            let _ = writeln!(out, "- How it is checked: {}", one_line(&c.how));
        }
        if let Some(r) = session.ruling(&c.id) {
            let _ = writeln!(
                out,
                "- Judge (R{}): {} — {}",
                r.round,
                r.kind.name(),
                one_line(&r.rationale)
            );
        }
        // The failing evidence: what the ruling cites, then every fail
        // result's own, each once.
        let mut keys: Vec<&EvidenceKey> = session
            .ruling(&c.id)
            .map(|r| r.evidence.iter().collect())
            .unwrap_or_default();
        for r in session
            .results
            .iter()
            .filter(|r| r.criterion == c.id && r.outcome == Outcome::Fail)
        {
            let _ = writeln!(
                out,
                "- {} (R{}) reported fail: {}",
                r.part,
                r.round,
                one_line(&r.note)
            );
            for key in &r.evidence {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        for check in session.cross_checks.iter().filter(
            |x| matches!(&x.subject, Subject::Result { criterion, .. } if *criterion == c.id),
        ) {
            let _ = writeln!(
                out,
                "- {} cross-checked {} (R{}): {} — {}",
                check.by,
                check.subject,
                check.round,
                check.outcome.name(),
                one_line(&check.note)
            );
        }
        // A defect's reproduction whose evidence backs this failure (L1-6
        // rule 6) gives the steps, also when the defect is not in scope.
        let backing: Vec<_> = session
            .cross_checks
            .iter()
            .filter(|x| {
                x.outcome == CrossOutcome::Reproduced
                    && x.evidence.iter().any(|k| keys.contains(&k))
            })
            .collect();
        let mut shown: Vec<&str> = Vec::new();
        for check in backing {
            let Subject::Defect { id } = &check.subject else {
                continue;
            };
            let _ = writeln!(
                out,
                "- {} reproduced defect {id} (R{}): {}",
                check.by,
                check.round,
                one_line(&check.note)
            );
            if let Some(d) = session.defect(id)
                && !shown.contains(&id.as_str())
            {
                shown.push(id);
                // Indented past the list item's own two spaces.
                let repro: Vec<String> = d
                    .repro
                    .trim()
                    .lines()
                    .map(|l| format!("      {l}"))
                    .collect();
                let _ = writeln!(
                    out,
                    "  {id} [{}] {}\n  Repro:\n\n{}\n",
                    d.severity.name(),
                    d.claim,
                    repro.join("\n")
                );
            }
            for key in &check.evidence {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        for key in keys {
            write_fix_item(&mut out, session, key, tails);
        }
        out.push('\n');
    }
    if !defects.is_empty() {
        out.push_str("## Defects\n\n");
    }
    for d in defects {
        let _ = writeln!(out, "### {} [{}] {}\n", d.id, d.severity.name(), d.claim);
        let location = match &d.location {
            Some(loc) => format!("`{loc}`"),
            None => "the target as a whole".to_owned(),
        };
        let _ = writeln!(out, "- Location: {location}");
        let _ = writeln!(
            out,
            "- Status: {} · judge: {}",
            d.status.name(),
            one_line(&d.rationale)
        );
        let _ = writeln!(out, "- Repro:\n\n{}\n", indent(d.repro.trim()));
        if let Some(fix) = d.fix.as_deref().or_else(|| {
            d.sources
                .iter()
                .find_map(|k| session.raw_defect(k).and_then(|r| r.fix.as_deref()))
        }) {
            let _ = writeln!(out, "- Suggested fix: {}", one_line(fix));
        }
        for raw in d.sources.iter().filter_map(|k| session.raw_defect(k)) {
            for key in &raw.evidence {
                write_fix_item(&mut out, session, key, tails);
            }
        }
        out.push('\n');
    }
    out
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One evidence item of a fix task, with its stored log's tail.
fn write_fix_item(
    out: &mut String,
    session: &Session,
    key: &EvidenceKey,
    tails: &BTreeMap<EvidenceKey, String>,
) {
    let Some(item) = session.evidence_item(key) else {
        return;
    };
    match &item.body {
        EvidenceBody::Command { cmd, cwd, exit, .. } => {
            let _ = writeln!(
                out,
                "- Evidence {key}: exit {exit} in `{cwd}`:\n\n{}\n",
                indent(cmd.trim())
            );
            if let Some(r) = &item.rerun {
                let _ = writeln!(out, "  Re-run: {}", rerun_english(r));
            }
        }
        EvidenceBody::File { path, caption, .. } => {
            let _ = write!(out, "- Evidence {key}: file {path}");
            if let Some(caption) = caption {
                let _ = write!(out, " — {}", one_line(caption));
            }
            out.push('\n');
        }
        EvidenceBody::Observation { text } => {
            let _ = writeln!(out, "- Evidence {key} (observation): {}", one_line(text));
        }
    }
    if let Some(stored) = &item.stored {
        let reply_dir = crate::store::reply_dir(session);
        let _ = writeln!(
            out,
            "  Stored copy: {}",
            reply_dir.join(&stored.path).display()
        );
    }
    if let Some(tail) = tails.get(key) {
        let _ = writeln!(
            out,
            "  Last {FIX_TAIL_LINES} lines of its log:\n\n```\n{}\n```\n",
            tail.trim_end()
        );
    }
}

fn rerun_english(rerun: &Rerun) -> String {
    match rerun {
        Rerun::Matched { exit, .. } => format!("matched (exit {exit})"),
        Rerun::Differed { exit: Some(e), .. } => format!("differed (exit {e})"),
        Rerun::Differed { exit: None, .. } => "differed (no exit status)".to_owned(),
        Rerun::TimedOut { .. } => "timed out".to_owned(),
        Rerun::NotRun { why, .. } => format!("not run: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::Isolation;
    use crate::testkit::{self, Options, Rig, VERIFIERS, all_pass, cmd, reply_json};

    #[test]
    fn ac33_a_caption_holding_brackets_keeps_its_media_link_whole() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply(
            "claude",
            &reply_json(
                &[
                    ("AC-1", "pass", &["e1", "e2", "e3"]),
                    ("AC-2", "pass", &["e1"]),
                    ("AC-3", "pass", &["e1"]),
                ],
                &[
                    cmd("e1", 0, true),
                    r#"{"id": "e2", "kind": "file", "path": "shots/key.png", "caption": "右側の ] キー"}"#
                        .to_owned(),
                    r#"{"id": "e3", "kind": "file", "path": "video/key.mov", "caption": "[x] を押す \\ 戻る"}"#
                        .to_owned(),
                ],
                "",
            ),
        );
        rig.reply("codex", &all_pass());
        rig.reply("claude-2", &all_pass());
        rig.ingest();
        let report = report_markdown(rig.engine.session(), None);
        assert!(
            report.contains("- ![右側の \\] キー](evidence/r1/claude/shots/key.png)"),
            "{report}"
        );
        assert!(
            report.contains("- ▶ [\\[x\\] を押す \\\\ 戻る](evidence/r1/claude/video/key.mov)"),
            "{report}"
        );
    }

    #[test]
    fn a_command_of_several_lines_is_shown_whole_to_the_judge_and_in_the_report() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply(
            "claude",
            &reply_json(
                &[
                    ("AC-1", "pass", &["e1"]),
                    ("AC-2", "pass", &["e1"]),
                    ("AC-3", "pass", &["e1"]),
                ],
                &[
                    r#"{"id": "e1", "kind": "command", "cmd": "set -e\ncargo test actual_case", "cwd": ".", "exit": 0, "log": "logs/e1.log", "rerun": true}"#
                        .to_owned(),
                ],
                "",
            ),
        );
        rig.reply("codex", &all_pass());
        rig.reply("claude-2", &all_pass());
        rig.ingest();
        rig.rerun_all_matched();
        let brief = rig.brief("judge", crate::model::types::BriefKind::Work);
        assert!(
            brief.contains(
                "  - claude/r1/e1: command of 2 lines (cwd .) exit 0; re-run matched (exit 0)\n    ```\n    set -e\n    cargo test actual_case\n    ```\n    stored: "
            ),
            "{brief}"
        );
        rig.judge(testkit::JUDGE_R1);
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        let report = report_markdown(rig.engine.session(), None);
        let block = "\n  ```\n  set -e\n  cargo test actual_case\n  ```\n";
        let evidence = evidence_section(&report);
        let at = evidence
            .find("- `claude/r1/e1` コマンド 2 行（.） · 終了コード 0 · 再実行 一致（0） · digest ")
            .unwrap_or_else(|| panic!("{report}"));
        let entry = &evidence[at..];
        let entry = &entry[..entry.find("\n- ").map_or(entry.len(), |end| end + 1)];
        assert!(entry.contains(block), "{entry}");
        // Under AC-1, cited by its ruling, one list level down.
        let ac1 = &report[report.find("### AC-1").unwrap()..];
        let ac1 = &ac1[..ac1.find("### AC-2").unwrap()];
        assert!(
            ac1.contains("\n    ```\n    set -e\n    cargo test actual_case\n    ```\n"),
            "{ac1}"
        );
        // A fence outlasts any run of backticks in the command.
        assert_eq!(
            crate::model::brief::fenced("echo ```\nls", "  "),
            "\n  ````\n  echo ```\n  ls\n  ````"
        );
    }

    #[test]
    fn a_report_written_while_an_extension_rebuilds_the_worktrees_does_not_say_ended() {
        use crate::engine::Input;
        let mut rig = Rig::new(1);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        for part in VERIFIERS {
            rig.reply(part, &all_pass());
        }
        rig.ingest();
        rig.rerun_all_matched();
        rig.judge(&format!(
            "{}{}",
            testkit::JUDGE_R1.trim_end().trim_end_matches('}'),
            r#", "verdict": {"decision": "pass", "rationale": "Holds.", "blocking": []}}"#
        ));
        assert!(rig.engine.session().closed, "{:?}", rig.rejected("judge"));
        let report = report_markdown(rig.engine.session(), None);
        assert!(report.contains("- ラウンド: 1 / 1・終了\n"), "{report}");
        for part in VERIFIERS {
            rig.feed(Input::WorktreeRemoved {
                part: testkit::p(part),
            });
        }
        let at = rig.tick();
        rig.feed(Input::UserExtend { by: 1, at });
        let s = rig.engine.session();
        assert!(!s.closed && s.verdict().is_none());
        let report = report_markdown(s, None);
        assert!(report.contains("- ラウンド: 1 / 2・配信\n"), "{report}");
        assert!(!report.contains("・終了"), "{report}");
        testkit::make_worktrees(&mut rig);
        let report = report_markdown(rig.engine.session(), None);
        assert!(report.contains("- ラウンド: 2 / 2・配信\n"), "{report}");
    }

    /// The `## 証跡一覧` section of `report`.
    fn evidence_section(report: &str) -> &str {
        let at = report.find(SECTIONS[4]).unwrap();
        let rest = &report[at..];
        &rest[..rest.find(SECTIONS[5]).unwrap()]
    }

    #[test]
    fn the_report_says_once_where_the_reruns_ran() {
        let mut rig = Rig::new(2);
        testkit::through_round_one(&mut rig);
        let report = report_markdown(rig.engine.session(), None);
        let pane = "再実行は各検証者自身のペインで、その検証者のエージェントがウィジェットの書いたスクリプトを起動して行いました。";
        assert_eq!(report.matches(pane).count(), 1, "{report}");
        assert!(evidence_section(&report).contains(pane), "{report}");

        let mut rig = Rig::build(Options {
            isolation: Isolation::Shared,
            ..Options::default()
        });
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        for part in VERIFIERS {
            rig.reply(part, &all_pass());
        }
        rig.ingest();
        let report = report_markdown(rig.engine.session(), None);
        let none = "共有（読み取りのみ）モードのため、再実行は一つも行っていません";
        assert_eq!(report.matches(none).count(), 1, "{report}");
        assert!(evidence_section(&report).contains(none), "{report}");
        assert!(!report.contains(pane), "{report}");
    }

    #[test]
    fn ac33_an_image_only_a_cross_check_cites_is_embedded_under_its_criterion() {
        let mut rig = Rig::new(3);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply("claude", &all_pass());
        rig.reply(
            "codex",
            &reply_json(
                &[
                    ("AC-1", "fail", &["e1"]),
                    ("AC-2", "pass", &["e2"]),
                    ("AC-3", "pass", &["e2"]),
                ],
                &[cmd("e1", 101, true), cmd("e2", 0, false)],
                "",
            ),
        );
        rig.reply("claude-2", &all_pass());
        rig.ingest();
        rig.rerun_all_matched();
        rig.judge(
            r#"{"summary": "s", "rulings": [
              {"criterion": "AC-1", "ruling": "failed", "rationale": "codex's test fails", "evidence": ["codex/r1/e1"]},
              {"criterion": "AC-2", "ruling": "verified", "rationale": "matched", "evidence": ["claude/r1/e1"]},
              {"criterion": "AC-3", "ruling": "verified", "rationale": "matched", "evidence": ["claude/r1/e1"]}],
             "assign": {"claude-2": ["codex/AC-1"]}}"#,
        );
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        rig.deliver();
        rig.reply("claude", r#"{"summary": "Nothing assigned."}"#);
        rig.reply("codex", r#"{"summary": "Nothing assigned."}"#);
        rig.reply(
            "claude-2",
            &reply_json(
                &[],
                &[
                    r#"{"id": "p1", "kind": "file", "path": "shots/home.png", "caption": "the new tab shows $HOME", "shows": ["AC-1"]}"#
                        .to_owned(),
                ],
                r#", "cross_checks": [{"subject": "codex/AC-1", "outcome": "reproduced", "note": "fails here too", "evidence": ["p1"]}]"#,
            ),
        );
        rig.ingest();
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("claude-2"));
        let report = report_markdown(rig.engine.session(), None);
        let ac1 = &report[report.find("### AC-1").unwrap()..report.find("### AC-2").unwrap()];
        assert!(
            ac1.contains("- ![the new tab shows $HOME](evidence/r2/claude-2/shots/home.png)"),
            "{ac1}"
        );
    }

    #[test]
    fn a_blocked_result_and_its_reason_stay_listed_after_a_later_update() {
        let mut rig = Rig::new(3);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply(
            "claude",
            &format!(
                r#"{{"summary": "s", "results": [
                  {{"criterion": "AC-1", "result": "pass", "note": "ok", "evidence": ["e1"]}},
                  {{"criterion": "AC-2", "result": "pass", "note": "ok", "evidence": ["e1"]}},
                  {{"criterion": "AC-3", "result": "blocked", "note": "画面がない"}}],
                 "evidence": [{}]}}"#,
                cmd("e1", 0, true)
            ),
        );
        rig.reply("codex", &all_pass());
        rig.reply("claude-2", &all_pass());
        rig.ingest();
        rig.rerun_all_matched();
        rig.judge(testkit::JUDGE_R1);
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        rig.deliver();
        rig.reply(
            "claude",
            &format!(
                r#"{{"summary": "s", "results": [{{"criterion": "AC-3", "result": "pass", "note": "a screen now", "evidence": ["e1"]}}], "evidence": [{}]}}"#,
                cmd("e1", 0, false)
            ),
        );
        rig.reply("codex", r#"{"summary": "Nothing assigned."}"#);
        rig.reply("claude-2", r#"{"summary": "Nothing assigned."}"#);
        rig.ingest();
        assert_eq!(
            rig.engine
                .session()
                .latest_result(&testkit::p("claude"), "AC-3")
                .map(|r| r.outcome),
            Some(crate::model::types::Outcome::Pass),
            "{:?}",
            rig.rejected("claude")
        );
        let report = report_markdown(rig.engine.session(), None);
        assert!(
            report.contains("- AC-3 を claude が実行できませんでした（R1）: 画面がない"),
            "{report}"
        );
    }

    /// The `## 検証の欠落` section of `report`.
    fn gaps_section(report: &str) -> &str {
        let at = report.find(SECTIONS[6]).unwrap();
        let rest = &report[at..];
        &rest[..rest.find(SECTIONS[7]).unwrap()]
    }

    #[test]
    fn a_report_closed_before_any_rerun_says_none_ran_and_lists_them() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        // claude's reply marks e1 for a re-run; the person ends the
        // verification while the others are still working.
        rig.reply("claude", &all_pass());
        let item = rig
            .engine
            .session()
            .evidence_item(&testkit::key("claude/r1/e1"))
            .cloned()
            .unwrap();
        assert!(item.rerun.is_none(), "{item:?}");
        let pending = report_markdown(rig.engine.session(), None);
        let pane = "再実行は各検証者自身のペインで、その検証者のエージェントがウィジェットの書いたスクリプトを起動して行いました。";
        assert!(!pending.contains(pane), "{pending}");
        assert!(
            evidence_section(&pending).contains("再実行はまだ一つも行われていません"),
            "{pending}"
        );
        rig.feed(crate::engine::Input::UserClose);
        let report = report_markdown(rig.engine.session(), None);
        assert!(!report.contains(pane), "{report}");
        assert!(
            evidence_section(&report).contains(
                "再実行は一つも行われませんでした（`rerun: true` の項目はすべて未実行、理由は「## 検証の欠落」）。"
            ),
            "{report}"
        );
        assert!(
            gaps_section(&report).contains(
                "- 再実行されなかったコマンド: `claude/r1/e1` — 再実行の前に検証が終了しました"
            ),
            "{report}"
        );
    }

    #[test]
    fn ac33_media_a_cross_check_on_a_merged_defect_cites_stays_under_the_defect_it_went_into() {
        let mut rig = Rig::new(3);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        let with_defect = |claim: &str| {
            format!(
                r#"{}, "defects": [{{"id": "1", "claim": "{claim}", "severity": "high", "category": "regression",
                   "confidence": "high", "repro": "1. kill -9 2. reopen", "evidence": ["e1"]}}]}}"#,
                all_pass().trim_end_matches('}')
            )
        };
        rig.reply("claude", &all_pass());
        rig.reply("codex", &with_defect("The store loses the last line"));
        rig.reply("claude-2", &with_defect("Reopening drops a line"));
        rig.ingest();
        rig.rerun_all_matched();
        let add = |id: &str, from: &str| {
            format!(
                r#"{{"op": "add", "id": "{id}", "from": ["{from}"], "claim": "The store loses a line", "repro": "1. kill -9 2. reopen",
                    "severity": "high", "category": "regression", "confidence": "high", "status": "open", "rationale": "Seen."}}"#
            )
        };
        rig.judge(&format!(
            r#"{}, "defect_ops": [{}, {}], "assign": {{"claude": ["D1"]}}}}"#,
            testkit::JUDGE_R1.trim_end().trim_end_matches('}'),
            add("D1", "claude-2/1"),
            add("D2", "codex/1")
        ));
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        // Only claude's cross-check on D1 cites the picture.
        rig.deliver();
        rig.reply(
            "claude",
            &reply_json(
                &[],
                &[r#"{"id": "p1", "kind": "file", "path": "shots/reopen.png", "caption": "the last line is gone after reopen"}"#
                    .to_owned()],
                r#", "cross_checks": [{"subject": "D1", "outcome": "reproduced", "note": "lost here too", "evidence": ["p1"]}]"#,
            ),
        );
        rig.reply("codex", r#"{"summary": "Nothing assigned."}"#);
        rig.reply("claude-2", r#"{"summary": "Nothing assigned."}"#);
        rig.ingest();
        assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude"));
        // Then D1 goes into D2.
        let same = |c: &str| format!(r#"{{"criterion": "{c}", "ruling": "same"}}"#);
        rig.judge(&format!(
            r#"{{"summary": "s", "rulings": [{}, {}, {}], "defect_ops": [{{"op": "merge", "id": "D2", "from": ["D1"]}}]}}"#,
            same("AC-1"),
            same("AC-2"),
            same("AC-3")
        ));
        assert_eq!(rig.round(), 3, "{:?}", rig.rejected("judge"));
        let session = rig.engine.session();
        let d2 = session.defect("D2").unwrap();
        assert!(session.tally(d2).supporters.contains(&testkit::p("claude")));
        let report = report_markdown(session, None);
        let defects = &report[report.find(SECTIONS[2]).unwrap()..report.find(SECTIONS[3]).unwrap()];
        assert!(!defects.contains("### D1"), "{defects}");
        let d2 = &defects[defects.find("### D2").unwrap()..];
        assert!(
            d2.contains(
                "- スクショ・動画:\n  - ![the last line is gone after reopen](evidence/r2/claude/shots/reopen.png)\n"
            ),
            "{d2}"
        );
    }

    #[test]
    fn a_criterion_with_a_pass_in_an_earlier_round_is_not_listed_as_lacking_one() {
        let mut rig = Rig::new(3);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        // R1: everyone passes AC-1 and AC-2; nobody tests AC-3.
        let r1 = reply_json(
            &[
                ("AC-1", "pass", &["e1"]),
                ("AC-2", "pass", &["e1"]),
                ("AC-3", "not_tested", &[]),
            ],
            &[cmd("e1", 0, true)],
            "",
        );
        for part in VERIFIERS {
            rig.reply(part, &r1);
        }
        rig.ingest();
        rig.rerun_all_matched();
        rig.judge(
            r#"{"summary": "s", "rulings": [
              {"criterion": "AC-1", "ruling": "verified", "rationale": "matched", "evidence": ["claude/r1/e1"]},
              {"criterion": "AC-2", "ruling": "verified", "rationale": "matched", "evidence": ["codex/r1/e1"]},
              {"criterion": "AC-3", "ruling": "inconclusive", "rationale": "untested"}]}"#,
        );
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        // R2: everyone's latest AC-1 result turns blocked.
        rig.deliver();
        for part in VERIFIERS {
            rig.reply(
                part,
                r#"{"summary": "s", "results": [{"criterion": "AC-1", "result": "blocked", "note": "the display is gone"}]}"#,
            );
        }
        rig.ingest();
        assert_eq!(rig.phase(), Phase::Ruling, "{:?}", rig.rejected("claude"));
        let report = report_markdown(rig.engine.session(), None);
        let gaps = gaps_section(&report);
        assert!(
            gaps.contains("- 成功・失敗の結果がない基準: AC-3\n"),
            "AC-1's R1 passes are in its history: {gaps}"
        );
        assert!(
            gaps.contains("- AC-1 を claude が実行できませんでした（R2）: the display is gone"),
            "{gaps}"
        );
    }

    #[test]
    fn ac24_the_fix_task_says_where_the_verifiers_ran() {
        let task = |isolation| {
            let mut rig = Rig::build(Options {
                isolation,
                ..Options::default()
            });
            testkit::accept_default_criteria(&mut rig);
            testkit::start_round_one(&mut rig);
            rig.deliver();
            for part in VERIFIERS {
                rig.reply(part, &all_pass());
            }
            fix_task(
                rig.engine.session(),
                &["AC-1".into()],
                &[],
                &BTreeMap::new(),
            )
        };
        let copies = "- The verifiers ran in copies of it;";
        let here = "- The verifiers all ran in this working tree itself (shared mode: told not to edit it, which nothing enforced);";
        let worktree = task(Isolation::Worktree);
        assert!(
            worktree.contains(copies) && !worktree.contains(here),
            "{worktree}"
        );
        let shared = task(Isolation::Shared);
        assert!(
            shared.contains(here) && !shared.contains(copies),
            "{shared}"
        );
    }

    #[test]
    fn ac24_a_fix_task_for_a_ref_says_to_stop_when_the_tree_is_not_on_its_commit() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        let stop = format!(
            "- The verifiers checked feat/cwd at `{}`. If this working tree is not on that \
             commit (`git rev-parse HEAD`), stop and ask before changing anything.\n",
            testkit::HEAD_SHA
        );
        let task = fix_task(
            rig.engine.session(),
            &["AC-1".into()],
            &[],
            &BTreeMap::new(),
        );
        assert!(task.contains(&stop), "{task}");
        // Work-tree mode: what the verifiers saw was the tree itself.
        let mut session = rig.engine.session().clone();
        session.manifest.head_ref = None;
        let task = fix_task(&session, &["AC-1".into()], &[], &BTreeMap::new());
        assert!(!task.contains("stop and ask"), "{task}");
    }

    #[test]
    fn ac24_a_criterion_failed_on_a_defects_reproduction_keeps_its_repro_steps_alone() {
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
        // The picture backs AC-2 failing through D1's reproduction alone
        // (L1-6 rule 6).
        rig.judge(
            r#"{"summary": "s", "rulings": [
              {"criterion": "AC-1", "ruling": "same"},
              {"criterion": "AC-2", "ruling": "failed", "rationale": "codex reproduced D1: the store is empty", "evidence": ["codex/r2/p1"]},
              {"criterion": "AC-3", "ruling": "same"}]}"#,
        );
        assert_eq!(rig.round(), 3, "{:?}", rig.rejected("judge"));
        let task = fix_task(
            rig.engine.session(),
            &["AC-2".into()],
            &[],
            &BTreeMap::new(),
        );
        let ac2 = &task[task.find("### AC-2").unwrap()..];
        assert!(
            ac2.contains("Evidence codex/r2/p1: file shots/empty.png"),
            "{ac2}"
        );
        assert!(
            ac2.contains(
                "- codex reproduced defect D1 (R2): lost here too\n  \
                 D1 [high] The store loses the last line\n  \
                 Repro:\n\n      1. kill -9 mid-write\n      2. reopen the store\n"
            ),
            "{ac2}"
        );
    }

    #[test]
    fn a_verification_closed_while_its_criteria_wait_lists_every_one_as_untested() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        assert_eq!(rig.phase(), Phase::Confirming);
        let held = report_markdown(rig.engine.session(), None);
        assert!(
            !gaps_section(&held).contains("成功・失敗の結果がない基準"),
            "not yet: the criteria may still be edited\n{held}"
        );
        rig.feed(crate::engine::Input::UserClose);
        let report = report_markdown(rig.engine.session(), None);
        assert!(
            gaps_section(&report).contains("- 成功・失敗の結果がない基準: AC-1, AC-2, AC-3\n"),
            "{report}"
        );
    }

    #[test]
    fn ac33_a_video_only_a_defect_cites_is_linked_with_its_marks_under_the_defect() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.deliver();
        rig.reply("claude", &all_pass());
        rig.reply("codex", &all_pass());
        rig.reply(
            "claude-2",
            &reply_json(
                &[
                    ("AC-1", "pass", &["e1"]),
                    ("AC-2", "pass", &["e1"]),
                    ("AC-3", "pass", &["e1"]),
                ],
                &[
                    cmd("e1", 0, true),
                    r#"{"id": "v1", "kind": "file", "path": "video/defect.mov", "caption": "kill -9, reopen",
                        "marks": [{"t": "00:05", "note": "the last line is gone"}]}"#
                        .to_owned(),
                ],
                r#", "defects": [{"id": "1", "claim": "The store loses the last line", "severity": "high", "category": "regression",
                   "confidence": "high", "location": {"path": "src/store.rs", "line": 11}, "repro": "1. kill -9 2. reopen", "evidence": ["v1"]}]"#,
            ),
        );
        rig.ingest();
        rig.rerun_all_matched();
        let judge = testkit::JUDGE_R1
            .trim_end()
            .trim_end_matches('}')
            .to_owned()
            + r#", "defect_ops": [{"op": "add", "id": "D1", "from": ["claude-2/1"],
                 "claim": "The store loses the last line", "repro": "1. kill -9 2. reopen", "severity": "high", "category": "regression",
                 "confidence": "high", "status": "open", "rationale": "Seen once."}]}"#;
        rig.judge(&judge);
        assert_eq!(rig.round(), 2, "{:?}", rig.rejected("judge"));
        let report = report_markdown(rig.engine.session(), None);
        let defects = &report[report.find(SECTIONS[2]).unwrap()..report.find(SECTIONS[3]).unwrap()];
        let d1 = &defects[defects.find("### D1").unwrap()..];
        for line in [
            "- スクショ・動画:\n",
            "  - ▶ [kill -9, reopen](evidence/r1/claude-2/video/defect.mov)\n",
            "    - 00:05 the last line is gone\n",
        ] {
            assert!(d1.contains(line), "{line:?} in\n{d1}");
        }
    }
}
