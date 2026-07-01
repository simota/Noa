//! A single-channel (R8) CPU glyph atlas backed by an etagere allocator.

use etagere::{AtlasAllocator, size2};

/// R8 glyph atlas: a shelf allocator plus a parallel CPU coverage buffer.
///
/// The renderer uploads [`Atlas::data`] to a GPU texture whenever
/// [`Atlas::take_dirty`] reports pending changes.
pub struct Atlas {
    allocator: AtlasAllocator,
    data: Vec<u8>,
    width: u32,
    height: u32,
    dirty: bool,
}

impl Atlas {
    /// Create a `width` x `height` R8 atlas (fully transparent).
    pub fn new(width: u32, height: u32) -> Self {
        let allocator = AtlasAllocator::new(size2(width as i32, height as i32));
        Self {
            allocator,
            data: vec![0u8; (width as usize) * (height as usize)],
            width,
            height,
            dirty: false,
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
        let alloc = self
            .allocator
            .allocate(size2(w as i32 + 1, h as i32 + 1))?;
        let min = alloc.rectangle.min;
        let x = min.x as u32;
        let y = min.y as u32;

        for row in 0..h {
            let src = (row as usize) * (w as usize);
            let dst = ((y + row) as usize) * (self.width as usize) + x as usize;
            self.data[dst..dst + w as usize].copy_from_slice(&bitmap[src..src + w as usize]);
        }
        self.dirty = true;
        Some((x as u16, y as u16))
    }

    /// Borrow the R8 pixel buffer (`width * height` bytes, row-major).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Atlas dimensions `(width, height)` in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Return the dirty flag and clear it.
    pub fn take_dirty(&mut self) -> bool {
        let d = self.dirty;
        self.dirty = false;
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_marks_dirty_and_writes() {
        let mut atlas = Atlas::new(64, 64);
        assert!(!atlas.take_dirty());
        let bitmap = vec![0xFFu8; 4 * 3];
        let (x, y) = atlas.reserve_and_blit(4, 3, &bitmap).unwrap();
        assert!(atlas.take_dirty());
        assert!(!atlas.take_dirty());
        let idx = (y as usize) * 64 + x as usize;
        assert_eq!(atlas.data()[idx], 0xFF);
        assert_eq!(atlas.size(), (64, 64));
    }

    #[test]
    fn zero_size_is_noop() {
        let mut atlas = Atlas::new(16, 16);
        assert_eq!(atlas.reserve_and_blit(0, 0, &[]), Some((0, 0)));
        assert!(!atlas.take_dirty());
    }
}
