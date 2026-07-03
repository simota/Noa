//! [`Renderer`] — owns the GPU pipeline, the font atlas texture, and the
//! instance buffers; rebuilds them from a [`crate::FrameSnapshot`] and draws.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use noa_core::{CellAttrs, CellSize, Color, GridPadding, PixelSize};
use noa_font::{FontGrid, Metrics, ShapedGlyph};
use noa_grid::{Row, SearchState, Selection, TerminalColors};

use crate::draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
use crate::instance::{CellInstance, PaneUniformParams, populate_pane_uniform};
use crate::pipeline::CellPipeline;
use crate::segment::{SegmentCell, ShapeRun, segment_row};
use crate::snapshot::FrameSnapshot;
use crate::theme::Theme;

const DEFAULT_PANE_ID: PaneId = PaneId::new(0);
const DIVIDER_RGBA: [u8; 4] = [82, 82, 82, 255];
const FOCUS_INDICATOR_RGBA: [u8; 4] = [95, 175, 255, 230];
static RENDERER_CONSTRUCTION_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn renderer_construction_count() -> u64 {
    RENDERER_CONSTRUCTION_COUNT.load(Ordering::Relaxed)
}

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

/// Everything that must be identical between two frames for a pane's
/// per-row instance cache to stay valid (WP4, ADR-R4 / FM-11). Bundled into
/// one `PartialEq`-compared struct on purpose: any single field drifting
/// forces a full rebuild, and a struct comparison is structurally harder to
/// under-implement (forget one field) than N scattered `if` checks.
///
/// `Row.dirty` (consumed via `FrameSnapshot::row_dirty`) only tracks CELL
/// mutations; every field here changes a row's *rendered* instances without
/// necessarily touching a cell, so a difference in any one of them forces
/// every row in the pane dirty for this rebuild. Cursor movement is handled
/// separately (see `PaneRenderCache::prev_cursor`) since it only dirties the
/// two affected rows, not the whole pane.
///
/// Atlas realloc is deliberately NOT part of this key: `etagere`'s packing
/// never moves an already-packed rect, so a realloc changes the atlas
/// texture size but not any existing glyph's `atlas_pos` — retained
/// instances referencing already-rastered glyphs stay valid, and newly
/// rastered glyphs only appear on rows that are dirty by construction.
#[derive(Clone, PartialEq)]
struct FrameInvalidationKey {
    row_base: usize,
    cols: u16,
    rows: u16,
    colors: TerminalColors,
    theme: Theme,
    selection: Option<Selection>,
    search: SearchState,
    cell_size: (f32, f32),
}

/// A pane's persisted per-row instance segments (WP4, REQ-PERF-2/3). Three
/// row-indexed vectors — NOT a per-row `[bg, glyph, deco]` grouping — so the
/// flatten step can reproduce the existing GLOBAL 3-pass order (all bg, then
/// all glyph, then all decoration) that a glyph descender overflowing into
/// the row below depends on for correct paint order (FM-12).
struct PaneRenderCache {
    bg: Vec<Vec<CellInstance>>,
    glyph: Vec<Vec<CellInstance>>,
    deco: Vec<Vec<CellInstance>>,
    key: Option<FrameInvalidationKey>,
    /// `(cursor.x, cursor.y, cursor.visible)` as of the last rebuild — used
    /// only to detect cursor movement, which dirties exactly the two
    /// affected rows (not a full-pane invalidation trigger).
    prev_cursor: Option<(u16, u16, bool)>,
}

