//! The minutes: the moderator's decisions, contested points, open questions
//! and next actions, each item linked to its issues.

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use discussion::model::types::{MinuteSection, Session};

use super::chat::issue_link;
use super::{Action, caption, gap, heading, keep_scroll, scroll_column};

pub(crate) fn section_title(section: MinuteSection) -> &'static str {
    match section {
        MinuteSection::Decisions => "決定事項",
        MinuteSection::Contested => "対立点",
        MinuteSection::OpenQuestions => "未解決の問い",
        MinuteSection::Actions => "次のアクション",
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "議事録");
    let list = scroll_column(tree, host, 18.0);
    keep_scroll(tree, list, session, "minutes");
    for section in [
        MinuteSection::Decisions,
        MinuteSection::Actions,
        MinuteSection::Contested,
        MinuteSection::OpenQuestions,
    ] {
        let items = session.minutes.section(section);
        let block = tree.insert(Some(list), Container::column().gap(6.0));
        let title = tree.insert(
            Some(block),
            Container::row()
                .align_items(noa_gpui_core::style::AlignItems::CENTER)
                .gap(6.0),
        );
        tree.insert(
            Some(title),
            Label::new(section_title(section))
                .font_size(13.0)
                .weight(600),
        );
        caption(tree, title, items.len().to_string());
        for item in items {
            let entry = tree.insert(
                Some(block),
                Container::new(Style {
                    flex_direction: FlexDirection::Column,
                    gap: gap(4.0),
                    padding: LayoutRect {
                        left: length(10.0),
                        right: length(8.0),
                        top: length(10.0),
                        bottom: length(10.0),
                    },
                    size: LayoutSize {
                        width: noa_gpui_core::style::percent(1.0),
                        height: auto(),
                    },
                    ..Default::default()
                })
                .background(Role::SurfaceAlt)
                .radius(6.0),
            );
            tree.insert(
                Some(entry),
                Label::new(item.text.clone())
                    .font_size(14.0)
                    .wrap(true)
                    .selectable(true),
            );
            let links = tree.insert(
                Some(entry),
                Container::wrapping_row(4.0, 4.0)
                    .align_items(noa_gpui_core::style::AlignItems::CENTER),
            );
            for issue in &item.issues {
                issue_link(tree, links, issue, actions);
            }
            caption(tree, links, format!("R{}〜", item.since));
        }
    }
}
