//! A channel-generic CPU glyph atlas backed by an etagere allocator.
//!
//! `FontGrid` owns two independent [`Atlas`] instances: an R8 (`bytes_per_px
//! = 1`) mask atlas for regular glyph coverage, and an RGBA8 (`bytes_per_px =
//! 4`) color atlas for color-bitmap glyphs (e.g. emoji). Byte offsets below
//! are all scaled by `bytes_per_px` so the same packing/growth logic serves
//! both without duplicating it (WP1, REQ-EMOJI-2/3).

use etagere::{size2, AllocId, BucketedAtlasAllocator};

const MAX_ATLAS_DIM: u32 = 8192;

/// A packed region returned by [`Atlas::reserve_and_blit`]. `alloc` is the
/// etagere handle needed to [`Atlas::deallocate`] the region later (glyph
/// eviction).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reservation {
    pub x: u16,
    pub y: u16,
    pub alloc: AllocId,
}

/// Channel-generic glyph atlas: a shelf allocator plus a parallel CPU pixel
/// buffer (`bytes_per_px` bytes per pixel — 1 for R8, 4 for RGBA8).
///
/// The renderer uploads [`Atlas::data`] to a GPU texture whenever
/// [`Atlas::generation`] advances beyond the renderer's last seen value.
pub struct Atlas {
    allocator: BucketedAtlasAllocator,
    data: Vec<u8>,
    width: u32,
    height: u32,
    generation: u64,
    bytes_per_px: u32,
    /// Largest dimension this atlas may grow to before allocation fails and
    /// the owner must evict. Production default is [`MAX_ATLAS_DIM`]; tests
    /// pin it small to force the full/evict path deterministically.
    max_dim: u32,
}

impl Atlas {
    /// Create a `width` x `height` atlas (fully transparent/zeroed),
    /// `bytes_per_px` bytes per pixel (1 = R8, 4 = RGBA8).
    pub fn new(width: u32, height: u32, bytes_per_px: u32) -> Self {
        Self::with_max_dim(width, height, bytes_per_px, MAX_ATLAS_DIM)
    }

    /// Like [`Atlas::new`] but with an explicit growth cap. Kept generic (not
    /// `#[cfg(test)]`) so the cap is an explicit per-atlas property rather
    /// than a hidden global; tests use it to force eviction without a 64 MiB
    /// allocation.
    pub fn with_max_dim(width: u32, height: u32, bytes_per_px: u32, max_dim: u32) -> Self {
        let allocator = BucketedAtlasAllocator::new(size2(width as i32, height as i32));
        Self {
            allocator,
            data: vec![0u8; (width as usize) * (height as usize) * bytes_per_px as usize],
            width,
            height,
            generation: 0,
            bytes_per_px,
            max_dim: max_dim.max(width).max(height),
        }
    }

    /// Reserve a `w` x `h` inner region and blit `bitmap`
    /// (`w*h*bytes_per_px` bytes) into it. The returned reservation points at
    /// that inner region; the allocation itself keeps a 1px zero pad on every
    /// side so filtered samples never see neighbouring or stale glyph pixels.
    /// Callers must pass a non-empty glyph (`w > 0 && h > 0`); zero-sized
    /// glyphs are filtered upstream in `store_rasterized`.
    ///
    /// Returns the packed [`Reservation`] on success, or `None` if the atlas
    /// is full.
    pub fn reserve_and_blit(&mut self, w: u32, h: u32, bitmap: &[u8]) -> Option<Reservation> {
        debug_assert!(
            w > 0 && h > 0,
            "reserve_and_blit requires a non-empty glyph"
        );
        let alloc = self.allocator.allocate(size2(w as i32 + 2, h as i32 + 2))?;
        let min = alloc.rectangle.min;
        let alloc_x = min.x as u32;
        let alloc_y = min.y as u32;
        let x = alloc_x + 1;
        let y = alloc_y + 1;
        let bpp = self.bytes_per_px as usize;
        let row_bytes = w as usize * bpp;
        let padded_row_bytes = (w as usize + 2) * bpp;

        // Zero the full padded region first. Reused allocations can contain
        // old ink; clearing all four sides prevents linear sampling from
        // bleeding stale pixels into the live inner glyph rect.
        for row in 0..(h + 2) {
            let dst = (((alloc_y + row) as usize) * (self.width as usize) + alloc_x as usize) * bpp;
            self.data[dst..dst + padded_row_bytes].fill(0);
        }

        for row in 0..h {
            let src = (row as usize) * row_bytes;
            let dst = (((y + row) as usize) * (self.width as usize) + x as usize) * bpp;
            self.data[dst..dst + row_bytes].copy_from_slice(&bitmap[src..src + row_bytes]);
        }
        self.bump_generation();
        Some(Reservation {
            x: x as u16,
            y: y as u16,
            alloc: alloc.id,
        })
    }

