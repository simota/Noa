//! [`Renderer`] — owns the GPU pipeline, the font atlas texture, and the
//! instance buffers; rebuilds them from a [`crate::FrameSnapshot`] and draws.

use std::ops::Range;

use noa_core::{CellAttrs, CellSize, Color, GridPadding, PixelSize};
use noa_font::{FontGrid, Metrics, ShapedGlyph};

use crate::draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
use crate::instance::{CellInstance, PaneUniformParams, populate_pane_uniform};
use crate::pipeline::CellPipeline;
use crate::segment::{SegmentCell, ShapeRun, segment_row};
use crate::snapshot::FrameSnapshot;
use crate::theme::Theme;

const DEFAULT_PANE_ID: PaneId = PaneId::new(0);
const DIVIDER_RGBA: [u8; 4] = [82, 82, 82, 255];
const FOCUS_INDICATOR_RGBA: [u8; 4] = [95, 175, 255, 230];

/// One pane's immutable frame input.
pub struct PaneFrame<'a> {
    pub pane: PaneId,
    pub rect: PaneRect,
    pub snapshot: &'a FrameSnapshot,
}

struct PaneGpuState {
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bind_group_rebuilds: u64,
}

struct PaneInstances {
    pane: PaneId,
    range: Range<u32>,
}

/// The wgpu instanced-cell renderer. Windowing-agnostic: it receives an
/// already-created `Device`/`Queue`/surface format and never touches
/// `winit` or `wgpu::Surface`.
pub struct Renderer {
    cell: CellPipeline,
    mask_atlas_texture: wgpu::Texture,
    mask_atlas_view: wgpu::TextureView,
    color_atlas_texture: wgpu::Texture,
    color_atlas_view: wgpu::TextureView,
    pane_gpu: Vec<PaneGpuState>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<CellInstance>,
    cell_instance_len: usize,
    pane_instances: Vec<PaneInstances>,
    pane_layout: Vec<(PaneId, PaneRect)>,
    divider_range: Range<u32>,
    focus_indicator_range: Range<u32>,
    viewport: PixelSize,
    cell_size: (f32, f32),
    grid_padding: GridPadding,
    clear_color: [f32; 4],
    target_format_is_srgb: bool,
    mask_atlas_seen_generation: u64,
    color_atlas_seen_generation: u64,
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

        let (mask_w, mask_h) = font.mask_atlas_size();
        let mask_atlas_texture = create_atlas_texture(
            device,
            mask_w,
            mask_h,
            wgpu::TextureFormat::R8Unorm,
            "noa-glyph-mask-atlas",
        );
        let mask_atlas_view =
            mask_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        upload_atlas(
            queue,
            &mask_atlas_texture,
            font.mask_atlas_data(),
            mask_w,
            mask_h,
            MASK_BYTES_PER_PX,
        );
        let mask_atlas_seen_generation = font.mask_atlas_generation();

