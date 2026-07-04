//! [`Renderer`] — owns the GPU pipeline, the font atlas texture, and the
//! instance buffers; rebuilds them from a [`crate::FrameSnapshot`] and draws.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use noa_core::{CellAttrs, CellSize, Color, GridPadding, PixelSize};
use noa_font::{FontGrid, Metrics, ShapedGlyph};
use noa_grid::{Cell, CursorStyle, PLACEHOLDER, Row, SearchState, Selection, TerminalColors};
use unicode_width::UnicodeWidthChar;

use crate::draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
use crate::image_layer::{ImageDraw, ImageLayer};
use crate::instance::{CellInstance, PaneUniformParams, populate_pane_uniform};
use crate::pipeline::CellPipeline;
use crate::segment::{SegmentCell, ShapeRun, segment_row};
use crate::snapshot::{FrameSnapshot, HoverLink, ImagePlacementSnapshot, SnapshotImage};
use crate::theme::{OverlayStyle, Theme, rgba};

const DEFAULT_PANE_ID: PaneId = PaneId::new(0);
const DIVIDER_RGBA: [u8; 4] = [82, 82, 82, 255];
const FOCUS_INDICATOR_RGBA: [u8; 4] = [95, 175, 255, 230];
const MAX_ATLAS_EVICTION_REBUILD_PASSES: usize = 4;
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
    /// Absolute instance index where this pane's background quads end and its
    /// glyph+decoration quads begin — the boundary between image band 0 and
    /// band 1 (design Step R).
    bg_end: u32,
    /// Absolute instance index where this pane's glyph+decoration quads end and
    /// its UI-overlay quads (search prompt / palette / dialog) begin — the
    /// boundary between image band 1 and band 2, and above which no image ever
    /// draws (UI overlays stay on top).
    text_end: u32,
}

/// One pane's kitty-graphics inputs, retained across `rebuild_panes` →
/// `draw_panes` so the image quads can be resolved against the draw-time pane
/// rect (the snapshot itself is not kept past rebuild).
struct PaneImages {
    pane: PaneId,
    placements: Vec<ImagePlacementSnapshot>,
    images: Vec<SnapshotImage>,
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
/// Atlas growth is deliberately NOT part of this key: `etagere`'s packing
/// never moves an already-packed rect, so growth changes the atlas texture
/// size but not any existing glyph's `atlas_pos`. Atlas eviction IS part of
/// the key: row cache entries hold concrete atlas coordinates, and eviction
/// makes those coordinates reusable for another glyph.
#[derive(Clone, PartialEq)]
struct FrameInvalidationKey {
    /// Session-absolute viewport base row (`FrameSnapshot::abs_row_base`), NOT
    /// the storage-index `row_base`: the latter repeats across equal push/evict
    /// counts, so keying on it would cache-hit stale history rows (the renderer
    /// reports history rows as never-dirty and relies on this key to catch a
    /// scroll). The absolute row is monotonic, so a scroll always changes it.
    abs_row_base: usize,
    /// Primary/alternate screen identity. DEC private modes can switch the
    /// backing screen without dirtying the newly active screen's rows.
    active_is_alt: bool,
    cols: u16,
    rows: u16,
    colors: TerminalColors,
    theme: Theme,
    selection: Option<Selection>,
    search: SearchState,
    cell_size: (f32, f32),
    /// Cmd+hover has no corresponding `Row::dirty` bit (it changes with the
    /// mouse/modifier state, not terminal output), so it rides the same
    /// full-pane-invalidation bundle as the other 6 pane-wide triggers
    /// above rather than tracking affected rows individually.
    hover_link: Option<HoverLink>,
    /// Monotonic [`FontGrid`] atlas-eviction epoch. A changed epoch means at
    /// least one cached glyph atlas coordinate may now refer to reclaimed
    /// space, so every row must regenerate its glyph instances.
    atlas_eviction_generation: u64,
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
    flat: Vec<CellInstance>,
    key: Option<FrameInvalidationKey>,
    /// `(cursor.x, cursor.y, cursor.visible, cursor.style, focused,
    /// cursor_blink_visible)` as of the last rebuild — used only to detect
    /// a change to the cursor's position OR its rendered shape (movement,
    /// DECSCUSR style, focus, or blink phase), which dirties exactly the
    /// two affected rows (not a full-pane invalidation trigger).
    prev_cursor: Option<(u16, u16, bool, CursorStyle, bool, bool)>,
}

impl PaneRenderCache {
    fn empty() -> Self {
        PaneRenderCache {
            bg: Vec::new(),
            glyph: Vec::new(),
            deco: Vec::new(),
            flat: Vec::new(),
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
    image_layer: ImageLayer,
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
    /// Per-pane kitty-graphics inputs for the most recent `rebuild_panes`,
    /// resolved into image quads at draw time (parallel to `pane_layout`).
    pane_images: Vec<PaneImages>,
    pane_layout: Vec<(PaneId, PaneRect)>,
    divider_range: Range<u32>,
    focus_indicator_range: Range<u32>,
    viewport: PixelSize,
    cell_size: (f32, f32),
    grid_padding: GridPadding,
    clear_color: [f32; 4],
    target_format: wgpu::TextureFormat,
    target_format_is_srgb: bool,
    mask_atlas_seen_identity: u64,
    mask_atlas_seen_generation: u64,
    color_atlas_seen_identity: u64,
    color_atlas_seen_generation: u64,
    /// Per-pane row-instance cache for dirty-row diffing (WP4), keyed by the
    /// pane's stable render-side identity so it survives split reordering
    /// across frames.
    pane_render_cache: HashMap<PaneId, PaneRenderCache>,
    /// Total rows regenerated across all panes in the most recent
    /// `rebuild_panes` call (AC-WP4-02).
    rows_rebuilt_last_frame: u64,
    /// `background-opacity` (0.0..=1.0). Scales only the clear-color alpha,
    /// which is what shows through wherever the terminal's DEFAULT background
    /// is visible (default-bg cells emit no bg quad, and the window padding is
    /// filled by the clear too). Explicit-bg / selection / cursor quads keep
    /// their own alpha (1.0), so they stay opaque — Ghostty semantics.
    background_opacity: f32,
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
        let image_layer = ImageLayer::new(device, format);

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
        let mask_atlas_seen_identity = font.atlas_identity();
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
        let color_atlas_seen_identity = font.atlas_identity();
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
            image_layer,
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
            pane_images: Vec::new(),
            pane_layout: Vec::new(),
            divider_range: 0..0,
            focus_indicator_range: 0..0,
            viewport: PixelSize { w: 0, h: 0 },
            cell_size: (metrics.cell_w, metrics.cell_h),
            grid_padding,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            target_format: format,
            target_format_is_srgb: format.is_srgb(),
            mask_atlas_seen_identity,
            mask_atlas_seen_generation,
            color_atlas_seen_identity,
            color_atlas_seen_generation,
            pane_render_cache: HashMap::new(),
            rows_rebuilt_last_frame: 0,
            background_opacity: 1.0,
        })
    }

