use super::types::{DIVIDER_HIT_ZONE_PX, DIVIDER_WIDTH_PX, PaneId, Point, Rect};

/// Result of pointer hit-testing in a split layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    Pane(PaneId),
    Divider,
}

/// Hit-test a point against dividers first, then panes.
pub fn hit_test(layout: &[(PaneId, Rect)], point: Point) -> Option<HitTarget> {
    if point_hits_divider(layout, point) {
        return Some(HitTarget::Divider);
    }

    layout
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(pane, _)| HitTarget::Pane(*pane))
}

fn point_hits_divider(layout: &[(PaneId, Rect)], point: Point) -> bool {
    for (index, (_, a)) in layout.iter().enumerate() {
        for (_, b) in layout.iter().skip(index + 1) {
            let (a, b) = (*a, *b);

            if a.right() <= b.x && vertical_divider_hit(a, b, point) {
                return true;
            }

            if b.right() <= a.x && vertical_divider_hit(b, a, point) {
                return true;
            }

            if a.bottom() <= b.y && horizontal_divider_hit(a, b, point) {
                return true;
            }

            if b.bottom() <= a.y && horizontal_divider_hit(b, a, point) {
                return true;
            }
        }
    }

    false
}

pub(super) fn vertical_divider_hit(left: Rect, right: Rect, point: Point) -> bool {
    let gap = right.x - left.right();
    if gap == 0 || gap > DIVIDER_WIDTH_PX {
        return false;
    }

    let overlap_top = left.y.max(right.y);
    let overlap_bottom = left.bottom().min(right.bottom());
    if overlap_top >= overlap_bottom {
        return false;
    }

    let hit_left = left.right().saturating_sub(DIVIDER_HIT_ZONE_PX);
    let hit_right = right.x.saturating_add(DIVIDER_HIT_ZONE_PX);

    point.x >= hit_left && point.x < hit_right && point.y >= overlap_top && point.y < overlap_bottom
}

pub(super) fn horizontal_divider_hit(top: Rect, bottom: Rect, point: Point) -> bool {
    let gap = bottom.y - top.bottom();
    if gap == 0 || gap > DIVIDER_WIDTH_PX {
        return false;
    }

    let overlap_left = top.x.max(bottom.x);
    let overlap_right = top.right().min(bottom.right());
    if overlap_left >= overlap_right {
        return false;
    }

    let hit_top = top.bottom().saturating_sub(DIVIDER_HIT_ZONE_PX);
    let hit_bottom = bottom.y.saturating_add(DIVIDER_HIT_ZONE_PX);

    point.y >= hit_top && point.y < hit_bottom && point.x >= overlap_left && point.x < overlap_right
}
