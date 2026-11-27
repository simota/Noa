//! Layout helpers and the faded box of a resolved candidate.

use noa_gpui_core::style::{
    AlignItems, Dimension, FlexDirection, LengthPercentage, Size as LayoutSize, auto, length,
    percent,
};
use noa_gpui_core::{PaintCx, Style, Widget};
use noa_gpui_scene::Color;

/// Refusals and unreadable sources.
pub const RED: Color = Color::hex(0xff453a);

pub const GUTTER: f32 = 16.0;
/// The detail pane's width.
pub const DETAIL_WIDTH: f32 = 440.0;
/// The opacity of a resolved candidate's row.
pub const RESOLVED_OPACITY: f32 = 0.9;

pub fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(px),
        height: length(px),
    }
}

pub fn full() -> LayoutSize<Dimension> {
    LayoutSize {
        width: percent(1.0),
        height: percent(1.0),
    }
}

/// Takes the space left in a flex container, and may shrink to nothing.
pub fn fill() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
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

pub fn column(gap_px: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        gap: gap(gap_px),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

pub fn row(gap_px: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::CENTER,
        gap: gap(gap_px),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// A full-width box sized by its content.
pub fn wide(style: Style) -> Style {
    Style {
        size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..style
    }
}

/// A growing box that may shrink below its content's width.
pub fn grow() -> Style {
    Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: auto(),
        },
        ..Default::default()
    }
}

/// A layout box drawn at `opacity` with everything in it.
pub struct Faded {
    pub style: Style,
    pub opacity: f32,
}

impl Widget for Faded {
    fn style(&self) -> Style {
        self.style.clone()
    }

    fn paint(&mut self, _cx: &mut PaintCx) {}

    fn opacity(&self) -> f32 {
        self.opacity
    }
}
