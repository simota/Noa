//! [`Renderer`] — owns the GPU pipeline, the font atlas texture, and the
//! instance buffers; rebuilds them from a [`crate::FrameSnapshot`] and draws.

use noa_core::{CellAttrs, Color, GridPadding, PixelSize};
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
    grid_padding: GridPadding,
    clear_color: [f32; 4],
    target_format_is_srgb: bool,
    atlas_seen_generation: u64,
}

impl Renderer {
    /// Build the renderer, uploading the font atlas as it currently stands.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font: &mut FontGrid,
        grid_padding: GridPadding,
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
        let atlas_seen_generation = font.atlas_generation();

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
            grid_padding,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            target_format_is_srgb: format.is_srgb(),
            atlas_seen_generation,
        })
    }

    /// Update the known viewport size (called on `WindowEvent::Resized`).
    pub fn resize(&mut self, px: PixelSize) {
        self.viewport = px;
    }

    /// Atlas generation this renderer has uploaded.
    pub fn atlas_seen_generation(&self) -> u64 {
        self.atlas_seen_generation
    }

    /// Rebuild the CPU instance list from a snapshot, re-rastering any glyphs
    /// not yet in the atlas and re-uploading the atlas texture if it grew.
    pub fn rebuild_cells(&mut self, snap: &FrameSnapshot, font: &mut FontGrid, theme: &Theme) {
        let (clear_color, cell_size) = rebuild_cell_instances(
            &mut self.instances,
            snap,
            font,
            theme,
            self.target_format_is_srgb,
        );
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
        let generation = font.atlas_generation();
        if generation == self.atlas_seen_generation {
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
        self.atlas_seen_generation = generation;
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
        let content_width = (width - self.grid_padding.horizontal()).max(0.0);
        let content_height = (height - self.grid_padding.vertical()).max(0.0);
        let uniforms = Uniforms {
            projection: orthographic_projection(width.max(1.0), height.max(1.0)),
            screen_size: [width, height],
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
            grid_padding: self.grid_padding.as_uniform(),
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
    target_format_is_srgb: bool,
) -> ([f32; 4], (f32, f32)) {
    let metrics = font.metrics();
    let clear_color = surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    );

    instances.clear();
    let mut bg_instances = Vec::new();
    let mut glyph_instances = Vec::new();
    let mut decoration_instances = Vec::new();

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
                        surface_output_rgba(
                            theme.cursor_with_colors(&snap.colors),
                            target_format_is_srgb,
                        )
                    } else {
                        surface_output_rgba(
                            theme.resolve_with_colors(fg_color, true, &snap.colors),
                            target_format_is_srgb,
                        )
                    }
                } else if selected {
                    surface_output_rgba(theme.selection_bg(), target_format_is_srgb)
                } else if active_search {
                    surface_output_rgba(theme.active_search_bg(), target_format_is_srgb)
                } else if search_match {
                    surface_output_rgba(theme.search_bg(), target_format_is_srgb)
                } else {
                    surface_output_rgba(
                        theme.resolve_with_colors(bg_color, false, &snap.colors),
                        target_format_is_srgb,
                    )
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

            let text_color = if cursor_cell {
                surface_output_rgba(
                    theme.resolve_with_colors(bg_color, false, &snap.colors),
                    target_format_is_srgb,
                )
            } else if selected {
                surface_output_rgba(theme.selection_fg(), target_format_is_srgb)
            } else if active_search {
                surface_output_rgba(theme.active_search_fg(), target_format_is_srgb)
            } else if search_match {
                surface_output_rgba(theme.search_fg(), target_format_is_srgb)
            } else {
                surface_output_rgba(
                    theme.resolve_with_colors(fg_color, true, &snap.colors),
                    target_format_is_srgb,
                )
            };

            let invisible = cell.attrs.contains(CellAttrs::INVISIBLE);
            let wide_spacer = cell.attrs.contains(CellAttrs::WIDE_SPACER);
            if !invisible && !wide_spacer {
                let decoration_color = if let Some(color) = cell.underline_color {
                    surface_output_rgba(
                        theme.resolve_with_colors(color, true, &snap.colors),
                        target_format_is_srgb,
                    )
                } else {
                    text_color
                };
                push_cell_decorations(
                    &mut decoration_instances,
                    x,
                    y,
                    cell.attrs,
                    to_u8_color(decoration_color),
                    metrics,
                );
            }

            // Glyph quad: skip blanks and invisible text.
            if !cell.is_blank() && !invisible && !wide_spacer {
                let mut flags = CellInstance::FLAG_GLYPH;
                if cursor_cell {
                    flags |= CellInstance::FLAG_CURSOR;
                }
                for ch in cell.text_chars() {
                    let glyph = font.get_or_raster(ch);
                    if glyph.atlas_size[0] == 0 || glyph.atlas_size[1] == 0 {
                        continue;
                    }
                    glyph_instances.push(CellInstance {
                        glyph_pos: glyph.atlas_pos,
                        glyph_size: glyph.atlas_size,
                        bearing: glyph_cell_bearing(metrics, glyph.bearing),
                        grid_pos: [x, y],
                        color: to_u8_color(text_color),
                        flags,
                    });
                }
            }
        }
    }
    instances.extend(bg_instances);
    instances.extend(glyph_instances);
    instances.extend(decoration_instances);

    (clear_color, (metrics.cell_w, metrics.cell_h))
}

