//! Headless GPU regression tests: build the real pipeline AND render one frame
//! on an actual adapter, asserting no wgpu **validation** error.
//!
//! These catch two classes of bug a plain `cargo build` cannot, because they
//! only surface at device runtime:
//!   1. shader ↔ bind-group-layout mismatches (a binding used in a stage whose
//!      layout visibility omits it) — caught at pipeline creation;
//!   2. uniform/instance buffer layout mismatches (Rust `#[repr(C)]` vs WGSL
//!      std140) — caught at draw time ("Buffer is bound with size N where the
//!      shader expects M").
//!
//! Both skip gracefully where no GPU adapter is available (headless CI without
//! a Metal/Vulkan device).

use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, GridPadding, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, Selection, SelectionPoint, TerminalColors};
use noa_render::{
    BackgroundImage, BackgroundImageFit, BackgroundImagePosition, CardPipeline, CardStyle,
    CardTexturePlacement, CardTilePlacement, CommandPaletteSnapshot, DrawOp, FrameSnapshot,
    ImagePlacementSnapshot, OverviewThumbnailResources, PaneFrame, PaneId, PaneRect, Renderer,
    SnapshotImage, Theme, build_draw_plan,
};
use std::sync::Arc;

/// Shared title-bar band height + card color for the overview headless tests
/// (mirrors `noa_app::tab_overview`'s compile-time constants; noa-render can't
/// depend on noa-app, so the tests re-state them).
const TEST_TITLE_BAR_H: u32 = 30;
const TEST_CARD_COLOR: [f32; 4] = [0.078, 0.091, 0.127, 1.0];

/// Acquire a real device+queue, or `None` when no adapter exists (skip).
fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

fn snapshot_for_text(text: &str) -> FrameSnapshot {
    let mut cells = text
        .chars()
        .map(|ch| Cell {
            ch,
            combining: String::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        })
        .collect::<Vec<_>>();
    if cells.is_empty() {
        cells.push(Cell::default());
    }

    let cols = cells.len().min(u16::MAX as usize) as u16;
    FrameSnapshot {
        scroll_shift: 0,
        rows: vec![Row {
            cells,
            wrapped: false,
            dirty: true,
        }],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols,
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
    }
}

fn render_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-test-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        // COPY_SRC so tests that need pixel readback (color-glyph passthrough
        // assertion) can reuse this same target instead of a bespoke one.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
}

/// Read back a `Bgra8UnormSrgb` render target as RGBA8 bytes (`width *
/// height * 4`, row-major, B/R already swapped to RGBA order).
fn read_rgba_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("noa-test-readback-buffer"),
        size: (padded_bytes_per_row * height) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("noa-test-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device for map_async");
    rx.recv()
        .expect("map_async callback never fired")
        .expect("map readback buffer");

    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        let row_bytes = &data[start..start + (width * 4) as usize];
        // Bgra8UnormSrgb texture: swap B/R so callers get RGBA order.
        for px in row_bytes.chunks_exact(4) {
            out.push(px[2]);
            out.push(px[1]);
            out.push(px[0]);
            out.push(px[3]);
        }
    }
    drop(data);
    buffer.unmap();
    out
}

fn hash_pixels(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn rebuild_text_frame(
    renderer: &mut Renderer,
    font: &mut FontGrid,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    text: &str,
) {
    let snap = snapshot_for_text(text);
    renderer.rebuild_cells(&snap, font, &Theme::new());
    renderer.sync_atlas(device, queue, font);
}

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

#[test]
fn overview_blit_pipeline_draws_tile_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit GPU draw test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 128, h: 64 };
    let tile_size = PixelSize { w: 64, h: 32 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "overview");

    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &renderer,
        scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );
    assert_eq!(overview.format(), renderer.target_format());
    assert_eq!(overview.scratch_size(), scratch_size);
    assert_eq!(overview.tile_size(), tile_size);
    assert_eq!(overview.tile_count(), 1);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("blit existing renderer to overview tile");
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during overview blit draw: {err:?}"
    );
}

