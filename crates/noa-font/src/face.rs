//! Font discovery (font-kit) and scaled font metrics (swash).

use font_kit::family_name::FamilyName;
use font_kit::font::Font;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
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
    regular_faces: Vec<usize>,
    bold_faces: Vec<usize>,
    italic_faces: Vec<usize>,
    bold_italic_faces: Vec<usize>,
    native_bold_face: Option<usize>,
    native_italic_face: Option<usize>,
    native_bold_italic_face: Option<usize>,
}

impl FontStack {
    pub fn new(
        primary: FontData,
        bold_primary: Option<FontData>,
        italic_primary: Option<FontData>,
        bold_italic_primary: Option<FontData>,
        fallbacks: Vec<FontData>,
    ) -> Self {
        let mut faces = Vec::with_capacity(
            1 + usize::from(bold_primary.is_some())
                + usize::from(italic_primary.is_some())
                + usize::from(bold_italic_primary.is_some())
                + fallbacks.len(),
        );
        let primary_index = push_unique_face(&mut faces, primary);
        let native_bold_face = push_native_style_face(&mut faces, primary_index, bold_primary);
        let native_italic_face = push_native_style_face(&mut faces, primary_index, italic_primary);
        let native_bold_italic_face =
            push_native_style_face(&mut faces, primary_index, bold_italic_primary);

        let fallback_faces: Vec<_> = fallbacks
            .into_iter()
            .map(|face| push_unique_face(&mut faces, face))
            .collect();

        let regular_faces = stack_for_style(primary_index, None, &fallback_faces);
        let bold_faces = stack_for_style(primary_index, native_bold_face, &fallback_faces);
        let italic_faces = stack_for_style(primary_index, native_italic_face, &fallback_faces);
        let bold_italic_faces =
            stack_for_style(primary_index, native_bold_italic_face, &fallback_faces);

        Self {
            faces,
            regular_faces,
            bold_faces,
            italic_faces,
            bold_italic_faces,
            native_bold_face,
            native_italic_face,
            native_bold_italic_face,
        }
    }

    pub fn primary(&self) -> &FontData {
        &self.faces[self.regular_faces[0]]
    }

    pub fn faces(&self) -> &[FontData] {
        &self.faces
    }

    pub fn face_indices_for_style(&self, style: FontStyle) -> &[usize] {
        match style {
            FontStyle::Regular => &self.regular_faces,
            FontStyle::Bold => &self.bold_faces,
            FontStyle::Italic => &self.italic_faces,
            FontStyle::BoldItalic => &self.bold_italic_faces,
        }
    }

