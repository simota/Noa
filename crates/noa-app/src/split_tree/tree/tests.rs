use crate::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};

use super::ops::contains_pane;
use super::resize::{ChildSide, RESIZE_LAYOUT_BOUNDS};
use super::types::DEFAULT_SPLIT_RATIO;
use super::*;

fn assert_horizontal_tiling(bounds: Rect, first: Rect, second: Rect) {
    assert_eq!(first.x, bounds.x);
    assert_eq!(first.y, bounds.y);
    assert_eq!(first.h, bounds.h);
    assert_eq!(second.y, bounds.y);
    assert_eq!(second.h, bounds.h);
    assert_eq!(first.right() + DIVIDER_WIDTH_PX, second.x);
    assert_eq!(second.right(), bounds.right());
}

fn assert_vertical_tiling(bounds: Rect, first: Rect, second: Rect) {
    assert_eq!(first.x, bounds.x);
    assert_eq!(first.y, bounds.y);
    assert_eq!(first.w, bounds.w);
    assert_eq!(second.x, bounds.x);
    assert_eq!(second.w, bounds.w);
    assert_eq!(first.bottom() + DIVIDER_WIDTH_PX, second.y);
    assert_eq!(second.bottom(), bounds.bottom());
}

fn rect_for(layout: &[(PaneId, Rect)], pane: PaneId) -> Rect {
    layout
        .iter()
        .find(|(candidate, _)| *candidate == pane)
        .map(|(_, rect)| *rect)
        .unwrap()
}

fn ratio_at(tree: &SplitTree, path: &[ChildSide]) -> f32 {
    let mut current = tree;
    for side in path {
        let SplitTree::Split { first, second, .. } = current else {
            panic!("path did not resolve to a split node");
        };
        current = match side {
            ChildSide::First => first,
            ChildSide::Second => second,
        };
    }

    let SplitTree::Split { ratio, .. } = current else {
        panic!("path did not resolve to a split node");
    };
    *ratio
}

fn assert_all_panes_at_or_above_floor(layout: &[(PaneId, Rect)]) {
    for (pane, rect) in layout {
        assert!(
            rect.w >= MIN_PANE_SIZE_PX,
            "pane {} width {} is below floor",
            pane.get(),
            rect.w
        );
        assert!(
            rect.h >= MIN_PANE_SIZE_PX,
            "pane {} height {} is below floor",
            pane.get(),
            rect.h
        );
    }
}

#[test]
fn equal_split_children_are_equal_with_odd_remainder_to_first() {
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(PaneId::new(1)),
        SplitTree::leaf(PaneId::new(2)),
    );
    let bounds = Rect::new(0, 0, 100, 40);

    let layout = compute_layout(&tree, bounds);
    let left = Rect::new(0, 0, 50, 40);
    let right = Rect::new(51, 0, 49, 40);

    assert_eq!(
        layout,
        vec![(PaneId::new(1), left), (PaneId::new(2), right)]
    );
    assert_horizontal_tiling(bounds, left, right);
    assert_eq!(left.w, 50);
    assert_eq!(right.w, 49);
}

#[test]
fn split_left_places_new_pane_on_the_left_and_focus_target_first() {
    let existing = PaneId::new(1);
    let new_pane = PaneId::new(2);
    let mut tree = SplitTree::leaf(existing);

    assert!(split_pane_in_direction(
        &mut tree,
        existing,
        new_pane,
        Direction::Left,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 100, 40));
    assert_eq!(
        layout,
        vec![
            (new_pane, Rect::new(0, 0, 50, 40)),
            (existing, Rect::new(51, 0, 49, 40)),
        ]
    );
}

#[test]
fn split_up_places_new_pane_above_the_existing_pane() {
    let existing = PaneId::new(1);
    let new_pane = PaneId::new(2);
    let mut tree = SplitTree::leaf(existing);

    assert!(split_pane_in_direction(
        &mut tree,
        existing,
        new_pane,
        Direction::Up,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 100, 40));
    assert_eq!(
        layout,
        vec![
            (new_pane, Rect::new(0, 0, 100, 20)),
            (existing, Rect::new(0, 21, 100, 19)),
        ]
    );
}

#[test]
fn adding_right_to_existing_horizontal_group_rebalances_the_row() {
    let left = PaneId::new(1);
    let middle = PaneId::new(2);
    let right = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::leaf(middle),
    );

    assert!(split_pane_in_direction(
        &mut tree,
        middle,
        right,
        Direction::Right,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 100, 40));
    assert_eq!(rect_for(&layout, left), Rect::new(0, 0, 33, 40));
    assert_eq!(rect_for(&layout, middle), Rect::new(34, 0, 33, 40));
    assert_eq!(rect_for(&layout, right), Rect::new(68, 0, 32, 40));
}

