//! The issue tree: the topic, then each issue's state and stance summary.
//! Selecting an issue reveals individual stances and filters the chat; ⚔
//! on a contested issue drafts a debate.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Separator};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use discussion::model::types::{Issue, IssueId, IssueState, Position, Session};

use super::{
    Action, caption, debate_in_play, gap, heading, keep_scroll, note, outlined, part_name,
    position_tag, scroll_column, state_tag, tag,
};

const INDENT: f32 = 16.0;

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filter: Option<&IssueId>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let head = heading(tree, host, "論点ツリー");
    if filter.is_some() {
        super::spacer(tree, head);
        let clear = tree.insert(
            Some(head),
            Button::new("絞り込み解除").control_size(ControlSize::Mini),
        );
        actions.push((clear, Action::ClearFilter));
    }
    let list = scroll_column(tree, host, 6.0);
    keep_scroll(tree, list, session, "tree");

    caption(tree, list, "議題");
    tree.insert(
        Some(list),
        Label::new(session.topic.clone())
            .font_size(14.0)
            .weight(600)
            .wrap(true),
    );
    note(
        tree,
        list,
        format!(
            "司会 Noa ({}) · パネル {}",
            session.moderator.kind.command(),
            session
                .panelists
                .iter()
                .map(|p| part_name(&p.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    tree.insert(Some(list), Separator::horizontal());

    if session.issues.issues.is_empty() {
        note(
            tree,
            list,
            "論点はまだありません。第 1 ラウンドの要約で司会が立てます",
        );
        return;
    }
    let round = session.current_round().map_or(0, |r| r.no);
    // ⚔ only where the engine would take it: one debate at a time.
    let can_debate = debate_in_play(session).is_none() && !session.closed;
    for (depth, issue) in session.issues.walk() {
        let selected = filter == Some(&issue.id);
        let row = tree.insert(
            Some(list),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::START,
                flex_shrink: 0.0,
                gap: gap(4.0),
                padding: LayoutRect {
                    left: length(depth as f32 * INDENT),
                    right: length(0.0),
                    top: length(0.0),
                    bottom: length(0.0),
                },
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        let node = issue_node(tree, row, session, issue, round, selected);
        actions.push((node, Action::Filter(issue.id.clone())));
        if can_debate && issue.state == IssueState::Contested {
            let sword = tree.insert(
                Some(row),
                Button::new("⚔ 討論")
                    .tooltip(format!("{} を討論にかける", issue.id))
                    .control_size(ControlSize::Small),
            );
            actions.push((sword, Action::DraftDebate(issue.id.clone())));
        }
    }
}

/// The title leads the row. Individual stances are shown for the selected
/// issue; every other issue keeps the distribution visible in one line.
fn issue_node(
    tree: &mut UiTree,
    row: WidgetId,
    session: &Session,
    issue: &Issue,
    round: u32,
    selected: bool,
) -> WidgetId {
    let node = tree.insert(
        Some(row),
        Pressable::new(Style {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(0.0),
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            gap: gap(7.0),
            padding: LayoutRect::length(8.0),
            ..Default::default()
        })
        .label(format!("{} {}", issue.id, issue.title))
        .selected(selected),
    );
    let title = tree.insert(
        Some(node),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    let (state, tone) = state_tag(issue.state);
    tag(tree, title, state, tone);
    tree.insert(
        Some(title),
        Label::new(issue.id.to_string())
            .font_size(12.0)
            .color(Role::Muted),
    );
    // New: added by the summary that opened this round, or in it.
    if round > 0 && issue.opened_in + 1 >= round {
        caption(tree, title, "新規");
    }
    tree.insert(
        Some(node),
        Label::new(issue.title.clone())
            .font_size(14.0)
            .weight(600)
            .wrap(true),
    );

    if !selected {
        note(tree, node, stance_summary(session, issue));
        return node;
    }

    let stances = tree.insert(
        Some(node),
        Container::wrapping_row(4.0, 4.0).align_items(AlignItems::CENTER),
    );
    for part in &session.panelists {
        let pos = issue.stances.get(&part.id).map(|s| s.pos);
        let (text, _) = position_tag(pos);
        let chip = format!("{} {text}", part_name(&part.id));
        outlined(tree, stances, &chip);
    }
    node
}

fn stance_summary(session: &Session, issue: &Issue) -> String {
    let mut counts = [0; 4];
    for part in &session.panelists {
        let index = match issue.stances.get(&part.id).map(|stance| stance.pos) {
            Some(Position::For) => 0,
            Some(Position::Against) => 1,
            Some(Position::Neutral) => 2,
            Some(Position::Unknown) | None => 3,
        };
        counts[index] += 1;
    }
    ["賛成", "反対", "中立", "未表明"]
        .into_iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{label} {count}"))
        .collect::<Vec<_>>()
        .join(" · ")
}
