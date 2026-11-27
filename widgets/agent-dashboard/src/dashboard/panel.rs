//! The parts the worker list (and the views that group workers) are built
//! from.

use noa_gpui_core::style::{
    FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Container, EmptyState, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::one_line;
use crate::live::Link;

use super::pager::PageMemory;
use super::style::{caption, fill_remaining, gap};
use super::{Mark, Registry};

/// The pager's "上に N 件 · 下に M 件" label, hidden until something is
/// hidden, followed in `row` by the list's page memory (`pager::fit`), which
/// takes no space.
pub(super) fn indicator(tree: &mut UiTree, row: WidgetId) -> WidgetId {
    let id = caption(tree, row, "");
    tree.set_hidden(id, true);
    let memory = tree.insert(Some(row), PageMemory::default());
    tree.set_hidden(memory, true);
    id
}

/// A pageable list, `gap` between its items: a clipped column, not a
/// scroll view, so that it shows whole items only (`pager::fit` hides those
/// that would be cut). Its items are its direct children.
pub(super) fn card_list(tree: &mut UiTree, parent: WidgetId, gap_px: f32) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(gap_px),
            ..fill_remaining()
        })
        .clip(true),
    )
}

/// A group's name and size. The name is a worker's text (an orchestrator,
/// an agent, a model): drawn on one line ([`one_line`]).
pub(super) fn group_header(tree: &mut UiTree, parent: WidgetId, name: &str, n: usize) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(format!("{} · {n}", one_line(name)))
            .font_size(crate::appearance::font(11.0))
            .weight(700)
            .color(Role::Muted),
    )
}

/// The body with no workers at all: why.
pub(super) fn empty(tree: &mut UiTree, body: WidgetId, link: &Link) {
    let state = match link {
        Link::Connecting => EmptyState::new("Noa に接続しています…"),
        Link::Live => EmptyState::new("エージェントのペインが見つかりません").message(
            "claude / codex / agy が動いているペインと、+ctl で起動したワーカーがここに出ます",
        ),
        // On one line, as the header shows it: a reason with a line break
        // would make the message two lines however short it is.
        Link::Down(reason) => EmptyState::new("エージェントのペインが見つかりません")
            .message(format!("Noa に接続できません: {}", one_line(reason))),
    };
    state.width(560.0).insert(tree, body);
}

/// Adds the selection bar to `card` as its last child: absolutely placed at
/// its left edge, so it changes no layout, and hidden until `pane` is
/// selected (`Dashboard::paint_selection`).
pub(super) fn mark_card(tree: &mut UiTree, card: WidgetId, pane: u64, reg: &mut Registry) {
    let bar = tree.insert(
        Some(card),
        Container::new(Style {
            position: Position::Absolute,
            inset: LayoutRect {
                left: length(0.0),
                right: auto(),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: length(4.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::Accent)
        .radius(2.0),
    );
    tree.set_hidden(bar, true);
    reg.marks.push((bar, pane, Mark::Bar));
}
