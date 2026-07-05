use noa_render::{OverlayStyle, command_palette_layout, confirm_dialog_layout};

use super::render::ensure_scratch;
use super::*;

/// Corner radius (logical px) of a modal overlay card (H) — the shared
/// large-card chrome radius.
const PALETTE_CARD_CORNER_RADIUS: f32 = crate::chrome::RADIUS_LG;
/// Outer soft drop-shadow width (logical px) of a modal card (H).
const PALETTE_CARD_GLOW_WIDTH: f32 = 18.0;
/// Peak opacity of the card's drop shadow (fades to 0 over the glow width).
const PALETTE_CARD_SHADOW_ALPHA: f32 = 0.65;
/// Interior padding between the card edge and the text block, in cell units
/// (horizontal is in `cell_w`, vertical in `cell_h`), so the query row and
/// list breathe instead of hugging the rounded border.
const PALETTE_CARD_PAD_X_CELLS: f32 = 1.25;
const PALETTE_CARD_PAD_Y_CELLS: f32 = 0.5;
/// Opacity of the modal scrim dimming the pane behind an open modal.
const PALETTE_SCRIM_ALPHA: u8 = 72;

/// Lazily (re)build the reused modal-card renderer + card pipeline for this
/// surface format, or when a font-size change moves the interior padding.
/// With the padding, grid cell (c,r) maps to texture pixel (pad + c*cell_w,
/// pad + r*cell_h) and the scratch is the block size plus the padded rim.
fn ensure_overlay_card_gpu(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    interior: GridPadding,
) {
    if gpu
        .palette_renderer
        .as_ref()
        .is_none_or(|renderer| renderer.target_format() != surface_format)
        || gpu.palette_padding != interior
    {
        gpu.palette_renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            surface_format,
            &mut gpu.font,
            interior,
        )
        .ok();
        gpu.palette_padding = interior;
    }
    ensure_card_pipeline(gpu, surface_format);
}

/// Ensure just the rounded-card pipeline exists — for composites that need no
/// mini-grid renderer (the scrollbar thumb and the visual-bell flash).
fn ensure_card_pipeline(gpu: &mut GpuState, surface_format: wgpu::TextureFormat) {
    if gpu
        .palette_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.palette_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            pipeline: CardPipeline::new(
                &gpu.device,
                surface_format,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
        });
    }
}