    /// Set `background-opacity` (0.0..=1.0). A startup-time setting: `noa-app`
    /// calls this once right after construction, before the first frame, so no
    /// per-frame cache invalidation is needed (opacity feeds only the clear
    /// color, which `rebuild_panes` recomputes every frame anyway). Values are
    /// clamped into range.
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.background_opacity = opacity.clamp(0.0, 1.0);
    }

    /// Update the known viewport size (called on `WindowEvent::Resized`).
    pub fn resize(&mut self, px: PixelSize) {
        self.viewport = px;
    }

    /// Force the background clear color used by the next [`Renderer::draw`],
    /// overriding the value [`Renderer::rebuild_cells`] derives from the
    /// snapshot's default background. Call this **after** `rebuild_cells`
    /// (which resets `clear_color`) — the Session Overview uses it to paint a
    /// title-bar tile in a distinct band color while its glyphs still draw
    /// over that band, since cells matching the default background emit no
    /// background quad and so inherit the clear color.
    pub fn set_clear_color(&mut self, color: [f32; 4]) {
        self.clear_color = color;
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
        self.pane_images.clear();
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
            let PaneRebuild {
                clear_color,
                cell_size,
                rows_rebuilt,
                bg_len,
                text_len,
            } = rebuild_pane_cached(
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
                bg_end: start + bg_len,
                text_end: start + text_len,
            });
            if !pane.snapshot.image_placements.is_empty() {
                self.pane_images.push(PaneImages {
                    pane: pane.pane,
                    placements: pane.snapshot.image_placements.clone(),
                    images: pane.snapshot.images.clone(),
                });
            }
            self.pane_layout.push((pane.pane, pane.rect));
        }

        // Drop caches for panes that are no longer visible (split closed) so
        // this map doesn't grow unbounded over a long session.
        self.pane_render_cache
            .retain(|id, _| panes.iter().any(|pane| pane.pane == *id));

        if let Some(clear_color) = first_clear_color {
            self.clear_color = apply_background_opacity(clear_color, self.background_opacity);
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
        let image_draws = self.build_frame_image_draws(device, queue, layout);

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

            for op in &plan {
                match op {
                    DrawOp::Clear => {}
                    DrawOp::PaneCells {
                        pane,
                        scissor,
                        bind_group_index,
                    } => {
                        let Some(entry) = self.pane_instances.iter().find(|e| e.pane == *pane)
                        else {
                            continue;
                        };
                        if scissor.w == 0 || scissor.h == 0 {
                            continue;
                        }
                        let Some(gpu) = self.pane_gpu.get(*bind_group_index) else {
                            continue;
                        };
                        let range = entry.range.clone();
                        let (bg_end, text_end) = (entry.bg_end, entry.text_end);
                        pass.set_scissor_rect(scissor.x, scissor.y, scissor.w, scissor.h);

                        // Interleave the pane's image quads with its cell passes
                        // in three z bands (design Step R): band 0 under the
                        // background, band 1 between background and text, band 2
                        // over text but under the UI overlays.
                        let bands = image_draws
                            .iter()
                            .find(|(id, _)| id == pane)
                            .map(|(_, b)| b);
                        if let Some(b) = bands {
                            self.image_layer.draw_band(&mut pass, &b[0]);
                        }
                        self.draw_cell_range(&mut pass, &gpu.bind_group, range.start..bg_end);
                        if let Some(b) = bands {
                            self.image_layer.draw_band(&mut pass, &b[1]);
                        }
                        self.draw_cell_range(&mut pass, &gpu.bind_group, bg_end..text_end);
                        if let Some(b) = bands {
                            self.image_layer.draw_band(&mut pass, &b[2]);
                        }
                        self.draw_cell_range(&mut pass, &gpu.bind_group, text_end..range.end);
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

        queue.submit(Some(encoder.finish()));
    }

    /// Draw one contiguous cell-instance subrange for a pane. A no-op for an
    /// empty range; sets the cell pipeline + instance buffer each time so it is
    /// safe to call after an image band switched the bound pipeline.
    fn draw_cell_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        bind_group: &wgpu::BindGroup,
        range: Range<u32>,
    ) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.cell.pipeline);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, range);
    }

    /// Upload this frame's images and build every pane's per-band image draw
    /// resources, returned paired with each pane id. Empty when no pane carries
    /// a kitty-graphics placement, so the common (image-free) path allocates
    /// nothing.
    fn build_frame_image_draws(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &[(PaneId, PaneRect)],
    ) -> Vec<(PaneId, [Vec<ImageDraw>; 3])> {
        if self.pane_images.is_empty() {
            return Vec::new();
        }
        self.image_layer.begin_frame();
        for pane_img in &self.pane_images {
            self.image_layer
                .upload_pane_images(device, queue, pane_img.pane, &pane_img.images);
        }
        let mut out = Vec::with_capacity(self.pane_images.len());
        for pane_img in &self.pane_images {
            let Some((_, rect)) = layout.iter().find(|(id, _)| *id == pane_img.pane) else {
                continue;
            };
            let draws = self.image_layer.build_pane_draws(
                device,
                queue,
                pane_img.pane,
                &pane_img.placements,
                &pane_img.images,
                *rect,
                self.grid_padding,
                self.cell_size,
                self.viewport,
            );
            out.push((pane_img.pane, draws));
        }
        self.image_layer.evict();
        out
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
            &mut self.mask_atlas_seen_identity,
            &mut self.mask_atlas_seen_generation,
            AtlasSyncInput {
                data: font.mask_atlas_data(),
                size: font.mask_atlas_size(),
                identity: font.atlas_identity(),
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
            &mut self.color_atlas_seen_identity,
            &mut self.color_atlas_seen_generation,
            AtlasSyncInput {
                data: font.color_atlas_data(),
                size: font.color_atlas_size(),
                identity: font.atlas_identity(),
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

    /// Draw a window-absolute overlay subrange (dividers / focus indicator).
    /// Sets the cell pipeline + instance buffer each time so it is safe to call
    /// after an image band switched the bound pipeline, and so it never relies
    /// on a prior `DrawOp` having left the cell pipeline bound (the pane may have
    /// emitted zero cell instances, or the last band drawn was an image band).
    fn draw_pixel_overlay_range(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<u32>) {
        if range.is_empty() || self.viewport.w == 0 || self.viewport.h == 0 {
            return;
        }
        let Some(gpu) = self.pane_gpu.first() else {
            return;
        };
        pass.set_pipeline(&self.cell.pipeline);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.set_scissor_rect(0, 0, self.viewport.w, self.viewport.h);
        pass.set_bind_group(0, &gpu.bind_group, &[]);
        pass.draw(0..6, range);
    }

    /// Lifetime count of image-texture uploads, exposed for the headless test
    /// asserting an epoch bump forces a re-upload (and a repeat frame does not).
    pub fn image_texture_upload_count(&self) -> u64 {
        self.image_layer.upload_count()
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
    append_search_prompt_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
    append_command_palette_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
    append_confirm_dialog_instances(instances, snap, font, theme, target_format_is_srgb, metrics);

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

    // Cursor shape only depends on pane-wide snapshot state (position,
    // DECSCUSR style, focus, blink phase), so it is resolved once per row
    // rather than recomputed per cell.
    let cursor_visual = cursor_visual_for(snap);
    let row_highlights = RowHighlights::new(snap, y, row.cells.len());

    for (col_idx, cell) in row.cells.iter().enumerate() {
        let x = col_idx as u16;
        let highlight = row_highlights.get(col_idx);
        let selected = highlight.selected;
        let active_search = highlight.active_search;
        let search_match = highlight.search_match;
        let cursor_here =
            cursor_visual != CursorVisual::None && snap.cursor.x == x && snap.cursor.y == y;
        // Only the block styles fill the cell and invert the glyph — bar,
        // underline, and the unfocused hollow outline are separate
        // decoration-pass overlays that leave the glyph's own colors alone
        // (REQ-CURSOR-2/3/4).
        let cursor_block_fill = cursor_here && cursor_visual == CursorVisual::Block;

        let inverse = cell.attrs.contains(CellAttrs::INVERSE);
        let (fg_color, bg_color) = if inverse {
            (cell.bg, cell.fg)
        } else {
            (cell.fg, cell.bg)
        };
        let cell_bg_rgb = theme.resolve_rgb_with_colors(bg_color, false, &snap.colors);
        let (bg_rgb, text_base_rgb) = if cursor_block_fill {
            (
                cursor_fill_rgb(theme, snap, fg_color, cell_bg_rgb),
                cell_bg_rgb,
            )
        } else if selected {
            (theme.selection_bg, theme.selection_fg)
        } else if active_search {
            (theme.active_search_bg, theme.active_search_fg)
        } else if search_match {
            (theme.search_bg, theme.search_fg)
        } else {
            (
                cell_bg_rgb,
                theme.resolve_rgb_with_colors(fg_color, true, &snap.colors),
            )
        };

        // Background quad: skip when it's the plain default bg (the
        // clear color already fills that), unless inverted.
        let bg_is_default = matches!(bg_color, Color::Default) && !inverse;
        if cursor_block_fill || selected || active_search || search_match || !bg_is_default {
            let bg = surface_output_rgb(bg_rgb, target_format_is_srgb);
            bg_instances.push(CellInstance {
                glyph_pos: [0, 0],
                glyph_size: [0, 0],
                bearing: [0, 0],
                grid_pos: [x, y],
                color: to_u8_color(bg),
                flags: if cursor_block_fill {
                    CellInstance::FLAG_CURSOR
                } else {
                    0
                },
            });
        }

        let text_rgb = theme.contrast_adjusted_fg(text_base_rgb, bg_rgb);
        let text_color = surface_output_rgb(text_rgb, target_format_is_srgb);

        let invisible = cell.attrs.contains(CellAttrs::INVISIBLE);
        let wide_spacer = cell.attrs.contains(CellAttrs::WIDE_SPACER);
        if !invisible && !wide_spacer {
            let decoration_color = if let Some(color) = cell.underline_color {
                let underline = theme.resolve_rgb_with_colors(color, true, &snap.colors);
                surface_output_rgb(
                    theme.contrast_adjusted_fg(underline, bg_rgb),
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
            if is_hover_link_cell(snap, cell, x, y) {
                push_hover_link_underline(
                    &mut decoration_instances,
                    x,
                    y,
                    to_u8_color(text_color),
                    metrics,
                );
            }
        }

        // Bar / underline / hollow-outline cursor shapes render as extra
        // decoration-pass rects layered on top of the cell's own content
        // (independent of the cell's own INVISIBLE/WIDE_SPACER attrs —
        // the cursor is a UI overlay, not part of the cell's own ink).
        if cursor_here && cursor_visual != CursorVisual::Block {
            let cursor_rgba = surface_output_rgb(
                cursor_fill_rgb(theme, snap, fg_color, bg_rgb),
                target_format_is_srgb,
            );
            push_cursor_decorations(
                &mut decoration_instances,
                x,
                y,
                cursor_visual,
                to_u8_color(cursor_rgba),
                metrics,
            );
        }

        // WP2 (REQ-SHAPE-1/4/6): feed this cell into the row's
        // shape-run segmentation instead of rasterizing it inline here.
        // Invisible cells are forced blank so shaping never produces
        // ink for them (mirrors the old `!invisible` glyph-skip check);
        // a plain blank/wide-spacer cell needs no special-casing here
        // because it naturally rasterizes to an empty glyph, filtered
        // out in `emit_run_glyph_instances`. A Kitty Unicode placeholder
        // cell (`U+10EEEE` + row/column diacritics) is likewise blanked:
        // the image layer draws its image piece, so the placeholder scalar
        // and its diacritics must never rasterize as text.
        let placeholder = cell.ch == PLACEHOLDER;
        let (shape_ch, shape_combining) = if invisible || placeholder {
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
            cursor: cursor_block_fill,
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

#[derive(Clone, Copy, Default)]
struct CellHighlight {
    selected: bool,
    active_search: bool,
    search_match: bool,
}

struct RowHighlights {
    cells: Option<Vec<CellHighlight>>,
}

impl RowHighlights {
    fn new(snap: &FrameSnapshot, y: u16, cols: usize) -> Self {
        if cols == 0 || (snap.selection.is_none() && snap.search.matches().is_empty()) {
            return Self { cells: None };
        }

        let mut cells = vec![CellHighlight::default(); cols];

        let storage_y = snap.row_base + y as usize;
        if let Some(selection) = snap.selection {
            let (start, end) = selection.normalized();
            if start.y <= storage_y && storage_y <= end.y {
                let start_x = if storage_y == start.y { start.x } else { 0 };
                let end_x = if storage_y == end.y { end.x } else { u16::MAX };
                mark_highlight_span(&mut cells, start_x, end_x, |cell| {
                    cell.selected = true;
                });
            }
        }

        for search_match in snap.search.matches() {
            if search_match.start.y == storage_y && search_match.end.y == storage_y {
                mark_highlight_span(
                    &mut cells,
                    search_match.start.x,
                    search_match.end.x,
                    |cell| {
                        cell.search_match = true;
                    },
                );
            }
        }

        if let Some(active) = snap.search.active_match()
            && active.start.y == storage_y
            && active.end.y == storage_y
        {
            mark_highlight_span(&mut cells, active.start.x, active.end.x, |cell| {
                cell.active_search = true;
            });
        }

        Self { cells: Some(cells) }
    }

    fn get(&self, idx: usize) -> CellHighlight {
        self.cells
            .as_ref()
            .and_then(|cells| cells.get(idx))
            .copied()
            .unwrap_or_default()
    }
}

fn mark_highlight_span(
    cells: &mut [CellHighlight],
    start_x: u16,
    end_x: u16,
    mut mark: impl FnMut(&mut CellHighlight),
) {
    let Some(max_x) = cells.len().checked_sub(1) else {
        return;
    };
    let start = usize::from(start_x).min(max_x);
    let end = usize::from(end_x).min(max_x);
    if start > end {
        return;
    }
    for cell in &mut cells[start..=end] {
        mark(cell);
    }
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

/// Result of [`rebuild_pane_cached`]: the pane's clear color, cell size, how
/// many rows were regenerated, and the two z-band boundary offsets (relative to
/// the pane's appended instance range) the image layer interleaves at.
struct PaneRebuild {
    clear_color: [f32; 4],
    cell_size: (f32, f32),
    rows_rebuilt: u64,
    /// Number of background instances (offset where band 0 → band 1 splits).
    bg_len: u32,
    /// Number of background + glyph + decoration instances, i.e. the offset
    /// where the pane's UI-overlay instances begin (band 1 → band 2 split).
    text_len: u32,
}

/// WP4 (REQ-PERF-2/3): rebuild `cache`'s per-row segments against `snap`,
/// regenerating only dirty rows, then append the flattened result to
/// `instances` (the caller owns clearing `instances` once per frame across
/// all panes).
fn rebuild_pane_cached(
    cache: &mut PaneRenderCache,
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
) -> PaneRebuild {
    let metrics = font.metrics();
    let clear_color = surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    );
    let cell_size = (metrics.cell_w, metrics.cell_h);

    let mut new_key = FrameInvalidationKey {
        abs_row_base: snap.abs_row_base,
        active_is_alt: snap.active_is_alt,
        cols: snap.cols,
        rows: snap.rows_n,
        colors: snap.colors.clone(),
        theme: theme.clone(),
        selection: snap.selection,
        search: snap.search.clone(),
        cell_size,
        hover_link: snap.hover_link,
        atlas_eviction_generation: font.atlas_eviction_generation(),
    };

    let rows = snap.rows.len();
    let new_cursor = (
        snap.cursor.x,
        snap.cursor.y,
        snap.cursor.visible,
        snap.cursor.style,
        snap.focused,
        snap.cursor_blink_visible,
    );
    let mut rows_rebuilt: u64 = 0;
    let instance_start = instances.len();
    let mut bg_len = 0;
    let mut glyph_len = 0;
    let mut deco_len = 0;
    let mut stable = false;

    for _pass in 0..MAX_ATLAS_EVICTION_REBUILD_PASSES {
        let eviction_before = font.atlas_eviction_generation();
        new_key.atlas_eviction_generation = eviction_before;

        // Any pane-wide trigger bundled in `FrameInvalidationKey` differing
        // from the cached previous-frame key forces every row dirty. A pane's
        // first frame (`cache.key` still `None`) is also a full rebuild.
        let full = cache.key.as_ref() != Some(&new_key) || cache.bg.len() != rows;

        let mut dirty: Vec<bool> = if full {
            vec![true; rows]
        } else {
            snap.row_dirty.clone()
        };

        // Narrower than the pane-wide triggers: a change to the cursor's
        // position or its rendered shape (movement, DECSCUSR style, focus, or
        // blink phase) dirties EXACTLY the two affected rows, not the whole
        // pane.
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
            cache.flat.clear();
        }

        let mut rebuilt_rows_this_pass = 0_u64;
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
                rebuilt_rows_this_pass += 1;
            }
        }

        bg_len = cache.bg.iter().map(|row| row.len() as u32).sum();
        glyph_len = cache.glyph.iter().map(|row| row.len() as u32).sum();
        deco_len = cache.deco.iter().map(|row| row.len() as u32).sum();

        if full || rebuilt_rows_this_pass > 0 || cache.flat.is_empty() {
            cache.flat.clear();
            flatten_row_segments(&mut cache.flat, &cache.bg, &cache.glyph, &cache.deco);
        }

        instances.truncate(instance_start);
        instances.extend_from_slice(&cache.flat);
        append_search_prompt_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        append_command_palette_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        append_confirm_dialog_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );

        let eviction_after = font.atlas_eviction_generation();
        new_key.atlas_eviction_generation = eviction_after;
        if eviction_after == eviction_before {
            stable = true;
            break;
        }

        // A glyph eviction can make any row-cache segment from before the
        // eviction sample a now-reused atlas rectangle. Force the next pass
        // through the full rebuild path against the updated epoch.
        cache.key = None;
    }

    if !stable {
        log::warn!(
            "glyph atlas kept evicting across {MAX_ATLAS_EVICTION_REBUILD_PASSES} rebuild passes; row cache may be unstable"
        );
        cache.key = None;
    }

    if stable {
        cache.key = Some(new_key);
    }
    cache.prev_cursor = Some(new_cursor);

    PaneRebuild {
        clear_color,
        cell_size,
        rows_rebuilt,
        bg_len,
        text_len: bg_len + glyph_len + deco_len,
    }
}

/// Append the open search-prompt overlay (Cmd+F), if any, to `instances`.
/// Deliberately NOT part of [`PaneRenderCache`]'s per-row cache — it is
/// recomputed fresh on every call so a buffer edit always repaints (the
/// per-row cache is keyed on grid content/highlight state, which a prompt
/// keystroke never touches). Appended after every other pane instance so it
/// draws on top of the pane's normal content, one row tall, right-aligned
/// to `snap.cols` at row 0 (REQ: top-right of the focused pane).
fn append_search_prompt_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(buffer) = snap.search_prompt.as_deref() else {
        return;
    };
    let cols = snap.cols;
    if cols == 0 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let text = search_prompt_display_text(buffer, &snap.search, cols);
    let text_color = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let mut cells = search_prompt_segment_cells(&text, text_color);

    // Dim the trailing status suffix (` {i}/{n}` or ` no matches`) so it reads
    // as secondary to the query — same muted tone as palette hints. The suffix
    // is all narrow ASCII, so its column count equals its char count, and it
    // always sits at the very end even after the front-drop clamp below (which
    // only removes leading cells).
    let muted_color = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let suffix_cols = search_prompt_suffix_cols(buffer, &snap.search);

    // Second safety clamp: char-level truncation above can still overflow
    // the column count once double-width glyphs expand into a trailing
    // spacer cell. Drop from the front so the TAIL (the query text and the
    // i/n counter) stays visible, matching the buffer-clamp behavior above.
    if cells.len() > cols as usize {
        let excess = cells.len() - cols as usize;
        cells.drain(0..excess);
    }
    let counter_start = cells.len().saturating_sub(suffix_cols);
    for cell in &mut cells[counter_start..] {
        cell.color = muted_color;
    }
    let x_start = cols - cells.len() as u16;

    let bg_color = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x_start + i, 0],
            color: bg_color,
            flags: 0,
        });
    }

    for mut run in segment_row(font, &cells) {
        run.start_col += x_start;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, 0, metrics);
    }
}

