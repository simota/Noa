//! Font discovery (font-kit) and scaled font metrics (swash).

use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::Properties;
use font_kit::source::Source;
use font_kit::source::SystemSource;
use swash::FontRef;

use crate::FontError;

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

pub fn load_font_stack() -> Result<FontStack, FontError> {
    let source = SystemSource::new();
    let primary = load_monospace()?;
    let mut fallbacks = Vec::new();

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

fn push_valid_face(faces: &mut Vec<FontData>, handle: Handle) {
    let Ok(face) = handle_to_data(handle) else {
        return;
    };
    if face.font_ref().is_err() {
        return;
    }
    faces.push(face);
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