/// Ensure `slot` holds a 1x1 texture of exactly `rgba` (straight, the alpha
/// carries the overlay's opacity), creating it on first use.
fn ensure_tint_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<(wgpu::Texture, wgpu::TextureView)>,
    label: &'static str,
    rgba: [u8; 4],
) {
    if slot.is_some() {
        return;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    *slot = Some((texture, view));
}

/// Straight display-space `[f32; 4]` (an [`OverlayStyle`] getter) back to the
/// 8-bit `Rgb` the synthetic-terminal feed path wants.
fn rgb_from_rgba(c: [f32; 4]) -> Rgb {
    Rgb::new(
        (c[0] * 255.0).round() as u8,
        (c[1] * 255.0).round() as u8,
        (c[2] * 255.0).round() as u8,
    )
}

/// Ensure the shared 1x1 scrim texture exists (its alpha carries the modal
/// scrim opacity).
fn ensure_scrim(gpu: &mut GpuState) {
    let GpuState {
        device,
        queue,
        palette_scrim,
        ..
    } = gpu;
    ensure_tint_texture(
        device,
        queue,
        palette_scrim,
        "noa-command-palette-scrim",
        [0, 0, 0, PALETTE_SCRIM_ALPHA],
    );
}

/// Composite the already-rasterized `palette_scratch` block as a modal card
/// over the pane: a translucent scrim dimming the whole pane, then a soft
/// black drop shadow, then the elevated surface with a themed 1px border —
/// two card-pipeline passes (shadow+fill, then fill+border) over the same
/// texture. Shared by the command palette and the confirm dialog so every
/// modal carries identical chrome. `opacity` scales all three passes (the
/// open fade-in); 1.0 is fully settled.
#[allow(clippy::too_many_arguments)]
fn composite_modal_card(
    gpu: &GpuState,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    pane_rect: PaneRect,
    x: u32,
    y: u32,
    block_px: PixelSize,
    border: [f32; 4],
    scale: f32,
    opacity: f32,
) {
    let placement = |selected| CardTexturePlacement {
        texture_view: &gpu.palette_scratch.as_ref().unwrap().2,
        x,
        y,
        w: block_px.w,
        h: block_px.h,
        selected,
    };

    let scrim_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    // Pass 1: fill + soft black drop shadow (selected → the shader's glow path).
    let shadow_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, PALETTE_CARD_SHADOW_ALPHA],
        corner_radius: PALETTE_CARD_CORNER_RADIUS * scale,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: PALETTE_CARD_GLOW_WIDTH * scale,
    };
    // Pass 2: fill + themed 1px border, no glow (unselected → the border path).
    let border_style = CardStyle {
        background: [0.0; 4],
        border_color: border,
        focus_color: border,
        corner_radius: PALETTE_CARD_CORNER_RADIUS * scale,
        border_width: 1.0 * scale,
        focus_width: 1.0 * scale,
        focus_glow_width: 0.0,
    };
    let card = &gpu.palette_card.as_ref().unwrap().pipeline;
    card.overlay_texture_cards_with_opacity(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &scrim_style,
        &[CardTexturePlacement {
            texture_view: &gpu.palette_scrim.as_ref().unwrap().1,
            x: pane_rect.x,
            y: pane_rect.y,
            w: pane_rect.w,
            h: pane_rect.h,
            selected: false,
        }],
        opacity,
    );
    card.overlay_texture_cards_with_opacity(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &shadow_style,
        &[placement(true)],
        opacity,
    );
    card.overlay_texture_cards_with_opacity(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &border_style,
        &[placement(false)],
        opacity,
    );
}

/// The interior padding (px) and padded block size (px) for a
/// `block_cols`x`block_rows` cell block under the current font metrics.
fn modal_block_geometry(metrics: noa_font::Metrics, cols: u16, rows: u16) -> (GridPadding, PixelSize) {
    let (cell_w, cell_h) = (metrics.cell_w, metrics.cell_h);
    let pad_x = (PALETTE_CARD_PAD_X_CELLS * cell_w).round();
    let pad_y = (PALETTE_CARD_PAD_Y_CELLS * cell_h).round();
    let interior = GridPadding::new(pad_y, pad_x, pad_y, pad_x);
    let block_px = PixelSize {
        w: ((cols as f32) * cell_w + 2.0 * pad_x).ceil().max(1.0) as u32,
        h: ((rows as f32) * cell_h + 2.0 * pad_y).ceil().max(1.0) as u32,
    };
    (interior, block_px)
}

/// Window-pixel origin of a cell block at grid `(x0, y0)` within `pane_rect`,
/// pulled back by the interior padding so the text block itself stays
/// grid-aligned.
#[allow(clippy::too_many_arguments)]
fn modal_block_origin(
    pane_rect: PaneRect,
    padding: GridPadding,
    metrics: noa_font::Metrics,
    x0: u16,
    y0: u16,
    interior: GridPadding,
) -> (u32, u32) {
    let x = (pane_rect.x as f32 + padding.left + (x0 as f32) * metrics.cell_w - interior.left)
        .round()
        .max(0.0) as u32;
    let y = (pane_rect.y as f32 + padding.top + (y0 as f32) * metrics.cell_h - interior.top)
        .round()
        .max(0.0) as u32;
    (x, y)
}

