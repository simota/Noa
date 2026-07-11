//! Pure Session Overview layout and hit-test math.
//!
//! This module deliberately stays independent of windows, terminals, ptys, and
//! GPU state so overview behavior can be tested without constructing app
//! runtime objects.

pub use crate::split_tree::{Direction, Point, Rect as TileRect};

mod input;
mod layout;
mod metrics;
mod render;
mod text;

pub use input::{
    OverviewAction, OverviewEscapeAction, WHEEL_PAGE_THRESHOLD, clamp_overview_page,
    move_overview_selection, overview_escape_action, overview_initial_selection,
    overview_key_action, overview_page_count, overview_page_slice_range, overview_tab_filter,
    overview_wheel_accum_on_show, page_after_wheel, page_step,
};
pub use layout::{
    OverviewChrome, OverviewLayout, compute_overview_grid, hit_test_overview_grid,
    overview_chrome_bands, overview_close_button_rect, overview_close_hit_test,
    overview_hint_bar_rect, overview_search_field_rect, overview_zoom_rect,
};
pub use metrics::{
    OVERVIEW_CARD_BORDER_WIDTH, OVERVIEW_CARD_CORNER_RADIUS, OVERVIEW_CARD_FOCUS_GLOW_WIDTH,
    OVERVIEW_CARD_FOCUS_WIDTH, OVERVIEW_GRID_CAP, OVERVIEW_HINT_BAND_H, OVERVIEW_HINT_BAR_H,
    OVERVIEW_HINT_BAR_MAX_W, OVERVIEW_HINT_BAR_MIN_W, OVERVIEW_MAX_RENDER_TILES_PER_FRAME,
    OVERVIEW_OUTER_MARGIN, OVERVIEW_SEARCH_BAND_H, OVERVIEW_SEARCH_FIELD_H,
    OVERVIEW_SEARCH_FIELD_MAX_W, OVERVIEW_SEARCH_FIELD_MIN_W, OVERVIEW_TILE_GUTTER,
    OVERVIEW_TILE_MIN_RENDER_INTERVAL, OVERVIEW_TITLE_BAR_H, OverviewMetrics, overview_bg_color,
    overview_border_color, overview_card_color, overview_chrome_border_color,
    overview_chrome_pill_color, overview_focus_ring_color, overview_label_padding,
    overview_title_bar_color,
};
pub use render::{
    OverviewBacklogDecision, OverviewRenderCandidate, OverviewResourceEvent, OverviewTileMode,
    overview_backlog_decision, overview_regen_required, overview_tile_mode_for_budget,
    select_due_overview_tile_ids, should_render_tile,
};
pub use text::{
    OVERVIEW_SEARCH_PLACEHOLDER, OverviewTileLabel, TITLE_BAR_CLOSE_GLYPH, center_label,
    overview_hint_bar_row, overview_hint_bar_text, overview_hint_bar_text_ascii,
    overview_hint_bar_text_compact, overview_placeholder_source_ids, overview_search_field_row,
    overview_search_field_text, overview_tile_labels, sanitize_placeholder_label,
    title_bar_row_ansi, title_bar_row_with_close,
};