fn push_cell_decorations(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    attrs: CellAttrs,
    color: [u8; 4],
    metrics: Metrics,
) {
    let thickness = decoration_thickness(metrics);
    let width = metrics.cell_w.round().max(1.0) as u16;

    if attrs.contains(CellAttrs::OVERLINE) {
        push_decoration_rect(instances, x, y, 0, 0, width, thickness, color);
    }
    if attrs.contains(CellAttrs::STRIKETHROUGH) {
        let strike_y = clamp_decoration_y(metrics.ascent * 0.55, thickness, metrics);
        push_decoration_rect(instances, x, y, 0, strike_y, width, thickness, color);
    }

    if attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
        let first_y = underline_y(metrics, thickness, -1.0);
        let second_y = underline_y(metrics, thickness, thickness as f32 + 1.0);
        push_decoration_rect(instances, x, y, 0, first_y, width, thickness, color);
        push_decoration_rect(instances, x, y, 0, second_y, width, thickness, color);
    } else if attrs.contains(CellAttrs::CURLY_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(
            instances,
            x,
            y,
            width,
            thickness,
            base_y,
            color,
            CurlPattern,
        );
    } else if attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, x, y, width, thickness, base_y, color, DotPattern);
    } else if attrs.contains(CellAttrs::DASHED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(
            instances,
            x,
            y,
            width,
            thickness,
            base_y,
            color,
            DashPattern,
        );
    } else if attrs.contains(CellAttrs::UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_decoration_rect(instances, x, y, 0, base_y, width, thickness, color);
    }
}

fn decoration_thickness(metrics: Metrics) -> u16 {
    metrics
        .underline_thickness
        .round()
        .max(1.0)
        .min(metrics.cell_h.max(1.0)) as u16
}

fn underline_y(metrics: Metrics, thickness: u16, offset: f32) -> i16 {
    let center = metrics.ascent - metrics.underline_position + offset;
    clamp_decoration_y(center - thickness as f32 / 2.0, thickness, metrics)
}

fn clamp_decoration_y(y: f32, thickness: u16, metrics: Metrics) -> i16 {
    let max_y = (metrics.cell_h - thickness as f32).max(0.0);
    y.round().clamp(0.0, max_y) as i16
}

trait SegmentPattern {
    fn segment(
        &self,
        index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]);
    fn advance(&self, thickness: u16) -> u16;
}

struct DotPattern;
struct DashPattern;
struct CurlPattern;

impl SegmentPattern for DotPattern {
    fn segment(
        &self,
        _index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        ([x as i16, base_y], [width.min(thickness), thickness])
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(2).max(2)
    }
}

impl SegmentPattern for DashPattern {
    fn segment(
        &self,
        _index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        let dash_width = width.min(thickness.saturating_mul(4).max(4));
        ([x as i16, base_y], [dash_width, thickness])
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(6).max(6)
    }
}

impl SegmentPattern for CurlPattern {
    fn segment(
        &self,
        index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        let y_offset = if index % 2 == 0 { 0 } else { thickness as i16 };
        (
            [x as i16, base_y.saturating_sub(y_offset)],
            [width.min(thickness.saturating_mul(2).max(2)), thickness],
        )
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(2).max(2)
    }
}

fn push_segmented_decoration<P: SegmentPattern>(
    instances: &mut Vec<CellInstance>,
    grid_x: u16,
    grid_y: u16,
    width: u16,
    thickness: u16,
    base_y: i16,
    color: [u8; 4],
    pattern: P,
) {
    let advance = pattern.advance(thickness);
    let mut index = 0;
    let mut x = 0;
    while x < width {
        let remaining = width - x;
        let (bearing, size) = pattern.segment(index, x, remaining, thickness, base_y);
        push_decoration_rect(
            instances, grid_x, grid_y, bearing[0], bearing[1], size[0], size[1], color,
        );
        index += 1;
        x = x.saturating_add(advance);
    }
}

