//! Theme construction for the app. The palette table + `Color -> [f32;4]`
//! resolution logic lives in `noa-render` (it's needed there to build GPU
//! instance colors); this module is the app-level seam that constructs the
//! selected theme noa-app hands to the renderer.

use noa_core::Rgb;

pub use noa_render::Theme;

/// Per-key color overrides (`background`, `foreground`, `cursor-color`,
/// `selection-foreground`, `selection-background`) applied on top of the
/// resolved theme. A `None` field keeps the theme's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeOverrides {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub selection_fg: Option<Rgb>,
    pub selection_bg: Option<Rgb>,
    pub minimum_contrast: f32,
}

impl Default for ThemeOverrides {
    fn default() -> Self {
        Self {
            background: None,
            foreground: None,
            cursor: None,
            selection_fg: None,
            selection_bg: None,
            minimum_contrast: Theme::default().minimum_contrast,
        }
    }
}

/// Resolve a config-selected theme name into the renderer theme.
pub fn resolve_theme(name: Option<&str>) -> Theme {
    let Some(name) = name else {
        return built_in_theme();
    };

    let Some(definition) = noa_theme::resolve(name) else {
        log::warn!("unknown theme {name:?}; falling back to the default theme");
        return built_in_theme();
    };

    theme_from_definition(definition)
}

/// Resolve a theme by name, then apply config color overrides.
pub fn resolve_theme_with_overrides(name: Option<&str>, overrides: &ThemeOverrides) -> Theme {
    let mut theme = resolve_theme(name);
    if let Some(background) = overrides.background {
        theme.default_bg = background;
    }
    if let Some(foreground) = overrides.foreground {
        theme.default_fg = foreground;
    }
    if let Some(cursor) = overrides.cursor {
        theme.cursor = cursor;
    }
    if let Some(selection_fg) = overrides.selection_fg {
        theme.selection_fg = selection_fg;
    }
    if let Some(selection_bg) = overrides.selection_bg {
        theme.selection_bg = selection_bg;
    }
    theme.minimum_contrast = overrides.minimum_contrast;
    theme
}

fn theme_from_definition(definition: &noa_theme::ThemeDef) -> Theme {
    // Search-highlight colors are derived from the vendor palette (not left at
    // the built-in yellow/blue) so highlights sit inside the theme's own color
    // world: the theme's yellow tints the inactive matches, its blue the
    // active one, each with a contrast-picked foreground.
    let search_bg = derive_highlight_bg(definition, 3, 11);
    let active_search_bg = definition.palette[4];

    Theme {
        default_fg: definition.default_fg,
        default_bg: definition.default_bg,
        cursor: definition.cursor,
        selection_fg: definition.selection_fg,
        selection_bg: definition.selection_bg,
        search_fg: contrast_fg(search_bg),
        search_bg,
        active_search_fg: contrast_fg(active_search_bg),
        active_search_bg,
        minimum_contrast: Theme::default().minimum_contrast,
        palette: definition.palette,
    }
}

/// The vendor palette's inactive-match highlight background: the theme's
/// yellow (`palette[idx]`), falling back to bright yellow (`palette[bright]`)
/// when yellow is indistinguishable from the terminal background.
fn derive_highlight_bg(def: &noa_theme::ThemeDef, idx: usize, bright: usize) -> Rgb {
    let primary = def.palette[idx];
    if primary == def.default_bg {
        def.palette[bright]
    } else {
        primary
    }
}

/// Pick a near-black or near-white foreground for legible text on `bg`, by
/// relative luminance: dark text on a light background, light text on a dark
/// one.
fn contrast_fg(bg: Rgb) -> Rgb {
    if relative_luminance(bg) > 0.5 {
        Rgb::new(0x1b, 0x1b, 0x1b)
    } else {
        Rgb::new(0xff, 0xff, 0xff)
    }
}

/// WCAG relative luminance (0.0 = black .. 1.0 = white) of an sRGB color.
/// `pub(crate)` (ADR-5/R-30) so `theme_settings::rich::attribute_of` can
/// reuse this exact luminance math for its Light/Dark classification instead
/// of adding a second implementation — `noa-render` stays untouched either
/// way (this lives in `noa-app`, not the renderer).
pub(crate) fn relative_luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

