//! The right pane: the selected criterion's thread (statement, how, source,
//! priority; per verifier per round its result, note and evidence with the
//! re-run outcome and media as placeholder cards; the person's media;
//! 証跡を追加; the cross-checks; the judge's ruling history), the selected
//! defect, or — nothing selected — the judge's latest summary and the
//! verdict.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, UiTree, WidgetId};

use verify::model::types::{
    CrossOutcome, DefectStatus, Evidence, EvidenceBody, EvidenceKey, Outcome, Session, Subject,
};

use super::evidence::{self, Kind};
use super::fix::{self, FixOffer};
use super::{
    Action, body, caption, card, decision_tone, heading, keep_scroll, note, ruling_tone,
    scroll_column, severity_tone, status_tone, tag,
};

pub(super) fn outcome_tone(outcome: Outcome) -> Tone {
    match outcome {
        Outcome::Pass => Tone::Success,
        Outcome::Fail => Tone::Critical,
        Outcome::Blocked => Tone::Warning,
        Outcome::NotTested => Tone::Neutral,
    }
}

fn cross_tone(outcome: CrossOutcome) -> Tone {
    match outcome {
        CrossOutcome::Reproduced => Tone::Success,
        CrossOutcome::NotReproduced => Tone::Critical,
        CrossOutcome::Disputed => Tone::Warning,
    }
}

/// What the right pane shows.
pub(super) enum Shown<'a> {
    Criterion(&'a str),
    Defect(&'a str),
    Nothing,
}

pub(super) struct Context<'a> {
    pub(super) offer: Option<&'a FixOffer>,
    /// 証跡を追加 is offered (round 1 started, not closed, a runner holds
    /// the verification).
    pub(super) can_attach: bool,
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    shown: Shown,
    cx: &Context,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    match shown {
        Shown::Criterion(id) if session.criterion(id).is_some() => {
            criterion(tree, host, session, id, cx, actions)
        }
        Shown::Defect(id) if session.defect(id).is_some() => {
            defect(tree, host, session, id, cx, actions)
        }
        _ => overview(tree, host, session, cx, actions),
    }
}

