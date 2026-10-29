//! Static Ghostty-compatible theme catalog for noa.

use noa_core::Rgb;

mod generated;

pub use generated::THEMES;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThemeDef {
    pub name: &'static str,
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    pub cursor: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
    pub palette: [Rgb; 256],
}

pub fn resolve(name: &str) -> Option<&'static ThemeDef> {
    THEMES
        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
        .ok()
        .map(|index| &THEMES[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;

    #[test]
    fn generated_catalog_is_sorted_for_binary_search() {
        assert!(
            THEMES.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "generated theme catalog must stay sorted by name"
        );
    }

    #[test]
    fn resolve_finds_known_vendored_theme() {
        let theme = resolve("3024 Day").expect("3024 Day is in the vendored theme snapshot");

        assert_eq!(theme.name, "3024 Day");
        assert_eq!(theme.default_bg, Rgb::new(0xf7, 0xf7, 0xf7));
        assert_eq!(theme.default_fg, Rgb::new(0x4a, 0x45, 0x43));
        assert_eq!(theme.cursor, Rgb::new(0x4a, 0x45, 0x43));
        assert_eq!(theme.selection_bg, Rgb::new(0xa5, 0xa2, 0xa2));
        assert_eq!(theme.selection_fg, Rgb::new(0x4a, 0x45, 0x43));
        assert_eq!(theme.palette[0], Rgb::new(0x09, 0x03, 0x00));
        assert_eq!(theme.palette[15], Rgb::new(0xf7, 0xf7, 0xf7));
        assert_eq!(theme.palette[16], Rgb::new(0x00, 0x00, 0x00));
    }

    #[test]
    fn resolve_returns_none_for_unknown_theme() {
        assert!(resolve("NoSuchTheme").is_none());
    }

    #[test]
    fn codegen_script_derives_missing_selection_and_cursor_from_defaults() {
        let script = include_str!("../../../scripts/gen-themes");

        assert!(script.contains(r#"cursor = values.get("cursor-color", default_fg)"#));
        assert!(
            script.contains(r#"selection_bg = values.get("selection-background", default_fg)"#)
        );
        assert!(
            script.contains(r#"selection_fg = values.get("selection-foreground", default_bg)"#)
        );
    }

    #[test]
    fn generated_catalog_matches_vendor_snapshot_plus_noa_themes() {
        // 574 vendored Ghostty themes + Noa's own `themes/Noa.conf`.
        assert_eq!(THEMES.len(), 575);
    }

    fn relative_luminance(color: Rgb) -> f64 {
        let channel = |value: u8| {
            let c = f64::from(value) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn noa_theme_uses_the_brand_palette() {
        let theme = resolve("Noa").expect("Noa ships its own theme");
        let orange = Rgb::new(0xe8, 0xa3, 0x3d);

        assert_eq!(theme.default_bg, Rgb::new(0x1b, 0x1d, 0x21));
        assert_eq!(theme.default_fg, Rgb::new(0xf7, 0xf7, 0xf2));
        assert_eq!(theme.cursor, orange);
        assert_eq!(theme.palette[3], orange);
    }

    #[test]
    fn noa_theme_ansi_colors_clear_minimum_contrast() {
        let theme = resolve("Noa").expect("Noa ships its own theme");

        // Index 0 is black: a background color, not text.
        for index in 1..16 {
            let ratio = contrast_ratio(theme.palette[index], theme.default_bg);
            assert!(ratio >= 4.5, "palette {index} contrast {ratio:.2} < 4.5");
        }
        assert!(contrast_ratio(theme.selection_fg, theme.selection_bg) >= 4.5);
    }
}
