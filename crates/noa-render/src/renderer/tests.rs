//! Split out of the former monolithic `renderer.rs` — unit tests.

use super::*;
use noa_core::{Color, GridSize, Rgb};
use noa_font::{FontConfig, ShapeCell, StyleKey};
use noa_grid::{Cell, Cursor, SearchMatch, SelectionPoint, Terminal};
use noa_vt::Stream;

use crate::segment::CellRenderInfo;

fn skip_font() -> Option<FontGrid> {
    match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => Some(font),
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            None
        }
    }
}

/// Acquire a real device+queue, or `None` when no adapter exists (skip).
/// Mirrors `noa-render/tests/pipeline.rs`'s headless-GPU skip pattern.
fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-renderer-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

#[test]
fn sync_atlas_uploads_rebuilt_font_grid_even_when_generation_restarts() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping rebuilt FontGrid atlas sync test");
        return;
    };
    let Some(mut first_font) = skip_font() else {
        return;
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut first_font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    let first_identity = first_font.atlas_identity();
    assert_eq!(renderer.mask_atlas_seen_identity, first_identity);
    assert_eq!(renderer.color_atlas_seen_identity, first_identity);

    let mut rebuilt_font = match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    assert_ne!(rebuilt_font.atlas_identity(), first_identity);
    assert_eq!(
        rebuilt_font.mask_atlas_generation(),
        renderer.mask_atlas_seen_generation(),
        "the regression requires a fresh FontGrid whose atlas generation restarts"
    );

    renderer.sync_atlas(&device, &queue, &mut rebuilt_font);

    assert_eq!(
        renderer.mask_atlas_seen_identity,
        rebuilt_font.atlas_identity(),
        "mask atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
    assert_eq!(
        renderer.color_atlas_seen_identity,
        rebuilt_font.atlas_identity(),
        "color atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
}

#[test]
fn renderer_target_format_is_srgb_stays_in_lockstep_with_surface_format() {
    // WP3 / REQ-AA-1 / AC-WP3-01: `Renderer::new` derives
    // `target_format_is_srgb` straight from the surface format passed
    // in, so `surface_output_rgba` only linearizes when the surface
    // actually is sRGB — no double-gamma, no no-gamma artifact.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping target_format_is_srgb lockstep test");
        return;
    };
    let Some(mut font) = skip_font() else {
        return;
    };

    let non_srgb = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer with non-sRGB surface format");
    assert!(
        !non_srgb.target_format_is_srgb,
        "Bgra8Unorm is not sRGB; native gamma-correct blending requires no linearization"
    );

    let srgb = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer with sRGB surface format");
    assert!(
        srgb.target_format_is_srgb,
        "Bgra8UnormSrgb is sRGB; solid colors must still be pre-linearized on this fallback path"
    );
}

#[test]
fn background_opacity_scales_only_clear_alpha() {
    let base = [0.1, 0.2, 0.3, 1.0];
    // Fully opaque leaves the clear color untouched.
    assert_eq!(apply_background_opacity(base, 1.0), base);
    // Partial opacity scales only alpha; rgb stays the theme background.
    assert_eq!(apply_background_opacity(base, 0.8), [0.1, 0.2, 0.3, 0.8]);
    // Zero is fully transparent; out-of-range values clamp.
    assert_eq!(apply_background_opacity(base, 0.0), [0.1, 0.2, 0.3, 0.0]);
    assert_eq!(apply_background_opacity(base, 2.0), base);
}

#[test]
fn default_bg_cell_emits_no_background_quad_so_clear_color_shows_through() {
    // The opacity path relies on default-background cells NOT painting a
    // bg quad: the (opacity-scaled) clear color is what fills them. A cell
    // with an explicit bg still paints an opaque quad.
    let Some(mut font) = skip_font() else {
        return;
    };
    let mut terminal = Terminal::new(GridSize::new(2, 1));
    terminal.primary.cursor.visible = false;
    terminal.primary.grid[0].cells[0].ch = ' ';
    terminal.primary.grid[0].cells[0].bg = Color::Default;
    terminal.primary.grid[0].cells[1].ch = ' ';
    terminal.primary.grid[0].cells[1].bg = Color::Rgb(Rgb::new(2, 3, 4));
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let bg_quads: Vec<_> = instances
        .iter()
        .filter(|instance| instance.flags == 0 && instance.glyph_size == [0, 0])
        .collect();
    assert_eq!(
        bg_quads.len(),
        1,
        "only the explicit-bg cell should paint a background quad"
    );
    assert_eq!(bg_quads[0].grid_pos, [1, 0]);
    assert_eq!(
        bg_quads[0].color[3], 255,
        "explicit background quads stay fully opaque regardless of background-opacity"
    );
}

fn metrics(ascent: f32) -> Metrics {
    Metrics {
        cell_w: 10.0,
        cell_h: 24.0,
        ascent,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: 0.0,
        underline_thickness: 1.0,
    }
}

#[test]
fn glyph_bearing_converts_from_baseline_to_cell_top() {
    assert_eq!(glyph_cell_bearing(metrics(18.0), [2, 14]), [2, 4]);
}

#[test]
fn cursor_cell_with_glyph_generates_reversed_glyph_instance() {
    let mut font = match FontGrid::new(14.0, noa_font::FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let glyph = font.get_or_raster('M');
    if glyph.atlas_size == [0, 0] {
        eprintln!("skipping: installed monospace font did not rasterize 'M'");
        return;
    }

    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let cursor_bg_index = instances
        .iter()
        .position(|instance| {
            instance.grid_pos == [0, 0]
                && instance.flags == CellInstance::FLAG_CURSOR
                && instance.glyph_size == [0, 0]
        })
        .expect("cursor cell should have a background cursor instance");
    let cursor_glyph_index = instances
        .iter()
        .position(|instance| {
            instance.grid_pos == [0, 0]
                && instance.flags & CellInstance::FLAG_CURSOR != 0
                && instance.flags & CellInstance::FLAG_GLYPH != 0
        })
        .expect("cursor cell glyph must be retained as a cursor glyph instance");
    assert!(
        cursor_bg_index < cursor_glyph_index,
        "cursor background must be emitted before the glyph so it does not cover text"
    );
    assert_eq!(
        instances[cursor_bg_index].color,
        [240, 10, 20, 255],
        "cursor background should use the cell foreground"
    );
    let cursor_glyph = instances[cursor_glyph_index];

    assert_ne!(
        cursor_glyph.glyph_size,
        [0, 0],
        "cursor glyph instance must sample the atlas instead of becoming a blank quad"
    );
    assert_eq!(
        cursor_glyph.color,
        [2, 3, 4, 255],
        "cursor glyph color should use the cell background"
    );
    assert_eq!(
        instances
            .last()
            .map(|instance| instance.flags & CellInstance::FLAG_GLYPH),
        Some(CellInstance::FLAG_GLYPH),
        "the final cursor-cell instance must not be an opaque blank cursor quad"
    );
}

/// Skip-on-no-font harness shared by the bar/underline/hollow/blink
/// cursor-shape tests below — mirrors
/// `cursor_cell_with_glyph_generates_reversed_glyph_instance`'s guard.
fn font_with_rasterized_m() -> Option<FontGrid> {
    let mut font = match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return None;
        }
    };
    if font.get_or_raster('M').atlas_size == [0, 0] {
        eprintln!("skipping: installed monospace font did not rasterize 'M'");
        return None;
    }
    Some(font)
}

