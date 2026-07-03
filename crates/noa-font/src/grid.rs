//! `FontGrid`: the glyph cache tying discovery, rasterization and atlas
//! packing together behind a per-`char` cache.

use std::collections::HashMap;

use swash::FontRef;
use swash::scale::ScaleContext;

use crate::atlas::Atlas;
use crate::face::{FontStack, Metrics, load_font_stack};
use crate::raster::rasterize;
use crate::{FontConfig, FontError, GlyphInfo, GlyphKey};

/// Default atlas dimensions. The atlas grows on demand when glyph pressure exceeds it.
const ATLAS_DIM: u32 = 1024;
const MASK_BYTES_PER_PX: u32 = 1;
const COLOR_BYTES_PER_PX: u32 = 4;

/// Owns the font bytes, a swash scale context, cell metrics, the two glyph
/// atlases (R8 mask + RGBA8 color) and the per-`char` glyph cache.
///
/// Two independent atlases (WP1, REQ-EMOJI-2/3): non-color glyphs pack into
/// the R8 `mask_atlas` as before; color-bitmap glyphs (emoji) pack into the
/// RGBA8 `color_atlas` and are sampled as passthrough by the renderer
/// (`GlyphInfo.color = true`). Each atlas tracks its own generation counter
/// so the renderer can sync them independently.
pub struct FontGrid {
    /// Owned font bytes; `FontRef` borrows from here (kept for its lifetime).
    font_stack: FontStack,
    ctx: ScaleContext,
    metrics: Metrics,
    mask_atlas: Atlas,
    color_atlas: Atlas,
    cache: HashMap<GlyphKey, GlyphInfo>,
    px_size: f32,
}

impl FontGrid {
    /// Discover a monospace system font and build a grid at `px_size` ppem.
    ///
    /// `font_cfg` carries the configured family stack, features, variations
    /// etc. (see [`FontConfig`]); WP0 threads it through the constructor so
    /// later WPs can consume it for real without re-breaking this signature.
    pub fn new(px_size: f32, font_cfg: FontConfig) -> Result<Self, FontError> {
        let font_stack = load_font_stack(&font_cfg)?;
        let metrics = {
            let font = font_stack.primary().font_ref()?;
            Metrics::compute(font, px_size)
        };
        Ok(Self {
            font_stack,
            ctx: ScaleContext::new(),
            metrics,
            mask_atlas: Atlas::new(ATLAS_DIM, ATLAS_DIM, MASK_BYTES_PER_PX),
            color_atlas: Atlas::new(ATLAS_DIM, ATLAS_DIM, COLOR_BYTES_PER_PX),
            cache: HashMap::new(),
            px_size,
        })
    }

    /// Look up a glyph, rasterizing and packing it into the appropriate atlas
    /// (mask or color) on a miss.
    ///
    /// If the atlas cannot grow enough to fit a glyph, the returned zero-sized
    /// rect is not cached so a future larger atlas can make the glyph visible.
    pub fn get_or_raster(&mut self, ch: char) -> GlyphInfo {
        let key = GlyphKey { ch };
        if let Some(info) = self.cache.get(&key) {
            return *info;
        }

        let (font_index, glyph_id) = self.resolve_glyph(ch);

        // Borrow only `font_stack` data so `self.ctx` stays mutably borrowable.
        // Bytes were validated in `new`, so parsing here cannot fail.
        let font_data = &self.font_stack.faces()[font_index];
        let font = FontRef::from_index(&font_data.bytes, font_data.index)
            .expect("font bytes validated at construction");
        let glyph = rasterize(&mut self.ctx, font, glyph_id, self.px_size);

        let mut cache_info = true;
        let (atlas_pos, atlas_size) = if glyph.width == 0 || glyph.height == 0 {
            ([0, 0], [0, 0])
        } else {
            let target_atlas = if glyph.color {
                &mut self.color_atlas
            } else {
                &mut self.mask_atlas
            };
            match target_atlas.reserve_and_blit_growing(glyph.width, glyph.height, &glyph.bitmap) {
                Some((x, y)) => ([x, y], [glyph.width as u16, glyph.height as u16]),
                None => {
                    cache_info = false;
                    log::warn!("glyph atlas full; not caching glyph {ch:?}");
                    ([0, 0], [0, 0])
                }
            }
        };

        let info = GlyphInfo {
            atlas_pos,
            atlas_size,
            bearing: [glyph.bearing_x as i16, glyph.bearing_y as i16],
            advance: glyph.advance,
            color: glyph.color,
        };
        if cache_info {
            self.cache.insert(key, info);
        }
        info
    }

    fn resolve_glyph(&self, ch: char) -> (usize, u16) {
        for (font_index, font_data) in self.font_stack.faces().iter().enumerate() {
            let font = FontRef::from_index(&font_data.bytes, font_data.index)
                .expect("font bytes validated at construction");
            let glyph_id = font.charmap().map(ch);
            if glyph_id != 0 {
                return (font_index, glyph_id);
            }
        }
        (0, 0)
    }

    #[cfg(test)]
    fn has_glyph(&self, ch: char) -> bool {
        self.resolve_glyph(ch).1 != 0
    }

