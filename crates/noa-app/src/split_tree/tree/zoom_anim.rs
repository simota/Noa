//! Pure split-zoom transition math: the reveal tween, the clip/content rects
//! it implies, and the zoom-state decisions derived from it.
//!
//! Stays GUI-free (no `winit`, `wgpu`, terminal or pty) so the whole
//! transition can be unit-tested without a window.
//!
//! One value drives everything: the *reveal* `r` of the animating pane `P`
//! (0 = tile rect with siblings opaque, 1 = full rect with siblings gone).
//! The grid of `P` already has its zoomed size during a tween; only the
//! visible clip and the grid's translation animate, so text never scales.

use std::time::{Duration, Instant};

use crate::anim::{DUR_SPLIT_ZOOM, Tween, lerp};

use super::ops::contains_pane;
use super::types::{PaneId, Rect, SplitTree};

/// `full × fraction`, exact at the endpoints (`mul_f32` rounds through `f32`).
fn scaled(full: Duration, fraction: f32) -> Duration {
    Duration::from_secs_f64(full.as_secs_f64() * f64::from(fraction))
}

/// One tween segment of the animating pane's reveal, `from → to`.
///
/// `r(t) = lerp(from, to, ease_out_cubic(linear_progress(t, dur)))`.
#[derive(Clone, Copy, Debug)]
pub struct SplitZoomAnim {
    pane: PaneId,
    from: f32,
    to: f32,
    tween: Tween,
}

impl SplitZoomAnim {
    fn segment(pane: PaneId, from: f32, to: f32, now: Instant) -> Self {
        let duration = scaled(DUR_SPLIT_ZOOM, (to - from).abs());
        Self {
            pane,
            from,
            to,
            tween: Tween::new(now, duration),
        }
    }

    /// A fresh zoom-in: reveal `0 → 1` over the full duration.
    pub fn zoom_in(pane: PaneId, now: Instant) -> Self {
        Self::segment(pane, 0.0, 1.0, now)
    }

    /// A fresh unzoom: reveal `1 → 0` over the full duration.
    pub fn unzoom(pane: PaneId, now: Instant) -> Self {
        Self::segment(pane, 1.0, 0.0, now)
    }

    /// The pane the tween animates.
    pub fn pane(&self) -> PaneId {
        self.pane
    }

    /// Whether the segment heads for the zoomed state (`to == 1`).
    fn zooming_in(&self) -> bool {
        self.to > self.from
    }

    /// The reveal `r ∈ [0, 1]` at `now`.
    pub fn reveal(&self, now: Instant) -> f32 {
        lerp(self.from, self.to, self.tween.progress(now)).clamp(0.0, 1.0)
    }

    /// A segment back the way we came, starting at the current reveal and
    /// lasting exactly the distance already travelled.
    pub fn reversed(&self, now: Instant) -> Self {
        Self::segment(self.pane, self.reveal(now), 1.0 - self.to, now)
    }

    /// Whether the segment has run its full duration at `now`.
    pub fn done(&self, now: Instant) -> bool {
        self.tween.done(now)
    }

    /// The instant the segment finishes.
    pub fn end(&self) -> Instant {
        self.tween.end()
    }
}

/// Linear interpolation of two rects. Left/top and right/bottom edges are
/// interpolated and rounded separately, so edges move monotonically without
/// 1 px jitter.
pub fn lerp_rect(a: Rect, b: Rect, r: f32) -> Rect {
    let edge = |from: u32, to: u32| lerp(from as f32, to as f32, r).round() as u32;
    let x = edge(a.x, b.x);
    let y = edge(a.y, b.y);
    let right = edge(a.right(), b.right());
    let bottom = edge(a.bottom(), b.bottom());
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

/// What a frame of the tween looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomFrame {
    pub pane: PaneId,
    /// The visible rect (scissor) of the animating pane.
    pub clip: Rect,
    /// Where the animating pane's full-size grid is drawn: `clip`'s origin,
    /// the zoomed grid's size. Translated, never scaled.
    pub content: Rect,
    /// Opacity of every other pane, `1 - r`.
    pub sibling_alpha: f32,
}

