use super::types::{DEFAULT_SPLIT_RATIO, PaneId, SplitOrientation, SplitTree};

/// Replace `target` with an even split whose second child is `new_pane`.
pub fn split_pane(
    tree: &mut SplitTree,
    target: PaneId,
    new_pane: PaneId,
    orientation: SplitOrientation,
) -> bool {
    match tree {
        SplitTree::Leaf { pane } if *pane == target => {
            *tree = SplitTree::split_even(
                orientation,
                SplitTree::leaf(target),
                SplitTree::leaf(new_pane),
            );
            true
        }
        SplitTree::Leaf { .. } => false,
        SplitTree::Split { first, second, .. } => {
            split_pane(first, target, new_pane, orientation)
                || split_pane(second, target, new_pane, orientation)
        }
    }
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

pub(super) fn contains_pane(tree: &SplitTree, needle: PaneId) -> bool {
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
