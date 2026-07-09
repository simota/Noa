use super::types::{
    DEFAULT_SPLIT_RATIO, Direction, MAX_PANES_PER_AXIS, PaneId, SplitOrientation, SplitTree,
};

enum AxisAdd {
    Added(SplitTree),
    Blocked,
    NotApplicable,
}

/// Replace `target` with an even split whose second child is `new_pane`.
pub fn split_pane(
    tree: &mut SplitTree,
    target: PaneId,
    new_pane: PaneId,
    orientation: SplitOrientation,
) -> bool {
    split_pane_with_order(tree, target, new_pane, orientation, false)
}

/// Add `new_pane` next to `target` in the requested direction.
///
/// Repeated additions along the same contiguous axis behave like adding to a
/// row/column: the axis group is rebuilt with count-based equal ratios instead
/// of repeatedly splitting only the focused leaf.
pub fn split_pane_in_direction(
    tree: &mut SplitTree,
    target: PaneId,
    new_pane: PaneId,
    direction: Direction,
) -> bool {
    match add_pane_to_axis_model(tree, target, new_pane, direction) {
        AxisAdd::Added(rebuilt) => {
            *tree = rebuilt;
            true
        }
        AxisAdd::Blocked => false,
        AxisAdd::NotApplicable => split_pane_with_order(
            tree,
            target,
            new_pane,
            direction.split_orientation(),
            direction.places_new_split_before_existing(),
        ),
    }
}

pub fn can_add_pane_in_direction(tree: &SplitTree, target: PaneId, direction: Direction) -> bool {
    match axis_group_len(tree, target, direction) {
        AxisGroupLen::Known(len) => len < MAX_PANES_PER_AXIS,
        AxisGroupLen::Unknown => contains_pane(tree, target),
    }
}

fn split_pane_with_order(
    tree: &mut SplitTree,
    target: PaneId,
    new_pane: PaneId,
    orientation: SplitOrientation,
    new_pane_first: bool,
) -> bool {
    match tree {
        SplitTree::Leaf { pane } if *pane == target => {
            let existing = SplitTree::leaf(target);
            let new = SplitTree::leaf(new_pane);
            let (first, second) = if new_pane_first {
                (new, existing)
            } else {
                (existing, new)
            };
            *tree = SplitTree::split_even(orientation, first, second);
            true
        }
        SplitTree::Leaf { .. } => false,
        SplitTree::Split { first, second, .. } => {
            split_pane_with_order(first, target, new_pane, orientation, new_pane_first)
                || split_pane_with_order(second, target, new_pane, orientation, new_pane_first)
        }
    }
}

fn add_pane_to_axis_model(
    tree: &SplitTree,
    target: PaneId,
    new_pane: PaneId,
    direction: Direction,
) -> AxisAdd {
    match direction.split_orientation() {
        SplitOrientation::Horizontal => add_pane_to_rows(
            tree,
            target,
            new_pane,
            direction.places_new_split_before_existing(),
        ),
        SplitOrientation::Vertical => add_pane_to_columns(
            tree,
            target,
            new_pane,
            direction.places_new_split_before_existing(),
        ),
    }
}

enum AxisGroupLen {
    Known(usize),
    Unknown,
}

fn axis_group_len(tree: &SplitTree, target: PaneId, direction: Direction) -> AxisGroupLen {
    match direction.split_orientation() {
        SplitOrientation::Horizontal => tree_to_rows(tree)
            .and_then(|rows| group_len_containing(rows, target))
            .map_or(AxisGroupLen::Unknown, AxisGroupLen::Known),
        SplitOrientation::Vertical => tree_to_columns(tree)
            .and_then(|columns| group_len_containing(columns, target))
            .map_or(AxisGroupLen::Unknown, AxisGroupLen::Known),
    }
}

fn add_pane_to_rows(
    tree: &SplitTree,
    target: PaneId,
    new_pane: PaneId,
    before_target: bool,
) -> AxisAdd {
    let Some(mut rows) = tree_to_rows(tree) else {
        return AxisAdd::NotApplicable;
    };
    let Some(row_index) = groups_containing(&rows, target).next() else {
        return AxisAdd::NotApplicable;
    };
    if rows[row_index].len() >= MAX_PANES_PER_AXIS {
        return AxisAdd::Blocked;
    }
    if !insert_pane_in_group(&mut rows[row_index], target, new_pane, before_target) {
        return AxisAdd::NotApplicable;
    }
    AxisAdd::Added(build_rows(rows))
}

fn add_pane_to_columns(
    tree: &SplitTree,
    target: PaneId,
    new_pane: PaneId,
    before_target: bool,
) -> AxisAdd {
    let Some(mut columns) = tree_to_columns(tree) else {
        return AxisAdd::NotApplicable;
    };
    let Some(column_index) = groups_containing(&columns, target).next() else {
        return AxisAdd::NotApplicable;
    };
    if columns[column_index].len() >= MAX_PANES_PER_AXIS {
        return AxisAdd::Blocked;
    }
    if !insert_pane_in_group(&mut columns[column_index], target, new_pane, before_target) {
        return AxisAdd::NotApplicable;
    }
    AxisAdd::Added(build_columns(columns))
}

