//! `FontGrid`: the glyph cache tying discovery, rasterization and atlas
//! packing together behind a per-`char` cache.

use std::collections::HashMap;

use swash::FontRef;
use swash::scale::ScaleContext;

use crate::atlas::Atlas;
use crate::face::{FontStack, Metrics, load_font_stack};
use crate::raster::rasterize;
use crate::{FontError, GlyphInfo, GlyphKey};

/// Default atlas dimensions (R8). The atlas grows on demand when glyph pressure exceeds it.
const ATLAS_DIM: u32 = 1024;

/// Owns the font bytes, a swash scale context, cell metrics, the glyph atlas
/// and the per-`char` glyph cache.
pub struct FontGrid {
    /// Owned font bytes; `FontRef` borrows from here (kept for its lifetime).
    font_stack: FontStack,
    ctx: ScaleContext,
    metrics: Metrics,
    atlas: Atlas,
    cache: HashMap<GlyphKey, GlyphInfo>,
    px_size: f32,
}

impl FontGrid {
    /// Discover a monospace system font and build a grid at `px_size` ppem.
    pub fn new(px_size: f32) -> Result<Self, FontError> {
        let font_stack = load_font_stack()?;
        let metrics = {
            let font = font_stack.primary().font_ref()?;
            Metrics::compute(font, px_size)
        };
        Ok(Self {
            font_stack,
            ctx: ScaleContext::new(),
            metrics,
            atlas: Atlas::new(ATLAS_DIM, ATLAS_DIM),
            cache: HashMap::new(),
            px_size,
        })
    }

    /// Look up a glyph, rasterizing and packing it into the atlas on a miss.
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
            match self
                .atlas
                .reserve_and_blit_growing(glyph.width, glyph.height, &glyph.bitmap)
            {
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

    /// Borrow the R8 atlas pixel buffer.
    pub fn atlas_data(&self) -> &[u8] {
        self.atlas.data()
    }

    /// Atlas dimensions `(width, height)`.
    pub fn atlas_size(&self) -> (u32, u32) {
        self.atlas.size()
    }

    /// Monotonic atlas mutation generation.
    pub fn atlas_generation(&self) -> u64 {
        self.atlas.generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_raster() {
        let mut grid = match FontGrid::new(14.0) {
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
        let generation = grid.atlas_generation();
        assert!(generation > 0, "rastering 'M' should advance the atlas");

        // Cache hit: same info, no new dirty.
        let info2 = grid.get_or_raster('M');
        assert_eq!(info.atlas_pos, info2.atlas_pos);
        assert_eq!(grid.atlas_generation(), generation);
    }

    #[test]
    fn japanese_glyph_uses_fallback_face_when_available() {
        let mut grid = match FontGrid::new(14.0) {
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
        assert!(grid.atlas_generation() > 0);
    }

    #[test]
    fn atlas_growth_keeps_glyphs_visible_after_initial_atlas_is_exceeded() {
        let mut grid = match FontGrid::new(220.0) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        let initial_size = grid.atlas_size();
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

            if grid.atlas_size() != initial_size {
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