/// Composite the open command palette as a single rounded card over the focused
/// pane (H). The block (query row + windowed list) is rasterized into a scratch
/// texture by the reused `palette_renderer`, then composited as one modal card
/// (scrim, shadow, surface + border — see [`composite_modal_card`]). Runs
/// inline in `redraw` after the panes and sidebar so the modal always draws on
/// top. The overlay's own square outline is dropped (the card supplies the
/// chrome); the hairline rule and accent bar ride inside the texture.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn draw_command_palette_card(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    palette: &CommandPaletteSnapshot,
    pane_rect: PaneRect,
    pane_cols: u16,
    pane_rows: u16,
    padding: GridPadding,
    scale: f32,
    opacity: f32,
) {
    let Some(layout) = command_palette_layout(palette, pane_cols, pane_rows) else {
        return;
    };
    let metrics = gpu.font.metrics();
    let (interior, block_px) = modal_block_geometry(metrics, layout.block_cols, layout.block_rows);
    ensure_overlay_card_gpu(gpu, surface_format, interior);
    ensure_scratch(
        &mut gpu.palette_scratch,
        &gpu.device,
        block_px,
        surface_format,
        "noa-command-palette",
    );
    ensure_scrim(gpu);
    if gpu.palette_renderer.is_none() || gpu.palette_card.is_none() || gpu.palette_scratch.is_none()
    {
        return;
    }

    // Rasterize the windowed block (rows sliced to the visible window, selection
    // rebased) into the scratch texture. The block fills the mini grid exactly,
    // so the overlay draws at the mini grid's origin.
    let visible = &palette.rows[layout.offset..layout.offset + layout.shown];
    let mini = CommandPaletteSnapshot {
        query: palette.query.clone(),
        rows: visible.to_vec(),
        selected: palette.selected.saturating_sub(layout.offset),
        total_entries: palette.total_entries,
    };
    let style = OverlayStyle::from_theme(&gpu.theme);
    {
        let mut term = Terminal::new(GridSize::new(layout.block_cols, layout.block_rows));
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;
        snapshot.command_palette = Some(mini);
        let scratch_view = &gpu.palette_scratch.as_ref().unwrap().2;
        let renderer = gpu.palette_renderer.as_mut().unwrap();
        renderer.resize(block_px);
        renderer.set_clear_color(style.surface_bg());
        renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        renderer.draw(&gpu.device, &gpu.queue, scratch_view);
    }

    let (x, y) = modal_block_origin(pane_rect, padding, metrics, layout.x0, layout.y0, interior);
    composite_modal_card(
        gpu,
        view,
        surface_size,
        pane_rect,
        x,
        y,
        block_px,
        style.border(),
        scale,
        opacity,
    );
}

/// Composite the open confirmation dialog (paste protection / clipboard-read)
/// as a rounded modal card over the focused pane — the same scrim + shadow +
/// surface + border chrome as the command palette, so the two modals share one
/// visual language. Reuses the palette's renderer, scratch, and scrim; the two
/// composites fully submit before the other rasterizes, so same-frame reuse is
/// safe (same pattern as the sidebar's per-card scratch).
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn draw_confirm_dialog_card(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    dialog: &noa_render::ConfirmDialogSnapshot,
    pane_rect: PaneRect,
    pane_cols: u16,
    pane_rows: u16,
    padding: GridPadding,
    scale: f32,
) {
    let Some(layout) = confirm_dialog_layout(dialog, pane_cols, pane_rows) else {
        return;
    };
    let metrics = gpu.font.metrics();
    let (interior, block_px) = modal_block_geometry(metrics, layout.block_cols, layout.block_rows);
    ensure_overlay_card_gpu(gpu, surface_format, interior);
    ensure_scratch(
        &mut gpu.palette_scratch,
        &gpu.device,
        block_px,
        surface_format,
        "noa-command-palette",
    );
    ensure_scrim(gpu);
    if gpu.palette_renderer.is_none() || gpu.palette_card.is_none() || gpu.palette_scratch.is_none()
    {
        return;
    }

    let style = OverlayStyle::from_theme(&gpu.theme);
    {
        let mut term = Terminal::new(GridSize::new(layout.block_cols, layout.block_rows));
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;
        snapshot.confirm_dialog = Some(dialog.clone());
        let scratch_view = &gpu.palette_scratch.as_ref().unwrap().2;
        let renderer = gpu.palette_renderer.as_mut().unwrap();
        renderer.resize(block_px);
        renderer.set_clear_color(style.surface_bg());
        renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        renderer.draw(&gpu.device, &gpu.queue, scratch_view);
    }

    let (x, y) = modal_block_origin(pane_rect, padding, metrics, layout.x0, layout.y0, interior);
    // The dialog appears at full opacity — it is a blocking prompt, so it
    // must be legible the instant it opens.
    composite_modal_card(
        gpu,
        view,
        surface_size,
        pane_rect,
        x,
        y,
        block_px,
        style.border(),
        scale,
        1.0,
    );
}

