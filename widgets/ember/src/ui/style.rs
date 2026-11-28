use noa_gpui_core::style::{FlexDirection, Size as LayoutSize, length, percent};
use noa_gpui_core::widgets::SvgIcon;
use noa_gpui_core::{Appearance, Length, Role, Style, StylePatch, Theme, UiTree, WidgetId};
use noa_gpui_scene::{Color, VectorIcon};

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

pub(super) fn reading_width(tree: &mut UiTree, id: WidgetId, max: f32) {
    let mut patch = StylePatch::default();
    patch.width = Some(Length::Percent(1.0));
    patch.min_width = Some(Length::Px(0.0));
    patch.max_width = Some(Length::Px(max));
    patch.margin[1] = Some(Length::Auto);
    patch.margin[3] = Some(Length::Auto);
    tree.set_inline_style(id, patch);
}

pub(super) fn pill(tree: &mut UiTree, id: WidgetId, primary: bool) {
    let mut patch = StylePatch::default();
    patch.border_radius = Some(18.0);
    patch.min_height = Some(Length::Px(32.0));
    patch.font_size = Some(crate::appearance::font(13.0));
    if primary {
        // A filled action without Button::default_button: plain Enter must
        // continue to insert a newline in the native text editor.
        patch.background_color = Some(Role::Accent.into());
        patch.color = Some(Role::OnAccent.into());
        patch.border_width = Some(0.0);
    }
    tree.set_inline_style(id, patch);
}

pub(super) fn avatar(tree: &mut UiTree, parent: WidgetId, size: f32) -> WidgetId {
    let icon = VectorIcon::from_svg(
        r##"<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg">
<path fill="#edaa50" d="M34 5C32 15 43 14 46 22C49 28 56 29 55 39C54 51 45 58 32 58C18 58 9 50 9 38C9 26 19 22 22 14C25 17 25 21 26 23C31 18 27 10 34 5Z"/>
<ellipse fill="#382519" cx="25" cy="37" rx="2.5" ry="4"/>
<ellipse fill="#382519" cx="39" cy="37" rx="2.5" ry="4"/>
<path fill="none" stroke="#382519" stroke-width="2" d="M28 45Q32 48 36 45"/>
</svg>"##,
    )
    .expect("the built-in Ember avatar is valid SVG");
    tree.insert(Some(parent), SvgIcon::new(icon, size).multicolor(true))
}

pub(super) fn theme() -> Theme {
    let mut theme = Theme::for_appearance(Appearance::Light);
    theme.background = Color::hex(0xfaf9f6);
    theme.surface = Color::hex(0xf0eeea);
    theme.surface_alt = Color::hex(0xe8e5df);
    theme.text = Color::hex(0x292b2e);
    theme.muted = Color::hex(0x716e68);
    theme.accent = Color::hex(0xedaa50);
    theme.accent_hover = Color::hex(0xf3bb71);
    theme.accent_pressed = Color::hex(0xdf9840);
    theme.on_accent = Color::hex(0x30220d);
    theme.separator = Color::hex(0xe3e0d9);
    theme.outline = Color::hex(0xb8b4ac);
    theme.focus_ring = theme.accent;
    theme.selection = Color::hex(0xf6dfbe);
    theme
}
