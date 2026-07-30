use super::shared::*;
use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, GridPadding, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, Selection, SelectionPoint, TerminalColors};
use noa_render::{
    CommandPaletteSnapshot, FrameSnapshot, OverlayStyle, PaletteRow, Renderer, Theme,
    command_palette_layout, overlay_surface_alpha, set_overlay_surface_alpha,
};

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
    let mut atlas_cache = noa_render::GlyphAtlasCache::default();
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    for text in ["first tab", "second tab"] {
        let atlases = atlas_cache.get(&device, &queue, format, &font);
        let mut renderer = Renderer::with_pipelines(
            &device,
            &queue,
            &pipelines,
            &atlases,
            &mut font,
            DEFAULT_GRID_PADDING,
        )
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
    let row = Row::from_cells(
        vec![
            Cell {
                ch: 'A',
                grapheme: None,
                fg: Color::Palette(1),
                bg: Color::Default,
                underline_color: None,
                hyperlink: None,
                attrs: CellAttrs::empty(),
            },
            Cell {
                ch: 'g',
                grapheme: None,
                fg: Color::Default,
                bg: Color::Palette(4),
                underline_color: None,
                hyperlink: None,
                attrs: CellAttrs::empty(),
            },
            Cell::default(),
            Cell::default(),
        ],
        false,
        true,
    );
    let snap = FrameSnapshot {
        scroll_shift: 0,
        rows: vec![row],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        copy_cursor: Some(SelectionPoint::new(2, 0)),
        colors: TerminalColors::default(),
        selection: Some(Selection::new(
            SelectionPoint::new(1, 0),
            SelectionPoint::new(1, 0),
        )),
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        viewport_offset: 0,
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
        record_rows: None,
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
        .map(|_| Row::from_cells(vec![Cell::default(); cols as usize], false, true))
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
        viewport_offset: 0,
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
                    enabled: true,
                },
                noa_render::PaletteRow::Entry {
                    title: "Split Down".to_string(),
                    hint: Some("\u{21e7}\u{2318}D".to_string()),
                    match_positions: vec![0, 1],
                    enabled: true,
                },
                noa_render::PaletteRow::Entry {
                    title: "Toggle Split Zoom".to_string(),
                    hint: None,
                    match_positions: vec![7, 8],
                    enabled: true,
                },
            ],
            selected: 1,
            total_entries: 3,
        }),
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
        record_rows: None,
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

