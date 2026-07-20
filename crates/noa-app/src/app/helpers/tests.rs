use super::*;
use noa_core::DEFAULT_GRID_PADDING;

#[cfg(target_os = "macos")]
#[test]
fn macos_option_as_alt_maps_to_winit_modes() {
    assert_eq!(
        macos_option_as_alt(noa_config::MacosOptionAsAlt::None),
        OptionAsAlt::None
    );
    assert_eq!(
        macos_option_as_alt(noa_config::MacosOptionAsAlt::Left),
        OptionAsAlt::OnlyLeft
    );
    assert_eq!(
        macos_option_as_alt(noa_config::MacosOptionAsAlt::Right),
        OptionAsAlt::OnlyRight
    );
    assert_eq!(
        macos_option_as_alt(noa_config::MacosOptionAsAlt::Both),
        OptionAsAlt::Both
    );
}

#[test]
fn quick_terminal_reveal_origin_lerps_hidden_to_final_per_axis() {
    // Top-like travel: only y moves.
    let final_origin = (100, 50);
    let hidden_origin = (100, 50 - 400);
    assert_eq!(
        quick_terminal_reveal_origin(final_origin, hidden_origin, 0.0),
        hidden_origin
    );
    assert_eq!(
        quick_terminal_reveal_origin(final_origin, hidden_origin, 1.0),
        final_origin
    );
    let mid = quick_terminal_reveal_origin(final_origin, hidden_origin, 0.5);
    assert_eq!(mid.0, 100);
    assert!(mid.1 > hidden_origin.1 && mid.1 < final_origin.1);

    // Left-like travel: only x moves.
    let final_origin = (10, 20);
    let hidden_origin = (10 - 300, 20);
    let mid = quick_terminal_reveal_origin(final_origin, hidden_origin, 0.5);
    assert_eq!(mid.1, 20);
    assert!(mid.0 > hidden_origin.0 && mid.0 < final_origin.0);

    // Center: no travel at all — every reveal fraction is the same point.
    let center = (5, 5);
    assert_eq!(quick_terminal_reveal_origin(center, center, 0.0), center);
    assert_eq!(quick_terminal_reveal_origin(center, center, 0.5), center);
    assert_eq!(quick_terminal_reveal_origin(center, center, 1.0), center);
}

#[test]
fn quick_terminal_position_geometry_hidden_and_final_per_position() {
    use noa_config::QuickTerminalPosition::{Bottom, Center, Left, Right, Top};

    // Monitor at (0, 0), 1000x800, panel 200x100.
    let (mx, my, mw, mh, w, h) = (0, 0, 1000u32, 800u32, 200u32, 100u32);

    let (final_origin, hidden_origin) = quick_terminal_position_geometry(Top, mx, my, mw, mh, w, h);
    assert_eq!(final_origin, (400, 0)); // centered x, flush with the top
    assert_eq!(hidden_origin, (400, -100)); // fully above the screen

    let (final_origin, hidden_origin) =
        quick_terminal_position_geometry(Bottom, mx, my, mw, mh, w, h);
    assert_eq!(final_origin, (400, 700)); // flush with the bottom
    assert_eq!(hidden_origin, (400, 800)); // fully below the screen

    let (final_origin, hidden_origin) =
        quick_terminal_position_geometry(Left, mx, my, mw, mh, w, h);
    assert_eq!(final_origin, (0, 350)); // flush with the left edge, centered y
    assert_eq!(hidden_origin, (-200, 350)); // fully off the left edge

    let (final_origin, hidden_origin) =
        quick_terminal_position_geometry(Right, mx, my, mw, mh, w, h);
    assert_eq!(final_origin, (800, 350)); // flush with the right edge
    assert_eq!(hidden_origin, (1000, 350)); // fully off the right edge

    let (final_origin, hidden_origin) =
        quick_terminal_position_geometry(Center, mx, my, mw, mh, w, h);
    assert_eq!(final_origin, (400, 350)); // centered on both axes
    assert_eq!(hidden_origin, final_origin, "center never travels");
}

#[test]
fn ease_out_cubic_is_clamped_and_anchored() {
    assert!((ease_out_cubic(0.0)).abs() < 0.001);
    assert!((ease_out_cubic(1.0) - 1.0).abs() < 0.001);
    // Clamps out-of-range input rather than overshooting.
    assert!((ease_out_cubic(-1.0)).abs() < 0.001);
    assert!((ease_out_cubic(2.0) - 1.0).abs() < 0.001);
    // Ease-out front-loads progress: past the midpoint by t=0.5.
    assert!(ease_out_cubic(0.5) > 0.5);
}

#[test]
fn split_creation_gate_rejects_maximum_existing_panes_and_too_small_rects() {
    let splittable = PaneRectApp::new(0, 0, 3, 3);
    assert!(can_create_split(
        MAX_PANES_PER_TAB - 1,
        splittable,
        SplitOrientation::Horizontal
    ));
    assert!(can_create_split(
        MAX_PANES_PER_TAB - 1,
        splittable,
        SplitOrientation::Vertical
    ));

    assert!(!can_create_split(
        MAX_PANES_PER_TAB,
        splittable,
        SplitOrientation::Horizontal
    ));
    assert!(!can_create_split(
        MAX_PANES_PER_TAB - 1,
        PaneRectApp::new(0, 0, 2, 3),
        SplitOrientation::Horizontal
    ));
    assert!(!can_create_split(
        MAX_PANES_PER_TAB - 1,
        PaneRectApp::new(0, 0, 3, 2),
        SplitOrientation::Vertical
    ));
}

#[test]
fn split_creation_direction_gate_matches_axis_requirements() {
    assert!(can_create_split_in_direction(
        MAX_PANES_PER_TAB - 1,
        PaneRectApp::new(0, 0, 3, 2),
        Direction::Left
    ));
    assert!(can_create_split_in_direction(
        MAX_PANES_PER_TAB - 1,
        PaneRectApp::new(0, 0, 3, 2),
        Direction::Right
    ));
    assert!(!can_create_split_in_direction(
        MAX_PANES_PER_TAB - 1,
        PaneRectApp::new(0, 0, 3, 2),
        Direction::Up
    ));
    assert!(!can_create_split_in_direction(
        MAX_PANES_PER_TAB,
        PaneRectApp::new(0, 0, 3, 3),
        Direction::Left
    ));
}

#[test]
fn mint_available_pane_id_skips_ids_already_used_by_tree_or_surface() {
    let mut next = 2;
    let used = [PaneId::new(2), PaneId::new(3)];

    let pane = mint_available_pane_id(&mut next, |candidate| used.contains(&candidate));

    assert_eq!(pane, PaneId::new(4));
    assert_eq!(next, 5);
}

#[test]
fn quick_terminal_progress_is_linear_and_clamped() {
    let duration = Duration::from_millis(200);
    assert!((quick_terminal_progress(Duration::ZERO, duration)).abs() < 0.001);
    assert!((quick_terminal_progress(Duration::from_millis(100), duration) - 0.5).abs() < 0.001);
    assert!((quick_terminal_progress(Duration::from_millis(400), duration) - 1.0).abs() < 0.001);
    // A zero-length slide is instantly complete (no divide-by-zero).
    assert!((quick_terminal_progress(Duration::ZERO, Duration::ZERO) - 1.0).abs() < 0.001);
}

#[test]
fn quick_terminal_slide_reveal_moves_from_current_state_to_target() {
    let duration = Duration::from_millis(200);

    assert!(
        (quick_terminal_slide_reveal(0.35, 1.0, Duration::ZERO, duration) - 0.35).abs() < 0.001
    );
    assert!((quick_terminal_slide_reveal(0.35, 1.0, duration, duration) - 1.0).abs() < 0.001);
    let showing_mid = quick_terminal_slide_reveal(0.35, 1.0, Duration::from_millis(100), duration);
    assert!(showing_mid > 0.35);
    assert!(showing_mid < 1.0);
    assert!(showing_mid > 0.35 + (1.0 - 0.35) * 0.5);

    assert!(
        (quick_terminal_slide_reveal(0.65, 0.0, Duration::ZERO, duration) - 0.65).abs() < 0.001
    );
    assert!((quick_terminal_slide_reveal(0.65, 0.0, duration, duration)).abs() < 0.001);
    let hiding_mid = quick_terminal_slide_reveal(0.65, 0.0, Duration::from_millis(100), duration);
    assert!(hiding_mid < 0.65);
    assert!(hiding_mid > 0.0);
    assert!(hiding_mid < 0.65 * 0.5);
}

