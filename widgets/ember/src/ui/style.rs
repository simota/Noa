use noa_gpui_core::style::{FlexDirection, Size as LayoutSize, length, percent};
use noa_gpui_core::{Appearance, Length, Style, StylePatch, Theme, UiTree, WidgetId};
use noa_gpui_scene::Color;

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

pub(super) fn wide(tree: &mut UiTree, id: WidgetId) {
    let mut patch = StylePatch::default();
    patch.width = Some(Length::Percent(1.0));
    patch.min_width = Some(Length::Px(0.0));
    patch.max_width = Some(Length::Percent(1.0));
    tree.set_inline_style(id, patch);
}

pub(super) fn theme() -> Theme {
    let mut theme = Theme::for_appearance(Appearance::Dark);
    theme.background = Color::hex(0x191a1c);
    theme.surface = Color::hex(0x202123);
    theme.surface_alt = Color::hex(0x292a2c);
    theme.text = Color::hex(0xeeefec);
    theme.muted = Color::hex(0xb0b0aa);
    theme.accent = Color::hex(0xe8a33d);
    theme.accent_hover = Color::hex(0xf1b45c);
    theme.accent_pressed = Color::hex(0xcf8c2e);
    theme.on_accent = Color::hex(0x30220d);
    theme.separator = Color::hex(0x353638);
    theme.outline = Color::hex(0x656563);
    theme.focus_ring = theme.accent;
    theme.selection = Color::hex(0x544027);
    theme
}
