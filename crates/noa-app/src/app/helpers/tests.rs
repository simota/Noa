    use super::*;

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
    fn quick_terminal_slide_offset_spans_hidden_to_revealed() {
        let height = 400.0;
        // Fully hidden: the whole panel sits above the screen top.
        assert!((quick_terminal_top_offset(height, 0.0) - (-height)).abs() < 0.001);
        // Fully revealed: flush with the screen top.
        assert!(quick_terminal_top_offset(height, 1.0).abs() < 0.001);
        // Monotonic: more reveal never moves the panel back up.
        let quarter = quick_terminal_top_offset(height, 0.25);
        let half = quick_terminal_top_offset(height, 0.5);
        assert!(quarter < half);
        assert!(half < 0.0);
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
    fn quick_terminal_progress_is_linear_and_clamped() {
        let duration = Duration::from_millis(200);
        assert!((quick_terminal_progress(Duration::ZERO, duration)).abs() < 0.001);
        assert!(
            (quick_terminal_progress(Duration::from_millis(100), duration) - 0.5).abs() < 0.001
        );
        assert!(
            (quick_terminal_progress(Duration::from_millis(400), duration) - 1.0).abs() < 0.001
        );
        // A zero-length slide is instantly complete (no divide-by-zero).
        assert!((quick_terminal_progress(Duration::ZERO, Duration::ZERO) - 1.0).abs() < 0.001);
    }

    #[test]
    fn quick_terminal_height_is_a_clamped_screen_fraction() {
        assert_eq!(quick_terminal_height(1000, 0.4), 400);
        assert_eq!(quick_terminal_height(1000, 1.0), 1000);
        // Fraction is clamped to a usable range and never exceeds the screen.
        assert_eq!(quick_terminal_height(1000, 2.0), 1000);
        assert_eq!(quick_terminal_height(1000, 0.0), 50);
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
        assert_eq!(y_only, GridPadding::new(2.0, 16.0, 2.0, 16.0));
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

        assert_eq!(size.width, 656.0);
        assert_eq!(size.height, 392.0);
    }

    #[test]
    fn surface_format_prefers_non_srgb_for_native_gamma_correct_blending() {
        // WP3 / REQ-AA-1 / AC-WP3-01: a non-sRGB surface format keeps the
        // fixed-function alpha blend unit in gamma space, matching
        // Ghostty's `native` macOS text-rendering mode.
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ]),
            wgpu::TextureFormat::Bgra8Unorm
        );
    }

    #[test]
    fn surface_format_falls_back_to_srgb_when_no_non_srgb_option_exists() {
        assert_eq!(
            preferred_surface_format(&[wgpu::TextureFormat::Bgra8UnormSrgb]),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn surface_format_falls_back_to_first_available_when_neither_bgra8_option_exists() {
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureFormat::Rgba8Unorm,
            ]),
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
            GridSize::new(78, 24)
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

        assert_eq!(position.x, 31);
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
    fn terminal_clear_action_uses_grid_clear_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));
        terminal.scroll_viewport_up(1);
        terminal.pending_writes.extend_from_slice(b"reply");

        apply_terminal_action(&mut terminal, TerminalAction::Clear);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.viewport_offset(), 0);
        assert_eq!(terminal.pending_writes, b"reply");
    }

    #[test]
    fn terminal_clear_scrollback_action_preserves_live_grid() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::ClearScrollback);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.primary.grid[0].cells[0].ch, 'D');
        assert_eq!(terminal.primary.grid[1].cells[0].ch, 'E');
        assert_eq!(terminal.primary.grid[2].cells[0].ch, 'F');
    }

    #[test]
    fn terminal_select_all_action_uses_grid_selection_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::SelectAll);

        assert_eq!(
            terminal.selected_text().as_deref(),
            Some("A\nB\nC\nD\nE\nF")
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
            "A program is still running in noa. Quit noa?"
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

        let sources =
            overview_tile_source_order(&window_order, live_windows, panes_for_window, None);

        assert_eq!(sources, vec![(1, 11), (1, 12), (1, 13), (3, 31)]);
    }

    #[test]
    fn overview_click_hit_test_resolves_only_live_tiles() {
        let source_ids = [10_u8, 11, 12, 13, 14, 15, 16, 17, 18, 19];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 90, 120), 9, 0, 0);

        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(45, 45)
            ),
            Some(14)
        );
        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(15, 105)
            ),
            None
        );
    }

    #[test]
    fn overview_close_hit_test_is_exclusive_with_tile_focus() {
        let source_ids = [10_u8, 11, 12, 13];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 200, 200), 9, 0, 0);
        // Tile 0's close button sits at its top-right corner; its body center
        // sits well inside. The two must resolve disjointly (REQ-OV-13).
        let tile0 = layout.tiles[0];
        let close_point = split_tree::Point::new(tile0.right() - 2, tile0.y + 2);
        let body_point = split_tree::Point::new(tile0.x + tile0.w / 2, tile0.y + tile0.h / 2);

        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10)
        );
        assert_eq!(
            overview_tile_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10),
            "both rects overlap at the corner; the caller's close-first ordering picks the close"
        );
        // The body center is a focus hit but never a close hit.
        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, body_point),
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
        // 137 - (16 left + 16 right) = 105, 245 - (0 top + 16 bottom) = 229 —
        // NOT floor(105/9)=11 cols * 9 = 99, which cell_w × cols would give.
        assert_eq!(taw, 105);
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
    fn command_target_resolution_uses_focused_tab_only_for_terminal_commands() {
        let focused = Some(42_u8);
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
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
            AppCommand::NewSplitRight,
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
        let _guard = terminal.lock().expect("terminal mutex poisoned");

        assert!(try_peek_overview_snapshot(&terminal).is_none());
    }

    #[test]
    fn overview_snapshot_seed_peeks_available_terminal() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));

        assert!(try_peek_overview_snapshot(&terminal).is_some());
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
        assert_eq!(tab_title(""), "noa");
        assert_eq!(tab_title("shell"), "shell");
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
    fn command_palette_snapshot_reflects_query_selection_and_keybinds() {
        // AC-18: the render payload mirrors the session (query / filtered
        // titles + keybind hints / selected) with no terminal involved.
        let keybinds = KeybindEngine::default();
        let palette = CommandPalette::open();

        let snapshot = command_palette_snapshot(&keybinds, &palette);
        assert_eq!(snapshot.query, "");
        assert_eq!(snapshot.selected, 0);
        assert_eq!(
            snapshot.rows.len(),
            command_palette::command_palette_entries().len()
        );
        // First entry is About (no binding); Copy carries its cmd+c hint.
        assert_eq!(snapshot.rows[0], ("About noa".to_string(), None));
        assert!(
            snapshot
                .rows
                .contains(&("Copy to Clipboard".to_string(), Some("cmd+c".to_string()))),
            "keybind hints are resolved from the engine"
        );
    }
