use std::fmt::Write as _;

use super::*;

const SIDEBAR_CARD_RULE_HEIGHT: f32 = 1.0;
const SIDEBAR_CARD_RULE_BORDER_MIX: f32 = 0.34;
const SIDEBAR_CARD_STATIC_FILL_OPACITY: f32 = 0.0;

/// Rasterize one synthetic sidebar grid (background + positioned text/dots)
/// with the reused `Renderer` into `view`. `base_bg` supplies the clear RGB and
/// default cell background; `bg_opacity` scales that clear alpha (0.0 makes the
/// texture text-only, 1.0 keeps it fully opaque). The card composite shader
/// passes the sampled texture alpha through (`card.wgsl`), so callers decide
/// whether a scratch is a surface or just foreground content.
#[allow(clippy::too_many_arguments)]
fn rasterize_runs(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut FontGrid,
    theme: &Theme,
    view: &wgpu::TextureView,
    size: PixelSize,
    grid: GridSize,
    base_bg: Rgb,
    bg_opacity: f32,
    runs: &[SidebarTextRun],
) {
    let mut term = Terminal::new(grid);
    term.set_base_colors(chrome().fg, base_bg, chrome().fg, theme.palette);
    let mut stream = Stream::new();
    // Autowrap off so a long cwd/preview clips at the right margin instead of
    // wrapping to the next row and shifting every run below it.
    stream.feed(b"\x1b[?7l", &mut term);
    let mut feed = String::new();
    for run in runs {
        feed.clear();
        // CUP is 1-based; position, optional bold, truecolor fg (+bg), write, reset.
        let _ = write!(feed, "\x1b[{};{}H", run.row + 1, run.col + 1);
        if run.bold {
            let _ = write!(feed, "\x1b[1m");
        }
        let _ = write!(feed, "\x1b[38;2;{};{};{}m", run.fg.r, run.fg.g, run.fg.b);
        if let Some(bg) = run.bg {
            let _ = write!(feed, "\x1b[48;2;{};{};{}m", bg.r, bg.g, bg.b);
        }
        let _ = write!(feed, "{}\x1b[0m", run.text);
        stream.feed(feed.as_bytes(), &mut term);
    }
    let mut snapshot = FrameSnapshot::from_terminal(&mut term);
    snapshot.cursor.visible = false;

    renderer.resize(size);
    renderer.rebuild_cells(&snapshot, font, theme);
    let mut clear_color = rgb_to_rgba(base_bg);
    clear_color[3] *= bg_opacity.clamp(0.0, 1.0);
    renderer.set_clear_color(clear_color);
    renderer.sync_atlas(device, queue, font);
    renderer.draw(device, queue, view);
}