/// Append the open command-palette overlay (`cmd+shift+p`), if any, to
/// `instances`. Like the search prompt this is recomputed fresh every call
/// (never per-row cached — a query/selection change never touches grid
/// content), and appended after every other pane instance so it draws on
/// top. Extends the search-prompt pattern to a multi-row block: a query row
/// plus one row per filtered entry (title left, keybind hint right-aligned),
/// centered in the pane, with the selected row drawn on an accent
/// background. Pure `CellInstance` bg-rects + shaped glyph runs — no new
/// pipeline or bind-group/std140 surface.
fn append_command_palette_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(palette) = snap.command_palette.as_ref() else {
        return;
    };
    let cols = snap.cols as usize;
    let grid_rows = snap.rows_n as usize;
    // Need at least the query row plus one padding column each side.
    if cols < 3 || grid_rows < 1 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let surface_bg = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    let surface_fg = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let muted_fg = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let accent_bg = to_u8_color(surface_output_rgba(
        style.accent_bg(),
        target_format_is_srgb,
    ));
    let accent_fg = to_u8_color(surface_output_rgba(
        style.accent_fg(),
        target_format_is_srgb,
    ));
    let border = to_u8_color(surface_output_rgba(style.border(), target_format_is_srgb));

    // A blank row separates the query from the entries/empty state when the
    // grid is tall enough to spare it; the entry window shrinks to make room.
    let pad = usize::from(grid_rows >= 4);
    let entry_capacity = grid_rows.saturating_sub(1 + pad);
    let (offset, shown) =
        palette_scroll_window(palette.rows.len(), palette.selected, entry_capacity);
    let entries = &palette.rows[offset..offset + shown];
    let show_empty = palette.rows.is_empty() && entry_capacity > 0;
    const EMPTY_PALETTE_LABEL: &str = "No commands found";

    // Inner content width: the widest of the query line and every shown
    // entry/empty-state line, clamped to leave one padding column on each side.
    let query_text = format!("> {}", palette.query);
    let title_w = entries
        .iter()
        .map(|(t, _)| t.chars().count())
        .max()
        .unwrap_or(0)
        .max(if show_empty {
            EMPTY_PALETTE_LABEL.chars().count()
        } else {
            0
        });
    let hint_w = entries
        .iter()
        .map(|(_, hint)| hint.as_deref().map_or(0, |h| h.chars().count()))
        .max()
        .unwrap_or(0);
    let gap = if hint_w > 0 { 2 } else { 0 };
    let inner = (title_w + gap + hint_w)
        .max(query_text.chars().count())
        .min(cols - 2);
    let block_w = inner + 2;
    let height = 1 + pad + shown + usize::from(show_empty);
    let x0 = ((cols - block_w) / 2) as u16;
    let y0 = (grid_rows.saturating_sub(height) / 2) as u16;

    let mut rows: Vec<OverlayRow> = Vec::with_capacity(height);
    // Query row: title text in surface_fg on the surface background.
    rows.push(OverlayRow::uniform(
        palette_line(&query_text, None, inner),
        surface_bg,
        surface_fg,
    ));
    if pad != 0 {
        rows.push(OverlayRow::uniform(
            palette_line("", None, inner),
            surface_bg,
            surface_fg,
        ));
    }
    if show_empty {
        rows.push(OverlayRow::uniform(
            palette_line(EMPTY_PALETTE_LABEL, None, inner),
            surface_bg,
            muted_fg,
        ));
    } else {
        for (i, (title, hint)) in entries.iter().enumerate() {
            let selected = offset + i == palette.selected;
            let text = palette_line(title, hint.as_deref(), inner);
            if selected {
                // Selected entry: whole row on the accent background, one color.
                rows.push(OverlayRow::uniform(text, accent_bg, accent_fg));
            } else {
                // Title in surface_fg, the right-aligned keybind hint dimmed.
                let hint_cols = hint.as_deref().map_or(0, |h| h.chars().count());
                rows.push(OverlayRow::title_hint(
                    text, surface_bg, surface_fg, muted_fg, inner, hint_cols,
                ));
            }
        }
    }

    append_overlay_block(
        instances,
        font,
        metrics,
        (x0, y0),
        block_w as u16,
        &rows,
        border,
    );
}

