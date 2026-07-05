use noa_render::{OverlayStyle, command_palette_layout};

use super::render::ensure_scratch;
use super::*;

/// Corner radius (logical px) of the command-palette card (H).
const PALETTE_CARD_CORNER_RADIUS: f32 = 10.0;
/// Outer soft drop-shadow width (logical px) of the palette card (H).
const PALETTE_CARD_GLOW_WIDTH: f32 = 18.0;
/// Peak opacity of the card's drop shadow (fades to 0 over the glow width).
const PALETTE_CARD_SHADOW_ALPHA: f32 = 0.65;
/// Interior padding between the card edge and the text block, in cell units
/// (horizontal is in `cell_w`, vertical in `cell_h`), so the query row and
/// list breathe instead of hugging the rounded border.
const PALETTE_CARD_PAD_X_CELLS: f32 = 1.25;
const PALETTE_CARD_PAD_Y_CELLS: f32 = 0.5;
/// Opacity of the modal scrim dimming the pane behind the open palette.
const PALETTE_SCRIM_ALPHA: u8 = 72;

/// Composite the open command palette as a single rounded card over the focused
/// pane (H). The block (query row + windowed list) is rasterized into a scratch
/// texture by the reused `palette_renderer`, then drawn as one rounded card:
/// a soft black drop shadow, the elevated surface, and a themed 1px border —
/// two card-pipeline passes (shadow+fill, then fill+border) over the same
/// texture. Runs inline in `redraw` after the panes and sidebar so the modal
/// always draws on top. The overlay's own square outline is dropped (the card
/// supplies the chrome); the hairline rule and accent bar ride inside the
/// texture.
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
) {
    let Some(layout) = command_palette_layout(palette, pane_cols, pane_rows) else {
        return;
    };
    let metrics = gpu.font.metrics();
    let (cell_w, cell_h) = (metrics.cell_w, metrics.cell_h);
    // Interior padding between the card edge and the text block, so rows
    // don't hug the rounded border. Applied as renderer padding: the grid
    // shifts inward and the clear color (the elevated surface) fills the rim.
    let pad_x = (PALETTE_CARD_PAD_X_CELLS * cell_w).round();
    let pad_y = (PALETTE_CARD_PAD_Y_CELLS * cell_h).round();
    let interior = GridPadding::new(pad_y, pad_x, pad_y, pad_x);
    let block_px = PixelSize {
        w: ((layout.block_cols as f32) * cell_w + 2.0 * pad_x)
            .ceil()
            .max(1.0) as u32,
        h: ((layout.block_rows as f32) * cell_h + 2.0 * pad_y)
            .ceil()
            .max(1.0) as u32,
    };

    // Lazily (re)build the reused block renderer + card pipeline for this
    // format, or when a font-size change moves the interior padding. With the
    // padding, grid cell (c,r) maps to texture pixel (pad + c*cell_w, pad +
    // r*cell_h) and the scratch is the block size plus the padded rim.
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

    // Card placement in window pixels: the block's grid origin within the pane,
    // offset by the pane's screen origin and the grid padding, pulled back by
    // the interior padding so the text block itself stays grid-aligned.
    let x = (pane_rect.x as f32 + padding.left + (layout.x0 as f32) * cell_w - pad_x)
        .round()
        .max(0.0) as u32;
    let y = (pane_rect.y as f32 + padding.top + (layout.y0 as f32) * cell_h - pad_y)
        .round()
        .max(0.0) as u32;
    let placement = |selected| CardTexturePlacement {
        texture_view: &gpu.palette_scratch.as_ref().unwrap().2,
        x,
        y,
        w: block_px.w,
        h: block_px.h,
        selected,
    };

    // Modal scrim: dim the whole pane behind the palette with a translucent
    // black card (radius/border/glow 0) so the modal visually lifts off the
    // terminal content. The 1x1 texture carries the opacity in its alpha.
    if gpu.palette_scrim.is_none() {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-command-palette-scrim"),
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
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, PALETTE_SCRIM_ALPHA],
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
        gpu.palette_scrim = Some((texture, view));
    }
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
    let border = style.border();
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
    card.overlay_texture_cards(
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
    );
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