fn tree_to_rows(tree: &SplitTree) -> Option<Vec<Vec<SplitTree>>> {
    match tree {
        SplitTree::Leaf { .. } => Some(vec![vec![tree.clone()]]),
        SplitTree::Split {
            orientation: SplitOrientation::Horizontal,
            first,
            second,
            ..
        } => {
            let mut left_rows = tree_to_rows(first)?;
            let right_rows = tree_to_rows(second)?;
            if left_rows.len() != right_rows.len() {
                return None;
            }
            for (left, right) in left_rows.iter_mut().zip(right_rows) {
                left.extend(right);
            }
            Some(left_rows)
        }
        SplitTree::Split {
            orientation: SplitOrientation::Vertical,
            first,
            second,
            ..
        } => {
            let mut rows = tree_to_rows(first)?;
            rows.extend(tree_to_rows(second)?);
            Some(rows)
        }
    }
}

fn tree_to_columns(tree: &SplitTree) -> Option<Vec<Vec<SplitTree>>> {
    match tree {
        SplitTree::Leaf { .. } => Some(vec![vec![tree.clone()]]),
        SplitTree::Split {
            orientation: SplitOrientation::Horizontal,
            first,
            second,
            ..
        } => {
            let mut columns = tree_to_columns(first)?;
            columns.extend(tree_to_columns(second)?);
            Some(columns)
        }
        SplitTree::Split {
            orientation: SplitOrientation::Vertical,
            first,
            second,
            ..
        } => {
            let mut top_columns = tree_to_columns(first)?;
            let bottom_columns = tree_to_columns(second)?;
            if top_columns.len() != bottom_columns.len() {
                return None;
            }
            for (top, bottom) in top_columns.iter_mut().zip(bottom_columns) {
                top.extend(bottom);
            }
            Some(top_columns)
        }
    }
}

fn groups_containing(
    groups: &[Vec<SplitTree>],
    target: PaneId,
) -> impl Iterator<Item = usize> + '_ {
    groups.iter().enumerate().filter_map(move |(index, group)| {
        group
            .iter()
            .any(|item| matches!(item, SplitTree::Leaf { pane } if *pane == target))
            .then_some(index)
    })
}

fn group_len_containing(groups: Vec<Vec<SplitTree>>, target: PaneId) -> Option<usize> {
    groups
        .into_iter()
        .find(|group| {
            group
                .iter()
                .any(|item| matches!(item, SplitTree::Leaf { pane } if *pane == target))
        })
        .map(|group| group.len())
}

fn insert_pane_in_group(
    group: &mut Vec<SplitTree>,
    target: PaneId,
    new_pane: PaneId,
    before_target: bool,
) -> bool {
    let Some(index) = group
        .iter()
        .position(|item| matches!(item, SplitTree::Leaf { pane } if *pane == target))
    else {
        return false;
    };
    let insertion_index = if before_target { index } else { index + 1 };
    group.insert(insertion_index, SplitTree::leaf(new_pane));
    true
}

fn build_rows(rows: Vec<Vec<SplitTree>>) -> SplitTree {
    let row_trees = rows
        .into_iter()
        .map(|row| build_even_axis_group(SplitOrientation::Horizontal, row))
        .collect();
    build_even_axis_group(SplitOrientation::Vertical, row_trees)
}

fn build_columns(columns: Vec<Vec<SplitTree>>) -> SplitTree {
    let column_trees = columns
        .into_iter()
        .map(|column| build_even_axis_group(SplitOrientation::Vertical, column))
        .collect();
    build_even_axis_group(SplitOrientation::Horizontal, column_trees)
}

fn build_even_axis_group(orientation: SplitOrientation, mut items: Vec<SplitTree>) -> SplitTree {
    debug_assert!(!items.is_empty());
    if items.len() == 1 {
        return items.remove(0);
    }

    let total = items.len();
    let first = items.remove(0);
    let second = build_even_axis_group(orientation, items);
    SplitTree::split(orientation, 1.0 / total as f32, first, second)
}

/// Reset every split ratio in the tree to equal children.
pub fn equalize(tree: &mut SplitTree) {
    match tree {
        SplitTree::Leaf { .. } => {}
        SplitTree::Split {
            ratio,
            first,
            second,
            ..
        } => {
            *ratio = DEFAULT_SPLIT_RATIO;
            equalize(first);
            equalize(second);
        }
    }
}

pub fn contains_pane(tree: &SplitTree, needle: PaneId) -> bool {
    match tree {
        SplitTree::Leaf { pane } => *pane == needle,
        SplitTree::Split { first, second, .. } => {
            contains_pane(first, needle) || contains_pane(second, needle)
        }
    }
}

pub(super) fn first_pane(tree: &SplitTree) -> Option<PaneId> {
    match tree {
        SplitTree::Leaf { pane } => Some(*pane),
        SplitTree::Split { first, second, .. } => first_pane(first).or_else(|| first_pane(second)),
    }
}

pub(super) fn pane_ids(tree: &SplitTree, out: &mut Vec<PaneId>) {
    match tree {
        SplitTree::Leaf { pane } => out.push(*pane),
        SplitTree::Split { first, second, .. } => {
            pane_ids(first, out);
            pane_ids(second, out);
        }
    }
}