fn rgb_from_instance(instance: &CellInstance) -> Rgb {
    Rgb::new(instance.color[0], instance.color[1], instance.color[2])
}

fn one_cell_terminal_with_cursor_style(style: CursorStyle) -> Terminal {
    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.cursor.style = style;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
    terminal
}

#[test]
fn minimum_contrast_adjusts_low_contrast_glyph_color() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.visible = false;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(0x22, 0x22, 0x22));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    let mut theme = Theme::new();
    theme.minimum_contrast = 4.5;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

    let glyph = instances
        .iter()
        .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
        .expect("glyph must be drawn");
    let adjusted = rgb_from_instance(glyph);
    let bg = Rgb::new(0x00, 0x00, 0x00);
    assert_ne!(adjusted, Rgb::new(0x22, 0x22, 0x22));
    assert!(
        crate::theme::contrast_ratio(adjusted, bg) >= 4.5,
        "adjusted={adjusted:?} ratio={}",
        crate::theme::contrast_ratio(adjusted, bg)
    );
}

#[test]
fn minimum_contrast_keeps_cursor_visible_against_cell_background() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(0x00, 0x00, 0x00));
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    let mut theme = Theme::new();
    theme.minimum_contrast = 3.0;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

    let cursor_bg = instances
        .iter()
        .find(|i| {
            i.grid_pos == [0, 0] && i.flags == CellInstance::FLAG_CURSOR && i.glyph_size == [0, 0]
        })
        .expect("cursor block fill must be drawn");
    let cursor_rgb = rgb_from_instance(cursor_bg);
    let bg = Rgb::new(0x00, 0x00, 0x00);
    assert_ne!(cursor_rgb, bg);
    assert!(
        crate::theme::contrast_ratio(cursor_rgb, bg) >= 3.0,
        "cursor={cursor_rgb:?} ratio={}",
        crate::theme::contrast_ratio(cursor_rgb, bg)
    );
}

#[test]
fn bar_and_underline_cursors_do_not_fill_or_recolor_the_cell() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    for style in [CursorStyle::SteadyBar, CursorStyle::SteadyUnderline] {
        let mut terminal = one_cell_terminal_with_cursor_style(style);
        let snap = FrameSnapshot::from_terminal(&mut terminal);

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

        assert!(
            instances
                .iter()
                .all(|i| i.flags != CellInstance::FLAG_CURSOR),
            "{style:?}: must not emit an opaque block-fill background quad"
        );

        let glyph_instance = instances
            .iter()
            .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
            .expect("cell glyph must still be drawn");
        assert_eq!(
            glyph_instance.color,
            [240, 10, 20, 255],
            "{style:?}: glyph keeps the cell's own foreground, not inverted to the background"
        );

        let cursor_decorations: Vec<_> = instances
            .iter()
            .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
            .collect();
        assert_eq!(
            cursor_decorations.len(),
            1,
            "{style:?}: exactly one cursor-shape decoration rect"
        );
        assert_eq!(cursor_decorations[0].grid_pos, [0, 0]);
    }
}

#[test]
fn unfocused_pane_draws_a_hollow_outline_not_a_block_fill() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::SteadyBlock);
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.focused = false;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    assert!(
        instances
            .iter()
            .all(|i| i.flags != CellInstance::FLAG_CURSOR),
        "an unfocused pane must not emit a block-fill background quad, even for a block style"
    );
    let outline_rects: Vec<_> = instances
        .iter()
        .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
        .collect();
    assert_eq!(
        outline_rects.len(),
        4,
        "an unfocused pane's cursor is a 4-sided hollow outline"
    );

    let glyph_instance = instances
        .iter()
        .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
        .expect("cell glyph must still be drawn");
    assert_eq!(
        glyph_instance.color,
        [240, 10, 20, 255],
        "glyph keeps its own foreground when unfocused"
    );
}

#[test]
fn focused_blinking_cursor_in_off_phase_emits_no_cursor_instances() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::BlinkingBlock);
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.cursor_blink_visible = false;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    assert!(
        instances
            .iter()
            .all(|i| i.flags & CellInstance::FLAG_CURSOR == 0),
        "a blinking cursor's off phase draws no block quad, decoration, or cursor-flagged glyph"
    );
    let glyph_instance = instances
        .iter()
        .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
        .expect("cell glyph must still be drawn");
    assert_eq!(
        glyph_instance.color,
        [240, 10, 20, 255],
        "off-phase glyph keeps its own foreground, unaffected by the hidden cursor"
    );
}

#[test]
fn cursor_visual_resolves_per_style_focus_and_blink_phase() {
    let mut snap = baseline_snapshot(['a', 'b', 'c']);
    snap.cursor.style = CursorStyle::BlinkingBlock;

    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Block,
        "focused + blink-visible block style fills the cell"
    );

    snap.cursor_blink_visible = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::None,
        "a focused blinking cursor's off phase draws nothing"
    );

    snap.cursor.style = CursorStyle::SteadyBar;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Bar,
        "a steady style ignores blink phase entirely"
    );

    snap.cursor.style = CursorStyle::SteadyUnderline;
    assert_eq!(cursor_visual_for(&snap), CursorVisual::Underline);

    snap.focused = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Hollow,
        "an unfocused pane always shows the hollow outline, ignoring style and blink phase"
    );

    snap.cursor.visible = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::None,
        "a DECTCEM-hidden cursor never renders, focused or not"
    );
}

#[test]
fn cursor_bar_decoration_is_a_full_height_left_edge_rect() {
    let mut instances = Vec::new();
    push_cursor_decorations(
        &mut instances,
        2,
        5,
        CursorVisual::Bar,
        [9, 8, 7, 255],
        metrics(18.0),
    );

    assert_eq!(instances.len(), 1);
    let bar = instances[0];
    assert_eq!(bar.grid_pos, [2, 5]);
    assert_eq!(
        bar.bearing,
        [0, 0],
        "bar sits flush against the cell's left edge"
    );
    assert_eq!(
        bar.glyph_size,
        [1, 24],
        "bar width tracks decoration thickness, full cell height"
    );
    assert_eq!(bar.color, [9, 8, 7, 255]);
    assert_eq!(
        bar.flags,
        CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
    );
}