/// The frame of `anim` at `now`, with `tile` the pane's rect in the plain
/// layout and `bounds` the full content rect (both re-read every frame).
pub fn zoom_frame(anim: &SplitZoomAnim, tile: Rect, bounds: Rect, now: Instant) -> ZoomFrame {
    let r = anim.reveal(now);
    let clip = lerp_rect(tile, bounds, r);
    ZoomFrame {
        pane: anim.pane,
        clip,
        content: Rect::new(clip.x, clip.y, bounds.w, bounds.h),
        sibling_alpha: 1.0 - r,
    }
}

/// The tween to store after a toggle changed the logical zoom from `prev` to
/// `next`. `reduce_motion` makes every change instant.
pub fn next_split_zoom_anim(
    prev: Option<PaneId>,
    next: Option<PaneId>,
    in_flight: Option<SplitZoomAnim>,
    reduce_motion: bool,
    now: Instant,
) -> Option<SplitZoomAnim> {
    if reduce_motion {
        return None;
    }
    match (prev, next) {
        _ if prev == next => in_flight,
        (None, Some(pane)) => match in_flight {
            Some(anim) if anim.pane == pane && !anim.zooming_in() => Some(anim.reversed(now)),
            _ => Some(SplitZoomAnim::zoom_in(pane, now)),
        },
        (Some(pane), None) => match in_flight {
            Some(anim) if anim.pane == pane && anim.zooming_in() => Some(anim.reversed(now)),
            _ => Some(SplitZoomAnim::unzoom(pane, now)),
        },
        // Zoom moving between panes is not a toggle; it is instant.
        (Some(_), Some(_)) => None,
        (None, None) => unreachable!("covered by prev == next"),
    }
}

/// The pane whose grid keeps the full content rect: the logical zoom, or the
/// animating pane while it shrinks back (its grid is resized once the tween
/// ends).
pub fn grid_zoomed(zoomed: Option<PaneId>, anim: Option<SplitZoomAnim>) -> Option<PaneId> {
    zoomed.or(anim.map(|anim| anim.pane))
}

/// The zoom the plain draw path uses: none while a tween is live (every pane
/// is drawn and the renderer applies the clip), else the logical zoom.
pub fn draw_zoomed(zoomed: Option<PaneId>, anim: Option<SplitZoomAnim>) -> Option<PaneId> {
    if anim.is_some() { None } else { zoomed }
}

/// Drop a tween whose pane left the tree (same rule as `live_zoomed`).
pub fn live_split_zoom_anim(
    tree: &SplitTree,
    anim: Option<SplitZoomAnim>,
) -> Option<SplitZoomAnim> {
    anim.filter(|anim| contains_pane(tree, anim.pane))
}

/// The hit layout during a tween: the animating pane's visible clip, first so
/// it wins over a sibling drawn under it, then every other pane unchanged.
pub fn transition_hit_layout(layout: &[(PaneId, Rect)], frame: &ZoomFrame) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::with_capacity(layout.len());
    if layout.iter().any(|(pane, _)| *pane == frame.pane) {
        out.push((frame.pane, frame.clip));
    }
    out.extend(
        layout
            .iter()
            .copied()
            .filter(|(pane, _)| *pane != frame.pane),
    );
    out
}