/// The evidence `keys` cite: a line per item (with its re-run outcome and
/// 開く), then the media among them as a strip of placeholder cards.
fn cited(
    tree: &mut UiTree,
    parent: WidgetId,
    session: &Session,
    keys: &[EvidenceKey],
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let items: Vec<&Evidence> = keys
        .iter()
        .filter_map(|k| session.evidence_item(k))
        .collect();
    let mut media = Vec::new();
    for item in &items {
        if item.media().is_some() {
            media.push(*item);
            continue;
        }
        let line = tree.insert(
            Some(parent),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tag(tree, line, Kind::of(item).label(), Tone::Neutral);
        let text = match &item.body {
            EvidenceBody::Command { cmd, exit, .. } => format!("$ {cmd} → {exit}"),
            EvidenceBody::File { path, .. } => path.clone(),
            EvidenceBody::Observation { text } => text.clone(),
        };
        tree.insert(
            Some(line),
            Label::new(text).font_size(11.5).wrap(true).selectable(true),
        );
        if let Some(rerun) = evidence::rerun_text(item) {
            caption(tree, line, rerun);
        }
        if matches!(item.body, EvidenceBody::Observation { .. }) {
            tag(tree, line, "未裏付け", Tone::Warning);
        } else if item.stored.is_some() {
            let open = tree.insert(
                Some(line),
                Button::new("開く").control_size(ControlSize::Small),
            );
            tree.set_accessible_label(open, format!("{} の保存されたコピーを開く", item.key));
            actions.push((open, Action::OpenEvidence(item.key.clone())));
        } else {
            caption(tree, line, "（保存されていません）");
        }
    }
    if !media.is_empty() {
        let strip = tree.insert(Some(parent), Container::wrapping_row(6.0, 6.0));
        for item in media {
            evidence::media_card(tree, strip, item, actions);
        }
    }
}

fn criterion(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    id: &str,
    cx: &Context,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let c = session.criterion(id).expect("checked by the caller");
    heading(tree, host, &format!("基準 {id}"));
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, &format!("thread-{id}"));
    let head = tree.insert(
        Some(list),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tag(tree, head, c.priority.label(), Tone::Accent);
    tag(
        tree,
        head,
        format!("出典: {}", super::criteria::source_label(c.source)),
        Tone::Neutral,
    );
    match session.ruling(id) {
        Some(r) => tag(tree, head, r.kind.label(), ruling_tone(r.kind)),
        None => tag(tree, head, "未判定", Tone::Neutral),
    }
    if c.edited {
        tag(tree, head, "あなたが変更", Tone::Warning);
    }
    tree.insert(
        Some(list),
        Label::new(c.statement.clone())
            .font_size(13.5)
            .weight(600)
            .wrap(true)
            .selectable(true),
    );
    caption(tree, list, "確かめ方");
    body(tree, list, c.how.clone());
    if let Some(offer) = cx.offer {
        fix::insert(tree, list, offer, actions);
    }

    caption(tree, list, "検証者の結果");
    let mut any = false;
    for part in &session.verifiers {
        for r in session
            .results
            .iter()
            .filter(|r| r.part == part.id && r.criterion == id)
        {
            any = true;
            let box_ = card(tree, list, false);
            let top = tree.insert(
                Some(box_),
                Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
            );
            tree.insert(
                Some(top),
                Label::new(part.id.to_string()).font_size(12.0).weight(600),
            );
            caption(tree, top, format!("R{}", r.round));
            tag(
                tree,
                top,
                format!("{} {}", r.outcome.mark(), r.outcome.label()),
                outcome_tone(r.outcome),
            );
            if !r.note.is_empty() {
                body(tree, box_, r.note.clone());
            }
            cited(tree, box_, session, &r.evidence, actions);
        }
    }
    if !any {
        note(
            tree,
            list,
            "まだ結果はありません（独立検証の返答で集まります）",
        );
    }

    let mine: Vec<&Evidence> = session
        .evidence
        .iter()
        .filter(|e| e.key.is_user() && evidence::relates(session, e, id))
        .collect();
    caption(tree, list, "あなたの証跡");
    if mine.is_empty() {
        note(tree, list, "まだありません");
    } else {
        let strip = tree.insert(Some(list), Container::wrapping_row(6.0, 6.0));
        for item in mine {
            evidence::media_card(tree, strip, item, actions);
        }
    }
    if cx.can_attach {
        let attach = tree.insert(
            Some(list),
            Button::new("証跡を追加…")
                .control_size(ControlSize::Small)
                .tooltip("スクリーンショットや画面収録を選ぶか、ウィンドウにドロップします"),
        );
        tree.set_accessible_label(attach, format!("{id} の証跡を追加"));
        actions.push((attach, Action::AttachEvidence));
    }

    caption(tree, list, "追試");
    let checks: Vec<_> = session
        .cross_checks
        .iter()
        .filter(|ch| matches!(&ch.subject, Subject::Result { criterion: on, .. } if on == id))
        .collect();
    if checks.is_empty() {
        note(tree, list, "まだ追試はありません（R2 以降で集まります）");
    }
    for ch in checks {
        let box_ = card(tree, list, false);
        let top = tree.insert(
            Some(box_),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(format!("{} → {}", ch.by, ch.subject))
                .font_size(12.0)
                .weight(600),
        );
        caption(tree, top, format!("R{}", ch.round));
        tag(
            tree,
            top,
            format!("⇄ {}", ch.outcome.label()),
            cross_tone(ch.outcome),
        );
        if !ch.note.is_empty() {
            body(tree, box_, ch.note.clone());
        }
        cited(tree, box_, session, &ch.evidence, actions);
    }

    caption(tree, list, "判定の履歴");
    match session.rulings.get(id) {
        Some(all) if !all.is_empty() => {
            for r in all.iter().rev() {
                let box_ = card(tree, list, false);
                let top = tree.insert(
                    Some(box_),
                    Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
                );
                caption(tree, top, format!("R{}", r.round));
                tag(tree, top, r.kind.label(), ruling_tone(r.kind));
                if !r.rationale.is_empty() {
                    body(tree, box_, r.rationale.clone());
                }
                if !r.evidence.is_empty() {
                    let keys: Vec<String> = r.evidence.iter().map(|k| k.to_string()).collect();
                    caption(tree, box_, format!("根拠: {}", keys.join(", ")));
                }
            }
        }
        _ => note(tree, list, "まだ判定はありません"),
    }
    let back = tree.insert(
        Some(list),
        Button::new("選択を解除").control_size(ControlSize::Small),
    );
    actions.push((back, Action::SelectCriterion(None)));
}

fn defect(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    id: &str,
    cx: &Context,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let d = session.defect(id).expect("checked by the caller");
    heading(tree, host, &format!("不具合 {id}"));
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, &format!("defect-{id}"));
    let head = tree.insert(
        Some(list),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tag(tree, head, d.severity.label(), severity_tone(d.severity));
    tag(
        tree,
        head,
        session.category_label(&d.category),
        Tone::Neutral,
    );
    tag(tree, head, d.status.label(), status_tone(d.status));
    caption(tree, head, d.confidence.label());
    tree.insert(
        Some(list),
        Label::new(d.claim.clone())
            .font_size(13.5)
            .weight(600)
            .wrap(true),
    );
    let tally = session.tally(d);
    caption(
        tree,
        list,
        format!("{}（{}）", tally.label(), tally.agreement.label()),
    );
    match &d.location {
        Some(loc) => caption(tree, list, loc.to_string()),
        None => caption(tree, list, "対象全体"),
    }
    if d.status == DefectStatus::Merged
        && let Some(into) = &d.merged_into
    {
        let go = tree.insert(
            Some(list),
            Button::new(format!("統合先 {into} を表示")).control_size(ControlSize::Small),
        );
        actions.push((go, Action::SelectDefect(Some(into.clone()))));
    }
    caption(tree, list, "再現手順");
    body(tree, list, d.repro.clone());
    let judge = card(tree, list, true);
    caption(tree, judge, "判定役の判断");
    body(tree, judge, d.rationale.clone());
    if let Some(fix) = &d.fix {
        caption(tree, judge, "修正案");
        body(tree, judge, fix.clone());
    }
    if let Some(offer) = cx.offer {
        fix::insert(tree, list, offer, actions);
    }
    caption(tree, list, "出典");
    for key in &d.sources {
        let Some(raw) = session.raw_defect(key) else {
            continue;
        };
        let c = card(tree, list, false);
        let top = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(key.to_string()).font_size(12.0).weight(600),
        );
        caption(tree, top, format!("R{}", raw.round));
        tag(tree, top, raw.severity.label(), severity_tone(raw.severity));
        body(tree, c, raw.claim.clone());
        caption(tree, c, format!("再現手順: {}", raw.repro));
        cited(tree, c, session, &raw.evidence, actions);
    }
    caption(tree, list, "追試");
    let checks: Vec<_> = session
        .cross_checks
        .iter()
        .filter(|ch| matches!(&ch.subject, Subject::Defect { id: on } if super::defects::names(session, d, on)))
        .collect();
    if checks.is_empty() {
        note(tree, list, "まだ追試はありません");
    }
    for ch in checks {
        let c = card(tree, list, false);
        let top = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(ch.by.to_string()).font_size(12.0).weight(600),
        );
        caption(tree, top, format!("R{}", ch.round));
        tag(
            tree,
            top,
            format!("⇄ {}", ch.outcome.label()),
            cross_tone(ch.outcome),
        );
        if !ch.note.is_empty() {
            body(tree, c, ch.note.clone());
        }
        cited(tree, c, session, &ch.evidence, actions);
    }
    caption(tree, list, "履歴");
    for (round, status, severity) in &d.history {
        caption(
            tree,
            list,
            format!("R{round}: {} · {}", status.label(), severity.label()),
        );
    }
    let back = tree.insert(
        Some(list),
        Button::new("選択を解除").control_size(ControlSize::Small),
    );
    actions.push((back, Action::SelectDefect(None)));
}