        // RGBA8Unorm (not sRGB) — the color bitmap is sampled verbatim as
        // passthrough, no re-encode (WP1, REQ-EMOJI-2).
        let (color_w, color_h) = font.color_atlas_size();
        let color_atlas_texture = create_atlas_texture(
            device,
            color_w,
            color_h,
            wgpu::TextureFormat::Rgba8Unorm,
            "noa-glyph-color-atlas",
        );
        let color_atlas_view =
            color_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        upload_atlas(
            queue,
            &color_atlas_texture,
            font.color_atlas_data(),
            color_w,
            color_h,
            COLOR_BYTES_PER_PX,
        );
        let color_atlas_seen_generation = font.color_atlas_generation();

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
            mask_atlas_texture,
            mask_atlas_view,
            color_atlas_texture,
            color_atlas_view,
            pane_gpu: Vec::new(),
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
            cell_instance_len: 0,
            pane_instances: Vec::new(),
            pane_layout: Vec::new(),
            divider_range: 0..0,
            focus_indicator_range: 0..0,
            viewport: PixelSize { w: 0, h: 0 },
            cell_size: (metrics.cell_w, metrics.cell_h),
            grid_padding,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            target_format_is_srgb: format.is_srgb(),
            mask_atlas_seen_generation,
            color_atlas_seen_generation,
        })
    }

    /// Update the known viewport size (called on `WindowEvent::Resized`).
    pub fn resize(&mut self, px: PixelSize) {
        self.viewport = px;
    }

    /// Mask atlas generation this renderer has uploaded.
    pub fn mask_atlas_seen_generation(&self) -> u64 {
        self.mask_atlas_seen_generation
    }

    /// Color atlas generation this renderer has uploaded.
    pub fn color_atlas_seen_generation(&self) -> u64 {
        self.color_atlas_seen_generation
    }

    /// Per-pane bind-group rebuild counters, exposed for headless pipeline
    /// tests that assert atlas reallocation refreshes every pane binding.
    pub fn pane_bind_group_rebuild_counts(&self) -> Vec<u64> {
        self.pane_gpu
            .iter()
            .map(|pane| pane.bind_group_rebuilds)
            .collect()
    }

    /// Rebuild the CPU instance list from a snapshot, re-rastering any glyphs
    /// not yet in the atlas and re-uploading the atlas texture if it grew.
    pub fn rebuild_cells(&mut self, snap: &FrameSnapshot, font: &mut FontGrid, theme: &Theme) {
        let panes = [PaneFrame {
            pane: DEFAULT_PANE_ID,
            rect: self.full_viewport_rect(),
            snapshot: snap,
        }];
        self.rebuild_panes(&panes, font, theme);
    }

    /// Rebuild the CPU instance list for all visible panes in one frame.
    ///
    /// The caller should rebuild every pane first, then call [`Renderer::sync_atlas`]
    /// once before drawing so all panes observe the same atlas mutation point.
    /// A noisy pane still causes all visible pane snapshots to be rebuilt and
    /// the full window to be redrawn each frame; dirty-row diffing remains a
    /// follow-up beyond the v1 split-pane renderer.
    pub fn rebuild_panes(&mut self, panes: &[PaneFrame<'_>], font: &mut FontGrid, theme: &Theme) {
        self.instances.clear();
        self.pane_instances.clear();
        self.pane_layout.clear();
        self.divider_range = 0..0;

        let mut first_clear_color = None;
        for pane in panes {
            let start = self.instances.len() as u32;
            let mut pane_instances = Vec::new();
            let (clear_color, cell_size) = rebuild_cell_instances(
                &mut pane_instances,
                pane.snapshot,
                font,
                theme,
                self.target_format_is_srgb,
            );
            first_clear_color.get_or_insert(clear_color);
            self.cell_size = cell_size;
            self.instances.extend(pane_instances);
            let end = self.instances.len() as u32;
            self.pane_instances.push(PaneInstances {
                pane: pane.pane,
                range: start..end,
            });
            self.pane_layout.push((pane.pane, pane.rect));
        }

        if let Some(clear_color) = first_clear_color {
            self.clear_color = clear_color;
        }
        self.cell_instance_len = self.instances.len();
    }

    /// Draw the current instance list into `view`, uploading updated GPU
    /// state (uniforms, atlas, instance buffer) first.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
        let layout = [(DEFAULT_PANE_ID, self.full_viewport_rect())];
        self.draw_panes(device, queue, view, &layout, None, None);
    }

    /// Draw the layout most recently supplied to [`Renderer::rebuild_panes`].
    pub fn draw_rebuilt_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        focused: Option<PaneId>,
        zoomed: Option<PaneId>,
    ) {
        let layout = self.pane_layout.clone();
        self.draw_panes(device, queue, view, &layout, focused, zoomed);
    }

    /// Draw a split-pane layout into `view` using the pure draw-plan order.
    pub fn draw_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        layout: &[(PaneId, PaneRect)],
        focused: Option<PaneId>,
        zoomed: Option<PaneId>,
    ) {
        let plan = build_draw_plan(layout, focused, zoomed);
        self.ensure_pane_gpu_count(device, layout.len());
        self.upload_uniforms(queue, layout);
        self.prepare_overlay_instances(&plan);
        self.upload_instances(device, queue);

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
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                for op in &plan {
                    match op {
                        DrawOp::Clear => {}
                        DrawOp::PaneCells {
                            pane,
                            scissor,
                            bind_group_index,
                        } => {
                            let Some(range) = self.instance_range_for(*pane) else {
                                continue;
                            };
                            if range.is_empty() || scissor.w == 0 || scissor.h == 0 {
                                continue;
                            }
                            let Some(gpu) = self.pane_gpu.get(*bind_group_index) else {
                                continue;
                            };
                            pass.set_scissor_rect(scissor.x, scissor.y, scissor.w, scissor.h);
                            pass.set_bind_group(0, &gpu.bind_group, &[]);
                            pass.draw(0..6, range);
                        }
                        DrawOp::Dividers { .. } => {
                            self.draw_pixel_overlay_range(&mut pass, self.divider_range.clone());
                        }
                        DrawOp::FocusIndicator { .. } => {
                            self.draw_pixel_overlay_range(
                                &mut pass,
                                self.focus_indicator_range.clone(),
                            );
                        }
                    }
                }
            }
        }

        queue.submit(Some(encoder.finish()));
    }

    /// Re-upload either/both atlas textures if `font`'s mask or color atlas
    /// grew or changed since the last upload. Call this before
    /// [`Renderer::draw`] each frame (from `noa-app`, while still holding
    /// `font` mutably for `rebuild_cells`).
    ///
    /// Shared per-atlas sync path (FM-09 mitigation, WP1): both atlases are
    /// synced through the same [`sync_one_atlas`] helper rather than two
    /// hand-copied blocks, so a size-changed realloc on *either* atlas always
    /// rebuilds every pane bind group — the two-atlas bind group carries both
    /// texture views, so a stale view on the un-synced atlas would otherwise
    /// be an easy "fixed one, forgot the other" bug.
    pub fn sync_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, font: &mut FontGrid) {
        let mask_changed = sync_one_atlas(
            device,
            queue,
            &mut self.mask_atlas_texture,
            &mut self.mask_atlas_view,
            &mut self.mask_atlas_seen_generation,
            AtlasSyncInput {
                data: font.mask_atlas_data(),
                size: font.mask_atlas_size(),
                generation: font.mask_atlas_generation(),
                format: wgpu::TextureFormat::R8Unorm,
                bytes_per_px: MASK_BYTES_PER_PX,
                label: "noa-glyph-mask-atlas",
            },
        );
        let color_changed = sync_one_atlas(
            device,
            queue,
            &mut self.color_atlas_texture,
            &mut self.color_atlas_view,
            &mut self.color_atlas_seen_generation,
            AtlasSyncInput {
                data: font.color_atlas_data(),
                size: font.color_atlas_size(),
                generation: font.color_atlas_generation(),
                format: wgpu::TextureFormat::Rgba8Unorm,
                bytes_per_px: COLOR_BYTES_PER_PX,
                label: "noa-glyph-color-atlas",
            },
        );
        if mask_changed || color_changed {
            self.rebuild_pane_bind_groups(device);
        }
    }

    fn full_viewport_rect(&self) -> PaneRect {
        PaneRect::new(0, 0, self.viewport.w, self.viewport.h)
    }

    fn ensure_pane_gpu_count(&mut self, device: &wgpu::Device, count: usize) {
        while self.pane_gpu.len() < count {
            let uniform_buffer = self.cell.make_uniform_buffer(device);
            let bind_group = self.cell.make_bind_group(
                device,
                &uniform_buffer,
                &self.mask_atlas_view,
                &self.color_atlas_view,
            );
            self.pane_gpu.push(PaneGpuState {
                uniform_buffer,
                bind_group,
                bind_group_rebuilds: 1,
            });
        }
    }

    /// Rebuild every pane's bind group against the current mask + color
    /// atlas views. Called whenever *either* atlas texture is recreated
    /// (`sync_atlas`), so a realloc of just one atlas still refreshes both
    /// views baked into each pane's bind group (FM-09).
    fn rebuild_pane_bind_groups(&mut self, device: &wgpu::Device) {
        for pane in &mut self.pane_gpu {
            pane.bind_group = self.cell.make_bind_group(
                device,
                &pane.uniform_buffer,
                &self.mask_atlas_view,
                &self.color_atlas_view,
            );
            pane.bind_group_rebuilds = pane.bind_group_rebuilds.saturating_add(1);
        }
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

    fn upload_uniforms(&self, queue: &wgpu::Queue, layout: &[(PaneId, PaneRect)]) {
        let (cell_w, cell_h) = self.cell_size;
        for (index, (_, rect)) in layout.iter().enumerate() {
            let Some(gpu) = self.pane_gpu.get(index) else {
                continue;
            };
            let uniforms = populate_pane_uniform(PaneUniformParams {
                pane_rect: *rect,
                window_size: self.viewport,
                grid_padding: self.grid_padding,
                cell_size: CellSize {
                    w: cell_w,
                    h: cell_h,
                },
                bg_color: self.clear_color,
            });
            queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        }
    }

    fn prepare_overlay_instances(&mut self, plan: &[DrawOp]) {
        self.instances.truncate(self.cell_instance_len);
        self.divider_range = 0..0;
        self.focus_indicator_range = 0..0;

        for op in plan {
            match op {
                DrawOp::Dividers { rects } => {
                    let start = self.instances.len() as u32;
                    for rect in rects {
                        self.instances.push(divider_instance(*rect));
                    }
                    let end = self.instances.len() as u32;
                    self.divider_range = start..end;
                }
                DrawOp::FocusIndicator { rects, .. } => {
                    let start = self.instances.len() as u32;
                    for rect in rects {
                        self.instances.push(focus_indicator_instance(*rect));
                    }
                    let end = self.instances.len() as u32;
                    self.focus_indicator_range = start..end;
                }
                DrawOp::Clear | DrawOp::PaneCells { .. } => {}
            }
        }
    }

    fn draw_pixel_overlay_range(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<u32>) {
        if range.is_empty() || self.viewport.w == 0 || self.viewport.h == 0 {
            return;
        }
        let Some(gpu) = self.pane_gpu.first() else {
            return;
        };
        pass.set_scissor_rect(0, 0, self.viewport.w, self.viewport.h);
        pass.set_bind_group(0, &gpu.bind_group, &[]);
        pass.draw(0..6, range);
    }

    fn instance_range_for(&self, pane: PaneId) -> Option<Range<u32>> {
        self.pane_instances
            .iter()
            .find(|entry| entry.pane == pane)
            .map(|entry| entry.range.clone())
    }
}

