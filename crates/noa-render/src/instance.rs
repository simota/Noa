//! GPU-facing `#[repr(C)]` types uploaded straight into vertex buffers.

/// One instanced quad's worth of per-cell data (mirrors Ghostty's 32-byte
/// `CellText`). Used for both the background pass and the glyph pass — the
/// `flags` bit0 selects which the shader is drawing.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub struct CellInstance {
    /// Atlas texel top-left `[x, y]` (glyph passes only).
    pub glyph_pos: [u16; 2],
    /// Atlas texel size `[w, h]` (glyph passes only; `[0,0]` = nothing to sample).
    pub glyph_size: [u16; 2],
    /// Glyph pen bearing `[left, top]` within the cell, in pixels (y-down from
    /// the cell's top edge).
    pub bearing: [i16; 2],
    /// Cell coordinate `[col, row]`.
    pub grid_pos: [u16; 2],
    /// Straight RGBA color, `0..=255` per channel: fg for glyph quads, bg for
    /// background quads.
    pub color: [u8; 4],
    /// Bit flags: bit0 = this instance samples the atlas (text quad, not a
    /// flat background rectangle); bit1 = min-contrast (unused inc-1); bit2 =
    /// cursor cell.
    pub flags: u32,
}

impl CellInstance {
    pub const FLAG_GLYPH: u32 = 1 << 0;
    pub const FLAG_MIN_CONTRAST: u32 = 1 << 1;
    pub const FLAG_CURSOR: u32 = 1 << 2;
}

/// Per-frame uniform data shared by every pipeline (mirrors Ghostty's
/// `Uniforms`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub struct Uniforms {
    pub projection: [[f32; 4]; 4],
    pub screen_size: [f32; 2],
    pub cell_size: [f32; 2],
    pub grid_size: [f32; 2],
    /// top, right, bottom, left.
    pub grid_padding: [f32; 4],
    pub cursor_pos: [f32; 2],
    pub cursor_color: [f32; 4],
    pub bg_color: [f32; 4],
    pub min_contrast: f32,
    pub _pad: [f32; 3],
}

/// A standard orthographic projection mapping pixel space (origin top-left,
/// y-down) to wgpu clip space (origin center, y-up, z untouched).
pub fn orthographic_projection(width: f32, height: f32) -> [[f32; 4]; 4] {
    // NDC x: [0,width] -> [-1,1]; NDC y: [0,height] -> [1,-1] (flip for y-down pixel space).
    let sx = 2.0 / width;
    let sy = -2.0 / height;
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0, 1.0],
    ]
}