#[cfg(test)]
use text::text_cell_width;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    const BOUNDS: TileRect = TileRect::new(10, 20, 90, 120);

    #[test]
    fn overview_grid_handles_zero_tabs_and_all_closed_as_empty() {
        let layout = compute_overview_grid(0, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

        assert_eq!(layout.cols, 0);
        assert_eq!(layout.rows, 0);
        assert_eq!(layout.placeholder_rows, 0);
        assert!(layout.tiles.is_empty());
        assert!(layout.placeholders.is_empty());
        assert!(!layout.overflow);
    }

    #[test]
    fn overview_grid_uses_equal_size_row_major_tiles_up_to_cap() {
        let cases = [
            (1, 1, 1),
            (2, 2, 1),
            (5, 3, 2),
            (7, 3, 3),
            (8, 3, 3),
            (9, 3, 3),
        ];

        for (tab_count, expected_cols, expected_rows) in cases {
            let layout = compute_overview_grid(tab_count, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

            assert_eq!(layout.cols, expected_cols, "cols for {tab_count}");
            assert_eq!(layout.rows, expected_rows, "rows for {tab_count}");
            assert_eq!(layout.tiles.len(), tab_count, "tile count for {tab_count}");
            assert!(
                layout.placeholders.is_empty(),
                "placeholders for {tab_count}"
            );
            assert!(!layout.overflow, "overflow for {tab_count}");
            assert_equal_tile_size(&layout.tiles);
            assert_row_major(&layout.tiles, expected_cols);
            assert_no_overlap(&layout.tiles);
        }
    }

    // A5 guard (v3 paging): every page is fed at most `OVERVIEW_GRID_CAP`
    // tiles (the page slice), so `compute_overview_grid` must never degrade
    // to placeholders/overflow for any length in that range — a page has no
    // placeholder rows by construction.
    #[test]
    fn compute_overview_grid_never_yields_placeholders_or_overflow_at_or_under_the_cap() {
        for tab_count in 0..=OVERVIEW_GRID_CAP {
            let layout = compute_overview_grid(tab_count, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
            assert!(
                layout.placeholders.is_empty(),
                "placeholders for {tab_count}"
            );
            assert!(!layout.overflow, "overflow for {tab_count}");
            assert_eq!(layout.tiles.len(), tab_count);
        }
    }

    #[test]
    fn overview_grid_places_overflow_in_title_only_placeholder_rows() {
        let ten = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        assert_eq!(ten.cols, 3);
        assert_eq!(ten.rows, 3);
        assert_eq!(ten.placeholder_rows, 1);
        assert_eq!(ten.tiles.len(), 9);
        assert_eq!(ten.placeholders.len(), 1);
        assert!(ten.overflow);
        assert_equal_tile_size(&ten.tiles);
        assert_eq!(ten.placeholders[0], TileRect::new(10, 110, 30, 30));

        let twelve = compute_overview_grid(12, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        assert_eq!(twelve.cols, 3);
        assert_eq!(twelve.rows, 3);
        assert_eq!(twelve.placeholder_rows, 1);
        assert_eq!(twelve.tiles.len(), 9);
        assert_eq!(twelve.placeholders.len(), 3);
        assert!(twelve.overflow);
        assert_eq!(
            twelve.placeholders,
            vec![
                TileRect::new(10, 110, 30, 30),
                TileRect::new(40, 110, 30, 30),
                TileRect::new(70, 110, 30, 30),
            ]
        );
    }

    #[test]
    fn overview_grid_leaves_trailing_row_empty_cells_for_non_square_counts() {
        let layout = compute_overview_grid(5, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

        assert_eq!(layout.cols, 3);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.tiles[4], TileRect::new(40, 80, 30, 60));

        let empty_cell_point = Point::new(75, 90);
        let hit_tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index, *rect))
            .collect();
        assert_eq!(hit_test_overview_grid(&hit_tiles, empty_cell_point), None);
    }

    #[test]
    fn overview_hit_test_maps_each_tile_interior_back_to_its_id() {
        let layout = compute_overview_grid(8, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        let tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index as u64 + 100, *rect))
            .collect();

        for (id, rect) in &tiles {
            let point = Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
            assert_eq!(hit_test_overview_grid(&tiles, point), Some(*id));
        }
    }

    #[test]
    fn overview_hit_test_ignores_outside_gaps_and_placeholder_row() {
        let layout = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        let tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index, *rect))
            .collect();

        assert_eq!(hit_test_overview_grid(&tiles, Point::new(0, 0)), None);
        assert_eq!(hit_test_overview_grid(&tiles, Point::new(101, 20)), None);
        assert_eq!(hit_test_overview_grid(&tiles, Point::new(15, 115)), None);
    }

    #[test]
    fn should_render_tile_uses_dirty_gate_and_min_interval() {
        let now = Instant::now();
        let last = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;

        assert!(!should_render_tile(
            false,
            Some(due),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(should_render_tile(
            true,
            None,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(!should_render_tile(
            true,
            Some(last),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(should_render_tile(
            true,
            Some(due),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
    }

    #[test]
    fn overview_lock_count_selects_only_dirty_due_tiles_up_to_cap() {
        let now = Instant::now();
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;
        let too_recent = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: false,
                last_render_at: Some(due),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: true,
                last_render_at: Some(too_recent),
            },
            OverviewRenderCandidate {
                id: 3,
                dirty: true,
                last_render_at: Some(due),
            },
            OverviewRenderCandidate {
                id: 4,
                dirty: true,
                last_render_at: None,
            },
            OverviewRenderCandidate {
                id: 5,
                dirty: true,
                last_render_at: Some(due),
            },
        ];

        let locked_tabs =
            select_due_overview_tile_ids(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL, 2);

        assert_eq!(locked_tabs, vec![3, 4]);
        assert_eq!(locked_tabs.len(), 2, "lock_count");
    }

    /// Tabs mirrored by the overview are almost always occluded (behind the
    /// overview window itself or in a native tab group); their tiles must
    /// still be selected for rendering (REQ-OV-4). Candidates carry no
    /// occlusion input at all, so a dirty+due tile from a fully hidden source
    /// window is selected like any other.
    #[test]
    fn tiles_from_occluded_source_windows_are_still_selected_when_dirty_and_due() {
        let now = Instant::now();
        let hidden_source = OverviewRenderCandidate {
            id: 7,
            dirty: true,
            last_render_at: None,
        };

        let selected = select_due_overview_tile_ids(
            &[hidden_source],
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            2,
        );

        assert_eq!(selected, vec![7]);
    }

    #[test]
    fn backlog_decision_schedules_a_delayed_wake_when_only_throttled_tiles_remain_dirty() {
        let now = Instant::now();
        let last_render_at = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let candidates = [OverviewRenderCandidate {
            id: 1,
            dirty: true,
            last_render_at: Some(last_render_at),
        }];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(!decision.request_immediate_redraw);
        assert_eq!(
            decision.wake_at,
            Some(last_render_at + OVERVIEW_TILE_MIN_RENDER_INTERVAL)
        );
    }

    #[test]
    fn backlog_decision_requests_immediate_redraw_when_a_due_dirty_tile_survives_the_frame_cap() {
        let now = Instant::now();
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: true,
                last_render_at: Some(now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: true,
                last_render_at: Some(due),
            },
        ];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(decision.request_immediate_redraw);
        assert_eq!(decision.wake_at, None);
    }

    #[test]
    fn backlog_decision_requests_nothing_when_every_tile_is_clean() {
        let now = Instant::now();
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: false,
                last_render_at: Some(now),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: false,
                last_render_at: None,
            },
        ];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(!decision.request_immediate_redraw);
        assert_eq!(decision.wake_at, None);
    }

    #[test]
    fn budget_exceeded_degrades_live_tiles_to_placeholders() {
        assert_eq!(
            overview_tile_mode_for_budget(false),
            OverviewTileMode::LiveThumbnail
        );
        assert_eq!(
            overview_tile_mode_for_budget(true),
            OverviewTileMode::Placeholder
        );
    }

    #[test]
    fn device_lost_and_surface_lost_require_resource_regeneration() {
        assert!(!overview_regen_required(OverviewResourceEvent::None));
        assert!(overview_regen_required(OverviewResourceEvent::DeviceLost));
        assert!(overview_regen_required(OverviewResourceEvent::SurfaceLost));
    }

    #[test]
    fn overview_tile_labels_follow_source_tab_titles() {
        let labels = overview_tile_labels(&[1_u8, 2, 3], |id| match id {
            1 => Some("build".to_string()),
            2 => Some("tests".to_string()),
            _ => None,
        });

        assert_eq!(
            labels,
            vec![
                OverviewTileLabel {
                    id: 1,
                    label: "build".to_string()
                },
                OverviewTileLabel {
                    id: 2,
                    label: "tests".to_string()
                },
                OverviewTileLabel {
                    id: 3,
                    label: "Noa".to_string()
                }
            ]
        );
    }

    #[test]
    fn overview_placeholder_source_ids_is_the_tail_beyond_the_live_cap() {
        let source_ids = [1_u8, 2, 3, 4, 5];

        assert_eq!(overview_placeholder_source_ids(&source_ids, 3), &[4_u8, 5]);
        assert_eq!(
            overview_placeholder_source_ids(&source_ids, 5),
            &[] as &[u8]
        );
        assert_eq!(
            overview_placeholder_source_ids(&source_ids, 8),
            &[] as &[u8]
        );
    }

    #[test]
    fn sanitize_placeholder_label_strips_control_chars_and_clamps_to_max_cols() {
        assert_eq!(sanitize_placeholder_label("build", 10), "build");
        assert_eq!(sanitize_placeholder_label("build", 3), "bui");
        assert_eq!(
            sanitize_placeholder_label("build\x07\x1b[31m", 20),
            "build[31m"
        );
        assert_eq!(sanitize_placeholder_label("", 10), "");
        assert_eq!(sanitize_placeholder_label("build", 0), "");
    }

    #[test]
    fn overview_grid_applies_gutter_and_margin_offsets() {
        let layout = compute_overview_grid(4, BOUNDS, OVERVIEW_GRID_CAP, 6, 4);

        assert_eq!(
            layout.tiles,
            vec![
                TileRect::new(14, 24, 38, 53),
                TileRect::new(58, 24, 38, 53),
                TileRect::new(14, 83, 38, 53),
                TileRect::new(58, 83, 38, 53),
            ]
        );
        assert_equal_tile_size(&layout.tiles);
        assert_no_overlap(&layout.tiles);
    }

    #[test]
    fn overview_grid_with_production_gutter_margin_keeps_equal_size_and_no_overlap() {
        let layout = compute_overview_grid(
            5,
            BOUNDS,
            OVERVIEW_GRID_CAP,
            OVERVIEW_TILE_GUTTER,
            OVERVIEW_OUTER_MARGIN,
        );

        assert_equal_tile_size(&layout.tiles);
        assert_no_overlap(&layout.tiles);
        for tile in &layout.tiles {
            assert!(tile.x >= BOUNDS.x + OVERVIEW_OUTER_MARGIN, "{tile:?}");
            assert!(tile.y >= BOUNDS.y + OVERVIEW_OUTER_MARGIN, "{tile:?}");
        }
    }

    #[test]
    fn move_overview_selection_moves_within_a_row_major_grid() {
        // 3x3 grid (cols=3), 9 tiles, starting at the center (index 4).
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Left), 3);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Right), 5);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Up), 1);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Down), 7);
    }

    #[test]
    fn move_overview_selection_clamps_at_grid_edges_without_wrapping() {
        // Top-left corner: Left/Up are no-ops.
        assert_eq!(move_overview_selection(0, 3, 9, Direction::Left), 0);
        assert_eq!(move_overview_selection(0, 3, 9, Direction::Up), 0);
        // Bottom-right corner: Right/Down are no-ops.
        assert_eq!(move_overview_selection(8, 3, 9, Direction::Right), 8);
        assert_eq!(move_overview_selection(8, 3, 9, Direction::Down), 8);
    }

    /// Chosen policy for a trailing row shorter than `cols` (REQ-OV-3): a
    /// move that would land past `tile_count` simply doesn't move, rather
    /// than snapping sideways to the last tile.
    #[test]
    fn move_overview_selection_does_not_move_into_a_missing_trailing_row_cell() {
        // 5 tiles, cols=3: row 0 = [0,1,2], row 1 = [3,4] (index 5 is missing).
        assert_eq!(move_overview_selection(2, 3, 5, Direction::Down), 2);
        assert_eq!(move_overview_selection(4, 3, 5, Direction::Right), 4);
        // Moves that stay within the short row still work.
        assert_eq!(move_overview_selection(3, 3, 5, Direction::Right), 4);
        assert_eq!(move_overview_selection(4, 3, 5, Direction::Left), 3);
    }

    #[test]
    fn move_overview_selection_handles_an_empty_grid_without_panicking() {
        assert_eq!(move_overview_selection(0, 0, 0, Direction::Right), 0);
    }

    #[test]
    fn overview_initial_selection_prefers_the_focused_live_tile() {
        let source_ids = [10_u8, 11, 12, 13, 14];
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&12)), 2);
    }

    #[test]
    fn overview_initial_selection_falls_back_to_zero_when_focused_is_overflow_or_absent() {
        let source_ids = [10_u8, 11, 12, 13, 14];
        // Focused tab exists but sits past the live tile cap (overflow row).
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&14)), 0);
        // No focused tab at all.
        assert_eq!(overview_initial_selection::<u8>(&source_ids, 3, None), 0);
        // Focused tab isn't a source tab at all.
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&99)), 0);
    }

    // --- v3 paging (REQ-OV-18/19/20) ----------------------------------------

    #[test]
    fn overview_page_count_computes_ceil_division_with_a_floor_of_one() {
        assert_eq!(
            overview_page_count(0, 9),
            1,
            "an empty source still has one page"
        );
        assert_eq!(overview_page_count(9, 9), 1);
        assert_eq!(overview_page_count(10, 9), 2);
        assert_eq!(overview_page_count(25, 9), 3);
    }

    #[test]
    fn overview_page_slice_range_partitions_the_source_with_no_overlap_or_gap() {
        let len = 25;
        let page_size = 9;
        let page_count = overview_page_count(len, page_size);

        let mut covered = Vec::new();
        for page in 0..page_count {
            let range = overview_page_slice_range(len, page_size, page);
            assert!(range.len() <= page_size, "page {page} exceeds page_size");
            covered.extend(range);
        }
        covered.sort_unstable();
        assert_eq!(
            covered,
            (0..len).collect::<Vec<_>>(),
            "every source index must appear exactly once across all pages"
        );
    }

    #[test]
    fn clamp_overview_page_clamps_an_over_range_page_to_the_last_page() {
        // 10 items at 9/page = pages [0, 1]; page 5 is past the end.
        assert_eq!(clamp_overview_page(5, 10, 9), 1);
        assert_eq!(clamp_overview_page(0, 10, 9), 0);
        // An empty source still has exactly one (empty) page: page 0.
        assert_eq!(clamp_overview_page(3, 0, 9), 0);
    }

    #[test]
    fn page_step_clamps_at_both_ends_without_wrapping() {
        // 25 items at 9/page = pages [0, 1, 2].
        assert_eq!(
            page_step(0, -1, 25, 9),
            0,
            "back from the first page stays put"
        );
        assert_eq!(
            page_step(2, 1, 25, 9),
            2,
            "forward from the last page stays put"
        );
        assert_eq!(page_step(1, 1, 25, 9), 2);
        assert_eq!(page_step(1, -1, 25, 9), 0);
    }

    #[test]
    fn page_after_wheel_accumulates_below_threshold_without_flipping() {
        let (page, accum) = page_after_wheel(0, 0.0, WHEEL_PAGE_THRESHOLD / 3.0, 25, 9);
        assert_eq!(page, 0);
        assert_eq!(accum, WHEEL_PAGE_THRESHOLD / 3.0);
    }

    // Sign convention (mirrors `mouse_wheel_viewport_scroll`'s
    // `delta_y > 0.0 => Up`): a negative accumulated delta steps forward.
    #[test]
    fn page_after_wheel_flips_one_page_and_carries_the_remainder_on_crossing() {
        let already = -(WHEEL_PAGE_THRESHOLD - 20.0);
        let (page, accum) = page_after_wheel(0, already, -50.0, 25, 9);
        assert_eq!(page, 1);
        assert_eq!(accum, already - 50.0 + WHEEL_PAGE_THRESHOLD);
    }

    // Trackpad `PixelDelta` gestures can deliver one oversized sample; it must
    // still flip only one page, never several, per call.
    #[test]
    fn page_after_wheel_never_flips_more_than_one_page_per_call() {
        let (page, _) = page_after_wheel(0, 0.0, -(WHEEL_PAGE_THRESHOLD * 10.0), 25, 9);
        assert_eq!(page, 1);
    }

    // Regression (Radar fix 2): the carried remainder after a flip must be
    // bounded to a magnitude less than the threshold, or a single oversized
    // sample (10x threshold here) leaves enough carry that the very next
    // call — even with a near-zero delta and no further user input — flips
    // again on its own.
    #[test]
    fn page_after_wheel_oversized_single_delta_does_not_cascade_into_a_second_flip() {
        let (page, accum) = page_after_wheel(0, 0.0, -(WHEEL_PAGE_THRESHOLD * 10.0), 25, 9);
        assert_eq!(page, 1, "first call: exactly one flip");
        assert!(
            accum.abs() < WHEEL_PAGE_THRESHOLD,
            "carry must be bounded below the threshold, got {accum}"
        );

        let (page_again, _) = page_after_wheel(page, accum, -0.01, 25, 9);
        assert_eq!(
            page_again, page,
            "second call: a tiny delta must not flip again"
        );
    }

    #[test]
    fn page_after_wheel_saturates_at_both_ends_and_resets_the_accumulator() {
        // Already at page 0: crossing "back" again stays at 0, and the
        // accumulator resets rather than carrying a remainder that would
        // otherwise snap forward several pages the instant the user reverses
        // direction.
        let (page, accum) = page_after_wheel(0, 0.0, WHEEL_PAGE_THRESHOLD, 25, 9);
        assert_eq!(page, 0);
        assert_eq!(accum, 0.0);

        // Already at the last page (index 2 of 3): crossing "forward" again
        // stays put and likewise resets.
        let (page, accum) = page_after_wheel(2, 0.0, -WHEEL_PAGE_THRESHOLD, 25, 9);
        assert_eq!(page, 2);
        assert_eq!(accum, 0.0);
    }

    // Regression (Radar fix 1): `App::show_tab_overview` assigns this seam's
    // output — instead of a bare literal — in the same unconditional
    // REQ-OV-14 block that resets `page` on every show, so any residue left
    // over from before the overlay was last hidden (the re-host branch and
    // `hide_tab_overview` both leave `wheel_accum` untouched on their own)
    // can never survive into a reopen.
    #[test]
    fn overview_wheel_accum_always_resets_to_zero_on_show() {
        assert_eq!(overview_wheel_accum_on_show(), 0.0);
    }

    #[test]
    fn overview_key_action_resolves_page_navigation_keys() {
        let no_mods = ModifiersState::empty();
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::PageDown), no_mods),
            Some(OverviewAction::PageForward)
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::PageUp), no_mods),
            Some(OverviewAction::PageBack)
        );

        let cmd = ModifiersState::SUPER;
        assert_eq!(
            overview_key_action(&Key::Character("]".into()), cmd),
            Some(OverviewAction::PageForward)
        );
        assert_eq!(
            overview_key_action(&Key::Character("[".into()), cmd),
            Some(OverviewAction::PageBack)
        );
        // No Cmd held: not part of the Overview keymap (a plain `[`/`]`
        // types into the "Search sessions" field instead).
        assert_eq!(
            overview_key_action(&Key::Character("]".into()), no_mods),
            None
        );
        // A shifted combo does not misfire (mirrors the Cmd+digit modifier
        // discipline tested above).
        assert_eq!(
            overview_key_action(&Key::Character("]".into()), cmd | ModifiersState::SHIFT),
            None
        );
    }

    #[test]
    fn overview_key_action_resolves_arrows_return_and_escape() {
        let no_mods = ModifiersState::empty();
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowLeft), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Left))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowRight), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Right))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowUp), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Up))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowDown), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Down))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Enter), no_mods),
            Some(OverviewAction::Activate)
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Escape), no_mods),
            Some(OverviewAction::Dismiss)
        );
    }

    #[test]
    fn overview_key_action_resolves_plain_cmd_digit_to_switch_to_live() {
        let cmd = ModifiersState::SUPER;
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), cmd),
            Some(OverviewAction::SwitchToLive(1))
        );
        assert_eq!(
            overview_key_action(&Key::Character("9".into()), cmd),
            Some(OverviewAction::SwitchToLive(9))
        );
        // Outside the 1..=9 keybind range.
        assert_eq!(overview_key_action(&Key::Character("0".into()), cmd), None);
        // A shifted combo does not misfire (mirrors the `cmd+1`..`cmd+9`
        // keybind chords, which likewise require no other modifier).
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), cmd | ModifiersState::SHIFT),
            None
        );
        // No Cmd held: not part of the Overview keymap.
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn overview_key_action_ignores_unbound_keys() {
        assert_eq!(
            overview_key_action(&Key::Character("a".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn overview_tab_filter_matches_case_insensitive_contiguous_substrings() {
        let titles = [
            (1_u32, "Build Log".to_string()),
            (2, "logs-worker".to_string()),
            (3, "README".to_string()),
        ];

        assert_eq!(overview_tab_filter("log", &titles), vec![1, 2]);
        assert_eq!(overview_tab_filter("LOG", &titles), vec![1, 2]);
        // Non-contiguous query does not match (distinct from subsequence
        // search, e.g. `command_palette::fuzzy_match`).
        assert!(overview_tab_filter("lg", &titles).is_empty());
        // Empty query matches everything, source order preserved.
        assert_eq!(overview_tab_filter("", &titles), vec![1, 2, 3]);
    }

    #[test]
    fn overview_close_hit_test_only_matches_the_title_bar_corner() {
        let tile = TileRect::new(0, 0, 100, 80);
        let tiles = [(1_u8, tile)];
        let metrics = OverviewMetrics::new(1.0);
        let close_rect = overview_close_button_rect(tile, metrics);

        let inside_close = Point::new(close_rect.x + 1, close_rect.y + 1);
        let inside_body = Point::new(10, 50);

        assert_eq!(
            overview_close_hit_test(&tiles, inside_close, metrics),
            Some(1)
        );
        assert_eq!(overview_close_hit_test(&tiles, inside_body, metrics), None);
        assert_eq!(
            overview_close_hit_test(&tiles, Point::new(1000, 1000), metrics),
            None
        );
    }

    #[test]
    fn overview_search_field_text_shows_placeholder_only_when_empty() {
        assert_eq!(overview_search_field_text(""), OVERVIEW_SEARCH_PLACEHOLDER);
        assert_eq!(overview_search_field_text("log"), "log");
    }

    #[test]
    fn overview_search_field_row_adds_search_affordance_and_clips() {
        assert_eq!(overview_search_field_row("", 20), "  ⌕  Search sessions");
        assert_eq!(overview_search_field_row("build", 20), "  ⌕  build");
        assert_eq!(overview_search_field_row("abcdef", 6), "  ⌕  a");
        assert_eq!(overview_search_field_row("build", 0), "");
    }

    #[test]
    fn overview_escape_action_clears_a_query_before_dismissing() {
        // Two-stage: a non-empty query is cleared first, an empty one dismisses.
        assert_eq!(
            overview_escape_action("log"),
            OverviewEscapeAction::ClearSearch
        );
        assert_eq!(overview_escape_action(""), OverviewEscapeAction::Dismiss);
    }

    #[test]
    fn title_bar_row_pins_close_glyph_to_the_last_column() {
        // 10 cols: 9-wide centered label field + the trailing close glyph.
        let row = title_bar_row_with_close("build", 10);
        assert_eq!(row.chars().count(), 10);
        assert_eq!(row.chars().next_back(), Some(TITLE_BAR_CLOSE_GLYPH));
        assert!(row.contains("build"));

        // A label wider than the field is clipped, but the glyph still shows.
        let clipped = title_bar_row_with_close("a-very-long-tab-title", 6);
        assert_eq!(clipped.chars().count(), 6);
        assert_eq!(clipped.chars().next_back(), Some(TITLE_BAR_CLOSE_GLYPH));

        // Degenerate widths never panic.
        assert_eq!(title_bar_row_with_close("build", 0), "");
        assert_eq!(
            title_bar_row_with_close("build", 1),
            TITLE_BAR_CLOSE_GLYPH.to_string()
        );
    }

    /// Strip SGR escapes so the ANSI composer's visible layout can be compared
    /// against the plain composer's.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    // The ANSI composer's visible cells match the plain composer (centered
    // label + pinned glyph), and the styling segments land where expected.
    #[test]
    fn title_bar_row_ansi_matches_plain_layout_and_styles_segments() {
        // No badge/dot/query: visible layout identical to the plain composer.
        let plain = title_bar_row_with_close("build", 12);
        let ansi = title_bar_row_ansi("build", 12, None, None, "");
        // Both are 12 visible cells with the same centered label.
        assert_eq!(strip_sgr(&ansi).trim_end(), plain.trim_end());

        // Badge: a leading `n ` inside the visible field, dim-colored.
        let badged = title_bar_row_ansi("build", 14, Some(3), None, "");
        let visible = strip_sgr(&badged);
        assert!(visible.contains("3 build"), "{visible:?}");
        assert_eq!(visible.chars().count(), 14);

        // Dot: the `● ` needs-user prefix picks up the caller's color.
        let red = noa_core::Rgb::new(0xe8, 0x5d, 0x5d);
        let dotted = title_bar_row_ansi("● build", 14, None, Some(red), "");
        assert!(dotted.contains("\x1b[38;2;232;93;93m●"), "{dotted:?}");

        // Query: the first case-insensitive match is bold+accented.
        let hit = title_bar_row_ansi("Build Log", 20, None, None, "log");
        assert!(hit.contains("\x1b[1m"), "{hit:?}");
        assert!(strip_sgr(&hit).contains("Build Log"));
        // A non-matching query changes nothing visible.
        let miss = title_bar_row_ansi("Build Log", 20, None, None, "zzz");
        assert!(!miss.contains("\x1b[1m"));

        // Degenerate widths never panic.
        assert_eq!(title_bar_row_ansi("build", 0, Some(1), None, "b"), "");
        assert_eq!(
            title_bar_row_ansi("build", 1, Some(1), None, "b"),
            TITLE_BAR_CLOSE_GLYPH.to_string()
        );
    }

    // Tab-zoom rect: scaled up from the tile, clamped to the grid bounds, and
    // centered within them.
    #[test]
    fn overview_zoom_rect_scales_clamps_and_centers() {
        let grid = TileRect::new(10, 20, 400, 300);
        let tile = TileRect::new(10, 20, 100, 80);
        let zoom = overview_zoom_rect(grid, tile);
        assert_eq!((zoom.w, zoom.h), (160, 128));
        assert_eq!(zoom.x, 10 + (400 - 160) / 2);
        assert_eq!(zoom.y, 20 + (300 - 128) / 2);

        // A tile whose zoom would overflow clamps to the grid bounds.
        let big = TileRect::new(0, 0, 390, 290);
        let clamped = overview_zoom_rect(grid, big);
        assert_eq!((clamped.w, clamped.h), (400, 300));
        assert_eq!((clamped.x, clamped.y), (10, 20));

        // Degenerate bounds pass through without division by zero.
        let empty = TileRect::new(0, 0, 0, 0);
        assert_eq!(overview_zoom_rect(empty, tile), empty);
    }

    #[test]
    fn chrome_bands_reserve_search_and_hint_and_keep_grid_in_between() {
        let bounds = TileRect::new(0, 0, 800, 600);
        let chrome = overview_chrome_bands(bounds, OverviewMetrics::new(1.0));

        // Search band pinned to the top, hint band to the bottom.
        assert_eq!(
            chrome.search_band,
            TileRect::new(0, 0, 800, OVERVIEW_SEARCH_BAND_H)
        );
        assert_eq!(
            chrome.hint_band,
            TileRect::new(0, 600 - OVERVIEW_HINT_BAND_H, 800, OVERVIEW_HINT_BAND_H)
        );
        // Grid sits between them, full width, no overlap, no gap.
        assert_eq!(chrome.grid_bounds.x, 0);
        assert_eq!(chrome.grid_bounds.y, OVERVIEW_SEARCH_BAND_H);
        assert_eq!(chrome.grid_bounds.w, 800);
        assert_eq!(
            chrome.grid_bounds.h,
            600 - OVERVIEW_SEARCH_BAND_H - OVERVIEW_HINT_BAND_H
        );
        // The three bands exactly tile the bounds vertically.
        assert_eq!(
            chrome.search_band.h + chrome.grid_bounds.h + chrome.hint_band.h,
            600
        );
    }

    #[test]
    fn chrome_pill_rects_center_inside_reserved_bands() {
        let bounds = TileRect::new(0, 0, 800, 600);
        let chrome = overview_chrome_bands(bounds, OverviewMetrics::new(1.0));

        assert_eq!(
            overview_search_field_rect(chrome.search_band, OverviewMetrics::new(1.0)),
            TileRect::new(256, 15, 288, 34)
        );
        assert_eq!(
            overview_hint_bar_rect(chrome.hint_band, OverviewMetrics::new(1.0)),
            TileRect::new(208, 557, 384, 32)
        );
    }

    #[test]
    fn chrome_bands_clamp_without_underflow_in_a_short_window() {
        // Window shorter than the search band alone: grid + hint collapse to
        // zero height, nothing underflows.
        let chrome = overview_chrome_bands(TileRect::new(0, 0, 100, 20), OverviewMetrics::new(1.0));
        assert_eq!(chrome.search_band.h, 20);
        assert_eq!(chrome.grid_bounds.h, 0);
        assert_eq!(chrome.hint_band.h, 0);
    }

    #[test]
    fn hint_bar_text_substitutes_the_live_tile_count() {
        assert_eq!(
            overview_hint_bar_text(6, 0, 1),
            "⌘1-6 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
        assert_eq!(
            overview_hint_bar_text(9, 0, 1),
            "⌘1-9 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
        // Never renders "1-0": a zero-tile overview still shows "1-1".
        assert_eq!(
            overview_hint_bar_text(0, 0, 1),
            "⌘1-1 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
    }

    // v3 paging (REQ-OV-19): a single page (`page_count <= 1`) renders
    // exactly as before paging existed; more than one page appends a
    // trailing "Page p/N" segment, 1-indexed for display.
    #[test]
    fn hint_bar_text_appends_page_segment_only_when_there_is_more_than_one_page() {
        assert_eq!(
            overview_hint_bar_text(6, 0, 1),
            "⌘1-6 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close",
            "a single page must render identically to pre-paging text"
        );
        assert_eq!(
            overview_hint_bar_text(9, 0, 3),
            "⌘1-9 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close・Page 1/3"
        );
        assert_eq!(
            overview_hint_bar_text(9, 2, 3),
            "⌘1-9 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close・Page 3/3"
        );
    }

    #[test]
    fn hint_bar_ascii_fallback_mirrors_the_unicode_range() {
        assert_eq!(
            overview_hint_bar_text_ascii(6, 0, 1),
            "cmd+1-6 to switch / arrows to navigate / return to open / tab to zoom / esc to close"
        );
        assert_eq!(
            overview_hint_bar_text_ascii(6, 1, 2),
            "cmd+1-6 to switch / arrows to navigate / return to open / tab to zoom / esc to close / Page 2/2"
        );
    }

    #[test]
    fn overview_metrics_scale_design_values_by_dpr() {
        let m1 = OverviewMetrics::new(1.0);
        assert_eq!(m1.title_bar_h, OVERVIEW_TITLE_BAR_H);
        assert_eq!(m1.search_band_h, OVERVIEW_SEARCH_BAND_H);
        assert_eq!(m1.card_focus_width, OVERVIEW_CARD_FOCUS_WIDTH);

        let m2 = OverviewMetrics::new(2.0);
        assert_eq!(m2.title_bar_h, OVERVIEW_TITLE_BAR_H * 2);
        assert_eq!(m2.tile_gutter, OVERVIEW_TILE_GUTTER * 2);
        assert_eq!(m2.search_field_max_w, OVERVIEW_SEARCH_FIELD_MAX_W * 2);
        assert_eq!(m2.hint_bar_max_w, OVERVIEW_HINT_BAR_MAX_W * 2);
        assert_eq!(m2.card_focus_width, OVERVIEW_CARD_FOCUS_WIDTH * 2.0);

        // Degenerate scales fall back to 1.0 rather than collapsing layout.
        assert_eq!(OverviewMetrics::new(0.0), m1);
        assert_eq!(OverviewMetrics::new(f32::NAN), m1);
    }

    #[test]
    fn label_padding_centers_the_row_and_scales_the_inset() {
        let pad = overview_label_padding(60, 24.0, 2.0);
        assert_eq!(pad.top, 18.0);
        assert_eq!(pad.left, 20.0);
        assert_eq!(pad.right, 20.0);
        assert_eq!(pad.bottom, 0.0);
        // A band shorter than the cell clamps to 0 instead of going negative.
        assert_eq!(overview_label_padding(10, 24.0, 1.0).top, 0.0);
    }

    #[test]
    fn hint_bar_row_prefers_full_text_then_compact_and_never_overflows() {
        // Wide enough: the full sentence, centered.
        let full = overview_hint_bar_row(6, 0, 1, 120);
        assert!(full.contains("to switch"));
        assert!(text_cell_width(&full) <= 120);

        // Too narrow for the full sentence (75 cells): the compact variant.
        let compact = overview_hint_bar_row(6, 0, 1, 65);
        assert!(compact.contains("⌘1-6 switch"));
        assert!(!compact.contains("to switch"));
        assert!(text_cell_width(&compact) <= 65);

        // Narrower than even the compact variant: hard head-anchored clip —
        // the row feeds a single-row `Terminal`, where overflow wraps and
        // scrolls, leaving only the sentence's tail visible.
        let clipped = overview_hint_bar_row(6, 0, 1, 10);
        assert!(clipped.starts_with("⌘1-6"));
        assert!(text_cell_width(&clipped) <= 10);
    }

    // v3 paging: the row-composing seam also carries the "Page p/N" segment
    // through both the full and compact variants, still hard-clipped to `cols`.
    #[test]
    fn hint_bar_row_includes_page_segment_when_paged() {
        let full = overview_hint_bar_row(6, 1, 2, 120);
        assert!(full.contains("Page 2/2"));
        assert!(text_cell_width(&full) <= 120);

        // Narrow enough to force the compact variant (full+page is well over
        // 75 cells) but still wide enough for compact+page (~70 cells) to
        // survive the hard clip intact.
        let compact = overview_hint_bar_row(6, 1, 2, 75);
        assert!(
            !compact.contains("to switch"),
            "expected the compact variant"
        );
        assert!(compact.contains("Page 2/2"));
        assert!(text_cell_width(&compact) <= 75);
    }

    #[test]
    fn overview_search_field_row_clips_by_cell_width_not_chars() {
        // The "  ⌕  " affordance is 5 cells; each CJK char is 2 cells, so 8
        // columns hold exactly one query char without wrapping the row.
        assert_eq!(overview_search_field_row("日本語", 8), "  ⌕  日");
    }

    #[test]
    fn overview_key_action_resolves_tab_to_zoom() {
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Tab), ModifiersState::empty()),
            Some(OverviewAction::ToggleZoom)
        );
    }

    #[test]
    fn center_label_pads_to_center_and_passes_overflow_through() {
        assert_eq!(center_label("ab", 6), "  ab");
        assert_eq!(center_label("abc", 3), "abc");
        // Wider than the field: returned unpadded (renderer clips it).
        assert_eq!(center_label("abcdef", 3), "abcdef");
    }

    fn assert_equal_tile_size(tiles: &[TileRect]) {
        let Some(first) = tiles.first() else {
            return;
        };

        for tile in tiles {
            assert_eq!((tile.w, tile.h), (first.w, first.h), "{tile:?}");
        }
    }

    fn assert_row_major(tiles: &[TileRect], cols: usize) {
        for (index, tile) in tiles.iter().enumerate() {
            let col = index % cols;
            let row = index / cols;
            let first = tiles[0];
            assert_eq!(tile.x, first.x + first.w * col as u32, "{tile:?}");
            assert_eq!(tile.y, first.y + first.h * row as u32, "{tile:?}");
        }
    }

    fn assert_no_overlap(tiles: &[TileRect]) {
        for (index, a) in tiles.iter().enumerate() {
            for b in tiles.iter().skip(index + 1) {
                assert!(!rects_overlap(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }

    fn rects_overlap(a: TileRect, b: TileRect) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }
}