fn divider_instance(rect: PaneRect) -> CellInstance {
    pixel_overlay_instance(rect, DIVIDER_RGBA)
}

fn focus_indicator_instance(rect: PaneRect) -> CellInstance {
    pixel_overlay_instance(rect, FOCUS_INDICATOR_RGBA)
}

fn pixel_overlay_instance(rect: PaneRect, color: [u8; 4]) -> CellInstance {
    CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [to_u16_saturating(rect.w), to_u16_saturating(rect.h)],
        bearing: [0, 0],
        grid_pos: [to_u16_saturating(rect.x), to_u16_saturating(rect.y)],
        color,
        flags: CellInstance::FLAG_DIVIDER,
    }
}

fn to_u16_saturating(value: u32) -> u16 {
    value.min(u32::from(u16::MAX)) as u16
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
        let mut segment_cells = Vec::with_capacity(row.cells.len());
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

            // WP2 (REQ-SHAPE-1/4/6): feed this cell into the row's
            // shape-run segmentation instead of rasterizing it inline here.
            // Invisible cells are forced blank so shaping never produces
            // ink for them (mirrors the old `!invisible` glyph-skip check);
            // a plain blank/wide-spacer cell needs no special-casing here
            // because it naturally rasterizes to an empty glyph, filtered
            // out in `emit_run_glyph_instances`.
            let (shape_ch, shape_combining) = if invisible {
                (' ', Vec::new())
            } else {
                (cell.ch, cell.combining.chars().collect())
            };
            segment_cells.push(SegmentCell {
                ch: shape_ch,
                combining: shape_combining,
                bold: cell.attrs.contains(CellAttrs::BOLD),
                italic: cell.attrs.contains(CellAttrs::ITALIC),
                selected,
                active_search,
                search_match,
                cursor: cursor_cell,
                color: to_u8_color(text_color),
            });
        }

        // REQ-SHAPE-1/6: shape this row's runs and emit glyph instances from
        // the SHAPED GLYPH list, not per source cell (FM-04) — see
        // `emit_run_glyph_instances`. A ligature therefore naturally
        // collapses to one instance at its cluster-start cell (the cells it
        // covers get no glyph instance at all), and a combining mark
        // becomes an extra instance anchored at its base cell, positioned
        // by its own shaped offset instead of an independent per-char pen
        // bearing (REQ-SHAPE-4).
        for run in segment_row(font, &segment_cells) {
            let shaped = font.shape_run(&run.cells);
            emit_run_glyph_instances(&mut glyph_instances, font, &run, &shaped, y, metrics);
        }
    }
    instances.extend(bg_instances);
    instances.extend(glyph_instances);
    instances.extend(decoration_instances);

    (clear_color, (metrics.cell_w, metrics.cell_h))
}

