//! Font discovery (font-kit) and scaled font metrics (swash).

use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::Properties;
use font_kit::source::Source;
use font_kit::source::SystemSource;
use swash::FontRef;

use crate::{FontConfig, FontError};

/// Raw font file bytes plus the face index within a collection.
///
/// swash's [`FontRef`] borrows these bytes, so [`crate::FontGrid`] keeps the
/// `Vec<u8>` owned alongside the derived `FontRef`.
pub struct FontData {
    pub bytes: Vec<u8>,
    pub index: usize,
}

impl FontData {
    /// Borrow a swash [`FontRef`] over the owned bytes.
    pub fn font_ref(&self) -> Result<FontRef<'_>, FontError> {
        FontRef::from_index(&self.bytes, self.index).ok_or(FontError::Parse)
    }
}

pub struct FontStack {
    faces: Vec<FontData>,
}

impl FontStack {
    pub fn new(primary: FontData, fallbacks: Vec<FontData>) -> Self {
        let mut faces = Vec::with_capacity(fallbacks.len() + 1);
        faces.push(primary);
        faces.extend(fallbacks);
        Self { faces }
    }

    pub fn primary(&self) -> &FontData {
        &self.faces[0]
    }

    pub fn faces(&self) -> &[FontData] {
        &self.faces
    }
}

/// Enumerate every font family name known to the system source, sorted and
/// deduplicated. Backs the `noa +list-fonts` CLI action.
pub fn list_families() -> Result<Vec<String>, FontError> {
    let mut families = SystemSource::new()
        .all_families()
        .map_err(|_| FontError::Enumerate)?;
    families.sort();
    families.dedup();
    Ok(families)
}

/// Discover a monospace system font and load its raw bytes.
///
/// Tries `select_best_match(Monospace)` first, then falls back to `Menlo`
/// (the macOS default terminal face).
pub fn load_monospace() -> Result<FontData, FontError> {
    let source = SystemSource::new();

    let handle = source
        .select_best_match(&[FamilyName::Monospace], &Properties::new())
        .or_else(|_| {
            source.select_family_by_name("Menlo").and_then(|family| {
                family
                    .fonts()
                    .first()
                    .cloned()
                    .ok_or(font_kit::error::SelectionError::NotFound)
            })
        })
        .map_err(|_| FontError::NoFont)?;

    handle_to_data(handle)
}

/// Discover the font stack described by `font_cfg`, falling back to the
/// system monospace / Menlo discovery (see [`load_monospace`]) when
/// `font_cfg.families` is empty or none of the configured families resolve.
///
/// WP0 wires the config input through so the constructor signature doesn't
/// need to change again later; fully resolving custom family stacks
/// (weights/styles, missing-family diagnostics, etc.) is WP1's job.
pub fn load_font_stack(font_cfg: &FontConfig) -> Result<FontStack, FontError> {
    let source = SystemSource::new();
    let primary = match load_configured_primary(&source, font_cfg) {
        Some(primary) => primary,
        None => load_monospace()?,
    };
    let mut fallbacks = Vec::new();

    // Probe for the system color-emoji family first so emoji codepoints
    // resolve to it rather than falling through to a tofu/blank glyph or a
    // mismatched fallback face (REQ-EMOJI-1). Same family-by-name lookup
    // mechanism as the primary/CJK discovery below; simply yields no match
    // on platforms/sandboxes without it, so the stack proceeds without an
    // emoji face rather than failing.
    if let Ok(handle) = Source::select_family_by_name(&source, emoji_fallback_family_name())
        .and_then(|family| {
            family
                .fonts()
                .first()
                .cloned()
                .ok_or(font_kit::error::SelectionError::NotFound)
        })
    {
        push_valid_face(&mut fallbacks, handle);
    }

    for postscript_name in cjk_fallback_postscript_names() {
        if let Ok(handle) = Source::select_by_postscript_name(&source, postscript_name) {
            push_valid_face(&mut fallbacks, handle);
        }
    }
    for family_name in cjk_fallback_family_names() {
        let family = [FamilyName::Title((*family_name).to_string())];
        if let Ok(handle) = Source::select_best_match(&source, &family, &Properties::new()) {
            push_valid_face(&mut fallbacks, handle);
        }
    }

    Ok(FontStack::new(primary, fallbacks))
}

/// Try each configured family name in order, returning the first that
/// resolves to a loadable face. `None` (not an error) means the caller
/// should fall back to system monospace discovery.
fn load_configured_primary(source: &SystemSource, font_cfg: &FontConfig) -> Option<FontData> {
    font_cfg.families.iter().find_map(|family_name| {
        let handle = source
            .select_family_by_name(family_name)
            .ok()?
            .fonts()
            .first()
            .cloned()?;
        let data = handle_to_data(handle).ok()?;
        data.font_ref().is_ok().then_some(data)
    })
}

fn push_valid_face(faces: &mut Vec<FontData>, handle: Handle) {
    let Ok(face) = handle_to_data(handle) else {
        return;
    };
    if face.font_ref().is_err() {
        return;
    }
    faces.push(face);
}

/// The macOS system color-emoji family name (REQ-EMOJI-1). `font-kit`'s
/// `select_family_by_name` resolves this the same way it resolves any other
/// installed family, whether the backing source is CoreText (macOS) or
/// another platform's font database (where it will simply not be found).
fn emoji_fallback_family_name() -> &'static str {
    "Apple Color Emoji"
}