#[test]
fn quick_terminal_slide_reveal_keeps_pixel_position_continuous_when_reversed() {
    let final_origin = (0, 0);
    let hidden_origin = (0, -400);
    let duration = Duration::from_millis(200);
    let reveal_before_reverse =
        quick_terminal_slide_reveal(0.0, 1.0, Duration::from_millis(80), duration);
    let top_before_reverse =
        quick_terminal_reveal_origin(final_origin, hidden_origin, reveal_before_reverse).1;

    let reveal_after_reverse =
        quick_terminal_slide_reveal(reveal_before_reverse, 0.0, Duration::ZERO, duration);
    let top_after_reverse =
        quick_terminal_reveal_origin(final_origin, hidden_origin, reveal_after_reverse).1;

    assert_eq!(top_after_reverse, top_before_reverse);
}

#[test]
fn quick_terminal_slide_reveal_is_monotone_at_frame_interval_samples() {
    let duration = Duration::from_millis(200);
    let mut previous_show = 0.0;
    let mut previous_hide = 1.0;

    for frame in 1..=13 {
        let elapsed = Duration::from_millis((frame * 16).min(200));
        let showing = quick_terminal_slide_reveal(0.0, 1.0, elapsed, duration);
        let hiding = quick_terminal_slide_reveal(1.0, 0.0, elapsed, duration);

        assert!((0.0..=1.0).contains(&showing));
        assert!((0.0..=1.0).contains(&hiding));
        assert!(showing >= previous_show);
        assert!(hiding <= previous_hide);

        previous_show = showing;
        previous_hide = hiding;
    }
}

#[test]
fn quick_terminal_size_footprint_maps_percent_and_defaults_per_position() {
    use noa_config::QuickTerminalPosition::{Bottom, Left, Right, Top};
    use noa_config::{QuickTerminalSize, QuickTerminalSizeDim};

    let forty_percent = QuickTerminalSize {
        primary: Some(QuickTerminalSizeDim::Percent(40.0)),
        secondary: None,
    };
    // top/bottom: primary is height (a % of monitor height), secondary
    // absent fills the full monitor width.
    assert_eq!(
        quick_terminal_size_footprint(Top, forty_percent, 1000, 800, 1.0),
        (1000, 320)
    );
    assert_eq!(
        quick_terminal_size_footprint(Bottom, forty_percent, 1000, 800, 1.0),
        (1000, 320)
    );
    // left/right: primary is width, secondary absent fills the full height.
    assert_eq!(
        quick_terminal_size_footprint(Left, forty_percent, 1000, 800, 1.0),
        (400, 800)
    );
    assert_eq!(
        quick_terminal_size_footprint(Right, forty_percent, 1000, 800, 1.0),
        (400, 800)
    );
}

#[test]
fn quick_terminal_size_footprint_uses_fixed_defaults_when_a_side_is_unset() {
    use noa_config::QuickTerminalPosition::{Left, Top};
    use noa_config::QuickTerminalSize;

    // No primary/secondary at all: falls back to the 400px cross-axis
    // default on the configured side, full parent on the other.
    let unset = QuickTerminalSize {
        primary: None,
        secondary: None,
    };
    assert_eq!(
        quick_terminal_size_footprint(Top, unset, 1000, 800, 1.0),
        (1000, 400)
    );
    assert_eq!(
        quick_terminal_size_footprint(Left, unset, 1000, 800, 1.0),
        (400, 800)
    );
}

#[test]
fn quick_terminal_size_footprint_center_picks_long_axis_by_orientation() {
    use noa_config::QuickTerminalPosition::Center;
    use noa_config::QuickTerminalSize;

    let unset = QuickTerminalSize {
        primary: None,
        secondary: None,
    };
    // Landscape (width >= height): primary is width (800px default), 400px
    // default the short (height) axis.
    assert_eq!(
        quick_terminal_size_footprint(Center, unset, 1000, 800, 1.0),
        (800, 400)
    );
    // Portrait: the long axis flips to height.
    assert_eq!(
        quick_terminal_size_footprint(Center, unset, 800, 1000, 1.0),
        (400, 800)
    );
}

#[test]
fn quick_terminal_size_footprint_scales_pixels_by_scale_factor_and_clamps() {
    use noa_config::QuickTerminalPosition::Top;
    use noa_config::{QuickTerminalSize, QuickTerminalSizeDim};

    let two_hundred_points = QuickTerminalSize {
        primary: Some(QuickTerminalSizeDim::Pixels(200)),
        secondary: None,
    };
    // 200 AppKit points at a 2x scale factor is 400 physical px.
    assert_eq!(
        quick_terminal_size_footprint(Top, two_hundred_points, 1000, 800, 2.0),
        (1000, 400)
    );

    // A configured side that would exceed the monitor clamps to it instead.
    let oversized = QuickTerminalSize {
        primary: Some(QuickTerminalSizeDim::Percent(150.0)),
        secondary: None,
    };
    assert_eq!(
        quick_terminal_size_footprint(Top, oversized, 1000, 800, 1.0),
        (1000, 800)
    );
}

#[test]
fn quick_terminal_autohide_waits_for_focus_in_current_reveal() {
    assert!(!quick_terminal_should_autohide_on_focus_loss(false, false));
    assert!(!quick_terminal_should_autohide_on_focus_loss(false, true));
    assert!(!quick_terminal_should_autohide_on_focus_loss(true, false));
    assert!(quick_terminal_should_autohide_on_focus_loss(true, true));
}

#[test]
fn quick_terminal_suppresses_redraw_only_when_fully_hidden() {
    assert!(!quick_terminal_should_suppress_redraw(false, true));
    assert!(quick_terminal_should_suppress_redraw(false, false));
    assert!(!quick_terminal_should_suppress_redraw(true, false));
    assert!(!quick_terminal_should_suppress_redraw(true, true));
}

#[test]
fn quick_terminal_anchor_prefers_os_focused_regular_window() {
    let anchor = quick_terminal_anchor_window_id(Some(2), Some(1), &[1, 2, 3], |id| id != 3);

    assert_eq!(anchor, Some(2));
}

#[test]
fn quick_terminal_anchor_skips_quick_terminal_and_falls_back_to_recent_regular_window() {
    let anchor = quick_terminal_anchor_window_id(Some(3), Some(3), &[1, 2], |id| id != 3);

    assert_eq!(anchor, Some(2));
}

fn metrics(cell_w: f32, cell_h: f32) -> noa_font::Metrics {
    noa_font::Metrics {
        cell_w,
        cell_h,
        ascent: cell_h * 0.75,
        descent: cell_h * 0.25,
        line_gap: 0.0,
        underline_position: 0.0,
        underline_thickness: 1.0,
    }
}

fn terminal_with_scrollback(grid_size: GridSize) -> Terminal {
    let mut terminal = Terminal::new(grid_size);
    let mut stream = Stream::new();
    stream.feed(b"A\r\nB\r\nC\r\nD\r\nE\r\nF", &mut terminal);
    terminal
}

#[test]
fn font_pixel_size_scales_logical_points() {
    assert_eq!(font_pixel_size(14.0, 1.0), 14.0);
    assert_eq!(font_pixel_size(14.0, 2.0), 28.0);
}

#[test]
fn resolve_grid_padding_keeps_defaults_for_unset_axes() {
    assert_eq!(resolve_grid_padding(None, None), DEFAULT_GRID_PADDING);
}

#[test]
fn resolve_grid_padding_applies_value_to_both_edges_of_an_axis() {
    let padding = resolve_grid_padding(Some(8.0), Some(4.0));
    assert_eq!(padding, GridPadding::new(4.0, 8.0, 4.0, 8.0));

    // Only x set: y keeps the asymmetric default (top 0, bottom 16).
    let x_only = resolve_grid_padding(Some(10.0), None);
    assert_eq!(x_only, GridPadding::new(0.0, 10.0, 16.0, 10.0));

    // Only y set: x keeps the default 16 on both sides.
    let y_only = resolve_grid_padding(None, Some(2.0));
    assert_eq!(y_only, GridPadding::new(2.0, 16.0, 2.0, 24.0));
}

#[test]
fn resolve_cursor_style_is_none_when_nothing_is_configured() {
    assert_eq!(resolve_cursor_style(None, None), None);
}

#[test]
fn resolve_cursor_style_defaults_shape_and_blink() {
    // Only blink toggled: shape defaults to block.
    assert_eq!(
        resolve_cursor_style(None, Some(false)),
        Some(CursorStyle::SteadyBlock)
    );
    // Only shape set: blink defaults on.
    assert_eq!(
        resolve_cursor_style(Some(noa_config::CursorShape::Bar), None),
        Some(CursorStyle::BlinkingBar)
    );
}