    /// Free a region previously handed out by [`Atlas::reserve_and_blit`],
    /// returning its space to the allocator for reuse (glyph eviction).
    ///
    /// etagere reclaims at bucket granularity — a shelf bucket's space becomes
    /// allocatable again only once *all* its items are freed — so freeing a
    /// single region may not immediately admit a same-size reserve. Callers
    /// that need space (`FontGrid::store_and_cache`) evict in a loop until a
    /// reserve succeeds. The stale pixels are left in place: they are never
    /// sampled again once the owner drops the glyph's `GlyphInfo`, and the
    /// next blit into a reused region overwrites them.
    pub fn deallocate(&mut self, id: AllocId) {
        self.allocator.deallocate(id);
    }

    /// Reserve and blit, growing the atlas until the glyph fits or the cap is reached.
    pub fn reserve_and_blit_growing(
        &mut self,
        w: u32,
        h: u32,
        bitmap: &[u8],
    ) -> Option<Reservation> {
        if let Some(pos) = self.reserve_and_blit(w, h, bitmap) {
            return Some(pos);
        }

        loop {
            if !self.grow_for(w, h) {
                return None;
            }
            if let Some(pos) = self.reserve_and_blit(w, h, bitmap) {
                return Some(pos);
            }
        }
    }

    fn grow_for(&mut self, w: u32, h: u32) -> bool {
        let required_width = w.saturating_add(2).max(self.width);
        let required_height = h.saturating_add(2).max(self.height);
        let next_width = next_grown_dim(self.width, required_width, self.max_dim);
        let next_height = next_grown_dim(self.height, required_height, self.max_dim);

        if next_width == self.width && next_height == self.height {
            return false;
        }

        self.grow(next_width, next_height);
        true
    }

    fn grow(&mut self, width: u32, height: u32) {
        debug_assert!(width >= self.width);
        debug_assert!(height >= self.height);

        self.allocator.grow(size2(width as i32, height as i32));

        let bpp = self.bytes_per_px as usize;
        let mut data = vec![0u8; (width as usize) * (height as usize) * bpp];
        for row in 0..self.height as usize {
            let old_start = row * self.width as usize * bpp;
            let new_start = row * width as usize * bpp;
            let row_bytes = self.width as usize * bpp;
            data[new_start..new_start + row_bytes]
                .copy_from_slice(&self.data[old_start..old_start + row_bytes]);
        }

        self.data = data;
        self.width = width;
        self.height = height;
        self.bump_generation();
    }

    /// Borrow the pixel buffer (`width * height * bytes_per_px` bytes, row-major).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Bytes per pixel (1 = R8, 4 = RGBA8).
    pub fn bytes_per_px(&self) -> u32 {
        self.bytes_per_px
    }