#[test]
fn cursor_underline_decoration_reuses_the_text_underline_geometry() {
    let mut instances = Vec::new();
    let m = metrics(18.0);
    push_cursor_decorations(
        &mut instances,
        0,
        0,
        CursorVisual::Underline,
        [1, 2, 3, 255],
        m,
    );

    assert_eq!(instances.len(), 1);
    let strip = instances[0];
    assert_eq!(
        strip.glyph_size,
        [10, 1],
        "underline spans the full cell width at decoration thickness"
    );
    assert_eq!(
        strip.bearing[1],
        underline_y(m, decoration_thickness(m), 0.0),
        "y matches the same baseline offset the UNDERLINE attribute decoration uses"
    );
    assert_eq!(
        strip.flags,
        CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
    );
}

#[test]
fn cursor_hollow_decoration_emits_four_edge_rects() {
    let mut instances = Vec::new();
    push_cursor_decorations(
        &mut instances,
        3,
        1,
        CursorVisual::Hollow,
        [4, 5, 6, 255],
        metrics(18.0),
    );

    assert_eq!(
        instances.len(),
        4,
        "hollow outline is exactly top/bottom/left/right"
    );
    assert!(instances.iter().all(|i| {
        i.grid_pos == [3, 1]
            && i.color == [4, 5, 6, 255]
            && i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR)
    }));

    assert_eq!(instances[0].bearing, [0, 0], "top edge");
    assert_eq!(instances[0].glyph_size, [10, 1]);
    assert_eq!(instances[1].bearing, [0, 23], "bottom edge");
    assert_eq!(instances[1].glyph_size, [10, 1]);
    assert_eq!(instances[2].bearing, [0, 0], "left edge");
    assert_eq!(instances[2].glyph_size, [1, 24]);
    assert_eq!(instances[3].bearing, [9, 0], "right edge");
    assert_eq!(instances[3].glyph_size, [1, 24]);
}

#[test]
fn decorations_emit_rect_instances_from_cell_attrs() {
    let mut instances = Vec::new();
    let metrics = Metrics {
        cell_w: 12.0,
        cell_h: 24.0,
        ascent: 18.0,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: -2.0,
        underline_thickness: 2.0,
    };

    push_cell_decorations(
        &mut instances,
        3,
        4,
        CellAttrs::DOUBLE_UNDERLINE | CellAttrs::STRIKETHROUGH | CellAttrs::OVERLINE,
        [1, 2, 3, 255],
        metrics,
    );

    assert_eq!(instances.len(), 4);
    assert!(
        instances
            .iter()
            .all(|instance| instance.flags == CellInstance::FLAG_DECORATION)
    );
    assert!(
        instances
            .iter()
            .all(|instance| instance.grid_pos == [3, 4] && instance.color == [1, 2, 3, 255])
    );
    assert_eq!(
        instances[0].bearing,
        [0, 0],
        "overline starts at the cell top"
    );
    assert_eq!(
        instances[2].glyph_size,
        [12, 2],
        "double underline keeps full-cell width and metric thickness"
    );
    assert!(
        instances[2].bearing[1] < instances[3].bearing[1],
        "double underline emits two vertically separated strokes"
    );
}

#[test]
fn focus_indicator_instance_uses_pixel_overlay_path_and_accent_color() {
    let instance = focus_indicator_instance(PaneRect::new(11, 13, 17, 2));

    assert_eq!(instance.grid_pos, [11, 13]);
    assert_eq!(instance.glyph_size, [17, 2]);
    assert_eq!(instance.color, FOCUS_INDICATOR_RGBA);
    assert_eq!(instance.flags, CellInstance::FLAG_DIVIDER);
}

#[test]
fn patterned_underlines_emit_segmented_rectangles() {
    let metrics = Metrics {
        cell_w: 9.0,
        cell_h: 20.0,
        ascent: 14.0,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: -1.0,
        underline_thickness: 1.0,
    };

    let mut dotted = Vec::new();
    push_cell_decorations(
        &mut dotted,
        0,
        0,
        CellAttrs::DOTTED_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
    );
    assert!(
        dotted.len() > 1,
        "dotted underline should be split into repeated dot rectangles"
    );
    assert!(dotted.iter().all(|instance| instance.glyph_size[0] == 1));

    let mut dashed = Vec::new();
    push_cell_decorations(
        &mut dashed,
        0,
        0,
        CellAttrs::DASHED_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
    );
    assert!(dashed.iter().any(|instance| instance.glyph_size[0] > 1));

    let mut curly = Vec::new();
    push_cell_decorations(
        &mut curly,
        0,
        0,
        CellAttrs::CURLY_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
    );
    assert!(
        curly
            .windows(2)
            .any(|pair| pair[0].bearing[1] != pair[1].bearing[1]),
        "curly underline should alternate segment vertical positions"
    );
}

#[test]
fn hover_link_registry_underlines_only_cells_carrying_that_link_id() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(3, 1));
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].hyperlink = Some(0);
    terminal.primary.grid[0].cells[1].ch = 'M';
    terminal.primary.grid[0].cells[1].hyperlink = Some(1); // a different link
    terminal.primary.grid[0].cells[2].ch = 'M'; // no link at all

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap.hover_link, None, "from_terminal defaults to no hover");

    let mut no_hover = Vec::new();
    rebuild_cell_instances(&mut no_hover, &snap, &mut font, &Theme::new(), false);
    assert!(
        no_hover
            .iter()
            .all(|i| i.flags != CellInstance::FLAG_DECORATION),
        "no hover target set: no hover underline should be emitted"
    );

    snap.hover_link = Some(HoverLink::Registry(0));
    let mut hovered = Vec::new();
    rebuild_cell_instances(&mut hovered, &snap, &mut font, &Theme::new(), false);
    let underlined: Vec<[u16; 2]> = hovered
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
        .map(|i| i.grid_pos)
        .collect();
    assert_eq!(
        underlined,
        vec![[0, 0]],
        "only the cell carrying the hovered registry id gets the hover underline, \
             not the cell with a different link id or the cell with no link"
    );
}

#[test]
fn hover_link_range_underlines_only_the_matching_run_on_its_row() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    for row in 0..2 {
        for x in 0..4 {
            terminal.primary.grid[row].cells[x].ch = 'M';
        }
    }

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.hover_link = Some(HoverLink::Range {
        y: 0,
        x_start: 1,
        x_end: 2,
    });

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);
    let mut underlined: Vec<[u16; 2]> = instances
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
        .map(|i| i.grid_pos)
        .collect();
    underlined.sort();
    assert_eq!(
        underlined,
        vec![[1, 0], [2, 0]],
        "only columns 1..=2 on row 0 are underlined; row 1 and the rest of row 0 are not"
    );
}

#[test]
fn search_prompt_display_text_keeps_the_tail_of_a_buffer_too_long_to_fit() {
    let search = SearchState::default();

    // cols=20, fixed chars ("Find: " + "▏" + " 0/0") = 11, so 9 chars of
    // buffer fit; the last 9 of "0123456789" is "123456789".
    let text = search_prompt_display_text("0123456789", &search, 20);
    assert_eq!(text, "Find: 123456789\u{258F} 0/0");
    assert_eq!(text.chars().count(), 20);

    let short = search_prompt_display_text("hi", &search, 20);
    assert_eq!(
        short, "Find: hi\u{258F} 0/0",
        "a short buffer is shown in full"
    );
}