#[test]
fn overview_blit_scratch_resizes_to_source_frame_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit scratch resize test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let initial_scratch_size = PixelSize { w: 64, h: 32 };
    let source_size = PixelSize { w: 160, h: 96 };
    let tile_size = PixelSize { w: 80, h: 50 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(source_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "source");

    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &renderer,
        initial_scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );
    assert_eq!(overview.scratch_size(), initial_scratch_size);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, source_size, 0)
        .expect("blit source-sized renderer to overview tile");
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(overview.scratch_size(), source_size);
    assert!(
        err.is_none(),
        "wgpu validation error during overview scratch resize blit: {err:?}"
    );
}

#[test]
fn overview_blit_tile_pixel_hash_tracks_content_changes() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit pixel-hash test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 160, h: 80 };
    let tile_size = PixelSize { w: 80, h: 40 };
    let mut font =
        FontGrid::new(28.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &renderer,
        scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render first tile");
    let first = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render unchanged tile");
    let unchanged = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));
    assert_eq!(
        first, unchanged,
        "unchanged tab content should produce the same overview tile hash"
    );

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "WWW");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render changed tile");
    let changed = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));
    assert_ne!(
        unchanged, changed,
        "changed tab content should change the overview tile pixel hash"
    );
}

#[test]
fn overview_blit_resources_drop_before_renderer_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit teardown test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 96, h: 48 };
    let tile_size = PixelSize { w: 48, h: 24 };
    let mut font =
        FontGrid::new(18.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    {
        let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
            .expect("build renderer");
        renderer.resize(scratch_size);
        rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "drop");

        {
            let mut overview = OverviewThumbnailResources::for_renderer(
                &device,
                &renderer,
                scratch_size,
                tile_size,
                1,
                TEST_TITLE_BAR_H,
                TEST_CARD_COLOR,
            );
            overview
                .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
                .expect("render before teardown");
        }
        drop(renderer);
    }
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after overview teardown");
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during overview resources -> renderer teardown: {err:?}"
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
fn split_pipeline_syncs_same_frame_new_glyphs_for_two_panes() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split atlas-ordering test");
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
    renderer.resize(PixelSize { w: 128, h: 32 });

    let left = snapshot_for_text("M");
    let right = snapshot_for_text("M");
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 64, 32)),
        (PaneId::new(2), PaneRect::new(65, 0, 63, 32)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &left,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &right,
        },
    ];
    let initial_generation = font.mask_atlas_generation();

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    if font.mask_atlas_generation() == initial_generation {
        eprintln!(
            "installed monospace font did not rasterize 'M' — skipping split atlas-ordering test"
        );
        return;
    }
    renderer.sync_atlas(&device, &queue, &mut font);

    assert_eq!(
        renderer.mask_atlas_seen_generation(),
        font.mask_atlas_generation()
    );

    let (_target, view) = render_target(&device, 128, 32);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, None, None);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during 2-pane same-frame atlas draw: {err:?}"
    );
}

#[test]
fn split_pipeline_draws_three_pane_plan_with_overlays_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split draw-plan GPU test");
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
    renderer.resize(PixelSize { w: 160, h: 96 });

    let a = snapshot_for_text("A");
    let b = snapshot_for_text("B");
    let c = snapshot_for_text("C");
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 80, 96)),
        (PaneId::new(2), PaneRect::new(81, 0, 79, 47)),
        (PaneId::new(3), PaneRect::new(81, 48, 79, 48)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &a,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &b,
        },
        PaneFrame {
            pane: layout[2].0,
            rect: layout[2].1,
            snapshot: &c,
        },
    ];
    let focused = layout[1].0;
    let plan = build_draw_plan(&layout, Some(focused), None);
    assert!(
        plan.iter()
            .any(|op| matches!(op, DrawOp::Dividers { rects } if !rects.is_empty())),
        "3-pane split plan should include same-pass divider geometry"
    );
    assert!(
        matches!(plan.last(), Some(DrawOp::FocusIndicator { pane, rects }) if *pane == focused && !rects.is_empty()),
        "focused split plan should include focus overlay geometry"
    );

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 160, 96);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, Some(focused), None);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during 3-pane scissored draw with dividers: {err:?}"
    );
}