    pub fn is_native_style_face(&self, face_index: usize, style: FontStyle) -> bool {
        match style {
            FontStyle::Regular => true,
            FontStyle::Bold => self.native_bold_face == Some(face_index),
            FontStyle::Italic => self.native_italic_face == Some(face_index),
            FontStyle::BoldItalic => self.native_bold_italic_face == Some(face_index),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub fn from_bold_italic(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }
}

fn stack_for_style(
    primary_index: usize,
    native_style_index: Option<usize>,
    fallback_faces: &[usize],
) -> Vec<usize> {
    let mut stack = Vec::with_capacity(1 + fallback_faces.len());
    stack.push(native_style_index.unwrap_or(primary_index));
    stack.extend_from_slice(fallback_faces);
    stack
}

fn push_unique_face(faces: &mut Vec<FontData>, face: FontData) -> usize {
    if let Some((idx, _)) = faces
        .iter()
        .enumerate()
        .find(|(_, existing)| font_data_matches(existing, &face))
    {
        return idx;
    }

    let idx = faces.len();
    faces.push(face);
    idx
}

fn push_native_style_face(
    faces: &mut Vec<FontData>,
    primary_index: usize,
    face: Option<FontData>,
) -> Option<usize> {
    let face = face?;
    if font_data_matches(&faces[primary_index], &face) {
        return None;
    }
    Some(push_unique_face(faces, face))
}

fn font_data_matches(a: &FontData, b: &FontData) -> bool {
    a.index == b.index && a.bytes == b.bytes
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
    load_monospace_from_source(&source)
}

fn load_monospace_from_source(source: &SystemSource) -> Result<FontData, FontError> {
    let properties = properties_for_style(FontStyle::Regular);
    let handle = source
        .select_best_match(&[FamilyName::Monospace], &properties)
        .or_else(|_| select_title_best_match(source, "Menlo", &properties))
        .map_err(|_| FontError::NoFont)?;

    handle_to_data(handle)
}

/// Discover the font stack described by `font_cfg`, falling back to the
/// system monospace / Menlo discovery (see [`load_monospace`]) when
/// `font_cfg.families` is empty or none of the configured families resolve.
///
/// Configured regular/bold/italic families are resolved with font-kit's CSS
/// matcher. If a native style face is unavailable, the style stack falls back
/// to the regular primary and rasterization may synthesize the missing style.
pub fn load_font_stack(font_cfg: &FontConfig) -> Result<FontStack, FontError> {
    let source = SystemSource::new();
    let primary = match load_configured_primary(&source, font_cfg) {
        Some(primary) => primary,
        None => load_monospace()?,
    };
    let bold_primary = load_style_primary(&source, font_cfg, FontStyle::Bold);
    let italic_primary = load_style_primary(&source, font_cfg, FontStyle::Italic);
    let bold_italic_primary = load_style_primary(&source, font_cfg, FontStyle::BoldItalic);
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

    Ok(FontStack::new(
        primary,
        bold_primary,
        italic_primary,
        bold_italic_primary,
        fallbacks,
    ))
}

/// Try each configured family name in order, returning the first that
/// resolves to a loadable face. `None` (not an error) means the caller
/// should fall back to system monospace discovery.
fn load_configured_primary(source: &SystemSource, font_cfg: &FontConfig) -> Option<FontData> {
    let properties = properties_for_style(FontStyle::Regular);
    load_first_matching_family(source, &font_cfg.families, &properties, None)
}

fn load_style_primary(
    source: &SystemSource,
    font_cfg: &FontConfig,
    style: FontStyle,
) -> Option<FontData> {
    let properties = properties_for_style(style);
    let explicit_families = match style {
        FontStyle::Regular => &font_cfg.families,
        FontStyle::Bold => &font_cfg.families_bold,
        FontStyle::Italic => &font_cfg.families_italic,
        FontStyle::BoldItalic => &font_cfg.families_bold_italic,
    };

    load_first_matching_family(source, explicit_families, &properties, Some(style))
        .or_else(|| {
            load_first_matching_family(source, &font_cfg.families, &properties, Some(style))
        })
        .or_else(|| load_system_style_primary(source, &properties, style))
}

fn load_system_style_primary(
    source: &SystemSource,
    properties: &Properties,
    style: FontStyle,
) -> Option<FontData> {
    source
        .select_best_match(&[FamilyName::Monospace], properties)
        .ok()
        .filter(|handle| handle_supports_style(handle, style))
        .and_then(load_valid_handle)
        .or_else(|| {
            select_title_best_match(source, "Menlo", properties)
                .ok()
                .filter(|handle| handle_supports_style(handle, style))
                .and_then(load_valid_handle)
        })
}

fn load_first_matching_family(
    source: &SystemSource,
    families: &[String],
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    families.iter().find_map(|family_name| {
        let handle = select_title_best_match(source, family_name, properties).ok()?;
        if let Some(style) = required_style
            && !handle_supports_style(&handle, style)
        {
            return None;
        }
        load_valid_handle(handle)
    })
}

fn select_title_best_match(
    source: &SystemSource,
    family_name: &str,
    properties: &Properties,
) -> Result<Handle, font_kit::error::SelectionError> {
    let family = [FamilyName::Title(family_name.to_string())];
    source.select_best_match(&family, properties)
}

fn load_valid_handle(handle: Handle) -> Option<FontData> {
    let data = handle_to_data(handle).ok()?;
    data.font_ref().is_ok().then_some(data)
}

fn properties_for_style(style: FontStyle) -> Properties {
    let mut properties = Properties::new();
    match style {
        FontStyle::Regular => {}
        FontStyle::Bold => {
            properties.weight(Weight::BOLD);
        }
        FontStyle::Italic => {
            properties.style(Style::Italic);
        }
        FontStyle::BoldItalic => {
            properties.weight(Weight::BOLD).style(Style::Italic);
        }
    }
    properties
}

fn handle_supports_style(handle: &Handle, style: FontStyle) -> bool {
    let Ok(font) = Font::from_handle(handle) else {
        return false;
    };
    let properties = font.properties();
    let wants_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let wants_italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
    let has_bold = !wants_bold || properties.weight >= Weight::SEMIBOLD;
    let has_italic = !wants_italic || matches!(properties.style, Style::Italic | Style::Oblique);
    has_bold && has_italic
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

        let cell_w = quantize_cell_dimension(cell_w);
        let cell_h = quantize_cell_dimension(ascent + descent + line_gap);

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

fn quantize_cell_dimension(value: f32) -> f32 {
    if value.is_finite() {
        value.round().max(1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontConfig;

    #[test]
    fn font_stack_tracks_native_style_faces_separately() {
        let primary = FontData {
            bytes: vec![1, 2, 3],
            index: 0,
        };
        let bold = FontData {
            bytes: vec![4, 5, 6],
            index: 0,
        };
        let fallback = FontData {
            bytes: vec![7, 8, 9],
            index: 0,
        };

        let stack = FontStack::new(primary, Some(bold), None, None, vec![fallback]);

        assert_eq!(stack.face_indices_for_style(FontStyle::Regular), &[0, 2]);
        assert_eq!(stack.face_indices_for_style(FontStyle::Bold), &[1, 2]);
        assert_eq!(stack.face_indices_for_style(FontStyle::Italic), &[0, 2]);
        assert!(stack.is_native_style_face(1, FontStyle::Bold));
        assert!(!stack.is_native_style_face(0, FontStyle::Bold));
    }

    #[test]
    fn duplicate_style_face_falls_back_to_regular_stack() {
        let primary = FontData {
            bytes: vec![1, 2, 3],
            index: 0,
        };
        let same_as_primary = FontData {
            bytes: vec![1, 2, 3],
            index: 0,
        };

        let stack = FontStack::new(primary, Some(same_as_primary), None, None, Vec::new());

        assert_eq!(stack.faces().len(), 1);
        assert_eq!(stack.face_indices_for_style(FontStyle::Bold), &[0]);
        assert!(!stack.is_native_style_face(0, FontStyle::Bold));
    }

    #[test]
    fn cell_metrics_are_quantized_to_whole_pixels() {
        assert_eq!(quantize_cell_dimension(7.49), 7.0);
        assert_eq!(quantize_cell_dimension(7.5), 8.0);
        assert_eq!(quantize_cell_dimension(0.25), 1.0);
        assert_eq!(quantize_cell_dimension(f32::NAN), 1.0);
    }

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