/// Append the open confirmation dialog (paste protection / clipboard-read),
/// if any, to `instances`. A centered two-row modal — a message line and a
/// key-hint line — reusing the command-palette block helpers. Recomputed
/// fresh every call and drawn on top of everything else.
fn append_confirm_dialog_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(dialog) = snap.confirm_dialog.as_ref() else {
        return;
    };
    let cols = snap.cols as usize;
    let grid_rows = snap.rows_n as usize;
    if cols < 3 || grid_rows < 2 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let surface_bg = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    let surface_fg = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let muted_fg = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let border = to_u8_color(surface_output_rgba(style.border(), target_format_is_srgb));

    let inner = dialog
        .message
        .chars()
        .count()
        .max(dialog.hint.chars().count())
        .min(cols - 2);
    let block_w = inner + 2;

    // A blank padding row above and below the two text rows when the grid is
    // tall enough (block = 4 rows); otherwise fall back to the compact
    // message+hint form so the dialog still fits a short pane.
    let pad = usize::from(grid_rows >= 6);
    let height = 2 + pad * 2;
    let x0 = ((cols - block_w) / 2) as u16;
    let y0 = (grid_rows.saturating_sub(height) / 2) as u16;

    let blank = || OverlayRow::uniform(palette_line("", None, inner), surface_bg, surface_fg);
    let mut rows: Vec<OverlayRow> = Vec::with_capacity(height);
    if pad != 0 {
        rows.push(blank());
    }
    rows.push(OverlayRow::uniform(
        palette_line(&dialog.message, None, inner),
        surface_bg,
        surface_fg,
    ));
    rows.push(OverlayRow::uniform(
        palette_line(&dialog.hint, None, inner),
        surface_bg,
        muted_fg,
    ));
    if pad != 0 {
        rows.push(blank());
    }

    append_overlay_block(
        instances,
        font,
        metrics,
        (x0, y0),
        block_w as u16,
        &rows,
        border,
    );
}

/// One row of a modal overlay block: a full-`block_w`-column line of text, a
/// background color, and a per-column foreground (so a palette entry can paint
/// its title and its dimmed keybind hint in different colors within one row).
struct OverlayRow {
    text: String,
    bg: [u8; 4],
    /// One foreground color per column of `text`.
    fg: Vec<[u8; 4]>,
}

impl OverlayRow {
    /// A row painted in a single foreground color.
    fn uniform(text: String, bg: [u8; 4], fg: [u8; 4]) -> Self {
        let cols = text.chars().count();
        OverlayRow {
            text,
            bg,
            fg: vec![fg; cols],
        }
    }

    /// A palette entry row: `fg` for the title area, `hint_fg` for the
    /// trailing `hint_cols` columns of the `inner`-wide content region (the
    /// right-aligned keybind hint). `text` is `inner + 2` columns wide (a
    /// one-space pad on each side), so the hint occupies columns
    /// `[1 + inner - hint_cols, 1 + inner)`.
    fn title_hint(
        text: String,
        bg: [u8; 4],
        fg: [u8; 4],
        hint_fg: [u8; 4],
        inner: usize,
        hint_cols: usize,
    ) -> Self {
        let mut row = Self::uniform(text, bg, fg);
        if hint_cols > 0 {
            let start = 1 + inner - hint_cols.min(inner);
            for slot in row.fg.iter_mut().skip(start).take(hint_cols) {
                *slot = hint_fg;
            }
        }
        row
    }
}

/// Emit a centered modal overlay block: each row's background quads and
/// glyph run, then a 1px border in `border` color around the whole block.
///
/// Border mechanism: per-cell `FLAG_DECORATION` rects along the block's edge
/// cells (a 1px inset within each perimeter cell). Chosen over the
/// window-absolute `FLAG_DIVIDER` path because decoration quads live in the
/// same per-pane cell pass and scissor as the block's own background and
/// glyphs — they share the pane's coordinate space and paint order, so there
/// is no risk of the later full-viewport divider pass drawing the outline
/// over the wrong pane. The tradeoff is the outline snaps to cell edges
/// rather than being a free-floating pixel rectangle.
fn append_overlay_block(
    instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    metrics: Metrics,
    origin: (u16, u16),
    block_w: u16,
    rows: &[OverlayRow],
    border: [u8; 4],
) {
    let (x0, y0) = origin;
    for (i, row) in rows.iter().enumerate() {
        append_overlay_row(instances, font, metrics, x0, y0 + i as u16, row);
    }
    if !rows.is_empty() {
        append_overlay_border(
            instances,
            x0,
            y0,
            block_w,
            rows.len() as u16,
            border,
            metrics,
        );
    }
}

/// Emit one overlay row's background rects (`block_w` cells wide, from `row`'s
/// text length) plus its per-column-colored shaped glyphs at grid row `y`.
fn append_overlay_row(
    instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    metrics: Metrics,
    x0: u16,
    y: u16,
    row: &OverlayRow,
) {
    let cells = overlay_segment_cells(&row.text, &row.fg);
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x0 + i, y],
            color: row.bg,
            flags: 0,
        });
    }
    for mut run in segment_row(font, &cells) {
        run.start_col += x0;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, y, metrics);
    }
}

/// Emit the 1px border of a `block_w` x `block_h` cell block anchored at
/// `(x0, y0)` as decoration-pass rects (see [`append_overlay_block`] for why
/// this path rather than the pixel-overlay divider path). Top/bottom edges
/// run along the block's first/last rows; left/right along its first/last
/// columns, so the four strips meet at the corner cells.
fn append_overlay_border(
    instances: &mut Vec<CellInstance>,
    x0: u16,
    y0: u16,
    block_w: u16,
    block_h: u16,
    color: [u8; 4],
    metrics: Metrics,
) {
    let thickness: u16 = 1;
    let width = metrics.cell_w.round().max(1.0) as u16;
    let height = metrics.cell_h.round().max(1.0) as u16;
    let right_x = width.saturating_sub(thickness) as i16;
    let bottom_y = height.saturating_sub(thickness) as i16;
    let y_last = y0 + block_h - 1;
    let x_last = x0 + block_w - 1;

    for i in 0..block_w {
        let x = x0 + i;
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x,
                grid_y: y0,
                color,
            },
            DecorationRect::new(0, 0, width, thickness),
        );
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x,
                grid_y: y_last,
                color,
            },
            DecorationRect::new(0, bottom_y, width, thickness),
        );
    }
    for j in 0..block_h {
        let y = y0 + j;
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x0,
                grid_y: y,
                color,
            },
            DecorationRect::new(0, 0, thickness, height),
        );
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x_last,
                grid_y: y,
                color,
            },
            DecorationRect::new(right_x, 0, thickness, height),
        );
    }
}

/// Turn an overlay row's full-width text into row-local [`SegmentCell`]s (one
/// per column), coloring each by `fg_by_col`. Mirrors
/// [`search_prompt_segment_cells`]'s width handling (double-width lead +
/// spacer, zero-width combining attaches to the previous cell) but assigns a
/// possibly different color per column.
fn overlay_segment_cells(text: &str, fg_by_col: &[[u8; 4]]) -> Vec<SegmentCell> {
    let fallback = fg_by_col.last().copied().unwrap_or([255, 255, 255, 255]);
    let blank = |color: [u8; 4]| SegmentCell {
        ch: ' ',
        combining: Vec::new(),
        bold: false,
        italic: false,
        selected: false,
        active_search: false,
        search_match: false,
        cursor: false,
        color,
    };

    let mut cells = Vec::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let color = fg_by_col.get(col).copied().unwrap_or(fallback);
        match UnicodeWidthChar::width(ch).unwrap_or(0) {
            0 => {
                if let Some(last) = cells.last_mut() {
                    let last: &mut SegmentCell = last;
                    last.combining.push(ch);
                }
            }
            2 => {
                cells.push(SegmentCell { ch, ..blank(color) });
                cells.push(blank(color));
                col += 2;
            }
            _ => {
                cells.push(SegmentCell { ch, ..blank(color) });
                col += 1;
            }
        }
    }
    cells
}

/// Which `count`-length list slice to render given `selected` and a row
/// `capacity`: the whole list when it fits, otherwise a `capacity`-tall
/// window scrolled just far enough to keep `selected` on screen. Returns
/// `(offset, shown)`.
fn palette_scroll_window(count: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if capacity == 0 || count == 0 {
        return (0, 0);
    }
    if count <= capacity {
        return (0, count);
    }
    let offset = if selected < capacity {
        0
    } else {
        (selected + 1 - capacity).min(count - capacity)
    };
    (offset, capacity)
}

/// One `inner`-column line for the palette block: `left` at the leading edge,
/// optional `right` flush to the trailing edge, spaces between. Palette
/// titles and keybind hints are ASCII (one column per char), so column count
/// equals char count. A one-space pad is added on each side, producing an
/// `inner + 2`-column string.
fn palette_line(left: &str, right: Option<&str>, inner: usize) -> String {
    let mut cols = vec![' '; inner];
    for (i, ch) in left.chars().enumerate() {
        if i >= inner {
            break;
        }
        cols[i] = ch;
    }
    if let Some(right) = right {
        let rlen = right.chars().count();
        if rlen <= inner {
            let start = inner - rlen;
            for (i, ch) in right.chars().enumerate() {
                cols[start + i] = ch;
            }
        }
    }
    let mut line = String::with_capacity(inner + 2);
    line.push(' ');
    line.extend(cols);
    line.push(' ');
    line
}

/// Compose the prompt's display text: `Find: {buffer}▏ {status}`. The status
/// is either the 1-based active match counter or an explicit `no matches`
/// state for a non-empty query with zero hits. Clamps to `cols` by keeping the
/// TAIL of a buffer too long to fit alongside the fixed prefix/status.
fn search_prompt_display_text(buffer: &str, search: &SearchState, cols: u16) -> String {
    const PREFIX: &str = "Find: ";
    const CURSOR_MARK: &str = "\u{258F}"; // "▏" left one eighth block, reads as a thin caret.
    let suffix = search_prompt_suffix(buffer, search);

    let fixed_chars = PREFIX.chars().count() + CURSOR_MARK.chars().count() + suffix.chars().count();
    let available = (cols as usize).saturating_sub(fixed_chars);
    let buffer_chars: Vec<char> = buffer.chars().collect();
    let shown: String = if buffer_chars.len() > available {
        buffer_chars[buffer_chars.len() - available..]
            .iter()
            .collect()
    } else {
        buffer.to_string()
    };

    format!("{PREFIX}{shown}{CURSOR_MARK}{suffix}")
}