#[test]
fn search_prompt_display_text_reports_no_matches_for_non_empty_query() {
    let mut search = SearchState::default();
    search.set_query("needle".to_string(), Vec::new());

    let text = search_prompt_display_text("needle", &search, 30);

    assert_eq!(text, "Find: needle\u{258F} no matches");
}

#[test]
fn search_prompt_overlay_emits_top_right_bg_and_glyph_instances_and_tracks_the_buffer() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(20, 2));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.search_prompt, None,
        "from_terminal defaults to no prompt"
    );

    let mut without_prompt = Vec::new();
    rebuild_cell_instances(&mut without_prompt, &snap, &mut font, &theme, false);
    let row0_bg_before = without_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();

    snap.search_prompt = Some("M".to_string());
    let mut with_prompt = Vec::new();
    rebuild_cell_instances(&mut with_prompt, &snap, &mut font, &theme, false);

    let prompt_bg: Vec<_> = with_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .collect();
    assert!(
        prompt_bg.len() > row0_bg_before,
        "opening the prompt must add background quads to row 0"
    );
    assert!(
        prompt_bg
            .iter()
            .all(|i| i.grid_pos[0] >= snap.cols - prompt_bg.len() as u16),
        "the prompt is right-aligned to the pane's rightmost columns: {prompt_bg:?}"
    );

    let glyphs: Vec<_> = with_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .collect();
    assert!(
        !glyphs.is_empty(),
        "the prompt text must emit glyph instances"
    );

    // A buffer edit must always repaint — this overlay is deliberately
    // NOT part of the per-row cache, so a longer buffer widens it on
    // the very next rebuild.
    snap.search_prompt = Some("Mxyz".to_string());
    let mut with_longer_prompt = Vec::new();
    rebuild_cell_instances(&mut with_longer_prompt, &snap, &mut font, &theme, false);
    let longer_bg = with_longer_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();
    assert!(
        longer_bg > prompt_bg.len(),
        "a longer buffer must widen the overlay ({longer_bg} vs {})",
        prompt_bg.len()
    );

    // Closing the prompt (search_prompt back to None) must remove the
    // overlay instances on the very next rebuild too.
    snap.search_prompt = None;
    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let closed_bg = closed
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();
    assert_eq!(
        closed_bg, row0_bg_before,
        "closing the prompt removes the overlay"
    );
}

#[test]
fn preedit_overlay_emits_underlined_run_at_cursor_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(20, 4));
    terminal.primary.cursor.x = 3;
    terminal.primary.cursor.y = 1;
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap.preedit, None, "from_terminal defaults to no preedit");

    // Baseline (no composition): count the decoration rects already on the
    // cursor row so the underline assertion below measures only the delta.
    let mut without = Vec::new();
    rebuild_cell_instances(&mut without, &snap, &mut font, &theme, false);
    let deco_row1_before = without
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags == CellInstance::FLAG_DECORATION)
        .count();

    snap.preedit = Some(crate::Preedit {
        text: "Mixed".to_string(),
        cursor_byte_range: None,
    });
    let mut with = Vec::new();
    rebuild_cell_instances(&mut with, &snap, &mut font, &theme, false);

    // Glyphs land on the cursor row, at or right of the cursor column.
    let glyphs: Vec<_> = with
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .collect();
    assert!(
        !glyphs.is_empty(),
        "preedit text must emit glyph instances on the cursor row"
    );
    assert!(
        glyphs.iter().all(|i| i.grid_pos[0] >= 3),
        "the preedit run starts at the cursor column: {glyphs:?}"
    );

    // The whole run is underlined: one decoration rect per drawn column on the
    // cursor row, on top of the baseline count.
    let deco_row1_after = with
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags == CellInstance::FLAG_DECORATION)
        .count();
    assert!(
        deco_row1_after >= deco_row1_before + 5,
        "the 5-column preedit run must add an underline rect per column \
         ({deco_row1_after} vs {deco_row1_before})"
    );

    // Closing the composition removes the overlay on the very next rebuild.
    snap.preedit = None;
    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let glyphs_closed = closed
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .count();
    assert_eq!(
        glyphs_closed, 0,
        "closing the composition removes the preedit glyphs"
    );
}

#[test]
fn preedit_overlay_clamps_the_run_to_the_pane_right_edge() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    // Cursor two columns from the right edge: only two composing cells fit.
    let mut terminal = Terminal::new(GridSize::new(6, 2));
    terminal.primary.cursor.x = 4;
    terminal.primary.cursor.y = 0;
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.preedit = Some(crate::Preedit {
        text: "MMMMMM".to_string(),
        cursor_byte_range: None,
    });
    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

    // No preedit background/underline column may spill past the last column.
    assert!(
        instances
            .iter()
            .filter(|i| i.grid_pos[1] == 0)
            .all(|i| i.grid_pos[0] < snap.cols),
        "the clamped preedit run must not overflow the pane's right edge"
    );
    let deco_cols = instances
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == CellInstance::FLAG_DECORATION)
        .count();
    assert_eq!(
        deco_cols, 2,
        "only the two columns between the cursor and the right edge are drawn"
    );
}

#[test]
fn palette_scroll_window_keeps_the_selection_visible() {
    // Fits entirely: whole list, no scroll.
    assert_eq!(palette_scroll_window(3, 2, 5), (0, 3));
    // Taller than capacity, selection near the top: window pinned to 0.
    assert_eq!(palette_scroll_window(10, 1, 4), (0, 4));
    // Selection past the first window: scroll just far enough.
    assert_eq!(palette_scroll_window(10, 5, 4), (2, 4));
    // Selection at the end: window pinned to the bottom.
    assert_eq!(palette_scroll_window(10, 9, 4), (6, 4));
    // Degenerate inputs never panic.
    assert_eq!(palette_scroll_window(0, 0, 4), (0, 0));
    assert_eq!(palette_scroll_window(5, 0, 0), (0, 0));
}

#[test]
fn command_palette_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(30, 8));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.command_palette, None,
        "from_terminal defaults to no palette"
    );

    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let bg_before = closed.iter().filter(|i| i.flags == 0).count();

    snap.command_palette = Some(crate::CommandPaletteSnapshot {
        query: "sp".to_string(),
        rows: vec![
            ("Split Right".to_string(), Some("cmd+d".to_string())),
            ("Split Down".to_string(), Some("cmd+shift+d".to_string())),
            ("Toggle Split Zoom".to_string(), None),
        ],
        selected: 1,
    });
    let mut with_palette = Vec::new();
    rebuild_cell_instances(&mut with_palette, &snap, &mut font, &theme, false);

    let bg_with = with_palette.iter().filter(|i| i.flags == 0).count();
    assert!(
        bg_with > bg_before,
        "opening the palette must add background quads"
    );
    // The block spans 4 grid rows (query + 3 entries); at least those
    // rows must carry palette instances.
    let rows_touched: std::collections::BTreeSet<u16> = with_palette
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 4,
        "query row plus three entry rows must all draw: {rows_touched:?}"
    );
    assert!(
        with_palette
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the palette text must emit glyph instances"
    );

    snap.command_palette = None;
    let mut reclosed = Vec::new();
    rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
    assert_eq!(
        reclosed.iter().filter(|i| i.flags == 0).count(),
        bg_before,
        "closing the palette removes its overlay instances"
    );
}

