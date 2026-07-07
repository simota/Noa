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
    PaneRectApp::new(bounds.x + inset, bounds.y, bounds.w - inset, bounds.h)
}

/// Logical (pt) height of the standard macOS titlebar. Used to reserve the
/// titlebar band when the content view is full-size but a (transparent)
/// titlebar is still drawn over it.
pub(crate) const MACOS_TITLEBAR_LOGICAL_HEIGHT: f64 = 28.0;

/// Physical top inset the pane area must reserve for the titlebar. Only the
/// `transparent` style needs one: `native` gets the space from AppKit (the
/// content area already starts below the real titlebar). Keeps
/// `transparent`'s grid aligned with `native`.
pub(crate) fn titlebar_top_inset_px(style: noa_config::MacosTitlebarStyle, scale: f64) -> u32 {
    if !cfg!(target_os = "macos") || style != noa_config::MacosTitlebarStyle::Transparent {
        return 0;
    }
    (MACOS_TITLEBAR_LOGICAL_HEIGHT * scale).round() as u32
}

/// Physical left/right/bottom margin kept clear around the pane area under
/// the `transparent` titlebar style, so the panes read as an inset surface
/// consistent with the reserved titlebar band. Equal to the sidebar cards'
/// [`crate::sidebar::SIDEBAR_CARD_MARGIN_X`] so pane edges line up with the
/// card edges. 0 for `native` (edge-to-edge, current behavior).
pub(crate) fn content_margin_px(style: noa_config::MacosTitlebarStyle, scale: f64) -> u32 {
    if !cfg!(target_os = "macos") || style != noa_config::MacosTitlebarStyle::Transparent {
        return 0;
    }
    ((crate::sidebar::SIDEBAR_CARD_MARGIN_X as f64) * scale).round() as u32
}

/// Shrink a window's pane bounds by the transparent-titlebar chrome: `top`
/// px reserved for the titlebar band, `margin` px kept clear on the left,
/// right, and bottom edges. Zero insets return `bounds` unchanged.
pub(crate) fn content_inset_bounds(bounds: PaneRectApp, top: u32, margin: u32) -> PaneRectApp {
    let top = top.min(bounds.h);
    let bottom = margin.min(bounds.h - top);
    let side = margin.min(bounds.w / 2);
    PaneRectApp::new(
        bounds.x + side,
        bounds.y + top,
        bounds.w - 2 * side,
        bounds.h - top - bottom,
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
        "Noa".to_string()
    } else {
        title.to_string()
    }
}

/// The tab label to display: a user-set override verbatim when present
/// (tab-title REQ-TTL-5 — it masks any shell title), else the shell-driven
/// path with its `"Noa"` fallback.
pub(crate) fn resolved_tab_title(title_override: Option<&str>, shell_title: &str) -> String {
    match title_override {
        Some(title) => title.to_string(),
        None => tab_title(shell_title),
    }
}
