//! The meeting chat: the pinned brief of the current round with one receipt
//! badge per recipient, then every message of the log (or those about the
//! filtered issue), with issue tags and reply links.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::Container;
use noa_gpui_core::widgets::{Button, ControlSize, Label, Pressable, Tag, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use discussion::model::types::{IssueId, Message, MsgKind, RoundKind, Session};

use super::{
    Action, Delivery, author_color, author_name, caption, debate_in_play, gap, is_about,
    kind_label, outlined, part_name, phase_label, spacer, subtree, tag, tail_column,
};

/// The messages the chat shows: all of them, or those about `filter` and
/// its sub-issues.
pub(crate) fn visible<'a>(session: &'a Session, filter: Option<&IssueId>) -> Vec<&'a Message> {
    match filter {
        None => session.messages.iter().collect(),
        Some(root) => {
            let issues = subtree(session, root);
            session
                .messages
                .iter()
                .filter(|m| is_about(session, m, &issues))
                .collect()
        }
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filter: Option<&IssueId>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    brief_bar(tree, host, session, actions);
    debate_banner(tree, host, session, actions);
    if let Some(issue) = filter {
        filter_bar(tree, host, session, issue, actions);
    }
    let list = tail_column(tree, host, 8.0);
    let messages = visible(session, filter);
    if messages.is_empty() {
        caption(
            tree,
            list,
            if filter.is_some() {
                "この論点に関する発言はまだありません"
            } else {
                "まだ発言はありません。最初のブリーフが届くとここに並びます"
            },
        );
    }
    for message in messages {
        message_card(tree, list, session, message, actions);
    }
}

