//! Text shaping via `rustybuzz` (WP2) — the shape<->raster seam consumed by
//! `noa-render`.
//!
//! **FROZEN** per `docs/specs/rendering-improvements.md` §L2/WP2:
//! [`FaceId`]/[`StyleKey`]/[`ShapeCell`]/[`ShapedGlyph`] are a
//! LOW-reversibility contract — read that section before changing field
//! names/types here.
//!
//! [`crate::FontGrid::shape_run`] and [`crate::FontGrid::raster_shaped`] are
//! implemented in `grid.rs` (they need `FontGrid`'s private atlas/cache
//! state); this module holds the frozen data types plus the pure shaping
//! logic that doesn't need that private state, so it's usable/testable in
//! isolation.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use rustybuzz::ttf_parser::Tag as HbTag;
use rustybuzz::{
    Direction, Face as HbFace, Feature as HbFeature, UnicodeBuffer, Variation as HbVariation,
};

use crate::config::{FontConfig, FontVariation};
use crate::face::FontData;

/// Index into the font stack's resolved faces (primary + fallbacks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceId(pub u16);

impl FaceId {
    /// Sentinel face for built-in procedurally-drawn glyphs (box-drawing,
    /// block elements, Powerline separators — see the `boxdraw` module). Not a
    /// real index into the font stack: `shape_run` emits it for builtin
    /// codepoints and `raster_shaped` recognises it to synthesise the mask
    /// instead of hitting a font. Chosen as `u16::MAX` so it never collides
    /// with a real fallback-face index.
    pub const BUILTIN: FaceId = FaceId(u16::MAX);
}

/// Render-seam style key derived from `CellAttrs` by `noa-render` (NOT
/// `noa-grid` — see CLAUDE.md's GUI-agnostic dependency rule). Only
/// bold/italic affect face + variation selection and therefore run
/// boundaries; inverse/underline/strikethrough/etc. do not break a shaped
/// run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct StyleKey {
    pub bold: bool,
    pub italic: bool,
}

/// One source cell fed into [`crate::FontGrid::shape_run`]: its base char,
/// any combining marks stacked on it (from the cell's grapheme cluster,
/// minus the base char), and its style.
///
/// Deliberately carries NOTHING else — no cursor/selection/frame state —
/// so a shape-cache key built ONLY from `&[ShapeCell]` can never
/// accidentally include per-frame state (FM-08 structural mitigation; see
/// the WP2 design doc). Keep it that way: do not add fields here for
/// render-only concerns (color, cursor, selection) — those live on the
/// render side, alongside a `ShapeCell` slice, never inside it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShapeCell {
    pub ch: char,
    pub combining: Vec<char>,
    pub style: StyleKey,
}

/// One shaped glyph from a [`crate::FontGrid::shape_run`] call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub face_id: FaceId,
    pub x_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    /// Source cell index WITHIN THE RUN (0-based), HarfBuzz cluster
    /// semantics. A ligature's cluster is its cluster-START cell; a
    /// combining mark's cluster equals its base cell's index (multiple
    /// `ShapedGlyph`s may share one cluster value — e.g. a base glyph plus
    /// one or more attached mark glyphs).
    pub cluster: u32,
}

/// One memoized shape run (REQ-SHAPE-5), stored in a bucket under its
/// [`shape_run_hash`]. Identified ONLY by `&[ShapeCell]` content +
/// [`StyleKey`] + a config digest (see [`config_digest`]) — never by a
/// `FrameSnapshot`/cursor/selection (FM-08: the functions that build the
/// hash and compare entries only ever see a `&[ShapeCell]` slice, so there
/// is no cursor/selection field available to leak into them by accident).
pub(crate) struct ShapeRunEntry {
    pub(crate) text: Vec<(char, Vec<char>)>,
    pub(crate) style: StyleKey,
    pub(crate) cfg_digest: u64,
    pub(crate) glyphs: Vec<ShapedGlyph>,
    pub(crate) last_used: u64,
}

/// Hash a run for shape-cache lookup without building an owned key: the
/// lookup path allocates nothing on a cache hit (the owned `text` in
/// [`ShapeRunEntry`] is built only on insert). Collisions are resolved by
/// [`run_matches`] plus the entry's `style`/`cfg_digest` fields.
pub(crate) fn shape_run_hash(cells: &[ShapeCell], style: StyleKey, cfg_digest: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for cell in cells {
        cell.ch.hash(&mut hasher);
        cell.combining.hash(&mut hasher);
    }
    style.hash(&mut hasher);
    cfg_digest.hash(&mut hasher);
    hasher.finish()
}

/// Full-equality check between a cached entry's text and a candidate run.
pub(crate) fn run_matches(entry_text: &[(char, Vec<char>)], cells: &[ShapeCell]) -> bool {
    entry_text.len() == cells.len()
        && entry_text
            .iter()
            .zip(cells)
            .all(|((ch, combining), cell)| *ch == cell.ch && *combining == cell.combining)
}

