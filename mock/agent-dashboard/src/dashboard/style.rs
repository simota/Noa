use noa_gpui_core::style::{LengthPercentage, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Container, Label, Tag};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Need, Phase};

pub(super) const HEADER_HEIGHT: f32 = 48.0;
pub(super) const GUTTER: f32 = 12.0;

/// Small but not zero: flex shrinks in proportion to `flex_shrink` times the
/// basis, so this lets the status label absorb a deficit before the strip.
pub(super) const HINT_STRIP_SHRINK: f32 = 0.0001;

pub(super) const GREEN: Color = Color::hex(0x34c759);
pub(super) const DIM_GREEN: Color = Color::hex(0x2f5d3c);
pub(super) const YELLOW: Color = Color::hex(0xffcc00);
pub(super) const ORANGE: Color = Color::hex(0xff9f0a);
pub(super) const RED: Color = Color::hex(0xff453a);
pub(super) const GRAY: Color = Color::hex(0x8e8e93);
pub(super) const BLUE: Color = Color::hex(0x0a84ff);

pub(super) fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(px),
        height: length(px),
    }
}

pub(super) fn fill_remaining() -> Style {
    Style {
        flex_grow: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}

pub(super) fn spacer(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(Some(parent), Container::row().grow(1.0));
}

pub(super) fn need_color(need: Need) -> Color {
    match need {
        Need::Approval => YELLOW,
        Need::Failure => RED,
        Need::Question => ORANGE,
        Need::Notice => BLUE,
        Need::Quiet => GRAY,
    }
}

pub(super) fn phase_color(phase: Phase) -> Color {
    match phase {
        Phase::Running => GREEN,
        Phase::Quiet => DIM_GREEN,
        Phase::Permission => YELLOW,
        Phase::Input => ORANGE,
        Phase::Ended => GRAY,
    }
}

pub(super) fn fixed_width(width: f32) -> Style {
    Style {
        size: LayoutSize {
            width: length(width),
            height: auto(),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

pub(super) fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text).font_size(11.0).color(Role::Muted),
    )
}

pub(super) fn set_label(tree: &mut UiTree, id: WidgetId, text: String) {
    tree.update::<Label, _>(id, |l, cx| l.set_text(cx, text));
}

pub(super) fn set_tag(tree: &mut UiTree, id: WidgetId, text: String) {
    tree.update::<Tag, _>(id, |t, cx| t.set_text(cx, text));
}

/// The parts that are known, joined by `separator`.
pub(super) fn joined(parts: &[&str], separator: &str) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(separator)
}