fn bar(tree: &mut UiTree, host: WidgetId, background: Role) -> WidgetId {
    tree.insert(
        Some(host),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            flex_wrap: noa_gpui_core::style::FlexWrap::Wrap,
            align_items: AlignItems::CENTER,
            flex_shrink: 0.0,
            gap: gap(6.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(background),
    )
}

/// 📌 The current round's brief: who got it and how far each got.
fn brief_bar(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let row = bar(tree, host, Role::SurfaceAlt);
    let Some(round) = session.current_round() else {
        caption(tree, row, "ブリーフはまだ送られていません");
        return;
    };
    tree.insert(
        Some(row),
        Label::new(format!("▍ブリーフ R{}", round.no))
            .font_size(12.5)
            .weight(600),
    );
    if let RoundKind::Debate(id) = &round.kind {
        tag(tree, row, &format!("⚔ 討論 {id}"), Tone::Warning);
    }
    caption(tree, row, phase_label(round.phase));
    let mut failed = false;
    for brief in round.briefs.iter().chain(&round.moderator_brief) {
        let delivery = Delivery::of(&brief.receipt);
        failed |= delivery == Delivery::Failed;
        let (state, tone) = delivery.tag();
        let id = tag(
            tree,
            row,
            &format!("{} {state}", part_name(&brief.to)),
            tone,
        );
        if let Some(why) = &brief.receipt.failed {
            tree.set_accessible_label(id, format!("{}: {why}", part_name(&brief.to)));
        }
    }
    if failed {
        spacer(tree, row);
        let retry = tree.insert(
            Some(row),
            Button::new("失敗分を再送").control_size(ControlSize::Small),
        );
        actions.push((retry, Action::Retry));
    }
}

/// A debate waiting for the user, or running: one click away.
fn debate_banner(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let Some(debate) = debate_in_play(session) else {
        return;
    };
    let row = bar(tree, host, Role::Surface);
    let title = session
        .issues
        .get(&debate.issue)
        .map_or(String::new(), |i| i.title.clone());
    let (text, button) = match debate.status {
        discussion::model::types::DebateStatus::Proposed => (
            format!("Noa が {} の討論を提案しています: {title}", debate.issue),
            "確認する",
        ),
        _ => (
            format!("討論 {} が進行中: {} {title}", debate.id, debate.issue),
            "討論を表示",
        ),
    };
    tag(tree, row, "⚔", Tone::Warning);
    tree.insert(
        Some(row),
        Label::new(text)
            .font_size(12.0)
            .truncation(Truncation::Tail),
    );
    spacer(tree, row);
    let open = tree.insert(
        Some(row),
        Button::new(button)
            .default_button(true)
            .control_size(ControlSize::Small),
    );
    actions.push((open, Action::ShowDebate(debate.id.clone())));
}

fn filter_bar(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    issue: &IssueId,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let row = bar(tree, host, Role::Surface);
    caption(tree, row, "絞り込み中");
    tag(tree, row, issue.as_str(), Tone::Accent);
    if let Some(found) = session.issues.get(issue) {
        tree.insert(
            Some(row),
            Label::new(found.title.clone())
                .font_size(12.0)
                .truncation(Truncation::Tail),
        );
    }
    spacer(tree, row);
    let clear = tree.insert(
        Some(row),
        Button::new("すべて表示").control_size(ControlSize::Small),
    );
    actions.push((clear, Action::ClearFilter));
}

/// One message: author, kind, addressees, issue tags (click to filter),
/// reply links, then the full body.
pub(super) fn message_card(
    tree: &mut UiTree,
    parent: WidgetId,
    session: &Session,
    message: &Message,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    let question = message.kind == MsgKind::Brief;
    let emphasis = matches!(message.kind, MsgKind::Summary | MsgKind::Verdict);
    let mut card = Container::new(Style {
        flex_direction: FlexDirection::Column,
        flex_shrink: 0.0,
        gap: gap(4.0),
        padding: LayoutRect {
            left: length(10.0),
            right: length(10.0),
            top: length(if question { 5.0 } else { 8.0 }),
            bottom: length(if question { 5.0 } else { 8.0 }),
        },
        size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..Default::default()
    })
    .radius(6.0);
    card = match message.kind {
        MsgKind::Brief => card,
        MsgKind::Intervention => card
            .background(noa_gpui_scene::Color::rgba(1.0, 0.8, 0.0, 0.08))
            .border(1.0, noa_gpui_scene::Color::rgba(1.0, 0.8, 0.0, 0.35)),
        _ if emphasis => card
            .background(Role::Surface)
            .border(1.0, noa_gpui_scene::Color::rgba(0.39, 0.82, 1.0, 0.45)),
        _ => card.background(Role::Surface),
    };
    let card = tree.insert(Some(parent), card);

    let head = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tree.insert(
        Some(head),
        Label::new(author_name(&message.author))
            .font_size(12.0)
            .weight(600)
            .color(author_color(session, &message.author)),
    );
    if !message.to.is_empty() {
        caption(
            tree,
            head,
            format!(
                "→ {}",
                message
                    .to
                    .iter()
                    .map(part_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    outlined(tree, head, kind_label(message.kind));
    for issue in &message.issues {
        issue_link(tree, head, issue, actions);
    }
    for target in &message.reply_to {
        let who = session
            .message(target)
            .map_or_else(|| target.to_string(), |m| author_name(&m.author));
        caption(tree, head, format!("↪ {who} ({target})"));
    }
    caption(tree, head, format!("R{} · {}", message.round, message.id));

    tree.insert(
        Some(card),
        Label::new(message.body.clone())
            .font_size(if question { 11.5 } else { 12.5 })
            .color(if question { Role::Muted } else { Role::Text })
            .wrap(true)
            .selectable(true),
    );
    card
}

/// `[B]`: a tag that filters the chat to the issue.
pub(super) fn issue_link(
    tree: &mut UiTree,
    parent: WidgetId,
    issue: &IssueId,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    let link = tree.insert(
        Some(parent),
        Pressable::new(Style::default())
            .label(format!("{issue} で絞り込む"))
            .radius(9.0),
    );
    tree.insert(
        Some(link),
        Tag::new(format!("[{issue}]")).tone(Tone::Accent),
    );
    actions.push((link, Action::Filter(issue.clone())));
    link
}
