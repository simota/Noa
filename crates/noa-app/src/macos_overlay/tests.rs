use super::model::{PaneRectPt, overlay_scroll_window};

#[test]
fn scroll_window_clamps_and_centers() {
    // Short lists show everything.
    assert_eq!(overlay_scroll_window(5, 2, 12), (0, 5));
    // Long lists center the selection…
    assert_eq!(overlay_scroll_window(40, 20, 12), (14, 12));
    // …and clamp at both ends.
    assert_eq!(overlay_scroll_window(40, 0, 12), (0, 12));
    assert_eq!(overlay_scroll_window(40, 39, 12), (28, 12));
}

#[test]
fn pane_rect_pt_scales_from_px() {
    let rect = PaneRectPt::from_px(200, 100, 800, 600, 2.0);
    assert_eq!(rect.x, 100.0);
    assert_eq!(rect.y, 50.0);
    assert_eq!(rect.w, 400.0);
    assert_eq!(rect.h, 300.0);
}
