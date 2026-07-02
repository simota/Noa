//! [`Renderer`] — owns the GPU pipeline, the font atlas texture, and the
//! instance buffers; rebuilds them from a [`crate::FrameSnapshot`] and draws.

use noa_core::{CellAttrs, Color, PixelSize};
use noa_font::{FontGrid, Metrics};

use crate::instance::{CellInstance, Uniforms, orthographic_projection};
use crate::pipeline::CellPipeline;
use crate::snapshot::FrameSnapshot;
use crate::theme::Theme;

/// The wgpu instanced-cell renderer. Windowing-agnostic: it receives an
/// already-created `Device`/`Queue`/surface format and never touches
/// `winit` or `wgpu::Surface`.
pub struct Renderer {
    cell: CellPipeline,
    atlas_texture: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<CellInstance>,
    viewport: PixelSize,
    cell_size: (f32, f32),
    clear_color: [f32; 4],
}

impl Renderer {
    /// Build the renderer, uploading the font atlas as it currently stands.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font: &mut FontGrid,
    ) -> anyhow::Result<Renderer> {
        let cell = CellPipeline::new(device, format);

        let (atlas_w, atlas_h) = font.atlas_size();
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-glyph-atlas"),
            size: wgpu::Extent3d {
                width: atlas_w,
                height: atlas_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());

        upload_atlas(queue, &atlas_texture, font.atlas_data(), atlas_w, atlas_h);
        font.take_atlas_dirty(); // we just uploaded the current contents

        let bind_group = cell.make_bind_group(device, &atlas_view);

        let instance_capacity = 4096;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("noa-instance-buffer"),
            size: (instance_capacity * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let metrics = font.metrics();

        Ok(Renderer {
            cell,
            atlas_texture,
            atlas_view,
            bind_group,
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
            viewport: PixelSize { w: 0, h: 0 },
            cell_size: (metrics.cell_w, metrics.cell_h),
            clear_color: [0.0, 0.0, 0.0, 1.0],
        })
    }

    /// Update the known viewport size (called on `WindowEvent::Resized`).
    pub fn resize(&mut self, px: PixelSize) {
        self.viewport = px;
    }

    /// Rebuild the CPU instance list from a snapshot, re-rastering any glyphs
    /// not yet in the atlas and re-uploading the atlas texture if it grew.
    pub fn rebuild_cells(&mut self, snap: &FrameSnapshot, font: &mut FontGrid, theme: &Theme) {
        let (clear_color, cell_size) =
            rebuild_cell_instances(&mut self.instances, snap, font, theme);
        self.clear_color = clear_color;
        self.cell_size = cell_size;
    }

    /// Draw the current instance list into `view`, uploading updated GPU
    /// state (uniforms, atlas, instance buffer) first.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
        // Re-upload the atlas if the caller rastered new glyphs into it
        // since our last upload. We don't own `FontGrid` here, so the atlas
        // resize/upload happens in `rebuild_cells`'s caller via `sync_atlas`;
        // for inc-1 the atlas is fixed-size and allocated once in `new`, so a
        // dirty check happens each `rebuild_cells` call through `sync_atlas`.

        self.upload_instances(device, queue);
        self.upload_uniforms(queue);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noa-frame-encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("noa-cell-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(self.clear_color[0]),
                            g: f64::from(self.clear_color[1]),
                            b: f64::from(self.clear_color[2]),
                            a: f64::from(self.clear_color[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if !self.instances.is_empty() {
                pass.set_pipeline(&self.cell.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..6, 0..self.instances.len() as u32);
            }
        }

        queue.submit(Some(encoder.finish()));
    }

    /// Re-upload the atlas texture if `font`'s atlas grew or changed since
    /// the last upload. Call this before [`Renderer::draw`] each frame (from
    /// `noa-app`, while still holding `font` mutably for `rebuild_cells`).
    pub fn sync_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, font: &mut FontGrid) {
        if !font.take_atlas_dirty() {
            return;
        }
        let (w, h) = font.atlas_size();
        if w != self.atlas_texture.width() || h != self.atlas_texture.height() {
            self.atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("noa-glyph-atlas"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.atlas_view = self
                .atlas_texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = self.cell.make_bind_group(device, &self.atlas_view);
        }
        upload_atlas(queue, &self.atlas_texture, font.atlas_data(), w, h);
    }

    fn upload_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = (self.instances.len() * 2).max(self.instance_capacity * 2);
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("noa-instance-buffer"),
                size: (self.instance_capacity * std::mem::size_of::<CellInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
    }

    fn upload_uniforms(&self, queue: &wgpu::Queue) {
        let (cell_w, cell_h) = self.cell_size;
        let width = self.viewport.w as f32;
        let height = self.viewport.h as f32;
        let uniforms = Uniforms {
            projection: orthographic_projection(width.max(1.0), height.max(1.0)),
            screen_size: [width, height],
            cell_size: [cell_w, cell_h],
            grid_size: [
                if cell_w > 0.0 { width / cell_w } else { 0.0 },
                if cell_h > 0.0 { height / cell_h } else { 0.0 },
            ],
            grid_padding: [0.0, 0.0, 0.0, 0.0],
            cursor_pos: [0.0, 0.0],
            cursor_color: [1.0, 1.0, 1.0, 1.0],
            bg_color: self.clear_color,
            min_contrast: 0.0,
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.cell.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }
}

fn rebuild_cell_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
) -> ([f32; 4], (f32, f32)) {
    let metrics = font.metrics();
    let clear_color = theme.default_bg_with_colors(&snap.colors);

    instances.clear();
    let mut bg_instances = Vec::new();
    let mut glyph_instances = Vec::new();

    for (row_idx, row) in snap.rows.iter().enumerate() {
        let y = row_idx as u16;
        for (col_idx, cell) in row.cells.iter().enumerate() {
            let x = col_idx as u16;
            let selected = snap.is_selected(x, y);
            let active_search = snap.is_active_search_match(x, y);
            let search_match = snap.is_search_match(x, y);
            let cursor_cell = snap.cursor.visible && snap.cursor.x == x && snap.cursor.y == y;

            let inverse = cell.attrs.contains(CellAttrs::INVERSE);
            let (fg_color, bg_color) = if inverse {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };

            // Background quad: skip when it's the plain default bg (the
            // clear color already fills that), unless inverted.
            let bg_is_default = matches!(bg_color, Color::Default) && !inverse;
            if cursor_cell || selected || active_search || search_match || !bg_is_default {
                let bg = if cursor_cell {
                    if snap.colors.cursor().is_some() {
                        theme.cursor_with_colors(&snap.colors)
                    } else {
                        theme.resolve_with_colors(fg_color, true, &snap.colors)
                    }
                } else if selected {
                    theme.selection_bg()
                } else if active_search {
                    theme.active_search_bg()
                } else if search_match {
                    theme.search_bg()
                } else {
                    theme.resolve_with_colors(bg_color, false, &snap.colors)
                };
                bg_instances.push(CellInstance {
                    glyph_pos: [0, 0],
                    glyph_size: [0, 0],
                    bearing: [0, 0],
                    grid_pos: [x, y],
                    color: to_u8_color(bg),
                    flags: if cursor_cell {
                        CellInstance::FLAG_CURSOR
                    } else {
                        0
                    },
                });
            }

            // Glyph quad: skip blanks and invisible text.
            let invisible = cell.attrs.contains(CellAttrs::INVISIBLE);
            let wide_spacer = cell.attrs.contains(CellAttrs::WIDE_SPACER);
            if cell.ch != ' ' && !invisible && !wide_spacer {
                let glyph = font.get_or_raster(cell.ch);
                if glyph.atlas_size[0] > 0 && glyph.atlas_size[1] > 0 {
                    let fg = if cursor_cell {
                        theme.resolve_with_colors(bg_color, false, &snap.colors)
                    } else if selected {
                        theme.selection_fg()
                    } else if active_search {
                        theme.active_search_fg()
                    } else if search_match {
                        theme.search_fg()
                    } else {
                        theme.resolve_with_colors(fg_color, true, &snap.colors)
                    };
                    let mut flags = CellInstance::FLAG_GLYPH;
                    if cursor_cell {
                        flags |= CellInstance::FLAG_CURSOR;
                    }
                    glyph_instances.push(CellInstance {
                        glyph_pos: glyph.atlas_pos,
                        glyph_size: glyph.atlas_size,
                        bearing: glyph_cell_bearing(metrics, glyph.bearing),
                        grid_pos: [x, y],
                        color: to_u8_color(fg),
                        flags,
                    });
                }
            }
        }
    }
    instances.extend(bg_instances);
    instances.extend(glyph_instances);

    (clear_color, (metrics.cell_w, metrics.cell_h))
}

fn to_u8_color(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn glyph_cell_bearing(metrics: Metrics, pen_bearing: [i16; 2]) -> [i16; 2] {
    [
        pen_bearing[0],
        (metrics.ascent.round() - pen_bearing[1] as f32) as i16,
    ]
}

fn upload_atlas(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &[u8], w: u32, h: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::{Color, GridSize, Rgb};
    use noa_grid::Terminal;

    fn metrics(ascent: f32) -> Metrics {
        Metrics {
            cell_w: 10.0,
            cell_h: 24.0,
            ascent,
            descent: 6.0,
            line_gap: 0.0,
            underline_position: 0.0,
            underline_thickness: 1.0,
        }
    }

    #[test]
    fn glyph_bearing_converts_from_baseline_to_cell_top() {
        assert_eq!(glyph_cell_bearing(metrics(18.0), [2, 14]), [2, 4]);
    }

    #[test]
    fn cursor_cell_with_glyph_generates_reversed_glyph_instance() {
        let mut font = match FontGrid::new(14.0) {
            Ok(font) => font,
            Err(err) => {
                eprintln!("skipping: no system monospace font available: {err}");
                return;
            }
        };
        let glyph = font.get_or_raster('M');
        if glyph.atlas_size == [0, 0] {
            eprintln!("skipping: installed monospace font did not rasterize 'M'");
            return;
        }

        let mut terminal = Terminal::new(GridSize::new(1, 1));
        terminal.primary.cursor.x = 0;
        terminal.primary.cursor.y = 0;
        terminal.primary.grid[0].cells[0].ch = 'M';
        terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
        terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
        let snap = FrameSnapshot::from_terminal(&terminal);

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new());

        let cursor_bg_index = instances
            .iter()
            .position(|instance| {
                instance.grid_pos == [0, 0]
                    && instance.flags == CellInstance::FLAG_CURSOR
                    && instance.glyph_size == [0, 0]
            })
            .expect("cursor cell should have a background cursor instance");
        let cursor_glyph_index = instances
            .iter()
            .position(|instance| {
                instance.grid_pos == [0, 0]
                    && instance.flags & CellInstance::FLAG_CURSOR != 0
                    && instance.flags & CellInstance::FLAG_GLYPH != 0
            })
            .expect("cursor cell glyph must be retained as a cursor glyph instance");
        assert!(
            cursor_bg_index < cursor_glyph_index,
            "cursor background must be emitted before the glyph so it does not cover text"
        );
        assert_eq!(
            instances[cursor_bg_index].color,
            [240, 10, 20, 255],
            "cursor background should use the cell foreground"
        );
        let cursor_glyph = instances[cursor_glyph_index];

        assert_ne!(
            cursor_glyph.glyph_size,
            [0, 0],
            "cursor glyph instance must sample the atlas instead of becoming a blank quad"
        );
        assert_eq!(
            cursor_glyph.color,
            [2, 3, 4, 255],
            "cursor glyph color should use the cell background"
        );
        assert_eq!(
            instances
                .last()
                .map(|instance| instance.flags & CellInstance::FLAG_GLYPH),
            Some(CellInstance::FLAG_GLYPH),
            "the final cursor-cell instance must not be an opaque blank cursor quad"
        );
    }
}
