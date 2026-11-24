//! Colours, layout helpers and number formatting shared by both tabs.

use noa_gpui_core::style::{
    AlignItems, Dimension, FlexDirection, JustifyContent, LengthPercentage, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Paint, Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use agent_usage::model::{Agent, Severity};

pub const CLAUDE: Color = Color::hex(0xd97757);
pub const CODEX: Color = Color::hex(0x10a37f);
pub const AGY: Color = Color::hex(0x4285f4);

pub const YELLOW: Color = Color::hex(0xffcc00);
pub const ORANGE: Color = Color::hex(0xff9f0a);
pub const RED: Color = Color::hex(0xff453a);
pub const GREEN: Color = Color::hex(0x34c759);

pub const GUTTER: f32 = 16.0;

pub fn agent_color(agent: Agent) -> Color {
    match agent {
        Agent::Claude => CLAUDE,
        Agent::Codex => CODEX,
        Agent::Agy => AGY,
    }
}

/// Normal = the accent, caution yellow, warning orange, critical red.
pub fn severity_paint(severity: Severity) -> Paint {
    match severity {
        Severity::Normal => Paint::Role(Role::Accent),
        Severity::Caution => Paint::Fixed(YELLOW),
        Severity::Warning => Paint::Fixed(ORANGE),
        Severity::Critical => Paint::Fixed(RED),
    }
}

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

/// A rounded card on the surface colour.
pub fn card(tree: &mut UiTree, parent: WidgetId, style: Style) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            padding: LayoutRect::length(12.0),
            gap: gap(6.0),
            ..style
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(10.0)
        .clip(true),
    )
}

/// A fixed-width cell holding one line, its text at the start or the end.
pub fn cell(tree: &mut UiTree, parent: WidgetId, width: f32, end: bool, label: Label) -> WidgetId {
    let host = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            justify_content: if end {
                JustifyContent::FLEX_END
            } else {
                JustifyContent::FLEX_START
            },
            size: LayoutSize {
                width: length(width),
                height: auto(),
            },
            flex_shrink: 0.0,
            overflow: noa_gpui_core::style::Point {
                x: noa_gpui_core::style::Overflow::Hidden,
                y: noa_gpui_core::style::Overflow::Visible,
            },
            ..Default::default()
        }),
    );
    tree.insert(Some(host), label);
    host
}

pub fn heading(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text.into()).font_size(13.0).weight(600),
    )
}

pub fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text.into()).font_size(11.0).color(Role::Muted),
    )
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

/// "12,345".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "950", "12.3K", "4.56M", "1.20B".
pub fn tokens(n: u64) -> String {
    let v = n as f64;
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}K", v / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.2}M", v / 1e6)
    } else {
        format!("{:.2}B", v / 1e9)
    }
}

/// "$12.34".
pub fn usd(v: f64) -> String {
    if v >= 1000.0 {
        format!("${}", count(v.round() as u64))
    } else {
        format!("${v:.2}")
    }
}

/// "3時間12分", "45分", "0分".
pub fn duration(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}時間{m:02}分")
    } else {
        format!("{m}分")
    }
}

/// "12秒前" / "3分前" / "2時間前" / "4日前".
pub fn ago(at: i64, now: i64) -> String {
    let s = (now - at).max(0);
    if s < 60 {
        format!("{s}秒前")
    } else if s < 3600 {
        format!("{}分前", s / 60)
    } else if s < 86_400 {
        format!("{}時間前", s / 3600)
    } else {
        format!("{}日前", s / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_for_reading() {
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(999), "999");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_340), "12.3K");
        assert_eq!(tokens(4_560_000), "4.56M");
        assert_eq!(usd(12.345), "$12.35");
        assert_eq!(duration(3 * 3600 + 12 * 60), "3時間12分");
        assert_eq!(ago(0, 720), "12分前");
    }
}
