//! Pane drop-zone highlight geometry (pane-dnd `docs/specs/pane-dnd.md`
//! L2(d)): the pure 60/40 highlight-rect math for a drop zone within a target
//! pane's bounds. Consumed by the Tab Overview's layout-minimap drop-zone
//! highlight (`app/overview/render.rs`) — the surviving pane-movement gesture.

use super::*;

/// Pane-dnd L2(c)/AC-4/L2(d): the highlight rect for a drop zone within a
/// target pane's bounds — mirrors `classify_pane_zone`'s own 60/40 split
/// (`session_overview::tab_tiles`) exactly, so the highlight always matches
/// what a release at that position resolves to: the inner 60% for `Center`
/// (`edge: None`), or the 20%-margin band on the given side for
/// `Edge(direction)`. Both derive from the same documented `EDGE_MARGIN = 0.2`
/// (ASSUME-5), kept in step by inspection with `classify_pane_zone`.
pub(in crate::app) fn pane_zone_highlight_rect(
    bounds: split_tree::Rect,
    edge: Option<Direction>,
) -> split_tree::Rect {
    const MARGIN: f32 = 0.2;
    let mx = (bounds.w as f32 * MARGIN).round() as u32;
    let my = (bounds.h as f32 * MARGIN).round() as u32;
    match edge {
        None => split_tree::Rect::new(
            bounds.x + mx,
            bounds.y + my,
            bounds.w.saturating_sub(mx * 2),
            bounds.h.saturating_sub(my * 2),
        ),
        Some(Direction::Left) => split_tree::Rect::new(bounds.x, bounds.y, mx, bounds.h),
        Some(Direction::Right) => {
            split_tree::Rect::new(bounds.x + bounds.w.saturating_sub(mx), bounds.y, mx, bounds.h)
        }
        Some(Direction::Up) => split_tree::Rect::new(bounds.x, bounds.y, bounds.w, my),
        Some(Direction::Down) => {
            split_tree::Rect::new(bounds.x, bounds.y + bounds.h.saturating_sub(my), bounds.w, my)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // L2(d): the highlight rect matches `classify_pane_zone`'s own 60/40
    // split over the same bounds (the AC-4 coordinate table).
    #[test]
    fn zone_highlight_rect_center_is_inner_60_percent() {
        let bounds = split_tree::Rect::new(0, 0, 100, 100);
        assert_eq!(
            pane_zone_highlight_rect(bounds, None),
            split_tree::Rect::new(20, 20, 60, 60)
        );
    }

    #[test]
    fn zone_highlight_rect_edges_are_20_percent_bands() {
        let bounds = split_tree::Rect::new(0, 0, 100, 100);
        assert_eq!(
            pane_zone_highlight_rect(bounds, Some(Direction::Left)),
            split_tree::Rect::new(0, 0, 20, 100)
        );
        assert_eq!(
            pane_zone_highlight_rect(bounds, Some(Direction::Right)),
            split_tree::Rect::new(80, 0, 20, 100)
        );
        assert_eq!(
            pane_zone_highlight_rect(bounds, Some(Direction::Up)),
            split_tree::Rect::new(0, 0, 100, 20)
        );
        assert_eq!(
            pane_zone_highlight_rect(bounds, Some(Direction::Down)),
            split_tree::Rect::new(0, 80, 100, 20)
        );
    }

    // The rect must be offset by the target pane's own origin, not just its
    // size — a non-zero-origin pane (any split but the top-left one) is the
    // common case.
    #[test]
    fn zone_highlight_rect_is_offset_by_bounds_origin() {
        let bounds = split_tree::Rect::new(200, 50, 100, 100);
        assert_eq!(
            pane_zone_highlight_rect(bounds, None),
            split_tree::Rect::new(220, 70, 60, 60)
        );
    }
}