#[test]
fn resolve_cursor_style_maps_every_combination() {
    use noa_config::CursorShape;
    let cases = [
        (CursorShape::Block, true, CursorStyle::BlinkingBlock),
        (CursorShape::Block, false, CursorStyle::SteadyBlock),
        (CursorShape::Bar, true, CursorStyle::BlinkingBar),
        (CursorShape::Bar, false, CursorStyle::SteadyBar),
        (CursorShape::Underline, true, CursorStyle::BlinkingUnderline),
        (CursorShape::Underline, false, CursorStyle::SteadyUnderline),
    ];
    for (shape, blink, expected) in cases {
        assert_eq!(
            resolve_cursor_style(Some(shape), Some(blink)),
            Some(expected)
        );
    }
}

#[test]
fn initial_window_size_converts_physical_metrics_to_logical_size() {
    let size = initial_window_logical_size(
        metrics(16.0, 32.0),
        GridSize::new(80, 24),
        2.0,
        DEFAULT_GRID_PADDING,
    );

    assert_eq!(size.width, 660.0);
    assert_eq!(size.height, 392.0);
}

fn test_surface_config(width: u32, height: u32) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: wgpu::TextureFormat::Bgra8Unorm,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 1,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
    }
}

#[test]
fn effective_surface_config_preserves_visible_size() {
    let config = test_surface_config(3840, 2160);
    let effective = effective_surface_config(&config, false);

    assert_eq!(effective.width, 3840);
    assert_eq!(effective.height, 2160);
    assert_eq!(config.width, 3840);
    assert_eq!(config.height, 2160);
}

#[test]
fn effective_surface_config_minimizes_occluded_size_without_mutating_state_config() {
    let config = test_surface_config(3840, 2160);
    let effective = effective_surface_config(&config, true);

    assert_eq!(effective.width, 1);
    assert_eq!(effective.height, 1);
    assert_eq!(config.width, 3840);
    assert_eq!(config.height, 2160);
}

#[test]
fn surface_format_prefers_non_srgb_for_native_gamma_correct_blending() {
    // WP3 / REQ-AA-1 / AC-WP3-01: `native` keeps a non-sRGB surface format so
    // the fixed-function alpha blend unit stays in gamma space, matching
    // Ghostty's `native` macOS text-rendering mode.
    assert_eq!(
        preferred_surface_format(
            &[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ],
            noa_font::AlphaBlending::Native,
        ),
        wgpu::TextureFormat::Bgra8Unorm
    );
}

#[test]
fn surface_format_prefers_srgb_for_linear_alpha_blending_modes() {
    // WP3: `linear` / `linear-corrected` want an sRGB surface so the blend
    // unit decodes to linear before blending.
    for mode in [
        noa_font::AlphaBlending::Linear,
        noa_font::AlphaBlending::LinearCorrected,
    ] {
        assert_eq!(
            preferred_surface_format(
                &[
                    wgpu::TextureFormat::Bgra8Unorm,
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                ],
                mode,
            ),
            wgpu::TextureFormat::Bgra8UnormSrgb,
            "{mode:?} must select an sRGB target for linear-space blending"
        );
    }
}

#[test]
fn surface_format_falls_back_to_srgb_when_no_non_srgb_option_exists() {
    assert_eq!(
        preferred_surface_format(
            &[wgpu::TextureFormat::Bgra8UnormSrgb],
            noa_font::AlphaBlending::Native,
        ),
        wgpu::TextureFormat::Bgra8UnormSrgb
    );
}

#[test]
fn surface_format_linear_falls_back_to_non_srgb_when_no_srgb_option_exists() {
    assert_eq!(
        preferred_surface_format(
            &[wgpu::TextureFormat::Bgra8Unorm],
            noa_font::AlphaBlending::Linear,
        ),
        wgpu::TextureFormat::Bgra8Unorm
    );
}

#[test]
fn surface_format_falls_back_to_first_available_when_neither_bgra8_option_exists() {
    assert_eq!(
        preferred_surface_format(
            &[
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureFormat::Rgba8Unorm,
            ],
            noa_font::AlphaBlending::Native,
        ),
        wgpu::TextureFormat::Rgba16Float
    );
}

#[test]
fn surface_alpha_mode_prefers_opaque_to_keep_terminal_colors_solid() {
    let caps = wgpu::SurfaceCapabilities {
        alpha_modes: vec![
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Opaque,
        ],
        ..Default::default()
    };

    assert_eq!(
        preferred_surface_alpha_mode(&caps, false),
        wgpu::CompositeAlphaMode::Opaque
    );
}

#[test]
fn surface_alpha_mode_falls_back_when_opaque_is_unavailable() {
    let caps = wgpu::SurfaceCapabilities {
        alpha_modes: vec![wgpu::CompositeAlphaMode::Inherit],
        ..Default::default()
    };

    assert_eq!(
        preferred_surface_alpha_mode(&caps, false),
        wgpu::CompositeAlphaMode::Inherit
    );
}

#[test]
fn surface_alpha_mode_prefers_post_multiplied_when_transparent() {
    let caps = wgpu::SurfaceCapabilities {
        alpha_modes: vec![
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
        ],
        ..Default::default()
    };

    assert_eq!(
        preferred_surface_alpha_mode(&caps, true),
        wgpu::CompositeAlphaMode::PostMultiplied
    );
}

#[test]
fn surface_alpha_mode_transparent_falls_back_through_preference_order() {
    // No PostMultiplied — the next preferred transparent mode wins.
    let caps = wgpu::SurfaceCapabilities {
        alpha_modes: vec![
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
        ],
        ..Default::default()
    };

    assert_eq!(
        preferred_surface_alpha_mode(&caps, true),
        wgpu::CompositeAlphaMode::PreMultiplied
    );
}

#[test]
fn surface_alpha_mode_transparent_falls_back_to_first_when_none_preferred() {
    // Only Opaque is offered — a transparent window still has to pick
    // something, so it takes the surface's first advertised mode.
    let caps = wgpu::SurfaceCapabilities {
        alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
        ..Default::default()
    };

    assert_eq!(
        preferred_surface_alpha_mode(&caps, true),
        wgpu::CompositeAlphaMode::Opaque
    );
}

#[test]
fn scale_factor_grid_recompute_uses_new_cell_metrics() {
    let size = PhysicalSize::new(968, 600);

    assert_eq!(
        grid_size_for_physical_size(size, metrics(12.0, 24.0), DEFAULT_GRID_PADDING),
        GridSize::new(77, 24)
    );
    assert_eq!(
        grid_size_for_physical_size(size, metrics(16.0, 30.0), DEFAULT_GRID_PADDING),
        GridSize::new(58, 19)
    );
    assert_eq!(
        grid_size_for_physical_size(
            PhysicalSize::new(1, 1),
            metrics(16.0, 30.0),
            DEFAULT_GRID_PADDING,
        ),
        GridSize::new(1, 1)
    );
}

#[test]
fn runtime_font_size_actions_adjust_and_reset_to_startup_size() {
    assert_eq!(
        runtime_font_size_update(15.0, 15.0, FontSizeAction::Increase),
        RuntimeFontSizeUpdate {
            point_size: 16.0,
            changed: true
        }
    );
    assert_eq!(
        runtime_font_size_update(15.0, 15.0, FontSizeAction::Decrease),
        RuntimeFontSizeUpdate {
            point_size: 14.0,
            changed: true
        }
    );
    assert_eq!(
        runtime_font_size_update(18.0, 15.0, FontSizeAction::Reset),
        RuntimeFontSizeUpdate {
            point_size: 15.0,
            changed: true
        }
    );
    assert_eq!(
        runtime_font_size_update(15.0, 15.0, FontSizeAction::Reset),
        RuntimeFontSizeUpdate {
            point_size: 15.0,
            changed: false
        }
    );
}

#[test]
fn runtime_font_size_actions_clamp_to_supported_range() {
    assert_eq!(
        runtime_font_size_update(96.0, 15.0, FontSizeAction::Increase),
        RuntimeFontSizeUpdate {
            point_size: 96.0,
            changed: false
        }
    );
    assert_eq!(
        runtime_font_size_update(6.0, 15.0, FontSizeAction::Decrease),
        RuntimeFontSizeUpdate {
            point_size: 6.0,
            changed: false
        }
    );
    assert_eq!(
        runtime_font_size_update(120.0, 15.0, FontSizeAction::Decrease),
        RuntimeFontSizeUpdate {
            point_size: 96.0,
            changed: true
        }
    );
    assert_eq!(
        runtime_font_size_update(f32::NAN, 15.0, FontSizeAction::Increase),
        RuntimeFontSizeUpdate {
            point_size: 6.0,
            changed: true
        }
    );
}

