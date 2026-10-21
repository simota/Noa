//! Title renderer using ab_glyph.
//!
//! Requires no dynamically linked dependencies.
//!
//! Can fallback to a embedded Cantarell-Regular.ttf font (SIL Open Font Licence v1.1)
//! if the system font doesn't work.
use crate::title::{config, font_preference::FontPreference};
use ab_glyph::{point, Font, FontRef, Glyph, PxScale, PxScaleFont, ScaleFont, VariableFont};
use std::{collections::HashMap, fs::File, process::Command};
use tiny_skia::{Color, Pixmap, PremultipliedColorU8};

const CANTARELL: &[u8] = include_bytes!("Cantarell-Regular.ttf");

#[derive(Debug)]
pub struct AbGlyphTitleText {
    title: String,
    font: Option<(memmap2::Mmap, FontPreference)>,
    /// Fonts that cover title characters the primary font lacks, as
    /// (mmapped file, face index). noa patch: upstream has no fallback, so a
    /// CJK title rendered as tofu.
    fallback_fonts: Vec<(memmap2::Mmap, u32)>,
    /// Which `fallback_fonts` entry draws each char the primary font lacks
    /// (`None`: no installed font covers it; drawn as the primary's notdef).
    fallback_for: HashMap<char, Option<usize>>,
    original_px_size: f32,
    size: PxScale,
    color: Color,
    pixmap: Option<Pixmap>,
}

impl AbGlyphTitleText {
    pub fn new(color: Color) -> Self {
        let font_pref = config::titlebar_font().unwrap_or_default();
        let font_pref_pt_size = font_pref.pt_size;
        let font = font_file_matching(&font_pref)
            .and_then(|f| mmap(&f))
            .map(|mmap| (mmap, font_pref));

        let size = parse_font(&font)
            .pt_to_px_scale(font_pref_pt_size)
            .unwrap_or_else(|| {
                log::error!("invalid font units_per_em");
                PxScale { x: 17.6, y: 17.6 }
            });

        Self {
            title: <_>::default(),
            font,
            fallback_fonts: Vec::new(),
            fallback_for: HashMap::new(),
            original_px_size: size.x,
            size,
            color,
            pixmap: None,
        }
    }

    pub fn update_scale(&mut self, scale: u32) {
        let new_scale = PxScale::from(self.original_px_size * scale as f32);
        if (self.size.x - new_scale.x).abs() > f32::EPSILON {
            self.size = new_scale;
            self.pixmap = self.render();
        }
    }

    pub fn update_title(&mut self, title: impl Into<String>) {
        let new_title = title.into();
        if new_title != self.title {
            self.title = new_title;
            self.resolve_fallbacks();
            self.pixmap = self.render();
        }
    }

    pub fn update_color(&mut self, color: Color) {
        if color != self.color {
            self.color = color;
            self.pixmap = self.render();
        }
    }

    pub fn pixmap(&self) -> Option<&Pixmap> {
        self.pixmap.as_ref()
    }

    /// Find a fallback font for every title char the primary font lacks.
    /// A char an already-loaded fallback covers reuses it, so a CJK title
    /// spawns `fc-match` about once rather than once per character.
    fn resolve_fallbacks(&mut self) {
        let primary = parse_font(&self.font);
        let pattern = self.font.as_ref().map(|(_, pref)| fc_pattern(pref));
        for c in self.title.chars() {
            if c.is_control() || primary.glyph_id(c).0 != 0 || self.fallback_for.contains_key(&c)
            {
                continue;
            }
            let covers = |(data, index): &(memmap2::Mmap, u32)| {
                FontRef::try_from_slice_and_index(data, *index)
                    .is_ok_and(|f| f.glyph_id(c).0 != 0)
            };
            let mut found = self.fallback_fonts.iter().position(covers);
            if found.is_none() {
                let pattern = pattern.as_deref().unwrap_or("sans-serif");
                if let Some(face) = fallback_face_for(pattern, c).filter(covers) {
                    self.fallback_fonts.push(face);
                    found = Some(self.fallback_fonts.len() - 1);
                }
            }
            self.fallback_for.insert(c, found);
        }
    }