#[test]
fn adding_down_to_existing_vertical_group_rebalances_the_column() {
    let top = PaneId::new(1);
    let middle = PaneId::new(2);
    let bottom = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::leaf(top),
        SplitTree::leaf(middle),
    );

    assert!(split_pane_in_direction(
        &mut tree,
        middle,
        bottom,
        Direction::Down,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 80, 100));
    assert_eq!(rect_for(&layout, top), Rect::new(0, 0, 80, 33));
    assert_eq!(rect_for(&layout, middle), Rect::new(0, 34, 80, 33));
    assert_eq!(rect_for(&layout, bottom), Rect::new(0, 68, 80, 32));
}

#[test]
fn adding_right_to_rectangular_grid_rebuilds_the_target_row() {
    let top_left = PaneId::new(1);
    let bottom_left = PaneId::new(2);
    let top_right = PaneId::new(3);
    let bottom_right = PaneId::new(4);
    let added = PaneId::new(5);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_left),
            SplitTree::leaf(bottom_left),
        ),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );

    assert!(split_pane_in_direction(
        &mut tree,
        top_right,
        added,
        Direction::Right,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 120, 80));
    assert_eq!(rect_for(&layout, top_left), Rect::new(0, 0, 40, 40));
    assert_eq!(rect_for(&layout, top_right), Rect::new(41, 0, 39, 40));
    assert_eq!(rect_for(&layout, added), Rect::new(81, 0, 39, 40));
    assert_eq!(rect_for(&layout, bottom_left), Rect::new(0, 41, 60, 39));
    assert_eq!(rect_for(&layout, bottom_right), Rect::new(61, 41, 59, 39));
}

#[test]
fn adding_down_to_rectangular_grid_rebuilds_the_target_column() {
    let top_left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_left = PaneId::new(3);
    let bottom_right = PaneId::new(4);
    let added = PaneId::new(5);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(top_left),
            SplitTree::leaf(top_right),
        ),
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(bottom_left),
            SplitTree::leaf(bottom_right),
        ),
    );

    assert!(split_pane_in_direction(
        &mut tree,
        top_right,
        added,
        Direction::Down,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 120, 90));
    assert_eq!(rect_for(&layout, top_left), Rect::new(0, 0, 60, 45));
    assert_eq!(rect_for(&layout, bottom_left), Rect::new(0, 46, 60, 44));
    assert_eq!(rect_for(&layout, top_right), Rect::new(61, 0, 59, 30));
    assert_eq!(rect_for(&layout, added), Rect::new(61, 31, 59, 29));
    assert_eq!(rect_for(&layout, bottom_right), Rect::new(61, 61, 59, 29));
}

#[test]
fn adding_beyond_three_panes_in_one_axis_is_rejected() {
    let first = PaneId::new(1);
    let second = PaneId::new(2);
    let third = PaneId::new(3);
    let fourth = PaneId::new(4);
    let mut row = SplitTree::split(
        SplitOrientation::Horizontal,
        1.0 / 3.0,
        SplitTree::leaf(first),
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(second),
            SplitTree::leaf(third),
        ),
    );
    let before = row.clone();

    assert!(!can_add_pane_in_direction(&row, third, Direction::Right));
    assert!(!split_pane_in_direction(
        &mut row,
        third,
        fourth,
        Direction::Right,
    ));
    assert_eq!(row, before);

    let mut column = SplitTree::split(
        SplitOrientation::Vertical,
        1.0 / 3.0,
        SplitTree::leaf(first),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(second),
            SplitTree::leaf(third),
        ),
    );
    let before = column.clone();

    assert!(!can_add_pane_in_direction(&column, third, Direction::Down));
    assert!(!split_pane_in_direction(
        &mut column,
        third,
        fourth,
        Direction::Down,
    ));
    assert_eq!(column, before);
}

#[test]
fn adding_across_axis_boundary_stays_local_to_the_focused_pane() {
    let top_left = PaneId::new(1);
    let bottom_left = PaneId::new(2);
    let right = PaneId::new(3);
    let added = PaneId::new(4);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_left),
            SplitTree::leaf(bottom_left),
        ),
        SplitTree::leaf(right),
    );

    assert!(split_pane_in_direction(
        &mut tree,
        top_left,
        added,
        Direction::Right,
    ));

    let layout = compute_layout(&tree, Rect::new(0, 0, 100, 60));
    assert_eq!(rect_for(&layout, top_left), Rect::new(0, 0, 25, 30));
    assert_eq!(rect_for(&layout, added), Rect::new(26, 0, 24, 30));
    assert_eq!(rect_for(&layout, bottom_left), Rect::new(0, 31, 50, 29));
    assert_eq!(rect_for(&layout, right), Rect::new(51, 0, 49, 60));
}

