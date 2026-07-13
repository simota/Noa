//! `FontConfig`: pure-data font configuration consumed by [`crate::FontGrid`].
//!
//! WP0 introduced this type as the config surface for later WPs; WP2 wired up
//! `features`/`variations*` (consumed by `shape.rs`/`grid.rs`) and WP3 wired
//! up `alpha_blending` (consumed by `noa-render`). No swash/font-kit types
//! leak into this file's public surface, and no I/O happens here (parsing
//! lives in `noa-config`).

/// Resolved font configuration passed into [`crate::FontGrid::new`].
///
/// Deliberately does not derive `Hash` — an `f32` field on [`FontVariation`]
/// blocks it, and a future WP adds a `style_digest` helper for cache keys
/// instead of deriving it directly.
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// Primary family stack, in preference order. Empty means "platform
    /// default coding font" (`Menlo` first on macOS, then system fallbacks).
    pub families: Vec<String>,
    pub families_bold: Vec<String>,
    pub families_italic: Vec<String>,
    pub families_bold_italic: Vec<String>,
    /// OpenType feature toggles (e.g. `calt`, `-liga`). Consumed by WP2 shaping.
    pub features: Vec<FontFeature>,
    /// Variable-font axis coordinates. Consumed by WP2 shaping + rasterization.
    pub variations: Vec<FontVariation>,
    pub variations_bold: Vec<FontVariation>,
    pub variations_italic: Vec<FontVariation>,
    pub variations_bold_italic: Vec<FontVariation>,
    pub synthetic_style: SyntheticStyle,
    /// Coverage-blend color space (WP3). Selects the render target space in
    /// `noa-app` (`native` → gamma-space blend on a non-sRGB surface; `linear`
    /// / `linear-corrected` → linear-space blend on an sRGB surface) and, for
    /// `linear-corrected`, the glyph-coverage correction in `noa-render`.
    pub alpha_blending: AlphaBlending,
    /// Dilate glyph coverage to emulate CoreText/Ghostty `font-thicken`
    /// stem-darkening (swash has no native smoothing). Default on, to match
    /// Ghostty's heavier stroke weight; see `raster::thicken_mask`.
    pub thicken: bool,
    /// Thicken intensity `0..=255` (`font-thicken-strength`); `0` disables it.
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
/// family lacks the native style. Never applied to a shared fallback face
/// (emoji/Nerd Font/CJK, or a macOS cascade hit) regardless of these toggles
/// — see `FontStack::is_native_style_face` / `is_fallback_face`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SyntheticStyle {
    pub bold: bool,
    pub italic: bool,
}

/// Coverage-blend color space, mirroring Ghostty's `alpha-blending` modes.
///
/// - `Native` (default): blend glyph coverage against the background directly
///   in gamma-encoded space, the way CoreText/FreeType render by default. In
///   `noa-app` this maps to a non-sRGB surface so the fixed-function blend
///   unit stays in gamma space.
/// - `Linear`: blend in linear space (technically correct, but thins
///   dark-on-light text). Maps to an sRGB surface so the blend unit decodes to
///   linear before blending.
/// - `LinearCorrected`: `Linear` plus a per-glyph coverage correction
///   (`noa-render`) that restores the perceived stem weight linear blending
///   would otherwise lose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AlphaBlending {
    #[default]
    Native,
    Linear,
    LinearCorrected,
}

impl AlphaBlending {
    /// True when the mode blends coverage in linear space (`Linear` /
    /// `LinearCorrected`) and therefore wants an sRGB render target.
    pub fn is_linear(self) -> bool {
        matches!(self, Self::Linear | Self::LinearCorrected)
    }
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
            thicken: true,
            thicken_strength: 255,
        }
    }
}
