//! Pure split-pane draw planning.
//!
//! This module is intentionally free of `wgpu` and windowing types. The app
//! layer supplies pane ids and already-computed pane rectangles; the renderer
//! turns that into one ordered render-pass plan.

use std::borrow::Cow;

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

    /// The overlap of `self` and `other`; zero-sized (at the overlap's
    /// clamped corner) when they do not overlap.
    pub fn intersect(self, other: PaneRect) -> PaneRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right()).max(x);
        let bottom = self.bottom().min(other.bottom()).max(y);
        PaneRect::new(x, y, right - x, bottom - y)
    }
}

/// One frame of the split-zoom transition, as the renderer draws it
/// (`docs/specs/split-zoom-animation.md`).
///
/// `pane`'s grid already has its zoomed size and its layout rect is the full
/// rect; only `clip` of it is visible, and its content is drawn translated so
/// the grid's top-left sits at `content_origin` (text never scales). Every
/// other split pane draws at `sibling_alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitZoomFx {
    pub pane: PaneId,
    pub clip: PaneRect,
    pub content_origin: (u32, u32),
    pub sibling_alpha: f32,
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
    /// Restores the frame's base (background fill plus background image)
    /// over `rect`, so split siblings drawn earlier do not show through the
    /// default-background cells of the split-zoom pane drawn next: those
    /// cells emit no background quad.
    ZoomUnderlay {
        rect: PaneRect,
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
    build_draw_plan_with_zoom_fx(layout, overlays, focused, zoomed, force_ring, None)
}