/// The rect pointer code maps window pixels to grid cells with: the content
/// rect for the animating pane, `surface_rect` for every other pane.
pub fn pane_mapping_rect(pane: PaneId, surface_rect: Rect, frame: &ZoomFrame) -> Rect {
    if pane == frame.pane {
        frame.content
    } else {
        surface_rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_tree::tree::{SplitOrientation, split_pane};

    const P: PaneId = PaneId::new(1);
    const Q: PaneId = PaneId::new(2);
    const TILE: Rect = Rect::new(500, 0, 500, 600);
    const BOUNDS: Rect = Rect::new(0, 0, 1000, 600);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn lerp_rect_hits_endpoints() {
        assert_eq!(lerp_rect(TILE, BOUNDS, 0.0), TILE);
        assert_eq!(lerp_rect(TILE, BOUNDS, 1.0), BOUNDS);
    }

    #[test]
    fn lerp_rect_edges_move_monotonically() {
        let (mut left, mut top) = (u32::MAX, u32::MAX);
        let (mut right, mut bottom) = (0, 0);
        let a = Rect::new(333, 71, 277, 301);
        let b = Rect::new(0, 0, 1001, 599);
        for i in 0..=200 {
            let rect = lerp_rect(a, b, i as f32 / 200.0);
            assert!(rect.x <= left && rect.y <= top, "left/top never move back");
            assert!(
                rect.right() >= right && rect.bottom() >= bottom,
                "right/bottom never move back"
            );
            left = rect.x;
            top = rect.y;
            right = rect.right();
            bottom = rect.bottom();
        }
    }

    #[test]
    fn lerp_rect_rounds_each_edge_separately() {
        // Width is round(right) - round(x), not round(width).
        let rect = lerp_rect(Rect::new(0, 0, 3, 3), Rect::new(1, 1, 3, 3), 0.5);
        assert_eq!(rect, Rect::new(1, 1, 3, 3));
        // Right/bottom stay at 3 while left/top move 1 → 0: the edges give
        // 3 - round(0.5) = 2, whereas round(lerp(2, 3, 0.5)) would give 3.
        let rect = lerp_rect(Rect::new(1, 1, 2, 2), Rect::new(0, 0, 3, 3), 0.5);
        assert_eq!(rect, Rect::new(1, 1, 2, 2));
    }

    #[test]
    fn reveal_at_start_mid_and_end() {
        let t0 = Instant::now();
        let anim = SplitZoomAnim::zoom_in(P, t0);
        assert_eq!(anim.reveal(t0), 0.0);
        let mid = anim.reveal(t0 + ms(200));
        assert!(mid > 0.5 && mid < 1.0, "ease-out is ahead of linear: {mid}");
        assert!(!anim.done(t0 + ms(399)));
        assert_eq!(anim.reveal(t0 + ms(400)), 1.0);
        assert!(anim.done(t0 + ms(400)));
        assert_eq!(anim.end(), t0 + ms(400));

        let un = SplitZoomAnim::unzoom(P, t0);
        assert_eq!(un.reveal(t0), 1.0);
        assert_eq!(un.reveal(t0 + ms(400)), 0.0);
        assert!(un.reveal(t0 + ms(200)) < 0.5);
    }

    #[test]
    fn frame_content_origin_follows_the_clip_origin() {
        let t0 = Instant::now();
        let anim = SplitZoomAnim::zoom_in(P, t0);
        for step in 0..=40 {
            let now = t0 + ms(step * 10);
            let frame = zoom_frame(&anim, TILE, BOUNDS, now);
            assert_eq!(frame.pane, P);
            assert_eq!(
                (frame.content.x, frame.content.y),
                (frame.clip.x, frame.clip.y)
            );
            assert_eq!((frame.content.w, frame.content.h), (BOUNDS.w, BOUNDS.h));
        }
        let first = zoom_frame(&anim, TILE, BOUNDS, t0);
        assert_eq!(first.clip, TILE);
        assert_eq!(first.content, Rect::new(TILE.x, TILE.y, BOUNDS.w, BOUNDS.h));
        let last = zoom_frame(&anim, TILE, BOUNDS, t0 + ms(400));
        assert_eq!(last.clip, BOUNDS);
        assert_eq!(last.content, BOUNDS);
    }

    #[test]
    fn sibling_alpha_is_one_minus_reveal() {
        let t0 = Instant::now();
        let anim = SplitZoomAnim::zoom_in(P, t0);
        assert_eq!(zoom_frame(&anim, TILE, BOUNDS, t0).sibling_alpha, 1.0);
        let now = t0 + ms(120);
        let frame = zoom_frame(&anim, TILE, BOUNDS, now);
        assert_eq!(frame.sibling_alpha, 1.0 - anim.reveal(now));
        assert_eq!(
            zoom_frame(&anim, TILE, BOUNDS, t0 + ms(400)).sibling_alpha,
            0.0
        );
    }

    #[test]
    fn reversal_starts_at_the_current_reveal_and_lasts_the_distance_travelled() {
        let t0 = Instant::now();
        let zoom = SplitZoomAnim::zoom_in(P, t0);
        let at = t0 + ms(100);
        let r = zoom.reveal(at);
        let back = zoom.reversed(at);
        assert!((back.reveal(at) - r).abs() < 1e-6);
        assert!(!back.zooming_in());
        let want = scaled(DUR_SPLIT_ZOOM, r);
        let got = back.end() - at;
        assert!(
            got.abs_diff(want) < Duration::from_micros(10),
            "{got:?} vs {want:?}"
        );
        assert_eq!(back.reveal(back.end()), 0.0);

        // Reversing an unzoom travels the remaining way back to 1.
        let un = SplitZoomAnim::unzoom(P, t0);
        let r = un.reveal(at);
        let back = un.reversed(at);
        assert!(back.zooming_in());
        assert!((back.reveal(at) - r).abs() < 1e-6);
        let want = scaled(DUR_SPLIT_ZOOM, 1.0 - r);
        assert!((back.end() - at).abs_diff(want) < Duration::from_micros(10));
        assert_eq!(back.reveal(back.end()), 1.0);
    }

    #[test]
    fn reversal_at_the_extremes() {
        let t0 = Instant::now();
        // r = 0 right after a zoom-in starts: nothing travelled, instant.
        let back = SplitZoomAnim::zoom_in(P, t0).reversed(t0);
        assert_eq!(back.reveal(t0), 0.0);
        assert!(back.done(t0));
        // r = 1 once an unzoom has not yet moved: the whole way back.
        let back = SplitZoomAnim::unzoom(P, t0).reversed(t0);
        assert_eq!(back.reveal(t0), 1.0);
        assert!(back.done(t0));
        // r = 1 at the end of a zoom-in: reversing takes the full duration.
        let end = t0 + ms(400);
        let back = SplitZoomAnim::zoom_in(P, t0).reversed(end);
        assert_eq!(back.end() - end, DUR_SPLIT_ZOOM);
        assert_eq!(back.reveal(end), 1.0);
        // r = 0 at the end of an unzoom: reversing takes the full duration.
        let back = SplitZoomAnim::unzoom(P, t0).reversed(end);
        assert_eq!(back.end() - end, DUR_SPLIT_ZOOM);
        assert_eq!(back.reveal(end), 0.0);
    }

    #[test]
    fn next_anim_table() {
        let t0 = Instant::now();
        let at = t0 + ms(100);

        // reduce_motion: instant, even with a tween in flight.
        let live_anim = SplitZoomAnim::zoom_in(P, t0);
        let live = Some(live_anim);
        assert!(next_split_zoom_anim(None, Some(P), None, true, t0).is_none());
        assert!(next_split_zoom_anim(Some(P), None, live, true, at).is_none());

        // No-op toggle keeps whatever is in flight.
        let kept = next_split_zoom_anim(Some(P), Some(P), live, false, at).unwrap();
        assert_eq!(kept.end(), live_anim.end());
        assert!(next_split_zoom_anim(None, None, None, false, at).is_none());

        // Fresh zoom-in / unzoom.
        let zoom = next_split_zoom_anim(None, Some(P), None, false, t0).unwrap();
        assert!(zoom.zooming_in() && zoom.pane() == P);
        assert_eq!(zoom.end() - t0, DUR_SPLIT_ZOOM);
        let un = next_split_zoom_anim(Some(P), None, None, false, t0).unwrap();
        assert!(!un.zooming_in() && un.pane() == P);
        assert_eq!(un.end() - t0, DUR_SPLIT_ZOOM);

        // Zoom-in in flight, toggled off: reverses.
        let rev = next_split_zoom_anim(Some(P), None, live, false, at).unwrap();
        assert!(!rev.zooming_in());
        assert!((rev.reveal(at) - live_anim.reveal(at)).abs() < 1e-6);
        // Unzoom in flight, toggled on: reverses.
        let un_anim = SplitZoomAnim::unzoom(P, t0);
        let rev = next_split_zoom_anim(None, Some(P), Some(un_anim), false, at).unwrap();
        assert!(rev.zooming_in());
        assert!((rev.reveal(at) - un_anim.reveal(at)).abs() < 1e-6);

        // A tween for another pane is replaced, not reversed.
        let other = Some(SplitZoomAnim::unzoom(Q, t0));
        let fresh = next_split_zoom_anim(None, Some(P), other, false, at).unwrap();
        assert_eq!(fresh.pane(), P);
        assert_eq!(fresh.reveal(at), 0.0);
        let other = Some(SplitZoomAnim::zoom_in(Q, t0));
        let fresh = next_split_zoom_anim(Some(P), None, other, false, at).unwrap();
        assert_eq!(fresh.pane(), P);
        assert_eq!(fresh.reveal(at), 1.0);
        // A same-direction tween for the same pane starts over too.
        let same = Some(SplitZoomAnim::zoom_in(P, t0));
        let fresh = next_split_zoom_anim(None, Some(P), same, false, at).unwrap();
        assert!(fresh.zooming_in() && fresh.reveal(at) == 0.0);

        // Zoom moving between panes is instant.
        assert!(next_split_zoom_anim(Some(P), Some(Q), live, false, at).is_none());
    }

    #[test]
    fn grid_zoomed_keeps_the_pane_through_the_shrink_and_releases_it_after() {
        let t0 = Instant::now();
        let shrink = Some(SplitZoomAnim::unzoom(P, t0));
        // Logical zoom already off, shrink live: the grid keeps the full size.
        assert_eq!(grid_zoomed(None, shrink), Some(P));
        // Tween dropped at the finish: the grid releases to its tile.
        assert_eq!(grid_zoomed(None, None), None);
        assert_eq!(grid_zoomed(Some(P), None), Some(P));
        assert_eq!(
            grid_zoomed(Some(P), Some(SplitZoomAnim::zoom_in(P, t0))),
            Some(P)
        );
    }

    #[test]
    fn draw_zoomed_is_none_while_live() {
        let t0 = Instant::now();
        let live = Some(SplitZoomAnim::zoom_in(P, t0));
        assert_eq!(draw_zoomed(Some(P), live), None);
        assert_eq!(draw_zoomed(None, Some(SplitZoomAnim::unzoom(P, t0))), None);
        assert_eq!(draw_zoomed(Some(P), None), Some(P));
        assert_eq!(draw_zoomed(None, None), None);
    }

    #[test]
    fn live_split_zoom_anim_drops_a_closed_pane() {
        let t0 = Instant::now();
        let mut tree = SplitTree::leaf(P);
        assert!(split_pane(&mut tree, P, Q, SplitOrientation::Vertical));
        let anim = Some(SplitZoomAnim::unzoom(Q, t0));
        assert!(live_split_zoom_anim(&tree, anim).is_some());
        assert!(live_split_zoom_anim(&tree, None).is_none());
        let gone = Some(SplitZoomAnim::unzoom(PaneId::new(99), t0));
        assert!(live_split_zoom_anim(&tree, gone).is_none());
    }

    #[test]
    fn transition_hit_layout_puts_the_animating_pane_first_with_the_clip() {
        let t0 = Instant::now();
        let layout = [(Q, Rect::new(0, 0, 500, 600)), (P, TILE)];
        let anim = SplitZoomAnim::zoom_in(P, t0);
        let frame = zoom_frame(&anim, TILE, BOUNDS, t0 + ms(100));
        let hit = transition_hit_layout(&layout, &frame);
        assert_eq!(hit, vec![(P, frame.clip), (Q, Rect::new(0, 0, 500, 600))]);
        // A pane that is not in the layout adds nothing.
        let only_q = transition_hit_layout(&layout[..1], &frame);
        assert_eq!(only_q, vec![layout[0]]);
    }

    #[test]
    fn pane_mapping_rect_gives_the_animating_pane_its_content_rect() {
        let t0 = Instant::now();
        let anim = SplitZoomAnim::zoom_in(P, t0);
        let frame = zoom_frame(&anim, TILE, BOUNDS, t0 + ms(100));
        assert_eq!(pane_mapping_rect(P, BOUNDS, &frame), frame.content);
        let sibling = Rect::new(0, 0, 500, 600);
        assert_eq!(pane_mapping_rect(Q, sibling, &frame), sibling);
    }

    #[test]
    fn a_point_at_the_clip_origin_plus_padding_maps_to_cell_zero_during_the_tween() {
        let t0 = Instant::now();
        let anim = SplitZoomAnim::zoom_in(P, t0);
        let frame = zoom_frame(&anim, TILE, BOUNDS, t0 + ms(100));
        assert_ne!(
            frame.clip.x, BOUNDS.x,
            "mid-tween the clip origin is not the full origin"
        );
        let (pad, cell) = (4u32, 10u32);
        let point = (frame.clip.x + pad, frame.clip.y + pad);

        let cell_in = |rect: Rect| {
            (
                (point.0 - rect.x - pad) / cell,
                (point.1 - rect.y - pad) / cell,
            )
        };
        // Through the mapping rect: the grid's top-left cell.
        assert_eq!(cell_in(pane_mapping_rect(P, BOUNDS, &frame)), (0, 0));
        // Through `surface.rect` (the full rect): a cell well inside the grid.
        let (col, _) = cell_in(BOUNDS);
        assert!(col > 10, "surface.rect maps to column {col}");
    }
}
