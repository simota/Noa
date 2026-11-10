//! Pure split-pane draw planning.
//!
//! This module is intentionally free of `wgpu` and windowing types. The app
//! layer supplies pane ids and already-computed pane rectangles; the renderer
//! turns that into one ordered render-pass plan.

const DIVIDER_THICKNESS_PX: u32 = 1;

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
    FocusIndicator {
        pane: PaneId,
        rects: Vec<PaneRect>,
    },
    /// An overlay group's opaque backdrop and one-pixel border, drawn before
    /// that group's own [`DrawOp::PaneCells`].
    OverlayFrame {
        backdrop: PaneRect,
        header: Option<(PaneRect, noa_core::Rgb)>,
        border: Vec<PaneRect>,
        focused: bool,
    },
}

/// Panes drawn above the split layout, as one stacked unit: an opaque
/// backdrop over `outer`, a border, then each pane's cells. Groups are drawn
/// in slice order, so a later group covers an earlier one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayGroup {
    pub outer: PaneRect,
    /// A solid band painted over the backdrop (the group's title strip), so
    /// the strip's color reaches the group's edges past its grid padding.
    pub header: Option<(PaneRect, noa_core::Rgb)>,
    pub panes: Vec<PaneId>,
    pub focused: bool,
}

/// Build the single-pass draw plan for the visible pane set.
///
/// The returned order is always one [`DrawOp::Clear`], then one scissored
/// [`DrawOp::PaneCells`] per visible pane in layout order, then
/// [`DrawOp::Dividers`], then an optional [`DrawOp::FocusIndicator`]. When
/// `zoomed` names a live pane, only that pane emits cells and divider/focus
/// geometry is suppressed. `force_ring` (scratch-terminal kaizen item 4)
/// draws the focus indicator around the sole pane even when there is only
/// one visible pane — normally suppressed then, since a single pane needs no
/// ring to disambiguate focus from its siblings; the scratch terminal popup
/// (always exactly one pane) uses this as a thin ephemerality cue instead.
pub fn build_draw_plan(
    layout: &[(PaneId, PaneRect)],
    focused: Option<PaneId>,
    zoomed: Option<PaneId>,
    force_ring: bool,
) -> Vec<DrawOp> {
    build_draw_plan_with_overlays(layout, &[], focused, zoomed, force_ring)
}