#[test]
fn font_size_resize_plan_recomputes_each_window_grid_from_new_metrics() {
    let plan = font_size_resize_plan(
        [
            (1_u8, PhysicalSize::new(968, 600)),
            (2_u8, PhysicalSize::new(488, 300)),
        ],
        metrics(16.0, 30.0),
        DEFAULT_GRID_PADDING,
    );

    assert_eq!(
        plan,
        vec![(1, GridSize::new(58, 19)), (2, GridSize::new(28, 9))]
    );
}

#[test]
fn ime_cursor_area_tracks_grid_cell_in_physical_pixels() {
    let (position, size) = ime_cursor_area(
        metrics(7.5, 15.25),
        2,
        3,
        PaneRectApp::new(0, 0, 100, 100),
        DEFAULT_GRID_PADDING,
    );

    assert_eq!(position.x, 39);
    assert_eq!(position.y, 46);
    assert_eq!(size.width, 8);
    assert_eq!(size.height, 16);
}

#[test]
fn viewport_scroll_commands_move_by_line_page_and_extremes() {
    let grid_size = GridSize::new(5, 3);
    let mut terminal = terminal_with_scrollback(grid_size);

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineUp);
    assert_eq!(terminal.viewport_offset(), 1);

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageUp);
    assert_eq!(terminal.viewport_offset(), 3);

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineDown);
    assert_eq!(terminal.viewport_offset(), 2);

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageDown);
    assert_eq!(terminal.viewport_offset(), 0);

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Top);
    assert_eq!(terminal.viewport_offset(), terminal.scrollback_len());

    apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Bottom);
    assert_eq!(terminal.viewport_offset(), 0);
}

#[test]
fn viewport_scroll_snapshot_tracks_scrolled_row_base() {
    let grid_size = GridSize::new(5, 3);
    let mut terminal = terminal_with_scrollback(grid_size);
    let before_row_base = terminal.active().visible_row_base();

    let snapshot =
        apply_viewport_scroll_and_snapshot(&mut terminal, grid_size, ViewportScroll::LineUp);

    assert_eq!(terminal.viewport_offset(), 1);
    assert_ne!(snapshot.row_base, before_row_base);
    assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
    assert_eq!(
        snapshot.abs_row_base,
        terminal.active().rows_evicted() + terminal.active().visible_row_base()
    );
    assert!(
        snapshot.row_dirty.iter().all(|&dirty| dirty),
        "overview snapshots are full-row dirty"
    );
    assert!(!snapshot.cursor.visible);
}

#[test]
fn mouse_wheel_delta_maps_to_viewport_scroll_rows() {
    assert_eq!(
        mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 2.0), 20.0),
        Some(MouseWheelViewportScroll::Up(2))
    );
    assert_eq!(
        mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 20.0),
        Some(MouseWheelViewportScroll::Down(1))
    );
    assert_eq!(
        mouse_wheel_viewport_scroll(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 45.0)),
            15.0,
        ),
        Some(MouseWheelViewportScroll::Up(3))
    );
    assert_eq!(
        mouse_wheel_viewport_scroll(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -20.0)),
            15.0,
        ),
        Some(MouseWheelViewportScroll::Down(2))
    );
    assert_eq!(
        mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 0.0), 20.0),
        None
    );
}

#[test]
fn mouse_wheel_cursor_key_routing_keeps_primary_screen_local() {
    assert!(mouse_wheel_should_send_cursor_keys(
        MouseTracking::Off,
        true,
        true
    ));

    assert!(!mouse_wheel_should_send_cursor_keys(
        MouseTracking::Off,
        false,
        true
    ));
    assert!(!mouse_wheel_should_send_cursor_keys(
        MouseTracking::Off,
        true,
        false
    ));
    assert!(!mouse_wheel_should_send_cursor_keys(
        MouseTracking::Off,
        false,
        false
    ));
    assert!(!mouse_wheel_should_send_cursor_keys(
        MouseTracking::Press,
        true,
        true
    ));
}

#[test]
fn mouse_wheel_viewport_scroll_moves_terminal_viewport() {
    let grid_size = GridSize::new(5, 3);
    let mut terminal = terminal_with_scrollback(grid_size);

    apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Up(2));
    assert_eq!(terminal.viewport_offset(), 2);

    apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Down(1));
    assert_eq!(terminal.viewport_offset(), 1);
}

#[test]
fn mouse_wheel_viewport_scroll_snapshot_tracks_scrolled_row_base() {
    let grid_size = GridSize::new(5, 3);
    let mut terminal = terminal_with_scrollback(grid_size);

    let snapshot = apply_mouse_wheel_viewport_scroll_and_snapshot(
        &mut terminal,
        MouseWheelViewportScroll::Up(2),
    );

    assert_eq!(terminal.viewport_offset(), 2);
    assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
    assert_eq!(
        snapshot.abs_row_base,
        terminal.active().rows_evicted() + terminal.active().visible_row_base()
    );
    assert!(!snapshot.cursor.visible);
}

#[test]
fn terminal_clear_action_erases_rows_above_cursor_when_not_at_a_prompt() {
    // No OSC 133 shell-integration marks were recorded, so the cursor isn't
    // "at a prompt": the clear erases scrollback plus the rows above the
    // cursor (rather than the whole display) and doesn't ask for a form feed.
    let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));
    terminal.scroll_viewport_up(1);
    terminal.pending_writes.extend_from_slice(b"reply");

    let form_feed = apply_terminal_action(&mut terminal, TerminalAction::Clear, false);

    assert!(!form_feed);
    assert_eq!(terminal.scrollback_len(), 0);
    assert_eq!(terminal.viewport_offset(), 0);
    assert_eq!(terminal.primary.cursor.y, 0);
    assert_eq!(terminal.primary.grid[0].cells[0].ch, 'F');
    assert_eq!(terminal.pending_writes, b"reply");
}

#[test]
fn terminal_clear_scrollback_action_preserves_live_grid() {
    let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

    apply_terminal_action(&mut terminal, TerminalAction::ClearScrollback, false);

    assert_eq!(terminal.scrollback_len(), 0);
    assert_eq!(terminal.primary.grid[0].cells[0].ch, 'D');
    assert_eq!(terminal.primary.grid[1].cells[0].ch, 'E');
    assert_eq!(terminal.primary.grid[2].cells[0].ch, 'F');
}

#[test]
fn terminal_select_all_action_uses_grid_selection_api() {
    let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

    apply_terminal_action(&mut terminal, TerminalAction::SelectAll, false);

    assert_eq!(
        terminal.selected_text().as_deref(),
        Some("A\nB\nC\nD\nE\nF")
    );
}

#[test]
fn terminal_clear_action_does_not_mutate_a_remote_replica() {
    let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));
    let scrollback_len = terminal.scrollback_len();
    let cursor = (terminal.primary.cursor.x, terminal.primary.cursor.y);
    let visible = terminal
        .primary
        .grid
        .iter()
        .map(|row| row.cells[0].ch)
        .collect::<Vec<_>>();

    let form_feed = apply_terminal_action(&mut terminal, TerminalAction::Clear, true);

    assert!(!form_feed);
    assert_eq!(terminal.scrollback_len(), scrollback_len);
    assert_eq!(
        (terminal.primary.cursor.x, terminal.primary.cursor.y),
        cursor
    );
    assert_eq!(
        terminal
            .primary
            .grid
            .iter()
            .map(|row| row.cells[0].ch)
            .collect::<Vec<_>>(),
        visible
    );
}

#[test]
fn close_tab_outcome_is_unambiguous() {
    assert_eq!(
        close_tab_outcome(&[1, 2, 3], Some(2), 9, false),
        TabCloseOutcome::Stale
    );
    assert_eq!(
        close_tab_outcome(&[1], Some(1), 1, false),
        TabCloseOutcome::Quit
    );
    assert_eq!(
        close_tab_outcome(&[1], Some(1), 1, true),
        TabCloseOutcome::Continue { focused: None }
    );
    assert_eq!(
        close_tab_outcome(&[1, 2, 3], Some(2), 2, false),
        TabCloseOutcome::Continue { focused: Some(3) }
    );
    assert_eq!(
        close_tab_outcome(&[1, 2, 3], Some(3), 3, false),
        TabCloseOutcome::Continue { focused: Some(2) }
    );
    assert_eq!(
        close_tab_outcome(&[1, 2, 3], Some(1), 2, false),
        TabCloseOutcome::Continue { focused: Some(1) }
    );
}

