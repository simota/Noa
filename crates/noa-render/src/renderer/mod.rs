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

use crate::background_image::{BackgroundImage, BackgroundImageLayer};
use crate::draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
use crate::image_layer::{ImageDraw, ImageLayer};
use crate::instance::{CellInstance, PaneUniformParams, populate_pane_uniform};
use crate::pipeline::CellPipeline;
use crate::segment::{SegmentCell, ShapeRun, segment_row};
use crate::snapshot::{
    CommandPaletteSnapshot, ConfirmDialogSnapshot, FrameSnapshot, HoverLink,
    ImagePlacementSnapshot, PaletteRow, SnapshotImage,
};
use crate::theme::{OverlayStyle, Theme, rgba};

const DEFAULT_PANE_ID: PaneId = PaneId::new(0);
/// Pre-first-rebuild fallbacks only: both colors are re-derived from the
/// active theme every [`Renderer::rebuild_panes`] (divider = the overlay
/// border tone, focus = the shared [`crate::theme::UI_ACCENT`]).
const DIVIDER_RGBA: [u8; 4] = [82, 82, 82, 255];
const FOCUS_INDICATOR_RGBA: [u8; 4] = [0x14, 0xa2, 0xff, FOCUS_INDICATOR_ALPHA];
/// The focused-pane outline's opacity (~90%), kept independent of the accent
/// color so the theme-derived recompute preserves it.
const FOCUS_INDICATOR_ALPHA: u8 = 230;
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
    /// `FrameSnapshot::row_base` as of the last rebuild. The scroll fast
    /// path requires it to have advanced by exactly `scroll_shift` (i.e. no
    /// scrollback eviction happened), because selection/search highlights
    /// are anchored in this storage-index space.
    prev_row_base: Option<usize>,
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
            prev_row_base: None,
        }
    }
}

/// The wgpu instanced-cell renderer. Windowing-agnostic: it receives an
/// already-created `Device`/`Queue`/surface format and never touches
/// `winit` or `wgpu::Surface`.
pub struct Renderer {
    cell: CellPipeline,
    image_layer: ImageLayer,
    /// Terminal background image (design: `background-image*`). Drawn once per
    /// frame in the lowest z band — below every pane's background quad, above
    /// the `LoadOp::Clear` color — spanning the whole surface. Empty until
    /// `noa-app` calls [`Renderer::set_background_image`].
    background_image_layer: BackgroundImageLayer,
    mask_atlas_texture: wgpu::Texture,
    mask_atlas_view: wgpu::TextureView,
    color_atlas_texture: wgpu::Texture,
    color_atlas_view: wgpu::TextureView,
    pane_gpu: Vec<PaneGpuState>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<CellInstance>,
    /// CPU shadow of what `instance_buffer` currently holds, so
    /// `upload_instances` can upload only the changed byte range instead of
    /// the whole list every frame.
    uploaded_instances: Vec<CellInstance>,
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
    /// Split-divider fill, re-derived from the active theme each
    /// [`Renderer::rebuild_panes`] (the overlay border tone) so the hairline
    /// tracks light/dark themes instead of staying a fixed gray.
    divider_color: [u8; 4],
    /// Focused-pane outline: the shared UI accent at
    /// [`FOCUS_INDICATOR_ALPHA`], converted to the target's output space each
    /// [`Renderer::rebuild_panes`].
    focus_indicator_color: [u8; 4],
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
        let background_image_layer = BackgroundImageLayer::new(device, format);

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

        let (color_w, color_h) = font.color_atlas_size();
        let color_atlas_texture = create_atlas_texture(
            device,
            color_w,
            color_h,
            color_atlas_format(format.is_srgb()),
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
            background_image_layer,
            mask_atlas_texture,
            mask_atlas_view,
            color_atlas_texture,
            color_atlas_view,
            pane_gpu: Vec::new(),
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
            uploaded_instances: Vec::new(),
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
            divider_color: DIVIDER_RGBA,
            focus_indicator_color: FOCUS_INDICATOR_RGBA,
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

    /// Set (or clear) the terminal background image. A startup-time setting:
    /// `noa-app` decodes the PNG once and calls this right after construction,
    /// per surface. The texture is uploaded here; placement (fit/position/
    /// repeat) is resolved against the surface size every frame in
    /// [`Renderer::draw_panes`], so no per-frame invalidation or resize hook is
    /// needed. `None` disables the image. The image quad's alpha is scaled only
    /// by `background-image-opacity`, never by `background-opacity`, matching
    /// Ghostty (spec NFR-3 / AC-9).
    pub fn set_background_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: Option<BackgroundImage>,
    ) {
        self.background_image_layer.set_image(device, queue, image);
    }

