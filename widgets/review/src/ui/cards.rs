//! The left pane: the target card (returned, so the screen can add
//! スナップショットを開く to it), one card per reviewer (lens, model,
//! this round's receipt, what it wrote, its pane), and the lead's card.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone, Truncation};
use noa_gpui_core::{UiTree, WidgetId};

use review::model::types::{Millis, PartId, Participant, Session, short_id};

use super::progress::{delivery, elapsed};
use super::{Action, caption, card, heading, keep_scroll, note, scroll_column, spacer, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filtered: Option<&PartId>,
    live: bool,
    now: Millis,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    heading(tree, host, "対象とパネル");
    let list = scroll_column(tree, host, 8.0);
    keep_scroll(tree, list, session, "cards");

    let m = &session.manifest;
    let target = card(tree, list, false);
    let head = tree.insert(
        Some(target),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    tag(tree, head, session.target.kind.label(), Tone::Neutral);
    if !m.omitted.is_empty() {
        tag(
            tree,
            head,
            format!("対象外 {}", m.omitted.len()),
            Tone::Warning,
        );
    }
    tree.insert(
        Some(target),
        Label::new(m.label.clone())
            .font_size(12.5)
            .weight(600)
            .wrap(true),
    );
    if let (Some(base), Some(head_sha)) = (&m.base_sha, &m.head_sha) {
        caption(
            tree,
            target,
            format!(
                "{} {} … {} {}",
                m.base_ref.as_deref().unwrap_or(""),
                short_id(base, 8),
                m.head_ref.as_deref().unwrap_or("作業ツリー"),
                short_id(head_sha, 8)
            ),
        );
    }
    let mut stat = format!("{} ファイル", m.files.len());
    if m.additions() > 0 || m.deletions() > 0 {
        stat.push_str(&format!("  +{} −{}", m.additions(), m.deletions()));
    }
    caption(tree, target, stat);

    let round = session.current_round();
    for part in &session.reviewers {
        let selected = filtered == Some(&part.id);
        let c = card(tree, list, selected);
        let top = tree.insert(
            Some(c),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        tree.insert(
            Some(top),
            Label::new(part.id.to_string()).font_size(13.0).weight(600),
        );
        if let Some(lens) = &part.lens {
            tag(tree, top, lens.label(), Tone::Accent);
        }
        let line = tree.insert(
            Some(c),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        caption(tree, line, setup_line(part));
        spacer(tree, line);
        receipt_tag(tree, line, session, part, now);
        let raw = session.raw.iter().filter(|r| r.key.part == part.id).count();
        let stances: usize = session
            .findings
            .iter()
            .filter_map(|f| f.stances.get(&part.id))
            .map(Vec::len)
            .sum();
        caption(tree, c, format!("指摘 {raw} 件 · スタンス {stances} 件"));
        if let Some(summary) = round
            .and_then(|r| r.reviews.get(&part.id))
            .map(|n| n.summary.clone())
        {
            tree.insert(
                Some(c),
                Label::new(summary)
                    .font_size(11.5)
                    .wrap(true)
                    .max_lines(3)
                    .truncation(Truncation::Tail),
            );
        }
        let buttons = tree.insert(Some(c), Container::row().gap(6.0));
        let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
        let filter = tree.insert(
            Some(buttons),
            small(if selected {
                "絞り込み解除"
            } else {
                "指摘を絞り込む"
            }),
        );
        actions.push((
            filter,
            Action::FilterReviewer((!selected).then(|| part.id.clone())),
        ));
        if live {
            let open = tree.insert(Some(buttons), small("ペインを開く"));
            actions.push((open, Action::FocusPane(part.id.clone())));
        }
    }

    let lead = card(tree, list, false);
    let top = tree.insert(
        Some(lead),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    tree.insert(Some(top), Label::new("リード").font_size(13.0).weight(600));
    let line = tree.insert(
        Some(lead),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(tree, line, setup_line(&session.lead));
    spacer(tree, line);
    receipt_tag(tree, line, session, &session.lead, now);
    note(
        tree,
        lead,
        "毎ラウンド指摘を統合し、最終ラウンドで総合判定を出します",
    );
    if live {
        let open = tree.insert(
            Some(lead),
            Button::new("ペインを開く").control_size(ControlSize::Small),
        );
        actions.push((open, Action::FocusPane(session.lead.id.clone())));
    }
    target
}

fn setup_line(part: &Participant) -> String {
    match part.setup.label() {
        Some(setup) => format!("{} · {setup}", part.kind.command()),
        None => part.kind.command().to_owned(),
    }
}

fn receipt_tag(
    tree: &mut UiTree,
    parent: WidgetId,
    session: &Session,
    part: &Participant,
    now: Millis,
) {
    let Some(brief) = session.current_round().and_then(|r| r.brief(&part.id)) else {
        if !session.closed {
            tag(tree, parent, "待機中", Tone::Neutral);
        }
        return;
    };
    let (label, tone) = delivery(&brief.receipt);
    tag(tree, parent, label, tone);
    if let Some(time) = elapsed(&brief.receipt, now).filter(|_| !session.closed) {
        caption(tree, parent, time);
    }
}
