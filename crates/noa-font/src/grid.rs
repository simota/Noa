//! `FontGrid`: the glyph cache tying discovery, rasterization and atlas
//! packing together behind a per-`char` cache.

use std::collections::HashMap;

use etagere::AllocId;
use swash::FontRef;
use swash::scale::ScaleContext;

use crate::atlas::Atlas;
use crate::boxdraw::{self, is_builtin_glyph};
use crate::face::{FontStack, Metrics, load_font_stack};
use crate::raster::{GlyphSynthesis, RasterizedGlyph, rasterize_with_variations};
use crate::shape::{self, FaceId, ShapeCell, ShapeRunKey, ShapedGlyph, StyleKey};
use crate::{FontConfig, FontError, GlyphInfo, GlyphKey};

/// Default atlas dimensions. The atlas grows on demand when glyph pressure exceeds it.
const ATLAS_DIM: u32 = 1024;
const MASK_BYTES_PER_PX: u32 = 1;
const COLOR_BYTES_PER_PX: u32 = 4;

/// Cap on the number of memoized shape runs (REQ-SHAPE-5). Past the cap,
/// `shape_run` evicts the least-recently-used entry before inserting a new
/// one (LRU), mirroring the glyph atlas's own eviction policy.
const SHAPE_CACHE_CAP: usize = 8192;

/// Which of the two atlases a packed glyph lives in — so eviction only frees
/// space from the atlas that is actually full.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AtlasKind {
    Mask,
    Color,
}

/// Which cache map + key owns a packed atlas region, so eviction can drop the
/// now-stale [`GlyphInfo`] when it reclaims the region's space.
#[derive(Clone, Copy, Debug)]
enum SlotOwner {
    Char(GlyphKey),
    Shaped(ShapedGlyphKey),
}

/// One live atlas allocation, tracked for LRU eviction. `alloc` is the
/// etagere handle used to free the region; `last_used` is the access clock
/// stamp (smallest = least-recently-used).
struct AtlasSlot {
    kind: AtlasKind,
    alloc: AllocId,
    owner: SlotOwner,
    last_used: u64,
}

/// A cached glyph plus, when it occupies atlas space, the id of its
/// [`AtlasSlot`]. Zero-sized glyphs (nothing to draw) carry `slot: None`.
#[derive(Clone, Copy)]
struct Cached {
    info: GlyphInfo,
    slot: Option<u32>,
}