#[test]
fn odd_width_layout_tiles_without_gap_or_overlap() {
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(PaneId::new(1)),
        SplitTree::leaf(PaneId::new(2)),
    );
    let bounds = Rect::new(10, 4, 100, 24);

    let left = Rect::new(10, 4, 50, 24);
    let right = Rect::new(61, 4, 49, 24);
    let layout = compute_layout(&tree, bounds);

    assert_eq!(
        layout,
        vec![(PaneId::new(1), left), (PaneId::new(2), right)]
    );
    assert_horizontal_tiling(bounds, left, right);
    assert_eq!(left.w + DIVIDER_WIDTH_PX + right.w, bounds.w);
    assert_eq!(left.w, right.w + 1);
}

#[test]
fn nested_ratios_are_preserved_across_window_sizes() {
    let tree = SplitTree::split(
        SplitOrientation::Horizontal,
        0.25,
        SplitTree::leaf(PaneId::new(1)),
        SplitTree::split(
            SplitOrientation::Vertical,
            0.75,
            SplitTree::leaf(PaneId::new(2)),
            SplitTree::leaf(PaneId::new(3)),
        ),
    );
    let small_bounds = Rect::new(0, 0, 401, 301);
    let large_bounds = Rect::new(0, 0, 801, 601);

    let small = compute_layout(&tree, small_bounds);
    let large = compute_layout(&tree, large_bounds);

    let small_left = Rect::new(0, 0, 100, 301);
    let small_top_right = Rect::new(101, 0, 300, 225);
    let small_bottom_right = Rect::new(101, 226, 300, 75);
    let large_left = Rect::new(0, 0, 200, 601);
    let large_top_right = Rect::new(201, 0, 600, 450);
    let large_bottom_right = Rect::new(201, 451, 600, 150);

    assert_eq!(
        small,
        vec![
            (PaneId::new(1), small_left),
            (PaneId::new(2), small_top_right),
            (PaneId::new(3), small_bottom_right),
        ]
    );
    assert_eq!(
        large,
        vec![
            (PaneId::new(1), large_left),
            (PaneId::new(2), large_top_right),
            (PaneId::new(3), large_bottom_right),
        ]
    );

    assert_horizontal_tiling(
        small_bounds,
        small_left,
        Rect::new(
            small_top_right.x,
            small_top_right.y,
            small_top_right.w,
            small_top_right.h + DIVIDER_WIDTH_PX + small_bottom_right.h,
        ),
    );
    assert_vertical_tiling(
        Rect::new(101, 0, 300, 301),
        small_top_right,
        small_bottom_right,
    );
    assert_eq!(small_left.w, 100);
    assert_eq!(small_top_right.h, 225);
    assert_eq!(small_bottom_right.h, 75);

    assert_horizontal_tiling(
        large_bounds,
        large_left,
        Rect::new(
            large_top_right.x,
            large_top_right.y,
            large_top_right.w,
            large_top_right.h + DIVIDER_WIDTH_PX + large_bottom_right.h,
        ),
    );
    assert_vertical_tiling(
        Rect::new(201, 0, 600, 601),
        large_top_right,
        large_bottom_right,
    );
    assert_eq!(large_left.w, 200);
    assert_eq!(large_top_right.h, 450);
    assert_eq!(large_bottom_right.h, 150);
}

#[test]
fn focus_in_direction_uses_overlap_tie_breaks_and_layout_edges() {
    let top_left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_left = PaneId::new(3);
    let bottom_right = PaneId::new(4);
    let grid = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(top_left),
            SplitTree::leaf(top_right),
        ),
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(bottom_left),
            SplitTree::leaf(bottom_right),
        ),
    );

    let left = PaneId::new(10);
    let small_top = PaneId::new(11);
    let large_bottom = PaneId::new(12);
    let nested_unequal = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split(
            SplitOrientation::Vertical,
            0.25,
            SplitTree::leaf(small_top),
            SplitTree::leaf(large_bottom),
        ),
    );

    let tie_top = PaneId::new(21);
    let tie_bottom = PaneId::new(22);
    let nested_tie = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(tie_top),
            SplitTree::leaf(tie_bottom),
        ),
    );

    let cases = [
        (
            "2x2 right from top-left",
            &grid,
            top_left,
            Direction::Right,
            Some(top_right),
        ),
        (
            "top edge has no upward pane",
            &grid,
            top_left,
            Direction::Up,
            None,
        ),
        (
            "nested right move chooses greatest overlap",
            &nested_unequal,
            left,
            Direction::Right,
            Some(large_bottom),
        ),
        (
            "equal-overlap right move chooses top-most",
            &nested_tie,
            left,
            Direction::Right,
            Some(tie_top),
        ),
    ];

    for (name, tree, focused, direction, expected) in cases {
        assert_eq!(
            focus_in_direction(tree, focused, direction),
            expected,
            "{name}"
        );
    }
}

