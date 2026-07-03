//! `FontConfig`: pure-data font configuration consumed by [`crate::FontGrid`].
//!
//! WP0 introduces this type as the config surface for later WPs (fallback
//! fonts, shaping/ligatures, native AA) — most fields are stored but not yet
//! consumed here. No swash/font-kit types leak into this file's public
//! surface, and no I/O happens here (parsing lives in `noa-config`).

/// Resolved font configuration passed into [`crate::FontGrid::new`].
///
/// Deliberately does not derive `Hash` — an `f32` field on [`FontVariation`]
/// blocks it, and a future WP adds a `style_digest` helper for cache keys
/// instead of deriving it directly.
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// Primary family stack, in preference order. Empty means "system
    /// monospace / Menlo fallback" (the current `load_font_stack` behavior).
    pub families: Vec<String>,
    pub families_bold: Vec<String>,
    pub families_italic: Vec<String>,
    pub families_bold_italic: Vec<String>,
    /// OpenType feature toggles (e.g. `calt`, `-liga`). Consumed for real in WP2.
    pub features: Vec<FontFeature>,
    /// Variable-font axis coordinates. Consumed for real in WP2.
    pub variations: Vec<FontVariation>,
    pub variations_bold: Vec<FontVariation>,
    pub variations_italic: Vec<FontVariation>,
    pub variations_bold_italic: Vec<FontVariation>,
    pub synthetic_style: SyntheticStyle,
    /// WP3 consumes this; WP0 only stores it.
    pub alpha_blending: AlphaBlending,
    /// Parsed-but-deferred; never consumed in this chain (CoreText-only).
    pub thicken: bool,
    pub thicken_strength: u8,
}

/// A single OpenType feature toggle, e.g. `calt` (enabled) or `-liga`
/// (`enabled: false`, explicitly disabled).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub enabled: bool,
}

/// A single variable-font axis coordinate, e.g. `wght=700`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f32,
}

/// Which styles are synthesized (faux-bold / faux-italic) when the resolved
/// family lacks the native style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SyntheticStyle {
    pub bold: bool,
    pub italic: bool,
}

/// Coverage-blend color space. `LinearFallback` is a parsed-but-deferred
/// value (`noa-config` emits a diagnostic and falls back to `Native`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AlphaBlending {
    #[default]
    Native,
    LinearFallback,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            families: vec![],
            families_bold: vec![],
            families_italic: vec![],
            families_bold_italic: vec![],
            features: vec![],
            variations: vec![],
            variations_bold: vec![],
            variations_italic: vec![],
            variations_bold_italic: vec![],
            synthetic_style: SyntheticStyle {
                bold: true,
                italic: true,
            },
            alpha_blending: AlphaBlending::Native,
            thicken: false,
            thicken_strength: 255,
        }
    }
}