/// Folds the style-relevant families/features/variations into a digest for
/// the shape cache key. Deliberately NOT `#[derive(Hash)]` on the whole
/// `FontConfig` (blocked by `f32` fields anyway, per `FontConfig`'s doc
/// comment) — every `f32` is folded via `.to_bits()` so bit-identical
/// configs always hash equal and no float-equality footgun is introduced.
pub(crate) fn config_digest(cfg: &FontConfig, style: StyleKey) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cfg.families.hash(&mut hasher);
    cfg.families_bold.hash(&mut hasher);
    cfg.families_italic.hash(&mut hasher);
    cfg.families_bold_italic.hash(&mut hasher);
    cfg.features.hash(&mut hasher);
    for variation in variations_for_style(cfg, style) {
        variation.tag.hash(&mut hasher);
        variation.value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Select the per-style variation-axis list (REQ-SHAPE-3 / D1): bold+italic,
/// bold, italic, or the base list.
fn variations_for_style(cfg: &FontConfig, style: StyleKey) -> &[FontVariation] {
    match (style.bold, style.italic) {
        (true, true) => &cfg.variations_bold_italic,
        (true, false) => &cfg.variations_bold,
        (false, true) => &cfg.variations_italic,
        (false, false) => &cfg.variations,
    }
}

/// The variation-axis coordinates (`(tag, value)`; tag = big-endian 4-byte
/// OpenType tag packed as `u32` — the SAME encoding both
/// `rustybuzz::ttf_parser::Tag` and `swash::Tag` use) for `style`, read from
/// `cfg`.
///
/// **D1 identical-coords helper.** Both [`shape_with_rustybuzz`] (via
/// `FontGrid::shape_run`) and `raster::rasterize_with_variations` (via
/// `FontGrid::raster_shaped`) MUST derive their coords from this one
/// function — never independently re-derive/convert variation coords from
/// `FontConfig` — so shaper and rasterizer structurally cannot drift apart.
pub fn variation_coords_for(cfg: &FontConfig, style: StyleKey) -> Vec<(u32, f32)> {
    variations_for_style(cfg, style)
        .iter()
        .map(|variation| (u32::from_be_bytes(variation.tag), variation.value))
        .collect()
}

/// Ligature/contextual features default OFF (REQ-SHAPE-2): `liga`, `calt`,
/// `dlig`. `cfg.features` can explicitly re-enable (or keep disabling) any
/// tag, including these three — a matching `FontFeature { enabled: true,
/// .. }` entry wins over the default.
fn feature_list(cfg: &FontConfig) -> Vec<HbFeature> {
    let mut enabled: HashMap<[u8; 4], bool> = HashMap::new();
    enabled.insert(*b"liga", false);
    enabled.insert(*b"calt", false);
    enabled.insert(*b"dlig", false);
    for feature in &cfg.features {
        enabled.insert(feature.tag, feature.enabled);
    }
    enabled
        .into_iter()
        .map(|(tag, on)| HbFeature::new(HbTag::from_bytes(&tag), u32::from(on), ..))
        .collect()
}

/// Shape `cells` (already resolved to a single face by the caller — a run
/// is single-face by construction, segmentation breaks at face boundaries)
/// against `font_data` via `rustybuzz`.
///
/// Pure function — no `FontGrid` access — so `FontGrid::shape_run` just
/// resolves the face + coords and memoizes the result via the shape cache.
pub(crate) fn shape_with_rustybuzz(
    font_data: &FontData,
    face_id: FaceId,
    px: f32,
    cells: &[ShapeCell],
    variation_coords: &[(u32, f32)],
    cfg: &FontConfig,
) -> Vec<ShapedGlyph> {
    let Some(mut face) = HbFace::from_slice(&font_data.bytes, font_data.index as u32) else {
        return Vec::new();
    };

    if !variation_coords.is_empty() {
        let variations: Vec<HbVariation> = variation_coords
            .iter()
            .map(|&(tag, value)| HbVariation {
                tag: HbTag(tag),
                value,
            })
            .collect();
        face.set_variations(&variations);
    }

    let units_per_em = face.units_per_em().max(1) as f32;
    let scale = px / units_per_em;

    let mut buffer = UnicodeBuffer::new();
    for (idx, cell) in cells.iter().enumerate() {
        buffer.add(cell.ch, idx as u32);
        for &mark in &cell.combining {
            buffer.add(mark, idx as u32);
        }
    }
    buffer.guess_segment_properties();
    // Force LTR: `ShapedGlyph::cluster` is documented as an ascending,
    // 0-based source-cell index (frozen seam), and `run_start_cell +
    // cluster` is how the renderer anchors every glyph. HarfBuzz only
    // guarantees that ascending ordering for LTR shaping; an RTL buffer
    // would hand back descending clusters and break that anchor math. noa
    // does not do bidi reordering (out of scope for this chain), so every
    // run is shaped left-to-right regardless of the guessed script.
    buffer.set_direction(Direction::LeftToRight);

    let features = feature_list(cfg);
    let output = rustybuzz::shape(&face, &features, buffer);

    output
        .glyph_infos()
        .iter()
        .zip(output.glyph_positions())
        .map(|(info, pos)| ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            face_id,
            x_advance: (pos.x_advance as f32 * scale).round() as i32,
            x_offset: (pos.x_offset as f32 * scale).round() as i32,
            y_offset: (pos.y_offset as f32 * scale).round() as i32,
            cluster: info.cluster,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FontFeature;

    fn cell(ch: char) -> ShapeCell {
        ShapeCell {
            ch,
            combining: Vec::new(),
            style: StyleKey::default(),
        }
    }

    #[test]
    fn config_digest_is_stable_and_reacts_to_relevant_changes() {
        let base = FontConfig::default();
        let d1 = config_digest(&base, StyleKey::default());
        let d2 = config_digest(&base, StyleKey::default());
        assert_eq!(d1, d2, "identical config must digest identically");

        let mut with_feature = base.clone();
        with_feature.features.push(FontFeature {
            tag: *b"calt",
            enabled: true,
        });
        assert_ne!(
            config_digest(&with_feature, StyleKey::default()),
            d1,
            "feature list changes must change the digest"
        );

        let mut with_variation = base.clone();
        with_variation.variations.push(FontVariation {
            tag: *b"wght",
            value: 700.0,
        });
        assert_ne!(
            config_digest(&with_variation, StyleKey::default()),
            d1,
            "variation list changes must change the digest"
        );
    }

    #[test]
    fn feature_list_defaults_ligatures_off_and_honors_explicit_enable() {
        let cfg = FontConfig::default();
        let features = feature_list(&cfg);
        let liga = features
            .iter()
            .find(|f| f.tag == HbTag::from_bytes(b"liga"))
            .expect("liga entry present");
        assert_eq!(liga.value, 0, "liga must default OFF (REQ-SHAPE-2)");

        let mut with_calt = FontConfig::default();
        with_calt.features.push(FontFeature {
            tag: *b"calt",
            enabled: true,
        });
        let features = feature_list(&with_calt);
        let calt = features
            .iter()
            .find(|f| f.tag == HbTag::from_bytes(b"calt"))
            .expect("calt entry present");
        assert_eq!(calt.value, 1, "explicit font-feature = calt must enable it");
    }

    #[test]
    fn variation_coords_for_selects_per_style_axis_list() {
        let mut cfg = FontConfig::default();
        cfg.variations.push(FontVariation {
            tag: *b"wght",
            value: 400.0,
        });
        cfg.variations_bold.push(FontVariation {
            tag: *b"wght",
            value: 700.0,
        });

        let regular = variation_coords_for(&cfg, StyleKey::default());
        let bold = variation_coords_for(
            &cfg,
            StyleKey {
                bold: true,
                italic: false,
            },
        );

        assert_eq!(regular, vec![(u32::from_be_bytes(*b"wght"), 400.0)]);
        assert_eq!(bold, vec![(u32::from_be_bytes(*b"wght"), 700.0)]);
    }

    #[test]
    fn shape_run_hash_differs_on_text_style_or_config() {
        let cfg = FontConfig::default();
        let digest = config_digest(&cfg, StyleKey::default());
        let a = shape_run_hash(&[cell('a'), cell('b')], StyleKey::default(), digest);
        let a_again = shape_run_hash(&[cell('a'), cell('b')], StyleKey::default(), digest);
        assert_eq!(a, a_again, "identical inputs must hash identically");

        let different_text = shape_run_hash(&[cell('a'), cell('c')], StyleKey::default(), digest);
        assert_ne!(a, different_text);

        let bold = StyleKey {
            bold: true,
            italic: false,
        };
        let different_style = shape_run_hash(&[cell('a'), cell('b')], bold, digest);
        assert_ne!(a, different_style);

        let mut with_feature = cfg.clone();
        with_feature.features.push(FontFeature {
            tag: *b"calt",
            enabled: true,
        });
        let different_cfg = shape_run_hash(
            &[cell('a'), cell('b')],
            StyleKey::default(),
            config_digest(&with_feature, StyleKey::default()),
        );
        assert_ne!(a, different_cfg);

        assert!(
            run_matches(
                &[('a', Vec::new()), ('b', Vec::new())],
                &[cell('a'), cell('b')]
            ),
            "identical runs must match"
        );
        assert!(
            !run_matches(&[('a', Vec::new())], &[cell('a'), cell('b')]),
            "length mismatch must not match"
        );
    }
}