#[test]
fn hit_test_prioritizes_divider_hit_zone_then_pane() {
    let left = PaneId::new(1);
    let right = PaneId::new(2);
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::leaf(right),
    );
    let layout = compute_layout(&tree, Rect::new(0, 0, 100, 20));

    let cases = [
        (
            "divider footprint",
            Point::new(50, 10),
            Some(HitTarget::Divider),
        ),
        (
            "right pane edge within divider hit zone",
            Point::new(55, 10),
            Some(HitTarget::Divider),
        ),
        (
            "right pane one pixel beyond divider hit zone",
            Point::new(56, 10),
            Some(HitTarget::Pane(right)),
        ),
        (
            "left pane edge within divider hit zone",
            Point::new(45, 10),
            Some(HitTarget::Divider),
        ),
        (
            "left pane one pixel beyond divider hit zone",
            Point::new(44, 10),
            Some(HitTarget::Pane(left)),
        ),
    ];

    for (name, point, expected) in cases {
        assert_eq!(hit_test(&layout, point), expected, "{name}");
    }
}

#[test]
fn split_resize_drag_moves_horizontal_divider_to_pointer_and_clamps() {
    let left = PaneId::new(1);
    let right = PaneId::new(2);
    let bounds = Rect::new(0, 0, 100, 20);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::leaf(right),
    );

    assert!(split_resize_drag_target_at_point(&tree, bounds, Point::new(10, 10)).is_none());
    let drag = split_resize_drag_target_at_point(&tree, bounds, Point::new(50, 10)).unwrap();
    resize_split_to_drag_point(&mut tree, &drag, Point::new(60, 10));
    let layout = compute_layout(&tree, bounds);
    assert_eq!(rect_for(&layout, left).w, 60);
    assert_eq!(rect_for(&layout, right).w, 39);

    resize_split_to_drag_point(&mut tree, &drag, Point::new(0, 10));
    let layout = compute_layout(&tree, bounds);
    assert_eq!(rect_for(&layout, left).w, MIN_PANE_SIZE_PX);
    assert_all_panes_at_or_above_floor(&layout);

    resize_split_to_drag_point(&mut tree, &drag, Point::new(500, 10));
    let layout = compute_layout(&tree, bounds);
    assert_eq!(rect_for(&layout, right).w, MIN_PANE_SIZE_PX);
    assert_all_panes_at_or_above_floor(&layout);
}

#[test]
fn split_resize_drag_targets_nested_vertical_divider() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let bounds = Rect::new(0, 0, 101, 101);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );

    let drag = split_resize_drag_target_at_point(&tree, bounds, Point::new(75, 50)).unwrap();
    resize_split_to_drag_point(&mut tree, &drag, Point::new(75, 70));
    let layout = compute_layout(&tree, bounds);
    assert_eq!(rect_for(&layout, left).w, 50);
    assert_eq!(rect_for(&layout, top_right).h, 70);
    assert_eq!(rect_for(&layout, bottom_right).h, 30);
}

#[test]
fn resize_split_steps_two_leaf_boundary_and_clamps_at_pane_floor() {
    let left = PaneId::new(1);
    let right = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::leaf(right),
    );

    resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
    let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
    assert_eq!(rect_for(&layout, left).w, 510);
    assert_eq!(rect_for(&layout, right).w, 490);

    resize_split(&mut tree, right, Direction::Left, SPLIT_RESIZE_STEP_PX);
    let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
    assert_eq!(rect_for(&layout, left).w, 500);
    assert_eq!(rect_for(&layout, right).w, 500);

    for _ in 0..200 {
        resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
    }
    let clamped = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
    assert_eq!(rect_for(&clamped, right).w, MIN_PANE_SIZE_PX);
    assert_all_panes_at_or_above_floor(&clamped);

    resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
    assert_eq!(compute_layout(&tree, RESIZE_LAYOUT_BOUNDS), clamped);
}

#[test]
fn resize_split_uses_nearest_matching_ancestor_and_noops_without_one() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );

    resize_split(&mut tree, top_right, Direction::Down, SPLIT_RESIZE_STEP_PX);
    let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
    assert_eq!(rect_for(&layout, top_right).h, 510);
    assert_eq!(rect_for(&layout, bottom_right).h, 490);
    assert_eq!(rect_for(&layout, left).w, 500);

    resize_split(&mut tree, top_right, Direction::Left, SPLIT_RESIZE_STEP_PX);
    let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
    assert_eq!(rect_for(&layout, left).w, 490);
    assert_eq!(rect_for(&layout, top_right).w, 510);
    assert_eq!(rect_for(&layout, bottom_right).w, 510);

    let before_noop = tree.clone();
    resize_split(&mut tree, top_right, Direction::Right, SPLIT_RESIZE_STEP_PX);
    assert_eq!(tree, before_noop);
}