#[test]
fn command_palette_overlay_shows_empty_state_for_zero_results() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(30, 8));
    let theme = Theme::new();
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.command_palette = Some(crate::CommandPaletteSnapshot {
        query: "zzzzzz".to_string(),
        rows: Vec::new(),
        selected: 0,
    });

    let mut with_empty_palette = Vec::new();
    rebuild_cell_instances(&mut with_empty_palette, &snap, &mut font, &theme, false);

    let rows_touched: std::collections::BTreeSet<u16> = with_empty_palette
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 3,
        "query row, spacer, and empty-state row must all draw: {rows_touched:?}"
    );
    assert!(
        with_empty_palette
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the empty-state text must emit glyph instances"
    );
}

#[test]
fn confirm_dialog_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(40, 10));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.confirm_dialog, None,
        "from_terminal defaults to no dialog"
    );

    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let bg_before = closed.iter().filter(|i| i.flags == 0).count();

    snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
        message: "Paste 3 line(s) of text?".to_string(),
        hint: "Enter: confirm    Esc: cancel".to_string(),
    });
    let mut with_dialog = Vec::new();
    rebuild_cell_instances(&mut with_dialog, &snap, &mut font, &theme, false);

    assert!(
        with_dialog.iter().filter(|i| i.flags == 0).count() > bg_before,
        "opening the dialog must add background quads"
    );
    // A message row and a hint row.
    let rows_touched: std::collections::BTreeSet<u16> = with_dialog
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 2,
        "message and hint rows must both draw: {rows_touched:?}"
    );
    assert!(
        with_dialog
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the dialog text must emit glyph instances"
    );

    snap.confirm_dialog = None;
    let mut reclosed = Vec::new();
    rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
    assert_eq!(
        reclosed.iter().filter(|i| i.flags == 0).count(),
        bg_before,
        "closing the dialog removes its overlay instances"
    );
}

#[test]
fn overlay_block_emits_border_on_all_four_edges() {
    // A 4-wide x 3-tall block anchored at (5, 3): the border must touch
    // every perimeter cell (top row, bottom row, left column, right
    // column) and emit only decoration-pass rects.
    let m = metrics(18.0); // cell_w = 10, cell_h = 24
    let color = [10, 20, 30, 255];
    let mut inst = Vec::new();
    append_overlay_border(&mut inst, 5, 3, 4, 3, color, m);

    assert!(
        inst.iter()
            .all(|i| i.flags & CellInstance::FLAG_DECORATION != 0),
        "border rects are decoration-pass quads"
    );

    let at = |x: u16, y: u16| inst.iter().any(|i| i.grid_pos == [x, y]);
    // Top (y=3) and bottom (y=5) edges span columns 5..=8.
    for x in 5..=8 {
        assert!(at(x, 3), "top edge missing at col {x}");
        assert!(at(x, 5), "bottom edge missing at col {x}");
    }
    // Left (x=5) and right (x=8) edges span rows 3..=5.
    for y in 3..=5 {
        assert!(at(5, y), "left edge missing at row {y}");
        assert!(at(8, y), "right edge missing at row {y}");
    }
    // The right edge is inset to the cell's right (bearing.x > 0), the
    // bottom edge to the cell's bottom (bearing.y > 0).
    assert!(
        inst.iter()
            .any(|i| i.grid_pos == [8, 3] && i.bearing[0] > 0),
        "right edge sits at the cell's right pixel"
    );
    assert!(
        inst.iter()
            .any(|i| i.grid_pos == [5, 5] && i.bearing[1] > 0),
        "bottom edge sits at the cell's bottom pixel"
    );
}

#[test]
fn confirm_dialog_padding_rows_collapse_on_small_grids() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };
    let theme = Theme::new();

    // A tall grid gets a blank pad row above and below the two text rows
    // (4 distinct overlay bg rows); a short grid falls back to the compact
    // message + hint form (2 rows).
    let tall = confirm_dialog_bg_rows(&mut font, &theme, 40, 10);
    assert_eq!(tall, 4, "tall grid pads the dialog to 4 rows");
    let short = confirm_dialog_bg_rows(&mut font, &theme, 40, 4);
    assert_eq!(short, 2, "short grid collapses to the compact 2-row form");
}

/// Count the distinct grid rows carrying confirm-dialog overlay background
/// quads (`flags == 0`) for a `cols` x `rows` grid. The default block
/// cursor paints a `FLAG_CURSOR` quad, not a plain bg quad, so it is not
/// counted here.
fn confirm_dialog_bg_rows(font: &mut FontGrid, theme: &Theme, cols: u16, rows: u16) -> usize {
    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
        message: "Paste 3 line(s)?".to_string(),
        hint: "Enter: confirm    Esc: cancel".to_string(),
    });
    let mut inst = Vec::new();
    rebuild_cell_instances(&mut inst, &snap, font, theme, false);
    inst.iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect::<std::collections::BTreeSet<u16>>()
        .len()
}

#[test]
fn srgb_surface_output_converts_theme_colors_to_linear() {
    let srgb = [30.0 / 255.0, 30.0 / 255.0, 30.0 / 255.0, 1.0];

    assert_eq!(
        to_u8_color(surface_output_rgba(srgb, false)),
        [30, 30, 30, 255]
    );
    assert_eq!(to_u8_color(surface_output_rgba(srgb, true)), [3, 3, 3, 255]);
}

#[test]
fn srgb_surface_output_preserves_extremes_and_alpha() {
    assert_eq!(
        surface_output_rgba([0.0, 1.0, 0.5, 0.5], true),
        [0.0, 1.0, srgb_to_linear(0.5), 0.5]
    );
}

// ---- WP2: shaping + ligatures + shape cache -----------------------