/// Emit one `CellInstance` per shaped glyph in `shaped` (FM-04 structural
/// mitigation: iterate the shaped-glyph list, never ask a source cell
/// "should I draw a glyph" — there is no per-cell suppressed flag to
/// forget). Each glyph is anchored at `run.start_col + glyph.cluster`: for
/// a ligature that is the cluster-start cell (the cells it covers get no
/// instance at all, since no `ShapedGlyph` in `shaped` carries their
/// cluster index — no double-draw); for a combining mark it is the mark's
/// base cell, positioned by the shaped `x_offset`/`y_offset` rather than an
/// independent per-char pen bearing.
fn emit_run_glyph_instances(
    glyph_instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    run: &ShapeRun,
    shaped: &[ShapedGlyph],
    row: u16,
    metrics: Metrics,
) {
    for glyph in shaped {
        let cluster = glyph.cluster as usize;
        let (Some(cell), Some(render_info)) =
            (run.cells.get(cluster), run.cell_render.get(cluster))
        else {
            continue;
        };

        let raster = font.raster_shaped(glyph.face_id, glyph.glyph_id, cell.style);
        if raster.atlas_size[0] == 0 || raster.atlas_size[1] == 0 {
            continue;
        }

        let mut flags = CellInstance::FLAG_GLYPH;
        if render_info.cursor {
            flags |= CellInstance::FLAG_CURSOR;
        }
        if raster.color {
            flags |= CellInstance::FLAG_COLOR_GLYPH;
        }

        let base_bearing = glyph_cell_bearing(metrics, raster.bearing);
        let bearing = [
            base_bearing[0].saturating_add(clamp_to_i16(glyph.x_offset)),
            base_bearing[1].saturating_sub(clamp_to_i16(glyph.y_offset)),
        ];
        let anchor_col = run.start_col.saturating_add(glyph.cluster as u16);

        glyph_instances.push(CellInstance {
            glyph_pos: raster.atlas_pos,
            glyph_size: raster.atlas_size,
            bearing,
            grid_pos: [anchor_col, row],
            color: render_info.color,
            flags,
        });
    }
}