/// Regression: the z-band interleave binds the image pipeline inside a pane's
/// draw and only re-establishes the cell pipeline inside `draw_cell_range`.
/// When the final pane's trailing (decoration) cell range is empty — all-blank
/// cells emit no glyph/decoration instances — and it carries a z>=0 image, the
/// image pipeline is the last thing bound as the pane loop ends. The following
/// `Dividers` / `FocusIndicator` overlay draws must set the cell pipeline
/// themselves, or wgpu aborts inside the macOS delegate on a pipeline vs
/// bind-group mismatch. Focus the image pane so BOTH overlays draw after it.
#[test]
fn split_overlays_draw_after_final_pane_image_band_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overlay-after-image-band test");
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
    renderer.resize(PixelSize { w: 160, h: 96 });

    let a = snapshot_for_text("A");
    let b = snapshot_for_text("B");
    // Final pane: all-blank cells plus an opaque image at z=0 (above text), so
    // its trailing cell range is empty and the last bound pipeline before the
    // overlays is the image pipeline.
    let c = image_snapshot(4, 4, Color::Rgb(Rgb::new(0, 40, 0)), [0, 0, 255, 255], 0, 0);
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 80, 96)),
        (PaneId::new(2), PaneRect::new(81, 0, 79, 47)),
        (PaneId::new(3), PaneRect::new(81, 48, 79, 48)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &a,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &b,
        },
        PaneFrame {
            pane: layout[2].0,
            rect: layout[2].1,
            snapshot: &c,
        },
    ];
    let focused = layout[2].0;
    let plan = build_draw_plan(&layout, Some(focused), None);
    assert!(
        plan.iter()
            .any(|op| matches!(op, DrawOp::Dividers { rects } if !rects.is_empty())),
        "3-pane split plan should include divider geometry"
    );
    assert!(
        matches!(plan.last(), Some(DrawOp::FocusIndicator { pane, rects }) if *pane == focused && !rects.is_empty()),
        "focusing the image pane should append its focus overlay after its image band"
    );

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 160, 96);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, Some(focused), None);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing split overlays after a final-pane image band: {err:?}"
    );
}

#[test]
fn split_pipeline_rebuilds_all_pane_bind_groups_after_atlas_reallocation() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split atlas-reallocation test");
        return;
    };
    let mut font = FontGrid::new(220.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 512, h: 256 });

    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 256, 256)),
        (PaneId::new(2), PaneRect::new(257, 0, 255, 256)),
    ];
    let first = snapshot_for_text("A");
    let second = snapshot_for_text("B");
    let initial_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &first,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &second,
        },
    ];

    renderer.rebuild_panes(&initial_panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (_initial_target, initial_view) = render_target(&device, 512, 256);
    renderer.draw_panes(&device, &queue, &initial_view, &layout, None, None);

    let before_counts = renderer.pane_bind_group_rebuild_counts();
    assert_eq!(before_counts.len(), 2);
    let before_size = font.mask_atlas_size();

    let pressure = snapshot_for_text(&large_visible_glyph_string());
    let still_visible = snapshot_for_text("Z");
    let pressure_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &pressure,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &still_visible,
        },
    ];
    renderer.rebuild_panes(&pressure_panes, &mut font, &Theme::new());
    if font.mask_atlas_size() == before_size {
        eprintln!(
            "large glyph pressure did not grow the atlas — skipping split atlas-reallocation test"
        );
        return;
    }

    let (_target, view) = render_target(&device, 512, 256);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.sync_atlas(&device, &queue, &mut font);
    let after_counts = renderer.pane_bind_group_rebuild_counts();
    renderer.draw_panes(&device, &queue, &view, &layout, None, None);
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(after_counts.len(), before_counts.len());
    assert!(
        before_counts
            .iter()
            .zip(after_counts.iter())
            .all(|(before, after)| after > before),
        "atlas reallocation must rebuild every pane bind group: before={before_counts:?} after={after_counts:?}"
    );
    assert!(
        err.is_none(),
        "wgpu validation error after atlas reallocation draw: {err:?}"
    );
}

fn large_visible_glyph_string() -> String {
    ('!'..='~')
        .chain('\u{00A1}'..='\u{017F}')
        .chain('\u{0370}'..='\u{03FF}')
        .chain('\u{0400}'..='\u{04FF}')
        .chain('\u{3041}'..='\u{3096}')
        .take(512)
        .collect()
}