/// AC-WP2-01 [noa-render half of the FM-04 mitigation]: a single
/// shaped glyph covering 2 source cells (simulating a ligature — real
/// ligature-font availability isn't guaranteed in every environment, so
/// this constructs the shaped-glyph list directly instead of depending
/// on one) must emit exactly ONE glyph instance, anchored at the
/// cluster-start cell; the covered (non-start) cell must get none.
/// Proves the consumer iterates the shaped-glyph list rather than
/// asking each source cell "should I draw" (no per-cell suppression
/// flag to forget).
#[test]
fn ligature_shaped_glyph_emits_one_instance_and_covered_cell_emits_none() {
    let Some(mut font) = skip_font() else { return };
    let style = StyleKey::default();

    let real = font
        .shape_run(&[ShapeCell {
            ch: 'M',
            combining: Vec::new(),
            style,
        }])
        .into_iter()
        .next()
        .expect("shaping 'M' must yield a glyph");

    let run = ShapeRun {
        start_col: 5,
        cells: vec![
            ShapeCell {
                ch: '!',
                combining: Vec::new(),
                style,
            },
            ShapeCell {
                ch: '=',
                combining: Vec::new(),
                style,
            },
        ],
        cell_render: vec![
            CellRenderInfo {
                color: [10, 20, 30, 255],
                cursor: false,
            },
            CellRenderInfo {
                color: [40, 50, 60, 255],
                cursor: false,
            },
        ],
    };
    // Exactly one shaped glyph for a 2-cell run: the ligature case.
    let shaped = vec![ShapedGlyph {
        glyph_id: real.glyph_id,
        face_id: real.face_id,
        x_advance: real.x_advance,
        x_offset: 0,
        y_offset: 0,
        cluster: 0,
    }];

    let mut glyph_instances = Vec::new();
    let metrics = font.metrics();
    emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 7, metrics);

    assert_eq!(
        glyph_instances.len(),
        1,
        "a ligature (one shaped glyph for 2 source cells) must emit exactly one instance"
    );
    assert_eq!(
        glyph_instances[0].grid_pos,
        [5, 7],
        "the ligature instance must be anchored at start_col + cluster (the cluster-start cell)"
    );
    assert_eq!(
        glyph_instances[0].color,
        [10, 20, 30, 255],
        "instance color must come from the cluster-start cell's render context"
    );
}

/// AC-WP2-04 [noa-render half]: multiple shaped glyphs sharing one
/// cluster (a base glyph plus an attached mark glyph) must each be
/// emitted, anchored at the SAME cell, positioned by their OWN shaped
/// `x_offset`/`y_offset` — not merged into one draw and not positioned
/// by an independent per-char pen bearing.
#[test]
fn combining_mark_glyph_is_positioned_by_shaped_offset_not_pen_bearing() {
    let Some(mut font) = skip_font() else { return };
    let style = StyleKey::default();

    let base = font
        .shape_run(&[ShapeCell {
            ch: 'M',
            combining: Vec::new(),
            style,
        }])
        .into_iter()
        .next()
        .expect("shaping 'M' must yield a glyph");

    let run = ShapeRun {
        start_col: 2,
        cells: vec![ShapeCell {
            ch: 'M',
            combining: vec!['\u{301}'],
            style,
        }],
        cell_render: vec![CellRenderInfo {
            color: [1, 2, 3, 255],
            cursor: false,
        }],
    };
    // Two glyphs sharing cluster 0: the base, and a stand-in "mark"
    // glyph (reusing a real, rasterizable glyph id so it isn't
    // filtered as empty) offset from it.
    let shaped = vec![
        ShapedGlyph {
            glyph_id: base.glyph_id,
            face_id: base.face_id,
            x_advance: base.x_advance,
            x_offset: 0,
            y_offset: 0,
            cluster: 0,
        },
        ShapedGlyph {
            glyph_id: base.glyph_id,
            face_id: base.face_id,
            x_advance: 0,
            x_offset: 3,
            y_offset: 5,
            cluster: 0,
        },
    ];

    let mut glyph_instances = Vec::new();
    let metrics = font.metrics();
    emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 9, metrics);

    assert_eq!(
        glyph_instances.len(),
        2,
        "both the base and the attached mark glyph must be emitted (attached cluster)"
    );
    assert!(
        glyph_instances.iter().all(|inst| inst.grid_pos == [2, 9]),
        "both glyphs must share the base cell's anchor position"
    );
    let base_bearing = glyph_instances[0].bearing;
    let mark_bearing = glyph_instances[1].bearing;
    assert_eq!(
        mark_bearing[0],
        base_bearing[0] + 3,
        "the mark's x position must come from its own shaped x_offset"
    );
    assert_eq!(
        mark_bearing[1],
        base_bearing[1] - 5,
        "the mark's y position must come from its own shaped y_offset (HarfBuzz y-up -> cell y-down)"
    );
}

/// AC-WP2-05 (FM-08 gap-closer): unlike a hand-built `ShapeCell` slice
/// passed directly to `shape_run`, this exercises the REAL
/// segmentation -> `shape_run` path (`rebuild_cell_instances`) across 3
/// consecutive render passes over unchanged terminal content, and
/// asserts the shape cache keeps hitting from the 2nd pass onward — not
/// just once.
#[test]
fn repeated_render_passes_hit_the_shape_cache_via_the_real_segmentation_path() {
    let Some(mut font) = skip_font() else { return };

    let mut terminal = Terminal::new(GridSize::new(12, 1));
    for (i, ch) in "hello!!==".chars().enumerate() {
        terminal.primary.grid[0].cells[i].ch = ch;
    }
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    let theme = Theme::new();
    let mut instances = Vec::new();

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_1 = font.shape_cache_hits();

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_2 = font.shape_cache_hits();
    assert!(
        hits_after_pass_2 > hits_after_pass_1,
        "an unchanged frame's 2nd render pass must hit the shape cache \
             (pass1={hits_after_pass_1}, pass2={hits_after_pass_2})"
    );

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_3 = font.shape_cache_hits();
    assert!(
        hits_after_pass_3 > hits_after_pass_2,
        "a 3rd unchanged render pass must ALSO hit the cache, not just the 2nd \
             (pass2={hits_after_pass_2}, pass3={hits_after_pass_3})"
    );
}

// ── WP4: dirty-row diffing ────────────────────────────────────────

/// A 3-row, 1-col snapshot with distinct content per row and every
/// pane-wide field at a neutral baseline. `row_dirty` is all-false —
/// on a second call through the SAME pane cache this represents "no
/// row-level cell mutation happened," isolating whichever single
/// pane-wide field a FM-11 sub-case varies.
fn baseline_snapshot(chars: [char; 3]) -> FrameSnapshot {
    let rows = chars
        .into_iter()
        .map(|ch| Row {
            cells: vec![Cell {
                ch,
                ..Cell::default()
            }],
            wrapped: false,
            dirty: false,
        })
        .collect();
    FrameSnapshot {
        scroll_shift: 0,
        rows,
        row_dirty: vec![false, false, false],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols: 1,
        rows_n: 3,
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

#[test]
fn rebuild_panes_reports_zero_rows_rebuilt_when_nothing_changed() {
    // AC-WP4-02 (REQ-PERF-2): a frame in which no row changed since the
    // last rebuild produces a rows_rebuilt count of exactly 0.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping rows_rebuilt zero-count test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    terminal.primary.grid[0].cells[0].ch = 'A';
    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.rows_rebuilt_last_frame() > 0,
        "the first frame through a fresh cache must rebuild at least one row"
    );

    // `from_terminal` already cleared the grid's dirty bits when snap1
    // was taken; the terminal has not been mutated since, so this
    // second snapshot reports every row clean.
    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        0,
        "an unchanged second frame must rebuild zero rows"
    );
}