/// Ensure `slot` holds a scratch render texture of exactly `size`/`format`,
/// reallocating only when either changes (reused frame-to-frame — F2).
/// Returns whether it actually (re)built the texture, so `ChromeTextures`
/// call sites can feed [`ChromeTextures::record_rebuild`] (NFR-2/AC-18).
#[must_use]
pub(super) fn ensure_scratch(
    slot: &mut Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    device: &wgpu::Device,
    size: PixelSize,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> bool {
    let size = PixelSize {
        w: size.w.max(1),
        h: size.h.max(1),
    };
    let rebuilt = slot
        .as_ref()
        .is_none_or(|(s, t, _)| *s != size || t.format() != format);
    if rebuilt {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.w,
                height: size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        *slot = Some((size, texture, view));
    }
    rebuilt
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SidebarFontCacheKey {
    atlas_identity: u64,
    mask_atlas_generation: u64,
    color_atlas_generation: u64,
    atlas_eviction_generation: u64,
}

impl SidebarFontCacheKey {
    fn from_font(font: &FontGrid) -> Self {
        Self {
            atlas_identity: font.atlas_identity(),
            mask_atlas_generation: font.mask_atlas_generation(),
            color_atlas_generation: font.color_atlas_generation(),
            atlas_eviction_generation: font.atlas_eviction_generation(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct SidebarRasterCacheKey {
    surface_format: wgpu::TextureFormat,
    padding: GridPadding,
    theme: Theme,
    chrome: crate::chrome::ChromePalette,
    font: SidebarFontCacheKey,
    model: SidebarDrawModel,
}

impl SidebarRasterCacheKey {
    fn new(
        surface_format: wgpu::TextureFormat,
        padding: GridPadding,
        theme: &Theme,
        chrome: crate::chrome::ChromePalette,
        font: &FontGrid,
        model: &SidebarDrawModel,
    ) -> Self {
        Self {
            surface_format,
            padding,
            theme: theme.clone(),
            chrome,
            font: SidebarFontCacheKey::from_font(font),
            model: model.clone(),
        }
    }
}

fn sidebar_cache_hit(
    previous: Option<&SidebarRasterCacheKey>,
    next: &SidebarRasterCacheKey,
) -> bool {
    previous.is_some_and(|previous| previous == next)
}

fn composite_sidebar_band_cache(
    gpu: &GpuState,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    model: &SidebarDrawModel,
) {
    let flat_style = CardStyle {
        background: rgb_to_rgba(active_theme(&gpu.theme, &gpu.preview_theme).default_bg),
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, 1.0],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: SEAM_SHADOW_WIDTH * model.scale,
    };
    let (Some(band_card), Some((_, _, band_view))) = (
        gpu.chrome_textures.sidebar_band_card.as_ref(),
        gpu.chrome_textures.sidebar_band.as_ref(),
    ) else {
        return;
    };
    band_card.pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        view,
        surface_size,
        &flat_style,
        &[CardTexturePlacement {
            texture_view: band_view,
            x: 0,
            y: 0,
            w: model.inset,
            h: model.height,
            selected: true,
        }],
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidebarCardFrame {
    Resting,
    Selected,
    Attention,
}

fn sidebar_card_frame(selected: bool, attention: bool) -> SidebarCardFrame {
    if selected {
        SidebarCardFrame::Selected
    } else if attention {
        SidebarCardFrame::Attention
    } else {
        SidebarCardFrame::Resting
    }
}

/// Rasterize the sidebar and composite it onto `view` at the window's left
/// inset via the reused card pipeline: a flat backdrop matching the terminal
/// theme's background (so the band reads as one surface with the panes), then
/// each fully-visible card as a square, borderless row with state carried by
/// its fill and status markers, then the optional `…` menu popup above them
/// all. Runs inline in `redraw` with the already-borrowed `gpu`, so the model
/// must be prebuilt (no `self` here).
///
/// The body is a straight-line sequence of self-contained overlay passes, each
/// factored into its own `draw_sidebar_*` helper so the draw order stays
/// legible; every helper stamps into the shared offscreen band texture and a
/// cache hit reuses that finished texture without touching any of them.
pub(in crate::app) fn draw_sidebar_band(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    padding: GridPadding,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    model: &SidebarDrawModel,
) {
    ensure_sidebar_pipelines(gpu, surface_format, padding);
    let band_size = PixelSize {
        w: model.inset.max(1),
        h: model.height.max(1),
    };
    if !ensure_sidebar_band_texture(gpu, band_size, surface_format) {
        return;
    }

    let next_cache_key = SidebarRasterCacheKey::new(
        surface_format,
        padding,
        active_theme(&gpu.theme, &gpu.preview_theme),
        chrome(),
        &gpu.sidebar_font,
        model,
    );
    if sidebar_cache_hit(
        gpu.chrome_textures.sidebar_raster_cache_key.as_ref(),
        &next_cache_key,
    ) {
        composite_sidebar_band_cache(gpu, view, surface_size, model);
        return;
    }

    ensure_card_menu_scratch(gpu, model, surface_format);

    // Each pass stamps one overlay into the shared band texture, in draw order:
    // band text, seam hairline, toolbar `+`, cards (+ accent bars), inter-row
    // rules, drag float/indicator, then the `…` menu popup above them all.
    draw_sidebar_band_runs(gpu, model, band_size);
    draw_sidebar_divider(gpu, model, band_size, surface_format);
    draw_sidebar_new_button(gpu, model, band_size, surface_format);
    draw_sidebar_cards(gpu, model, band_size, surface_format);
    draw_sidebar_rules(gpu, model, band_size, surface_format);
    draw_sidebar_drag(gpu, model, band_size, surface_format);
    draw_sidebar_menu(gpu, model, band_size);

    gpu.chrome_textures.sidebar_raster_cache_key = Some(next_cache_key);
    composite_sidebar_band_cache(gpu, view, surface_size, model);
}

/// Lazily (re)build the reused band renderer + the two card pipelines for this
/// surface format. Each is rebuilt only when absent or when the format changed.
fn ensure_sidebar_pipelines(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    padding: GridPadding,
) {
    if gpu
        .chrome_textures
        .sidebar_renderer
        .as_ref()
        .is_none_or(|renderer| renderer.target_format() != surface_format)
    {
        let pipelines = gpu.pipelines.get(&gpu.device, surface_format);
        let sidebar_font_atlases = gpu.sidebar_font_atlases.get(
            &gpu.device,
            &gpu.queue,
            surface_format,
            &gpu.sidebar_font,
        );
        gpu.chrome_textures.sidebar_renderer = Renderer::with_pipelines(
            &gpu.device,
            &gpu.queue,
            &pipelines,
            &sidebar_font_atlases,
            &mut gpu.sidebar_font,
            padding,
        )
        .ok();
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if gpu
        .chrome_textures
        .sidebar_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.chrome_textures.sidebar_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            // Static sidebar cards now render as transparent text layers over a
            // seamless band; alpha blending preserves already-drawn chrome where
            // those layers have no fill.
            pipeline: CardPipeline::new(
                &gpu.device,
                surface_format,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
        });
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if gpu
        .chrome_textures
        .sidebar_band_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.chrome_textures.sidebar_band_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            // The band backdrop is transparent outside its text runs; plain
            // alpha blending leaves the pane pass's clear color + background
            // image untouched there, so the band background is pixel-identical
            // to the panes'.
            pipeline: CardPipeline::new(
                &gpu.device,
                surface_format,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
        });
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
}

/// Ensure the offscreen band texture matches `band_size`/`format` (dropping the
/// raster cache key on a realloc so the reused texture is repainted), then
/// report whether every pipeline + the band texture is present — the invariant
/// the rest of the draw path unwraps against.
#[must_use]
fn ensure_sidebar_band_texture(
    gpu: &mut GpuState,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) -> bool {
    let sidebar_band_rebuilt = ensure_scratch(
        &mut gpu.chrome_textures.sidebar_band,
        &gpu.device,
        band_size,
        surface_format,
        "noa-sidebar-band",
    );
    if sidebar_band_rebuilt {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
        gpu.chrome_textures.sidebar_raster_cache_key = None;
    }
    gpu.chrome_textures.sidebar_renderer.is_some()
        && gpu.chrome_textures.sidebar_card.is_some()
        && gpu.chrome_textures.sidebar_band_card.is_some()
        && gpu.chrome_textures.sidebar_band.is_some()
}

/// Ensure the per-card and `…` menu scratch textures exist for this frame's
/// model. Both are reused across cards/frames; sizing them here keeps the card
/// and menu passes free of allocation bookkeeping.
fn ensure_card_menu_scratch(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    surface_format: wgpu::TextureFormat,
) {
    if !model.cards.is_empty()
        && ensure_scratch(
            &mut gpu.chrome_textures.sidebar_card_tex,
            &gpu.device,
            PixelSize {
                w: model.card_w,
                h: model.card_h,
            },
            surface_format,
            "noa-sidebar-card",
        )
    {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if let Some(menu) = &model.menu
        && ensure_scratch(
            &mut gpu.chrome_textures.sidebar_menu_tex,
            &gpu.device,
            PixelSize {
                w: menu.rect.w,
                h: menu.rect.h,
            },
            surface_format,
            "noa-sidebar-menu",
        )
    {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
}

/// Pass 1 — band text runs over a fully transparent base. The rest of the draw
/// path stamps every sidebar overlay into this same offscreen texture; a cache
/// hit reuses that finished texture instead.
fn draw_sidebar_band_runs(gpu: &mut GpuState, model: &SidebarDrawModel, band_size: PixelSize) {
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let base_bg = theme.default_bg;
    let (Some(renderer), Some((_, _, band_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
    ) else {
        return;
    };
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        band_view,
        band_size,
        model.grid,
        base_bg,
        0.0,
        &model.runs,
    );
}

/// Pass 1b — hairline divider over the band's rightmost pixel(s): a solid
/// `chrome().divider` strip that gives the seam a crisp edge against the pane
/// background (the terminal keeps its own theme, so the two surfaces otherwise
/// meet as unrelated colors).
fn draw_sidebar_divider(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) {
    let hairline_w = (SEAM_HAIRLINE_WIDTH * model.scale).round().max(1.0) as u32;
    if SEAM_HAIRLINE_WIDTH <= 0.0 || model.inset <= hairline_w {
        return;
    }
    let divider_size = PixelSize {
        w: hairline_w,
        h: model.height,
    };
    if ensure_scratch(
        &mut gpu.chrome_textures.sidebar_divider_tex,
        &gpu.device,
        divider_size,
        surface_format,
        "noa-sidebar-divider",
    ) {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, divider_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_divider_tex.as_ref(),
    ) else {
        return;
    };
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        divider_view,
        divider_size,
        GridSize { cols: 1, rows: 1 },
        chrome().divider,
        1.0,
        &[],
    );
    let divider_style = CardStyle {
        background: rgb_to_rgba(chrome().divider),
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
        band_view,
        band_size,
        &divider_style,
        &[CardTexturePlacement {
            texture_view: divider_view,
            x: model.inset - hairline_w,
            y: 0,
            w: hairline_w,
            h: model.height,
            selected: false,
        }],
    );
}

/// Pass 1c — toolbar `+` button: a borderless geometric `+` glyph — two centered
/// bars, drawn as solid rounded rects (pixel-placed rather than a font glyph,
/// which the coarse sidebar cell grid can't center in a tile this small). No
/// persistent frame; hover just lays a subtle borderless fill behind the `+`
/// and brightens the bars.
fn draw_sidebar_new_button(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) {
    if model.new_button.w == 0 || model.new_button.h == 0 {
        return;
    }
    let btn = model.new_button;
    let btn_size = PixelSize {
        w: btn.w.max(1),
        h: btn.h.max(1),
    };
    if ensure_scratch(
        &mut gpu.chrome_textures.sidebar_button_tex,
        &gpu.device,
        btn_size,
        surface_format,
        "noa-sidebar-button",
    ) {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    let glyph = if model.new_button_hover {
        chrome().fg
    } else {
        chrome().dim_fg
    };
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, button_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_button_tex.as_ref(),
    ) else {
        return;
    };
    let pipeline = &card.pipeline;

    // Hover only: a borderless rounded fill behind the `+` (no frame at rest).
    // Rendered into the reused button scratch, composited over the band.
    if model.new_button_hover {
        rasterize_runs(
            renderer,
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            theme,
            button_view,
            btn_size,
            GridSize { cols: 1, rows: 1 },
            chrome().card,
            model.background_opacity,
            &[],
        );
        let button_style = CardStyle {
            background: rgb_to_rgba(chrome().card),
            border_color: [0.0; 4],
            focus_color: [0.0; 4],
            corner_radius: TOOLBAR_BUTTON_RADIUS * model.scale,
            border_width: 0.0,
            focus_width: 0.0,
            focus_glow_width: 0.0,
        };
        pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            band_view,
            band_size,
            &button_style,
            &[CardTexturePlacement {
                texture_view: button_view,
                x: btn.x,
                y: btn.y,
                w: btn.w,
                h: btn.h,
                selected: false,
            }],
        );
    }

    // The `+` glyph: refill the same scratch with the glyph color (the hover
    // fill composite above, if any, already submitted, so the reuse is safe)
    // and composite two thin rounded bars centered on the tile.
    let arm = (TOOLBAR_PLUS_ARM * model.scale).round().max(1.0) as u32;
    let thick = (TOOLBAR_PLUS_THICKNESS * model.scale).round().max(1.0) as u32;
    let cx = btn.x + btn.w / 2;
    let cy = btn.y + btn.h / 2;
    let hbar = SidebarRect::new(
        cx.saturating_sub(arm / 2),
        cy.saturating_sub(thick / 2),
        arm,
        thick,
    );
    let vbar = SidebarRect::new(
        cx.saturating_sub(thick / 2),
        cy.saturating_sub(arm / 2),
        thick,
        arm,
    );
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        button_view,
        btn_size,
        GridSize { cols: 1, rows: 1 },
        glyph,
        model.background_opacity,
        &[],
    );
    let bar_style = CardStyle {
        background: rgb_to_rgba(glyph),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: (thick as f32 / 2.0).min(2.0 * model.scale),
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        band_view,
        band_size,
        &bar_style,
        &[
            CardTexturePlacement {
                texture_view: button_view,
                x: hbar.x,
                y: hbar.y,
                w: hbar.w,
                h: hbar.h,
                selected: false,
            },
            CardTexturePlacement {
                texture_view: button_view,
                x: vbar.x,
                y: vbar.y,
                w: vbar.w,
                h: vbar.h,
                selected: false,
            },
        ],
    );
}

/// Pass 2 — each fully-visible card as text-only content over the seamless
/// band, plus the per-card status accent bar (busy / attention / bell) along
/// its left edge. The card texture clear is transparent, so rows do not create
/// a separate rectangular surface; status bars and rules carry the boundaries.
fn draw_sidebar_cards(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) {
    if model.cards.is_empty() {
        return;
    }
    let panel_bg = active_theme(&gpu.theme, &gpu.preview_theme).default_bg;
    let card_style = CardStyle {
        background: rgb_to_rgba(sidebar_card_bg(panel_bg)),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };

    // Every fully-visible card has the same height (`card_h`), so its accent bar
    // has a constant size — ensure that scratch once here (only when a card
    // actually carries an accent) instead of per card inside the loop, keeping
    // the loop free of allocation bookkeeping.
    let bar_w = (2.0 * model.scale).round().max(1.0) as u32;
    let any_accent = model.cards.iter().any(|card| card.accent.is_some());
    if any_accent
        && model.card_h > 0
        && ensure_scratch(
            &mut gpu.chrome_textures.sidebar_accent_tex,
            &gpu.device,
            PixelSize {
                w: bar_w,
                h: model.card_h,
            },
            surface_format,
            "noa-sidebar-accent",
        )
    {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }

    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card_pipe), Some((_, _, card_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_card_tex.as_ref(),
    ) else {
        return;
    };
    let pipeline = &card_pipe.pipeline;
    let accent_view = gpu
        .chrome_textures
        .sidebar_accent_tex
        .as_ref()
        .map(|t| &t.2);
    let card_size = PixelSize {
        w: model.card_w,
        h: model.card_h,
    };

    for card_draw in &model.cards {
        rasterize_runs(
            renderer,
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            theme,
            card_view,
            card_size,
            card_draw.grid,
            card_draw.bg,
            SIDEBAR_CARD_STATIC_FILL_OPACITY,
            &card_draw.runs,
        );
        // Attention keeps the same flat row treatment as Selected; the red
        // dot/label/accent bar carry the urgency without an outline seam.
        let selected = matches!(
            sidebar_card_frame(card_draw.selected, card_draw.attention),
            SidebarCardFrame::Selected | SidebarCardFrame::Attention
        );
        pipeline.overlay_texture_cards_clipped(
            &gpu.device,
            &gpu.queue,
            band_view,
            band_size,
            &card_style,
            &[CardTexturePlacement {
                texture_view: card_view,
                x: card_draw.rect.x,
                y: card_draw.rect.y,
                w: card_draw.rect.w,
                h: card_draw.rect.h,
                selected,
            }],
            card_draw.src_uv,
            1.0,
        );

        // Status accent bar (busy / attention / bell) along the card's left
        // edge: a thin full-height strip on the flat row. The reused accent
        // scratch (ensured above) is refilled per card because the color varies.
        if let Some(accent) = card_draw.accent {
            let bar_h = card_draw.rect.h;
            if bar_h > 0
                && let Some(accent_view) = accent_view
            {
                rasterize_runs(
                    renderer,
                    &gpu.device,
                    &gpu.queue,
                    &mut gpu.sidebar_font,
                    theme,
                    accent_view,
                    PixelSize { w: bar_w, h: bar_h },
                    GridSize { cols: 1, rows: 1 },
                    accent,
                    1.0,
                    &[],
                );
                let accent_style = CardStyle {
                    background: rgb_to_rgba(accent),
                    border_color: [0.0; 4],
                    focus_color: [0.0; 4],
                    corner_radius: 0.0,
                    border_width: 0.0,
                    focus_width: 0.0,
                    focus_glow_width: 0.0,
                };
                pipeline.overlay_texture_cards_clipped(
                    &gpu.device,
                    &gpu.queue,
                    band_view,
                    band_size,
                    &accent_style,
                    &[CardTexturePlacement {
                        texture_view: accent_view,
                        x: card_draw.rect.x,
                        y: card_draw.rect.y,
                        w: bar_w,
                        h: bar_h,
                        selected: false,
                    }],
                    card_draw.src_uv,
                    1.0,
                );
            }
        }
    }
}

/// Pass 2a — thin horizontal rules only at boundaries between adjacent flat rows.
/// This keeps the one-piece surface while making the cell breaks legible.
fn draw_sidebar_rules(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) {
    if model.cards.len() <= 1 {
        return;
    }
    let rule_h = (SIDEBAR_CARD_RULE_HEIGHT * model.scale).round().max(1.0) as u32;
    if ensure_scratch(
        &mut gpu.chrome_textures.sidebar_rule_tex,
        &gpu.device,
        PixelSize {
            w: model.card_w.max(1),
            h: rule_h,
        },
        surface_format,
        "noa-sidebar-card-rule",
    ) {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    let panel_bg = active_theme(&gpu.theme, &gpu.preview_theme).default_bg;
    let rule_color = mix_rgb(
        sidebar_card_bg(panel_bg),
        chrome().border,
        SIDEBAR_CARD_RULE_BORDER_MIX,
    );
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, rule_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_rule_tex.as_ref(),
    ) else {
        return;
    };
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        rule_view,
        PixelSize {
            w: model.card_w.max(1),
            h: rule_h,
        },
        GridSize { cols: 1, rows: 1 },
        rule_color,
        1.0,
        &[],
    );
    let rule_style = CardStyle {
        background: rgb_to_rgba(rule_color),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    let rule_placements: Vec<_> = model
        .cards
        .windows(2)
        .filter_map(|pair| {
            let upper = &pair[0];
            let lower = &pair[1];
            if upper.rect.bottom() == lower.rect.y && upper.rect.w > 0 && upper.rect.h >= rule_h {
                Some(CardTexturePlacement {
                    texture_view: rule_view,
                    x: upper.rect.x,
                    y: lower.rect.y.saturating_sub(rule_h),
                    w: upper.rect.w,
                    h: rule_h,
                    selected: false,
                })
            } else {
                None
            }
        })
        .collect();
    if !rule_placements.is_empty() {
        card.pipeline.overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            band_view,
            band_size,
            &rule_style,
            &rule_placements,
        );
    }
}

/// Pass 2b — drag-reorder feedback: the accent drop-indicator line at the insertion
/// gap, then the floating dragged card composited above every static card.
fn draw_sidebar_drag(
    gpu: &mut GpuState,
    model: &SidebarDrawModel,
    band_size: PixelSize,
    surface_format: wgpu::TextureFormat,
) {
    if let Some(line) = &model.drop_indicator {
        if ensure_scratch(
            &mut gpu.chrome_textures.sidebar_drop_tex,
            &gpu.device,
            PixelSize {
                w: line.w.max(1),
                h: line.h.max(1),
            },
            surface_format,
            "noa-sidebar-drop",
        ) {
            #[cfg(debug_assertions)]
            gpu.chrome_textures.record_rebuild();
        }
        let theme = active_theme(&gpu.theme, &gpu.preview_theme);
        if let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, drop_view))) = (
            gpu.chrome_textures.sidebar_renderer.as_mut(),
            gpu.chrome_textures.sidebar_band.as_ref(),
            gpu.chrome_textures.sidebar_card.as_ref(),
            gpu.chrome_textures.sidebar_drop_tex.as_ref(),
        ) {
            rasterize_runs(
                renderer,
                &gpu.device,
                &gpu.queue,
                &mut gpu.sidebar_font,
                theme,
                drop_view,
                PixelSize {
                    w: line.w.max(1),
                    h: line.h.max(1),
                },
                GridSize { cols: 1, rows: 1 },
                chrome().accent,
                1.0,
                &[],
            );
            let drop_style = CardStyle {
                background: rgb_to_rgba(chrome().accent),
                border_color: [0.0; 4],
                focus_color: [0.0; 4],
                corner_radius: (line.h as f32 / 2.0).min(3.0 * model.scale),
                border_width: 0.0,
                focus_width: 0.0,
                focus_glow_width: 0.0,
            };
            card.pipeline.overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                band_view,
                band_size,
                &drop_style,
                &[CardTexturePlacement {
                    texture_view: drop_view,
                    x: line.x,
                    y: line.y,
                    w: line.w,
                    h: line.h,
                    selected: false,
                }],
            );
        }
    }

    let Some(drag) = &model.dragging else {
        return;
    };
    let panel_bg = active_theme(&gpu.theme, &gpu.preview_theme).default_bg;
    let card_style = CardStyle {
        background: rgb_to_rgba(sidebar_card_bg(panel_bg)),
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, card_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_card_tex.as_ref(),
    ) else {
        return;
    };
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        card_view,
        PixelSize {
            w: model.card_w,
            h: model.card_h,
        },
        drag.grid,
        drag.bg,
        model.background_opacity,
        &drag.runs,
    );
    card.pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        band_view,
        band_size,
        &card_style,
        &[CardTexturePlacement {
            texture_view: card_view,
            x: drag.rect.x,
            y: drag.rect.y,
            w: drag.rect.w,
            h: drag.rect.h,
            selected: true,
        }],
    );
}