#[test]
fn shared_font_atlas_syncs_to_multiple_renderers() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping multi-renderer atlas test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut first = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build first renderer");
    let mut second = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build second renderer");

    let initial_generation = font.mask_atlas_generation();
    let glyph = font.get_or_raster('M');
    if glyph.atlas_size == [0, 0] || font.mask_atlas_generation() == initial_generation {
        eprintln!("installed monospace font did not rasterize 'M' — skipping atlas sync test");
        return;
    }
    let generation = font.mask_atlas_generation();

    first.sync_atlas(&device, &queue, &mut font);
    second.sync_atlas(&device, &queue, &mut font);

    assert_eq!(first.mask_atlas_seen_generation(), generation);
    assert_eq!(second.mask_atlas_seen_generation(), generation);
}

/// AC-WP1-03 / FM-01: a `FLAG_COLOR_GLYPH` instance must draw with no wgpu
/// validation error (the two-atlas bind-group layout, `vs_main`'s
/// texel-space color UV, and `fs_main`'s `color_atlas_tex` normalization all
/// have to agree). Also a lightweight AC-WP1-02 check: read the drawn pixels
/// back and confirm the color glyph is *not* tinted by the cell's foreground
/// color the way the R8 mask path would be (real color-vs-tint distinction).
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

/// FM-09: force the color atlas alone to grow (many emoji glyphs) while
/// holding the mask atlas untouched, and assert every pane bind group is
/// still rebuilt. This is the case a "fixed the mask-atlas sync block, forgot
/// the color one" bug would slip through if `sync_atlas` duplicated its two
/// atlas blocks instead of sharing one code path.
#[test]
fn split_pipeline_rebuilds_bind_groups_after_color_only_atlas_reallocation() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping color-only atlas-reallocation test");
        return;
    };
    let mut font = FontGrid::new(200.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 512, h: 256 });

    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 256, 256)),
        (PaneId::new(2), PaneRect::new(257, 0, 255, 256)),
    ];
    let stable = snapshot_for_text("Z");
    let initial_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &stable,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &stable,
        },
    ];

    renderer.rebuild_panes(&initial_panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (_initial_target, initial_view) = render_target(&device, 512, 256);
    renderer.draw_panes(&device, &queue, &initial_view, &layout, None, None);

    let before_counts = renderer.pane_bind_group_rebuild_counts();
    assert_eq!(before_counts.len(), 2);
    let mask_size_before = font.mask_atlas_size();
    let mask_generation_before = font.mask_atlas_generation();
    let color_size_before = font.color_atlas_size();

    // Populate (and, hopefully, grow) the color atlas directly via
    // `get_or_raster` so we control exactly which characters are confirmed
    // color glyphs — this keeps the mask atlas provably untouched by this
    // pressure step, rather than hoping every candidate codepoint resolves
    // to a color face.
    let (emoji_text, color_atlas_grew) = build_color_atlas_pressure_string(&mut font);
    if !color_atlas_grew || emoji_text.is_empty() {
        eprintln!(
            "no color-capable emoji pressure grew the color atlas in this environment — \
             skipping color-only atlas-reallocation test"
        );
        return;
    }
    assert_eq!(
        font.mask_atlas_generation(),
        mask_generation_before,
        "building emoji-only pressure must not touch the mask atlas"
    );
    assert_eq!(
        font.mask_atlas_size(),
        mask_size_before,
        "building emoji-only pressure must not grow the mask atlas"
    );
    assert!(font.color_atlas_size() != color_size_before);

    let emoji_pressure = snapshot_for_text(&emoji_text);
    let pressure_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &emoji_pressure,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &stable,
        },
    ];
    renderer.rebuild_panes(&pressure_panes, &mut font, &Theme::new());

    // Rendering the (already-rastered, cache-hit) pressure text must not
    // have touched the mask atlas either.
    assert_eq!(font.mask_atlas_generation(), mask_generation_before);
    assert_eq!(font.mask_atlas_size(), mask_size_before);

    let (_target, view) = render_target(&device, 512, 256);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.sync_atlas(&device, &queue, &mut font);
    let after_counts = renderer.pane_bind_group_rebuild_counts();
    renderer.draw_panes(&device, &queue, &view, &layout, None, None);
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(after_counts.len(), before_counts.len());
    assert!(
        before_counts
            .iter()
            .zip(after_counts.iter())
            .all(|(before, after)| after > before),
        "color-only atlas reallocation must still rebuild every pane bind group (FM-09): \
         before={before_counts:?} after={after_counts:?}"
    );
    assert!(
        err.is_none(),
        "wgpu validation error after color-only atlas reallocation draw: {err:?}"
    );
}