#[test]
fn macos_tab_close_focus_is_deferred_until_appkit_teardown_finishes() {
    assert_eq!(
        tab_close_focus_decision(true, Some(2_u8), true),
        TabCloseFocusDecision::Deferred(2),
        "macOS close must defer focus until AppKit finishes native-tab teardown"
    );
}

#[test]
fn non_macos_tab_close_focus_remains_immediate() {
    assert_eq!(
        tab_close_focus_decision(false, Some(2_u8), true),
        TabCloseFocusDecision::Immediate(2),
        "non-macOS close must keep the existing immediate focus behavior"
    );
}

#[test]
fn stale_deferred_tab_close_focus_restore_is_ignored() {
    assert!(
        should_apply_deferred_focus_restore(2_u8, Some(2_u8), true),
        "a live deferred restore for the still-focused tab must be applied"
    );
    assert!(
        !should_apply_deferred_focus_restore(2_u8, Some(3_u8), true),
        "stale restore must be ignored after focus moves to another tab"
    );
    assert!(
        !should_apply_deferred_focus_restore(2_u8, Some(2_u8), false),
        "stale restore must be ignored after its target tab closes"
    );
}

#[test]
fn close_confirm_message_names_scope_and_count() {
    assert_eq!(
        close_confirm_message(CloseConfirmTarget::Pane, 1),
        "A program is still running in this pane. Close it?"
    );
    assert_eq!(
        close_confirm_message(CloseConfirmTarget::Window, 2),
        "2 programs are still running in this window. Close this window?"
    );
    assert_eq!(
        close_confirm_message(CloseConfirmTarget::App, 1),
        "A program is still running in Noa. Quit Noa?"
    );
    assert_eq!(
        close_confirm_message(CloseConfirmTarget::App, 0),
        "Quit Noa?"
    );
}

#[test]
fn spawn_group_choice_routes_new_tab_and_new_window() {
    // New Tab joins the focused window's group; with no focus (startup) it
    // falls back to a fresh group.
    assert_eq!(
        spawn_group_choice(SpawnTarget::CurrentWindow, Some(7_u64)),
        GroupChoice::Existing(7)
    );
    assert_eq!(
        spawn_group_choice::<u64>(SpawnTarget::CurrentWindow, None),
        GroupChoice::Fresh
    );
    // New Window always starts a fresh group, even when one is focused.
    assert_eq!(
        spawn_group_choice(SpawnTarget::NewWindow, Some(7_u64)),
        GroupChoice::Fresh
    );
    assert_eq!(
        spawn_group_choice::<u64>(SpawnTarget::NewWindow, None),
        GroupChoice::Fresh
    );
}

#[test]
fn tab_insert_index_places_new_tab_after_middle_anchor() {
    assert_eq!(tab_insert_index(&[1_u8, 2, 3], Some(2)), 2);
}

#[test]
fn tab_insert_index_places_new_tab_after_end_anchor() {
    assert_eq!(tab_insert_index(&[1_u8, 2, 3], Some(3)), 3);
}

#[test]
fn tab_insert_index_falls_back_to_end_for_missing_anchor() {
    assert_eq!(tab_insert_index(&[1_u8, 2, 3], Some(9)), 3);
}

#[test]
fn tab_insert_index_falls_back_to_end_without_anchor() {
    assert_eq!(tab_insert_index(&[1_u8, 2, 3], None), 3);
}

#[test]
fn ids_in_group_filters_focused_windows_tabs() {
    // Two windows: tabs 1,3 in group 0; tabs 2,4 in group 1. Close Window
    // for the group-0 window must target exactly its tabs, in order.
    let order = [1_u8, 2, 3, 4];
    let group_of = |id: u8| match id {
        1 | 3 => Some(0_u8),
        2 | 4 => Some(1_u8),
        _ => None,
    };
    assert_eq!(ids_in_group(&order, group_of, 0), vec![1, 3]);
    assert_eq!(ids_in_group(&order, group_of, 1), vec![2, 4]);
    // A group with no live tabs yields nothing.
    assert_eq!(ids_in_group(&order, group_of, 9), Vec::<u8>::new());
}

#[test]
fn inter_pane_targets_include_same_group_all_tabs_and_exclude_source_pair() {
    let order = [1_u8, 2, 3];
    let group_of = |window| match window {
        1 | 3 => Some(7_u8),
        2 => Some(8_u8),
        _ => None,
    };
    let panes = |window| match window {
        1 => vec![10_u8, 11],
        2 => vec![20],
        3 => vec![10, 31],
        _ => Vec::new(),
    };

    let targets = inter_pane_targets_in_group(&order, group_of, panes, 1, 10);

    assert_eq!(
        targets,
        vec![
            InterPaneTarget {
                window_id: 1,
                pane_id: 11,
                tab_index: 1,
                pane_index: 2,
            },
            InterPaneTarget {
                window_id: 3,
                pane_id: 10,
                tab_index: 2,
                pane_index: 1,
            },
            InterPaneTarget {
                window_id: 3,
                pane_id: 31,
                tab_index: 2,
                pane_index: 2,
            },
        ]
    );
}

#[test]
fn inter_pane_targets_noop_when_source_is_the_only_pane_in_group() {
    let only_source = inter_pane_targets_in_group(&[1_u8], |_| Some(1_u8), |_| vec![9_u8], 1, 9);
    assert!(only_source.is_empty());
}

#[test]
fn inter_pane_target_label_uses_tab_title_and_pane_identity() {
    assert_eq!(
        inter_pane_target_label(2, Some("api"), 3, 42),
        "Tab 2 - api / Pane 3 (PaneId 42)"
    );
    assert_eq!(
        inter_pane_target_label(1, Some("  "), 1, 7),
        "Tab 1 / Pane 1 (PaneId 7)"
    );
}

#[test]
fn split_tree_pane_ids_returns_all_leaves_in_tree_order() {
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(PaneId::new(1)),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(PaneId::new(2)),
            SplitTree::leaf(PaneId::new(3)),
        ),
    );

    assert_eq!(
        split_tree_pane_ids(&tree),
        vec![PaneId::new(1), PaneId::new(2), PaneId::new(3)]
    );
}

#[test]
fn overview_window_order_excludes_overview_and_closed_tabs() {
    let window_order = [1_u8, 2, 3, 4];
    let live_windows = |id| id != 3;
    let panes_for_window = |id| vec![id + 10];

    let sources =
        overview_tile_source_order(&window_order, live_windows, panes_for_window, Some(4));

    assert_eq!(sources, vec![(1, 11), (2, 12)]);
}

#[test]
fn overview_window_order_expands_each_tab_to_panes_in_leaf_order() {
    let window_order = [1_u8, 2, 3];
    let live_windows = |id| id != 2;
    let panes_for_window = |id| match id {
        1 => vec![11, 12, 13],
        3 => vec![31],
        _ => Vec::new(),
    };

    let sources = overview_tile_source_order(&window_order, live_windows, panes_for_window, None);

    assert_eq!(sources, vec![(1, 11), (1, 12), (1, 13), (3, 31)]);
}

#[test]
fn overview_click_hit_test_resolves_only_live_tiles() {
    let source_ids = [10_u8, 11, 12, 13, 14, 15, 16, 17, 18, 19];
    let layout = compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 90, 120), 9, 0, 0);

    assert_eq!(
        overview_tile_target_at_point(&source_ids, &layout.tiles, split_tree::Point::new(45, 45)),
        Some(14)
    );
    assert_eq!(
        overview_tile_target_at_point(&source_ids, &layout.tiles, split_tree::Point::new(15, 105)),
        None
    );
}

#[test]
fn overview_close_hit_test_is_exclusive_with_tile_focus() {
    let source_ids = [10_u8, 11, 12, 13];
    let layout = compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 200, 200), 9, 0, 0);
    // Tile 0's close button sits at its top-right corner; its body center
    // sits well inside. The two must resolve disjointly (REQ-OV-13).
    let tile0 = layout.tiles[0];
    let close_point = split_tree::Point::new(tile0.right() - 2, tile0.y + 2);
    let body_point = split_tree::Point::new(tile0.x + tile0.w / 2, tile0.y + tile0.h / 2);

    assert_eq!(
        overview_close_target_at_point(
            &source_ids,
            &layout.tiles,
            close_point,
            OverviewMetrics::new(1.0)
        ),
        Some(10)
    );
    assert_eq!(
        overview_tile_target_at_point(&source_ids, &layout.tiles, close_point),
        Some(10),
        "both rects overlap at the corner; the caller's close-first ordering picks the close"
    );
    // The body center is a focus hit but never a close hit.
    assert_eq!(
        overview_close_target_at_point(
            &source_ids,
            &layout.tiles,
            body_point,
            OverviewMetrics::new(1.0)
        ),
        None
    );
    assert_eq!(
        overview_tile_target_at_point(&source_ids, &layout.tiles, body_point),
        Some(10)
    );
}

