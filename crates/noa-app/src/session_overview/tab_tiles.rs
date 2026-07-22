//! Pure geometry, hit-testing, and drop-resolution for TAB-UNIT Overview
//! tiles (Overview U1-U4).
//!
//! The Overview grid lays out one tile per *tab* (window), and each tile
//! reproduces that tab's internal split layout by compositing its panes at
//! scaled sub-rects. This module owns the window-, terminal-, pty-, and
//! GPU-free math for that: where each pane sits inside a tile
//! ([`tab_tile_pane_rects`]), which pane a point falls on
//! ([`tab_tile_pane_at_point`]), which 60/40 zone within a pane a drop
//! resolves to ([`classify_pane_zone`], shared verbatim with the main-view
//! pane drag), and what a release ultimately does ([`resolve_overview_drop`]).
//!
//! Everything here is pure and `Copy`-friendly so the whole interaction can be
//! unit-tested without constructing app runtime objects (a `winit::WindowId`
//! is not even constructible in the offline test environment).

use crate::split_tree::{Direction, PaneId, Point, Rect as TileRect, SplitTree, compute_layout};

/// The content region of a tab tile — the tile minus its top title band —
/// where the tab's panes are composited (U1). A tile shorter than its title
/// band degenerates to a zero-height content rect rather than underflowing.
pub fn tab_tile_content_rect(tile: TileRect, title_bar_h: u32) -> TileRect {
    let bar = title_bar_h.min(tile.h);
    TileRect::new(tile.x, tile.y + bar, tile.w, tile.h - bar)
}

/// Each pane's scaled sub-rect within a tab tile's content region (U1): the
/// tab's `SplitTree` laid out into `content` via the very same
/// [`compute_layout`] the live window uses, so the thumbnail's divider
/// positions and pane proportions match the real tab exactly (just scaled).
/// Returned in tree order, pairing each pane id with its rect.
pub fn tab_tile_pane_rects(content: TileRect, tree: &SplitTree) -> Vec<(PaneId, TileRect)> {
    compute_layout(tree, content)
}

/// Hit-test a point against a tab tile's scaled pane sub-rects (U1): the pane
/// whose sub-rect contains `point`, or `None` outside every pane (a divider
/// gap, or outside the content region). `pane_rects` is
/// [`tab_tile_pane_rects`]' output for the tile the point already landed in.
pub fn tab_tile_pane_at_point(pane_rects: &[(PaneId, TileRect)], point: Point) -> Option<PaneId> {
    pane_rects
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(pane, _)| *pane)
}

/// Which zone of a pane a drop point falls in (pane-dnd L2(c)/AC-4): the 60/40
/// vocabulary shared by the main-view pane drag (`app::pane_drag`) and the
/// Overview in-tile / cross-tab drops. The center 60% (20% margin per side) is
/// [`PaneZone::Center`]; the outer 40% band resolves to whichever edge the
/// point sits nearest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneZone {
    Center,
    Edge(Direction),
}

/// Classify `point` against a pane's `bounds` into the 60/40 center/edge zones
/// (pane-dnd L2(c)/AC-4/ASSUME-5). Degenerates to `Center` for a zero-size
/// rect (never occurs for a laid-out pane, but this must never divide by
/// zero). The Overview maps a tile-local cursor into a pane's scaled sub-rect
/// and calls this with the same vocabulary the live pane drag uses.
pub fn classify_pane_zone(bounds: TileRect, point: Point) -> PaneZone {
    if bounds.w == 0 || bounds.h == 0 {
        return PaneZone::Center;
    }
    const EDGE_MARGIN: f32 = 0.2;
    let fx = point.x.saturating_sub(bounds.x) as f32 / bounds.w as f32;
    let fy = point.y.saturating_sub(bounds.y) as f32 / bounds.h as f32;
    let left = fx;
    let right = 1.0 - fx;
    let top = fy;
    let bottom = 1.0 - fy;
    // `min()` always returns one of its exact inputs (no intermediate
    // arithmetic), so the following equality checks are exact, not a
    // float-tolerance comparison.
    let min_margin = left.min(right).min(top).min(bottom);
    if min_margin >= EDGE_MARGIN {
        PaneZone::Center
    } else if min_margin == left {
        PaneZone::Edge(Direction::Left)
    } else if min_margin == right {
        PaneZone::Edge(Direction::Right)
    } else if min_margin == top {
        PaneZone::Edge(Direction::Up)
    } else {
        PaneZone::Edge(Direction::Down)
    }
}