/// Directly rasterize a range of emoji candidates, collecting only the ones
/// confirmed as color glyphs (`GlyphInfo.color`) into a pressure string.
/// Returns `(text, atlas_grew)`. Stops early once the color atlas has grown
/// and a reasonable number of glyphs were collected, to bound runtime.
fn build_color_atlas_pressure_string(font: &mut noa_font::FontGrid) -> (String, bool) {
    let before = font.color_atlas_size();
    let mut text = String::new();
    let mut grew = false;
    for ch in emoji_candidate_range() {
        let glyph = font.get_or_raster(ch);
        if glyph.color && glyph.atlas_size != [0, 0] {
            text.push(ch);
        }
        if font.color_atlas_size() != before {
            grew = true;
            if text.len() >= 64 {
                break;
            }
        }
    }
    (text, grew)
}

fn emoji_candidate_range() -> impl Iterator<Item = char> {
    ('\u{1F300}'..='\u{1F5FF}')
        .chain('\u{1F600}'..='\u{1F64F}')
        .chain('\u{1F680}'..='\u{1F6FF}')
        .chain('\u{1F900}'..='\u{1F9FF}')
}

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
            },
            noa_render::PaletteRow::Entry {
                title: "Split Down".to_string(),
                hint: None,
                match_positions: vec![0, 1],
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
        .map(|_| Row {
            cells: vec![Cell::default(); layout.block_cols as usize],
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

// ── Kitty-graphics image layer (design Step R) ──────────────────────────

/// A grid of `cols`×`rows` cells with an explicit background color, plus one
/// image placement (id 1, solid `image_color`) covering the whole grid at
/// z-index `z`.
fn image_snapshot(
    cols: u16,
    rows_n: u16,
    bg: Color,
    image_color: [u8; 4],
    epoch: u64,
    z: i32,
) -> FrameSnapshot {
    let rows: Vec<Row> = (0..rows_n)
        .map(|_| Row {
            cells: vec![
                Cell {
                    ch: ' ',
                    combining: String::new(),
                    fg: Color::Default,
                    bg,
                    underline_color: None,
                    hyperlink: None,
                    attrs: CellAttrs::empty(),
                };
                cols as usize
            ],
            wrapped: false,
            dirty: true,
        })
        .collect();
    // 4×4 solid-color image; the Linear sampler stretches it over the quad.
    let rgba: Vec<u8> = image_color
        .iter()
        .copied()
        .cycle()
        .take(4 * 4 * 4)
        .collect();
    FrameSnapshot {
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
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: vec![ImagePlacementSnapshot {
            image_id: 1,
            epoch,
            grid_x: 0,
            grid_y: 0,
            cell_x_off: 0,
            cell_y_off: 0,
            cols,
            rows: rows_n,
            src: None,
            z,
        }],
        images: vec![SnapshotImage {
            id: 1,
            epoch,
            width: 4,
            height: 4,
            rgba: Arc::from(rgba),
        }],
    }
}

#[test]
fn image_layer_draws_image_and_text_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image-layer draw test");
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
    renderer.resize(PixelSize { w: 96, h: 64 });

    // A textful grid with an opaque image over it at z=0 (above text).
    let mut snap = image_snapshot(
        6,
        4,
        Color::Rgb(Rgb::new(180, 0, 0)),
        [0, 0, 255, 255],
        0,
        0,
    );
    snap.rows[0].cells[0].ch = 'A';
    snap.rows[0].cells[0].fg = Color::Palette(2);

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 96, 64);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing an image placement mixed with text: {err:?}"
    );
}