#[test]
fn skewed_nested_ratios_all_equalize_to_half() {
    let mut tree = SplitTree::split(
        SplitOrientation::Horizontal,
        0.2,
        SplitTree::leaf(PaneId::new(1)),
        SplitTree::split(
            SplitOrientation::Vertical,
            0.8,
            SplitTree::leaf(PaneId::new(2)),
            SplitTree::leaf(PaneId::new(3)),
        ),
    );

    equalize(&mut tree);

    assert_eq!(ratio_at(&tree, &[]), DEFAULT_SPLIT_RATIO);
    assert_eq!(ratio_at(&tree, &[ChildSide::Second]), DEFAULT_SPLIT_RATIO);
}

#[test]
fn close_pane_two_leaf_relayouts_survivor_without_closing_tab() {
    let left = PaneId::new(1);
    let right = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::leaf(right),
    );
    let bounds = Rect::new(0, 0, 100, 40);

    let outcome = close_pane(&mut tree, left);

    assert_eq!(
        outcome,
        CloseOutcome {
            next_focus: Some(right),
            tab_should_close: false,
        }
    );
    assert_eq!(compute_layout(&tree, bounds), vec![(right, bounds)]);
}

#[test]
fn close_pane_three_leaf_picks_sibling_and_last_leaf_closes_tab() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );

    let outcome = close_pane(&mut tree, top_right);

    assert_eq!(
        outcome,
        CloseOutcome {
            next_focus: Some(bottom_right),
            tab_should_close: false,
        }
    );
    assert!(!contains_pane(&tree, top_right));
    assert!(contains_pane(&tree, left));
    assert!(contains_pane(&tree, bottom_right));

    let mut single = SplitTree::leaf(left);
    assert_eq!(
        close_pane(&mut single, left),
        CloseOutcome {
            next_focus: None,
            tab_should_close: true,
        }
    );
}

#[test]
fn zoom_toggle_filters_draw_list_but_resizes_every_pane() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );
    let bounds = Rect::new(0, 0, 101, 81);
    let layout = compute_layout(&tree, bounds);

    let zoomed = zoom_toggle(&tree, None, top_right, bounds);

    assert_eq!(zoomed.zoomed, Some(top_right));
    assert_eq!(zoomed.draw_panes, vec![top_right]);
    assert_eq!(zoomed.resize_targets.len(), 3);
    assert_eq!(rect_for(&zoomed.resize_targets, top_right), bounds);
    assert_eq!(
        rect_for(&zoomed.resize_targets, left),
        rect_for(&layout, left)
    );
    assert_eq!(
        rect_for(&zoomed.resize_targets, bottom_right),
        rect_for(&layout, bottom_right)
    );

    let unzoomed = zoom_toggle(&tree, Some(top_right), top_right, bounds);

    assert_eq!(unzoomed.zoomed, None);
    assert_eq!(unzoomed.draw_panes, vec![left, top_right, bottom_right]);
    assert_eq!(unzoomed.resize_targets, layout);
}

#[test]
fn closing_zoomed_pane_force_unzooms_before_removal() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );

    let outcome = close_pane_with_zoom(&mut tree, top_right, Some(top_right));

    assert_eq!(outcome.zoomed, None);
    assert_eq!(
        outcome.close_outcome,
        CloseOutcome {
            next_focus: Some(bottom_right),
            tab_should_close: false,
        }
    );
    assert!(!contains_pane(&tree, top_right));
    assert_eq!(
        zoom_decision(&tree, outcome.zoomed, Rect::new(0, 0, 100, 40)).draw_panes,
        vec![left, bottom_right]
    );
}

#[test]
fn zoom_resize_targets_use_full_bounds_for_zoomed_pane_and_tree_rects_for_hidden_panes() {
    let left = PaneId::new(1);
    let top_right = PaneId::new(2);
    let bottom_right = PaneId::new(3);
    let tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(left),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(top_right),
            SplitTree::leaf(bottom_right),
        ),
    );
    let resized_bounds = Rect::new(0, 0, 151, 91);
    let tree_targets = compute_layout(&tree, resized_bounds);

    let zoomed_targets = zoom_resize_targets(&tree, Some(top_right), resized_bounds);

    assert_eq!(rect_for(&zoomed_targets, top_right), resized_bounds);
    assert_eq!(
        rect_for(&zoomed_targets, left),
        rect_for(&tree_targets, left)
    );
    assert_eq!(
        rect_for(&zoomed_targets, bottom_right),
        rect_for(&tree_targets, bottom_right)
    );

    let unzoomed = zoom_toggle(&tree, Some(top_right), top_right, resized_bounds);
    assert_eq!(unzoomed.zoomed, None);
    assert_eq!(unzoomed.resize_targets, tree_targets);
}

#[test]
fn focus_switch_plan_commits_preedit_before_retargeting_ime() {
    let losing = PaneId::new(1);
    let winning = PaneId::new(2);

    assert_eq!(
        focus_switch_plan(losing, winning),
        vec![ImeOp::CommitPreedit(losing), ImeOp::RetargetIme(winning)]
    );
}