impl PaneRenderCache {
    fn empty() -> Self {
        PaneRenderCache {
            bg: Vec::new(),
            glyph: Vec::new(),
            deco: Vec::new(),
            key: None,
            prev_cursor: None,
        }
    }
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
    target_format: wgpu::TextureFormat,
    target_format_is_srgb: bool,
    mask_atlas_seen_generation: u64,
    color_atlas_seen_generation: u64,
    /// Per-pane row-instance cache for dirty-row diffing (WP4), keyed by the
    /// pane's stable render-side identity so it survives split reordering
    /// across frames.
    pane_render_cache: HashMap<PaneId, PaneRenderCache>,
    /// Total rows regenerated across all panes in the most recent
    /// `rebuild_panes` call (AC-WP4-02).
    rows_rebuilt_last_frame: u64,
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
        RENDERER_CONSTRUCTION_COUNT.fetch_add(1, Ordering::Relaxed);
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
            target_format: format,
            target_format_is_srgb: format.is_srgb(),
            mask_atlas_seen_generation,
            color_atlas_seen_generation,
            pane_render_cache: HashMap::new(),
            rows_rebuilt_last_frame: 0,
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

    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.target_format
    }

    /// Per-pane bind-group rebuild counters, exposed for headless pipeline
    /// tests that assert atlas reallocation refreshes every pane binding.
    pub fn pane_bind_group_rebuild_counts(&self) -> Vec<u64> {
        self.pane_gpu
            .iter()
            .map(|pane| pane.bind_group_rebuilds)
            .collect()
    }

    /// Total rows regenerated across all panes in the most recent
    /// `rebuild_panes` (or `rebuild_cells`) call — 0 when every visible
    /// row's cached instances were reused unchanged (AC-WP4-02).
    pub fn rows_rebuilt_last_frame(&self) -> u64 {
        self.rows_rebuilt_last_frame
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
    ///
    /// WP4 (REQ-PERF-2/3): each pane keeps a [`PaneRenderCache`] of its
    /// per-row bg/glyph/decoration instance segments across frames. A row is
    /// only regenerated when `FrameSnapshot::row_dirty` says it changed, the
    /// cursor moved into or out of it, or a pane-wide invalidation trigger
    /// fired (see [`FrameInvalidationKey`]) — an unchanged row costs zero
    /// instance-rebuild work. The final instance list is still flattened in
    /// the exact bg-then-glyph-then-decoration GLOBAL order the shader relies
    /// on (FM-12): a glyph descender overflowing into the row below must
    /// blend over that row's background, which only holds if every row's bg
    /// instance precedes every row's glyph instance across the whole pane.
    pub fn rebuild_panes(&mut self, panes: &[PaneFrame<'_>], font: &mut FontGrid, theme: &Theme) {
        self.instances.clear();
        self.pane_instances.clear();
        self.pane_layout.clear();
        self.divider_range = 0..0;

        let mut first_clear_color = None;
        let mut rows_rebuilt_total: u64 = 0;
        for pane in panes {
            let start = self.instances.len() as u32;
            let cache = self
                .pane_render_cache
                .entry(pane.pane)
                .or_insert_with(PaneRenderCache::empty);
            let (clear_color, cell_size, rows_rebuilt) = rebuild_pane_cached(
                cache,
                &mut self.instances,
                pane.snapshot,
                font,
                theme,
                self.target_format_is_srgb,
            );
            rows_rebuilt_total += rows_rebuilt;
            first_clear_color.get_or_insert(clear_color);
            self.cell_size = cell_size;
            let end = self.instances.len() as u32;
            self.pane_instances.push(PaneInstances {
                pane: pane.pane,
                range: start..end,
            });
            self.pane_layout.push((pane.pane, pane.rect));
        }

        // Drop caches for panes that are no longer visible (split closed) so
        // this map doesn't grow unbounded over a long session.
        self.pane_render_cache
            .retain(|id, _| panes.iter().any(|pane| pane.pane == *id));

        if let Some(clear_color) = first_clear_color {
            self.clear_color = clear_color;
        }
        self.cell_instance_len = self.instances.len();
        self.rows_rebuilt_last_frame = rows_rebuilt_total;
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

    /// Test-only window into the built instance list (AC-WP4-03, FM-16):
    /// lets unit tests compare the per-row-patched output against a full
    /// rebuild's output without needing a full draw/readback round trip.
    #[cfg(test)]
    fn instances_for_test(&self) -> &[CellInstance] {
        &self.instances
    }

    /// Test-only accessor for the cell/overlay instance boundary (FM-16).
    #[cfg(test)]
    fn cell_instance_len_for_test(&self) -> usize {
        self.cell_instance_len
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

/// Full (always-rebuild-every-row) instance build, used directly by unit
/// tests that exercise the per-cell/per-glyph logic in isolation and as the
/// reference path `PaneRenderCache`'s per-row patching must stay
/// output-identical to (AC-WP4-03). `Renderer::rebuild_panes` does not call
/// this — it drives [`rebuild_row_instances`] per pane through
/// [`rebuild_pane_cached`] instead, so unchanged rows can be skipped.
/// `cfg(test)`-only: nothing in the non-test build calls it anymore.
#[cfg(test)]
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

    let mut bg_rows = Vec::with_capacity(snap.rows.len());
    let mut glyph_rows = Vec::with_capacity(snap.rows.len());
    let mut deco_rows = Vec::with_capacity(snap.rows.len());
    for (row_idx, row) in snap.rows.iter().enumerate() {
        let (bg, glyph, deco) = rebuild_row_instances(
            row_idx as u16,
            row,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        bg_rows.push(bg);
        glyph_rows.push(glyph);
        deco_rows.push(deco);
    }

    instances.clear();
    flatten_row_segments(instances, &bg_rows, &glyph_rows, &deco_rows);

    (clear_color, (metrics.cell_w, metrics.cell_h))
}

/// Build one row's background / glyph / decoration instance segments. Pure
/// function of `(y, row, snap, ...)` — no cross-row state — which is what
/// makes per-row caching in [`PaneRenderCache`] safe: a clean row's segments
/// from a previous frame are byte-identical to what this function would
/// produce again, because nothing here reaches outside the row.
fn rebuild_row_instances(
    y: u16,
    row: &Row,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) -> (Vec<CellInstance>, Vec<CellInstance>, Vec<CellInstance>) {
    let mut bg_instances = Vec::new();
    let mut glyph_instances = Vec::new();
    let mut decoration_instances = Vec::new();
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

    (bg_instances, glyph_instances, decoration_instances)
}

/// Concatenate row-indexed bg/glyph/decoration segments in the GLOBAL
/// bg-then-glyph-then-decoration order every row depends on (FM-12): a
/// glyph descender from row `r` can overflow into row `r+1`'s space and
/// must blend OVER row `r+1`'s background, which only holds if EVERY row's
/// bg instance precedes EVERY row's glyph instance in the flattened list.
/// Grouping instances per-row (`[row0: bg,glyph,deco, row1: ...]`) would
/// break this and is NOT a valid alternative here.
fn flatten_row_segments(
    instances: &mut Vec<CellInstance>,
    bg_rows: &[Vec<CellInstance>],
    glyph_rows: &[Vec<CellInstance>],
    deco_rows: &[Vec<CellInstance>],
) {
    for row in bg_rows {
        instances.extend_from_slice(row);
    }
    for row in glyph_rows {
        instances.extend_from_slice(row);
    }
    for row in deco_rows {
        instances.extend_from_slice(row);
    }
}

/// WP4 (REQ-PERF-2/3): rebuild `cache`'s per-row segments against `snap`,
/// regenerating only dirty rows, then append the flattened result to
/// `instances` (the caller owns clearing `instances` once per frame across
/// all panes). Returns `(clear_color, cell_size, rows_rebuilt)`.
fn rebuild_pane_cached(
    cache: &mut PaneRenderCache,
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
) -> ([f32; 4], (f32, f32), u64) {
    let metrics = font.metrics();
    let clear_color = surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    );
    let cell_size = (metrics.cell_w, metrics.cell_h);

    let new_key = FrameInvalidationKey {
        row_base: snap.row_base,
        cols: snap.cols,
        rows: snap.rows_n,
        colors: snap.colors.clone(),
        theme: theme.clone(),
        selection: snap.selection,
        search: snap.search.clone(),
        cell_size,
    };

    let rows = snap.rows.len();
    let new_cursor = (snap.cursor.x, snap.cursor.y, snap.cursor.visible);
    // Any of the 6 pane-wide triggers (row_base/cols/rows/colors/theme/
    // selection/search — bundled in `FrameInvalidationKey`) differing from
    // the cached previous-frame key forces every row dirty. A pane's first
    // frame (`cache.key` still `None`) is also a full rebuild.
    let full = cache.key.as_ref() != Some(&new_key) || cache.bg.len() != rows;

    let mut dirty: Vec<bool> = if full {
        vec![true; rows]
    } else {
        snap.row_dirty.clone()
    };

    // 7th trigger (narrower than the 6 above): cursor movement dirties
    // EXACTLY the two affected rows, not the whole pane.
    if !full
        && let Some(prev) = cache.prev_cursor
        && prev != new_cursor
    {
        if let Some(slot) = dirty.get_mut(prev.1 as usize) {
            *slot = true;
        }
        if let Some(slot) = dirty.get_mut(new_cursor.1 as usize) {
            *slot = true;
        }
    }

    if full {
        cache.bg = vec![Vec::new(); rows];
        cache.glyph = vec![Vec::new(); rows];
        cache.deco = vec![Vec::new(); rows];
    }

    let mut rows_rebuilt: u64 = 0;
    for (row_idx, row) in snap.rows.iter().enumerate() {
        if dirty.get(row_idx).copied().unwrap_or(true) {
            let (bg, glyph, deco) = rebuild_row_instances(
                row_idx as u16,
                row,
                snap,
                font,
                theme,
                target_format_is_srgb,
                metrics,
            );
            cache.bg[row_idx] = bg;
            cache.glyph[row_idx] = glyph;
            cache.deco[row_idx] = deco;
            rows_rebuilt += 1;
        }
    }

    flatten_row_segments(instances, &cache.bg, &cache.glyph, &cache.deco);

    cache.key = Some(new_key);
    cache.prev_cursor = Some(new_cursor);

    (clear_color, cell_size, rows_rebuilt)
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
    use noa_grid::{Cell, Cursor, SearchMatch, SelectionPoint, Terminal};

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
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
        let snap = FrameSnapshot::from_terminal(&mut terminal);

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
        let snap = FrameSnapshot::from_terminal(&mut terminal);
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

    // ── WP4: dirty-row diffing ────────────────────────────────────────

    /// A 3-row, 1-col snapshot with distinct content per row and every
    /// pane-wide field at a neutral baseline. `row_dirty` is all-false —
    /// on a second call through the SAME pane cache this represents "no
    /// row-level cell mutation happened," isolating whichever single
    /// pane-wide field a FM-11 sub-case varies.
    fn baseline_snapshot(chars: [char; 3]) -> FrameSnapshot {
        let rows = chars
            .into_iter()
            .map(|ch| Row {
                cells: vec![Cell {
                    ch,
                    ..Cell::default()
                }],
                wrapped: false,
                dirty: false,
            })
            .collect();
        FrameSnapshot {
            rows,
            row_dirty: vec![false, false, false],
            cursor: Cursor::default(),
            colors: TerminalColors::default(),
            selection: None,
            search: SearchState::default(),
            row_base: 0,
            cols: 1,
            rows_n: 3,
        }
    }

    #[test]
    fn rebuild_panes_reports_zero_rows_rebuilt_when_nothing_changed() {
        // AC-WP4-02 (REQ-PERF-2): a frame in which no row changed since the
        // last rebuild produces a rows_rebuilt count of exactly 0.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping rows_rebuilt zero-count test");
            return;
        };
        let Some(mut font) = skip_font() else { return };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer");
        renderer.resize(PixelSize { w: 64, h: 64 });

        let mut terminal = Terminal::new(GridSize::new(4, 2));
        terminal.primary.grid[0].cells[0].ch = 'A';
        let theme = Theme::new();
        let pane = PaneId::new(1);
        let rect = PaneRect::new(0, 0, 64, 64);

        let snap1 = FrameSnapshot::from_terminal(&mut terminal);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap1,
            }],
            &mut font,
            &theme,
        );
        assert!(
            renderer.rows_rebuilt_last_frame() > 0,
            "the first frame through a fresh cache must rebuild at least one row"
        );

        // `from_terminal` already cleared the grid's dirty bits when snap1
        // was taken; the terminal has not been mutated since, so this
        // second snapshot reports every row clean.
        let snap2 = FrameSnapshot::from_terminal(&mut terminal);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap2,
            }],
            &mut font,
            &theme,
        );
        assert_eq!(
            renderer.rows_rebuilt_last_frame(),
            0,
            "an unchanged second frame must rebuild zero rows"
        );
    }

    #[test]
    fn per_row_patch_output_matches_a_full_rebuild_ac_wp4_03() {
        // AC-WP4-03 (REQ-PERF-3): identical terminal state rendered once via
        // a full rebuild and once via the per-row patch path must produce
        // an IDENTICAL instance list.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping AC-WP4-03 identical-output test");
            return;
        };
        let Some(mut font) = skip_font() else { return };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer");
        renderer.resize(PixelSize { w: 64, h: 64 });

        let theme = Theme::new();
        let pane = PaneId::new(1);
        let rect = PaneRect::new(0, 0, 64, 64);

        let mut terminal = Terminal::new(GridSize::new(4, 3));
        terminal.primary.grid[0].cells[0].ch = 'A';
        terminal.primary.grid[1].cells[0].ch = 'B';
        terminal.primary.grid[2].cells[0].ch = 'C';

        // First frame: fresh cache -> full rebuild.
        let snap1 = FrameSnapshot::from_terminal(&mut terminal);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap1,
            }],
            &mut font,
            &theme,
        );

        // Mutate ONE row only, so the second frame is a genuine per-row
        // patch: rows 0 and 2 are reused untouched from the cache, only
        // row 1 regenerates. Direct field mutation bypasses the real
        // cell-mutating paths that set `Row::dirty` (e.g. `Screen::print`),
        // so mark it explicitly — mirrors how `noa-render/tests/pipeline.rs`
        // constructs `Row { dirty: true, .. }` literals directly.
        terminal.primary.grid[1].cells[0].ch = 'X';
        terminal.primary.grid[1].dirty = true;
        let snap2 = FrameSnapshot::from_terminal(&mut terminal);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap2,
            }],
            &mut font,
            &theme,
        );
        assert_eq!(
            renderer.rows_rebuilt_last_frame(),
            1,
            "only the mutated row should have been rebuilt on the second frame"
        );
        let patched = renderer.instances_for_test().to_vec();

        // Reference: an unconditional full rebuild of the SAME
        // (post-mutation) state via the always-full free function.
        let mut reference = Vec::new();
        rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);

        assert_eq!(
            patched, reference,
            "the per-row-patched instance list must be byte-identical to a full \
             rebuild of the same state (bg-then-glyph-then-decoration GLOBAL order, FM-12)"
        );
    }

    #[test]
    fn pane_wide_invalidation_triggers_are_covered_fm11() {
        // FM-11: each of the 6 pane-wide triggers bundled into
        // `FrameInvalidationKey` must force EVERY row in the pane dirty when
        // it differs from the previous frame, even though `row_dirty` says
        // no cell changed. Cursor movement (the narrower 7th case) instead
        // dirties exactly the two affected rows.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping FM-11 trigger table test");
            return;
        };
        let Some(mut font) = skip_font() else { return };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer");
        renderer.resize(PixelSize { w: 64, h: 64 });

        let theme = Theme::new();
        let rect = PaneRect::new(0, 0, 64, 64);

        // Each sub-case gets its own PaneId so it starts from a fresh cache
        // without needing a fresh Renderer (cheaper: one GPU device for the
        // whole table).
        let mut rebuild_twice = |pane_id: u64,
                                 snap_a: &FrameSnapshot,
                                 theme_a: &Theme,
                                 snap_b: &FrameSnapshot,
                                 theme_b: &Theme| {
            let pane = PaneId::new(pane_id);
            renderer.rebuild_panes(
                &[PaneFrame {
                    pane,
                    rect,
                    snapshot: snap_a,
                }],
                &mut font,
                theme_a,
            );
            renderer.rebuild_panes(
                &[PaneFrame {
                    pane,
                    rect,
                    snapshot: snap_b,
                }],
                &mut font,
                theme_b,
            );
            renderer.rows_rebuilt_last_frame()
        };

        // 1. row_base (viewport scroll offset).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.row_base = 1;
            let rebuilt = rebuild_twice(101, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(rebuilt, 3, "row_base change must force a full pane rebuild");
        }

        // 2a. cols (resize).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.cols = 2;
            let rebuilt = rebuild_twice(102, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(rebuilt, 3, "cols change must force a full pane rebuild");
        }

        // 2b. rows_n (resize).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.rows_n = 4;
            let rebuilt = rebuild_twice(103, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(rebuilt, 3, "rows_n change must force a full pane rebuild");
        }

        // 3. colors (terminal palette override).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            let mut colors = TerminalColors::default();
            colors.set_default_fg(Rgb::new(9, 9, 9));
            snap_b.colors = colors;
            let rebuilt = rebuild_twice(104, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 3,
                "a terminal color override change must force a full pane rebuild"
            );
        }

        // 4. active Theme identity.
        {
            let snap = baseline_snapshot(['A', 'B', 'C']);
            let mut theme_b = Theme::new();
            theme_b.default_fg = Rgb::new(5, 6, 7);
            let rebuilt = rebuild_twice(105, &snap, &theme, &snap, &theme_b);
            assert_eq!(rebuilt, 3, "a theme swap must force a full pane rebuild");
        }

        // 5. selection state.
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.selection = Some(Selection::new(
                SelectionPoint::new(0, 0),
                SelectionPoint::new(0, 0),
            ));
            let rebuilt = rebuild_twice(106, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 3,
                "a selection change must force a full pane rebuild"
            );
        }

        // 6. search state (active-match / search-match spans).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            let mut search = SearchState::default();
            search.set_query(
                "A".to_string(),
                vec![SearchMatch {
                    start: SelectionPoint::new(0, 0),
                    end: SelectionPoint::new(0, 0),
                }],
            );
            snap_b.search = search;
            let rebuilt = rebuild_twice(107, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 3,
                "a search-state change must force a full pane rebuild"
            );
        }

        // 7. cursor movement — the narrower case: dirties exactly the two
        // affected rows, NOT a full-pane invalidation.
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.cursor.y = 2;
            let rebuilt = rebuild_twice(108, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 2,
                "cursor movement must dirty exactly the two affected rows, not the whole pane"
            );
        }
    }

    #[test]
    fn overlay_boundary_stays_correct_after_a_per_row_patched_rebuild_fm16() {
        // FM-16: the cell/overlay instance boundary (`cell_instance_len`)
        // must still be computed correctly — and overlay instances must
        // still land at the right offset — after a rebuild that only
        // per-row-patched some rows instead of doing a full rebuild.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping FM-16 overlay boundary test");
            return;
        };
        let Some(mut font) = skip_font() else { return };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut font,
            GridPadding::ZERO,
        )
        .expect("build renderer");
        renderer.resize(PixelSize { w: 128, h: 64 });

        let theme = Theme::new();
        let pane_a = PaneId::new(1);
        let pane_b = PaneId::new(2);
        let rect_a = PaneRect::new(0, 0, 64, 64);
        let rect_b = PaneRect::new(65, 0, 63, 64);

        let mut term_a = Terminal::new(GridSize::new(4, 2));
        term_a.primary.grid[0].cells[0].ch = 'A';
        let mut term_b = Terminal::new(GridSize::new(4, 2));
        term_b.primary.grid[0].cells[0].ch = 'Z';

        let snap_a1 = FrameSnapshot::from_terminal(&mut term_a);
        let snap_b1 = FrameSnapshot::from_terminal(&mut term_b);
        renderer.rebuild_panes(
            &[
                PaneFrame {
                    pane: pane_a,
                    rect: rect_a,
                    snapshot: &snap_a1,
                },
                PaneFrame {
                    pane: pane_b,
                    rect: rect_b,
                    snapshot: &snap_b1,
                },
            ],
            &mut font,
            &theme,
        ); // full first frame for both panes

        // Mutate one row in pane A only -> the next rebuild is a genuine
        // per-row patch (pane B rebuilds zero rows; pane A rebuilds one).
        // Direct field mutation bypasses `Screen::print`'s `dirty = true`,
        // so mark it explicitly (see the AC-WP4-03 test above for detail).
        term_a.primary.grid[1].cells[0].ch = 'B';
        term_a.primary.grid[1].dirty = true;
        let snap_a2 = FrameSnapshot::from_terminal(&mut term_a);
        let snap_b2 = FrameSnapshot::from_terminal(&mut term_b);
        renderer.rebuild_panes(
            &[
                PaneFrame {
                    pane: pane_a,
                    rect: rect_a,
                    snapshot: &snap_a2,
                },
                PaneFrame {
                    pane: pane_b,
                    rect: rect_b,
                    snapshot: &snap_b2,
                },
            ],
            &mut font,
            &theme,
        );
        assert_eq!(
            renderer.rows_rebuilt_last_frame(),
            1,
            "only pane A's single mutated row should rebuild"
        );

        let layout = [(pane_a, rect_a), (pane_b, rect_b)];
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-fm16-test-target"),
            size: wgpu::Extent3d {
                width: 128,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let cell_instance_len_before = renderer.cell_instance_len_for_test();
        assert_eq!(
            cell_instance_len_before,
            renderer.instances_for_test().len(),
            "cell_instance_len must equal the instance list length right after rebuild_panes \
             (no overlay appended yet)"
        );

        renderer.draw_panes(&device, &queue, &view, &layout, Some(pane_a), None);

        let all_instances = renderer.instances_for_test();
        assert!(
            all_instances.len() > cell_instance_len_before,
            "draw_panes over two panes with a focused pane must append at least one \
             overlay (divider/focus) instance past the cell-instance boundary"
        );
        for inst in &all_instances[cell_instance_len_before..] {
            assert_eq!(
                inst.flags,
                CellInstance::FLAG_DIVIDER,
                "every instance appended past cell_instance_len must be an overlay \
                 (divider/focus) quad, not leftover or corrupted cell data"
            );
        }
    }
}