#[test]
fn atlas_eviction_epoch_forces_full_row_cache_rebuild() {
    // Regression: row-cache glyph instances store concrete atlas
    // coordinates. When FontGrid evicts a glyph slot, those coordinates
    // can later be reused by another glyph, so an otherwise-clean frame
    // must not reuse the old row instances.
    let mut font = match FontGrid::new_with_capped_atlas_for_tests(14.0, FontConfig::default(), 48)
    {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let theme = Theme::new();
    let mut cache = PaneRenderCache::empty();
    let snap = baseline_snapshot(['A', 'B', 'C']);
    let mut instances = Vec::new();

    let first = rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
    assert_eq!(
        first.rows_rebuilt, 3,
        "fresh pane cache should build every visible row"
    );
    instances.clear();

    let before_eviction = font.atlas_eviction_generation();
    for ch in ('!'..='~').chain('\u{3041}'..='\u{3096}') {
        font.get_or_raster(ch);
        if font.atlas_eviction_generation() > before_eviction {
            break;
        }
    }
    assert!(
        font.atlas_eviction_generation() > before_eviction,
        "capped atlas must evict after flooding distinct glyphs"
    );

    let second = rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
    assert!(
        second.rows_rebuilt >= 3,
        "atlas eviction must force a full row-cache rebuild even when row_dirty is false"
    );
}

#[test]
fn per_row_patch_output_matches_a_full_rebuild_ac_wp4_03() {
    // AC-WP4-03 (REQ-PERF-3): identical terminal state rendered once via
    // a full rebuild and once via the per-row patch path must produce
    // an IDENTICAL instance list.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping AC-WP4-03 identical-output test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let mut terminal = Terminal::new(GridSize::new(4, 3));
    terminal.primary.grid[0].cells[0].ch = 'A';
    terminal.primary.grid[1].cells[0].ch = 'B';
    terminal.primary.grid[2].cells[0].ch = 'C';

    // First frame: fresh cache -> full rebuild.
    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );

    // Mutate ONE row only, so the second frame is a genuine per-row
    // patch: rows 0 and 2 are reused untouched from the cache, only
    // row 1 regenerates. Direct field mutation bypasses the real
    // cell-mutating paths that set `Row::dirty` (e.g. `Screen::print`),
    // so mark it explicitly — mirrors how `noa-render/tests/pipeline.rs`
    // constructs `Row { dirty: true, .. }` literals directly.
    terminal.primary.grid[1].cells[0].ch = 'X';
    terminal.primary.grid[1].dirty = true;
    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        1,
        "only the mutated row should have been rebuilt on the second frame"
    );
    let patched = renderer.instances_for_test().to_vec();

    // Reference: an unconditional full rebuild of the SAME
    // (post-mutation) state via the always-full free function.
    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "the per-row-patched instance list must be byte-identical to a full \
             rebuild of the same state (bg-then-glyph-then-decoration GLOBAL order, FM-12)"
    );
}

#[test]
fn scroll_translation_output_matches_a_full_rebuild() {
    // P1 scroll fast path: a scrollback-recording scroll must translate the
    // cached row segments instead of rebuilding every row, and the result
    // must be byte-identical to an unconditional full rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping scroll-translation test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let mut terminal = Terminal::new(GridSize::new(4, 3));
    terminal.primary.grid[0].cells[0].ch = 'A';
    terminal.primary.grid[1].cells[0].ch = 'B';
    terminal.primary.grid[2].cells[0].ch = 'C';

    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );

    // A full-viewport scroll on the primary screen records scrollback, so
    // it must surface as a translation, not as three dirty rows.
    terminal.primary.scroll_up_region(1);
    terminal.primary.grid[2].cells[0].ch = 'D';
    terminal.primary.grid[2].dirty = true;

    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap2.scroll_shift, 1, "recorded scroll reports its shift");
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.rows_rebuilt_last_frame() < 3,
        "translated scroll must not rebuild every row (rebuilt {})",
        renderer.rows_rebuilt_last_frame()
    );
    let patched = renderer.instances_for_test().to_vec();

    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "scroll-translated instance list must be byte-identical to a full rebuild"
    );
}

#[test]
fn pane_wide_invalidation_triggers_are_covered_fm11() {
    // FM-11: each of the 7 pane-wide triggers bundled into
    // `FrameInvalidationKey` must force EVERY row in the pane dirty when
    // it differs from the previous frame, even though `row_dirty` says
    // no cell changed. Cursor movement (the narrower 8th case) instead
    // dirties exactly the two affected rows.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping FM-11 trigger table test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let rect = PaneRect::new(0, 0, 64, 64);

    // Each sub-case gets its own PaneId so it starts from a fresh cache
    // without needing a fresh Renderer (cheaper: one GPU device for the
    // whole table).
    let mut rebuild_twice = |pane_id: u64,
                             snap_a: &FrameSnapshot,
                             theme_a: &Theme,
                             snap_b: &FrameSnapshot,
                             theme_b: &Theme| {
        let pane = PaneId::new(pane_id);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: snap_a,
            }],
            &mut font,
            theme_a,
        );
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: snap_b,
            }],
            &mut font,
            theme_b,
        );
        renderer.rows_rebuilt_last_frame()
    };

    // 1. abs_row_base (viewport scroll offset, session-absolute).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.abs_row_base = 1;
        let rebuilt = rebuild_twice(101, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "abs_row_base change must force a full pane rebuild"
        );
    }

    // 2a. cols (resize).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.cols = 2;
        let rebuilt = rebuild_twice(102, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(rebuilt, 3, "cols change must force a full pane rebuild");
    }

    // 2b. rows_n (resize).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.rows_n = 4;
        let rebuilt = rebuild_twice(103, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(rebuilt, 3, "rows_n change must force a full pane rebuild");
    }

    // 3. colors (terminal palette override).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        let mut colors = TerminalColors::default();
        colors.set_default_fg(Rgb::new(9, 9, 9));
        snap_b.colors = colors;
        let rebuilt = rebuild_twice(104, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a terminal color override change must force a full pane rebuild"
        );
    }

    // 4. active Theme identity.
    {
        let snap = baseline_snapshot(['A', 'B', 'C']);
        let mut theme_b = Theme::new();
        theme_b.default_fg = Rgb::new(5, 6, 7);
        let rebuilt = rebuild_twice(105, &snap, &theme, &snap, &theme_b);
        assert_eq!(rebuilt, 3, "a theme swap must force a full pane rebuild");
    }

    // 5. selection state.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.selection = Some(Selection::new(
            SelectionPoint::new(0, 0),
            SelectionPoint::new(0, 0),
        ));
        let rebuilt = rebuild_twice(106, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a selection change must force a full pane rebuild"
        );
    }

    // 6. search state (active-match / search-match spans).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        let mut search = SearchState::default();
        search.set_query(
            "A".to_string(),
            vec![SearchMatch {
                start: SelectionPoint::new(0, 0),
                end: SelectionPoint::new(0, 0),
            }],
        );
        snap_b.search = search;
        let rebuilt = rebuild_twice(107, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a search-state change must force a full pane rebuild"
        );
    }

    // 7. hover_link (Cmd+hover underline target). Hover changes carry no
    // terminal damage at all (no cell/pty mutation), so this trigger is
    // what makes the underline actually repaint.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.hover_link = Some(HoverLink::Registry(0));
        let rebuilt = rebuild_twice(109, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a hover_link change must force a full pane rebuild"
        );
    }

    // 8. cursor movement — the narrower case: dirties exactly the two
    // affected rows, NOT a full-pane invalidation.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.cursor.y = 2;
        let rebuilt = rebuild_twice(108, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 2,
            "cursor movement must dirty exactly the two affected rows, not the whole pane"
        );
    }
}

