//! The minutes: the moderator's decisions, contested points, open questions
//! and next actions, each item linked to its issues.

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use discussion::model::types::{AgentKind, MinuteSection, Session};

use super::chat::issue_link;
use super::{Action, caption, gap, heading, scroll_column, tag};

pub(crate) fn section_title(section: MinuteSection) -> &'static str {
    match section {
        MinuteSection::Decisions => "決定事項",
        MinuteSection::Contested => "対立点",
        MinuteSection::OpenQuestions => "未解決の問い",
        MinuteSection::Actions => "次のアクション",
    }
}

fn section_tone(section: MinuteSection) -> Tone {
    match section {
        MinuteSection::Decisions => Tone::Success,
        MinuteSection::Contested => Tone::Warning,
        MinuteSection::OpenQuestions => Tone::Accent,
        MinuteSection::Actions => Tone::Neutral,
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "議事録");
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    let tools = tree.insert(Some(host), Container::wrapping_row(6.0, 6.0));
    let copy = tree.insert(Some(tools), small("結果をコピー"));
    actions.push((copy, Action::CopyResult));
    let export = tree.insert(Some(tools), small("議事録を書き出す…"));
    actions.push((export, Action::ExportMinutes));
    // An implementation needs a repository to work in.
    let implement = tree.insert(
        Some(host),
        Container::wrapping_row(6.0, 6.0).align_items(noa_gpui_core::style::AlignItems::CENTER),
    );
    if session.workdir.is_some() {
        caption(tree, implement, "実装を依頼:");
        for kind in AgentKind::ALL {
            let button = tree.insert(Some(implement), small(kind.command()));
            actions.push((button, Action::Implement(kind)));
        }
    } else {
        caption(
            tree,
            implement,
            "実装の依頼は、作業ディレクトリを指定した議論で使えます",
        );
    }
    let list = scroll_column(tree, host, 14.0);
    for section in MinuteSection::ALL {
        let items = session.minutes.section(section);
        let block = tree.insert(Some(list), Container::column().gap(6.0));
        let title = tree.insert(
            Some(block),
            Container::row()
                .align_items(noa_gpui_core::style::AlignItems::CENTER)
                .gap(6.0),
        );
        tag(tree, title, section_title(section), section_tone(section));
        caption(tree, title, items.len().to_string());
        if items.is_empty() {
            caption(tree, block, "（なし）");
        }
        for item in items {
            let entry = tree.insert(
                Some(block),
                Container::new(Style {
                    flex_direction: FlexDirection::Column,
                    gap: gap(4.0),
                    padding: LayoutRect {
                        left: length(10.0),
                        right: length(8.0),
                        top: length(6.0),
                        bottom: length(6.0),
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
                Label::new(item.text.clone()).font_size(12.0).wrap(true),
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