/// Where an Overview pane drag resolves to on release (U2/U3/U4). Generic over
/// the window/pane id types so it is unit-testable without a live `WindowId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewDrop<W, P> {
    /// In-tab center-zone drop (U2): swap the two panes within the tab
    /// (`swap_pane_with_zoom`).
    Swap { window: W, source: P, target: P },
    /// In-tab edge-zone drop (U2): split-insert the source pane at the target
    /// pane's `direction`-side edge (`move_pane_with_zoom`).
    Split {
        window: W,
        source: P,
        target: P,
        direction: Direction,
    },
    /// Cross-tab drop onto a pane in another tab (U3): move the pane there via
    /// `move_pane_to_tab_at`, split-inserting at the target pane's `direction`
    /// edge — an edge zone keeps its direction, a center zone inserts to the
    /// target's right (`Direction::Right`; cross-tab swap is out, the engine is
    /// one-way).
    CrossTab {
        source_window: W,
        source: P,
        dest_window: W,
        target: P,
        direction: Direction,
    },
    /// No move — a self-drop (the source pane's own tile/pane), a foreign
    /// window group, or a release over no pane at all.
    Cancel,
}

/// Resolve an Overview pane-drag drop (pure, unit-tested — U2/U3/U4).
///
/// `over` names the pane the release landed on: its window/tab, the pane id,
/// and the 60/40 zone the pointer sits in within that pane's scaled tile rect
/// ([`classify_pane_zone`]). `None` — a release over no pane (an empty tile
/// gap, the chrome bands, outside the grid) — cancels.
///
/// `same_group` says whether the target tab is in the *same* window group as
/// the source; it is consulted only for a cross-tab drop, since the engine
/// (`App::move_pane_to_tab_at`) rejects a cross-group move (AC-29), so a
/// foreign-group target cancels here too — the front-line half of that guard.
///
/// An in-tab drop onto the source's own pane cancels regardless of zone (a
/// pane never swaps or splits with itself); a cross-tab drop can never target
/// the source pane, since `PaneId`s are process-global and the target lives in
/// a different window.
pub fn resolve_overview_drop<W: PartialEq + Copy, P: PartialEq + Copy>(
    source_window: W,
    source_pane: P,
    over: Option<(W, P, PaneZone)>,
    same_group: bool,
) -> OverviewDrop<W, P> {
    let Some((window, target, zone)) = over else {
        return OverviewDrop::Cancel;
    };
    if window == source_window {
        // In-tab rearrange (U2). A drop onto the source's own pane is a no-op
        // regardless of zone.
        if target == source_pane {
            return OverviewDrop::Cancel;
        }
        return match zone {
            PaneZone::Center => OverviewDrop::Swap {
                window,
                source: source_pane,
                target,
            },
            PaneZone::Edge(direction) => OverviewDrop::Split {
                window,
                source: source_pane,
                target,
                direction,
            },
        };
    }
    // Cross-tab (U3): only within the same window group.
    if !same_group {
        return OverviewDrop::Cancel;
    }
    let direction = match zone {
        PaneZone::Center => Direction::Right,
        PaneZone::Edge(direction) => direction,
    };
    OverviewDrop::CrossTab {
        source_window,
        source: source_pane,
        dest_window: window,
        target,
        direction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_tree::{SplitOrientation, SplitTree};

    fn pane(n: u64) -> PaneId {
        PaneId::new(n)
    }

    // U1: a tab tile's content region drops the title band off the top.
    #[test]
    fn content_rect_reserves_the_title_band() {
        let tile = TileRect::new(10, 20, 200, 150);
        assert_eq!(tab_tile_content_rect(tile, 30), TileRect::new(10, 50, 200, 120));
        // A tile shorter than the band degenerates to zero height, never
        // underflows.
        let short = TileRect::new(0, 0, 100, 20);
        assert_eq!(tab_tile_content_rect(short, 30), TileRect::new(0, 20, 100, 0));
    }

    // U1: a single-pane tab fills the whole content region; hit-testing
    // anywhere in it resolves to that pane, and outside it to `None`.
    #[test]
    fn single_pane_tab_fills_content_and_hit_tests() {
        let content = TileRect::new(0, 30, 200, 120);
        let tree = SplitTree::leaf(pane(1));
        let rects = tab_tile_pane_rects(content, &tree);
        assert_eq!(rects, vec![(pane(1), content)]);
        assert_eq!(
            tab_tile_pane_at_point(&rects, Point::new(100, 90)),
            Some(pane(1))
        );
        // Above the content region (in the title band) hits no pane.
        assert_eq!(tab_tile_pane_at_point(&rects, Point::new(100, 10)), None);
    }

    // U1: a horizontal split places its two panes side by side inside the
    // content region, and a point in each half resolves to the right pane.
    #[test]
    fn split_tab_hit_tests_each_pane() {
        let content = TileRect::new(0, 30, 200, 120);
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(pane(1)),
            SplitTree::leaf(pane(2)),
        );
        let rects = tab_tile_pane_rects(content, &tree);
        assert_eq!(rects.len(), 2);
        // Left pane on the left, right pane on the right.
        assert_eq!(tab_tile_pane_at_point(&rects, Point::new(20, 90)), Some(pane(1)));
        assert_eq!(tab_tile_pane_at_point(&rects, Point::new(180, 90)), Some(pane(2)));
    }

    // pane-dnd AC-4: the 60/40 coordinate table, one axis varied at a time —
    // identical to the main-view classifier this shares its vocabulary with.
    #[test]
    fn classify_pane_zone_matches_l2c_coordinate_table() {
        let bounds = TileRect::new(0, 0, 100, 100);
        assert_eq!(classify_pane_zone(bounds, Point::new(50, 50)), PaneZone::Center);
        assert_eq!(
            classify_pane_zone(bounds, Point::new(10, 50)),
            PaneZone::Edge(Direction::Left)
        );
        assert_eq!(
            classify_pane_zone(bounds, Point::new(90, 50)),
            PaneZone::Edge(Direction::Right)
        );
        assert_eq!(
            classify_pane_zone(bounds, Point::new(50, 10)),
            PaneZone::Edge(Direction::Up)
        );
        assert_eq!(
            classify_pane_zone(bounds, Point::new(50, 90)),
            PaneZone::Edge(Direction::Down)
        );
    }

    #[test]
    fn classify_pane_zone_center_for_zero_size_rect() {
        assert_eq!(classify_pane_zone(TileRect::new(0, 0, 0, 0), Point::new(0, 0)), PaneZone::Center);
    }

    // U2: an in-tab center drop swaps; an in-tab edge drop splits at that edge.
    #[test]
    fn in_tab_center_swaps_and_edge_splits() {
        let swap = resolve_overview_drop(1u64, 10u64, Some((1u64, 20u64, PaneZone::Center)), true);
        assert_eq!(
            swap,
            OverviewDrop::Swap {
                window: 1,
                source: 10,
                target: 20
            }
        );
        let split = resolve_overview_drop(
            1u64,
            10u64,
            Some((1u64, 20u64, PaneZone::Edge(Direction::Down))),
            true,
        );
        assert_eq!(
            split,
            OverviewDrop::Split {
                window: 1,
                source: 10,
                target: 20,
                direction: Direction::Down
            }
        );
    }

    // U2: an in-tab drop onto the source's own pane cancels, for both zones.
    #[test]
    fn in_tab_self_pane_drop_cancels() {
        assert_eq!(
            resolve_overview_drop(1u64, 10u64, Some((1u64, 10u64, PaneZone::Center)), true),
            OverviewDrop::Cancel
        );
        assert_eq!(
            resolve_overview_drop(
                1u64,
                10u64,
                Some((1u64, 10u64, PaneZone::Edge(Direction::Left))),
                true
            ),
            OverviewDrop::Cancel
        );
    }

    // U3: a cross-tab edge drop keeps its direction; a cross-tab center drop
    // inserts to the target's right (swap is out — one-way engine).
    #[test]
    fn cross_tab_edge_keeps_direction_center_inserts_right() {
        let edge = resolve_overview_drop(
            1u64,
            10u64,
            Some((2u64, 20u64, PaneZone::Edge(Direction::Up))),
            true,
        );
        assert_eq!(
            edge,
            OverviewDrop::CrossTab {
                source_window: 1,
                source: 10,
                dest_window: 2,
                target: 20,
                direction: Direction::Up
            }
        );
        let center = resolve_overview_drop(1u64, 10u64, Some((2u64, 20u64, PaneZone::Center)), true);
        assert_eq!(
            center,
            OverviewDrop::CrossTab {
                source_window: 1,
                source: 10,
                dest_window: 2,
                target: 20,
                direction: Direction::Right
            }
        );
    }

    // U3/AC-29: a cross-tab drop onto a foreign window group cancels.
    #[test]
    fn cross_tab_foreign_group_cancels() {
        assert_eq!(
            resolve_overview_drop(1u64, 10u64, Some((2u64, 20u64, PaneZone::Center)), false),
            OverviewDrop::Cancel
        );
    }

    // U4: a release over no pane at all cancels.
    #[test]
    fn drop_over_no_pane_cancels() {
        assert_eq!(
            resolve_overview_drop(1u64, 10u64, None, true),
            OverviewDrop::Cancel
        );
    }
}