fn cjk_fallback_postscript_names() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[
            "HiraginoSans-W3",
            "HiraginoSans-W4",
            "HiraginoSans-W5",
            "HiraKakuProN-W3",
            "HiraKakuPro-W3",
            "YuGothic-Regular",
            "YuGothicUI-Regular",
            "NotoSansCJKjp-Regular",
            "NotoSansMonoCJKjp-Regular",
            "HiraginoSansGB-W3",
            "AppleGothic",
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[]
    }
}

fn cjk_fallback_family_names() -> &'static [&'static str] {
    &[
        "Hiragino Sans",
        "Hiragino Kaku Gothic ProN",
        "Yu Gothic",
        "Noto Sans CJK JP",
        "Noto Sans Mono CJK JP",
        "Hiragino Sans GB",
        "AppleGothic",
    ]
}

fn handle_to_data(handle: Handle) -> Result<FontData, FontError> {
    match handle {
        Handle::Memory { bytes, font_index } => Ok(FontData {
            bytes: bytes.as_ref().clone(),
            index: font_index as usize,
        }),
        Handle::Path { path, font_index } => {
            let bytes = std::fs::read(&path).map_err(|_| FontError::NoFont)?;
            Ok(FontData {
                bytes,
                index: font_index as usize,
            })
        }
    }
}

/// Scaled per-face metrics, in pixels, at a given pixels-per-em size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Advance width of the monospace cell (pixels).
    pub cell_w: f32,
    /// Full line height of the cell (pixels).
    pub cell_h: f32,
    /// Ascent above the baseline (pixels).
    pub ascent: f32,
    /// Descent below the baseline (pixels, positive).
    pub descent: f32,
    /// Extra leading between lines (pixels).
    pub line_gap: f32,
    /// Underline position relative to the baseline (pixels, y-up).
    pub underline_position: f32,
    /// Underline stroke thickness (pixels).
    pub underline_thickness: f32,
}

impl Metrics {
    /// Compute metrics for `font` scaled to `px` pixels-per-em.
    ///
    /// `cell_w` is the advance of a representative monospace glyph ('M' if
    /// present, else the font's average advance width).
    pub fn compute(font: FontRef<'_>, px: f32) -> Self {
        let m = font.metrics(&[]).scale(px);

        let ascent = m.ascent;
        let descent = m.descent;
        let line_gap = m.leading;

        // Advance of 'M' at this size; fall back to average advance width.
        let gmetrics = font.glyph_metrics(&[]).scale(px);
        let m_gid = font.charmap().map('M');
        let cell_w = if m_gid != 0 {
            gmetrics.advance_width(m_gid)
        } else {
            0.0
        };
        let cell_w = if cell_w > 0.0 {
            cell_w
        } else {
            m.average_width
        };
        let cell_w = if cell_w > 0.0 {
            cell_w
        } else {
            // Last-resort estimate for degenerate faces.
            px * 0.6
        };

        let cell_h = (ascent + descent + line_gap).ceil().max(1.0);

        Metrics {
            cell_w,
            cell_h,
            ascent,
            descent,
            line_gap,
            underline_position: m.underline_offset,
            underline_thickness: if m.stroke_size > 0.0 {
                m.stroke_size
            } else {
                (px / 14.0).max(1.0)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontConfig;

    /// AC-WP1-01 (REQ-EMOJI-1): given the resolved fallback stack from
    /// `load_font_stack`, an emoji codepoint resolves to the Apple Color
    /// Emoji face specifically — not merely *some* fallback face, and not a
    /// tofu/blank glyph.
    #[test]
    fn emoji_codepoint_resolves_to_apple_color_emoji_face() {
        let source = SystemSource::new();
        let Ok(emoji_family) = source.select_family_by_name(emoji_fallback_family_name()) else {
            eprintln!(
                "skipping: {} not installed in this environment",
                emoji_fallback_family_name()
            );
            return;
        };
        let Some(emoji_handle) = emoji_family.fonts().first().cloned() else {
            eprintln!(
                "skipping: {} family has no fonts",
                emoji_fallback_family_name()
            );
            return;
        };
        let emoji_data =
            handle_to_data(emoji_handle).expect("load Apple Color Emoji bytes directly");

        let stack = match load_font_stack(&FontConfig::default()) {
            Ok(stack) => stack,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        // 😀 U+1F600 GRINNING FACE.
        let emoji_ch = '\u{1F600}';
        let resolved = stack.faces().iter().find(|face| {
            let Ok(font) = face.font_ref() else {
                return false;
            };
            font.charmap().map(emoji_ch) != 0
        });

        let resolved = resolved.expect(
            "an emoji codepoint must resolve to some face in the fallback stack, \
             not fall through to a tofu/blank glyph",
        );
        assert_eq!(
            resolved.bytes, emoji_data.bytes,
            "emoji codepoint should resolve to the Apple Color Emoji face specifically, \
             not an unrelated fallback face"
        );
    }

    /// `+list-fonts` relies on this shape: at least one family exists on a
    /// system with fonts, and the list is strictly sorted (which also proves
    /// deduplication).
    #[test]
    fn list_families_is_non_empty_sorted_and_deduped() {
        let families = list_families().expect("system font source should enumerate");

        assert!(!families.is_empty());
        assert!(
            families.windows(2).all(|pair| pair[0] < pair[1]),
            "family list must be strictly sorted (sorted + deduped)"
        );
    }
}