fn overview(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    cx: &Context,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "判定");
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "overview");
    match session.verdict() {
        Some(v) => {
            let c = card(tree, list, true);
            let top = tree.insert(
                Some(c),
                Container::row().gap(6.0).align_items(AlignItems::CENTER),
            );
            caption(tree, top, "総合判定");
            tag(tree, top, v.decision.label(), decision_tone(v.decision));
            body(tree, c, v.rationale.clone());
            if !v.blocking.is_empty() {
                caption(
                    tree,
                    c,
                    format!("合格を妨げるもの: {}", v.blocking.join(", ")),
                );
            }
        }
        None if session.closed => note(tree, list, "判定なし（終了しました）"),
        None => note(tree, list, "総合判定は最終ラウンドの判定で出ます"),
    }
    if let Some(offer) = cx.offer {
        fix::insert(tree, list, offer, actions);
    }
    let summaries: Vec<_> = session
        .rounds
        .iter()
        .rev()
        .filter_map(|r| r.outcome.as_ref().map(|o| (r.no, r.kind, o)))
        .collect();
    if summaries.is_empty() && session.criteria_summary.is_empty() {
        note(tree, list, "判定役のまとめはまだありません");
    }
    for (no, kind, outcome) in summaries {
        let c = card(tree, list, false);
        caption(tree, c, format!("R{no}（{}）の判定", kind.label()));
        body(tree, c, outcome.summary.clone());
        for (part, subjects) in &outcome.assign {
            let names: Vec<String> = subjects.iter().map(|s| s.to_string()).collect();
            tree.insert(
                Some(c),
                Label::new(format!("→ {part} に追試: {}", names.join(", ")))
                    .font_size(11.5)
                    .color(Role::Muted)
                    .wrap(true),
            );
        }
    }
    if !session.criteria_summary.is_empty() {
        let c = card(tree, list, false);
        caption(tree, c, "受け入れ基準のまとめ");
        body(tree, c, session.criteria_summary.clone());
    }
    note(
        tree,
        list,
        "基準を選ぶと各検証者の結果と証跡が、不具合を選ぶと出典と追試がここに表示されます",
    );
}
