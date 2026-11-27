//! The right pane: the selected finding's thread (its claim and the lead's
//! decision, every raw finding it is made of, each reviewer's stance by
//! round, and how its status moved), or the lead's latest summary and the
//! verdict when nothing is selected.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, UiTree, WidgetId};

use review::model::types::{FindingStatus, Session, StanceKind};

use super::fix::{self, FixOffer};
use super::{
    Action, body, caption, card, decision_tone, heading, keep_scroll, note, scroll_column,
    severity_tone, status_tone, tag,
};

fn stance_tone(kind: StanceKind) -> Tone {
    match kind {
        StanceKind::Confirm => Tone::Success,
        StanceKind::Dispute => Tone::Critical,
        StanceKind::Refine => Tone::Accent,
        StanceKind::Unsure | StanceKind::Withdraw => Tone::Neutral,
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    selected: Option<&str>,
    has_files: bool,
    offer: Option<&FixOffer>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let Some(f) = selected.and_then(|id| session.finding(id)) else {
        overview(tree, host, session, offer, actions);
        return;
    };
    heading(tree, host, &format!("指摘 {}", f.id));
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, &format!("thread-{}", f.id));

    let head = tree.insert(
        Some(list),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tag(tree, head, f.severity.label(), severity_tone(f.severity));
    tag(
        tree,
        head,
        session.category_label(&f.category),
        Tone::Neutral,
    );
    tag(tree, head, f.status.label(), status_tone(f.status));
    caption(tree, head, f.confidence.label());
    tree.insert(
        Some(list),
        Label::new(f.claim.clone())
            .font_size(crate::appearance::font(13.5))
            .weight(600)
            .wrap(true),
    );
    let tally = session.tally(f);
    caption(
        tree,
        list,
        format!("{}（{}）", tally.label(), tally.agreement.label()),
    );
    let loc_row = tree.insert(
        Some(list),
        Container::row()
            .gap(crate::appearance::space(6.0))
            .align_items(AlignItems::CENTER),
    );
    match &f.location {
        Some(loc) => {
            caption(tree, loc_row, loc.to_string());
            if !f.in_target {
                tag(tree, loc_row, "対象外", Tone::Warning);
            } else if f.in_diff == Some(false) {
                tag(tree, loc_row, "差分外", Tone::Neutral);
            }
            if has_files && f.in_target {
                let open = tree.insert(
                    Some(loc_row),
                    Button::new("ファイルを開く").control_size(ControlSize::Small),
                );
                actions.push((open, Action::OpenSnapshot(Some(loc.path.clone()))));
            }
        }
        None => caption(tree, loc_row, "対象全体"),
    }
    if f.status == FindingStatus::Merged
        && let Some(into) = &f.merged_into
    {
        let go = tree.insert(
            Some(list),
            Button::new(format!("統合先 {into} を表示")).control_size(ControlSize::Small),
        );
        actions.push((go, Action::SelectFinding(Some(into.clone()))));
    }
    let lead = card(tree, list, true);
    caption(tree, lead, "リードの判断");
    body(tree, lead, f.rationale.clone());
    if let Some(fix) = &f.fix {
        caption(tree, lead, "修正案");
        body(tree, lead, fix.clone());
    }
    if let Some(offer) = offer {
        fix::insert(tree, list, offer, actions);
    }

    caption(tree, list, "出典");
    for key in &f.sources {
        let Some(raw) = session.raw_finding(key) else {
            continue;
        };
        let c = card(tree, list, false);
        let top = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(key.to_string())
                .font_size(crate::appearance::font(12.0))
                .weight(600),
        );
        caption(tree, top, format!("R{}", raw.round));
        tag(tree, top, raw.severity.label(), severity_tone(raw.severity));
        caption(tree, top, raw.confidence.label());
        body(tree, c, raw.claim.clone());
        caption(tree, c, "根拠");
        body(tree, c, raw.evidence.clone());
        if let Some(fix) = &raw.fix {
            caption(tree, c, "修正案");
            body(tree, c, fix.clone());
        }
    }

    caption(tree, list, "スタンス");
    if f.stances.is_empty() {
        note(
            tree,
            list,
            "まだスタンスはありません（相互検証ラウンドで集まります）",
        );
    }
    for (part, stances) in &f.stances {
        for s in stances {
            let c = card(tree, list, false);
            let top = tree.insert(
                Some(c),
                Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
            );
            tree.insert(
                Some(top),
                Label::new(part.to_string())
                    .font_size(crate::appearance::font(12.0))
                    .weight(600),
            );
            caption(tree, top, format!("R{}", s.round));
            tag(tree, top, s.kind.label(), stance_tone(s.kind));
            if let Some(sev) = s.severity {
                caption(tree, top, format!("重大度の提案: {}", sev.label()));
            }
            if !s.note.is_empty() {
                body(tree, c, s.note.clone());
            }
            if let Some(evidence) = &s.evidence {
                caption(tree, c, "根拠");
                body(tree, c, evidence.clone());
            }
        }
    }

    caption(tree, list, "履歴");
    for (round, status, severity) in &f.history {
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
    actions.push((back, Action::SelectFinding(None)));
}

fn overview(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    offer: Option<&FixOffer>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "取りまとめ");
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "overview");
    match session.verdict() {
        Some(v) => {
            let c = card(tree, list, true);
            let top = tree.insert(
                Some(c),
                Container::row()
                    .gap(crate::appearance::space(6.0))
                    .align_items(AlignItems::CENTER),
            );
            caption(tree, top, "総合判定");
            tag(tree, top, v.decision.label(), decision_tone(v.decision));
            body(tree, c, v.rationale.clone());
            if !v.must_fix.is_empty() {
                caption(tree, c, format!("必ず直すもの: {}", v.must_fix.join(", ")));
            }
        }
        None if session.closed => note(tree, list, "判定なし（ユーザーが終了）"),
        None => note(tree, list, "総合判定は最終ラウンドの取りまとめで出ます"),
    }
    if let Some(offer) = offer {
        fix::insert(tree, list, offer, actions);
    }
    let summaries: Vec<_> = session
        .rounds
        .iter()
        .rev()
        .filter_map(|r| r.outcome.as_ref().map(|o| (r.no, r.kind, o)))
        .collect();
    if summaries.is_empty() {
        note(tree, list, "リードの取りまとめはまだありません");
    }
    for (no, kind, outcome) in summaries {
        let c = card(tree, list, false);
        caption(tree, c, format!("R{no}（{}）のまとめ", kind.label()));
        body(tree, c, outcome.summary.clone());
        if !outcome.questions.is_empty() {
            for (part, q) in &outcome.questions {
                tree.insert(
                    Some(c),
                    Label::new(format!("→ {part}: {q}"))
                        .font_size(crate::appearance::font(11.5))
                        .color(Role::Muted)
                        .wrap(true),
                );
            }
        }
    }
    note(
        tree,
        list,
        "指摘を選ぶと、各レビュアーのスタンスがここに表示されます",
    );
}