    /// Atlas dimensions `(width, height)` in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Monotonic atlas mutation generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

fn next_grown_dim(current: u32, required: u32, max_dim: u32) -> u32 {
    current.saturating_mul(2).max(required).min(max_dim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_marks_dirty_and_writes() {
        let mut atlas = Atlas::new(64, 64, 1);
        assert_eq!(atlas.generation(), 0);
        let bitmap = vec![0xFFu8; 4 * 3];
        let r = atlas.reserve_and_blit(4, 3, &bitmap).unwrap();
        assert_eq!(atlas.generation(), 1);
        let idx = (r.y as usize) * 64 + r.x as usize;
        assert_eq!(atlas.data()[idx], 0xFF);
        assert_eq!(atlas.size(), (64, 64));
    }

    #[test]
    fn blit_returns_inner_rect_and_keeps_all_four_padding_edges_clear() {
        let mut atlas = Atlas::new(16, 16, 1);
        let bitmap = vec![0x80u8; 3 * 2];
        let r = atlas.reserve_and_blit(3, 2, &bitmap).unwrap();

        assert!(r.x > 0 && r.y > 0, "reservation must point past the pad");
        let width = atlas.size().0 as usize;
        let x = r.x as usize;
        let y = r.y as usize;
        assert_eq!(atlas.data()[y * width + x], 0x80, "inner glyph pixel");
        assert_eq!(atlas.data()[y * width + x - 1], 0, "left pad");
        assert_eq!(atlas.data()[(y - 1) * width + x], 0, "top pad");
        assert_eq!(atlas.data()[y * width + x + 3], 0, "right pad");
        assert_eq!(atlas.data()[(y + 2) * width + x], 0, "bottom pad");
    }

    #[test]
    fn deallocate_reclaims_space_when_full() {
        // A tiny non-growing atlas: fill it, confirm it is full, then free the
        // packed regions and confirm the space is handed back out. etagere
        // reclaims at bucket granularity (a bucket's space returns only once
        // all its items are freed), so we free every allocation — mirroring
        // how `FontGrid::store_and_cache` evicts in a loop until a reserve
        // succeeds.
        let mut atlas = Atlas::with_max_dim(16, 16, 1, 16);
        let bitmap = vec![0xFFu8; 6 * 6];

        let mut allocs = Vec::new();
        while let Some(r) = atlas.reserve_and_blit(6, 6, &bitmap) {
            allocs.push(r.alloc);
        }
        assert!(
            allocs.len() >= 2,
            "atlas should pack at least two 6x6 glyphs"
        );
        assert!(
            atlas.reserve_and_blit_growing(6, 6, &bitmap).is_none(),
            "a capped, full atlas must not grow or allocate further"
        );

        for id in allocs {
            atlas.deallocate(id);
        }
        assert!(
            atlas.reserve_and_blit(6, 6, &bitmap).is_some(),
            "freed regions must be reusable by later allocations"
        );
    }

    #[test]
    fn growing_reserve_preserves_existing_pixels() {
        let mut atlas = Atlas::new(8, 8, 1);
        let first_bitmap = vec![0x7Fu8; 4 * 4];
        let first = atlas.reserve_and_blit(4, 4, &first_bitmap).unwrap();
        let first_generation = atlas.generation();

        let large_bitmap = vec![0xFFu8; 8 * 8];
        let large = atlas
            .reserve_and_blit_growing(8, 8, &large_bitmap)
            .expect("atlas should grow to fit the second glyph");

        let (width, height) = atlas.size();
        assert!(width > 8 && height > 8);
        assert!(
            atlas.generation() > first_generation,
            "growth and blit should advance the atlas generation"
        );

        let first_idx = first.y as usize * width as usize + first.x as usize;
        let large_idx = large.y as usize * width as usize + large.x as usize;
        assert_eq!(atlas.data()[first_idx], 0x7F);
        assert_eq!(atlas.data()[large_idx], 0xFF);
    }

    #[test]
    fn rgba_atlas_blits_and_grows_four_bytes_per_pixel() {
        let mut atlas = Atlas::new(8, 8, 4);
        assert_eq!(atlas.bytes_per_px(), 4);
        assert_eq!(atlas.data().len(), 8 * 8 * 4);

        let first_bitmap: Vec<u8> = [10u8, 20, 30, 255].repeat(4 * 4);
        let first = atlas.reserve_and_blit(4, 4, &first_bitmap).unwrap();
        let (width, _height) = atlas.size();
        let first_idx = (first.y as usize * width as usize + first.x as usize) * 4;
        assert_eq!(&atlas.data()[first_idx..first_idx + 4], &[10, 20, 30, 255]);

        let large_bitmap: Vec<u8> = [1u8, 2, 3, 4].repeat(8 * 8);
        let large = atlas
            .reserve_and_blit_growing(8, 8, &large_bitmap)
            .expect("atlas should grow to fit the second RGBA glyph");
        let (grown_width, grown_height) = atlas.size();
        assert!(grown_width > 8 && grown_height > 8);
        assert_eq!(
            atlas.data().len(),
            grown_width as usize * grown_height as usize * 4
        );

        // Original RGBA pixel must survive the grow's row-by-row copy.
        let first_idx_after_grow = (first.y as usize * grown_width as usize + first.x as usize) * 4;
        assert_eq!(
            &atlas.data()[first_idx_after_grow..first_idx_after_grow + 4],
            &[10, 20, 30, 255]
        );
        let large_idx = (large.y as usize * grown_width as usize + large.x as usize) * 4;
        assert_eq!(&atlas.data()[large_idx..large_idx + 4], &[1, 2, 3, 4]);
    }
}