fn built_in_theme() -> Theme {
    Theme::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;

    #[test]
    fn resolve_theme_none_uses_default_theme() {
        assert_theme_eq(&resolve_theme(None), &Theme::default());
    }

    #[test]
    fn resolve_theme_unknown_theme_falls_back_to_default() {
        assert_theme_eq(&resolve_theme(Some("NoSuchTheme")), &Theme::default());
    }

    #[test]
    fn resolve_theme_maps_known_vendor_themes_to_render_theme() {
        for name in ["3024 Day", "Afterglow", "Alabaster"] {
            let definition = noa_theme::resolve(name).expect("vendored theme exists");
            let theme = resolve_theme(Some(name));

            assert_eq!(theme.default_fg, definition.default_fg);
            assert_eq!(theme.default_bg, definition.default_bg);
            assert_eq!(theme.cursor, definition.cursor);
            assert_eq!(theme.selection_fg, definition.selection_fg);
            assert_eq!(theme.selection_bg, definition.selection_bg);
            assert_eq!(theme.minimum_contrast, Theme::default().minimum_contrast);
            assert_eq!(theme.palette, definition.palette);
        }
    }

    #[test]
    fn vendor_theme_hex_spot_checks_are_byte_exact() {
        let cases = [
            (
                "3024 Day", "#4a4543", "#f7f7f7", "#4a4543", "#4a4543", "#a5a2a2", "#090300",
                "#f7f7f7",
            ),
            (
                "Afterglow",
                "#d0d0d0",
                "#212121",
                "#d0d0d0",
                "#d0d0d0",
                "#303030",
                "#151515",
                "#f5f5f5",
            ),
            (
                "Alabaster",
                "#000000",
                "#f7f7f7",
                "#007acc",
                "#000000",
                "#bfdbfe",
                "#000000",
                "#f7f7f7",
            ),
        ];

        for (name, fg, bg, cursor, selection_fg, selection_bg, palette0, palette15) in cases {
            let theme = resolve_theme(Some(name));

            assert_rgb_hex(theme.default_fg, fg);
            assert_rgb_hex(theme.default_bg, bg);
            assert_rgb_hex(theme.cursor, cursor);
            assert_rgb_hex(theme.selection_fg, selection_fg);
            assert_rgb_hex(theme.selection_bg, selection_bg);
            assert_rgb_hex(theme.palette[0], palette0);
            assert_rgb_hex(theme.palette[15], palette15);
        }
    }

    #[test]
    fn search_colors_derive_from_vendor_palette() {
        let definition = noa_theme::resolve("3024 Day").expect("vendored theme exists");
        let theme = resolve_theme(Some("3024 Day"));

        let expected_search_bg = derive_highlight_bg(definition, 3, 11);
        let expected_active_bg = definition.palette[4];

        // Highlights come from the vendor palette (yellow / blue), not the
        // built-in defaults, each with a contrast-picked foreground.
        assert_eq!(theme.search_bg, expected_search_bg);
        assert_eq!(theme.search_fg, contrast_fg(expected_search_bg));
        assert_eq!(theme.active_search_bg, expected_active_bg);
        assert_eq!(theme.active_search_fg, contrast_fg(expected_active_bg));
    }

    #[test]
    fn contrast_fg_is_dark_on_light_and_light_on_dark() {
        assert_eq!(
            contrast_fg(Rgb::new(0xff, 0xff, 0xff)),
            Rgb::new(0x1b, 0x1b, 0x1b)
        );
        assert_eq!(
            contrast_fg(Rgb::new(0x00, 0x00, 0x00)),
            Rgb::new(0xff, 0xff, 0xff)
        );
        // Mid gray leans light (luminance ~0.22 for #808080 < 0.5) → white fg.
        assert_eq!(
            contrast_fg(Rgb::new(0x80, 0x80, 0x80)),
            Rgb::new(0xff, 0xff, 0xff)
        );
    }

    #[test]
    fn overrides_replace_only_specified_colors() {
        let base = resolve_theme(Some("3024 Day"));
        let overrides = ThemeOverrides {
            background: Some(Rgb::new(1, 2, 3)),
            cursor: Some(Rgb::new(4, 5, 6)),
            selection_bg: Some(Rgb::new(7, 8, 9)),
            minimum_contrast: 4.5,
            ..Default::default()
        };

        let theme = resolve_theme_with_overrides(Some("3024 Day"), &overrides);

        assert_eq!(theme.default_bg, Rgb::new(1, 2, 3));
        assert_eq!(theme.cursor, Rgb::new(4, 5, 6));
        assert_eq!(theme.selection_bg, Rgb::new(7, 8, 9));
        assert_eq!(theme.minimum_contrast, 4.5);
        // Untouched fields keep the resolved theme's value.
        assert_eq!(theme.default_fg, base.default_fg);
        assert_eq!(theme.selection_fg, base.selection_fg);
        assert_eq!(theme.palette, base.palette);
    }

    #[test]
    fn empty_overrides_are_identical_to_plain_resolution() {
        let overrides = ThemeOverrides::default();
        assert_theme_eq(
            &resolve_theme_with_overrides(Some("Afterglow"), &overrides),
            &resolve_theme(Some("Afterglow")),
        );
    }

    fn assert_theme_eq(actual: &Theme, expected: &Theme) {
        assert_eq!(actual.default_fg, expected.default_fg);
        assert_eq!(actual.default_bg, expected.default_bg);
        assert_eq!(actual.cursor, expected.cursor);
        assert_eq!(actual.selection_fg, expected.selection_fg);
        assert_eq!(actual.selection_bg, expected.selection_bg);
        assert_eq!(actual.search_fg, expected.search_fg);
        assert_eq!(actual.search_bg, expected.search_bg);
        assert_eq!(actual.active_search_fg, expected.active_search_fg);
        assert_eq!(actual.active_search_bg, expected.active_search_bg);
        assert_eq!(actual.minimum_contrast, expected.minimum_contrast);
        assert_eq!(actual.palette, expected.palette);
    }

    fn assert_rgb_hex(actual: Rgb, expected: &str) {
        assert_eq!(
            format!("#{:02x}{:02x}{:02x}", actual.r, actual.g, actual.b),
            expected
        );
    }
}