/// [`build_draw_plan`] plus overlay groups. A pane named by a group is left
/// out of the split layout's cells, dividers, focus ring and zoom filter, and
/// drawn after them with its group; `bind_group_index` stays the pane's index
/// in `layout`.
pub fn build_draw_plan_with_overlays(
    layout: &[(PaneId, PaneRect)],
    overlays: &[OverlayGroup],
    focused: Option<PaneId>,
    zoomed: Option<PaneId>,
    force_ring: bool,
) -> Vec<DrawOp> {
    let is_overlay = |pane: &PaneId| overlays.iter().any(|group| group.panes.contains(pane));
    let base: Vec<(PaneId, PaneRect)> = layout
        .iter()
        .filter(|(pane, _)| !is_overlay(pane))
        .copied()
        .collect();
    let zoomed = zoomed.filter(|pane| base.iter().any(|(candidate, _)| candidate == pane));
    let mut plan = Vec::with_capacity(layout.len() + overlays.len() + 3);
    plan.push(DrawOp::Clear);

    for (index, (pane, rect)) in layout.iter().enumerate() {
        if is_overlay(pane) || zoomed.is_some_and(|zoomed| zoomed != *pane) {
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
        divider_rects(&base)
    };
    plan.push(DrawOp::Dividers { rects });

    if let Some((pane, rects)) = focus_indicator(&base, focused, zoomed, force_ring) {
        plan.push(DrawOp::FocusIndicator { pane, rects });
    }

    for group in overlays {
        if group.outer.w == 0 || group.outer.h == 0 {
            continue;
        }
        plan.push(DrawOp::OverlayFrame {
            backdrop: group.outer,
            header: group.header,
            border: overlay_border_rects(group.outer),
            focused: group.focused,
        });
        for pane in &group.panes {
            if let Some((index, (_, rect))) = layout
                .iter()
                .enumerate()
                .find(|(_, (candidate, _))| candidate == pane)
            {
                plan.push(DrawOp::PaneCells {
                    pane: *pane,
                    scissor: *rect,
                    bind_group_index: index,
                });
            }
        }
    }

    plan
}

/// A one-pixel frame just inside `rect`.
fn overlay_border_rects(rect: PaneRect) -> Vec<PaneRect> {
    let mut rects = vec![PaneRect::new(rect.x, rect.y, rect.w, 1)];
    if rect.h > 1 {
        rects.push(PaneRect::new(rect.x, rect.bottom() - 1, rect.w, 1));
    }
    if rect.h > 2 {
        rects.push(PaneRect::new(rect.x, rect.y + 1, 1, rect.h - 2));
        if rect.w > 1 {
            rects.push(PaneRect::new(rect.right() - 1, rect.y + 1, 1, rect.h - 2));
        }
    }
    rects
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
    if right.x - left.right() != DIVIDER_THICKNESS_PX {
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
    if bottom.y - top.bottom() != DIVIDER_THICKNESS_PX {
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

fn focus_indicator(
    layout: &[(PaneId, PaneRect)],
    focused: Option<PaneId>,
    zoomed: Option<PaneId>,
    force_ring: bool,
) -> Option<(PaneId, Vec<PaneRect>)> {
    let visible_count = layout
        .iter()
        .filter(|(pane, _)| zoomed.is_none_or(|zoomed| zoomed == *pane))
        .count();
    if visible_count <= 1 && !force_ring {
        return None;
    }

    let focused = focused?;
    if zoomed.is_some_and(|zoomed| zoomed != focused) {
        return None;
    }
    let rect = layout
        .iter()
        .find_map(|(pane, rect)| (*pane == focused).then_some(*rect))?;
    let rects = focus_indicator_rects(rect);
    (!rects.is_empty()).then_some((focused, rects))
}

fn focus_indicator_rects(rect: PaneRect) -> Vec<PaneRect> {
    let thickness = 2.min(rect.w).min(rect.h);
    if thickness == 0 {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(4);
    rects.push(PaneRect::new(rect.x, rect.y, rect.w, thickness));

    if rect.h > thickness {
        rects.push(PaneRect::new(
            rect.x,
            rect.bottom() - thickness,
            rect.w,
            thickness,
        ));
    }

    let vertical_h = rect.h.saturating_sub(thickness.saturating_mul(2));
    if vertical_h > 0 {
        let vertical_y = rect.y + thickness;
        rects.push(PaneRect::new(rect.x, vertical_y, thickness, vertical_h));
        if rect.w > thickness {
            rects.push(PaneRect::new(
                rect.right() - thickness,
                vertical_y,
                thickness,
                vertical_h,
            ));
        }
    }

    rects
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
                DrawOp::Clear
                | DrawOp::Dividers { .. }
                | DrawOp::FocusIndicator { .. }
                | DrawOp::OverlayFrame { .. } => None,
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

        let plan = build_draw_plan(&layout, None, None, false);

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

        let plan = build_draw_plan(&layout, None, None, false);
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

        let plan = build_draw_plan(&layout, None, Some(PaneId::new(2)), false);

        assert_eq!(
            pane_cells(&plan),
            vec![(PaneId::new(2), PaneRect::new(51, 0, 49, 20), 1)]
        );
        assert!(matches!(plan.last(), Some(DrawOp::Dividers { rects }) if rects.is_empty()));
    }

    #[test]
    fn divider_plan_ignores_large_gaps_from_incomplete_layouts() {
        let horizontal_gap = [
            (PaneId::new(1), PaneRect::new(0, 0, 33, 20)),
            (PaneId::new(3), PaneRect::new(68, 0, 32, 20)),
        ];
        let vertical_gap = [
            (PaneId::new(1), PaneRect::new(0, 0, 40, 33)),
            (PaneId::new(3), PaneRect::new(0, 68, 40, 32)),
        ];

        let horizontal_plan = build_draw_plan(&horizontal_gap, None, None, false);
        let vertical_plan = build_draw_plan(&vertical_gap, None, None, false);

        assert!(
            matches!(horizontal_plan.last(), Some(DrawOp::Dividers { rects }) if rects.is_empty()),
            "large horizontal gaps should not be painted as split dividers"
        );
        assert!(
            matches!(vertical_plan.last(), Some(DrawOp::Dividers { rects }) if rects.is_empty()),
            "large vertical gaps should not be painted as split dividers"
        );
    }

    #[test]
    fn focused_plan_draws_two_pixel_indicator_after_dividers() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let layout = [
            (left, PaneRect::new(0, 0, 50, 20)),
            (right, PaneRect::new(51, 0, 49, 20)),
        ];

        let plan = build_draw_plan(&layout, Some(right), None, false);

        assert!(matches!(plan.get(3), Some(DrawOp::Dividers { .. })));
        assert!(
            matches!(
                plan.get(4),
                Some(DrawOp::FocusIndicator { pane, rects })
                    if *pane == right
                        && rects == &vec![
                            PaneRect::new(51, 0, 49, 2),
                            PaneRect::new(51, 18, 49, 2),
                            PaneRect::new(51, 2, 2, 16),
                            PaneRect::new(98, 2, 2, 16),
                        ]
            ),
            "focused pane should get a stable 2px pixel-space focus ring"
        );
        assert_eq!(plan.len(), 5);
    }

    #[test]
    fn focus_indicator_is_suppressed_for_single_or_zoomed_visible_pane() {
        let layout = [
            (PaneId::new(1), PaneRect::new(0, 0, 50, 20)),
            (PaneId::new(2), PaneRect::new(51, 0, 49, 20)),
        ];

        let single = build_draw_plan(&layout[..1], Some(PaneId::new(1)), None, false);
        assert!(
            single
                .iter()
                .all(|op| !matches!(op, DrawOp::FocusIndicator { .. }))
        );

        let zoomed = build_draw_plan(&layout, Some(PaneId::new(2)), Some(PaneId::new(2)), false);
        assert!(
            zoomed
                .iter()
                .all(|op| !matches!(op, DrawOp::FocusIndicator { .. }))
        );
        assert!(matches!(zoomed.last(), Some(DrawOp::Dividers { rects }) if rects.is_empty()));
    }

    // scratch-terminal kaizen item 4: `force_ring` is the one override that
    // still draws a focus indicator around a single visible pane, giving the
    // scratch terminal popup (always exactly one pane) its ephemerality cue.
    #[test]
    fn overlay_panes_draw_after_the_split_layout_and_stay_out_of_it() {
        let split_a = PaneId::new(1);
        let split_b = PaneId::new(2);
        let strip = PaneId::new(10);
        let content = PaneId::new(11);
        let layout = [
            (split_a, PaneRect::new(0, 0, 499, 600)),
            (split_b, PaneRect::new(500, 0, 500, 600)),
            (strip, PaneRect::new(100, 100, 300, 20)),
            (content, PaneRect::new(100, 120, 300, 180)),
        ];
        let overlays = [OverlayGroup {
            outer: PaneRect::new(100, 100, 300, 200),
            header: None,
            panes: vec![strip, content],
            focused: true,
        }];

        // Zooming a split pane must not hide the overlay, and the overlay
        // must not become the focus ring's or a divider's subject.
        let plan = build_draw_plan_with_overlays(&layout, &overlays, Some(content), Some(split_b), false);
        let kinds: Vec<String> = plan
            .iter()
            .map(|op| match op {
                DrawOp::Clear => "clear".to_string(),
                DrawOp::PaneCells { pane, bind_group_index, .. } => {
                    format!("cells:{}@{bind_group_index}", pane.get())
                }
                DrawOp::Dividers { rects } => format!("dividers:{}", rects.len()),
                DrawOp::FocusIndicator { .. } => "focus".to_string(),
                DrawOp::OverlayFrame { backdrop, border, focused, .. } => {
                    format!("frame:{}x{}:{}:{focused}", backdrop.w, backdrop.h, border.len())
                }
            })
            .collect();
        assert_eq!(
            kinds,
            ["clear", "cells:2@1", "dividers:0", "frame:300x200:4:true", "cells:10@2", "cells:11@3"]
        );

        let unzoomed = build_draw_plan_with_overlays(&layout, &overlays, Some(split_a), None, false);
        assert!(unzoomed.iter().any(|op| matches!(op, DrawOp::Dividers { rects } if rects.len() == 1)));
        assert!(unzoomed.iter().any(|op| matches!(op, DrawOp::FocusIndicator { pane, .. } if *pane == split_a)));
        assert_eq!(build_draw_plan(&layout[..2], Some(split_a), None, false), {
            let mut base = unzoomed.clone();
            base.truncate(5);
            base
        });
    }

    #[test]
    fn force_ring_draws_focus_indicator_for_a_single_visible_pane() {
        let layout = [(PaneId::new(1), PaneRect::new(0, 0, 50, 20))];

        let plan = build_draw_plan(&layout, Some(PaneId::new(1)), None, true);
        assert_eq!(
            plan.last(),
            Some(&DrawOp::FocusIndicator {
                pane: PaneId::new(1),
                rects: focus_indicator_rects(PaneRect::new(0, 0, 50, 20)),
            })
        );
    }
}
