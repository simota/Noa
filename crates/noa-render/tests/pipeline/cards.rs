use super::shared::*;
use noa_core::{DEFAULT_GRID_PADDING, GridPadding, PixelSize};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, TerminalColors};
use noa_render::{
    CardPipeline, CardStyle, CardTexturePlacement, CardTilePlacement, CommandPaletteSnapshot,
    FrameSnapshot, OverviewThumbnailResources, Renderer, Theme,
};

/// AC-OV-12/14 (headless): the rounded-card composite pipeline builds its
/// bind-group layout (texture + sampler in the fragment stage, uniform in the
/// **vertex+fragment** stage) and draws two cards — one selected — onto a
/// surface-format target with no wgpu validation error. This guards the CardUniforms
/// `#[repr(C)]` <-> WGSL std140 layout and the VERTEX_FRAGMENT visibility of the
/// card uniform (CLAUDE.md GPU gotcha).
#[test]
fn overview_card_pipeline_composites_tiles_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview card composite test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 160, h: 120 };
    let tile_size = PixelSize { w: 80, h: 60 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "card");

    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &queue,
        &renderer,
        scratch_size,
        tile_size,
        2,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );
    // Populate both tiles (mirror in the content region, card color in the band).
    for tile_index in 0..2 {
        overview
            .render_existing_renderer_to_tile(
                &device,
                &queue,
                &mut renderer,
                scratch_size,
                tile_index,
            )
            .expect("render tile for card composite");
    }

    let surface_size = PixelSize { w: 220, h: 160 };
    let (target_tex, target_view) = render_target(&device, surface_size.w, surface_size.h);
    let style = CardStyle {
        background: [0.034, 0.046, 0.084, 1.0],
        border_color: [0.298, 0.32, 0.380, 1.0],
        focus_color: [0.078, 0.635, 1.0, 1.0],
        corner_radius: 8.0,
        border_width: 1.0,
        focus_width: 2.0,
        focus_glow_width: 8.0,
    };
    let placements = [
        CardTilePlacement {
            tile_index: 0,
            x: 20,
            y: 20,
            w: tile_size.w,
            h: tile_size.h,
            selected: true,
        },
        CardTilePlacement {
            tile_index: 1,
            x: 120,
            y: 20,
            w: tile_size.w,
            h: tile_size.h,
            selected: false,
        },
    ];

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview.composite_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &style,
        &placements,
    );
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after card composite");
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during overview card composite: {err:?}"
    );

    // The composite must actually paint: the backdrop clear + at least one
    // card make the target non-uniform, so a coarse content check is that not
    // every pixel is identical.
    let pixels = read_rgba_pixels(&device, &queue, &target_tex, surface_size.w, surface_size.h);
    let first = &pixels[0..4];
    assert!(
        pixels.chunks_exact(4).any(|px| px != first),
        "card composite produced a uniform frame (nothing drawn)"
    );
}

/// Bolt perf item 3: `draw_texture_cards` pools its per-card uniform buffer +
/// bind group by sampled `TextureView` identity instead of allocating fresh
/// ones every call. Composite the *same* view three times (as a
/// hover-only Overview redraw would re-draw the same tiles every frame) and
/// assert the pool never grows past one entry, then draw a second, distinct
/// view and assert it grows to exactly two — proving both reuse and that a
/// genuinely new card still gets pooled.
#[test]
fn overview_card_pipeline_pools_repeated_texture_views() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview card pool test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let (tile_a_tex, tile_a_view) = render_target(&device, 40, 30);
    let (tile_b_tex, tile_b_view) = render_target(&device, 40, 30);
    let _ = (&tile_a_tex, &tile_b_tex);

    let card = CardPipeline::new(&device, format, wgpu::BlendState::ALPHA_BLENDING);
    let style = CardStyle {
        background: [0.0; 4],
        border_color: [0.3, 0.32, 0.38, 1.0],
        focus_color: [0.078, 0.635, 1.0, 1.0],
        corner_radius: 4.0,
        border_width: 1.0,
        focus_width: 2.0,
        focus_glow_width: 4.0,
    };
    let surface_size = PixelSize { w: 120, h: 90 };
    let (target_tex, target_view) = render_target(&device, surface_size.w, surface_size.h);
    let _ = &target_tex;
    fn placement(view: &wgpu::TextureView) -> CardTexturePlacement<'_> {
        CardTexturePlacement {
            texture_view: view,
            x: 10,
            y: 10,
            w: 40,
            h: 30,
            selected: false,
        }
    }

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    for _ in 0..3 {
        card.overlay_texture_cards(
            &device,
            &queue,
            &target_view,
            surface_size,
            &style,
            &[placement(&tile_a_view)],
        );
    }
    assert_eq!(
        card.card_pool_len_for_test(),
        1,
        "redrawing the same texture view repeatedly must reuse one pooled card, not allocate one per draw"
    );

    card.overlay_texture_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &style,
        &[placement(&tile_b_view)],
    );
    assert_eq!(
        card.card_pool_len_for_test(),
        2,
        "a genuinely new texture view must still get its own pooled card"
    );

    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after pooled card composite");
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error while exercising the card pool: {err:?}"
    );
}