    /// Cell / face metrics at the configured size.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Borrow the R8 mask atlas pixel buffer.
    pub fn mask_atlas_data(&self) -> &[u8] {
        self.mask_atlas.data()
    }

    /// Mask atlas dimensions `(width, height)`.
    pub fn mask_atlas_size(&self) -> (u32, u32) {
        self.mask_atlas.size()
    }

    /// Monotonic mask atlas mutation generation.
    pub fn mask_atlas_generation(&self) -> u64 {
        self.mask_atlas.generation()
    }

    /// Borrow the RGBA8 color atlas pixel buffer.
    pub fn color_atlas_data(&self) -> &[u8] {
        self.color_atlas.data()
    }

    /// Color atlas dimensions `(width, height)`.
    pub fn color_atlas_size(&self) -> (u32, u32) {
        self.color_atlas.size()
    }

    /// Monotonic color atlas mutation generation.
    pub fn color_atlas_generation(&self) -> u64 {
        self.color_atlas.generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_raster() {
        let mut grid = match FontGrid::new(14.0, FontConfig::default()) {
            Ok(g) => g,
            Err(e) => {
                // No system font in this environment; skip rather than fail.
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        let m = grid.metrics();
        assert!(m.cell_w > 0.0, "cell_w must be positive, got {}", m.cell_w);
        assert!(m.cell_h > 0.0, "cell_h must be positive, got {}", m.cell_h);

        let info = grid.get_or_raster('M');
        assert!(
            info.atlas_size[0] > 0 && info.atlas_size[1] > 0,
            "'M' should rasterize to a non-empty atlas region: {:?}",
            info.atlas_size
        );
        let generation = grid.mask_atlas_generation();
        assert!(generation > 0, "rastering 'M' should advance the atlas");

        // Cache hit: same info, no new dirty.
        let info2 = grid.get_or_raster('M');
        assert_eq!(info.atlas_pos, info2.atlas_pos);
        assert_eq!(grid.mask_atlas_generation(), generation);
    }

    #[test]
    fn japanese_glyph_uses_fallback_face_when_available() {
        let mut grid = match FontGrid::new(14.0, FontConfig::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        if !grid.has_glyph('日') {
            eprintln!("skipping: no installed font can render Japanese");
            return;
        }

        let info = grid.get_or_raster('日');

        assert!(
            info.atlas_size[0] > 0 && info.atlas_size[1] > 0,
            "'日' should rasterize to a non-empty atlas region: {:?}",
            info.atlas_size
        );
        assert!(grid.mask_atlas_generation() > 0);
    }

    #[test]
    fn emoji_glyph_rasterizes_into_color_atlas_not_mask_atlas() {
        let mut grid = match FontGrid::new(14.0, FontConfig::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        if !grid.has_glyph('\u{1F600}') {
            eprintln!("skipping: no installed font can render emoji");
            return;
        }

        let mask_generation_before = grid.mask_atlas_generation();
        let color_generation_before = grid.color_atlas_generation();

        let info = grid.get_or_raster('\u{1F600}');

        assert!(
            info.atlas_size[0] > 0 && info.atlas_size[1] > 0,
            "emoji should rasterize to a non-empty atlas region: {:?}",
            info.atlas_size
        );
        assert!(info.color, "emoji glyph must be flagged as a color glyph");
        assert!(
            grid.color_atlas_generation() > color_generation_before,
            "emoji glyph must be packed into the color atlas"
        );
        assert_eq!(
            grid.mask_atlas_generation(),
            mask_generation_before,
            "emoji glyph must not touch the mask atlas"
        );

        // The color atlas byte buffer is RGBA8 (4 bytes/px) sized.
        let (cw, ch) = grid.color_atlas_size();
        assert_eq!(grid.color_atlas_data().len(), cw as usize * ch as usize * 4);
    }

    #[test]
    fn atlas_growth_keeps_glyphs_visible_after_initial_atlas_is_exceeded() {
        let mut grid = match FontGrid::new(220.0, FontConfig::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        let initial_size = grid.mask_atlas_size();
        let mut rastered = 0;

        for ch in large_visible_glyph_set() {
            if !grid.has_glyph(ch) {
                continue;
            }

            let info = grid.get_or_raster(ch);
            assert!(
                info.atlas_size[0] > 0 && info.atlas_size[1] > 0,
                "{ch:?} should stay visible after atlas pressure: {:?}",
                info.atlas_size
            );
            rastered += 1;

            if grid.mask_atlas_size() != initial_size {
                return;
            }
        }

        panic!(
            "test did not raster enough glyphs to exceed initial atlas {initial_size:?}; rastered {rastered}"
        );
    }

    fn large_visible_glyph_set() -> impl Iterator<Item = char> {
        ('!'..='~')
            .chain('\u{00A1}'..='\u{00AC}')
            .chain('\u{00AE}'..='\u{017F}')
            .chain('\u{0370}'..='\u{03FF}')
            .chain('\u{0400}'..='\u{04FF}')
            .chain('\u{3041}'..='\u{3096}')
            .chain('\u{30A1}'..='\u{30FA}')
            .chain('\u{4E00}'..='\u{4E80}')
    }
}
