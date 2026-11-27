//! Layout helpers and colours shared by the screen, the columns and the
//! cards.

use noa_gpui_core::style::{
    AlignItems, Dimension, FlexDirection, FlexWrap, LengthPercentage, Rect as LayoutRect,
    Size as LayoutSize, length, percent,
};
use noa_gpui_core::widgets::{Container, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use tasks::model::types::{Column, Source};

/// Semantic identities let a clock tick update age labels without replacing
/// focusable rows, their menus or text selection.
pub fn remember_age(tree: &mut UiTree, id: WidgetId, at: u64, prefix: &str, suffix: &str) {
    let identity = serde_json::to_string(&(at, prefix, suffix)).expect("age label identity");
    tree.set_autosave_name(id, format!("tasks/age/{identity}"));
}

pub fn refresh_ages(tree: &mut UiTree, root: WidgetId, now: u64) {
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        pending.extend_from_slice(tree.children(id));
        let data = tree
            .autosave_name(id)
            .and_then(|s| s.strip_prefix("tasks/age/"))
            .and_then(|json| serde_json::from_str::<(u64, String, String)>(json).ok());
        if let Some((at, prefix, suffix)) = data {
            let value = format!("{prefix}{}{suffix}", super::columns::age(now, at));
            tree.update::<noa_gpui_core::widgets::Label, _>(id, |label, cx| {
                label.set_text(cx, value)
            });
        }
    }
}

pub const GUTTER: f32 = 16.0;
/// The width every column starts from; they share what is left.
pub const COLUMN_BASIS: f32 = 220.0;
/// The detail pane's width, beside the columns.
pub const DETAIL_WIDTH: f32 = 420.0;

pub fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(crate::appearance::space(px)),
        height: length(crate::appearance::space(px)),
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

pub fn row(gap_px: f32) -> Style {
    Style {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::CENTER,
        gap: gap(gap_px),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// A row of the header bars: padded, on the surface colour.
pub fn bar(tree: &mut UiTree, parent: WidgetId, gap_px: f32) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            padding: LayoutRect {
                left: length(GUTTER),
                right: length(GUTTER),
                top: length(8.0),
                bottom: length(8.0),
            },
            flex_wrap: FlexWrap::Wrap,
            ..row(gap_px)
        })
        .background(Role::Surface),
    )
}

/// A one-pixel line across its parent.
pub fn hairline(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(
        Some(parent),
        Container::new(Style {
            size: LayoutSize {
                width: percent(1.0),
                height: length(1.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::Separator),
    );
}

pub fn spacer(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_grow: 1.0,
            ..Default::default()
        }),
    );
}

/// A card's box: a column of its lines.
pub fn card_style() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        padding: LayoutRect::length(crate::appearance::space(10.0)),
        gap: gap(4.0),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// The source badge's tone: local tasks in the accent, issues neutral,
/// the code's markers as a warning.
pub fn source_tone(source: Source) -> Tone {
    match source {
        Source::Local => Tone::Accent,
        Source::GitHub => Tone::Neutral,
        Source::Code => Tone::Warning,
    }
}

/// The tone of a column's count.
pub fn column_tone(column: Column) -> Tone {
    match column {
        Column::Todo => Tone::Neutral,
        Column::Running => Tone::Accent,
        Column::Review => Tone::Warning,
        Column::Done => Tone::Success,
    }
}