/// The session sidebar composites its rasterized band texture onto a window
/// surface with `CardPipeline::overlay_texture_cards` (flat: corner_radius 0,
/// border_width 0) — the same shader/uniform path as overview cards but the
/// non-clearing `Load` overlay. Draw that path headlessly and assert no wgpu
/// validation error and a non-uniform result (something was actually drawn).
#[test]
fn sidebar_band_overlay_composites_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping sidebar band overlay test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;

    // Rasterize a "band" texture (RENDER_ATTACHMENT + TEXTURE_BINDING) the way
    // the sidebar does: a reused Renderer drawing text into it.
    let band_size = PixelSize { w: 120, h: 300 };
    let band = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-test-sidebar-band"),
        size: wgpu::Extent3d {
            width: band_size.w,
            height: band_size.h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let band_view = band.create_view(&wgpu::TextureViewDescriptor::default());
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build sidebar band renderer");
    renderer.resize(band_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "session");
    renderer.set_clear_color([0.08, 0.086, 0.106, 1.0]);
    renderer.draw(&device, &queue, &band_view);

    // Composite it flat onto a wider surface at the left inset.
    let surface_size = PixelSize { w: 400, h: 300 };
    let (target_tex, target_view) = render_target(&device, surface_size.w, surface_size.h);
    let card = CardPipeline::new(&device, format, wgpu::BlendState::ALPHA_BLENDING);
    let style = CardStyle {
        background: [0.08, 0.086, 0.106, 1.0],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    card.overlay_texture_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &style,
        &[CardTexturePlacement {
            texture_view: &band_view,
            x: 0,
            y: 0,
            w: band_size.w,
            h: band_size.h,
            selected: false,
        }],
    );
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after sidebar band overlay");
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during sidebar band overlay: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target_tex, surface_size.w, surface_size.h);
    let first = &pixels[0..4];
    assert!(
        pixels.chunks_exact(4).any(|px| px != first),
        "sidebar band overlay produced a uniform frame (nothing drawn)"
    );
}

/// Pane-dnd Track D: the drag-visual composites live entirely in `noa-app`-
/// side compositing over these existing `noa-render` primitives (no new GPU
/// pipeline or uniform struct), so this headless test exercises both call
/// shapes directly instead of duplicating app types here:
///
/// 1. The zone highlight — a flat rect sampling a 1x1 tint texture via
///    `overlay_texture_cards` (the same technique `sidebar_band_overlay_...`
///    above uses for the sidebar band, and production's scrollbar-thumb/
///    visual-bell-flash composites already exercise headlessly via other
///    call sites — this is the same shape at an arbitrary target rect).
/// 2. The floating snapshot — a small rasterized-text scratch texture
///    stretched to a larger placement via `overlay_texture_cards_with_opacity`
///    at pane-dnd's 70% opacity. This exact API is already used in
///    production (command-palette fade-in) but had no headless coverage
///    before this test — pane-dnd is its second caller.
#[test]
fn pane_drag_visuals_composite_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping pane-drag visuals composite test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let card = CardPipeline::new(&device, format, wgpu::BlendState::ALPHA_BLENDING);
    let surface_size = PixelSize { w: 400, h: 300 };
    let (target_tex, target_view) = render_target(&device, surface_size.w, surface_size.h);

    // Zone highlight: a 1x1 UI_ACCENT-tinted texture (alpha carries the
    // ~28% overlay opacity, matching `ensure_tint_texture`'s convention).
    let zone_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-test-pane-drag-zone"),
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
            texture: &zone_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0x14, 0xa2, 0xff, 71],
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
    let zone_view = zone_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let flat_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };

    // Floating snapshot: a small text scratch, stretched to a larger card.
    let scratch_size = PixelSize { w: 48, h: 20 };
    let (scratch_tex, scratch_view) = render_target(&device, scratch_size.w, scratch_size.h);
    let _ = &scratch_tex;
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build pane-drag snapshot renderer");
    renderer.resize(scratch_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "12x34");
    renderer.draw(&device, &queue, &scratch_view);
    let snapshot_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 6.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    card.overlay_texture_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &flat_style,
        &[CardTexturePlacement {
            texture_view: &zone_view,
            x: 20,
            y: 20,
            w: 80,
            h: 120,
            selected: false,
        }],
    );
    card.overlay_texture_cards_with_opacity(
        &device,
        &queue,
        &target_view,
        surface_size,
        &snapshot_style,
        &[CardTexturePlacement {
            texture_view: &scratch_view,
            x: 150,
            y: 100,
            w: 160,
            h: 90,
            selected: false,
        }],
        0.7,
    );
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after pane-drag visuals composite");
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during pane-drag visuals composite: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target_tex, surface_size.w, surface_size.h);
    let first = &pixels[0..4];
    assert!(
        pixels.chunks_exact(4).any(|px| px != first),
        "pane-drag visuals composite produced a uniform frame (nothing drawn)"
    );
}

