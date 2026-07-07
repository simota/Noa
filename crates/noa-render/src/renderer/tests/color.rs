use super::*;

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
