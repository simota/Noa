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

use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, Selection, SelectionPoint, TerminalColors};
use noa_render::{
    DrawOp, FrameSnapshot, OverviewThumbnailResources, PaneFrame, PaneId, PaneRect, Renderer,
    Theme, build_draw_plan, renderer_construction_count,
};

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
        cols,
        rows_n: 1,
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
        cols: 4,
        rows_n: 1,
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

    let mut overview =
        OverviewThumbnailResources::for_renderer(&device, &renderer, scratch_size, tile_size, 1);
    assert_eq!(overview.format(), renderer.target_format());
    assert_eq!(overview.scratch_size(), scratch_size);
    assert_eq!(overview.tile_size(), tile_size);
    assert_eq!(overview.tile_count(), 1);

    let before_blit = renderer_construction_count();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, 0)
        .expect("blit existing renderer to overview tile");
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(
        renderer_construction_count(),
        before_blit,
        "overview_renderer_reuse: blitting a tile must reuse the existing Renderer"
    );
    assert!(
        err.is_none(),
        "wgpu validation error during overview blit draw: {err:?}"
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
    let mut overview =
        OverviewThumbnailResources::for_renderer(&device, &renderer, scratch_size, tile_size, 1);

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, 0)
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
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, 0)
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
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, 0)
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
            );
            overview
                .render_existing_renderer_to_tile(&device, &queue, &mut renderer, 0)
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
            cols: 1,
            rows_n: 2,
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
        rows: vec![row],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        cols: 1,
        rows_n: 1,
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
