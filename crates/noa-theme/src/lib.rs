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
    fn generated_catalog_matches_vendor_snapshot_size() {
        assert_eq!(THEMES.len(), 574);
    }
}