/// Pass 3 — the `…` menu popup, composited above the cards.
fn draw_sidebar_menu(gpu: &mut GpuState, model: &SidebarDrawModel, band_size: PixelSize) {
    let Some(menu) = &model.menu else {
        return;
    };
    let theme = active_theme(&gpu.theme, &gpu.preview_theme);
    let (Some(renderer), Some((_, _, band_view)), Some(card), Some((_, _, menu_view))) = (
        gpu.chrome_textures.sidebar_renderer.as_mut(),
        gpu.chrome_textures.sidebar_band.as_ref(),
        gpu.chrome_textures.sidebar_card.as_ref(),
        gpu.chrome_textures.sidebar_menu_tex.as_ref(),
    ) else {
        return;
    };
    rasterize_runs(
        renderer,
        &gpu.device,
        &gpu.queue,
        &mut gpu.sidebar_font,
        theme,
        menu_view,
        PixelSize {
            w: menu.rect.w,
            h: menu.rect.h,
        },
        menu.grid,
        chrome().pill,
        1.0,
        &menu.runs,
    );
    let menu_style = CardStyle {
        background: rgb_to_rgba(chrome().pill),
        border_color: rgb_to_rgba(chrome().border),
        focus_color: [0.0; 4],
        corner_radius: crate::chrome::RADIUS_SM * model.scale,
        border_width: 1.0 * model.scale,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    card.pipeline.overlay_texture_cards(
        &gpu.device,
        &gpu.queue,
        band_view,
        band_size,
        &menu_style,
        &[CardTexturePlacement {
            texture_view: menu_view,
            x: menu.rect.x,
            y: menu.rect.y,
            w: menu.rect.w,
            h: menu.rect.h,
            selected: false,
        }],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;

    fn cache_model() -> SidebarDrawModel {
        SidebarDrawModel {
            inset: 240,
            height: 600,
            scale: 2.0,
            card_h: 120,
            card_w: 216,
            grid: GridSize { cols: 24, rows: 40 },
            runs: vec![SidebarTextRun::new(
                1,
                1,
                "sidebar".to_string(),
                Rgb::new(1, 2, 3),
            )],
            new_button: SidebarRect::new(200, 4, 28, 22),
            new_button_hover: false,
            cards: vec![SidebarCardDraw {
                rect: SidebarRect::new(8, 40, 216, 120),
                grid: GridSize { cols: 24, rows: 8 },
                bg: Rgb::new(10, 11, 12),
                selected: true,
                attention: false,
                accent: None,
                runs: vec![SidebarTextRun::new(
                    1,
                    1,
                    "card".to_string(),
                    Rgb::new(4, 5, 6),
                )],
                src_uv: [0.0, 0.0, 1.0, 1.0],
            }],
            menu: None,
            dragging: None,
            drop_indicator: None,
            background_opacity: 0.85,
        }
    }

    fn cache_key(model: SidebarDrawModel) -> SidebarRasterCacheKey {
        SidebarRasterCacheKey {
            surface_format: wgpu::TextureFormat::Bgra8Unorm,
            padding: GridPadding::ZERO,
            theme: Theme::new(),
            chrome: crate::chrome::CHROME_DARK,
            font: SidebarFontCacheKey {
                atlas_identity: 1,
                mask_atlas_generation: 2,
                color_atlas_generation: 3,
                atlas_eviction_generation: 4,
            },
            model,
        }
    }

    #[test]
    fn attention_frame_does_not_override_selected_sidebar_card() {
        assert_eq!(sidebar_card_frame(false, false), SidebarCardFrame::Resting);
        assert_eq!(sidebar_card_frame(false, true), SidebarCardFrame::Attention);
        assert_eq!(sidebar_card_frame(true, false), SidebarCardFrame::Selected);
        assert_eq!(sidebar_card_frame(true, true), SidebarCardFrame::Selected);
    }

    #[test]
    fn identical_sidebar_raster_key_hits() {
        let key = cache_key(cache_model());
        let next = key.clone();

        assert!(sidebar_cache_hit(Some(&key), &next));
        assert!(!sidebar_cache_hit(None, &next));
    }

    #[test]
    fn sidebar_raster_key_misses_when_model_changes() {
        let model = cache_model();
        let previous = cache_key(model.clone());
        let mut changed_hover = model.clone();
        changed_hover.new_button_hover = true;
        let mut changed_text = model;
        changed_text.runs[0].text.push_str(" changed");

        assert!(!sidebar_cache_hit(
            Some(&previous),
            &cache_key(changed_hover)
        ));
        assert!(!sidebar_cache_hit(
            Some(&previous),
            &cache_key(changed_text)
        ));
    }

    #[test]
    fn sidebar_raster_key_misses_when_external_inputs_change() {
        let previous = cache_key(cache_model());
        let mut changed_theme = previous.clone();
        changed_theme.theme.default_bg = Rgb::new(9, 8, 7);
        let mut changed_font = previous.clone();
        changed_font.font.mask_atlas_generation += 1;

        assert!(!sidebar_cache_hit(Some(&previous), &changed_theme));
        assert!(!sidebar_cache_hit(Some(&previous), &changed_font));
    }
}
