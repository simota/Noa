//! Pure split-pane draw planning.
//!
//! This module is intentionally free of `wgpu` and windowing types. The app
//! layer supplies pane ids and already-computed pane rectangles; the renderer
//! turns that into one ordered render-pass plan.

/// Stable render-side identity for a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

impl PaneId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Pixel-space pane rectangle, measured from the window content origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl PaneRect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.w)
    }

    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.h)
    }
}

/// One operation in a single render pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrawOp {
    Clear,
    PaneCells {
        pane: PaneId,
        scissor: PaneRect,
        bind_group_index: usize,
    },
    Dividers {
        rects: Vec<PaneRect>,
    },
}

/// Build the single-pass draw plan for the visible pane set.
///
/// The returned order is always one [`DrawOp::Clear`], then one scissored
/// [`DrawOp::PaneCells`] per visible pane in layout order, then
/// [`DrawOp::Dividers`]. When `zoomed` names a live pane, only that pane emits
/// cells and divider geometry is suppressed.
pub fn build_draw_plan(layout: &[(PaneId, PaneRect)], zoomed: Option<PaneId>) -> Vec<DrawOp> {
    let zoomed = zoomed.filter(|pane| layout.iter().any(|(candidate, _)| candidate == pane));
    let mut plan = Vec::with_capacity(layout.len() + 2);
    plan.push(DrawOp::Clear);

    for (index, (pane, rect)) in layout.iter().enumerate() {
        if zoomed.is_some_and(|zoomed| zoomed != *pane) {
            continue;
        }

        plan.push(DrawOp::PaneCells {
            pane: *pane,
            scissor: *rect,
            bind_group_index: index,
        });
    }

    let rects = if zoomed.is_some() {
        Vec::new()
    } else {
        divider_rects(layout)
    };
    plan.push(DrawOp::Dividers { rects });

    plan
}

fn divider_rects(layout: &[(PaneId, PaneRect)]) -> Vec<PaneRect> {
    let mut rects = Vec::new();
    for (index, (_, a)) in layout.iter().enumerate() {
        for (_, b) in layout.iter().skip(index + 1) {
            if let Some(rect) = vertical_divider_between(*a, *b) {
                push_unique(&mut rects, rect);
            }
            if let Some(rect) = vertical_divider_between(*b, *a) {
                push_unique(&mut rects, rect);
            }
            if let Some(rect) = horizontal_divider_between(*a, *b) {
                push_unique(&mut rects, rect);
            }
            if let Some(rect) = horizontal_divider_between(*b, *a) {
                push_unique(&mut rects, rect);
            }
        }
    }
    rects
}

fn vertical_divider_between(left: PaneRect, right: PaneRect) -> Option<PaneRect> {
    if left.right() >= right.x {
        return None;
    }

    let top = left.y.max(right.y);
    let bottom = left.bottom().min(right.bottom());
    (top < bottom).then_some(PaneRect::new(
        left.right(),
        top,
        right.x - left.right(),
        bottom - top,
    ))
}

fn horizontal_divider_between(top: PaneRect, bottom: PaneRect) -> Option<PaneRect> {
    if top.bottom() >= bottom.y {
        return None;
    }

    let left = top.x.max(bottom.x);
    let right = top.right().min(bottom.right());
    (left < right).then_some(PaneRect::new(
        left,
        top.bottom(),
        right - left,
        bottom.y - top.bottom(),
    ))
}

fn push_unique(rects: &mut Vec<PaneRect>, rect: PaneRect) {
    if !rects.contains(&rect) {
        rects.push(rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_cells(plan: &[DrawOp]) -> Vec<(PaneId, PaneRect, usize)> {
        plan.iter()
            .filter_map(|op| match op {
                DrawOp::PaneCells {
                    pane,
                    scissor,
                    bind_group_index,
                } => Some((*pane, *scissor, *bind_group_index)),
                DrawOp::Clear | DrawOp::Dividers { .. } => None,
            })
            .collect()
    }

    #[test]
    fn two_pane_plan_keeps_dividers_after_cells_in_one_plan() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let layout = [
            (left, PaneRect::new(0, 0, 50, 20)),
            (right, PaneRect::new(51, 0, 49, 20)),
        ];

        let plan = build_draw_plan(&layout, None);

        assert_eq!(plan.first(), Some(&DrawOp::Clear));
        assert!(matches!(plan.get(1), Some(DrawOp::PaneCells { pane, .. }) if *pane == left));
        assert!(matches!(plan.get(2), Some(DrawOp::PaneCells { pane, .. }) if *pane == right));
        assert!(
            matches!(plan.get(3), Some(DrawOp::Dividers { rects }) if rects == &vec![PaneRect::new(50, 0, 1, 20)])
        );
        assert_eq!(plan.len(), 4);
    }

    #[test]
    fn three_pane_plan_has_one_clear_and_distinct_scissors_and_bind_groups() {
        let layout = [
            (PaneId::new(1), PaneRect::new(0, 0, 50, 41)),
            (PaneId::new(2), PaneRect::new(51, 0, 49, 20)),
            (PaneId::new(3), PaneRect::new(51, 21, 49, 20)),
        ];

        let plan = build_draw_plan(&layout, None);
        let cells = pane_cells(&plan);

        assert_eq!(
            plan.iter().filter(|op| matches!(op, DrawOp::Clear)).count(),
            1
        );
        assert_eq!(cells.len(), 3);
        assert_eq!(
            cells.iter().map(|(_, rect, _)| *rect).collect::<Vec<_>>(),
            vec![layout[0].1, layout[1].1, layout[2].1]
        );
        assert_eq!(
            cells.iter().map(|(_, _, index)| *index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_ne!(cells[0].1, cells[1].1);
        assert_ne!(cells[0].1, cells[2].1);
        assert_ne!(cells[1].1, cells[2].1);
        assert!(matches!(plan.last(), Some(DrawOp::Dividers { .. })));
    }

    #[test]
    fn zoomed_plan_keeps_only_the_zoomed_pane_and_suppresses_divider_rects() {
        let layout = [
            (PaneId::new(1), PaneRect::new(0, 0, 50, 20)),
            (PaneId::new(2), PaneRect::new(51, 0, 49, 20)),
        ];

        let plan = build_draw_plan(&layout, Some(PaneId::new(2)));

        assert_eq!(
            pane_cells(&plan),
            vec![(PaneId::new(2), PaneRect::new(51, 0, 49, 20), 1)]
        );
        assert!(matches!(plan.last(), Some(DrawOp::Dividers { rects }) if rects.is_empty()));
    }
}
