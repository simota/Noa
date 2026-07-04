//! Pure Tab Overview layout and hit-test math.
//!
//! This module deliberately stays independent of windows, terminals, ptys, and
//! GPU state so overview behavior can be tested without constructing app
//! runtime objects.

pub use crate::split_tree::{Point, Rect as TileRect};
use std::time::{Duration, Instant};

/// Spec-locked maximum number of live thumbnail tiles in the overview grid.
pub const OVERVIEW_GRID_CAP: usize = 9;

/// Spec-locked 10Hz throttle for thumbnail regeneration.
pub const OVERVIEW_TILE_MIN_RENDER_INTERVAL: Duration = Duration::from_millis(100);

/// Per-frame cap for offscreen tile work. The render path is sequential, but
/// this keeps one overview frame from doing unbounded terminal locks.
pub const OVERVIEW_MAX_RENDER_TILES_PER_FRAME: usize = 2;

/// Pure layout result for the Tab Overview grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewLayout {
    pub cols: usize,
    pub rows: usize,
    pub placeholder_rows: usize,
    pub tiles: Vec<TileRect>,
    pub placeholders: Vec<TileRect>,
    pub overflow: bool,
}

/// Input row for pure thumbnail-regeneration selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRenderCandidate<Id> {
    pub id: Id,
    pub dirty: bool,
    pub last_render_at: Option<Instant>,
}

/// Title label associated with a live or placeholder overview tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewTileLabel<Id> {
    pub id: Id,
    pub label: String,
}

/// Rendering mode selected for an overview tile under resource pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewTileMode {
    LiveThumbnail,
    Placeholder,
}

/// Injected GPU lifecycle signal used by the resource-regeneration decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewResourceEvent {
    None,
    DeviceLost,
    SurfaceLost,
}

/// Compute equal-size row-major tile rectangles for the Tab Overview.
///
/// `cap` is part of the pure seam so tests can exercise the degradation
/// boundary directly; production uses [`OVERVIEW_GRID_CAP`].
pub fn compute_overview_grid(tab_count: usize, bounds: TileRect, cap: usize) -> OverviewLayout {
    let live_cap = cap.min(tab_count);
    let overflow_count = tab_count.saturating_sub(live_cap);
    let overflow = overflow_count > 0;

    if live_cap == 0 {
        return OverviewLayout {
            cols: 0,
            rows: 0,
            placeholder_rows: 0,
            tiles: Vec::new(),
            placeholders: Vec::new(),
            overflow,
        };
    }

    let cols = ceil_sqrt(live_cap);
    let rows = live_cap.div_ceil(cols);
    let placeholder_rows = if overflow {
        overflow_count.div_ceil(cols)
    } else {
        0
    };
    let total_rows = rows + placeholder_rows;
    let tile_w = bounds.w / cols as u32;
    let tile_h = bounds.h / total_rows as u32;

    let tiles = (0..live_cap)
        .map(|index| rect_at(bounds, tile_w, tile_h, cols, index))
        .collect();
    let placeholders = (0..overflow_count)
        .map(|index| rect_at(bounds, tile_w, tile_h, cols, live_cap + index))
        .collect();

    OverviewLayout {
        cols,
        rows,
        placeholder_rows,
        tiles,
        placeholders,
        overflow,
    }
}

/// Return the target id for `point`, or `None` outside live tiles.
///
/// Callers pass only live thumbnail tile pairs. Placeholder rows and empty grid
/// cells are therefore naturally non-interactive.
pub fn hit_test_overview_grid<T: Copy>(tiles: &[(T, TileRect)], point: Point) -> Option<T> {
    tiles
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(id, _)| *id)
}

/// Decide whether a single tile is dirty and outside the compile-time
/// regeneration throttle.
pub fn should_render_tile(
    dirty: bool,
    last_render_at: Option<Instant>,
    now: Instant,
    min_interval: Duration,
) -> bool {
    if !dirty {
        return false;
    }
    let Some(last_render_at) = last_render_at else {
        return true;
    };
    now.saturating_duration_since(last_render_at) >= min_interval
}

/// Select the dirty-and-due tile ids for one overview frame.
///
/// Source-window occlusion must NOT gate this selection: tabs mirrored in the
/// overview are almost always occluded (they sit behind the overview window
/// itself and/or in a macOS native tab group), so filtering them out would
/// leave every live tile permanently blank and defeat REQ-OV-4's live mirror.
/// REQ-NF-7's occlusion-aware redraw suppression is honored at the tab-window
/// redraw layer (`TargetedRedrawDecision`) instead, which the overview tile
/// path does not bypass.
pub fn select_due_overview_tile_ids<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
    max_tiles: usize,
) -> Vec<Id> {
    candidates
        .iter()
        .filter(|candidate| {
            should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval)
        })
        .take(max_tiles)
        .map(|candidate| candidate.id)
        .collect()
}

/// Outcome of the post-frame dirty-backlog check `redraw_overview` runs
/// after each Tab Overview frame (Fix A): either an immediate redraw is
/// warranted right now, or — if every remaining dirty tile is merely
/// throttle-blocked — the single instant at which the earliest one becomes
/// due, so the caller can schedule one delayed wake-up instead of spinning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewBacklogDecision {
    pub request_immediate_redraw: bool,
    pub wake_at: Option<Instant>,
}

