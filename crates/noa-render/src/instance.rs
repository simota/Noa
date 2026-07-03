//! GPU-facing `#[repr(C)]` types uploaded straight into vertex buffers.

use noa_core::{CellSize, GridPadding, PixelSize};

use crate::draw_plan::PaneRect;

/// One instanced quad's worth of per-cell data (mirrors Ghostty's 32-byte
/// `CellText`). Used for both the background pass and the glyph pass — the
/// `flags` bit0 selects which the shader is drawing.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug, PartialEq)]
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
    /// cursor cell; bit3 = decoration rectangle; bit4 = pixel-space overlay;
    /// bit5 = color glyph (samples the RGBA8 color atlas as passthrough
    /// instead of the R8 mask atlas — WP1, REQ-EMOJI-2).
    pub flags: u32,
}

impl CellInstance {
    pub const FLAG_GLYPH: u32 = 1 << 0;
    pub const FLAG_MIN_CONTRAST: u32 = 1 << 1;
    pub const FLAG_CURSOR: u32 = 1 << 2;
    pub const FLAG_DECORATION: u32 = 1 << 3;
    pub const FLAG_DIVIDER: u32 = 1 << 4;
    pub const FLAG_COLOR_GLYPH: u32 = 1 << 5;
}

/// Per-frame uniform data shared by every pipeline (mirrors Ghostty's
/// `Uniforms`).
///
/// Field order is deliberate: the 16-byte-aligned members (mat4 + vec4s) come
/// first, then the 8-byte vec2s, then scalars + tail padding to a 16-byte
/// multiple. Laid out this way, the tight `#[repr(C)]` Rust layout (160 bytes,
/// no interior padding) matches the WGSL std140 uniform layout in
/// `shaders/cell.wgsl` **byte-for-byte** — a mismatch here binds a buffer the
/// shader reads at the wrong offsets / rejects for size (wgpu validation error
/// at draw time). Keep the two struct definitions in lockstep.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub struct Uniforms {
    projection: [[f32; 4]; 4],
    /// top, right, bottom, left.
    grid_padding: [f32; 4],
    cursor_color: [f32; 4],
    bg_color: [f32; 4],
    screen_size: [f32; 2],
    cell_size: [f32; 2],
    grid_size: [f32; 2],
    cursor_pos: [f32; 2],
    min_contrast: f32,
    _pad: [f32; 3],
}

/// Inputs for the single per-pane uniform population path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneUniformParams {
    pub pane_rect: PaneRect,
    pub window_size: PixelSize,
    pub grid_padding: GridPadding,
    pub cell_size: CellSize,
    pub bg_color: [f32; 4],
}

/// Populate uniform data for one pane.
///
/// `pane_rect` contributes the pane-local origin to the shader's
/// top/left padding while the projection remains window-wide. That lets one
/// render pass draw several scissored pane regions without baking pane origin
/// into each cell instance.
pub fn populate_pane_uniform(params: PaneUniformParams) -> Uniforms {
    let window_w = params.window_size.w as f32;
    let window_h = params.window_size.h as f32;
    let pane_x = params.pane_rect.x as f32;
    let pane_y = params.pane_rect.y as f32;
    let pane_w = params.pane_rect.w as f32;
    let pane_h = params.pane_rect.h as f32;
    let cell_w = params.cell_size.w;
    let cell_h = params.cell_size.h;
    let content_width = (pane_w - params.grid_padding.horizontal()).max(0.0);
    let content_height = (pane_h - params.grid_padding.vertical()).max(0.0);

    Uniforms {
        projection: orthographic_projection(window_w.max(1.0), window_h.max(1.0)),
        grid_padding: [
            pane_y + params.grid_padding.top,
            (window_w - (pane_x + pane_w)).max(0.0) + params.grid_padding.right,
            (window_h - (pane_y + pane_h)).max(0.0) + params.grid_padding.bottom,
            pane_x + params.grid_padding.left,
        ],
        cursor_color: [1.0, 1.0, 1.0, 1.0],
        bg_color: params.bg_color,
        screen_size: [window_w, window_h],
        cell_size: [cell_w, cell_h],
        grid_size: [
            if cell_w > 0.0 {
                content_width / cell_w
            } else {
                0.0
            },
            if cell_h > 0.0 {
                content_height / cell_h
            } else {
                0.0
            },
        ],
        cursor_pos: [0.0, 0.0],
        min_contrast: 0.0,
        _pad: [0.0; 3],
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn populated_uniforms_include_each_pane_origin_and_grid_extent() {
        let window_size = PixelSize { w: 200, h: 100 };
        let grid_padding = GridPadding::new(2.0, 4.0, 6.0, 8.0);
        let cell_size = CellSize { w: 10.0, h: 4.0 };
        let bg_color = [0.1, 0.2, 0.3, 1.0];

        let left = populate_pane_uniform(PaneUniformParams {
            pane_rect: PaneRect::new(0, 0, 90, 100),
            window_size,
            grid_padding,
            cell_size,
            bg_color,
        });
        let right = populate_pane_uniform(PaneUniformParams {
            pane_rect: PaneRect::new(91, 0, 109, 100),
            window_size,
            grid_padding,
            cell_size,
            bg_color,
        });

        assert_eq!(left.screen_size, [200.0, 100.0]);
        assert_eq!(right.screen_size, [200.0, 100.0]);
        assert_eq!(left.grid_padding, [2.0, 114.0, 6.0, 8.0]);
        assert_eq!(right.grid_padding, [2.0, 4.0, 6.0, 99.0]);
        assert_eq!(left.grid_size, [7.8, 23.0]);
        assert_eq!(right.grid_size, [9.7, 23.0]);
        assert_eq!(left.bg_color, bg_color);
        assert_eq!(right.bg_color, bg_color);
    }
}