#[test]
fn targeted_redraw_decision_drops_stale_and_suppresses_occluded_tabs() {
    assert_eq!(
        targeted_redraw_decision(false, false),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        targeted_redraw_decision(true, true),
        TargetedRedrawDecision::Suppress
    );
    assert_eq!(
        targeted_redraw_decision(true, false),
        TargetedRedrawDecision::Request
    );
}

#[test]
fn background_refresh_selection_returns_none_with_no_dirty_windows() {
    let now = Instant::now();
    assert_eq!(
        background_refresh_selection::<u32>(&[], None, now, BG_REFRESH_INTERVAL),
        None
    );
}

#[test]
fn background_refresh_selection_is_due_immediately_with_no_prior_global_refresh() {
    let now = Instant::now();
    assert_eq!(
        background_refresh_selection(&[(1u32, None)], None, now, BG_REFRESH_INTERVAL),
        Some(1)
    );
}

/// The global gate, not a per-window one (kaizen cycle 4, finding P1-B): even
/// though window 2 has never been refreshed (individually "due"), a global
/// refresh within the interval blocks EVERY window's turn, not just window 1's.
#[test]
fn background_refresh_selection_throttles_globally_across_every_window() {
    let start = Instant::now();
    let dirty = [(1u32, Some(start)), (2u32, None)];
    assert_eq!(
        background_refresh_selection(
            &dirty,
            Some(start),
            start + BG_REFRESH_INTERVAL - Duration::from_millis(1),
            BG_REFRESH_INTERVAL
        ),
        None
    );
    assert_eq!(
        background_refresh_selection(
            &dirty,
            Some(start),
            start + BG_REFRESH_INTERVAL,
            BG_REFRESH_INTERVAL
        ),
        Some(2)
    );
}

/// Fairness: among ready candidates, the one refreshed longest ago (or never)
/// wins — a continuously-busy window can't starve a quieter one.
#[test]
fn background_refresh_selection_prefers_the_least_recently_refreshed_window() {
    let now = Instant::now();
    let long_ago = now - Duration::from_secs(10);
    let recently = now - Duration::from_millis(1);
    let dirty = [(1u32, Some(recently)), (2u32, Some(long_ago)), (3u32, None)];
    // Window 3 has never been refreshed at all — more overdue than any
    // timestamped window, however old.
    assert_eq!(
        background_refresh_selection(&dirty, None, now, BG_REFRESH_INTERVAL),
        Some(3)
    );
    let dirty_without_never_refreshed = [(1u32, Some(recently)), (2u32, Some(long_ago))];
    assert_eq!(
        background_refresh_selection(
            &dirty_without_never_refreshed,
            None,
            now,
            BG_REFRESH_INTERVAL
        ),
        Some(2)
    );
}

#[test]
fn bg_refresh_wake_deadline_is_none_when_no_dirty_backlog_remains() {
    // Fully idle: no dirty candidates left, so no timer is armed at all —
    // regardless of when the last refresh happened.
    let now = Instant::now();
    assert_eq!(
        bg_refresh_wake_deadline(false, Some(now), BG_REFRESH_INTERVAL),
        None
    );
    assert_eq!(
        bg_refresh_wake_deadline(false, None, BG_REFRESH_INTERVAL),
        None
    );
}

#[test]
fn bg_refresh_wake_deadline_is_the_next_throttle_reopening_when_backlog_remains() {
    let last_refresh = Instant::now();
    assert_eq!(
        bg_refresh_wake_deadline(true, Some(last_refresh), BG_REFRESH_INTERVAL),
        Some(last_refresh + BG_REFRESH_INTERVAL)
    );
}

#[test]
fn reveal_fast_path_requires_both_pending_and_a_renderable_cached_frame() {
    assert!(!reveal_fast_path_decision(false, true));
    assert!(!reveal_fast_path_decision(true, false));
    assert!(reveal_fast_path_decision(true, true));
}

#[test]
fn stale_pane_user_event_redraw_decision_noops_without_panicking() {
    assert_eq!(
        pane_user_event_redraw_decision(None),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        pane_user_event_redraw_decision(Some((false, false))),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        pane_user_event_redraw_decision(Some((true, true))),
        TargetedRedrawDecision::Suppress
    );
    assert_eq!(
        pane_user_event_redraw_decision(Some((true, false))),
        TargetedRedrawDecision::Request
    );
}

#[test]
fn overview_redraw_decision_respects_visibility_and_occlusion() {
    assert_eq!(
        overview_redraw_decision(None, true, false),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        overview_redraw_decision(Some((false, false)), true, false),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        overview_redraw_decision(Some((true, false)), false, false),
        TargetedRedrawDecision::Stale
    );
    assert_eq!(
        overview_redraw_decision(Some((true, false)), true, true),
        TargetedRedrawDecision::Suppress
    );
    assert_eq!(
        overview_redraw_decision(Some((true, true)), true, false),
        TargetedRedrawDecision::Suppress
    );
    assert_eq!(
        overview_redraw_decision(Some((true, false)), true, false),
        TargetedRedrawDecision::Request
    );
}

#[test]
fn multi_pane_resize_batching_resizes_all_grids_before_pty_winsize_sends() {
    let first = PaneId::new(1);
    let second = PaneId::new(2);
    let third = PaneId::new(3);

    let plan = pane_resize_batch_plan([
        (first, GridSize::new(40, 12)),
        (second, GridSize::new(41, 12)),
        (third, GridSize::new(80, 6)),
    ]);

    assert_eq!(
        plan,
        vec![
            PaneResizeAction::GridResize(first, GridSize::new(40, 12)),
            PaneResizeAction::GridResize(second, GridSize::new(41, 12)),
            PaneResizeAction::GridResize(third, GridSize::new(80, 6)),
            PaneResizeAction::PtyResize(first, GridSize::new(40, 12)),
            PaneResizeAction::PtyResize(second, GridSize::new(41, 12)),
            PaneResizeAction::PtyResize(third, GridSize::new(80, 6)),
        ]
    );
}

// AC-5 (FR-4): with the sidebar inset applied to the window bounds, every
// pane's grid resize still precedes every pty winsize send. Mirrors the
// real relayout path — inset the bounds, lay panes out, derive grid sizes,
// then batch — so the grid-first ordering the shell relies on holds when a
// sidebar toggle triggers the resize.
#[test]
fn sidebar_inset_resize_batching_resizes_all_grids_before_pty_winsize_sends() {
    let metrics = metrics(9.0, 18.0);
    let padding = DEFAULT_GRID_PADDING;
    let first = PaneId::new(1);
    let second = PaneId::new(2);

    // A two-pane horizontal split filling a 1000×600 window.
    let mut tree = split_tree::SplitTree::leaf(first);
    assert!(split_tree::split_pane(
        &mut tree,
        first,
        second,
        split_tree::SplitOrientation::Horizontal,
    ));

    let full = pane_bounds_for_size(PhysicalSize::new(1000, 600));
    let inset_px = 360;
    let inset_bounds = sidebar_inset_bounds(full, inset_px);
    // The inset reserves the sidebar band on the left.
    assert_eq!(
        inset_bounds,
        PaneRectApp::new(inset_px, 0, 1000 - inset_px, 600)
    );

    let targets: Vec<(PaneId, GridSize)> = zoom_resize_targets(&tree, None, inset_bounds)
        .into_iter()
        .map(|(pane_id, rect)| (pane_id, grid_size_for_pane_rect(rect, metrics, padding)))
        .collect();
    assert_eq!(targets.len(), 2);

    let plan = pane_resize_batch_plan(targets.iter().copied());
    // All grid resizes come before any pty winsize send (grid-first).
    let last_grid = plan
        .iter()
        .rposition(|action| matches!(action, PaneResizeAction::GridResize(_, _)))
        .expect("plan has grid resizes");
    let first_pty = plan
        .iter()
        .position(|action| matches!(action, PaneResizeAction::PtyResize(_, _)))
        .expect("plan has pty resizes");
    assert!(
        last_grid < first_pty,
        "every GridResize must precede every PtyResize"
    );

    // The inset genuinely narrows the panes vs. the full-width layout.
    let full_targets: Vec<(PaneId, GridSize)> = zoom_resize_targets(&tree, None, full)
        .into_iter()
        .map(|(pane_id, rect)| (pane_id, grid_size_for_pane_rect(rect, metrics, padding)))
        .collect();
    let inset_cols: u16 = targets.iter().map(|(_, g)| g.cols).sum();
    let full_cols: u16 = full_targets.iter().map(|(_, g)| g.cols).sum();
    assert!(
        inset_cols < full_cols,
        "the sidebar inset must shrink the pane columns"
    );
}