fn push_decoration_rect(
    instances: &mut Vec<CellInstance>,
    grid_x: u16,
    grid_y: u16,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    color: [u8; 4],
) {
    instances.push(CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [width.max(1), height.max(1)],
        bearing: [x, y],
        grid_pos: [grid_x, grid_y],
        color,
        flags: CellInstance::FLAG_DECORATION,
    });
}

fn to_u8_color(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn surface_output_rgba(c: [f32; 4], target_format_is_srgb: bool) -> [f32; 4] {
    if !target_format_is_srgb {
        return c;
    }

    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        c[3].clamp(0.0, 1.0),
    ]
}

fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
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
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

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

    #[test]
    fn decorations_emit_rect_instances_from_cell_attrs() {
        let mut instances = Vec::new();
        let metrics = Metrics {
            cell_w: 12.0,
            cell_h: 24.0,
            ascent: 18.0,
            descent: 6.0,
            line_gap: 0.0,
            underline_position: -2.0,
            underline_thickness: 2.0,
        };

        push_cell_decorations(
            &mut instances,
            3,
            4,
            CellAttrs::DOUBLE_UNDERLINE | CellAttrs::STRIKETHROUGH | CellAttrs::OVERLINE,
            [1, 2, 3, 255],
            metrics,
        );

        assert_eq!(instances.len(), 4);
        assert!(
            instances
                .iter()
                .all(|instance| instance.flags == CellInstance::FLAG_DECORATION)
        );
        assert!(
            instances
                .iter()
                .all(|instance| instance.grid_pos == [3, 4] && instance.color == [1, 2, 3, 255])
        );
        assert_eq!(
            instances[0].bearing,
            [0, 0],
            "overline starts at the cell top"
        );
        assert_eq!(
            instances[2].glyph_size,
            [12, 2],
            "double underline keeps full-cell width and metric thickness"
        );
        assert!(
            instances[2].bearing[1] < instances[3].bearing[1],
            "double underline emits two vertically separated strokes"
        );
    }

    #[test]
    fn patterned_underlines_emit_segmented_rectangles() {
        let metrics = Metrics {
            cell_w: 9.0,
            cell_h: 20.0,
            ascent: 14.0,
            descent: 6.0,
            line_gap: 0.0,
            underline_position: -1.0,
            underline_thickness: 1.0,
        };

        let mut dotted = Vec::new();
        push_cell_decorations(
            &mut dotted,
            0,
            0,
            CellAttrs::DOTTED_UNDERLINE,
            [9, 9, 9, 255],
            metrics,
        );
        assert!(
            dotted.len() > 1,
            "dotted underline should be split into repeated dot rectangles"
        );
        assert!(dotted.iter().all(|instance| instance.glyph_size[0] == 1));

        let mut dashed = Vec::new();
        push_cell_decorations(
            &mut dashed,
            0,
            0,
            CellAttrs::DASHED_UNDERLINE,
            [9, 9, 9, 255],
            metrics,
        );
        assert!(dashed.iter().any(|instance| instance.glyph_size[0] > 1));

        let mut curly = Vec::new();
        push_cell_decorations(
            &mut curly,
            0,
            0,
            CellAttrs::CURLY_UNDERLINE,
            [9, 9, 9, 255],
            metrics,
        );
        assert!(
            curly
                .windows(2)
                .any(|pair| pair[0].bearing[1] != pair[1].bearing[1]),
            "curly underline should alternate segment vertical positions"
        );
    }

    #[test]
    fn srgb_surface_output_converts_theme_colors_to_linear() {
        let srgb = [30.0 / 255.0, 30.0 / 255.0, 30.0 / 255.0, 1.0];

        assert_eq!(
            to_u8_color(surface_output_rgba(srgb, false)),
            [30, 30, 30, 255]
        );
        assert_eq!(to_u8_color(surface_output_rgba(srgb, true)), [3, 3, 3, 255]);
    }

    #[test]
    fn srgb_surface_output_preserves_extremes_and_alpha() {
        assert_eq!(
            surface_output_rgba([0.0, 1.0, 0.5, 0.5], true),
            [0.0, 1.0, srgb_to_linear(0.5), 0.5]
        );
    }
}
