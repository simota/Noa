//! Pane/render geometry conversions and tab-title formatting.

use super::*;


pub(crate) fn pane_bounds_for_size(size: PhysicalSize<u32>) -> PaneRectApp {
    PaneRectApp::new(0, 0, size.width, size.height)
}

/// Shrink a window's pane bounds by a left-edge sidebar inset (FR-4). The
/// panes shift right by `inset` and lose that width, leaving the band free for
/// the sidebar; a zero inset returns `bounds` unchanged. Kept separate from
/// `pane_bounds_for_size` so that function's signature stays untouched
/// (Omen P1) and this stays a pure, testable transform.
pub(crate) fn sidebar_inset_bounds(bounds: PaneRectApp, inset: u32) -> PaneRectApp {
    if inset == 0 {
        return bounds;
    }
    let inset = inset.min(bounds.w);
    PaneRectApp::new(
        bounds.x + inset,
        bounds.y,
        bounds.w - inset,
        bounds.h,
    )
}

pub(crate) fn can_split_rect(rect: PaneRectApp, orientation: SplitOrientation) -> bool {
    let required = MIN_PANE_SIZE_PX
        .saturating_mul(2)
        .saturating_add(split_tree::DIVIDER_WIDTH_PX);
    match orientation {
        SplitOrientation::Horizontal => rect.w >= required,
        SplitOrientation::Vertical => rect.h >= required,
    }
}

pub(crate) fn grid_size_for_pane_rect(
    rect: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    grid_size_for_physical_size(PhysicalSize::new(rect.w, rect.h), metrics, padding)
}

pub(crate) fn split_point_from_physical_position(
    position: PhysicalPosition<f64>,
) -> Option<split_tree::Point> {
    if !position.x.is_finite() || !position.y.is_finite() || position.x < 0.0 || position.y < 0.0 {
        return None;
    }
    Some(split_tree::Point::new(
        position.x.floor().min(f64::from(u32::MAX)) as u32,
        position.y.floor().min(f64::from(u32::MAX)) as u32,
    ))
}

pub(crate) fn render_pane_id(pane_id: PaneId) -> RenderPaneId {
    RenderPaneId::new(pane_id.get())
}

pub(crate) fn render_pane_rect(rect: PaneRectApp) -> PaneRect {
    PaneRect::new(rect.x, rect.y, rect.w, rect.h)
}

pub(crate) fn visible_pane_ids(tree: &SplitTree, zoomed: Option<PaneId>) -> Vec<PaneId> {
    split_tree::zoom_decision(tree, zoomed, PaneRectApp::new(0, 0, 0, 0)).draw_panes
}

pub(crate) fn tab_title(title: &str) -> String {
    if title.is_empty() {
        "noa".to_string()
    } else {
        title.to_string()
    }
}