#[test]
fn sidebar_inset_bounds_shifts_and_shrinks_from_the_left() {
    let full = PaneRectApp::new(0, 0, 1000, 600);
    // Zero inset is the identity.
    assert_eq!(sidebar_inset_bounds(full, 0), full);
    // A normal inset shifts x right and removes that width.
    assert_eq!(
        sidebar_inset_bounds(full, 360),
        PaneRectApp::new(360, 0, 640, 600)
    );
    // An inset wider than the window collapses the pane area to zero width
    // without underflowing.
    assert_eq!(
        sidebar_inset_bounds(full, 2000),
        PaneRectApp::new(1000, 0, 0, 600)
    );
}

// FM-4 regression: text-area px must come from the same `rect`/padding
// grid_size_for_pane_rect used, not an independent cell_w × cols
// multiplication — which would drift whenever the pane's pixel size
// isn't an exact multiple of the cell size (as here: 137px / 9px cells).
#[test]
fn pixel_metrics_for_pane_derive_text_area_from_rect_not_from_grid_size() {
    let rect = PaneRectApp::new(0, 0, 137, 245);
    let metrics = metrics(9.0, 18.0);

    let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

    assert_eq!(cw, 9);
    assert_eq!(ch, 18);
    // 137 - (24 left + 16 right) = 97, 245 - (0 top + 16 bottom) = 229 —
    // NOT floor(97/9)=10 cols * 9 = 90, which cell_w × cols would give.
    assert_eq!(taw, 97);
    assert_eq!(tah, 229);
}

#[test]
fn pixel_metrics_for_pane_clamps_padding_larger_than_rect_to_zero() {
    let rect = PaneRectApp::new(0, 0, 10, 10);
    let metrics = metrics(9.0, 18.0);

    let (_, _, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

    assert_eq!(taw, 0);
    assert_eq!(tah, 0);
}

#[test]
fn focus_reporting_encodes_csi_i_and_csi_o_only_when_enabled() {
    assert_eq!(focus_report_bytes(true, true), Some(b"\x1b[I".as_slice()));
    assert_eq!(focus_report_bytes(false, true), Some(b"\x1b[O".as_slice()));
    assert_eq!(focus_report_bytes(true, false), None);
    assert_eq!(focus_report_bytes(false, false), None);
}

#[test]
fn pane_keyboard_focus_uses_os_focus_not_sticky_last_focus() {
    assert!(pane_owns_keyboard_focus(1_u8, 10_u8, Some(1_u8), 10_u8));
    assert!(!pane_owns_keyboard_focus(1_u8, 11_u8, Some(1_u8), 10_u8));
    assert!(!pane_owns_keyboard_focus(1_u8, 10_u8, Some(2_u8), 10_u8));
    assert!(
        !pane_owns_keyboard_focus(1_u8, 10_u8, None, 10_u8),
        "a backgrounded app keeps sticky focus for commands, but not for cursor rendering"
    );
}

#[test]
fn keyboard_preedit_swallowing_is_scoped_to_the_owning_window() {
    assert!(keyboard_preedit_should_swallow_key(Some(1_u8), 1_u8, false));
    assert!(
        !keyboard_preedit_should_swallow_key(Some(1_u8), 2_u8, false),
        "a modal preedit from another tab must not block this tab's keyboard input"
    );
    assert!(keyboard_preedit_should_swallow_key(None, 2_u8, true));
}

#[test]
fn command_target_resolution_uses_focused_tab_only_for_terminal_commands() {
    let focused = Some(42_u8);
    for command in [
        AppCommand::Copy,
        AppCommand::Paste,
        AppCommand::SendSelectionToPane,
        AppCommand::ExportScrollback,
        AppCommand::PipeScrollbackToPager,
        AppCommand::Terminal(TerminalAction::Clear),
        AppCommand::Terminal(TerminalAction::ClearScrollback),
        AppCommand::Terminal(TerminalAction::SelectAll),
        AppCommand::FontSize(FontSizeAction::Increase),
        AppCommand::FontSize(FontSizeAction::Decrease),
        AppCommand::FontSize(FontSizeAction::Reset),
        AppCommand::Search(SearchAction::Find),
        AppCommand::Search(SearchAction::FindNext),
        AppCommand::Search(SearchAction::FindPrevious),
        AppCommand::Search(SearchAction::Clear),
        AppCommand::ScrollViewport(ViewportScroll::PageDown),
        AppCommand::CloseTab,
    ] {
        assert_eq!(resolve_command_target(command, focused), focused);
    }

    for command in [
        AppCommand::NewTab,
        AppCommand::SelectTab(1),
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::ToggleFullscreen,
        AppCommand::Quit,
    ] {
        assert_eq!(resolve_command_target(command, focused), None);
    }
}

#[test]
fn toggle_tab_overview_is_a_native_tab_group_command() {
    assert_eq!(
        command_scope(AppCommand::ToggleTabOverview),
        CommandScope::NativeTabGroup
    );
    assert_eq!(
        resolve_command_target(AppCommand::ToggleTabOverview, Some(42_u8)),
        None
    );
}

#[test]
fn overview_command_scope_resolves_terminal_commands_to_no_ops() {
    let focused = Some(42_u8);
    for command in [
        AppCommand::Copy,
        AppCommand::Paste,
        AppCommand::SendSelectionToPane,
        AppCommand::ExportScrollback,
        AppCommand::PipeScrollbackToPager,
        AppCommand::Terminal(TerminalAction::Clear),
        AppCommand::Terminal(TerminalAction::ClearScrollback),
        AppCommand::Terminal(TerminalAction::SelectAll),
        AppCommand::FontSize(FontSizeAction::Increase),
        AppCommand::FontSize(FontSizeAction::Decrease),
        AppCommand::FontSize(FontSizeAction::Reset),
        AppCommand::Search(SearchAction::Find),
        AppCommand::Search(SearchAction::FindNext),
        AppCommand::Search(SearchAction::FindPrevious),
        AppCommand::Search(SearchAction::Clear),
        AppCommand::ScrollViewport(ViewportScroll::PageDown),
        AppCommand::NewSplitLeft,
        AppCommand::NewSplitRight,
        AppCommand::NewSplitUp,
        AppCommand::NewSplitDown,
        AppCommand::FocusDirection(Direction::Left),
        AppCommand::ResizeSplit(Direction::Right),
        AppCommand::EqualizeSplits,
        AppCommand::ToggleSplitZoom,
        AppCommand::CloseTab,
    ] {
        assert_eq!(overview_command_scope(command), CommandScope::Overview);
        assert_eq!(resolve_command_target(command, focused), focused);
    }

    assert_eq!(
        overview_command_scope(AppCommand::ToggleTabOverview),
        CommandScope::NativeTabGroup
    );
}

#[test]
fn overview_intercepts_only_non_terminal_window_commands() {
    let command = AppCommand::Paste;

    assert!(overview_should_intercept_command(
        command,
        true,
        CommandOrigin::OverviewWindow
    ));
    assert!(overview_should_intercept_command(
        command,
        true,
        CommandOrigin::App
    ));
    assert!(!overview_should_intercept_command(
        command,
        true,
        CommandOrigin::TerminalWindow
    ));
    assert!(!overview_should_intercept_command(
        command,
        false,
        CommandOrigin::OverviewWindow
    ));
    assert!(!overview_should_intercept_command(
        AppCommand::ToggleTabOverview,
        true,
        CommandOrigin::OverviewWindow
    ));
}

#[test]
fn overview_snapshot_seed_skips_locked_terminal_without_waiting() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));
    let slot = Arc::new(Mutex::new(None));
    let _guard = terminal.lock();

    assert!(!try_refresh_overview_snapshot(&terminal, &slot));
    assert!(slot.lock().is_none());
}

#[test]
fn overview_snapshot_seed_refreshes_available_terminal() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));
    let slot = Arc::new(Mutex::new(None));

    assert!(try_refresh_overview_snapshot(&terminal, &slot));
    assert!(slot.lock().is_some());
}

#[test]
fn overview_snapshot_seed_refresh_reuses_unique_slot() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));
    let slot = Arc::new(Mutex::new(None));

    assert!(try_refresh_overview_snapshot(&terminal, &slot));
    let first_ptr = {
        let slot = slot.lock();
        Arc::as_ptr(slot.as_ref().expect("first refresh publishes"))
    };

    terminal.lock().primary.grid[0].cells[0].ch = 'Z';
    assert!(try_refresh_overview_snapshot(&terminal, &slot));

    let slot = slot.lock();
    let snapshot = slot.as_ref().expect("second refresh publishes");
    assert_eq!(Arc::as_ptr(snapshot), first_ptr);
    assert_eq!(snapshot.rows[0].cells[0].ch, 'Z');
}