#[test]
fn abs_row_base_change_forces_rebuild_even_when_row_base_collides() {
    // Regression: the invalidation key must ride the session-absolute
    // `abs_row_base`, not the storage-index `row_base`. A scroll that evicts
    // and pushes an equal number of rows reproduces the same `row_base`
    // while `abs_row_base` advances; keying on `row_base` would cache-hit and
    // paint stale history rows. Same row_base + different abs_row_base must
    // still force a full pane rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping abs_row_base collision test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let rect = PaneRect::new(0, 0, 64, 64);
    let pane = PaneId::new(201);

    let snap_a = baseline_snapshot(['A', 'B', 'C']);
    let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
    // The bug scenario: identical storage-index row_base, advanced absolute.
    assert_eq!(snap_a.row_base, snap_b.row_base);
    snap_b.abs_row_base = snap_a.abs_row_base + 3;

    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap_a,
        }],
        &mut font,
        &theme,
    );
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap_b,
        }],
        &mut font,
        &theme,
    );

    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        3,
        "abs_row_base change must force a full pane rebuild despite an unchanged row_base"
    );
}

#[test]
fn active_screen_switch_forces_rebuild_even_when_rows_are_clean() {
    // Regression: switching from alt back to primary can expose a screen
    // whose rows did not mutate while it was hidden. If the row cache key
    // ignores the active screen identity, the clean primary frame can reuse
    // alt-screen glyph instances.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping active-screen switch test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 96, h: 96 });

    let theme = Theme::new();
    let pane = PaneId::new(301);
    let rect = PaneRect::new(0, 0, 96, 96);
    let mut terminal = Terminal::new(GridSize::new(3, 3));
    terminal.primary.grid[0].cells[0].ch = 'P';
    terminal.primary.grid[1].cells[0].ch = 'R';
    terminal.primary.grid[2].cells[0].ch = 'I';

    let primary = FrameSnapshot::from_terminal(&mut terminal);
    assert!(!primary.active_is_alt);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &primary,
        }],
        &mut font,
        &theme,
    );

    let mut stream = Stream::new();
    stream.feed(b"\x1b[?1049hALT", &mut terminal);
    let alt = FrameSnapshot::from_terminal(&mut terminal);
    assert!(alt.active_is_alt);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &alt,
        }],
        &mut font,
        &theme,
    );

    stream.feed(b"\x1b[?1049l", &mut terminal);
    let primary_again = FrameSnapshot::from_terminal(&mut terminal);
    assert!(!primary_again.active_is_alt);
    assert!(
        primary_again.row_dirty.iter().all(|dirty| !dirty),
        "primary rows were not mutated while hidden, so only screen identity can invalidate"
    );
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &primary_again,
        }],
        &mut font,
        &theme,
    );

    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        3,
        "alt -> primary switch must rebuild every row even when row_dirty is clean"
    );
}

#[test]
fn overlay_boundary_stays_correct_after_a_per_row_patched_rebuild_fm16() {
    // FM-16: the cell/overlay instance boundary (`cell_instance_len`)
    // must still be computed correctly — and overlay instances must
    // still land at the right offset — after a rebuild that only
    // per-row-patched some rows instead of doing a full rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping FM-16 overlay boundary test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 128, h: 64 });

    let theme = Theme::new();
    let pane_a = PaneId::new(1);
    let pane_b = PaneId::new(2);
    let rect_a = PaneRect::new(0, 0, 64, 64);
    let rect_b = PaneRect::new(65, 0, 63, 64);

    let mut term_a = Terminal::new(GridSize::new(4, 2));
    term_a.primary.grid[0].cells[0].ch = 'A';
    let mut term_b = Terminal::new(GridSize::new(4, 2));
    term_b.primary.grid[0].cells[0].ch = 'Z';

    let snap_a1 = FrameSnapshot::from_terminal(&mut term_a);
    let snap_b1 = FrameSnapshot::from_terminal(&mut term_b);
    renderer.rebuild_panes(
        &[
            PaneFrame {
                pane: pane_a,
                rect: rect_a,
                snapshot: &snap_a1,
            },
            PaneFrame {
                pane: pane_b,
                rect: rect_b,
                snapshot: &snap_b1,
            },
        ],
        &mut font,
        &theme,
    ); // full first frame for both panes

    // Mutate one row in pane A only -> the next rebuild is a genuine
    // per-row patch (pane B rebuilds zero rows; pane A rebuilds one).
    // Direct field mutation bypasses `Screen::print`'s `dirty = true`,
    // so mark it explicitly (see the AC-WP4-03 test above for detail).
    term_a.primary.grid[1].cells[0].ch = 'B';
    term_a.primary.grid[1].dirty = true;
    let snap_a2 = FrameSnapshot::from_terminal(&mut term_a);
    let snap_b2 = FrameSnapshot::from_terminal(&mut term_b);
    renderer.rebuild_panes(
        &[
            PaneFrame {
                pane: pane_a,
                rect: rect_a,
                snapshot: &snap_a2,
            },
            PaneFrame {
                pane: pane_b,
                rect: rect_b,
                snapshot: &snap_b2,
            },
        ],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        1,
        "only pane A's single mutated row should rebuild"
    );

    let layout = [(pane_a, rect_a), (pane_b, rect_b)];
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-fm16-test-target"),
        size: wgpu::Extent3d {
            width: 128,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let cell_instance_len_before = renderer.cell_instance_len_for_test();
    assert_eq!(
        cell_instance_len_before,
        renderer.instances_for_test().len(),
        "cell_instance_len must equal the instance list length right after rebuild_panes \
             (no overlay appended yet)"
    );

    renderer.draw_panes(&device, &queue, &view, &layout, Some(pane_a), None);

    let all_instances = renderer.instances_for_test();
    assert!(
        all_instances.len() > cell_instance_len_before,
        "draw_panes over two panes with a focused pane must append at least one \
             overlay (divider/focus) instance past the cell-instance boundary"
    );
    for inst in &all_instances[cell_instance_len_before..] {
        assert_eq!(
            inst.flags,
            CellInstance::FLAG_DIVIDER,
            "every instance appended past cell_instance_len must be an overlay \
                 (divider/focus) quad, not leftover or corrupted cell data"
        );
    }
}