#[test]
fn pane_command_target_resolution_uses_focused_pane_for_terminal_commands() {
    let focused = Some(PaneId::new(42));
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
        AppCommand::CloseTab,
    ] {
        assert_eq!(resolve_pane_command_target(command, focused), focused);
    }

    for command in [
        AppCommand::About,
        AppCommand::Preferences,
        AppCommand::ReloadConfig,
        AppCommand::NewTab,
        AppCommand::ToggleFullscreen,
        AppCommand::ToggleTabOverview,
        AppCommand::SelectTab(1),
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::CloseWindow,
        AppCommand::Quit,
    ] {
        assert_eq!(resolve_pane_command_target(command, focused), None);
    }

    assert_eq!(resolve_pane_command_target(AppCommand::Copy, None), None);
}

// ---------------------------------------------------------------------
// Pane D&D (docs/specs/pane-dnd.md) — swap_pane / extract_pane / move_pane
// ---------------------------------------------------------------------

use super::ops::pane_ids;

#[test]
fn swap_pane_exchanges_leaf_ids_and_preserves_tree_shape() {
    let a = PaneId::new(1);
    let b = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(a),
        SplitTree::leaf(b),
    );

    assert!(swap_pane(&mut tree, a, b));

    assert_eq!(
        tree,
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(b),
            SplitTree::leaf(a),
        )
    );
}

#[test]
fn move_pane_edge_insert_direction_and_order_matches_axis_convention() {
    let moved = PaneId::new(1);
    let target = PaneId::new(2);

    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        // Seed `moved` and `target` as siblings on the *opposite* axis so
        // `move_pane`'s edge-insert exercises a fresh axis group for
        // `direction` rather than the adjacent-same-axis-group case (that
        // case has its own regression test below).
        let seed_orientation = match direction.split_orientation() {
            SplitOrientation::Horizontal => SplitOrientation::Vertical,
            SplitOrientation::Vertical => SplitOrientation::Horizontal,
        };
        let mut tree = SplitTree::split_even(
            seed_orientation,
            SplitTree::leaf(moved),
            SplitTree::leaf(target),
        );

        let outcome = move_pane(&mut tree, moved, target, direction, true).unwrap();
        assert!(!outcome.tab_should_close);

        // Directional correctness: moved always ends up on `direction`'s
        // side of target, regardless of tree child order.
        let layout = compute_layout(&tree, Rect::new(0, 0, 100, 100));
        let moved_rect = rect_for(&layout, moved);
        let target_rect = rect_for(&layout, target);
        let spatially_correct = match direction {
            Direction::Left => moved_rect.right() <= target_rect.x,
            Direction::Right => target_rect.right() <= moved_rect.x,
            Direction::Up => moved_rect.bottom() <= target_rect.y,
            Direction::Down => target_rect.bottom() <= moved_rect.y,
        };
        assert!(spatially_correct, "{direction:?}");

        // Insertion order: which child comes first in the tree must match
        // `places_new_split_before_existing()`, exactly like
        // `split_left_places_new_pane_on_the_left_and_focus_target_first`
        // above (AC-6).
        let SplitTree::Split {
            orientation, first, ..
        } = &tree
        else {
            panic!("{direction:?}: expected the reinserted tree to be a single split");
        };
        assert_eq!(*orientation, direction.split_orientation(), "{direction:?}");
        let SplitTree::Leaf { pane: first_pane } = first.as_ref() else {
            panic!("{direction:?}: expected a leaf first child");
        };
        let expected_first = if direction.places_new_split_before_existing() {
            moved
        } else {
            target
        };
        assert_eq!(*first_pane, expected_first, "{direction:?}");
    }
}

#[test]
fn move_pane_adjacent_in_same_axis_group_collapses_and_reinserts_correctly() {
    // Omen A3: `moved` and `target` are the only two siblings in this
    // horizontal group, so extracting `moved` collapses the split entirely
    // down to `target`'s own leaf before `target` gets re-split against
    // `moved` on `direction`'s side. Composition must resolve `target` by
    // value post-collapse, not by a stale pre-extraction path.
    let moved = PaneId::new(1);
    let target = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(moved),
        SplitTree::leaf(target),
    );

    let outcome = move_pane(&mut tree, moved, target, Direction::Right, true).unwrap();
    assert!(!outcome.tab_should_close);

    assert_eq!(
        tree,
        SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(target),
            SplitTree::leaf(moved),
        )
    );
}

#[test]
fn move_pane_axis_cap_exceeded_is_rejected_and_tree_unchanged() {
    let first = PaneId::new(1);
    let second = PaneId::new(2);
    let third = PaneId::new(3);
    let moved = PaneId::new(4);
    // A 3-pane row already at MAX_PANES_PER_AXIS, plus `moved` split off on
    // the other axis (mirrors `adding_beyond_three_panes_in_one_axis_is_rejected`).
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::split(
            SplitOrientation::Horizontal,
            1.0 / 3.0,
            SplitTree::leaf(first),
            SplitTree::split_even(
                SplitOrientation::Horizontal,
                SplitTree::leaf(second),
                SplitTree::leaf(third),
            ),
        ),
        SplitTree::leaf(moved),
    );
    let before = tree.clone();

    let result = move_pane(&mut tree, moved, third, Direction::Right, true);

    assert_eq!(result, Err(MoveError::MaxPanesExceeded));
    assert_eq!(tree, before);
}