#[test]
fn image_z_band_controls_whether_it_covers_the_cell_background() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image z-band pixel test");
        return;
    };
    let width = 96u32;
    let height = 64u32;
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
    renderer.resize(PixelSize {
        w: width,
        h: height,
    });

    let center = |pixels: &[u8]| -> [u8; 4] {
        let x = (width / 2) as usize;
        let y = (height / 2) as usize;
        let i = (y * width as usize + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // z=0 (above text): the opaque blue image draws over the red cell background.
    let above = image_snapshot(
        40,
        40,
        Color::Rgb(Rgb::new(200, 0, 0)),
        [0, 0, 255, 255],
        0,
        0,
    );
    renderer.rebuild_cells(&above, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target, view) = render_target(&device, width, height);
    renderer.draw(&device, &queue, &view);
    let above_px = center(&read_rgba_pixels(&device, &queue, &target, width, height));
    assert!(
        above_px[2] > above_px[0],
        "z>=0 image must draw OVER the cell background (blue dominant), got {above_px:?}"
    );

    // z below the background threshold: the image draws UNDER the background, so
    // the red background covers it.
    let below = image_snapshot(
        40,
        40,
        Color::Rgb(Rgb::new(200, 0, 0)),
        [0, 0, 255, 255],
        0,
        -2_000_000_000,
    );
    renderer.rebuild_cells(&below, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target2, view2) = render_target(&device, width, height);
    renderer.draw(&device, &queue, &view2);
    let below_px = center(&read_rgba_pixels(&device, &queue, &target2, width, height));
    assert!(
        below_px[0] > below_px[2],
        "z<bg-threshold image must draw UNDER the cell background (red dominant), got {below_px:?}"
    );
}

#[test]
fn image_texture_reuploads_only_on_epoch_bump() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image epoch-reupload test");
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
    renderer.resize(PixelSize { w: 64, h: 48 });
    let (_target, view) = render_target(&device, 64, 48);

    let draw = |renderer: &mut Renderer, font: &mut FontGrid, epoch: u64| {
        let snap = image_snapshot(
            4,
            4,
            Color::Rgb(Rgb::new(0, 80, 0)),
            [255, 255, 0, 255],
            epoch,
            0,
        );
        renderer.rebuild_cells(&snap, font, &Theme::new());
        renderer.sync_atlas(&device, &queue, font);
        renderer.draw(&device, &queue, &view);
    };

    draw(&mut renderer, &mut font, 0);
    let after_first = renderer.image_texture_upload_count();
    assert!(after_first >= 1, "first frame uploads the image texture");

    draw(&mut renderer, &mut font, 0);
    assert_eq!(
        renderer.image_texture_upload_count(),
        after_first,
        "same (id, epoch) must reuse the cached texture — no re-upload"
    );

    draw(&mut renderer, &mut font, 1);
    assert!(
        renderer.image_texture_upload_count() > after_first,
        "an epoch bump must force a texture re-upload"
    );
}

#[test]
fn unicode_placeholder_resolves_and_draws_over_text() {
    use noa_core::GridSize;
    use noa_grid::Terminal;
    use noa_vt::Stream;

    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping placeholder draw test");
        return;
    };
    let width = 96u32;
    let height = 64u32;
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
    renderer.resize(PixelSize {
        w: width,
        h: height,
    });

    // A terminal holding a solid-blue image placed as a virtual placement (U=1)
    // at z=0 (above text), referenced only through a Unicode placeholder cell.
    let mut term = Terminal::new(GridSize::new(6, 4));
    term.set_pixel_metrics(12, 16, width, height);
    let mut stream = Stream::new();
    let blue: Vec<u8> = [0u8, 0, 255, 255]
        .iter()
        .copied()
        .cycle()
        .take(4 * 4 * 4)
        .collect();
    let mut apc = b"\x1b_Ga=T,f=32,s=4,v=4,i=1,U=1,c=6,r=4,C=1;".to_vec();
    let mut b64 = Vec::new();
    noa_grid_test_base64(&blue, &mut b64);
    apc.extend_from_slice(&b64);
    apc.extend_from_slice(b"\x1b\\");
    stream.feed(&apc, &mut term);
    // Fill the whole grid with placeholder cells (image id 1). The first cell of
    // each grid row anchors row/column 0; the rest infer.
    for y in 0..4usize {
        for x in 0..6usize {
            let cell = &mut term.primary.grid[y].cells[x];
            cell.ch = noa_grid::PLACEHOLDER;
            cell.fg = Color::Rgb(Rgb::new(0, 0, 1));
            cell.combining.clear();
            if x == 0 {
                // Row index = y (diacritics table values 0..3).
                cell.combining.push(placeholder_diacritic(y as u32));
                cell.combining.push(placeholder_diacritic(0));
            }
        }
    }

    let snap = FrameSnapshot::from_terminal(&mut term);
    assert!(
        !snap.image_placements.is_empty(),
        "the placeholder cells must resolve to at least one image placement"
    );

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target, view) = render_target(&device, width, height);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "placeholder draw hit a wgpu validation error: {err:?}"
    );

    // Center pixel: the opaque blue image covers the grid.
    let pixels = read_rgba_pixels(&device, &queue, &target, width, height);
    let i = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
    let px = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
    assert!(
        px[2] > px[0],
        "placeholder image (blue) must be visible over the cells, got {px:?}"
    );
}

