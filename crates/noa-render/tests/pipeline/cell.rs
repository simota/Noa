use super::shared::*;
use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, Selection, SelectionPoint, TerminalColors};
use noa_render::{CommandPaletteSnapshot, FrameSnapshot, Renderer, Theme};

#[test]
fn cell_pipeline_builds_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping GPU pipeline-build test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    );
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        renderer.is_ok(),
        "Renderer::new failed: {:?}",
        renderer.err()
    );
    assert!(
        err.is_none(),
        "wgpu validation error while building the cell pipeline: {err:?}"
    );
}

/// Two `Renderer`s built from one [`noa_render::SharedPipelines`] set (the
/// per-tab production path via `PipelineCache`) must both draw a frame with
/// no validation error — guards the pipeline-sharing refactor against a
/// shared-vs-per-renderer resource mismatch (e.g. a bind group built against
/// a layout the shared pipeline doesn't own).
#[test]
fn two_renderers_sharing_pipelines_draw_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping GPU shared-pipeline test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut cache = noa_render::PipelineCache::default();
    let pipelines = cache.get(&device, format);
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    for text in ["first tab", "second tab"] {
        let mut renderer =
            Renderer::with_pipelines(&device, &queue, &pipelines, &mut font, DEFAULT_GRID_PADDING)
                .expect("build renderer from shared pipelines");
        renderer.resize(PixelSize { w: 64, h: 32 });
        rebuild_text_frame(&mut renderer, &mut font, &device, &queue, text);
        let (_target, view) = render_target(&device, 64, 32);
        renderer.draw(&device, &queue, &view);
    }
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error drawing via shared pipelines: {err:?}"
    );
}

#[test]
fn cell_pipeline_draws_one_frame_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping GPU draw test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 32 });

    // A tiny snapshot exercising a glyph quad, a background quad, and the cursor.
    let row = Row {
        cells: vec![
            Cell {
                ch: 'A',
                combining: String::new(),
                fg: Color::Palette(1),
                bg: Color::Default,
                underline_color: None,
                hyperlink: None,
                attrs: CellAttrs::empty(),
            },
            Cell {
                ch: 'g',
                combining: String::new(),
                fg: Color::Default,
                bg: Color::Palette(4),
                underline_color: None,
                hyperlink: None,
                attrs: CellAttrs::empty(),
            },
            Cell::default(),
            Cell::default(),
        ],
        wrapped: false,
        dirty: true,
    };
    let snap = FrameSnapshot {
        scroll_shift: 0,
        rows: vec![row],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: Some(Selection::new(
            SelectionPoint::new(1, 0),
            SelectionPoint::new(1, 0),
        )),
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols: 4,
        rows_n: 1,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    };
    let theme = Theme::new();

    renderer.rebuild_cells(&snap, &mut font, &theme);
    renderer.sync_atlas(&device, &queue, &mut font);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-test-target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during draw (uniform/instance buffer layout?): {err:?}"
    );
}

#[test]
fn command_palette_overlay_draws_one_frame_without_validation_error() {
    // AC-19 (headless): a FrameSnapshot carrying a command-palette payload
    // (query row + multiple entry rows, one selected) draws on a real
    // adapter with no wgpu validation error — the multi-row overlay reuses
    // the existing cell pipeline, adding no new bind-group/std140 surface.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping command-palette GPU draw test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 320, h: 160 });

    let cols = 30u16;
    let rows_n = 8u16;
    let rows: Vec<Row> = (0..rows_n)
        .map(|_| Row {
            cells: vec![Cell::default(); cols as usize],
            wrapped: false,
            dirty: true,
        })
        .collect();
    let snap = FrameSnapshot {
        scroll_shift: 0,
        row_dirty: vec![true; rows.len()],
        rows,
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols,
        rows_n,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: Some(CommandPaletteSnapshot {
            query: "sp".to_string(),
            rows: vec![
                noa_render::PaletteRow::Entry {
                    title: "Split Right".to_string(),
                    hint: Some("\u{2318}D".to_string()),
                    match_positions: vec![0, 1],
                },
                noa_render::PaletteRow::Entry {
                    title: "Split Down".to_string(),
                    hint: Some("\u{21e7}\u{2318}D".to_string()),
                    match_positions: vec![0, 1],
                },
                noa_render::PaletteRow::Entry {
                    title: "Toggle Split Zoom".to_string(),
                    hint: None,
                    match_positions: vec![7, 8],
                },
            ],
            selected: 1,
            total_entries: 3,
        }),
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    };

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 320, 160);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing the command-palette overlay: {err:?}"
    );
}