    /// Render returning the new `Pixmap`.
    fn render(&self) -> Option<Pixmap> {
        let bold = self.font.as_ref().is_some_and(|(_, pref)| is_bold(pref));
        let mut fonts = vec![parse_font(&self.font)];
        for (data, index) in &self.fallback_fonts {
            fonts.push(with_weight(
                FontRef::try_from_slice_and_index(data, *index).ok()?,
                bold,
            ));
        }
        let fonts: Vec<_> = fonts.iter().map(|f| f.as_scaled(self.size)).collect();

        let glyphs = self.layout(&fonts);
        let (last_font, last_glyph) = glyphs.last()?;
        let used = || glyphs.iter().map(|(i, _)| &fonts[*i]);
        let ascent = used().map(|f| f.ascent()).fold(fonts[0].ascent(), f32::max);
        let descent = used().map(|f| f.descent()).fold(fonts[0].descent(), f32::min);
        // + 2 because ab_glyph likes to draw outside of its area,
        // so we add 1px border around the pixmap
        let width = (last_glyph.position.x + fonts[*last_font].h_advance(last_glyph.id)).ceil()
            as u32
            + 2;
        let height = (ascent - descent).ceil() as u32 + 2;

        let mut pixmap = Pixmap::new(width, height)?;

        let pixels = pixmap.pixels_mut();

        for (font, mut glyph) in glyphs {
            glyph.position.y = ascent;
            if let Some(outline) = fonts[font].outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let left = bounds.min.x as u32;
                let top = bounds.min.y as u32;
                outline.draw(|x, y, c| {
                    // `ab_glyph` may return values greater than 1.0, but they are defined to be
                    // same as 1.0. For our purposes, we need to constrain this value.
                    let c = c.min(1.0);

                    // offset the index by 1, so it is in the center of the pixmap
                    let p_idx = (top + y + 1) * width + (left + x + 1);
                    let Some(pixel) = pixels.get_mut(p_idx as usize) else {
                        log::warn!("Ignoring out of range pixel (pixel id: {p_idx}");
                        return;
                    };

                    let old_alpha_u8 = pixel.alpha();

                    let new_alpha = c + (old_alpha_u8 as f32 / 255.0);
                    if let Some(px) = PremultipliedColorU8::from_rgba(
                        (self.color.red() * new_alpha * 255.0) as _,
                        (self.color.green() * new_alpha * 255.0) as _,
                        (self.color.blue() * new_alpha * 255.0) as _,
                        (new_alpha * 255.0) as _,
                    ) {
                        *pixel = px;
                    }
                })
            }
        }

        Some(pixmap)
    }

    /// Simple single-line glyph layout, as (index into `fonts`, glyph).
    /// `fonts[0]` is the primary font; the rest are `fallback_fonts`, offset
    /// by one. The caller sets the baseline (`position.y`).
    fn layout(&self, fonts: &[PxScaleFont<impl Font>]) -> Vec<(usize, Glyph)> {
        let mut caret = point(0.0, 0.0);
        let mut last_glyph: Option<(usize, Glyph)> = None;
        let mut target = Vec::new();
        for c in self.title.chars() {
            if c.is_control() {
                continue;
            }
            let font_idx = match self.fallback_for.get(&c) {
                Some(Some(i)) => i + 1,
                _ => 0,
            };
            let font = &fonts[font_idx];
            let mut glyph = font.scaled_glyph(c);
            if let Some((previous_font, previous)) = last_glyph.take() {
                // Kerning pairs only exist within one font.
                if previous_font == font_idx {
                    caret.x += font.kern(previous.id, glyph.id);
                }
            }
            glyph.position = caret;

            last_glyph = Some((font_idx, glyph.clone()));
            caret.x += font.h_advance(glyph.id);

            target.push((font_idx, glyph));
        }
        target
    }
}

/// Parse the memmapped system font or fallback to built-in cantarell.
fn parse_font(sys_font: &Option<(memmap2::Mmap, FontPreference)>) -> FontRef<'_> {
    match sys_font {
        Some((mmap, font_pref)) => {
            FontRef::try_from_slice(mmap)
                .map(|f| with_weight(f, is_bold(font_pref)))
                .unwrap_or_else(|_| {
                    // We control the default font, so I guess it's fine to unwrap it
                    #[allow(clippy::unwrap_used)]
                    FontRef::try_from_slice(CANTARELL).unwrap()
                })
        }
        // We control the default font, so I guess it's fine to unwrap it
        #[allow(clippy::unwrap_used)]
        _ => FontRef::try_from_slice(CANTARELL).unwrap(),
    }
}

fn is_bold(pref: &FontPreference) -> bool {
    pref.style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("bold"))
}

/// Basic "bold" handling for variable fonts.
fn with_weight(mut font: FontRef<'_>, bold: bool) -> FontRef<'_> {
    if bold {
        font.set_variation(b"wght", 700.0);
    }
    font
}

fn fc_pattern(pref: &FontPreference) -> String {
    let mut pattern = pref.name.clone();
    if let Some(style) = &pref.style {
        pattern.push(':');
        pattern.push_str(style);
    }
    pattern
}

/// The face Fontconfig picks for `c` in the style of `pattern`. Charset
/// outranks family in Fontconfig matching, so this lands on a covering font
/// when one is installed; the caller still checks coverage.
fn fallback_face_for(pattern: &str, c: char) -> Option<(memmap2::Mmap, u32)> {
    let out = Command::new("fc-match")
        .arg("-f")
        .arg("%{file}\n%{index}")
        .arg(format!("{pattern}:charset={:x}", c as u32))
        .output()
        .ok()?;
    let out = String::from_utf8(out.stdout).ok()?;
    let (path, index) = out.split_once('\n')?;
    let face = mmap(&File::open(path).ok()?)?;
    Some((face, index.trim().parse().ok()?))
}

/// Font-config without dynamically linked dependencies
fn font_file_matching(pref: &FontPreference) -> Option<File> {
    Command::new("fc-match")
        .arg("-f")
        .arg("%{file}")
        .arg(fc_pattern(pref))
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|path| File::open(path.trim()).ok())
}

fn mmap(file: &File) -> Option<memmap2::Mmap> {
    // Safety: System font files are not expected to be mutated during use
    unsafe { memmap2::Mmap::map(file).ok() }
}