/// Item H: exercise the command-palette rounded-card composite exactly as
/// `noa-app` does it — rasterize the block into a scratch texture with a
/// zero-padding `Renderer`, then composite two `overlay_texture_cards` passes
/// (soft shadow, then themed border) over a surface. Assert no wgpu validation
/// error and that something was drawn.
#[test]
fn command_palette_card_composites_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping command-palette card test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;

    let palette = CommandPaletteSnapshot {
        query: "sp".to_string(),
        rows: vec![
            noa_render::PaletteRow::Header {
                label: "Splits".to_string(),
            },
            noa_render::PaletteRow::Entry {
                title: "Split Right".to_string(),
                hint: Some("\u{2318}D".to_string()),
                match_positions: vec![0, 1],
                enabled: true,
            },
            noa_render::PaletteRow::Entry {
                title: "Split Down".to_string(),
                hint: None,
                match_positions: vec![0, 1],
                enabled: true,
            },
        ],
        selected: 1,
        total_entries: 2,
    };

    // Size the scratch to the block, exactly as the app does.
    let (pane_cols, pane_rows) = (40u16, 20u16);
    let layout = noa_render::command_palette_layout(&palette, pane_cols, pane_rows)
        .expect("palette layout for a roomy grid");
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let (cell_w, cell_h) = {
        let m = font.metrics();
        (m.cell_w, m.cell_h)
    };
    let block_px = PixelSize {
        w: ((layout.block_cols as f32) * cell_w).ceil().max(1.0) as u32,
        h: ((layout.block_rows as f32) * cell_h).ceil().max(1.0) as u32,
    };
    let (scratch_tex, scratch_view) = render_target(&device, block_px.w, block_px.h);
    let _ = &scratch_tex;

    // Zero-padding renderer draws the block-sized mini snapshot into the scratch.
    let mut renderer = Renderer::new(
        &device,
        &queue,
        format,
        &mut font,
        GridPadding::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("build palette block renderer");
    renderer.resize(block_px);
    let rows: Vec<Row> = (0..layout.block_rows)
        .map(|_| {
            Row::from_cells(
                vec![Cell::default(); layout.block_cols as usize],
                false,
                true,
            )
        })
        .collect();
    let snap = FrameSnapshot {
        scroll_shift: 0,
        row_dirty: vec![true; rows.len()],
        rows,
        cursor: Cursor::default(),
        copy_cursor: None,
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols: layout.block_cols,
        rows_n: layout.block_rows,
        focused: true,
        cursor_blink_visible: false,
        hover_link: None,
        search_prompt: None,
        command_palette: Some(palette.clone()),
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    };
    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    renderer.draw(&device, &queue, &scratch_view);

    // Composite the two card passes over a surface.
    let surface_size = PixelSize { w: 600, h: 400 };
    let (target_tex, target_view) = render_target(&device, surface_size.w, surface_size.h);
    let card = CardPipeline::new(&device, format, wgpu::BlendState::ALPHA_BLENDING);
    let placement = |selected| CardTexturePlacement {
        texture_view: &scratch_view,
        x: 40,
        y: 40,
        w: block_px.w,
        h: block_px.h,
        selected,
    };
    let shadow_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, 1.0],
        corner_radius: 10.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 12.0,
    };
    let border_style = CardStyle {
        background: [0.0; 4],
        border_color: [0.3, 0.32, 0.38, 1.0],
        focus_color: [0.3, 0.32, 0.38, 1.0],
        corner_radius: 10.0,
        border_width: 1.0,
        focus_width: 1.0,
        focus_glow_width: 0.0,
    };

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    card.overlay_texture_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &shadow_style,
        &[placement(true)],
    );
    card.overlay_texture_cards(
        &device,
        &queue,
        &target_view,
        surface_size,
        &border_style,
        &[placement(false)],
    );
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after palette card composite");
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during command-palette card composite: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target_tex, surface_size.w, surface_size.h);
    let first = &pixels[0..4];
    assert!(
        pixels.chunks_exact(4).any(|px| px != first),
        "command-palette card produced a uniform frame (nothing drawn)"
    );
}
