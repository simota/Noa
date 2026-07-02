//! `FontGrid`: the glyph cache tying discovery, rasterization and atlas
//! packing together behind a per-`char` cache.

use std::collections::HashMap;

use swash::FontRef;
use swash::scale::ScaleContext;

use crate::atlas::Atlas;
use crate::face::{FontStack, Metrics, load_font_stack};
use crate::raster::rasterize;
use crate::{FontError, GlyphInfo, GlyphKey};

/// Default atlas dimensions (R8). Grows are a future task; see TODO below.
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
    /// On atlas-full the glyph is cached with a zero-sized atlas rect so the
    /// caller still gets a valid advance and we don't retry every frame.
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

        let (atlas_pos, atlas_size) = if glyph.width == 0 || glyph.height == 0 {
            ([0, 0], [0, 0])
        } else {
            match self
                .atlas
                .reserve_and_blit(glyph.width, glyph.height, &glyph.bitmap)
            {
                Some((x, y)) => ([x, y], [glyph.width as u16, glyph.height as u16]),
                None => {
                    // TODO(agent): grow the atlas instead of dropping glyphs.
                    log::warn!("glyph atlas full; dropping glyph {ch:?}");
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
        self.cache.insert(key, info);
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

    /// Return whether the atlas changed since the last call, clearing the flag.
    pub fn take_atlas_dirty(&mut self) -> bool {
        self.atlas.take_dirty()
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
        assert!(grid.take_atlas_dirty(), "rastering 'M' should dirty the atlas");
        assert!(!grid.take_atlas_dirty(), "dirty flag should clear");

        // Cache hit: same info, no new dirty.
        let info2 = grid.get_or_raster('M');
        assert_eq!(info.atlas_pos, info2.atlas_pos);
        assert!(!grid.take_atlas_dirty());
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
        assert!(
            grid.take_atlas_dirty(),
            "rastering '日' should dirty the atlas"
        );
    }
}