/// Glassmorphism fix-1-followup regression: the command-palette scratch's
/// alpha must be uniform across the whole card face — the clear (fix 1)
/// carries `overlay_surface_alpha()`, and a plain row must NOT also draw an
/// `overlay_surface_alpha()`-carrying background quad on top of it (that
/// double-applies the alpha, e.g. `0.68 + 0.68*0.32 = 0.898` instead of
/// `0.68`). The selected row is the one row that legitimately needs a
/// different fill (its accent wash); see `append_command_palette_instances`'s
/// `selected_wash` for why that can only be *bounded* close to the target
/// alpha, not made exactly equal, under ordinary "over" blending.
///
/// This draws the raw cell-instance scratch directly (no card composite —
/// that's `cards.rs`'s job), so it observes exactly what `noa-app`'s
/// `set_clear_color`-after-`rebuild_cells` + `draw` sequence would produce.
#[test]
fn command_palette_surface_alpha_is_uniform_across_plain_and_selected_rows() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping command-palette surface-alpha test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    // Zero padding: grid cell (c, r) maps to pixel (c*cell_w, r*cell_h)
    // exactly, so the probe pixels below don't need to account for a margin.
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        GridPadding::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("build renderer");

    let cols = 30u16;
    let rows_n = 8u16;
    let (cell_w, cell_h) = {
        let m = font.metrics();
        (m.cell_w, m.cell_h)
    };
    let surface_size = PixelSize {
        w: (f32::from(cols) * cell_w).ceil() as u32,
        h: (f32::from(rows_n) * cell_h).ceil() as u32,
    };
    renderer.resize(surface_size);

    let palette = CommandPaletteSnapshot {
        query: "sp".to_string(),
        rows: vec![
            PaletteRow::Entry {
                title: "Split Right".to_string(),
                hint: None,
                match_positions: vec![],
                enabled: true,
            },
            PaletteRow::Entry {
                title: "Split Down".to_string(),
                hint: None,
                match_positions: vec![],
                enabled: true,
            },
            PaletteRow::Entry {
                title: "Toggle Split Zoom".to_string(),
                hint: None,
                match_positions: vec![],
                enabled: true,
            },
        ],
        selected: 1,
        total_entries: 3,
    };
    // Row 0 (query) and entry index 0 are plain; entry index 1 ("Split
    // Down") is selected. Computed the same way the app computes it, so this
    // test breaks (loudly) if the block's geometry formula ever changes.
    let layout =
        command_palette_layout(&palette, cols, rows_n).expect("palette layout for a roomy grid");
    let query_row = layout.y0;
    let selected_row = layout.y0 + 1 + 1; // list_y0 (y0+1) + entry index 1
    let plain_entry_row = layout.y0 + 1; // list_y0 + entry index 0
    // Row 0 of the grid sits above the block (`layout.y0 >= 1` here), so it's
    // untouched by any palette instance — a pure "clear only" probe, standing
    // in for the card's own interior padding margin in production (both are
    // fed by nothing but the clear).
    assert!(query_row >= 1, "test fixture assumption: block below row 0");

    let rows: Vec<Row> = (0..rows_n)
        .map(|_| Row::from_cells(vec![Cell::default(); cols as usize], false, true))
        .collect();
    // Cursor hidden: `Cursor::default()` is visible at (0, 0) by construction
    // (DECTCEM defaults on), which would otherwise paint an opaque block over
    // the very "clear only" probe pixel this test relies on.
    let cursor = Cursor {
        visible: false,
        ..Cursor::default()
    };
    let snap = FrameSnapshot {
        scroll_shift: 0,
        row_dirty: vec![true; rows.len()],
        rows,
        cursor,
        copy_cursor: None,
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        viewport_offset: 0,
        active_is_alt: false,
        cols,
        rows_n,
        focused: true,
        cursor_blink_visible: false,
        hover_link: None,
        search_prompt: None,
        command_palette: Some(palette),
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
        record_rows: None,
    };

    let target = 0.68_f32;
    set_overlay_surface_alpha(target);
    assert_eq!(overlay_surface_alpha(), target);

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    // After `rebuild_cells` (mirrors `noa-app`'s fix-1 ordering — see
    // `crates/noa-app/src/app/sidebar/palette.rs`'s `set_clear_color` call
    // sites) so this clear isn't clobbered by the snapshot's own default bg.
    let style = OverlayStyle::from_theme(&Theme::new());
    renderer.set_clear_color(style.surface_bg());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (target_tex, view) = render_target(&device, surface_size.w, surface_size.h);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    // Always restore, even on assertion failure below, so a failing run
    // doesn't leak a translucent surface alpha into whatever test runs next
    // in this process.
    set_overlay_surface_alpha(1.0);
    assert!(
        err.is_none(),
        "wgpu validation error during command-palette surface-alpha draw: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target_tex, surface_size.w, surface_size.h);
    let alpha_at = |col: u16, row: u16| {
        let x = (f32::from(col) * cell_w + cell_w * 0.5) as u32;
        let y = (f32::from(row) * cell_h + cell_h * 0.5) as u32;
        let offset = ((y * surface_size.w + x) * 4 + 3) as usize;
        pixels[offset]
    };
    // Column `layout.x0`: the block's leading pad column, always a literal
    // space (`palette_line`'s one-space margin) — no glyph ink there, so the
    // sampled alpha is purely the row's background treatment.
    let probe_col = layout.x0;

    let expected = (target * 255.0).round() as i16;
    let padding_alpha = i16::from(alpha_at(probe_col, 0));
    let plain_alpha = i16::from(alpha_at(probe_col, plain_entry_row));
    let query_alpha = i16::from(alpha_at(probe_col, query_row));
    let selected_alpha = i16::from(alpha_at(probe_col, selected_row));

    for (label, got) in [
        ("padding (row 0, outside the block)", padding_alpha),
        ("plain entry row", plain_alpha),
        ("query row", query_alpha),
    ] {
        assert!(
            (got - expected).abs() <= 3,
            "{label} alpha should equal overlay_surface_alpha() ({expected}), got {got}"
        );
    }

    // The selected row can't be made exactly `expected` (see the module doc
    // above), but it must land much closer to it than the pre-fix bug did:
    // pre-fix this pixel would read ~229 (`0.68 + 0.68*0.32`, i.e.
    // `rgba_surface(selected_bg)` stacked on the clear); this fix's formula
    // gives `0.20 + 0.68*0.80 = 0.744` -> ~190.
    let expected_selected = (SELECTED_ROW_WASH_ALPHA_FOR_TEST
        + target * (1.0 - SELECTED_ROW_WASH_ALPHA_FOR_TEST))
        * 255.0;
    assert!(
        (f32::from(selected_alpha) - expected_selected).abs() <= 4.0,
        "selected row alpha should be ~{expected_selected:.0} (wash formula), got {selected_alpha}"
    );
    assert!(
        selected_alpha < 210,
        "selected row alpha {selected_alpha} is too close to the pre-fix bug's ~229 (0.68 stacked on 0.68)"
    );
}
// Mirrors `SELECTED_ROW_WASH_ALPHA` in `crates/noa-render/src/renderer/overlay.rs`
// (private to that module) — kept here only so this test's expected-value
// formula is self-documenting; update both if that constant ever changes.
const SELECTED_ROW_WASH_ALPHA_FOR_TEST: f32 = 0.20;

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
        let make_row = |ch: char, dirty: bool| {
            Row::from_cells(
                vec![Cell {
                    ch,
                    ..Cell::default()
                }],
                false,
                dirty,
            )
        };
        FrameSnapshot {
            scroll_shift: 0,
            rows: vec![
                make_row(first, row_dirty[0]),
                make_row(second, row_dirty[1]),
            ],
            row_dirty: row_dirty.to_vec(),
            cursor: Cursor::default(),
            copy_cursor: None,
            colors: TerminalColors::default(),
            selection: None,
            search: SearchState::default(),
            row_base: 0,
            abs_row_base: 0,
            viewport_offset: 0,
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
            record_rows: None,
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
    let row = Row::from_cells(
        vec![Cell {
            ch: '\u{1F600}',
            grapheme: None,
            fg: magenta_fg,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        }],
        false,
        true,
    );
    let snap = FrameSnapshot {
        scroll_shift: 0,
        rows: vec![row],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        copy_cursor: None,
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        viewport_offset: 0,
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
        record_rows: None,
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
