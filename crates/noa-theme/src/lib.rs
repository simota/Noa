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
