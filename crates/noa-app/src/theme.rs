//! Theme construction for the app. The palette table + `Color -> [f32;4]`
//! resolution logic lives in `noa-render` (it's needed there to build GPU
//! instance colors); this module is the app-level seam that constructs the
//! selected theme noa-app hands to the renderer.

pub use noa_render::Theme;

/// The default inc-1 theme.
pub fn default_theme() -> Theme {
    resolve_theme(None)
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

fn theme_from_definition(definition: &noa_theme::ThemeDef) -> Theme {
    let defaults = built_in_theme();

    Theme {
        default_fg: definition.default_fg,
        default_bg: definition.default_bg,
        cursor: definition.cursor,
        selection_fg: definition.selection_fg,
        selection_bg: definition.selection_bg,
        search_fg: defaults.search_fg,
        search_bg: defaults.search_bg,
        active_search_fg: defaults.active_search_fg,
        active_search_bg: defaults.active_search_bg,
        palette: definition.palette,
    }
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
        assert_theme_eq(&resolve_theme(None), &default_theme());
    }

    #[test]
    fn resolve_theme_unknown_theme_falls_back_to_default() {
        assert_theme_eq(&resolve_theme(Some("NoSuchTheme")), &default_theme());
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
    fn search_colors_remain_default_when_vendor_theme_is_selected() {
        let default = default_theme();
        let theme = resolve_theme(Some("3024 Day"));

        assert_eq!(theme.search_fg, default.search_fg);
        assert_eq!(theme.search_bg, default.search_bg);
        assert_eq!(theme.active_search_fg, default.active_search_fg);
        assert_eq!(theme.active_search_bg, default.active_search_bg);
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
        assert_eq!(actual.palette, expected.palette);
    }

    fn assert_rgb_hex(actual: Rgb, expected: &str) {
        assert_eq!(
            format!("#{:02x}{:02x}{:02x}", actual.r, actual.g, actual.b),
            expected
        );
    }
}