/// [`build_draw_plan_with_overlays`] during a split-zoom transition.
///
/// `fx = Some((pane, clip))` with `pane` a split (non-overlay) pane of
/// `layout`: `zoomed` is ignored and every other split pane draws in layout
/// order, then a [`DrawOp::ZoomUnderlay`] over `clip ∩ pane's rect`, then
/// `pane`'s cells scissored to that same rect, so it covers its siblings
/// only where it is revealed. Dividers are empty and there is no focus ring,
/// as when zoomed. Overlay groups follow unchanged. Any other `fx` (absent,
/// or naming a pane the split layout does not hold) gives the plain plan.
pub fn build_draw_plan_with_zoom_fx(
    layout: &[(PaneId, PaneRect)],
    overlays: &[OverlayGroup],
    focused: Option<PaneId>,
    zoomed: Option<PaneId>,
    force_ring: bool,
    fx: Option<(PaneId, PaneRect)>,
) -> Vec<DrawOp> {
    let is_overlay = |pane: &PaneId| overlays.iter().any(|group| group.panes.contains(pane));
    // Without overlays (the common frame) the split layout is the whole
    // layout, so it is borrowed rather than copied.
    let base: Cow<'_, [(PaneId, PaneRect)]> = if overlays.is_empty() {
        Cow::Borrowed(layout)
    } else {
        layout
            .iter()
            .filter(|(pane, _)| !is_overlay(pane))
            .copied()
            .collect()
    };
    // The transition pane with its bind-group index and revealed rect.
    let fx = fx.and_then(|(fx_pane, clip)| {
        layout
            .iter()
            .enumerate()
            .find(|(_, (pane, _))| *pane == fx_pane && !is_overlay(pane))
            .map(|(index, (pane, rect))| (*pane, index, clip.intersect(*rect)))
    });
    let zoomed = zoomed
        .filter(|_| fx.is_none())
        .filter(|pane| base.iter().any(|(candidate, _)| candidate == pane));
    let mut plan = Vec::with_capacity(layout.len() + overlays.len() + 4);
    plan.push(DrawOp::Clear);

    for (index, (pane, rect)) in layout.iter().enumerate() {
        if is_overlay(pane)
            || zoomed.is_some_and(|zoomed| zoomed != *pane)
            || fx.is_some_and(|(fx_pane, _, _)| fx_pane == *pane)
        {
            continue;
        }

        plan.push(DrawOp::PaneCells {
            pane: *pane,
            scissor: *rect,
            bind_group_index: index,
        });
    }

    if let Some((pane, index, revealed)) = fx {
        plan.push(DrawOp::ZoomUnderlay { rect: revealed });
        plan.push(DrawOp::PaneCells {
            pane,
            scissor: revealed,
            bind_group_index: index,
        });
    }

    let rects = if zoomed.is_some() || fx.is_some() {
        Vec::new()
    } else {
        divider_rects(&base)
    };
    plan.push(DrawOp::Dividers { rects });

    if fx.is_none()
        && let Some((pane, rects)) = focus_indicator(&base, focused, zoomed, force_ring)
    {
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

/// The layout a split-zoom frame's uniforms and kitty-image quads are
/// resolved against: `fx.pane`'s rect keeps its size but moves to
/// `fx.content_origin`, so its grid is drawn anchored to the clip's top-left
/// corner. Only the origin moves, so the grid size and balanced padding
/// (functions of the rect's size) are unchanged. The draw plan keeps the real
/// layout, whose rect bounds the scissor.
pub(crate) fn zoom_fx_uniform_layout(
    layout: &[(PaneId, PaneRect)],
    fx: &SplitZoomFx,
) -> Vec<(PaneId, PaneRect)> {
    layout
        .iter()
        .map(|&(pane, rect)| {
            if pane == fx.pane {
                let (x, y) = fx.content_origin;
                (pane, PaneRect::new(x, y, rect.w, rect.h))
            } else {
                (pane, rect)
            }
        })
        .collect()
}

/// The opacity `pane` draws at: `fx.sibling_alpha` for a split pane other
/// than the transition pane, opaque for the transition pane, overlay panes
/// and every pane when no transition is live.
pub(crate) fn zoom_fx_pane_alpha(
    fx: Option<&SplitZoomFx>,
    overlays: &[OverlayGroup],
    pane: PaneId,
) -> f32 {
    match fx {
        Some(fx)
            if fx.pane != pane && !overlays.iter().any(|group| group.panes.contains(&pane)) =>
        {
            fx.sibling_alpha
        }
        _ => 1.0,
    }
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
                | DrawOp::OverlayFrame { .. }
                | DrawOp::ZoomUnderlay { .. } => None,
            })
            .collect()
    }

    /// A compact per-op label, so a whole plan's order reads as one list.
    fn op_kinds(plan: &[DrawOp]) -> Vec<String> {
        plan.iter()
            .map(|op| match op {
                DrawOp::Clear => "clear".to_string(),
                DrawOp::PaneCells {
                    pane,
                    bind_group_index,
                    ..
                } => format!("cells:{}@{bind_group_index}", pane.get()),
                DrawOp::Dividers { rects } => format!("dividers:{}", rects.len()),
                DrawOp::FocusIndicator { pane, .. } => format!("focus:{}", pane.get()),
                DrawOp::OverlayFrame { .. } => "frame".to_string(),
                DrawOp::ZoomUnderlay { .. } => "underlay".to_string(),
            })
            .collect()
    }

    #[test]
    fn rect_intersection_is_the_overlap_or_zero_sized() {
        let a = PaneRect::new(10, 10, 100, 50);
        assert_eq!(
            a.intersect(PaneRect::new(50, 0, 100, 30)),
            PaneRect::new(50, 10, 60, 20)
        );
        assert_eq!(a.intersect(a), a);
        assert_eq!(
            a.intersect(PaneRect::new(0, 0, 400, 400)),
            a,
            "a rect inside another intersects to itself"
        );
        let apart = a.intersect(PaneRect::new(200, 100, 10, 10));
        assert_eq!((apart.w, apart.h), (0, 0));
    }

    // Split-zoom transition: siblings first in layout order, then the
    // underlay, then the transition pane last, both on `clip ∩ rect`; no
    // dividers and no ring even when focus and `force_ring` would draw one.
    #[test]
    fn zoom_fx_plan_draws_siblings_then_underlay_then_the_pane_last() {
        let tile_a = PaneRect::new(0, 0, 49, 40);
        let full = PaneRect::new(0, 0, 100, 40);
        let tile_c = PaneRect::new(50, 21, 50, 19);
        let a = PaneId::new(1);
        let p = PaneId::new(2);
        let c = PaneId::new(3);
        // The transition pane's layout rect is already the full rect.
        let layout = [(a, tile_a), (p, full), (c, tile_c)];
        let clip = PaneRect::new(25, 10, 75, 30);

        for focused in [None, Some(a), Some(p)] {
            for force_ring in [false, true] {
                let plan = build_draw_plan_with_zoom_fx(
                    &layout,
                    &[],
                    focused,
                    None,
                    force_ring,
                    Some((p, clip)),
                );
                assert_eq!(
                    op_kinds(&plan),
                    [
                        "clear",
                        "cells:1@0",
                        "cells:3@2",
                        "underlay",
                        "cells:2@1",
                        "dividers:0"
                    ],
                    "focused={focused:?} ring={force_ring}"
                );
                assert_eq!(plan[3], DrawOp::ZoomUnderlay { rect: clip });
                assert_eq!(
                    pane_cells(&plan),
                    vec![(a, tile_a, 0), (c, tile_c, 2), (p, clip, 1)]
                );
            }
        }
    }

    #[test]
    fn zoom_fx_scissor_is_the_clip_cut_to_the_pane_rect() {
        let p = PaneId::new(2);
        let rect = PaneRect::new(10, 0, 90, 40);
        let layout = [(PaneId::new(1), PaneRect::new(0, 0, 49, 40)), (p, rect)];
        // A clip reaching past the pane's rect on the left and bottom.
        let clip = PaneRect::new(0, 20, 60, 100);
        let revealed = PaneRect::new(10, 20, 50, 20);

        let plan = build_draw_plan_with_zoom_fx(&layout, &[], None, None, false, Some((p, clip)));

        assert!(plan.contains(&DrawOp::ZoomUnderlay { rect: revealed }));
        assert_eq!(pane_cells(&plan).last(), Some(&(p, revealed, 1)));
    }

    #[test]
    fn zoom_fx_ignores_zoomed_and_keeps_overlay_groups_after() {
        let a = PaneId::new(1);
        let p = PaneId::new(2);
        let float = PaneId::new(10);
        let layout = [
            (a, PaneRect::new(0, 0, 49, 40)),
            (p, PaneRect::new(0, 0, 100, 40)),
            (float, PaneRect::new(20, 5, 30, 10)),
        ];
        let overlays = [OverlayGroup {
            outer: PaneRect::new(20, 5, 30, 10),
            header: None,
            panes: vec![float],
            focused: false,
        }];
        let clip = PaneRect::new(50, 0, 50, 40);

        for zoomed in [None, Some(a), Some(p)] {
            let plan = build_draw_plan_with_zoom_fx(
                &layout,
                &overlays,
                Some(p),
                zoomed,
                false,
                Some((p, clip)),
            );
            assert_eq!(
                op_kinds(&plan),
                [
                    "clear",
                    "cells:1@0",
                    "underlay",
                    "cells:2@1",
                    "dividers:0",
                    "frame",
                    "cells:10@2"
                ],
                "zoomed={zoomed:?}"
            );
        }
    }

    // `fx = None` is exactly the overlay plan, and an `fx` naming a pane the
    // split layout does not hold (absent, or an overlay pane) falls back to
    // it, across every layout / focus / zoom / ring / overlay combination.
    #[test]
    fn zoom_fx_none_or_absent_pane_is_the_plain_plan() {
        let layout = [
            (PaneId::new(1), PaneRect::new(0, 0, 499, 299)),
            (PaneId::new(2), PaneRect::new(500, 0, 500, 299)),
            (PaneId::new(3), PaneRect::new(0, 300, 1000, 300)),
            (PaneId::new(10), PaneRect::new(100, 100, 200, 100)),
        ];
        let overlay_sets: [&[OverlayGroup]; 2] = [
            &[],
            &[OverlayGroup {
                outer: PaneRect::new(100, 100, 200, 100),
                header: None,
                panes: vec![PaneId::new(10)],
                focused: true,
            }],
        ];
        let ids = [
            None,
            Some(PaneId::new(1)),
            Some(PaneId::new(3)),
            Some(PaneId::new(7)),
        ];
        let clip = PaneRect::new(10, 10, 300, 200);
        for overlays in overlay_sets {
            let fallbacks = if overlays.is_empty() {
                vec![PaneId::new(7)]
            } else {
                vec![PaneId::new(7), PaneId::new(10)]
            };
            for layout in [&layout[..0], &layout[..1], &layout[..3], &layout[..]] {
                for focused in ids {
                    for zoomed in ids {
                        for force_ring in [false, true] {
                            let plain = build_draw_plan_with_overlays(
                                layout, overlays, focused, zoomed, force_ring,
                            );
                            let ctx = format!(
                                "layout={layout:?} overlays={} focused={focused:?} zoomed={zoomed:?} ring={force_ring}",
                                overlays.len()
                            );
                            assert_eq!(
                                build_draw_plan_with_zoom_fx(
                                    layout, overlays, focused, zoomed, force_ring, None
                                ),
                                plain,
                                "{ctx}"
                            );
                            for absent in &fallbacks {
                                assert_eq!(
                                    build_draw_plan_with_zoom_fx(
                                        layout,
                                        overlays,
                                        focused,
                                        zoomed,
                                        force_ring,
                                        Some((*absent, clip)),
                                    ),
                                    plain,
                                    "{ctx} fx={absent:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zoom_fx_uniform_layout_translates_only_the_transition_pane() {
        let a = PaneId::new(1);
        let p = PaneId::new(2);
        let layout = [
            (a, PaneRect::new(0, 0, 49, 40)),
            (p, PaneRect::new(0, 0, 100, 40)),
        ];
        let fx = SplitZoomFx {
            pane: p,
            clip: PaneRect::new(25, 3, 75, 37),
            content_origin: (25, 3),
            sibling_alpha: 0.5,
        };

        assert_eq!(
            zoom_fx_uniform_layout(&layout, &fx),
            vec![
                (a, PaneRect::new(0, 0, 49, 40)),
                (p, PaneRect::new(25, 3, 100, 40)),
            ]
        );
    }

    #[test]
    fn zoom_fx_alpha_fades_split_siblings_only() {
        let a = PaneId::new(1);
        let p = PaneId::new(2);
        let float = PaneId::new(10);
        let overlays = [OverlayGroup {
            outer: PaneRect::new(0, 0, 10, 10),
            header: None,
            panes: vec![float],
            focused: false,
        }];
        let fx = SplitZoomFx {
            pane: p,
            clip: PaneRect::new(0, 0, 10, 10),
            content_origin: (0, 0),
            sibling_alpha: 0.25,
        };

        assert_eq!(zoom_fx_pane_alpha(Some(&fx), &overlays, a), 0.25);
        assert_eq!(zoom_fx_pane_alpha(Some(&fx), &overlays, p), 1.0);
        assert_eq!(zoom_fx_pane_alpha(Some(&fx), &overlays, float), 1.0);
        assert_eq!(zoom_fx_pane_alpha(None, &overlays, a), 1.0);
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
        let plan =
            build_draw_plan_with_overlays(&layout, &overlays, Some(content), Some(split_b), false);
        let kinds: Vec<String> = plan
            .iter()
            .map(|op| match op {
                DrawOp::Clear => "clear".to_string(),
                DrawOp::PaneCells {
                    pane,
                    bind_group_index,
                    ..
                } => {
                    format!("cells:{}@{bind_group_index}", pane.get())
                }
                DrawOp::Dividers { rects } => format!("dividers:{}", rects.len()),
                DrawOp::FocusIndicator { .. } => "focus".to_string(),
                DrawOp::OverlayFrame {
                    backdrop,
                    border,
                    focused,
                    ..
                } => {
                    format!(
                        "frame:{}x{}:{}:{focused}",
                        backdrop.w,
                        backdrop.h,
                        border.len()
                    )
                }
                DrawOp::ZoomUnderlay { .. } => "underlay".to_string(),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "clear",
                "cells:2@1",
                "dividers:0",
                "frame:300x200:4:true",
                "cells:10@2",
                "cells:11@3"
            ]
        );

        let unzoomed =
            build_draw_plan_with_overlays(&layout, &overlays, Some(split_a), None, false);
        assert!(
            unzoomed
                .iter()
                .any(|op| matches!(op, DrawOp::Dividers { rects } if rects.len() == 1))
        );
        assert!(
            unzoomed
                .iter()
                .any(|op| matches!(op, DrawOp::FocusIndicator { pane, .. } if *pane == split_a))
        );
        assert_eq!(build_draw_plan(&layout[..2], Some(split_a), None, false), {
            let mut base = unzoomed.clone();
            base.truncate(5);
            base
        });
    }

    // An overlay group that names no layout pane and has no area changes
    // nothing: the plan equals the overlay-free one for every zoom / focus /
    // ring combination, including a zoomed or focused pane that is absent.
    #[test]
    fn inert_overlay_group_leaves_the_plan_unchanged() {
        let layout = [
            (PaneId::new(1), PaneRect::new(0, 0, 499, 299)),
            (PaneId::new(2), PaneRect::new(500, 0, 500, 299)),
            (PaneId::new(3), PaneRect::new(0, 300, 1000, 300)),
        ];
        let inert = [OverlayGroup {
            outer: PaneRect::new(10, 10, 0, 0),
            header: None,
            panes: vec![PaneId::new(99)],
            focused: true,
        }];
        let ids = [
            None,
            Some(PaneId::new(1)),
            Some(PaneId::new(3)),
            Some(PaneId::new(7)),
        ];
        for layout in [&layout[..0], &layout[..1], &layout[..]] {
            for focused in ids {
                for zoomed in ids {
                    for force_ring in [false, true] {
                        assert_eq!(
                            build_draw_plan_with_overlays(
                                layout, &inert, focused, zoomed, force_ring
                            ),
                            build_draw_plan_with_overlays(layout, &[], focused, zoomed, force_ring),
                            "layout={layout:?} focused={focused:?} zoomed={zoomed:?} ring={force_ring}"
                        );
                    }
                }
            }
        }
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
