//! Pane repositioning: swap two panes' identities, detach a pane without
//! destroying it, and compose the two into a directional move. This is the
//! pure-tree half of pane D&D (docs/specs/pane-dnd.md L2(a)); the UI layer
//! owns self-drop rejection, force-unzoom orchestration lives in
//! [`super::zoom`], and the per-tab pane cap lives in `app/helpers/geometry.rs`
//! (this layer has no pixel geometry to evaluate it).

use super::close::{RemovePaneResult, remove_pane_from_tree};
use super::ops::{can_add_pane_in_direction, contains_pane, split_pane_in_direction};
use super::types::{Direction, PaneId, SplitTree};

/// Pure decision returned after detaching a pane from the tree without
/// touching its Surface/Pty.
///
/// Modeled on [`super::close::CloseOutcome`] but kept as a distinct type
/// (per spec L2(a)): closing destroys the pane's terminal state, extraction
/// only detaches it from `tree` so the caller can re-attach it elsewhere
/// (swap, directional move, cross-tab move).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub next_focus: Option<PaneId>,
    pub tab_should_close: bool,
}

/// Exchange the `PaneId`s of the `Leaf` nodes holding `a` and `b`; the tree
/// shape (and every ratio/orientation) is unchanged, so `surfaces` needs no
/// change since its keys are unchanged too — only which leaf displays which
/// pane's content moves.
///
/// Returns `false` (tree left byte-for-byte unchanged) when `a == b` or
/// either pane is absent from `tree`. The UI layer owns rejecting self-drop
/// as a user-facing no-op (FR-14); this layer's job is simply to never
/// corrupt the tree on bad input.
pub fn swap_pane(tree: &mut SplitTree, a: PaneId, b: PaneId) -> bool {
    if a == b || !contains_pane(tree, a) || !contains_pane(tree, b) {
        return false;
    }
    swap_leaf_ids(tree, a, b);
    true
}

fn swap_leaf_ids(tree: &mut SplitTree, a: PaneId, b: PaneId) {
    match tree {
        SplitTree::Leaf { pane } => {
            if *pane == a {
                *pane = b;
            } else if *pane == b {
                *pane = a;
            }
        }
        SplitTree::Split { first, second, .. } => {
            swap_leaf_ids(first, a, b);
            swap_leaf_ids(second, a, b);
        }
    }
}

/// Detach `pane` from `tree`, collapsing its sibling into the parent split
/// exactly like [`close_pane`](super::close::close_pane) — this reuses that
/// function's recursive collapse walk directly so the two paths can never
/// diverge — but the caller remains responsible for the pane's Surface/Pty;
/// this function only edits `tree` shape.
pub fn extract_pane(tree: &mut SplitTree, pane: PaneId) -> RemoveOutcome {
    match remove_pane_from_tree(tree.clone(), pane) {
        RemovePaneResult::NotFound(_) => RemoveOutcome {
            next_focus: None,
            tab_should_close: false,
        },
        RemovePaneResult::Removed {
            tree: None,
            next_focus,
        } => RemoveOutcome {
            next_focus,
            tab_should_close: true,
        },
        RemovePaneResult::Removed {
            tree: Some(updated),
            next_focus,
        } => {
            *tree = updated;
            RemoveOutcome {
                next_focus,
                tab_should_close: false,
            }
        }
    }
}

/// Rejection reasons for [`move_pane`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveError {
    /// `moved == target`, or one of them is absent from `tree`.
    InvalidPanes,
    /// Insertion at `target` would exceed a pane-count cap (axis or per-tab).
    MaxPanesExceeded,
}

/// Detach `moved` from `tree` and split-insert it at `target`'s
/// `direction`-side edge — the composed D&D edge-drop operation (spec L2(a)).
///
/// **Atomicity (Omen A3):** the whole operation runs against a private clone
/// of `tree` and is only written back on success, so a mid-composition
/// failure (e.g. the post-extraction cap check) never leaves `tree`
/// half-transformed — every rejection path returns with `tree` byte-for-byte
/// unchanged.
///
/// **Adjacent-same-axis-group case:** if `moved` and `target` were the two
/// panes of the very split `moved` sat in, extracting `moved` collapses that
/// split and promotes `target` in its place. Because every step here
/// resolves `target` by `PaneId` value (never by a captured tree path), the
/// subsequent insertion still finds `target` at its new position and places
/// `moved` at its `direction` edge, matching the spec's defined outcome for
/// this case with no special-casing required.
///
/// **Cap contract — single entry point (Omen A5):** this function always
/// enforces the per-axis cap (`MAX_PANES_PER_AXIS`) against the
/// post-extraction tree via [`can_add_pane_in_direction`]. The per-tab cap
/// (`MAX_PANES_PER_TAB`) needs the destination tab's pixel geometry, which
/// this pure tree layer never has, so it is not computed here — the caller
/// MUST evaluate it first (`app::helpers::geometry::can_create_split`) and
/// pass the result as `tab_cap_ok`. Every caller (the D&D edge-drop commit,
/// `commit_pane_move`, and the Overview cross-tab move) MUST route every
/// directional move through this one function with a correctly computed
/// `tab_cap_ok`, so the two caps can never diverge between call sites — never
/// call `split_pane_in_direction` directly to bypass this contract.
pub fn move_pane(
    tree: &mut SplitTree,
    moved: PaneId,
    target: PaneId,
    direction: Direction,
    tab_cap_ok: bool,
) -> Result<RemoveOutcome, MoveError> {
    if moved == target || !contains_pane(tree, moved) || !contains_pane(tree, target) {
        return Err(MoveError::InvalidPanes);
    }

    let mut working = tree.clone();
    let remove_outcome = extract_pane(&mut working, moved);

    // `target != moved` and extraction only ever removes `moved`'s leaf, so
    // `target` always survives extraction (possibly promoted, see doc
    // comment above). This guard is defensive against future refactors of
    // `extract_pane` rather than a reachable path today.
    if !contains_pane(&working, target) {
        return Err(MoveError::InvalidPanes);
    }

    if !tab_cap_ok || !can_add_pane_in_direction(&working, target, direction) {
        return Err(MoveError::MaxPanesExceeded);
    }

    if !split_pane_in_direction(&mut working, target, moved, direction) {
        return Err(MoveError::MaxPanesExceeded);
    }

    *tree = working;
    Ok(remove_outcome)
}
