use noa_gpui_core::style::{
    AlignItems, FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Badge, Container, EmptyState, Kbd, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::live::Link;

use super::style::{caption, fill_remaining, fixed_width, gap};
use super::{Mark, Registry};

pub(super) const QUEUE_WIDTH: f32 = 400.0;
pub(super) const DONE_WIDTH: f32 = 270.0;

pub(super) const GRAPH_WIDTH: f32 = 430.0;
pub(super) const HISTORY_HEIGHT: f32 = 256.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    Queue,
    Running,
    Done,
    Timeline,
    Graph,
}

impl Panel {
    pub(super) const ALL: [Panel; 5] = [
        Panel::Queue,
        Panel::Running,
        Panel::Done,
        Panel::Timeline,
        Panel::Graph,
    ];
    pub(super) const NOW: [Panel; 3] = [Panel::Queue, Panel::Running, Panel::Done];
    pub(super) const HISTORY: [Panel; 2] = [Panel::Timeline, Panel::Graph];

    pub(super) fn title(self) -> &'static str {
        match self {
            Panel::Queue => "要対応",
            Panel::Running => "作業中",
            Panel::Done => "完了",
            Panel::Timeline => "状態の推移",
            Panel::Graph => "関連",
        }
    }

    /// The digit key that shows and hides the panel.
    pub(super) fn digit(self) -> &'static str {
        ["1", "2", "3", "4", "5"][self as usize]
    }

    pub(super) fn grows(self) -> bool {
        matches!(self, Panel::Running | Panel::Timeline)
    }

    pub(super) fn grower(self) -> Panel {
        if Self::ALL[..3].contains(&self) {
            Panel::Running
        } else {
            Panel::Timeline
        }
    }

    pub(super) fn width(self) -> f32 {
        match self {
            Panel::Queue => QUEUE_WIDTH,
            Panel::Done => DONE_WIDTH,
            Panel::Graph => GRAPH_WIDTH,
            Panel::Running | Panel::Timeline => 0.0,
        }
    }

    pub(super) fn style(self, shown: &[bool; 5]) -> Style {
        if self.grows() {
            fill_remaining()
        } else if shown[self.grower() as usize] {
            fixed_width(self.width())
        } else {
            Style {
                flex_grow: 1.0,
                flex_basis: length(self.width()),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }
        }
    }
}

pub(super) fn history_row(now_shown: bool) -> Style {
    if now_shown {
        Style {
            size: LayoutSize {
                width: auto(),
                height: length(HISTORY_HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }
    } else {
        fill_remaining()
    }
}

pub(super) fn panel(tree: &mut UiTree, parent: WidgetId, style: Style) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            padding: LayoutRect::length(10.0),
            ..style
        })
        .background(Role::Surface)
        .radius(10.0),
    )
}

/// A panel's title row: its shortcut keycap, then its title. Returns the
/// row for the caller to add to.
pub(super) fn title_row(tree: &mut UiTree, parent: WidgetId, panel: Panel, gap: f32) -> WidgetId {
    let row = tree.insert(
        Some(parent),
        Container::row().align_items(AlignItems::CENTER).gap(gap),
    );
    tree.insert(Some(row), Kbd::new([panel.digit()]));
    tree.insert(
        Some(row),
        Label::new(panel.title())
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    row
}

/// The pager's "上に N 件 · 下に M 件" label, hidden until something is
/// hidden.
pub(super) fn indicator(tree: &mut UiTree, row: WidgetId) -> WidgetId {
    let id = caption(tree, row, "");
    tree.set_hidden(id, true);
    id
}

/// The title row of a card panel: keycap, title and count badge. Returns
/// the row.
pub(super) fn panel_title(
    tree: &mut UiTree,
    parent: WidgetId,
    panel: Panel,
    n: usize,
    tone: Tone,
) -> WidgetId {
    let row = title_row(tree, parent, panel, 8.0);
    tree.insert(Some(row), Badge::count(n as u32).tone(tone));
    row
}

/// A panel list, `gap` between its items: a clipped column, not a scroll
/// view, so that it shows whole items only (`pager::fit` hides those that
/// would be cut). Its items are its direct children (the queue reorders its
/// cards by child index).
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

/// What a panel shows when the filter leaves it nothing.
pub(super) fn nothing_matches(tree: &mut UiTree, list: WidgetId) {
    caption(tree, list, "該当なし");
}

pub(super) fn group_header(tree: &mut UiTree, parent: WidgetId, name: &str, n: usize) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(format!("{name} · {n}"))
            .font_size(11.0)
            .weight(700)
            .color(Role::Muted),
    )
}

/// The body with no workers: why, and each panel's shortcut and whether it
/// is shown (no panel title is on screen to say so, and the header's chips
/// name only the hidden panels).
pub(super) fn empty(tree: &mut UiTree, body: WidgetId, link: &Link, shown: &[bool; 5]) {
    let state = match link {
        Link::Connecting => EmptyState::new("Noa に接続しています…"),
        Link::Live => EmptyState::new("エージェントのペインが見つかりません").message(
            "claude / codex / agy が動いているペインと、+ctl で起動したワーカーがここに出ます",
        ),
        Link::Down(reason) => EmptyState::new("エージェントのペインが見つかりません")
            .message(format!("Noa に接続できません: {reason}")),
    };
    let parts = state.width(560.0).insert(tree, body);
    let keys = tree.insert(
        Some(parts.root),
        Container::row().align_items(AlignItems::CENTER).gap(12.0),
    );
    for panel in Panel::ALL {
        let key = tree.insert(
            Some(keys),
            Container::row().align_items(AlignItems::CENTER).gap(3.0),
        );
        tree.insert(Some(key), Kbd::new([panel.digit()]));
        let label = if shown[panel as usize] {
            Label::new(panel.title())
        } else {
            Label::new(format!("{}（非表示）", panel.title())).color(Role::Muted)
        };
        tree.insert(Some(key), label.font_size(11.0));
    }
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
