use noa_gpui_core::style::{
    FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::{Appearance, Style, Theme};
use noa_gpui_scene::Color;
use workflow::model::NodeState;

pub(super) fn fill() -> Style {
    Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}
pub(super) fn column() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        gap: LayoutSize {
            width: length(crate::appearance::space(8.0)),
            height: length(crate::appearance::space(8.0)),
        },
        ..fill()
    }
}
pub(super) fn root() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        size: LayoutSize {
            width: percent(1.0),
            height: percent(1.0),
        },
        ..Default::default()
    }
}
pub(super) fn absolute(x: f32, y: f32, w: f32, h: f32) -> Style {
    Style {
        position: Position::Absolute,
        inset: LayoutRect {
            left: length(x),
            top: length(y),
            right: auto(),
            bottom: auto(),
        },
        size: LayoutSize {
            width: length(w),
            height: length(h),
        },
        ..Default::default()
    }
}
pub(super) fn theme() -> Theme {
    let mut theme = Theme::for_appearance(Appearance::Dark);
    theme.background = Color::hex(0x111719);
    theme.surface = Color::hex(0x1a2225);
    theme.surface_alt = Color::hex(0x222d30);
    theme.text = Color::hex(0xe5eeeb);
    theme.muted = Color::hex(0x9cacaa);
    theme.accent = Color::hex(0x91d7bd);
    theme.accent_hover = Color::hex(0xa5e3cc);
    theme.accent_pressed = Color::hex(0x72bba1);
    theme.on_accent = Color::hex(0x102b22);
    theme.separator = Color::hex(0x344144);
    theme.outline = Color::hex(0x526460);
    theme.focus_ring = theme.accent;
    theme.selection = Color::hex(0x234c3f);
    theme
}
pub(super) fn state_color(state: NodeState) -> Color {
    Color::hex(match state {
        NodeState::Succeeded => 0x91d7bd,
        NodeState::Running => 0x8ec9f0,
        NodeState::Waiting | NodeState::Paused => 0xf1c879,
        NodeState::Failed | NodeState::Uncertain => 0xf29b93,
        NodeState::Pending | NodeState::Canceled => 0x9cacaa,
    })
}
pub(super) fn state_symbol(state: NodeState) -> &'static str {
    match state {
        NodeState::Succeeded => "✓",
        NodeState::Running => "●",
        NodeState::Waiting => "◇",
        NodeState::Failed => "!",
        NodeState::Uncertain => "?",
        NodeState::Paused => "Ⅱ",
        NodeState::Canceled => "×",
        NodeState::Pending => "○",
    }
}
