use super::model::{NativeOverlayCache, OverlayColors, PaneRectPt, overlay_scroll_window};
use super::sync::theme_settings_sync_decision;
use crate::theme_settings::{ThemeSettings, ThemeSettingsInit, ThemeSettingsMode};

fn test_colors() -> OverlayColors {
    OverlayColors {
        surface_fg: [1.0, 1.0, 1.0, 1.0],
        muted: [0.5, 0.5, 0.5, 1.0],
        accent: [0.2, 0.4, 0.8, 1.0],
        danger: [0.8, 0.2, 0.2, 1.0],
        selected_bg: [0.3, 0.3, 0.3, 1.0],
        surface_bg: [0.1, 0.1, 0.1, 1.0],
        border: [0.4, 0.4, 0.4, 1.0],
    }
}

fn test_rect() -> PaneRectPt {
    PaneRectPt {
        x: 0.0,
        y: 0.0,
        w: 800.0,
        h: 600.0,
    }
}

fn test_theme_settings_init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        current_theme: noa_theme::THEMES[0].0.to_string(),
        font_size: 14.0,
        cursor_style: noa_config::CursorShape::Block,
        background_opacity: 1.0,
        background_blur_radius: 0,
        background_image: String::new(),
        background_image_opacity: 1.0,
        background_image_position: noa_config::BackgroundImagePosition::Center,
        background_image_fit: noa_config::BackgroundImageFit::Contain,
        background_image_repeat: false,
        background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
        window_padding_x: 2.0,
        window_padding_y: 2.0,
        macos_titlebar_style: noa_config::MacosTitlebarStyle::Native,
        sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
        quick_terminal_size: 0.4,
        confirm_quit: true,
        font_family: "Menlo".to_string(),
        available_font_families: Vec::new(),
        theme_pair: None,
        carryover: None,
    }
}

// AC-26/NFR-7 (R-20, ADR-2): a second sync of the exact same state — the
// idempotent-frame case `App::redraw` hits every frame the overlay sits
// open without changing — must not build a new `ThemeSettingsViewModel` at
// all. `theme_settings_sync_decision` returning `None` is that witness (a
// `Some` would mean a fresh ViewModel got built).
#[test]
fn idempotent_frame_never_rebuilds_the_view_model() {
    let settings = ThemeSettings::open(test_theme_settings_init());
    let colors = test_colors();
    let mut cache = NativeOverlayCache::default();

    let first = theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert!(first.is_some(), "the first sync always builds (no prior cache)");

    let second = theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert!(
        second.is_none(),
        "an unchanged frame must not rebuild the ViewModel"
    );
}

// AC-27 (R-20): a real state change (the highlight moving) must never be
// missed by the lightweight fingerprint comparison.
#[test]
fn changed_state_always_rebuilds_the_view_model() {
    let mut settings = ThemeSettings::open(test_theme_settings_init());
    let colors = test_colors();
    let mut cache = NativeOverlayCache::default();

    theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    settings.move_down();

    let after_change =
        theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert!(
        after_change.is_some(),
        "a real highlight change must be detected and rebuild the ViewModel"
    );
}

// AC-58/NFR-7 (FM-05): the debug rebuild counter increments exactly once
// per true state change dispatched through the sync decision, and stays
// flat across idempotent frames.
#[cfg(debug_assertions)]
#[test]
fn rebuild_counter_tracks_true_state_changes_only() {
    let mut settings = ThemeSettings::open(test_theme_settings_init());
    let colors = test_colors();
    let mut cache = NativeOverlayCache::default();

    theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert_eq!(cache.theme_settings_rebuild_count(), 1);

    theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert_eq!(
        cache.theme_settings_rebuild_count(),
        1,
        "an idempotent sync must not bump the rebuild counter"
    );

    settings.move_down();
    theme_settings_sync_decision(&mut cache, Some((&settings, test_rect())), &colors);
    assert_eq!(cache.theme_settings_rebuild_count(), 2);
}

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