#[test]
fn move_pane_per_tab_cap_rejected_via_tab_cap_ok_leaves_tree_unchanged() {
    // AC-27: the per-tab cap (MAX_PANES_PER_TAB=9) needs pixel geometry this
    // pure tree layer doesn't have (Omen A5) — the caller computes it and
    // passes `tab_cap_ok`. This is a fresh axis group, so the axis cap alone
    // would allow the insert; `tab_cap_ok=false` is what rejects it here.
    let moved = PaneId::new(1);
    let target = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::leaf(moved),
        SplitTree::leaf(target),
    );
    let before = tree.clone();

    let result = move_pane(&mut tree, moved, target, Direction::Right, false);

    assert_eq!(result, Err(MoveError::MaxPanesExceeded));
    assert_eq!(tree, before);
}

#[test]
fn self_swap_and_self_move_are_no_ops() {
    let pane = PaneId::new(1);
    let other = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(pane),
        SplitTree::leaf(other),
    );
    let before = tree.clone();

    assert!(!swap_pane(&mut tree, pane, pane));
    assert_eq!(tree, before);

    let result = move_pane(&mut tree, pane, pane, Direction::Right, true);
    assert_eq!(result, Err(MoveError::InvalidPanes));
    assert_eq!(tree, before);
}

#[test]
fn swap_and_move_with_nonexistent_pane_are_rejected() {
    let pane = PaneId::new(1);
    let other = PaneId::new(2);
    let missing = PaneId::new(99);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(pane),
        SplitTree::leaf(other),
    );
    let before = tree.clone();

    assert!(!swap_pane(&mut tree, pane, missing));
    assert_eq!(tree, before);

    let result = move_pane(&mut tree, pane, missing, Direction::Right, true);
    assert_eq!(result, Err(MoveError::InvalidPanes));
    assert_eq!(tree, before);

    let result = move_pane(&mut tree, missing, pane, Direction::Right, true);
    assert_eq!(result, Err(MoveError::InvalidPanes));
    assert_eq!(tree, before);
}

#[test]
fn swap_pane_with_zoom_force_unzooms_when_either_pane_was_zoomed() {
    let a = PaneId::new(1);
    let b = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(a),
        SplitTree::leaf(b),
    );

    let outcome = swap_pane_with_zoom(&mut tree, a, b, Some(a));
    assert!(outcome.swapped);
    assert_eq!(outcome.zoomed, None, "no dangling zoom target after swap");
}

// P2-3 (FR-6): a successful swap force-unzooms even when the zoomed pane is
// neither `a` nor `b` — a zoomed pane fills the whole tab, so `c`'s zoom
// would otherwise hide the (still real) swap between `a` and `b`. This test
// used to assert the opposite (`outcome.zoomed == Some(c)`, "unrelated zoom
// left intact"); that was the bug FR-6 requires fixing (zoom C, then a swap
// of A<->B was invisible behind C's zoom).
#[test]
fn swap_pane_with_zoom_force_unzooms_even_for_an_unrelated_zoom_target() {
    let a = PaneId::new(1);
    let b = PaneId::new(2);
    let c = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(a),
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(b),
            SplitTree::leaf(c),
        ),
    );

    let outcome = swap_pane_with_zoom(&mut tree, a, b, Some(c));
    assert!(outcome.swapped);
    assert_eq!(
        outcome.zoomed, None,
        "a successful swap unzooms unconditionally, regardless of which pane was zoomed"
    );
}

#[test]
fn swap_pane_with_zoom_rejected_self_swap_leaves_zoom_intact() {
    let a = PaneId::new(1);
    let b = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::leaf(a),
        SplitTree::leaf(b),
    );
    let before = tree.clone();

    let outcome = swap_pane_with_zoom(&mut tree, a, a, Some(a));
    assert!(!outcome.swapped);
    assert_eq!(
        outcome.zoomed,
        Some(a),
        "rejected op leaves zoom state untouched"
    );
    assert_eq!(tree, before);
}

#[test]
fn move_pane_with_zoom_force_unzooms_moved_or_target_before_transform() {
    let moved = PaneId::new(1);
    let target = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::leaf(moved),
        SplitTree::leaf(target),
    );

    let outcome = move_pane_with_zoom(
        &mut tree,
        moved,
        target,
        Direction::Right,
        true,
        Some(target),
    );
    assert!(outcome.move_result.is_ok());
    assert_eq!(outcome.zoomed, None, "no dangling zoom target after move");
}