/// Cache key for the shaped-glyph raster path ([`FontGrid::raster_shaped`]):
/// a rasterized glyph identified by face + glyph id + style (style matters
/// because it selects both the variation coords and the synthetic-style
/// transform).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ShapedGlyphKey {
    face_id: FaceId,
    glyph_id: u16,
    style: StyleKey,
}

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
    cache: HashMap<GlyphKey, Cached>,
    px_size: f32,
    /// The config this grid was built with (WP0); `shape_run`/`raster_shaped`
    /// read features/variations/synthetic-style from here (WP2) so callers
    /// don't have to pass it on every call.
    font_cfg: FontConfig,
    /// Per-run shape cache (REQ-SHAPE-5): memoizes `shape_run` so an
    /// unchanged run doesn't re-invoke `rustybuzz` every frame. LRU-evicted
    /// at `SHAPE_CACHE_CAP` (`last_used` = access clock stamp).
    shape_cache: HashMap<ShapeRunKey, (Vec<ShapedGlyph>, u64)>,
    shape_cache_hits: u64,
    /// Cache for the shaped-glyph raster path, keyed by (face, glyph id,
    /// style) rather than by `char` (`cache` above stays the char-keyed path
    /// for `get_or_raster`).
    raster_shaped_cache: HashMap<ShapedGlyphKey, Cached>,
    /// Live atlas allocations for LRU eviction, keyed by slot id. When an
    /// atlas is full and cannot grow, the least-recently-used slot of the
    /// same [`AtlasKind`] is freed and its owning cache entry dropped.
    slots: HashMap<u32, AtlasSlot>,
    next_slot_id: u32,
    /// Monotonic access clock; every cache read/insert stamps `last_used`.
    clock: u64,
    /// Monotonic counter bumped whenever an atlas slot is evicted. Renderer
    /// row caches hold concrete atlas coordinates, so eviction is a semantic
    /// invalidation even if the CPU atlas dimensions did not change.
    atlas_eviction_generation: u64,
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
            font_cfg,
            shape_cache: HashMap::new(),
            shape_cache_hits: 0,
            raster_shaped_cache: HashMap::new(),
            slots: HashMap::new(),
            next_slot_id: 0,
            clock: 0,
            atlas_eviction_generation: 0,
        })
    }

    /// Build a grid whose glyph atlases are pinned to a tiny, non-growing
    /// `dim` x `dim` size. This is public only so renderer tests in this
    /// workspace can force atlas eviction deterministically.
    #[doc(hidden)]
    pub fn new_with_capped_atlas_for_tests(
        px_size: f32,
        font_cfg: FontConfig,
        dim: u32,
    ) -> Result<Self, FontError> {
        let font_stack = load_font_stack(&font_cfg)?;
        let metrics = {
            let font = font_stack.primary().font_ref()?;
            Metrics::compute(font, px_size)
        };
        Ok(Self {
            font_stack,
            ctx: ScaleContext::new(),
            metrics,
            mask_atlas: Atlas::with_max_dim(dim, dim, MASK_BYTES_PER_PX, dim),
            color_atlas: Atlas::with_max_dim(dim, dim, COLOR_BYTES_PER_PX, dim),
            cache: HashMap::new(),
            px_size,
            font_cfg,
            shape_cache: HashMap::new(),
            shape_cache_hits: 0,
            raster_shaped_cache: HashMap::new(),
            slots: HashMap::new(),
            next_slot_id: 0,
            clock: 0,
            atlas_eviction_generation: 0,
        })
    }

    /// Look up a glyph, rasterizing and packing it into the appropriate atlas
    /// (mask or color) on a miss.
    ///
    /// If the atlas cannot grow enough to fit a glyph, the returned zero-sized
    /// rect is not cached so a future larger atlas can make the glyph visible.
    pub fn get_or_raster(&mut self, ch: char) -> GlyphInfo {
        let key = GlyphKey { ch };
        if let Some(cached) = self.cache.get(&key).copied() {
            self.touch(cached.slot);
            return cached.info;
        }

        // Box-drawing / block / Powerline codepoints are drawn by noa itself at
        // exact cell dimensions (so lines join across cells) rather than looked
        // up in a font — see the `boxdraw` module.
        if is_builtin_glyph(ch) {
            let glyph = builtin_rasterized(ch, &self.metrics);
            return self.store_and_cache(&glyph, SlotOwner::Char(key));
        }

        let (font_index, glyph_id) = self.resolve_glyph(ch);

        // Borrow only `font_stack` data so `self.ctx` stays mutably borrowable.
        // Bytes were validated in `new`, so parsing here cannot fail.
        let font_data = &self.font_stack.faces()[font_index];
        let font = FontRef::from_index(&font_data.bytes, font_data.index)
            .expect("font bytes validated at construction");
        let synthesis = GlyphSynthesis {
            embolden: false,
            shear: false,
            thicken: self.font_cfg.thicken,
            thicken_strength: self.font_cfg.thicken_strength,
        };
        let glyph =
            rasterize_with_variations(&mut self.ctx, font, glyph_id, self.px_size, &[], synthesis);

        self.store_and_cache(&glyph, SlotOwner::Char(key))
    }

    /// Resolve which face in the font stack a codepoint maps to (the first
    /// face whose cmap contains it). Used for render-side run segmentation
    /// (REQ-SHAPE-6) and internally by [`FontGrid::shape_run`] to pick which
    /// face to shape a run against — a run is guaranteed single-face by the
    /// caller (segmentation breaks at face boundaries).
    pub fn resolve_face(&self, ch: char) -> FaceId {
        // Builtin (procedurally-drawn) codepoints resolve to a sentinel face so
        // segmentation isolates them into their own runs — see
        // [`FaceId::BUILTIN`] and [`FontGrid::raster_shaped`]'s builtin branch.
        if is_builtin_glyph(ch) {
            return FaceId::BUILTIN;
        }
        FaceId(self.resolve_glyph(ch).0 as u16)
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

    /// The variation-axis coordinates for `style`, read from this grid's
    /// `FontConfig`. Shared by [`FontGrid::shape_run`] and
    /// [`FontGrid::raster_shaped`] (D1 invariant — see
    /// `docs/specs/rendering-improvements.md` WP2) so rustybuzz and swash
    /// can never independently derive/convert variation coords and drift
    /// apart.
    pub fn variation_coords(&self, style: StyleKey) -> Vec<(u32, f32)> {
        shape::variation_coords_for(&self.font_cfg, style)
    }

    /// Shape one already-segmented, single-face-resolvable run (segmentation
    /// happens render-side, before calling this — a run is guaranteed
    /// single-face by the caller). Internally memoized (REQ-SHAPE-5): an
    /// unchanged run on a later call is a cache hit and does not re-invoke
    /// `rustybuzz`.
    pub fn shape_run(&mut self, cells: &[ShapeCell]) -> Vec<ShapedGlyph> {
        let Some(first) = cells.first() else {
            return Vec::new();
        };
        let style = first.style;
        let key = shape::shape_run_key(cells, style, &self.font_cfg);
        let now = self.tick();
        if let Some(entry) = self.shape_cache.get_mut(&key) {
            entry.1 = now;
            self.shape_cache_hits += 1;
            return entry.0.clone();
        }

        // Builtin runs (box-drawing/block/Powerline) bypass rustybuzz entirely:
        // one glyph per cell, anchored 1:1, its codepoint carried in `glyph_id`
        // (all builtin codepoints fit in `u16`) for `raster_shaped` to draw.
        // Segmentation guarantees the whole run is builtin (all cells share
        // `FaceId::BUILTIN`).
        if self.resolve_face(first.ch) == FaceId::BUILTIN {
            let x_advance = self.metrics.cell_w.round() as i32;
            let glyphs: Vec<ShapedGlyph> = cells
                .iter()
                .enumerate()
                .map(|(idx, cell)| ShapedGlyph {
                    glyph_id: cell.ch as u16,
                    face_id: FaceId::BUILTIN,
                    x_advance,
                    x_offset: 0,
                    y_offset: 0,
                    cluster: idx as u32,
                })
                .collect();
            if self.shape_cache.len() >= SHAPE_CACHE_CAP {
                self.evict_lru_shape_run();
            }
            self.shape_cache.insert(key, (glyphs.clone(), now));
            return glyphs;
        }

        let face_id = self.resolve_face(first.ch);
        let variation_coords = self.variation_coords(style);
        let font_data = &self.font_stack.faces()[face_id.0 as usize];
        let glyphs = shape::shape_with_rustybuzz(
            font_data,
            face_id,
            self.px_size,
            cells,
            &variation_coords,
            &self.font_cfg,
        );

        if self.shape_cache.len() >= SHAPE_CACHE_CAP {
            self.evict_lru_shape_run();
        }
        self.shape_cache.insert(key, (glyphs.clone(), now));
        glyphs
    }

    /// Number of `shape_run` calls served from the shape cache (REQ-SHAPE-5
    /// / AC-WP2-05).
    pub fn shape_cache_hits(&self) -> u64 {
        self.shape_cache_hits
    }

    /// Raster by (face, glyph_id) — the shaped-glyph raster path, used
    /// instead of the char-keyed [`FontGrid::get_or_raster`] for any cell
    /// that went through [`FontGrid::shape_run`]. Applies the SAME
    /// variation coordinates `shape_run` used for shaping this style (D1,
    /// via [`FontGrid::variation_coords`]) and, when the resolved face lacks
    /// the requested native style, a synthetic-style transform gated by
    /// `FontConfig.synthetic_style` (REQ-SHAPE-7).
    pub fn raster_shaped(&mut self, face_id: FaceId, glyph_id: u16, style: StyleKey) -> GlyphInfo {
        let key = ShapedGlyphKey {
            face_id,
            glyph_id,
            style,
        };
        if let Some(cached) = self.raster_shaped_cache.get(&key).copied() {
            self.touch(cached.slot);
            return cached.info;
        }

        // Builtin sentinel face: `glyph_id` is a codepoint, not a font glyph —
        // synthesise the mask procedurally (see the `boxdraw` module) and pack
        // it into the R8 mask atlas like any other coverage glyph.
        if face_id == FaceId::BUILTIN {
            let ch =
                char::from_u32(glyph_id as u32).expect("builtin glyph_id is a valid codepoint");
            let glyph = builtin_rasterized(ch, &self.metrics);
            return self.store_and_cache(&glyph, SlotOwner::Shaped(key));
        }

        let font_data = &self.font_stack.faces()[face_id.0 as usize];
        let font = FontRef::from_index(&font_data.bytes, font_data.index)
            .expect("font bytes validated at construction");
        let variation_coords = shape::variation_coords_for(&self.font_cfg, style);
        let synthesis = synthesis_for(&self.font_cfg, style);
        let glyph = rasterize_with_variations(
            &mut self.ctx,
            font,
            glyph_id,
            self.px_size,
            &variation_coords,
            synthesis,
        );

        self.store_and_cache(&glyph, SlotOwner::Shaped(key))
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

    /// Monotonic generation bumped whenever a glyph atlas slot is evicted.
    pub fn atlas_eviction_generation(&self) -> u64 {
        self.atlas_eviction_generation
    }

    /// Advance the access clock and return the new stamp.
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Refresh a cached glyph's recency on a cache hit so a live glyph is not
    /// evicted out from under an on-screen cell.
    fn touch(&mut self, slot: Option<u32>) {
        let now = self.tick();
        if let Some(id) = slot
            && let Some(s) = self.slots.get_mut(&id)
        {
            s.last_used = now;
        }
    }

    fn atlas_mut(&mut self, kind: AtlasKind) -> &mut Atlas {
        match kind {
            AtlasKind::Mask => &mut self.mask_atlas,
            AtlasKind::Color => &mut self.color_atlas,
        }
    }

    /// Insert a freshly cached glyph into the map named by its owner.
    fn insert_cached(&mut self, owner: SlotOwner, cached: Cached) {
        match owner {
            SlotOwner::Char(key) => {
                self.cache.insert(key, cached);
            }
            SlotOwner::Shaped(key) => {
                self.raster_shaped_cache.insert(key, cached);
            }
        }
    }

    /// Free the least-recently-used slot of `kind`, returning its space to the
    /// atlas and dropping its owning cache entry. Returns `false` when no slot
    /// of that kind exists (nothing left to evict).
    fn evict_one(&mut self, kind: AtlasKind) -> bool {
        let victim = self
            .slots
            .iter()
            .filter(|(_, s)| s.kind == kind)
            .min_by_key(|(_, s)| s.last_used)
            .map(|(id, _)| *id);
        let Some(id) = victim else {
            return false;
        };
        let slot = self.slots.remove(&id).expect("victim id came from slots");
        self.atlas_mut(kind).deallocate(slot.alloc);
        match slot.owner {
            SlotOwner::Char(key) => {
                self.cache.remove(&key);
            }
            SlotOwner::Shaped(key) => {
                self.raster_shaped_cache.remove(&key);
            }
        }
        self.atlas_eviction_generation = self.atlas_eviction_generation.wrapping_add(1);
        true
    }

    /// Evict the least-recently-used memoized shape run (LRU cap enforcement).
    fn evict_lru_shape_run(&mut self) {
        if let Some(key) = self
            .shape_cache
            .iter()
            .min_by_key(|(_, (_, last_used))| *last_used)
            .map(|(key, _)| key.clone())
        {
            self.shape_cache.remove(&key);
        }
    }

    /// Pack a rasterized glyph into the appropriate atlas and cache the result
    /// under `owner`. On a full atlas that cannot grow, evicts least-recently-
    /// used glyphs of the same [`AtlasKind`] and retries; only if nothing can
    /// be evicted is the glyph dropped uncached (so a later frame can retry).
    fn store_and_cache(&mut self, glyph: &RasterizedGlyph, owner: SlotOwner) -> GlyphInfo {
        // Nothing to pack (space, control chars): cache the empty info so the
        // miss isn't repeated, but it holds no atlas slot.
        if glyph.width == 0 || glyph.height == 0 {
            let info = glyph_info(glyph, [0, 0], [0, 0]);
            self.insert_cached(owner, Cached { info, slot: None });
            return info;
        }

        let kind = if glyph.color {
            AtlasKind::Color
        } else {
            AtlasKind::Mask
        };
        let reservation = loop {
            if let Some(r) = self.atlas_mut(kind).reserve_and_blit_growing(
                glyph.width,
                glyph.height,
                &glyph.bitmap,
            ) {
                break Some(r);
            }
            if !self.evict_one(kind) {
                break None;
            }
        };

        match reservation {
            Some(r) => {
                let last_used = self.tick();
                let slot_id = self.next_slot_id;
                self.next_slot_id = self.next_slot_id.wrapping_add(1);
                self.slots.insert(
                    slot_id,
                    AtlasSlot {
                        kind,
                        alloc: r.alloc,
                        owner,
                        last_used,
                    },
                );
                let info = glyph_info(glyph, [r.x, r.y], [glyph.width as u16, glyph.height as u16]);
                self.insert_cached(
                    owner,
                    Cached {
                        info,
                        slot: Some(slot_id),
                    },
                );
                info
            }
            None => {
                log::warn!("glyph atlas full and nothing to evict; not caching glyph {owner:?}");
                glyph_info(glyph, [0, 0], [0, 0])
            }
        }
    }
}

#[cfg(test)]
impl FontGrid {
    /// Build a grid whose glyph atlases are pinned to a tiny, non-growing
    /// `dim`×`dim` so a handful of glyphs fills them — forcing the eviction
    /// path deterministically without a multi-megabyte allocation.
    fn new_with_capped_atlas(
        px_size: f32,
        font_cfg: FontConfig,
        dim: u32,
    ) -> Result<Self, FontError> {
        Self::new_with_capped_atlas_for_tests(px_size, font_cfg, dim)
    }

    fn slot_count(&self) -> usize {
        self.slots.len()
    }

    fn is_char_cached(&self, ch: char) -> bool {
        self.cache.contains_key(&GlyphKey { ch })
    }

    /// The slot registry and the cache maps must never drift: every slot's
    /// owner must still point back at it, and every cached glyph holding a
    /// slot id must reference a live slot.
    fn assert_slot_cache_consistent(&self) {
        for (id, slot) in &self.slots {
            match slot.owner {
                SlotOwner::Char(key) => assert_eq!(
                    self.cache.get(&key).and_then(|c| c.slot),
                    Some(*id),
                    "slot {id} owner char {key:?} does not point back at it"
                ),
                SlotOwner::Shaped(key) => assert_eq!(
                    self.raster_shaped_cache.get(&key).and_then(|c| c.slot),
                    Some(*id),
                    "slot {id} owner shaped {key:?} does not point back at it"
                ),
            }
        }
        for cached in self.cache.values() {
            if let Some(id) = cached.slot {
                assert!(
                    self.slots.contains_key(&id),
                    "cache references dead slot {id}"
                );
            }
        }
        for cached in self.raster_shaped_cache.values() {
            if let Some(id) = cached.slot {
                assert!(
                    self.slots.contains_key(&id),
                    "raster_shaped cache references dead slot {id}"
                );
            }
        }
    }
}

/// Decide the synthetic-style transform for `style` under `font_cfg`
/// (REQ-SHAPE-7): bold/italic synthesis is only attempted when the run
/// actually requests that style AND the config toggle for it is on. Pulled
/// out as a standalone function so the decision is unit-testable without
/// rasterizing a real glyph.
fn synthesis_for(font_cfg: &FontConfig, style: StyleKey) -> GlyphSynthesis {
    GlyphSynthesis {
        embolden: style.bold && font_cfg.synthetic_style.bold,
        shear: style.italic && font_cfg.synthetic_style.italic,
        thicken: font_cfg.thicken,
        thicken_strength: font_cfg.thicken_strength,
    }
}

/// Rasterize a built-in glyph (box-drawing/block/Powerline) into a
/// [`RasterizedGlyph`] positioned flush to the cell origin.
///
/// The mask fills the whole `ceil(cell_w) x ceil(cell_h)` cell box, so it is
/// placed with `bearing_x = 0` and `bearing_y = ascent`: the renderer's
/// `glyph_cell_bearing` maps that to a cell-top-left offset of `[0, 0]`, and
/// neighbouring cells' lines therefore stay collinear.
fn builtin_rasterized(ch: char, metrics: &Metrics) -> RasterizedGlyph {
    let g = boxdraw::draw_builtin(ch, metrics);
    RasterizedGlyph {
        bitmap: g.coverage,
        width: g.width,
        height: g.height,
        bearing_x: 0,
        bearing_y: metrics.ascent.round() as i32,
        advance: metrics.cell_w,
        color: false,
    }
}

/// Build a [`GlyphInfo`] from a rasterized glyph and its packed atlas
/// placement (`[0, 0]`/`[0, 0]` when there is nothing to draw).
fn glyph_info(glyph: &RasterizedGlyph, atlas_pos: [u16; 2], atlas_size: [u16; 2]) -> GlyphInfo {
    GlyphInfo {
        atlas_pos,
        atlas_size,
        bearing: [glyph.bearing_x as i16, glyph.bearing_y as i16],
        advance: glyph.advance,
        color: glyph.color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontFeature, FontVariation, SyntheticStyle};

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

    /// Atlas eviction: flooding a tiny non-growing atlas evicts
    /// least-recently-used glyphs (bounding the live slot set) while a
    /// repeatedly-touched "hot" glyph survives, and the slot registry stays
    /// consistent with the cache maps throughout.
    #[test]
    fn atlas_eviction_evicts_lru_and_keeps_hot_glyph_consistent() {
        let mut grid = match FontGrid::new_with_capped_atlas(14.0, FontConfig::default(), 40) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        let hot = 'A';
        let cold = 'B';
        assert!(grid.has_glyph(hot) && grid.has_glyph(cold));
        grid.get_or_raster(hot);
        grid.get_or_raster(cold);

        // Flood the tiny atlas, keeping `hot` most-recently-used each round so
        // it is never the eviction victim.
        let mut flooded = 0;
        for ch in 'C'..='~' {
            if !grid.has_glyph(ch) {
                continue;
            }
            grid.get_or_raster(hot);
            grid.get_or_raster(ch);
            flooded += 1;
        }
        assert!(
            flooded > 40,
            "test needs to raster more distinct glyphs than the tiny atlas holds; got {flooded}"
        );

        assert!(grid.slot_count() > 0, "at least one glyph must be packed");
        assert!(
            grid.slot_count() < 40,
            "eviction must bound the live slot set well below the {flooded} rastered glyphs, got {}",
            grid.slot_count()
        );
        assert!(
            grid.is_char_cached(hot),
            "the repeatedly-touched hot glyph must survive LRU eviction"
        );
        assert!(
            !grid.is_char_cached(cold),
            "an early, untouched glyph must have been evicted under atlas pressure"
        );
        assert!(
            grid.atlas_eviction_generation() > 0,
            "atlas eviction must advance the renderer-visible eviction generation"
        );
        grid.assert_slot_cache_consistent();
    }

    // ---- WP2: shaping + ligatures + shape cache -----------------------

    fn shape_cell(ch: char) -> ShapeCell {
        ShapeCell {
            ch,
            combining: Vec::new(),
            style: StyleKey::default(),
        }
    }

    macro_rules! skip_if_no_font {
        ($e:expr) => {
            match $e {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("skipping: no system monospace font available: {e}");
                    return;
                }
            }
        };
    }

    /// AC-WP2-01 [noa-font half]: a hand-built shaped output where a
    /// ligature's glyph is anchored at the cluster-start cell is exercised
    /// end-to-end via `shape_run` on `!=` with ligature features force-on;
    /// this environment may not have a ligature-capable font installed, so
    /// the strict "fewer glyphs than chars" assertion is best-effort (the
    /// renderer-level test in `noa-render` covers the "no duplicate glyph"
    /// consumer contract deterministically, without depending on any
    /// installed font's ligature table).
    #[test]
    fn ligature_cluster_maps_to_start_cell_when_font_supports_it() {
        let mut cfg = FontConfig::default();
        cfg.features.push(FontFeature {
            tag: *b"calt",
            enabled: true,
        });
        cfg.features.push(FontFeature {
            tag: *b"liga",
            enabled: true,
        });
        let mut grid = skip_if_no_font!(FontGrid::new(24.0, cfg));

        let cells = vec![shape_cell('!'), shape_cell('=')];
        let glyphs = grid.shape_run(&cells);

        if glyphs.len() < cells.len() {
            let start_glyphs: Vec<_> = glyphs.iter().filter(|g| g.cluster == 0).collect();
            let covered_glyphs: Vec<_> = glyphs.iter().filter(|g| g.cluster == 1).collect();
            assert_eq!(
                start_glyphs.len(),
                1,
                "the ligature must be exactly one glyph anchored at the cluster-start cell"
            );
            assert!(
                covered_glyphs.is_empty(),
                "the covered (non-start) cell must not get its own glyph (no duplicate draw)"
            );
        } else {
            eprintln!(
                "skipping strict ligature assertion: installed font has no \"!=\" calt/liga rule"
            );
        }
    }

    /// AC-WP2-02: `liga`/`calt`/`dlig` are OFF by default; `font-feature =
    /// calt` re-enables them. The feature-list mechanism itself is proven
    /// font-independently in `shape::tests`; here we exercise the same
    /// claim through the real `shape_run` entry point.
    #[test]
    fn ligatures_default_off_through_shape_run() {
        let mut grid = skip_if_no_font!(FontGrid::new(24.0, FontConfig::default()));
        let cells = vec![shape_cell('!'), shape_cell('=')];
        let glyphs = grid.shape_run(&cells);
        assert_eq!(
            glyphs.len(),
            cells.len(),
            "ligature features must default OFF (REQ-SHAPE-2): \"!=\" must shape as 2 glyphs"
        );
    }

    /// AC-WP2-03: `rustybuzz` and `swash` must receive identical variation
    /// coords for a given `font-variation` config so a shaped glyph's
    /// advance matches its rasterized glyph's advance (no drift). This
    /// environment's installed font may not be a variable font (coords are
    /// then a harmless no-op on both sides), but the assertion is still a
    /// real regression guard: a real drift bug (wrong px scale, or shaping
    /// against a different face than rasterizing) would show up as a
    /// mismatch here regardless of variable-font support.
    #[test]
    fn shaped_advance_and_rasterized_advance_do_not_drift() {
        let mut cfg = FontConfig::default();
        cfg.variations.push(FontVariation {
            tag: *b"wght",
            value: 700.0,
        });
        let mut grid = skip_if_no_font!(FontGrid::new(24.0, cfg));

        let style = StyleKey::default();
        let cells = vec![shape_cell('M')];
        let shaped = grid.shape_run(&cells);
        let glyph = shaped
            .first()
            .expect("shaping 'M' must yield at least one glyph");

        let raster = grid.raster_shaped(glyph.face_id, glyph.glyph_id, style);
        let raster_advance = raster.advance.round() as i32;

        assert!(
            (glyph.x_advance - raster_advance).abs() <= 1,
            "shaped x_advance ({}) and rasterized advance ({}) must not drift \
             (D1 identical-coords invariant)",
            glyph.x_advance,
            raster_advance
        );
    }

    /// AC-WP2-04: a combining mark + base in one cell shapes as an attached
    /// cluster — base and every combining-mark glyph share the source
    /// cell's cluster index (the renderer then positions each by its own
    /// shaped offset, never by an independent per-char pen bearing — see
    /// the `noa-render` glyph-emission test for that half).
    #[test]
    fn combining_mark_and_base_share_the_cells_cluster() {
        let mut grid = skip_if_no_font!(FontGrid::new(24.0, FontConfig::default()));
        let cells = vec![ShapeCell {
            ch: 'e',
            combining: vec!['\u{301}'], // COMBINING ACUTE ACCENT
            style: StyleKey::default(),
        }];

        let glyphs = grid.shape_run(&cells);
        assert!(
            !glyphs.is_empty(),
            "shaping a base+combining cell must yield at least the base glyph"
        );
        assert!(
            glyphs.iter().all(|g| g.cluster == 0),
            "base + every combining-mark glyph must share the source cell's cluster index: {glyphs:?}"
        );
    }

    /// AC-WP2-05: an unchanged run on a second call is a cache hit and does
    /// not re-invoke `rustybuzz`.
    #[test]
    fn shape_run_caches_unchanged_run_and_counts_hits() {
        let mut grid = skip_if_no_font!(FontGrid::new(24.0, FontConfig::default()));
        let cells = vec![shape_cell('a'), shape_cell('b')];

        assert_eq!(grid.shape_cache_hits(), 0);
        let first = grid.shape_run(&cells);
        assert_eq!(
            grid.shape_cache_hits(),
            0,
            "the first shape_run call must be a cache miss"
        );

        let second = grid.shape_run(&cells);
        assert_eq!(
            grid.shape_cache_hits(),
            1,
            "an unchanged run on the next call must be a cache hit"
        );
        assert_eq!(first, second);

        // A run with different text must NOT hit the cache.
        let different = vec![shape_cell('a'), shape_cell('c')];
        grid.shape_run(&different);
        assert_eq!(
            grid.shape_cache_hits(),
            1,
            "a run with different text must be a fresh cache miss"
        );
    }

    /// AC-WP2-07: `font-synthetic-style` gates faux-bold/faux-italic
    /// synthesis per style, including disabling it (`no-bold`).
    #[test]
    fn synthetic_style_decision_respects_config_toggle_per_style() {
        // Decoupled from thicken (its own global toggle): pin it off here so
        // the literals below stay focused on embolden/shear gating.
        let mut cfg = FontConfig {
            synthetic_style: SyntheticStyle {
                bold: true,
                italic: false,
            },
            thicken: false,
            thicken_strength: 0,
            ..Default::default()
        };

        assert_eq!(
            synthesis_for(
                &cfg,
                StyleKey {
                    bold: true,
                    italic: false
                }
            ),
            GlyphSynthesis {
                embolden: true,
                shear: false,
                thicken: false,
                thicken_strength: 0,
            }
        );
        assert_eq!(
            synthesis_for(
                &cfg,
                StyleKey {
                    bold: false,
                    italic: true
                }
            ),
            GlyphSynthesis {
                embolden: false,
                shear: false,
                thicken: false,
                thicken_strength: 0,
            },
            "italic synthesis must stay off when synthetic_style.italic is false"
        );
        assert_eq!(
            synthesis_for(
                &cfg,
                StyleKey {
                    bold: false,
                    italic: false
                }
            ),
            GlyphSynthesis::default(),
            "a plain (non-bold, non-italic) style must never synthesize anything"
        );

        cfg.synthetic_style = SyntheticStyle {
            bold: false,
            italic: false,
        }; // `no-bold`
        assert_eq!(
            synthesis_for(
                &cfg,
                StyleKey {
                    bold: true,
                    italic: false
                }
            ),
            GlyphSynthesis {
                embolden: false,
                shear: false,
                thicken: false,
                thicken_strength: 0,
            },
            "font-synthetic-style = no-bold must disable bold synthesis even for a bold style"
        );
    }

    /// AC-WP2-06 support: `resolve_face` distinguishes a Latin-only face
    /// from whatever face a CJK codepoint resolves to, when a CJK-capable
    /// fallback is installed (the full row-segmentation assertion lives in
    /// `noa-render`, which calls this same method).
    #[test]
    fn resolve_face_distinguishes_latin_and_cjk_when_fallback_available() {
        let grid = skip_if_no_font!(FontGrid::new(24.0, FontConfig::default()));
        if !grid.has_glyph('日') {
            eprintln!("skipping: no installed font can render Japanese");
            return;
        }
        let latin_face = grid.resolve_face('A');
        let cjk_face = grid.resolve_face('日');
        assert_ne!(
            latin_face, cjk_face,
            "a CJK codepoint should resolve to a different face than plain Latin ASCII \
             when a CJK fallback face is installed"
        );
    }
}