/// Decide the post-frame backlog action from each source tile's dirty +
/// last-render state.
///
/// A tile only warrants `request_immediate_redraw` when it is dirty *and*
/// already due (i.e. [`should_render_tile`] would render it right now) —
/// that only happens when [`OVERVIEW_MAX_RENDER_TILES_PER_FRAME`] left it
/// un-rendered this frame. A tile that is merely dirty-but-throttled
/// contributes its throttle deadline (`last_render_at + min_interval`, or
/// `now` if it has never been rendered) to `wake_at`, and the earliest one
/// wins: one delayed wake-up covers every throttled tile, since a tile that
/// becomes due re-triggers this same check when it fires.
pub fn overview_backlog_decision<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
) -> OverviewBacklogDecision {
    let mut wake_at: Option<Instant> = None;
    for candidate in candidates {
        if !candidate.dirty {
            continue;
        }
        if should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval) {
            return OverviewBacklogDecision {
                request_immediate_redraw: true,
                wake_at: None,
            };
        }
        let due_at = candidate
            .last_render_at
            .map(|last_render_at| last_render_at + min_interval)
            .unwrap_or(now);
        wake_at = Some(wake_at.map_or(due_at, |current| current.min(due_at)));
    }
    OverviewBacklogDecision {
        request_immediate_redraw: false,
        wake_at,
    }
}

/// Decide the tile mode from an injected VRAM budget flag.
pub fn overview_tile_mode_for_budget(budget_exceeded: bool) -> OverviewTileMode {
    if budget_exceeded {
        OverviewTileMode::Placeholder
    } else {
        OverviewTileMode::LiveThumbnail
    }
}

/// Decide whether overview GPU resources must be regenerated.
pub fn overview_regen_required(event: OverviewResourceEvent) -> bool {
    matches!(
        event,
        OverviewResourceEvent::DeviceLost | OverviewResourceEvent::SurfaceLost
    )
}

/// Map source tabs to display labels using already-known tab titles.
pub fn overview_tile_labels<Id: Copy>(
    source_ids: &[Id],
    mut title_for_id: impl FnMut(Id) -> Option<String>,
) -> Vec<OverviewTileLabel<Id>> {
    source_ids
        .iter()
        .copied()
        .map(|id| OverviewTileLabel {
            id,
            label: title_for_id(id).unwrap_or_else(|| "noa".to_string()),
        })
        .collect()
}

/// Overflow window ids relegated to title-only placeholder rows (REQ-OV-10):
/// the tail of `source_ids` beyond the live tile cap. Index-parallel with
/// `OverviewLayout::placeholders` (both walk the same overflow ids in order).
pub fn overview_placeholder_source_ids<Id: Copy>(
    source_ids: &[Id],
    live_tile_count: usize,
) -> &[Id] {
    source_ids.get(live_tile_count..).unwrap_or(&[])
}

/// Sanitize a tab title for display in a single-row placeholder tile: tab
/// titles arrive via OSC 0/2 with no control-character filtering, and a
/// placeholder tile has no live mirror to clip an overlong string visually,
/// so this strips control characters and clamps to `max_cols` characters.
pub fn sanitize_placeholder_label(label: &str, max_cols: u16) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(max_cols as usize)
        .collect()
}

fn ceil_sqrt(n: usize) -> usize {
    let mut cols = 1;
    while cols * cols < n {
        cols += 1;
    }
    cols
}

fn rect_at(bounds: TileRect, tile_w: u32, tile_h: u32, cols: usize, index: usize) -> TileRect {
    let col = index % cols;
    let row = index / cols;
    TileRect::new(
        bounds.x + tile_w.saturating_mul(col as u32),
        bounds.y + tile_h.saturating_mul(row as u32),
        tile_w,
        tile_h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: TileRect = TileRect::new(10, 20, 90, 120);

    #[test]
    fn overview_grid_handles_zero_tabs_and_all_closed_as_empty() {
        let layout = compute_overview_grid(0, BOUNDS, OVERVIEW_GRID_CAP);

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
            let layout = compute_overview_grid(tab_count, BOUNDS, OVERVIEW_GRID_CAP);

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

    #[test]
    fn overview_grid_places_overflow_in_title_only_placeholder_rows() {
        let ten = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP);
        assert_eq!(ten.cols, 3);
        assert_eq!(ten.rows, 3);
        assert_eq!(ten.placeholder_rows, 1);
        assert_eq!(ten.tiles.len(), 9);
        assert_eq!(ten.placeholders.len(), 1);
        assert!(ten.overflow);
        assert_equal_tile_size(&ten.tiles);
        assert_eq!(ten.placeholders[0], TileRect::new(10, 110, 30, 30));

        let twelve = compute_overview_grid(12, BOUNDS, OVERVIEW_GRID_CAP);
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
        let layout = compute_overview_grid(5, BOUNDS, OVERVIEW_GRID_CAP);

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
        let layout = compute_overview_grid(8, BOUNDS, OVERVIEW_GRID_CAP);
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
        let layout = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP);
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
                    label: "noa".to_string()
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