fn clamp_to_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
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
    let cell = DecorationCell {
        grid_x: x,
        grid_y: y,
        color,
    };

    if attrs.contains(CellAttrs::OVERLINE) {
        push_decoration_rect(instances, cell, DecorationRect::new(0, 0, width, thickness));
    }
    if attrs.contains(CellAttrs::STRIKETHROUGH) {
        let strike_y = clamp_decoration_y(metrics.ascent * 0.55, thickness, metrics);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, strike_y, width, thickness),
        );
    }

    if attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
        let first_y = underline_y(metrics, thickness, -1.0);
        let second_y = underline_y(metrics, thickness, thickness as f32 + 1.0);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, first_y, width, thickness),
        );
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, second_y, width, thickness),
        );
    } else if attrs.contains(CellAttrs::CURLY_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, CurlPattern);
    } else if attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, DotPattern);
    } else if attrs.contains(CellAttrs::DASHED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, DashPattern);
    } else if attrs.contains(CellAttrs::UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, base_y, width, thickness),
        );
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

#[derive(Clone, Copy)]
struct DecorationCell {
    grid_x: u16,
    grid_y: u16,
    color: [u8; 4],
}

#[derive(Clone, Copy)]
struct DecorationRect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

impl DecorationRect {
    const fn new(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

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
        let y_offset = if index.is_multiple_of(2) {
            0
        } else {
            thickness as i16
        };
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
    cell: DecorationCell,
    width: u16,
    thickness: u16,
    base_y: i16,
    pattern: P,
) {
    let advance = pattern.advance(thickness);
    let mut index = 0;
    let mut x = 0;
    while x < width {
        let remaining = width - x;
        let (bearing, size) = pattern.segment(index, x, remaining, thickness, base_y);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(bearing[0], bearing[1], size[0], size[1]),
        );
        index += 1;
        x = x.saturating_add(advance);
    }
}