/// The trailing status suffix of the search prompt. A non-empty query with no
/// hits gets a readable state instead of an ambiguous `0/0`; otherwise the
/// suffix is the 1-based active index / total match count.
fn search_prompt_suffix(buffer: &str, search: &SearchState) -> String {
    if !buffer.is_empty() && !search.query().is_empty() && search.matches().is_empty() {
        return " no matches".to_string();
    }

    let total = search.matches().len();
    let current = search.active_index().map_or(0, |idx| idx + 1);
    format!(" {current}/{total}")
}

/// Column count of the search prompt's status suffix (all narrow ASCII, so
/// columns == chars).
fn search_prompt_suffix_cols(buffer: &str, search: &SearchState) -> usize {
    search_prompt_suffix(buffer, search).chars().count()
}

/// Turn the prompt's display text into row-local [`SegmentCell`]s, one per
/// column — a double-width character gets a lead cell plus a blank spacer
/// cell (mirroring `noa_grid::Screen`'s WIDE/WIDE_SPACER print path), and a
/// zero-width combining mark attaches to the previous cell instead of
/// consuming its own column.
fn search_prompt_segment_cells(text: &str, color: [u8; 4]) -> Vec<SegmentCell> {
    let blank = |color: [u8; 4]| SegmentCell {
        ch: ' ',
        combining: Vec::new(),
        bold: false,
        italic: false,
        selected: false,
        active_search: false,
        search_match: false,
        cursor: false,
        color,
    };

    let mut cells = Vec::new();
    for ch in text.chars() {
        match UnicodeWidthChar::width(ch).unwrap_or(0) {
            0 => {
                if let Some(last) = cells.last_mut() {
                    let last: &mut SegmentCell = last;
                    last.combining.push(ch);
                }
            }
            2 => {
                cells.push(SegmentCell { ch, ..blank(color) });
                cells.push(blank(color));
            }
            _ => cells.push(SegmentCell { ch, ..blank(color) }),
        }
    }
    cells
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

/// How the cursor renders at its current cell, resolved once per row from
/// pane-wide [`FrameSnapshot`] state (position, DECSCUSR style, focus, blink
/// phase) — never per-cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CursorVisual {
    /// DECTCEM off, viewport scrolled away from the live cursor, or a
    /// focused `Blinking*` style in its off phase.
    None,
    /// Solid block fill + inverted glyph (steady or blinking-and-visible).
    Block,
    /// Thin vertical bar at the cell's left edge; glyph keeps its own colors.
    Bar,
    /// Thin horizontal strip at the cell's bottom; glyph keeps its own colors.
    Underline,
    /// Unfocused pane: a hollow rectangle outline regardless of DECSCUSR
    /// style. Never blinks.
    Hollow,
}

fn cursor_visual_for(snap: &FrameSnapshot) -> CursorVisual {
    if !snap.cursor.visible {
        return CursorVisual::None;
    }
    if !snap.focused {
        return CursorVisual::Hollow;
    }
    let is_blinking_style = matches!(
        snap.cursor.style,
        CursorStyle::BlinkingBlock | CursorStyle::BlinkingUnderline | CursorStyle::BlinkingBar
    );
    if is_blinking_style && !snap.cursor_blink_visible {
        return CursorVisual::None;
    }
    match snap.cursor.style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => CursorVisual::Block,
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => CursorVisual::Underline,
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => CursorVisual::Bar,
    }
}

/// The cursor's own color: an explicit OSC 12 override if set, else the
/// cell's foreground (so an unstyled cursor tracks the text it sits on).
/// The returned color is contrast-adjusted against the effective cell
/// background so the cursor stays visible even on low-contrast themes.
fn cursor_fill_rgb(
    theme: &Theme,
    snap: &FrameSnapshot,
    fg_color: Color,
    bg_rgb: noa_core::Rgb,
) -> noa_core::Rgb {
    let base = if let Some(cursor) = snap.colors.cursor() {
        cursor
    } else {
        theme.resolve_rgb_with_colors(fg_color, true, &snap.colors)
    };
    theme.contrast_adjusted_fg(base, bg_rgb)
}

fn surface_output_rgb(rgb: noa_core::Rgb, target_format_is_srgb: bool) -> [f32; 4] {
    surface_output_rgba(rgba(rgb), target_format_is_srgb)
}

/// Emit the decoration-pass rect(s) for a non-block cursor shape. `visual`
/// must not be [`CursorVisual::Block`] or [`CursorVisual::None`] — those are
/// handled by the background-quad path (block) or emit nothing (none).
fn push_cursor_decorations(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    visual: CursorVisual,
    color: [u8; 4],
    metrics: Metrics,
) {
    let cell = DecorationCell {
        grid_x: x,
        grid_y: y,
        color,
    };
    let thickness = decoration_thickness(metrics);
    let width = metrics.cell_w.round().max(1.0) as u16;
    let height = metrics.cell_h.round().max(1.0) as u16;

    match visual {
        CursorVisual::Bar => {
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, thickness, height),
            );
        }
        CursorVisual::Underline => {
            let base_y = underline_y(metrics, thickness, 0.0);
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, base_y, width, thickness),
            );
        }
        CursorVisual::Hollow => {
            let right = width.saturating_sub(thickness) as i16;
            let bottom = height.saturating_sub(thickness) as i16;
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, width, thickness),
            ); // top
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, bottom, width, thickness),
            ); // bottom
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, thickness, height),
            ); // left
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(right, 0, thickness, height),
            ); // right
        }
        CursorVisual::Block | CursorVisual::None => {}
    }
}

/// Like [`push_decoration_rect`], but also tags the instance `FLAG_CURSOR`
/// so it's identifiable as a cursor-shape overlay rather than a regular
/// text decoration (underline/strike/etc). The shader only checks
/// `FLAG_DECORATION` for this quad's vertex path, so the extra bit is inert
/// there — it exists for renderer-side introspection (tests).
fn push_cursor_decoration_rect(
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
        flags: CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR,
    });
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

/// Whether `cell` at `(x, y)` falls under the snapshot's current Cmd+hover
/// target (see [`HoverLink`]).
fn is_hover_link_cell(snap: &FrameSnapshot, cell: &Cell, x: u16, y: u16) -> bool {
    match snap.hover_link {
        Some(HoverLink::Registry(id)) => cell.hyperlink == Some(id),
        Some(HoverLink::Range {
            y: row_y,
            x_start,
            x_end,
        }) => y == row_y && x >= x_start && x <= x_end,
        None => false,
    }
}

/// Cmd+hover underline for an OSC 8 hyperlink or auto-detected URL — an
/// extra decoration-pass rect independent of the cell's own UNDERLINE/
/// CURLY_UNDERLINE/etc attrs (both can coexist), using the same plain
/// underline geometry and the cell's own (possibly selection/search-
/// recolored) foreground.
fn push_hover_link_underline(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    color: [u8; 4],
    metrics: Metrics,
) {
    let thickness = decoration_thickness(metrics);
    let width = metrics.cell_w.round().max(1.0) as u16;
    let base_y = underline_y(metrics, thickness, 0.0);
    push_decoration_rect(
        instances,
        DecorationCell {
            grid_x: x,
            grid_y: y,
            color,
        },
        DecorationRect::new(0, base_y, width, thickness),
    );
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

/// Scale a clear color's alpha by `background-opacity`, leaving rgb intact.
/// Only the clear color carries the setting: it fills the window padding and
/// every default-background cell (those emit no bg quad, so the clear shows
/// through). Explicit-bg / selection / cursor quads keep alpha 1.0 and stay
/// opaque. With the surface in `PostMultiplied` alpha mode this makes the
/// default-bg regions translucent while inked glyphs — whose coverage pushes
/// the framebuffer alpha back toward 1.0 through `ALPHA_BLENDING` — stay solid.
fn apply_background_opacity(clear_color: [f32; 4], opacity: f32) -> [f32; 4] {
    let mut out = clear_color;
    out[3] = clear_color[3] * opacity.clamp(0.0, 1.0);
    out
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
    let scaled = channel * 255.0;
    let rounded = scaled.round();
    if (scaled - rounded).abs() <= 0.0001 {
        return srgb_to_linear_u8_lut()[rounded as usize];
    }

    srgb_to_linear_exact(channel)
}

fn srgb_to_linear_u8_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0; 256];
        for (idx, slot) in lut.iter_mut().enumerate() {
            *slot = srgb_to_linear_exact(idx as f32 / 255.0);
        }
        lut
    })
}