/// Composite a small centered toast card — the `cols × rows` resize overlay —
/// over the window: the modal surface + border + soft shadow chrome, without
/// the scrim (the toast is informational, not modal). The text is fed through
/// a one-row synthetic terminal so it renders with the same font/metrics as
/// every other overlay.
pub(in crate::app) fn draw_toast_card(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    text: &str,
    scale: f32,
) {
    // The toast text is digits, spaces, and `×` — all one column per char.
    let cols = (text.chars().count().max(1)).min(u16::MAX as usize) as u16;
    let metrics = gpu.font.metrics();
    let (interior, block_px) = modal_block_geometry(metrics, cols, 1);
    ensure_overlay_card_gpu(gpu, surface_format, interior);
    ensure_scratch(
        &mut gpu.palette_scratch,
        &gpu.device,
        block_px,
        surface_format,
        "noa-command-palette",
    );
    if gpu.palette_renderer.is_none() || gpu.palette_card.is_none() || gpu.palette_scratch.is_none()
    {
        return;
    }

    let style = OverlayStyle::from_theme(&gpu.theme);
    {
        let fg = rgb_from_rgba(style.surface_fg());
        let bg = rgb_from_rgba(style.surface_bg());
        let mut term = Terminal::new(GridSize::new(cols, 1));
        term.set_base_colors(fg, bg, fg, gpu.theme.palette);
        let mut stream = Stream::new();
        stream.feed(b"\x1b[?7l", &mut term);
        stream.feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;
        let scratch_view = &gpu.palette_scratch.as_ref().unwrap().2;
        let renderer = gpu.palette_renderer.as_mut().unwrap();
        renderer.resize(block_px);
        renderer.set_clear_color(style.surface_bg());
        renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        renderer.draw(&gpu.device, &gpu.queue, scratch_view);
    }

    let x = (surface_size.w.saturating_sub(block_px.w)) / 2;
    let y = (surface_size.h.saturating_sub(block_px.h)) / 2;
    let border = style.border();
    let shadow_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, PALETTE_CARD_SHADOW_ALPHA],
        corner_radius: crate::chrome::RADIUS_MD * scale,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: crate::chrome::GLOW_SELECTED * scale,
    };
    let border_style = CardStyle {
        background: [0.0; 4],
        border_color: border,
        focus_color: border,
        corner_radius: crate::chrome::RADIUS_MD * scale,
        border_width: 1.0 * scale,
        focus_width: 1.0 * scale,
        focus_glow_width: 0.0,
    };
    let placement = |selected| CardTexturePlacement {
        texture_view: &gpu.palette_scratch.as_ref().unwrap().2,
        x,
        y,
        w: block_px.w,
        h: block_px.h,
        selected,
    };
    let card = &gpu.palette_card.as_ref().unwrap().pipeline;
    card.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &shadow_style,
        &[placement(true)],
    );
    card.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &border_style,
        &[placement(false)],
    );
}