fn push_decoration_rect(
    instances: &mut Vec<CellInstance>,
    cell: DecorationCell,
    rect: DecorationRect,
) {
    instances.push(CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [rect.width.max(1), rect.height.max(1)],
        bearing: [rect.x, rect.y],
        grid_pos: [cell.grid_x, cell.grid_y],
        color: cell.color,
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

/// R8 mask atlas: 1 byte per pixel.
const MASK_BYTES_PER_PX: u32 = 1;
/// RGBA8 color atlas: 4 bytes per pixel.
const COLOR_BYTES_PER_PX: u32 = 4;

fn upload_atlas(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    w: u32,
    h: u32,
    bytes_per_px: u32,
) {
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
            bytes_per_row: Some(w * bytes_per_px),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

fn create_atlas_texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Inputs for one atlas's [`sync_one_atlas`] call.
struct AtlasSyncInput<'a> {
    data: &'a [u8],
    size: (u32, u32),
    generation: u64,
    format: wgpu::TextureFormat,
    bytes_per_px: u32,
    label: &'static str,
}

/// Sync one atlas's GPU texture to its CPU-side generation: re-upload the
/// pixel data whenever the generation advanced, first recreating the texture
/// and view if the atlas grew. Returns `true` iff the texture was recreated,
/// so callers know whether dependent bind groups need rebuilding.
///
/// Shared by [`Renderer::sync_atlas`] for both the mask and color atlas
/// (FM-09 mitigation) rather than duplicating this logic per atlas.
fn sync_one_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &mut wgpu::Texture,
    view: &mut wgpu::TextureView,
    seen_generation: &mut u64,
    input: AtlasSyncInput<'_>,
) -> bool {
    if input.generation == *seen_generation {
        return false;
    }
    let (w, h) = input.size;
    let mut recreated = false;
    if w != texture.width() || h != texture.height() {
        *texture = create_atlas_texture(device, w, h, input.format, input.label);
        *view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        recreated = true;
    }
    upload_atlas(queue, texture, input.data, w, h, input.bytes_per_px);
    *seen_generation = input.generation;
    recreated
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::{Color, GridSize, Rgb};
    use noa_font::{FontConfig, ShapeCell, StyleKey};
    use noa_grid::Terminal;

    use crate::segment::CellRenderInfo;

    fn skip_font() -> Option<FontGrid> {
        match FontGrid::new(14.0, FontConfig::default()) {
            Ok(font) => Some(font),
            Err(err) => {
                eprintln!("skipping: no system monospace font available: {err}");
                None
            }
        }
    }

    /// Acquire a real device+queue, or `None` when no adapter exists (skip).
    /// Mirrors `noa-render/tests/pipeline.rs`'s headless-GPU skip pattern.
    fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("noa-renderer-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            }))
            .ok()?;
        Some((device, queue))
    }

    #[test]
    fn renderer_target_format_is_srgb_stays_in_lockstep_with_surface_format() {
        // WP3 / REQ-AA-1 / AC-WP3-01: `Renderer::new` derives
        // `target_format_is_srgb` straight from the surface format passed
        // in, so `surface_output_rgba` only linearizes when the surface
        // actually is sRGB — no double-gamma, no no-gamma artifact.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping target_format_is_srgb lockstep test");
            return;
        };
        let Some(mut font) = skip_font() else {
            return;
        };

        let non_srgb = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer with non-sRGB surface format");
        assert!(
            !non_srgb.target_format_is_srgb,
            "Bgra8Unorm is not sRGB; native gamma-correct blending requires no linearization"
        );

        let srgb = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer with sRGB surface format");
        assert!(
            srgb.target_format_is_srgb,
            "Bgra8UnormSrgb is sRGB; solid colors must still be pre-linearized on this fallback path"
        );
    }

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
        let mut font = match FontGrid::new(14.0, noa_font::FontConfig::default()) {
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
    fn focus_indicator_instance_uses_pixel_overlay_path_and_accent_color() {
        let instance = focus_indicator_instance(PaneRect::new(11, 13, 17, 2));

        assert_eq!(instance.grid_pos, [11, 13]);
        assert_eq!(instance.glyph_size, [17, 2]);
        assert_eq!(instance.color, FOCUS_INDICATOR_RGBA);
        assert_eq!(instance.flags, CellInstance::FLAG_DIVIDER);
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

    // ---- WP2: shaping + ligatures + shape cache -----------------------

    /// AC-WP2-01 [noa-render half of the FM-04 mitigation]: a single
    /// shaped glyph covering 2 source cells (simulating a ligature — real
    /// ligature-font availability isn't guaranteed in every environment, so
    /// this constructs the shaped-glyph list directly instead of depending
    /// on one) must emit exactly ONE glyph instance, anchored at the
    /// cluster-start cell; the covered (non-start) cell must get none.
    /// Proves the consumer iterates the shaped-glyph list rather than
    /// asking each source cell "should I draw" (no per-cell suppression
    /// flag to forget).
    #[test]
    fn ligature_shaped_glyph_emits_one_instance_and_covered_cell_emits_none() {
        let Some(mut font) = skip_font() else { return };
        let style = StyleKey::default();

        let real = font
            .shape_run(&[ShapeCell {
                ch: 'M',
                combining: Vec::new(),
                style,
            }])
            .into_iter()
            .next()
            .expect("shaping 'M' must yield a glyph");

        let run = ShapeRun {
            start_col: 5,
            cells: vec![
                ShapeCell {
                    ch: '!',
                    combining: Vec::new(),
                    style,
                },
                ShapeCell {
                    ch: '=',
                    combining: Vec::new(),
                    style,
                },
            ],
            cell_render: vec![
                CellRenderInfo {
                    color: [10, 20, 30, 255],
                    cursor: false,
                },
                CellRenderInfo {
                    color: [40, 50, 60, 255],
                    cursor: false,
                },
            ],
        };
        // Exactly one shaped glyph for a 2-cell run: the ligature case.
        let shaped = vec![ShapedGlyph {
            glyph_id: real.glyph_id,
            face_id: real.face_id,
            x_advance: real.x_advance,
            x_offset: 0,
            y_offset: 0,
            cluster: 0,
        }];

        let mut glyph_instances = Vec::new();
        let metrics = font.metrics();
        emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 7, metrics);

        assert_eq!(
            glyph_instances.len(),
            1,
            "a ligature (one shaped glyph for 2 source cells) must emit exactly one instance"
        );
        assert_eq!(
            glyph_instances[0].grid_pos,
            [5, 7],
            "the ligature instance must be anchored at start_col + cluster (the cluster-start cell)"
        );
        assert_eq!(
            glyph_instances[0].color,
            [10, 20, 30, 255],
            "instance color must come from the cluster-start cell's render context"
        );
    }

    /// AC-WP2-04 [noa-render half]: multiple shaped glyphs sharing one
    /// cluster (a base glyph plus an attached mark glyph) must each be
    /// emitted, anchored at the SAME cell, positioned by their OWN shaped
    /// `x_offset`/`y_offset` — not merged into one draw and not positioned
    /// by an independent per-char pen bearing.
    #[test]
    fn combining_mark_glyph_is_positioned_by_shaped_offset_not_pen_bearing() {
        let Some(mut font) = skip_font() else { return };
        let style = StyleKey::default();

        let base = font
            .shape_run(&[ShapeCell {
                ch: 'M',
                combining: Vec::new(),
                style,
            }])
            .into_iter()
            .next()
            .expect("shaping 'M' must yield a glyph");

        let run = ShapeRun {
            start_col: 2,
            cells: vec![ShapeCell {
                ch: 'M',
                combining: vec!['\u{301}'],
                style,
            }],
            cell_render: vec![CellRenderInfo {
                color: [1, 2, 3, 255],
                cursor: false,
            }],
        };
        // Two glyphs sharing cluster 0: the base, and a stand-in "mark"
        // glyph (reusing a real, rasterizable glyph id so it isn't
        // filtered as empty) offset from it.
        let shaped = vec![
            ShapedGlyph {
                glyph_id: base.glyph_id,
                face_id: base.face_id,
                x_advance: base.x_advance,
                x_offset: 0,
                y_offset: 0,
                cluster: 0,
            },
            ShapedGlyph {
                glyph_id: base.glyph_id,
                face_id: base.face_id,
                x_advance: 0,
                x_offset: 3,
                y_offset: 5,
                cluster: 0,
            },
        ];

        let mut glyph_instances = Vec::new();
        let metrics = font.metrics();
        emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 9, metrics);

        assert_eq!(
            glyph_instances.len(),
            2,
            "both the base and the attached mark glyph must be emitted (attached cluster)"
        );
        assert!(
            glyph_instances.iter().all(|inst| inst.grid_pos == [2, 9]),
            "both glyphs must share the base cell's anchor position"
        );
        let base_bearing = glyph_instances[0].bearing;
        let mark_bearing = glyph_instances[1].bearing;
        assert_eq!(
            mark_bearing[0],
            base_bearing[0] + 3,
            "the mark's x position must come from its own shaped x_offset"
        );
        assert_eq!(
            mark_bearing[1],
            base_bearing[1] - 5,
            "the mark's y position must come from its own shaped y_offset (HarfBuzz y-up -> cell y-down)"
        );
    }

    /// AC-WP2-05 (FM-08 gap-closer): unlike a hand-built `ShapeCell` slice
    /// passed directly to `shape_run`, this exercises the REAL
    /// segmentation -> `shape_run` path (`rebuild_cell_instances`) across 3
    /// consecutive render passes over unchanged terminal content, and
    /// asserts the shape cache keeps hitting from the 2nd pass onward — not
    /// just once.
    #[test]
    fn repeated_render_passes_hit_the_shape_cache_via_the_real_segmentation_path() {
        let Some(mut font) = skip_font() else { return };

        let mut terminal = Terminal::new(GridSize::new(12, 1));
        for (i, ch) in "hello!!==".chars().enumerate() {
            terminal.primary.grid[0].cells[i].ch = ch;
        }
        let snap = FrameSnapshot::from_terminal(&terminal);
        let theme = Theme::new();
        let mut instances = Vec::new();

        rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
        let hits_after_pass_1 = font.shape_cache_hits();

        rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
        let hits_after_pass_2 = font.shape_cache_hits();
        assert!(
            hits_after_pass_2 > hits_after_pass_1,
            "an unchanged frame's 2nd render pass must hit the shape cache \
             (pass1={hits_after_pass_1}, pass2={hits_after_pass_2})"
        );

        rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
        let hits_after_pass_3 = font.shape_cache_hits();
        assert!(
            hits_after_pass_3 > hits_after_pass_2,
            "a 3rd unchanged render pass must ALSO hit the cache, not just the 2nd \
             (pass2={hits_after_pass_2}, pass3={hits_after_pass_3})"
        );
    }
}