fn srgb_to_linear_exact(channel: f32) -> f32 {
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
    identity: u64,
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
    seen_identity: &mut u64,
    seen_generation: &mut u64,
    input: AtlasSyncInput<'_>,
) -> bool {
    if input.identity == *seen_identity
        && input.generation == *seen_generation
        && input.size == (texture.width(), texture.height())
    {
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
    *seen_identity = input.identity;
    *seen_generation = input.generation;
    recreated
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::{Color, GridSize, Rgb};
    use noa_font::{FontConfig, ShapeCell, StyleKey};
    use noa_grid::{Cell, Cursor, SearchMatch, SelectionPoint, Terminal};
    use noa_vt::Stream;

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
    fn sync_atlas_uploads_rebuilt_font_grid_even_when_generation_restarts() {
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping rebuilt FontGrid atlas sync test");
            return;
        };
        let Some(mut first_font) = skip_font() else {
            return;
        };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8Unorm,
            &mut first_font,
            GridPadding::ZERO,
        )
        .expect("build renderer");
        let first_identity = first_font.atlas_identity();
        assert_eq!(renderer.mask_atlas_seen_identity, first_identity);
        assert_eq!(renderer.color_atlas_seen_identity, first_identity);

        let mut rebuilt_font = match FontGrid::new(14.0, FontConfig::default()) {
            Ok(font) => font,
            Err(err) => {
                eprintln!("skipping: no system monospace font available: {err}");
                return;
            }
        };
        assert_ne!(rebuilt_font.atlas_identity(), first_identity);
        assert_eq!(
            rebuilt_font.mask_atlas_generation(),
            renderer.mask_atlas_seen_generation(),
            "the regression requires a fresh FontGrid whose atlas generation restarts"
        );

        renderer.sync_atlas(&device, &queue, &mut rebuilt_font);

        assert_eq!(
            renderer.mask_atlas_seen_identity,
            rebuilt_font.atlas_identity(),
            "mask atlas sync must not skip a rebuilt FontGrid just because generation matches"
        );
        assert_eq!(
            renderer.color_atlas_seen_identity,
            rebuilt_font.atlas_identity(),
            "color atlas sync must not skip a rebuilt FontGrid just because generation matches"
        );
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

    #[test]
    fn background_opacity_scales_only_clear_alpha() {
        let base = [0.1, 0.2, 0.3, 1.0];
        // Fully opaque leaves the clear color untouched.
        assert_eq!(apply_background_opacity(base, 1.0), base);
        // Partial opacity scales only alpha; rgb stays the theme background.
        assert_eq!(apply_background_opacity(base, 0.8), [0.1, 0.2, 0.3, 0.8]);
        // Zero is fully transparent; out-of-range values clamp.
        assert_eq!(apply_background_opacity(base, 0.0), [0.1, 0.2, 0.3, 0.0]);
        assert_eq!(apply_background_opacity(base, 2.0), base);
    }

    #[test]
    fn default_bg_cell_emits_no_background_quad_so_clear_color_shows_through() {
        // The opacity path relies on default-background cells NOT painting a
        // bg quad: the (opacity-scaled) clear color is what fills them. A cell
        // with an explicit bg still paints an opaque quad.
        let Some(mut font) = skip_font() else {
            return;
        };
        let mut terminal = Terminal::new(GridSize::new(2, 1));
        terminal.primary.cursor.visible = false;
        terminal.primary.grid[0].cells[0].ch = ' ';
        terminal.primary.grid[0].cells[0].bg = Color::Default;
        terminal.primary.grid[0].cells[1].ch = ' ';
        terminal.primary.grid[0].cells[1].bg = Color::Rgb(Rgb::new(2, 3, 4));
        let snap = FrameSnapshot::from_terminal(&mut terminal);

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

        let bg_quads: Vec<_> = instances
            .iter()
            .filter(|instance| instance.flags == 0 && instance.glyph_size == [0, 0])
            .collect();
        assert_eq!(
            bg_quads.len(),
            1,
            "only the explicit-bg cell should paint a background quad"
        );
        assert_eq!(bg_quads[0].grid_pos, [1, 0]);
        assert_eq!(
            bg_quads[0].color[3], 255,
            "explicit background quads stay fully opaque regardless of background-opacity"
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

    /// Skip-on-no-font harness shared by the bar/underline/hollow/blink
    /// cursor-shape tests below — mirrors
    /// `cursor_cell_with_glyph_generates_reversed_glyph_instance`'s guard.
    fn font_with_rasterized_m() -> Option<FontGrid> {
        let mut font = match FontGrid::new(14.0, FontConfig::default()) {
            Ok(font) => font,
            Err(err) => {
                eprintln!("skipping: no system monospace font available: {err}");
                return None;
            }
        };
        if font.get_or_raster('M').atlas_size == [0, 0] {
            eprintln!("skipping: installed monospace font did not rasterize 'M'");
            return None;
        }
        Some(font)
    }

    fn rgb_from_instance(instance: &CellInstance) -> Rgb {
        Rgb::new(instance.color[0], instance.color[1], instance.color[2])
    }

    fn one_cell_terminal_with_cursor_style(style: CursorStyle) -> Terminal {
        let mut terminal = Terminal::new(GridSize::new(1, 1));
        terminal.primary.cursor.x = 0;
        terminal.primary.cursor.y = 0;
        terminal.primary.cursor.style = style;
        terminal.primary.grid[0].cells[0].ch = 'M';
        terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
        terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
        terminal
    }

    #[test]
    fn minimum_contrast_adjusts_low_contrast_glyph_color() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(1, 1));
        terminal.primary.cursor.visible = false;
        terminal.primary.grid[0].cells[0].ch = 'M';
        terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(0x22, 0x22, 0x22));
        terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
        let snap = FrameSnapshot::from_terminal(&mut terminal);
        let mut theme = Theme::new();
        theme.minimum_contrast = 4.5;

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

        let glyph = instances
            .iter()
            .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
            .expect("glyph must be drawn");
        let adjusted = rgb_from_instance(glyph);
        let bg = Rgb::new(0x00, 0x00, 0x00);
        assert_ne!(adjusted, Rgb::new(0x22, 0x22, 0x22));
        assert!(
            crate::theme::contrast_ratio(adjusted, bg) >= 4.5,
            "adjusted={adjusted:?} ratio={}",
            crate::theme::contrast_ratio(adjusted, bg)
        );
    }

    #[test]
    fn minimum_contrast_keeps_cursor_visible_against_cell_background() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(1, 1));
        terminal.primary.cursor.x = 0;
        terminal.primary.cursor.y = 0;
        terminal.primary.grid[0].cells[0].ch = 'M';
        terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
        terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
        let snap = FrameSnapshot::from_terminal(&mut terminal);
        let mut theme = Theme::new();
        theme.minimum_contrast = 3.0;

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

        let cursor_bg = instances
            .iter()
            .find(|i| {
                i.grid_pos == [0, 0]
                    && i.flags == CellInstance::FLAG_CURSOR
                    && i.glyph_size == [0, 0]
            })
            .expect("cursor block fill must be drawn");
        let cursor_rgb = rgb_from_instance(cursor_bg);
        let bg = Rgb::new(0x00, 0x00, 0x00);
        assert_ne!(cursor_rgb, bg);
        assert!(
            crate::theme::contrast_ratio(cursor_rgb, bg) >= 3.0,
            "cursor={cursor_rgb:?} ratio={}",
            crate::theme::contrast_ratio(cursor_rgb, bg)
        );
    }

    #[test]
    fn bar_and_underline_cursors_do_not_fill_or_recolor_the_cell() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        for style in [CursorStyle::SteadyBar, CursorStyle::SteadyUnderline] {
            let mut terminal = one_cell_terminal_with_cursor_style(style);
            let snap = FrameSnapshot::from_terminal(&mut terminal);

            let mut instances = Vec::new();
            rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

            assert!(
                instances
                    .iter()
                    .all(|i| i.flags != CellInstance::FLAG_CURSOR),
                "{style:?}: must not emit an opaque block-fill background quad"
            );

            let glyph_instance = instances
                .iter()
                .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
                .expect("cell glyph must still be drawn");
            assert_eq!(
                glyph_instance.color,
                [240, 10, 20, 255],
                "{style:?}: glyph keeps the cell's own foreground, not inverted to the background"
            );

            let cursor_decorations: Vec<_> = instances
                .iter()
                .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
                .collect();
            assert_eq!(
                cursor_decorations.len(),
                1,
                "{style:?}: exactly one cursor-shape decoration rect"
            );
            assert_eq!(cursor_decorations[0].grid_pos, [0, 0]);
        }
    }

    #[test]
    fn unfocused_pane_draws_a_hollow_outline_not_a_block_fill() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::SteadyBlock);
        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        snap.focused = false;

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

        assert!(
            instances
                .iter()
                .all(|i| i.flags != CellInstance::FLAG_CURSOR),
            "an unfocused pane must not emit a block-fill background quad, even for a block style"
        );
        let outline_rects: Vec<_> = instances
            .iter()
            .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
            .collect();
        assert_eq!(
            outline_rects.len(),
            4,
            "an unfocused pane's cursor is a 4-sided hollow outline"
        );

        let glyph_instance = instances
            .iter()
            .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
            .expect("cell glyph must still be drawn");
        assert_eq!(
            glyph_instance.color,
            [240, 10, 20, 255],
            "glyph keeps its own foreground when unfocused"
        );
    }

    #[test]
    fn focused_blinking_cursor_in_off_phase_emits_no_cursor_instances() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::BlinkingBlock);
        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        snap.cursor_blink_visible = false;

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

        assert!(
            instances
                .iter()
                .all(|i| i.flags & CellInstance::FLAG_CURSOR == 0),
            "a blinking cursor's off phase draws no block quad, decoration, or cursor-flagged glyph"
        );
        let glyph_instance = instances
            .iter()
            .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
            .expect("cell glyph must still be drawn");
        assert_eq!(
            glyph_instance.color,
            [240, 10, 20, 255],
            "off-phase glyph keeps its own foreground, unaffected by the hidden cursor"
        );
    }

    #[test]
    fn cursor_visual_resolves_per_style_focus_and_blink_phase() {
        let mut snap = baseline_snapshot(['a', 'b', 'c']);
        snap.cursor.style = CursorStyle::BlinkingBlock;

        assert_eq!(
            cursor_visual_for(&snap),
            CursorVisual::Block,
            "focused + blink-visible block style fills the cell"
        );

        snap.cursor_blink_visible = false;
        assert_eq!(
            cursor_visual_for(&snap),
            CursorVisual::None,
            "a focused blinking cursor's off phase draws nothing"
        );

        snap.cursor.style = CursorStyle::SteadyBar;
        assert_eq!(
            cursor_visual_for(&snap),
            CursorVisual::Bar,
            "a steady style ignores blink phase entirely"
        );

        snap.cursor.style = CursorStyle::SteadyUnderline;
        assert_eq!(cursor_visual_for(&snap), CursorVisual::Underline);

        snap.focused = false;
        assert_eq!(
            cursor_visual_for(&snap),
            CursorVisual::Hollow,
            "an unfocused pane always shows the hollow outline, ignoring style and blink phase"
        );

        snap.cursor.visible = false;
        assert_eq!(
            cursor_visual_for(&snap),
            CursorVisual::None,
            "a DECTCEM-hidden cursor never renders, focused or not"
        );
    }

    #[test]
    fn cursor_bar_decoration_is_a_full_height_left_edge_rect() {
        let mut instances = Vec::new();
        push_cursor_decorations(
            &mut instances,
            2,
            5,
            CursorVisual::Bar,
            [9, 8, 7, 255],
            metrics(18.0),
        );

        assert_eq!(instances.len(), 1);
        let bar = instances[0];
        assert_eq!(bar.grid_pos, [2, 5]);
        assert_eq!(
            bar.bearing,
            [0, 0],
            "bar sits flush against the cell's left edge"
        );
        assert_eq!(
            bar.glyph_size,
            [1, 24],
            "bar width tracks decoration thickness, full cell height"
        );
        assert_eq!(bar.color, [9, 8, 7, 255]);
        assert_eq!(
            bar.flags,
            CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
        );
    }

    #[test]
    fn cursor_underline_decoration_reuses_the_text_underline_geometry() {
        let mut instances = Vec::new();
        let m = metrics(18.0);
        push_cursor_decorations(
            &mut instances,
            0,
            0,
            CursorVisual::Underline,
            [1, 2, 3, 255],
            m,
        );

        assert_eq!(instances.len(), 1);
        let strip = instances[0];
        assert_eq!(
            strip.glyph_size,
            [10, 1],
            "underline spans the full cell width at decoration thickness"
        );
        assert_eq!(
            strip.bearing[1],
            underline_y(m, decoration_thickness(m), 0.0),
            "y matches the same baseline offset the UNDERLINE attribute decoration uses"
        );
        assert_eq!(
            strip.flags,
            CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
        );
    }

    #[test]
    fn cursor_hollow_decoration_emits_four_edge_rects() {
        let mut instances = Vec::new();
        push_cursor_decorations(
            &mut instances,
            3,
            1,
            CursorVisual::Hollow,
            [4, 5, 6, 255],
            metrics(18.0),
        );

        assert_eq!(
            instances.len(),
            4,
            "hollow outline is exactly top/bottom/left/right"
        );
        assert!(instances.iter().all(|i| {
            i.grid_pos == [3, 1]
                && i.color == [4, 5, 6, 255]
                && i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR)
        }));

        assert_eq!(instances[0].bearing, [0, 0], "top edge");
        assert_eq!(instances[0].glyph_size, [10, 1]);
        assert_eq!(instances[1].bearing, [0, 23], "bottom edge");
        assert_eq!(instances[1].glyph_size, [10, 1]);
        assert_eq!(instances[2].bearing, [0, 0], "left edge");
        assert_eq!(instances[2].glyph_size, [1, 24]);
        assert_eq!(instances[3].bearing, [9, 0], "right edge");
        assert_eq!(instances[3].glyph_size, [1, 24]);
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
    fn hover_link_registry_underlines_only_cells_carrying_that_link_id() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(3, 1));
        terminal.primary.grid[0].cells[0].ch = 'M';
        terminal.primary.grid[0].cells[0].hyperlink = Some(0);
        terminal.primary.grid[0].cells[1].ch = 'M';
        terminal.primary.grid[0].cells[1].hyperlink = Some(1); // a different link
        terminal.primary.grid[0].cells[2].ch = 'M'; // no link at all

        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        assert_eq!(snap.hover_link, None, "from_terminal defaults to no hover");

        let mut no_hover = Vec::new();
        rebuild_cell_instances(&mut no_hover, &snap, &mut font, &Theme::new(), false);
        assert!(
            no_hover
                .iter()
                .all(|i| i.flags != CellInstance::FLAG_DECORATION),
            "no hover target set: no hover underline should be emitted"
        );

        snap.hover_link = Some(HoverLink::Registry(0));
        let mut hovered = Vec::new();
        rebuild_cell_instances(&mut hovered, &snap, &mut font, &Theme::new(), false);
        let underlined: Vec<[u16; 2]> = hovered
            .iter()
            .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
            .map(|i| i.grid_pos)
            .collect();
        assert_eq!(
            underlined,
            vec![[0, 0]],
            "only the cell carrying the hovered registry id gets the hover underline, \
             not the cell with a different link id or the cell with no link"
        );
    }

    #[test]
    fn hover_link_range_underlines_only_the_matching_run_on_its_row() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(4, 2));
        for row in 0..2 {
            for x in 0..4 {
                terminal.primary.grid[row].cells[x].ch = 'M';
            }
        }

        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        snap.hover_link = Some(HoverLink::Range {
            y: 0,
            x_start: 1,
            x_end: 2,
        });

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);
        let mut underlined: Vec<[u16; 2]> = instances
            .iter()
            .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
            .map(|i| i.grid_pos)
            .collect();
        underlined.sort();
        assert_eq!(
            underlined,
            vec![[1, 0], [2, 0]],
            "only columns 1..=2 on row 0 are underlined; row 1 and the rest of row 0 are not"
        );
    }

    #[test]
    fn search_prompt_display_text_keeps_the_tail_of_a_buffer_too_long_to_fit() {
        let search = SearchState::default();

        // cols=20, fixed chars ("Find: " + "▏" + " 0/0") = 11, so 9 chars of
        // buffer fit; the last 9 of "0123456789" is "123456789".
        let text = search_prompt_display_text("0123456789", &search, 20);
        assert_eq!(text, "Find: 123456789\u{258F} 0/0");
        assert_eq!(text.chars().count(), 20);

        let short = search_prompt_display_text("hi", &search, 20);
        assert_eq!(
            short, "Find: hi\u{258F} 0/0",
            "a short buffer is shown in full"
        );
    }

    #[test]
    fn search_prompt_display_text_reports_no_matches_for_non_empty_query() {
        let mut search = SearchState::default();
        search.set_query("needle".to_string(), Vec::new());

        let text = search_prompt_display_text("needle", &search, 30);

        assert_eq!(text, "Find: needle\u{258F} no matches");
    }

    #[test]
    fn search_prompt_overlay_emits_top_right_bg_and_glyph_instances_and_tracks_the_buffer() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(20, 2));
        let theme = Theme::new();

        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        assert_eq!(
            snap.search_prompt, None,
            "from_terminal defaults to no prompt"
        );

        let mut without_prompt = Vec::new();
        rebuild_cell_instances(&mut without_prompt, &snap, &mut font, &theme, false);
        let row0_bg_before = without_prompt
            .iter()
            .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
            .count();

        snap.search_prompt = Some("M".to_string());
        let mut with_prompt = Vec::new();
        rebuild_cell_instances(&mut with_prompt, &snap, &mut font, &theme, false);

        let prompt_bg: Vec<_> = with_prompt
            .iter()
            .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
            .collect();
        assert!(
            prompt_bg.len() > row0_bg_before,
            "opening the prompt must add background quads to row 0"
        );
        assert!(
            prompt_bg
                .iter()
                .all(|i| i.grid_pos[0] >= snap.cols - prompt_bg.len() as u16),
            "the prompt is right-aligned to the pane's rightmost columns: {prompt_bg:?}"
        );

        let glyphs: Vec<_> = with_prompt
            .iter()
            .filter(|i| i.grid_pos[1] == 0 && i.flags & CellInstance::FLAG_GLYPH != 0)
            .collect();
        assert!(
            !glyphs.is_empty(),
            "the prompt text must emit glyph instances"
        );

        // A buffer edit must always repaint — this overlay is deliberately
        // NOT part of the per-row cache, so a longer buffer widens it on
        // the very next rebuild.
        snap.search_prompt = Some("Mxyz".to_string());
        let mut with_longer_prompt = Vec::new();
        rebuild_cell_instances(&mut with_longer_prompt, &snap, &mut font, &theme, false);
        let longer_bg = with_longer_prompt
            .iter()
            .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
            .count();
        assert!(
            longer_bg > prompt_bg.len(),
            "a longer buffer must widen the overlay ({longer_bg} vs {})",
            prompt_bg.len()
        );

        // Closing the prompt (search_prompt back to None) must remove the
        // overlay instances on the very next rebuild too.
        snap.search_prompt = None;
        let mut closed = Vec::new();
        rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
        let closed_bg = closed
            .iter()
            .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
            .count();
        assert_eq!(
            closed_bg, row0_bg_before,
            "closing the prompt removes the overlay"
        );
    }

    #[test]
    fn palette_scroll_window_keeps_the_selection_visible() {
        // Fits entirely: whole list, no scroll.
        assert_eq!(palette_scroll_window(3, 2, 5), (0, 3));
        // Taller than capacity, selection near the top: window pinned to 0.
        assert_eq!(palette_scroll_window(10, 1, 4), (0, 4));
        // Selection past the first window: scroll just far enough.
        assert_eq!(palette_scroll_window(10, 5, 4), (2, 4));
        // Selection at the end: window pinned to the bottom.
        assert_eq!(palette_scroll_window(10, 9, 4), (6, 4));
        // Degenerate inputs never panic.
        assert_eq!(palette_scroll_window(0, 0, 4), (0, 0));
        assert_eq!(palette_scroll_window(5, 0, 0), (0, 0));
    }

    #[test]
    fn command_palette_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(30, 8));
        let theme = Theme::new();

        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        assert_eq!(
            snap.command_palette, None,
            "from_terminal defaults to no palette"
        );

        let mut closed = Vec::new();
        rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
        let bg_before = closed.iter().filter(|i| i.flags == 0).count();

        snap.command_palette = Some(crate::CommandPaletteSnapshot {
            query: "sp".to_string(),
            rows: vec![
                ("Split Right".to_string(), Some("cmd+d".to_string())),
                ("Split Down".to_string(), Some("cmd+shift+d".to_string())),
                ("Toggle Split Zoom".to_string(), None),
            ],
            selected: 1,
        });
        let mut with_palette = Vec::new();
        rebuild_cell_instances(&mut with_palette, &snap, &mut font, &theme, false);

        let bg_with = with_palette.iter().filter(|i| i.flags == 0).count();
        assert!(
            bg_with > bg_before,
            "opening the palette must add background quads"
        );
        // The block spans 4 grid rows (query + 3 entries); at least those
        // rows must carry palette instances.
        let rows_touched: std::collections::BTreeSet<u16> = with_palette
            .iter()
            .filter(|i| i.flags == 0)
            .map(|i| i.grid_pos[1])
            .collect();
        assert!(
            rows_touched.len() >= 4,
            "query row plus three entry rows must all draw: {rows_touched:?}"
        );
        assert!(
            with_palette
                .iter()
                .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
            "the palette text must emit glyph instances"
        );

        snap.command_palette = None;
        let mut reclosed = Vec::new();
        rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
        assert_eq!(
            reclosed.iter().filter(|i| i.flags == 0).count(),
            bg_before,
            "closing the palette removes its overlay instances"
        );
    }

    #[test]
    fn command_palette_overlay_shows_empty_state_for_zero_results() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(30, 8));
        let theme = Theme::new();
        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        snap.command_palette = Some(crate::CommandPaletteSnapshot {
            query: "zzzzzz".to_string(),
            rows: Vec::new(),
            selected: 0,
        });

        let mut with_empty_palette = Vec::new();
        rebuild_cell_instances(&mut with_empty_palette, &snap, &mut font, &theme, false);

        let rows_touched: std::collections::BTreeSet<u16> = with_empty_palette
            .iter()
            .filter(|i| i.flags == 0)
            .map(|i| i.grid_pos[1])
            .collect();
        assert!(
            rows_touched.len() >= 3,
            "query row, spacer, and empty-state row must all draw: {rows_touched:?}"
        );
        assert!(
            with_empty_palette
                .iter()
                .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
            "the empty-state text must emit glyph instances"
        );
    }

    #[test]
    fn confirm_dialog_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };

        let mut terminal = Terminal::new(GridSize::new(40, 10));
        let theme = Theme::new();

        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        assert_eq!(
            snap.confirm_dialog, None,
            "from_terminal defaults to no dialog"
        );

        let mut closed = Vec::new();
        rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
        let bg_before = closed.iter().filter(|i| i.flags == 0).count();

        snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
            message: "Paste 3 line(s) of text?".to_string(),
            hint: "Enter: confirm    Esc: cancel".to_string(),
        });
        let mut with_dialog = Vec::new();
        rebuild_cell_instances(&mut with_dialog, &snap, &mut font, &theme, false);

        assert!(
            with_dialog.iter().filter(|i| i.flags == 0).count() > bg_before,
            "opening the dialog must add background quads"
        );
        // A message row and a hint row.
        let rows_touched: std::collections::BTreeSet<u16> = with_dialog
            .iter()
            .filter(|i| i.flags == 0)
            .map(|i| i.grid_pos[1])
            .collect();
        assert!(
            rows_touched.len() >= 2,
            "message and hint rows must both draw: {rows_touched:?}"
        );
        assert!(
            with_dialog
                .iter()
                .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
            "the dialog text must emit glyph instances"
        );

        snap.confirm_dialog = None;
        let mut reclosed = Vec::new();
        rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
        assert_eq!(
            reclosed.iter().filter(|i| i.flags == 0).count(),
            bg_before,
            "closing the dialog removes its overlay instances"
        );
    }

    #[test]
    fn overlay_block_emits_border_on_all_four_edges() {
        // A 4-wide x 3-tall block anchored at (5, 3): the border must touch
        // every perimeter cell (top row, bottom row, left column, right
        // column) and emit only decoration-pass rects.
        let m = metrics(18.0); // cell_w = 10, cell_h = 24
        let color = [10, 20, 30, 255];
        let mut inst = Vec::new();
        append_overlay_border(&mut inst, 5, 3, 4, 3, color, m);

        assert!(
            inst.iter()
                .all(|i| i.flags & CellInstance::FLAG_DECORATION != 0),
            "border rects are decoration-pass quads"
        );

        let at = |x: u16, y: u16| inst.iter().any(|i| i.grid_pos == [x, y]);
        // Top (y=3) and bottom (y=5) edges span columns 5..=8.
        for x in 5..=8 {
            assert!(at(x, 3), "top edge missing at col {x}");
            assert!(at(x, 5), "bottom edge missing at col {x}");
        }
        // Left (x=5) and right (x=8) edges span rows 3..=5.
        for y in 3..=5 {
            assert!(at(5, y), "left edge missing at row {y}");
            assert!(at(8, y), "right edge missing at row {y}");
        }
        // The right edge is inset to the cell's right (bearing.x > 0), the
        // bottom edge to the cell's bottom (bearing.y > 0).
        assert!(
            inst.iter()
                .any(|i| i.grid_pos == [8, 3] && i.bearing[0] > 0),
            "right edge sits at the cell's right pixel"
        );
        assert!(
            inst.iter()
                .any(|i| i.grid_pos == [5, 5] && i.bearing[1] > 0),
            "bottom edge sits at the cell's bottom pixel"
        );
    }

    #[test]
    fn confirm_dialog_padding_rows_collapse_on_small_grids() {
        let Some(mut font) = font_with_rasterized_m() else {
            return;
        };
        let theme = Theme::new();

        // A tall grid gets a blank pad row above and below the two text rows
        // (4 distinct overlay bg rows); a short grid falls back to the compact
        // message + hint form (2 rows).
        let tall = confirm_dialog_bg_rows(&mut font, &theme, 40, 10);
        assert_eq!(tall, 4, "tall grid pads the dialog to 4 rows");
        let short = confirm_dialog_bg_rows(&mut font, &theme, 40, 4);
        assert_eq!(short, 2, "short grid collapses to the compact 2-row form");
    }

    /// Count the distinct grid rows carrying confirm-dialog overlay background
    /// quads (`flags == 0`) for a `cols` x `rows` grid. The default block
    /// cursor paints a `FLAG_CURSOR` quad, not a plain bg quad, so it is not
    /// counted here.
    fn confirm_dialog_bg_rows(font: &mut FontGrid, theme: &Theme, cols: u16, rows: u16) -> usize {
        let mut terminal = Terminal::new(GridSize::new(cols, rows));
        let mut snap = FrameSnapshot::from_terminal(&mut terminal);
        snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
            message: "Paste 3 line(s)?".to_string(),
            hint: "Enter: confirm    Esc: cancel".to_string(),
        });
        let mut inst = Vec::new();
        rebuild_cell_instances(&mut inst, &snap, font, theme, false);
        inst.iter()
            .filter(|i| i.flags == 0)
            .map(|i| i.grid_pos[1])
            .collect::<std::collections::BTreeSet<u16>>()
            .len()
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
            abs_row_base: 0,
            active_is_alt: false,
            cols: 1,
            rows_n: 3,
            focused: true,
            cursor_blink_visible: true,
            hover_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            image_placements: Vec::new(),
            images: Vec::new(),
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
    fn atlas_eviction_epoch_forces_full_row_cache_rebuild() {
        // Regression: row-cache glyph instances store concrete atlas
        // coordinates. When FontGrid evicts a glyph slot, those coordinates
        // can later be reused by another glyph, so an otherwise-clean frame
        // must not reuse the old row instances.
        let mut font =
            match FontGrid::new_with_capped_atlas_for_tests(14.0, FontConfig::default(), 48) {
                Ok(font) => font,
                Err(err) => {
                    eprintln!("skipping: no system monospace font available: {err}");
                    return;
                }
            };
        let theme = Theme::new();
        let mut cache = PaneRenderCache::empty();
        let snap = baseline_snapshot(['A', 'B', 'C']);
        let mut instances = Vec::new();

        let first =
            rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
        assert_eq!(
            first.rows_rebuilt, 3,
            "fresh pane cache should build every visible row"
        );
        instances.clear();

        let before_eviction = font.atlas_eviction_generation();
        for ch in ('!'..='~').chain('\u{3041}'..='\u{3096}') {
            font.get_or_raster(ch);
            if font.atlas_eviction_generation() > before_eviction {
                break;
            }
        }
        assert!(
            font.atlas_eviction_generation() > before_eviction,
            "capped atlas must evict after flooding distinct glyphs"
        );

        let second =
            rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
        assert!(
            second.rows_rebuilt >= 3,
            "atlas eviction must force a full row-cache rebuild even when row_dirty is false"
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
        // FM-11: each of the 7 pane-wide triggers bundled into
        // `FrameInvalidationKey` must force EVERY row in the pane dirty when
        // it differs from the previous frame, even though `row_dirty` says
        // no cell changed. Cursor movement (the narrower 8th case) instead
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

        // 1. abs_row_base (viewport scroll offset, session-absolute).
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.abs_row_base = 1;
            let rebuilt = rebuild_twice(101, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 3,
                "abs_row_base change must force a full pane rebuild"
            );
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

        // 7. hover_link (Cmd+hover underline target). Hover changes carry no
        // terminal damage at all (no cell/pty mutation), so this trigger is
        // what makes the underline actually repaint.
        {
            let snap_a = baseline_snapshot(['A', 'B', 'C']);
            let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
            snap_b.hover_link = Some(HoverLink::Registry(0));
            let rebuilt = rebuild_twice(109, &snap_a, &theme, &snap_b, &theme);
            assert_eq!(
                rebuilt, 3,
                "a hover_link change must force a full pane rebuild"
            );
        }

        // 8. cursor movement — the narrower case: dirties exactly the two
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
    fn abs_row_base_change_forces_rebuild_even_when_row_base_collides() {
        // Regression: the invalidation key must ride the session-absolute
        // `abs_row_base`, not the storage-index `row_base`. A scroll that evicts
        // and pushes an equal number of rows reproduces the same `row_base`
        // while `abs_row_base` advances; keying on `row_base` would cache-hit and
        // paint stale history rows. Same row_base + different abs_row_base must
        // still force a full pane rebuild.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping abs_row_base collision test");
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
        let pane = PaneId::new(201);

        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        // The bug scenario: identical storage-index row_base, advanced absolute.
        assert_eq!(snap_a.row_base, snap_b.row_base);
        snap_b.abs_row_base = snap_a.abs_row_base + 3;

        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap_a,
            }],
            &mut font,
            &theme,
        );
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &snap_b,
            }],
            &mut font,
            &theme,
        );

        assert_eq!(
            renderer.rows_rebuilt_last_frame(),
            3,
            "abs_row_base change must force a full pane rebuild despite an unchanged row_base"
        );
    }

    #[test]
    fn active_screen_switch_forces_rebuild_even_when_rows_are_clean() {
        // Regression: switching from alt back to primary can expose a screen
        // whose rows did not mutate while it was hidden. If the row cache key
        // ignores the active screen identity, the clean primary frame can reuse
        // alt-screen glyph instances.
        let Some((device, queue)) = device_queue() else {
            eprintln!("no wgpu adapter available — skipping active-screen switch test");
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
        renderer.resize(PixelSize { w: 96, h: 96 });

        let theme = Theme::new();
        let pane = PaneId::new(301);
        let rect = PaneRect::new(0, 0, 96, 96);
        let mut terminal = Terminal::new(GridSize::new(3, 3));
        terminal.primary.grid[0].cells[0].ch = 'P';
        terminal.primary.grid[1].cells[0].ch = 'R';
        terminal.primary.grid[2].cells[0].ch = 'I';

        let primary = FrameSnapshot::from_terminal(&mut terminal);
        assert!(!primary.active_is_alt);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &primary,
            }],
            &mut font,
            &theme,
        );

        let mut stream = Stream::new();
        stream.feed(b"\x1b[?1049hALT", &mut terminal);
        let alt = FrameSnapshot::from_terminal(&mut terminal);
        assert!(alt.active_is_alt);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &alt,
            }],
            &mut font,
            &theme,
        );

        stream.feed(b"\x1b[?1049l", &mut terminal);
        let primary_again = FrameSnapshot::from_terminal(&mut terminal);
        assert!(!primary_again.active_is_alt);
        assert!(
            primary_again.row_dirty.iter().all(|dirty| !dirty),
            "primary rows were not mutated while hidden, so only screen identity can invalidate"
        );
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: &primary_again,
            }],
            &mut font,
            &theme,
        );

        assert_eq!(
            renderer.rows_rebuilt_last_frame(),
            3,
            "alt -> primary switch must rebuild every row even when row_dirty is clean"
        );
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