// P2-3 (FR-6) companion to the swap-side test above: a successful move also
// force-unzooms when the zoomed pane is neither `moved` nor `target`.
#[test]
fn move_pane_with_zoom_force_unzooms_even_for_an_unrelated_zoom_target() {
    let moved = PaneId::new(1);
    let target = PaneId::new(2);
    let zoomed_elsewhere = PaneId::new(3);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Horizontal,
        SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::leaf(moved),
            SplitTree::leaf(target),
        ),
        SplitTree::leaf(zoomed_elsewhere),
    );

    let outcome = move_pane_with_zoom(
        &mut tree,
        moved,
        target,
        Direction::Right,
        true,
        Some(zoomed_elsewhere),
    );
    assert!(outcome.move_result.is_ok());
    assert_eq!(
        outcome.zoomed, None,
        "a successful move unzooms unconditionally, regardless of which pane was zoomed"
    );
}

#[test]
fn move_pane_with_zoom_rejected_op_leaves_zoom_intact_and_tree_unchanged() {
    let moved = PaneId::new(1);
    let target = PaneId::new(2);
    let mut tree = SplitTree::split_even(
        SplitOrientation::Vertical,
        SplitTree::leaf(moved),
        SplitTree::leaf(target),
    );
    let before = tree.clone();

    // moved == target -> InvalidPanes, rejected before any zoom mutation.
    let outcome = move_pane_with_zoom(&mut tree, moved, moved, Direction::Right, true, Some(moved));
    assert_eq!(outcome.move_result, Err(MoveError::InvalidPanes));
    assert_eq!(outcome.zoomed, Some(moved));
    assert_eq!(tree, before);
}

/// AC-23. The proptest crate is unavailable offline in this environment
/// (verified: no cached crate, no network access in this sandbox); a seeded
/// deterministic LCG (Numerical-Recipes constants) is used instead to
/// generate pseudo-random operation sequences. Every `Split` node holds
/// exactly 2 children by the `SplitTree` type's own definition, so "every
/// Split has 2+ children or is
/// collapsed" is a compiler-enforced invariant here, not something this test
/// needs to check by hand; what it does check is the set of live leaf
/// `PaneId`s against an independently tracked expected set, and that no
/// operation panics.
#[test]
fn dnd_operation_sequence_property_preserves_pane_id_set_and_never_panics() {
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn next_range(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }
    }

    const DIRECTIONS: [Direction; 4] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];

    for seed in [1u64, 42, 12345, 999_999] {
        let mut rng = Lcg(seed);
        let mut next_id = 2u64;
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(PaneId::new(0)),
            SplitTree::leaf(PaneId::new(1)),
        );
        let mut expected: Vec<PaneId> = vec![PaneId::new(0), PaneId::new(1)];

        for _ in 0..500 {
            let mut live = Vec::new();
            pane_ids(&tree, &mut live);
            live.sort_by_key(|pane| pane.get());
            let mut expected_sorted = expected.clone();
            expected_sorted.sort_by_key(|pane| pane.get());
            assert_eq!(
                live, expected_sorted,
                "seed {seed}: leaf PaneId set drifted from the tracked expected set"
            );

            match rng.next_range(4) {
                0 => {
                    // split: attach a fresh pane next to a random existing one.
                    let target = expected[rng.next_range(expected.len())];
                    let new_pane = PaneId::new(next_id);
                    next_id += 1;
                    let direction = DIRECTIONS[rng.next_range(4)];
                    if split_pane_in_direction(&mut tree, target, new_pane, direction) {
                        expected.push(new_pane);
                    }
                }
                1 => {
                    // close: remove a random existing pane, unless it is the
                    // last one (close_pane's own contract closes the tab
                    // instead of leaving an empty tree).
                    if expected.len() > 1 {
                        let index = rng.next_range(expected.len());
                        let pane = expected[index];
                        let outcome = close_pane(&mut tree, pane);
                        assert!(!outcome.tab_should_close);
                        expected.remove(index);
                    }
                }
                2 => {
                    // swap_pane: exchange two random existing panes (a == b
                    // is a valid, expected no-op input here).
                    let a = expected[rng.next_range(expected.len())];
                    let b = expected[rng.next_range(expected.len())];
                    swap_pane(&mut tree, a, b);
                }
                _ => {
                    // move_pane: relocate a random pane next to another;
                    // success or rejection, the tracked identity set never
                    // changes (moved is detached and reinserted, not
                    // recreated).
                    if expected.len() > 1 {
                        let moved_index = rng.next_range(expected.len());
                        let moved = expected[moved_index];
                        let mut target = expected[rng.next_range(expected.len())];
                        if target == moved {
                            target = expected[(moved_index + 1) % expected.len()];
                        }
                        let direction = DIRECTIONS[rng.next_range(4)];
                        let _ = move_pane(&mut tree, moved, target, direction, true);
                    }
                }
            }
        }
    }
}