#[test]
fn toggle_tab_overview_dispatch_flips_visibility() {
    let overview_visible =
        tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, false)
            .expect("toggle command should update overview state");
    assert!(overview_visible);
    assert_eq!(
        tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, overview_visible),
        Some(false)
    );
    assert_eq!(
        tab_overview_visibility_after_dispatch(AppCommand::Copy, overview_visible),
        None
    );
}

#[test]
fn empty_terminal_title_falls_back_to_app_name() {
    // No override, no shell title, and no cwd/process to build a dynamic
    // fallback from → the app name.
    assert_eq!(resolved_tab_title(None, "", None, None), "Noa");
    // A non-empty shell title (OSC 0/2) is shown verbatim.
    assert_eq!(resolved_tab_title(None, "shell", None, None), "shell");
}

#[test]
fn empty_shell_title_uses_dynamic_process_and_cwd_fallback() {
    // cwd only → the cwd's tail segment (the repo/dir name).
    assert_eq!(
        resolved_tab_title(None, "", Some("/Users/me/repos/noa"), None),
        "noa"
    );
    // A non-shell process only (no cwd) → the process name.
    assert_eq!(resolved_tab_title(None, "", None, Some("vim")), "vim");
    // Both → `process — cwdtail`.
    assert_eq!(
        resolved_tab_title(None, "", Some("/Users/me/repos/noa"), Some("cargo")),
        "cargo — noa"
    );
    // A plain shell as the foreground process collapses to just the cwd tail
    // (a `zsh` prompt reads better as its directory name); a login-shell `-`
    // argv0 prefix and a full path both still classify as the shell.
    assert_eq!(
        resolved_tab_title(None, "", Some("/Users/me/repos/noa"), Some("zsh")),
        "noa"
    );
    assert_eq!(
        resolved_tab_title(None, "", Some("/Users/me/repos/noa"), Some("-zsh")),
        "noa"
    );
    assert_eq!(
        resolved_tab_title(None, "", Some("/Users/me/repos/noa"), Some("/bin/bash")),
        "noa"
    );
    // A plain shell with no cwd has nothing to show → the app name.
    assert_eq!(resolved_tab_title(None, "", None, Some("zsh")), "Noa");
}

#[test]
fn command_palette_toggle_is_app_scoped_and_overview_no_op() {
    // AC-1: openable from any tab. AC-15: a no-op while the overview is
    // focused (Overview scope).
    assert_eq!(
        command_scope(AppCommand::ToggleCommandPalette),
        CommandScope::App
    );
    assert_eq!(
        overview_command_scope(AppCommand::ToggleCommandPalette),
        CommandScope::Overview
    );
}

#[test]
fn fullscreen_toggle_is_app_scoped() {
    assert_eq!(
        command_scope(AppCommand::ToggleFullscreen),
        CommandScope::App
    );
    assert_eq!(
        overview_command_scope(AppCommand::ToggleFullscreen),
        CommandScope::App
    );
    assert_eq!(
        resolve_command_target(AppCommand::ToggleFullscreen, Some(42_u8)),
        None
    );
}

#[test]
fn command_palette_snapshot_reflects_query_selection_and_keybinds() {
    // AC-18: the render payload mirrors the session (query / grouped rows /
    // symbol keybind hints / selected) with no terminal involved.
    use noa_render::PaletteRow;
    let keybinds = KeybindEngine::default();
    let palette = CommandPalette::open();

    let snapshot = command_palette_snapshot(&keybinds, &palette, |_| true);
    assert_eq!(snapshot.query, "");
    assert_eq!(
        snapshot.total_entries,
        command_palette::command_palette_entries().len()
    );
    // The empty-query view is grouped: the first row is the Application
    // heading, and the highlight lands on the first entry just below it.
    assert_eq!(
        snapshot.rows[0],
        PaletteRow::Header {
            label: "Application".to_string()
        }
    );
    assert!(matches!(
        &snapshot.rows[snapshot.selected],
        PaletteRow::Entry { .. }
    ));
    // About has no binding; Copy carries its ⌘C hint (rendered as symbols).
    assert!(snapshot.rows.contains(&PaletteRow::Entry {
        title: "About Noa".to_string(),
        hint: None,
        match_positions: Vec::new(),
        enabled: true,
    }));
    assert!(
        snapshot.rows.contains(&PaletteRow::Entry {
            title: "Copy to Clipboard".to_string(),
            hint: Some("\u{2318}C".to_string()),
            match_positions: Vec::new(),
            enabled: true,
        }),
        "keybind hints are resolved from the engine and rendered as symbols"
    );
}

#[test]
fn command_palette_snapshot_marks_unavailable_commands_disabled() {
    use noa_render::PaletteRow;
    let keybinds = KeybindEngine::default();
    let mut palette = CommandPalette::open();
    palette.push_text("add pane right");

    let snapshot = command_palette_snapshot(&keybinds, &palette, |command| {
        command != AppCommand::NewSplitRight
    });

    assert!(snapshot.rows.contains(&PaletteRow::Entry {
        title: "Add Pane Right".to_string(),
        hint: Some("\u{2318}D".to_string()),
        match_positions: (0..14).collect(),
        enabled: false,
    }));
}

// tab-title AC-TTL-5: the override masks any shell title; without one the
// existing shell-title/fallback path is untouched.
#[test]
fn resolved_tab_title_prefers_the_override_over_any_shell_title() {
    assert_eq!(
        resolved_tab_title(Some("api server"), "vim", None, None),
        "api server"
    );
    assert_eq!(
        resolved_tab_title(Some("api server"), "", None, None),
        "api server"
    );
    // The override even masks a dynamic process/cwd fallback (REQ-TTL-5).
    assert_eq!(
        resolved_tab_title(Some("api server"), "", Some("/Users/me/noa"), Some("cargo")),
        "api server"
    );
    assert_eq!(resolved_tab_title(None, "vim", None, None), "vim");
    // A non-empty OSC title wins over the dynamic fallback.
    assert_eq!(
        resolved_tab_title(None, "vim", Some("/Users/me/noa"), Some("cargo")),
        "vim"
    );
    assert_eq!(resolved_tab_title(None, "", None, None), "Noa");
}

// tab-close title-freeze fix: `refresh_window_title` applies the resolved
// title through this diff on every redraw AND while occluded, so a changed
// title always propagates to the applied mirror (returns `Some`) regardless
// of the window's occlusion state, while an unchanged one skips the AppKit
// `set_title` (returns `None`).
#[test]
fn tab_title_update_fires_only_when_the_applied_mirror_differs() {
    // A background tab whose shell retitled: the mirror still holds the old
    // (last-foreground) value, so the change must propagate.
    assert_eq!(
        tab_title_update("old tab", "cargo — noa"),
        Some("cargo — noa".to_string())
    );
    // Promotion to a title that already matches the mirror is a no-op — no
    // redundant titlebar layout pass.
    assert_eq!(tab_title_update("cargo — noa", "cargo — noa"), None);
    // Falling back to the default is still a change when the mirror held a
    // stale shell title.
    assert_eq!(tab_title_update("vim", "Noa"), Some("Noa".to_string()));
}

// AC-PXI-5: the diff-cache helper skips the setter when the raw cwd is
// unchanged across frames, and fires (with the new value to cache) when it
// changes — including a focus switch, which the caller feeds in as a
// changed `current_cwd` with no OSC 7 involved.
#[test]
fn proxy_icon_update_only_fires_on_a_changed_raw_cwd() {
    let cached = Some("/a".to_string());

    assert_eq!(proxy_icon_update(&cached, Some("/a")), None);
    assert_eq!(
        proxy_icon_update(&cached, Some("/b")),
        Some(Some("/b".to_string()))
    );
    assert_eq!(proxy_icon_update(&cached, None), Some(None));
    assert_eq!(proxy_icon_update(&None, None), None);
}

// AC-PXI-6: no `Path::exists` check — a nonexistent path still resolves,
// gated only by the `visible`/`hidden` config value.
#[test]
fn resolve_proxy_icon_path_gates_on_config_visibility_only() {
    assert_eq!(
        resolve_proxy_icon_path(true, Some("/does/not/exist")),
        Some("/does/not/exist".to_string())
    );
    assert_eq!(resolve_proxy_icon_path(false, Some("/a")), None);
    assert_eq!(resolve_proxy_icon_path(true, None), None);
}
