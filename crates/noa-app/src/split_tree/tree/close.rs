use super::ops::first_pane;
use super::types::{PaneId, SplitTree};

/// Pure decision returned after removing a pane from the split tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseOutcome {
    pub next_focus: Option<PaneId>,
    pub tab_should_close: bool,
}

/// Remove `pane` from the tree, collapsing its sibling into the parent split.
///
/// The last pane cannot be removed from the tree because the caller should
/// close the containing tab/window instead; this is reported via
/// `tab_should_close=true`.
pub fn close_pane(tree: &mut SplitTree, pane: PaneId) -> CloseOutcome {
    match remove_pane_from_tree(tree.clone(), pane) {
        RemovePaneResult::NotFound(_) => CloseOutcome {
            next_focus: None,
            tab_should_close: false,
        },
        RemovePaneResult::Removed {
            tree: None,
            next_focus,
        } => CloseOutcome {
            next_focus,
            tab_should_close: true,
        },
        RemovePaneResult::Removed {
            tree: Some(updated),
            next_focus,
        } => {
            *tree = updated;
            CloseOutcome {
                next_focus,
                tab_should_close: false,
            }
        }
    }
}

/// Shared by [`close_pane`] and [`super::reposition::extract_pane`] — the
/// latter reuses this recursive collapse walk so the two never diverge.
pub(super) enum RemovePaneResult {
    NotFound(SplitTree),
    Removed {
        tree: Option<SplitTree>,
        next_focus: Option<PaneId>,
    },
}

pub(super) fn remove_pane_from_tree(tree: SplitTree, target: PaneId) -> RemovePaneResult {
    match tree {
        SplitTree::Leaf { pane } if pane == target => RemovePaneResult::Removed {
            tree: None,
            next_focus: None,
        },
        SplitTree::Leaf { .. } => RemovePaneResult::NotFound(tree),
        SplitTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let first_tree = *first;
            let second_tree = *second;

            match remove_pane_from_tree(first_tree, target) {
                RemovePaneResult::Removed {
                    tree: None,
                    next_focus: _,
                } => {
                    let next_focus = first_pane(&second_tree);
                    RemovePaneResult::Removed {
                        tree: Some(second_tree),
                        next_focus,
                    }
                }
                RemovePaneResult::Removed {
                    tree: Some(updated_first),
                    next_focus,
                } => RemovePaneResult::Removed {
                    tree: Some(SplitTree::split(
                        orientation,
                        ratio,
                        updated_first,
                        second_tree,
                    )),
                    next_focus,
                },
                RemovePaneResult::NotFound(first_tree) => {
                    match remove_pane_from_tree(second_tree, target) {
                        RemovePaneResult::Removed {
                            tree: None,
                            next_focus: _,
                        } => {
                            let next_focus = first_pane(&first_tree);
                            RemovePaneResult::Removed {
                                tree: Some(first_tree),
                                next_focus,
                            }
                        }
                        RemovePaneResult::Removed {
                            tree: Some(updated_second),
                            next_focus,
                        } => RemovePaneResult::Removed {
                            tree: Some(SplitTree::split(
                                orientation,
                                ratio,
                                first_tree,
                                updated_second,
                            )),
                            next_focus,
                        },
                        RemovePaneResult::NotFound(second_tree) => RemovePaneResult::NotFound(
                            SplitTree::split(orientation, ratio, first_tree, second_tree),
                        ),
                    }
                }
            }
        }
    }
}
