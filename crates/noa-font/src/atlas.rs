//! A single-channel (R8) CPU glyph atlas backed by an etagere allocator.

use etagere::{BucketedAtlasAllocator, size2};

const MAX_ATLAS_DIM: u32 = 8192;

/// R8 glyph atlas: a shelf allocator plus a parallel CPU coverage buffer.
///
/// The renderer uploads [`Atlas::data`] to a GPU texture whenever
/// [`Atlas::generation`] advances beyond the renderer's last seen value.
pub struct Atlas {
    allocator: BucketedAtlasAllocator,
    data: Vec<u8>,
    width: u32,
    height: u32,
    generation: u64,
}

impl Atlas {
    /// Create a `width` x `height` R8 atlas (fully transparent).
    pub fn new(width: u32, height: u32) -> Self {
        let allocator = BucketedAtlasAllocator::new(size2(width as i32, height as i32));
        Self {
            allocator,
            data: vec![0u8; (width as usize) * (height as usize)],
            width,
            height,
            generation: 0,
        }
    }

    /// Reserve a `w` x `h` region and blit `bitmap` (R8, `w*h` bytes) into it.
    ///
    /// Returns the top-left `(x, y)` of the packed region on success, or
    /// `None` if the atlas is full. A zero-sized request returns `(0, 0)`
    /// without touching the buffer.
    pub fn reserve_and_blit(&mut self, w: u32, h: u32, bitmap: &[u8]) -> Option<(u16, u16)> {
        if w == 0 || h == 0 {
            return Some((0, 0));
        }
        // Pad by 1px on each side to avoid bilinear bleed between neighbours.
        let alloc = self.allocator.allocate(size2(w as i32 + 1, h as i32 + 1))?;
        let min = alloc.rectangle.min;
        let x = min.x as u32;
        let y = min.y as u32;

        for row in 0..h {
            let src = (row as usize) * (w as usize);
            let dst = ((y + row) as usize) * (self.width as usize) + x as usize;
            self.data[dst..dst + w as usize].copy_from_slice(&bitmap[src..src + w as usize]);
        }
        self.bump_generation();
        Some((x as u16, y as u16))
    }

    /// Reserve and blit, growing the atlas until the glyph fits or the cap is reached.
    pub fn reserve_and_blit_growing(
        &mut self,
        w: u32,
        h: u32,
        bitmap: &[u8],
    ) -> Option<(u16, u16)> {
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
        let required_width = w.saturating_add(1).max(self.width);
        let required_height = h.saturating_add(1).max(self.height);
        let next_width = next_grown_dim(self.width, required_width);
        let next_height = next_grown_dim(self.height, required_height);

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

        let mut data = vec![0u8; (width as usize) * (height as usize)];
        for row in 0..self.height as usize {
            let old_start = row * self.width as usize;
            let new_start = row * width as usize;
            data[new_start..new_start + self.width as usize]
                .copy_from_slice(&self.data[old_start..old_start + self.width as usize]);
        }

        self.data = data;
        self.width = width;
        self.height = height;
        self.bump_generation();
    }

    /// Borrow the R8 pixel buffer (`width * height` bytes, row-major).
    pub fn data(&self) -> &[u8] {
        &self.data
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

fn next_grown_dim(current: u32, required: u32) -> u32 {
    current.saturating_mul(2).max(required).min(MAX_ATLAS_DIM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_marks_dirty_and_writes() {
        let mut atlas = Atlas::new(64, 64);
        assert_eq!(atlas.generation(), 0);
        let bitmap = vec![0xFFu8; 4 * 3];
        let (x, y) = atlas.reserve_and_blit(4, 3, &bitmap).unwrap();
        assert_eq!(atlas.generation(), 1);
        let idx = (y as usize) * 64 + x as usize;
        assert_eq!(atlas.data()[idx], 0xFF);
        assert_eq!(atlas.size(), (64, 64));
    }

    #[test]
    fn zero_size_is_noop() {
        let mut atlas = Atlas::new(16, 16);
        assert_eq!(atlas.reserve_and_blit(0, 0, &[]), Some((0, 0)));
        assert_eq!(atlas.generation(), 0);
    }

    #[test]
    fn growing_reserve_preserves_existing_pixels() {
        let mut atlas = Atlas::new(8, 8);
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

        let first_idx = first.1 as usize * width as usize + first.0 as usize;
        let large_idx = large.1 as usize * width as usize + large.0 as usize;
        assert_eq!(atlas.data()[first_idx], 0x7F);
        assert_eq!(atlas.data()[large_idx], 0xFF);
    }
}
