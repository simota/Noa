use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Badge, Container, EmptyState, Label, ScrollView, Tone};
use noa_gpui_core::{Key, Role, Shortcut, Style, UiTree, WidgetId};

use crate::live::Link;

use super::style::{fill_remaining, fixed_width, gap};

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

    pub(super) fn shortcut(self) -> Shortcut {
        let digit = ['1', '2', '3', '4', '5'][self as usize];
        Shortcut::key(Key::Char(digit))
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

pub(super) fn panel_title(tree: &mut UiTree, parent: WidgetId, title: &str, n: usize, tone: Tone) {
    let row = tree.insert(
        Some(parent),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(row),
        Label::new(title)
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    tree.insert(Some(row), Badge::count(n as u32).tone(tone));
}

pub(super) fn card_list(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    tree.insert(
        Some(parent),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            ..fill_remaining()
        }),
    )
}

pub(super) fn group_header(tree: &mut UiTree, parent: WidgetId, name: &str, n: usize) {
    tree.insert(
        Some(parent),
        Label::new(format!("{name} · {n}"))
            .font_size(11.0)
            .weight(700)
            .color(Role::Muted),
    );
}

pub(super) fn empty(tree: &mut UiTree, body: WidgetId, link: &Link) {
    let state = match link {
        Link::Connecting => EmptyState::new("Noa に接続しています…"),
        Link::Live => EmptyState::new("エージェントのペインが見つかりません").message(
            "claude / codex / agy が動いているペインと、+ctl で起動したワーカーがここに出ます",
        ),
        Link::Down(reason) => EmptyState::new("エージェントのペインが見つかりません")
            .message(format!("Noa に接続できません: {reason}")),
    };
    state.width(560.0).insert(tree, body);
}
