//! Center pane "Timeline" tab: chronological log of submissions, judge
//! evaluations, user interventions, and final verdicts.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Container, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::types::{Author, MsgKind, PartId, Session};

use super::{caption, gap, keep_scroll, note, scroll_column, tag};

fn kind_badge(kind: MsgKind) -> (&'static str, Tone) {
    match kind {
        MsgKind::Submission => ("提出", Tone::Neutral),
        MsgKind::Judgment => ("審判講評", Tone::Accent),
        MsgKind::Intervention => ("ユーザー指示", Tone::Warning),
        MsgKind::FinalVerdict => ("最終結果", Tone::Success),
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filter_part: Option<&PartId>,
) {
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "timeline");

    let mut shown = 0usize;
    for msg in &session.messages {
        if let Some(filter) = filter_part {
            let matches = match &msg.author {
                Author::Part(id) => id == filter,
                Author::User => msg.to.is_empty() || msg.to.contains(filter),
            };
            if !matches {
                continue;
            }
        }
        shown += 1;
        let card = tree.insert(
            Some(list),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
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
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(8.0),
        );
        let head = tree.insert(
            Some(card),
            Container::row()
                .align_items(AlignItems::CENTER)
                .gap(crate::appearance::space(6.0)),
        );
        let (label, tone) = kind_badge(msg.kind);
        tag(tree, head, format!("R{} · {label}", msg.round), tone);
        tree.insert(
            Some(head),
            Label::new(session.author_name(&msg.author))
                .font_size(crate::appearance::font(12.0))
                .weight(600),
        );
        if !msg.to.is_empty() {
            let to: Vec<&str> = msg.to.iter().map(|p| p.as_str()).collect();
            caption(tree, head, format!("→ {}", to.join(", ")));
        }
        tree.insert(
            Some(card),
            Label::new(msg.body.clone())
                .font_size(crate::appearance::font(12.0))
                .wrap(true),
        );
    }

    if shown == 0 {
        note(tree, list, "まだログがありません。");
    }
}
