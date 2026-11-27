use crate::appearance::space;
use noa_gpui_core::Style;
use noa_gpui_core::style::{
    AlignItems, Dimension, FlexDirection, LengthPercentage, Size, auto, length, percent,
};

pub fn gap(px: f32) -> Size<LengthPercentage> {
    Size {
        width: length(space(px)),
        height: length(space(px)),
    }
}

pub fn column(px: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        gap: gap(px),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

pub fn row(px: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::CENTER,
        gap: gap(px),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

pub fn wide(style: Style) -> Style {
    Style {
        size: Size {
            width: percent(1.0),
            height: auto(),
        },
        ..style
    }
}

pub fn grow() -> Style {
    Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: Size {
            width: length(0.0),
            height: auto(),
        },
        ..Default::default()
    }
}

pub fn full() -> Size<Dimension> {
    Size {
        width: percent(1.0),
        height: percent(1.0),
    }
}

pub fn fill() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: Size {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}