/// One scrolled pane's scrollbar-thumb input: its screen rect and scroll
/// state, captured while the pane's `Terminal` lock was already held.
pub(in crate::app) struct ScrollThumb {
    pub(in crate::app) rect: PaneRect,
    /// Rows scrolled back from the bottom (> 0, or the thumb wouldn't draw).
    pub(in crate::app) offset: usize,
    /// Total scrollback rows available above the viewport.
    pub(in crate::app) scrollback: usize,
    /// Viewport height in rows.
    pub(in crate::app) viewport_rows: u16,
}

/// Composite a thin rounded scrollback thumb along each scrolled pane's right
/// edge. State-driven: a pane appears here only while scrolled back
/// (`viewport_offset > 0`), so the thumb vanishes the moment the view returns
/// to the bottom — no timers.
pub(in crate::app) fn draw_scrollbar_thumbs(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    thumbs: &[ScrollThumb],
    scale: f32,
) {
    if thumbs.is_empty() {
        return;
    }
    ensure_card_pipeline(gpu, surface_format);
    let style = OverlayStyle::from_theme(&gpu.theme);
    {
        let tint = rgb_from_rgba(style.muted_fg());
        let GpuState {
            device,
            queue,
            scrollbar_tex,
            ..
        } = gpu;
        ensure_tint_texture(
            device,
            queue,
            scrollbar_tex,
            "noa-scrollbar-thumb",
            [tint.r, tint.g, tint.b, 153], // ~60% opacity
        );
    }
    let (Some(card), Some((_, thumb_view))) =
        (gpu.palette_card.as_ref(), gpu.scrollbar_tex.as_ref())
    else {
        return;
    };

    let width = (6.0 * scale).round().max(2.0) as u32;
    let inset = (3.0 * scale).round() as u32;
    let min_h = (24.0 * scale).round() as u32;
    let thumb_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: width as f32 / 2.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    for thumb in thumbs {
        if thumb.offset == 0 || thumb.scrollback == 0 || thumb.rect.h == 0 {
            continue;
        }
        let total = (thumb.scrollback + thumb.viewport_rows as usize).max(1) as f32;
        let track_h = thumb.rect.h;
        let h = ((track_h as f32 * thumb.viewport_rows as f32 / total) as u32)
            .clamp(min_h.min(track_h), track_h);
        // 0 = scrolled to the very top, 1 = at the bottom.
        let pos = (thumb.scrollback - thumb.offset.min(thumb.scrollback)) as f32
            / thumb.scrollback as f32;
        let y = thumb.rect.y + ((track_h.saturating_sub(h)) as f32 * pos).round() as u32;
        let x = (thumb.rect.x + thumb.rect.w).saturating_sub(width + inset);
        card.pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            view,
            surface_size,
            &thumb_style,
            &[CardTexturePlacement {
                texture_view: thumb_view,
                x,
                y,
                w: width,
                h,
                selected: false,
            }],
        );
    }
}

/// Composite the `visual-bell` flash: a brief translucent white wash over the
/// whole window.
pub(in crate::app) fn draw_bell_flash(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
) {
    ensure_card_pipeline(gpu, surface_format);
    {
        let GpuState {
            device,
            queue,
            bell_flash_tex,
            ..
        } = gpu;
        ensure_tint_texture(
            device,
            queue,
            bell_flash_tex,
            "noa-bell-flash",
            [255, 255, 255, 56], // ~22% white wash
        );
    }
    let (Some(card), Some((_, flash_view))) =
        (gpu.palette_card.as_ref(), gpu.bell_flash_tex.as_ref())
    else {
        return;
    };
    let flash_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    card.pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &flash_style,
        &[CardTexturePlacement {
            texture_view: flash_view,
            x: 0,
            y: 0,
            w: surface_size.w,
            h: surface_size.h,
            selected: false,
        }],
    );
}