/// The row/column diacritic encoding `value` (Kitty's table; values 0..=3 are
/// the first four entries). Kept in the test to avoid exposing the table.
fn placeholder_diacritic(value: u32) -> char {
    const FIRST_FOUR: [char; 4] = ['\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}'];
    FIRST_FOUR[value as usize]
}

/// Minimal base64 encoder for the placeholder test's image payload.
fn noa_grid_test_base64(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[(n >> 18 & 63) as usize]);
        out.push(ALPHABET[(n >> 12 & 63) as usize]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize]
        } else {
            b'='
        });
    }
}

/// AC-9 / NFR-3 (headless GPU): a valid background image draws one frame in the
/// lowest z band with no wgpu validation error, and its quad's alpha reflects
/// `background-image-opacity` INDEPENDENTLY of the clear color's
/// `background-opacity`. A 2x2 opaque image at `fit = contain` into a 64x32
/// surface covers a centered 32-wide band (letterbox left/right); reading the
/// rendered alpha back shows the covered band at the image-opacity-blended
/// alpha and the letterbox at the clear (background-opacity) alpha — proving
/// the image is not scaled by `background-opacity`.
#[test]
fn background_image_draws_below_cells_with_independent_opacity() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping background-image GPU draw test");
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
    let (w, h) = (64u32, 32u32);
    renderer.resize(PixelSize { w, h });

    // Clear color alpha = background-opacity = 0.3.
    renderer.set_background_opacity(0.3);
    // 2x2 fully-opaque red image, drawn at background-image-opacity = 0.5.
    let image = BackgroundImage {
        rgba: Arc::from(vec![255u8, 0, 0, 255].repeat(4)),
        width: 2,
        height: 2,
        fit: BackgroundImageFit::Contain,
        position: BackgroundImagePosition::Center,
        repeat: false,
        opacity: 0.5,
    };
    renderer.set_background_image(&device, &queue, Some(image));
    assert!(renderer.has_background_image());

    // Blank snapshot: the single default cell emits no background quad, so the
    // frame is just the clear color + the background image.
    let snap = snapshot_for_text(" ");
    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (target, view) = render_target(&device, w, h);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error drawing the background image: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target, w, h);
    let alpha_at = |x: u32, y: u32| -> u8 {
        let idx = ((y * w + x) * 4 + 3) as usize;
        pixels[idx]
    };
    // contain: 2x2 -> scale 16 -> 32x32 centered, covering x in [16, 48).
    let covered = alpha_at(32, 16); // center of the image band
    let letterbox = alpha_at(4, 16); // left edge, image absent

    // Clear (background-opacity 0.3) -> ~0.3 * 255 = 76.
    assert!(
        (68..=84).contains(&letterbox),
        "letterbox alpha should reflect background-opacity (~76): got {letterbox}"
    );
    // Straight-alpha blend: 0.5 (image) + 0.3 * (1 - 0.5) = 0.65 -> ~166.
    assert!(
        (158..=174).contains(&covered),
        "covered alpha should reflect background-image-opacity blended over the \
         clear color (~166), independent of background-opacity: got {covered}"
    );
    assert!(
        covered > letterbox + 40,
        "the image quad's alpha ({covered}) must clearly exceed the clear-only \
         letterbox alpha ({letterbox}), proving the image is not scaled by \
         background-opacity (NFR-3)"
    );
}