/// WP4 (REQ-NF-4, AC-WP4-03): draw one frame via a full rebuild (the first
/// frame through a fresh `PaneRenderCache`) and a second frame via the
/// per-row dirty-patch path (only one of two rows marked dirty), asserting
/// neither draw trips a wgpu validation error — mirrors the class of bug
/// this file exists to catch (uniform/instance buffer layout mismatches),
/// now specifically for the row-indexed segment cache introduced by WP4.
#[test]
fn cell_pipeline_draws_full_then_dirty_patched_frame_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping WP4 full-then-patched draw test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    fn two_row_snapshot(first: char, second: char, row_dirty: [bool; 2]) -> FrameSnapshot {
        let make_row = |ch: char, dirty: bool| Row {
            cells: vec![Cell {
                ch,
                ..Cell::default()
            }],
            wrapped: false,
            dirty,
        };
        FrameSnapshot {
            scroll_shift: 0,
            rows: vec![
                make_row(first, row_dirty[0]),
                make_row(second, row_dirty[1]),
            ],
            row_dirty: row_dirty.to_vec(),
            cursor: Cursor::default(),
            colors: TerminalColors::default(),
            selection: None,
            search: SearchState::default(),
            row_base: 0,
            abs_row_base: 0,
            active_is_alt: false,
            cols: 1,
            rows_n: 2,
            focused: true,
            cursor_blink_visible: true,
            hover_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            preedit: None,
            image_placements: Vec::new(),
            images: Vec::new(),
        }
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-wp4-test-target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let theme = Theme::new();

    // First frame: fresh cache -> every row is a full rebuild.
    let snap1 = two_row_snapshot('A', 'B', [true, true]);
    renderer.rebuild_cells(&snap1, &mut font, &theme);
    renderer.sync_atlas(&device, &queue, &mut font);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during the full-rebuild draw: {err:?}"
    );

    // Second frame: only row 1 is dirty -> exercises the per-row dirty-patch
    // path (row 0's cached bg/glyph/decoration segments are reused as-is).
    let snap2 = two_row_snapshot('A', 'X', [false, true]);
    renderer.rebuild_cells(&snap2, &mut font, &theme);
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        1,
        "the second frame should rebuild exactly the one dirtied row"
    );
    renderer.sync_atlas(&device, &queue, &mut font);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during the dirty-row-patch draw: {err:?}"
    );
}

#[test]
fn cell_pipeline_draws_color_glyph_without_validation_error_and_samples_passthrough() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping color-glyph GPU draw test");
        return;
    };
    let mut font =
        FontGrid::new(32.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    // 😀 U+1F600 GRINNING FACE. Probe directly first so we can skip cleanly
    // if this environment has no color-capable emoji face resolved.
    let probe = font.get_or_raster('\u{1F600}');
    if !probe.color || probe.atlas_size == [0, 0] {
        eprintln!(
            "no color-capable emoji face resolved in this environment — skipping color-glyph GPU draw test"
        );
        return;
    }

    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    // fg deliberately set to a saturated magenta real emoji artwork is very
    // unlikely to contain: if the color-glyph path tinted the atlas sample
    // with the cell foreground (like the R8 mask path's `color.a * coverage`
    // formula does), the rendered pixel would trend toward this exact color.
    // Passthrough sampling (REQ-EMOJI-2) should not.
    let magenta_fg = Color::Rgb(Rgb::new(255, 0, 255));
    let row = Row {
        cells: vec![Cell {
            ch: '\u{1F600}',
            combining: String::new(),
            fg: magenta_fg,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        }],
        wrapped: false,
        dirty: true,
    };
    let snap = FrameSnapshot {
        scroll_shift: 0,
        rows: vec![row],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols: 1,
        rows_n: 1,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    };
    let theme = Theme::new();

    renderer.rebuild_cells(&snap, &mut font, &theme);
    renderer.sync_atlas(&device, &queue, &mut font);

    let (target, view) = render_target(&device, 64, 64);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error drawing a FLAG_COLOR_GLYPH instance: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target, 64, 64);
    let has_non_tinted_opaque_pixel = pixels.chunks_exact(4).any(|p| {
        let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
        // A tinted-like-the-mask-path pixel at near-full coverage would land
        // near-pure magenta; require at least one clearly opaque pixel that
        // is not that.
        a > 200 && !(r > 230 && g < 40 && b > 230)
    });
    assert!(
        has_non_tinted_opaque_pixel,
        "expected at least one opaque pixel that is not magenta-tinted — a color glyph must \
         sample the RGBA8 atlas as passthrough (REQ-EMOJI-2), not tint with the cell fg color"
    );
}
