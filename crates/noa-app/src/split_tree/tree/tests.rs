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
        assert_eq!(resolve_pane_command_target(command, focused), focused);
    }

    for command in [
        AppCommand::About,
        AppCommand::Preferences,
        AppCommand::NewTab,
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
