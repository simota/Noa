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

use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, PixelSize};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, Selection, SelectionPoint, TerminalColors};
use noa_render::{
    DrawOp, FrameSnapshot, PaneFrame, PaneId, PaneRect, Renderer, Theme, build_draw_plan,
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
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
    let initial_generation = font.atlas_generation();

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    if font.atlas_generation() == initial_generation {
        eprintln!(
            "installed monospace font did not rasterize 'M' — skipping split atlas-ordering test"
        );
        return;
    }
    renderer.sync_atlas(&device, &queue, &mut font);

    assert_eq!(renderer.atlas_seen_generation(), font.atlas_generation());

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
    let before_size = font.atlas_size();

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
    if font.atlas_size() == before_size {
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

    let initial_generation = font.atlas_generation();
    let glyph = font.get_or_raster('M');
    if glyph.atlas_size == [0, 0] || font.atlas_generation() == initial_generation {
        eprintln!("installed monospace font did not rasterize 'M' — skipping atlas sync test");
        return;
    }
    let generation = font.atlas_generation();

    first.sync_atlas(&device, &queue, &mut font);
    second.sync_atlas(&device, &queue, &mut font);

    assert_eq!(first.atlas_seen_generation(), generation);
    assert_eq!(second.atlas_seen_generation(), generation);
}