    /// Whether a background image is currently set (test/introspection).
    pub fn has_background_image(&self) -> bool {
        self.background_image_layer.has_image()
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
        let mut first_clear_color = None;
        let mut rows_rebuilt_total: u64 = 0;
        // Cross-pane eviction guard: `rebuild_pane_cached` only stabilizes each
        // pane against evictions caused by its OWN rebuild. A later pane's
        // rasterization can still evict atlas rectangles that an
        // earlier-rebuilt pane's instances already reference, and nothing
        // requests another frame — so the stale coordinates would stay on
        // screen until the next unrelated redraw. Detect that here and redo
        // the whole layout against the settled epoch.
        for cross_pane_pass in 0..MAX_ATLAS_EVICTION_REBUILD_PASSES {
            self.instances.clear();
            self.pane_instances.clear();
            self.pane_images.clear();
            self.pane_layout.clear();
            self.divider_range = 0..0;

            first_clear_color = None;
            rows_rebuilt_total = 0;
            let eviction_before = font.atlas_eviction_generation();
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

            // A single pane is already internally stabilized by
            // `rebuild_pane_cached`'s own retry loop; only multi-pane layouts
            // can leave an earlier pane stale.
            if panes.len() <= 1 || font.atlas_eviction_generation() == eviction_before {
                break;
            }
            if cross_pane_pass + 1 == MAX_ATLAS_EVICTION_REBUILD_PASSES {
                log::warn!(
                    "glyph atlas kept evicting across {MAX_ATLAS_EVICTION_REBUILD_PASSES} cross-pane rebuild passes; some panes may reference stale atlas rectangles for one frame"
                );
                break;
            }
            // Every pane rebuilt before the eviction may reference reclaimed
            // atlas rectangles; drop all keys so the next pass rebuilds
            // against the settled epoch.
            for cache in self.pane_render_cache.values_mut() {
                cache.key = None;
            }
        }

        // Drop caches for panes that are no longer visible (split closed) so
        // this map doesn't grow unbounded over a long session.
        self.pane_render_cache
            .retain(|id, _| panes.iter().any(|pane| pane.pane == *id));

        if let Some(clear_color) = first_clear_color {
            self.clear_color = apply_background_opacity(clear_color, self.background_opacity);
        }
        // Theme-derived overlay colors for the divider / focus-indicator
        // instances emitted later in `prepare_overlay_instances` (which has no
        // theme access of its own).
        let overlay = OverlayStyle::from_theme(theme);
        self.divider_color = to_u8_color(surface_output_rgba(
            overlay.border(),
            self.target_format_is_srgb,
        ));
        let accent = to_u8_color(surface_output_rgba(
            rgba(crate::theme::UI_ACCENT),
            self.target_format_is_srgb,
        ));
        self.focus_indicator_color = [accent[0], accent[1], accent[2], FOCUS_INDICATOR_ALPHA];
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
        // Background image: one full-surface quad below everything, resolved
        // against the current viewport (recomputes on resize — spec AC-13/14).
        // A tiling `repeat` is still one quad (Repeat sampler), so this is O(1).
        let bg_image_draw = self
            .background_image_layer
            .build_draw(device, queue, self.viewport);

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

            // Draw the terminal background image first, spanning the whole
            // surface (default full-framebuffer scissor), so it sits below every
            // pane's background quad and above the clear color. Independent of
            // the per-pane kitty `BelowBackground` band.
            self.background_image_layer
                .draw(&mut pass, bg_image_draw.as_ref());

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
                format: color_atlas_format(self.target_format_is_srgb),
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
        let mut fresh_buffer = false;
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = (self.instances.len() * 2).max(self.instance_capacity * 2);
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("noa-instance-buffer"),
                size: (self.instance_capacity * std::mem::size_of::<CellInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            fresh_buffer = true;
        }
        if self.instances.is_empty() {
            self.uploaded_instances.clear();
            return;
        }
        // Diff against the CPU shadow of the buffer's contents and upload
        // only the changed range. Any length change re-uploads in full (all
        // instances past the first changed row shift anyway).
        if fresh_buffer || self.uploaded_instances.len() != self.instances.len() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
            self.uploaded_instances.clone_from(&self.instances);
            return;
        }
        let Some(first) = self
            .instances
            .iter()
            .zip(&self.uploaded_instances)
            .position(|(new, old)| new != old)
        else {
            return; // frame is byte-identical to what the GPU already holds
        };
        let last = self
            .instances
            .iter()
            .zip(&self.uploaded_instances)
            .rposition(|(new, old)| new != old)
            .expect("a first diff implies a last diff");
        queue.write_buffer(
            &self.instance_buffer,
            (first * std::mem::size_of::<CellInstance>()) as u64,
            bytemuck::cast_slice(&self.instances[first..=last]),
        );
        self.uploaded_instances[first..=last].copy_from_slice(&self.instances[first..=last]);
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
                        self.instances
                            .push(pixel_overlay_instance(*rect, self.divider_color));
                    }
                    let end = self.instances.len() as u32;
                    self.divider_range = start..end;
                }
                DrawOp::FocusIndicator { rects, .. } => {
                    let start = self.instances.len() as u32;
                    for rect in rects {
                        self.instances
                            .push(pixel_overlay_instance(*rect, self.focus_indicator_color));
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

mod atlas;
mod cell;
mod color;
mod cursor;
mod overlay;
#[cfg(test)]
mod tests;

use atlas::*;
use cell::*;
use color::*;
use cursor::*;
use overlay::*;
pub use overlay::{
    ConfirmDialogLayout, PaletteLayout, command_palette_layout, confirm_dialog_layout,
};
