use super::model::{PaneRectPt, overlay_scroll_window, theme_settings_view_model};
use crate::theme_settings::{
    Liveness, ThemeSettings, ThemeSettingsInit, ThemeSettingsMode,
};

fn settings_init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        current_theme: "3024 Day".to_string(),
        font_size: 14.0,
        cursor_style: noa_config::CursorShape::Block,
        // Opaque (1.0) by default, matching `theme_settings/tests.rs`'s own
        // `settings_init` convention — the C-6 opaque downgrade is what
        // most tests here are NOT trying to exercise, so the liveness test
        // below uses `transparent_settings_init` instead.
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
        scrollback_limit: noa_config::DEFAULT_SCROLLBACK_LIMIT,
        cursor_style_blink: None,
        minimum_contrast: noa_config::DEFAULT_MINIMUM_CONTRAST,
        macos_option_as_alt: noa_config::MacosOptionAsAlt::None,
    }
}

/// A transparent-started variant of [`settings_init`] — the C-6 opaque
/// downgrade never applies, so every row's [`Liveness`] matches the static
/// `is_live()` classification.
fn transparent_settings_init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        background_opacity: 0.9,
        ..settings_init()
    }
}

// AC-7: opening the overlay (nothing touched yet) already carries every
// row's live/next-launch/on-save classification — zero-lie display from
// the first frame. Fix F1: only the three genuine persist-only rows read
// `OnLaunch`; every other non-live row reads `OnSave`.
#[test]
fn view_model_rows_carry_liveness_before_anything_is_touched() {
    let state = ThemeSettings::open(transparent_settings_init());
    let vm = theme_settings_view_model(&state);

    assert_eq!(vm.rows.len(), crate::theme_settings::SettingsRowKind::COUNT);
    for (idx, kind) in crate::theme_settings::SettingsRowKind::ALL.iter().enumerate() {
        let expected = if kind.is_live() {
            Liveness::Live
        } else if matches!(
            kind,
            crate::theme_settings::SettingsRowKind::FontFamily
                | crate::theme_settings::SettingsRowKind::WindowPadding
                | crate::theme_settings::SettingsRowKind::MacosTitlebarStyle
                | crate::theme_settings::SettingsRowKind::MacosOptionAsAlt
        ) {
            Liveness::OnLaunch
        } else {
            Liveness::OnSave
        };
        assert_eq!(vm.rows[idx].liveness, expected, "{kind:?}");
    }
}

// AC-8: editing a live row leaves its badge classification unchanged.
#[test]
fn view_model_liveness_is_unaffected_by_editing_a_live_row() {
    let mut state = ThemeSettings::open(settings_init());
    state.adjust(1, std::time::Instant::now()); // row 0 = FontSize (live)
    let vm = theme_settings_view_model(&state);

    assert_eq!(vm.rows[0].liveness, Liveness::Live);
}

// AC-17: `selected_description` always matches
// `SettingsRowKind::ALL[selected_row].description()`, independent of
// search/highlight state.
#[test]
fn view_model_selected_description_matches_the_selected_row() {
    let mut state = ThemeSettings::open(settings_init());
    state.move_down();
    state.move_down();
    let vm = theme_settings_view_model(&state);

    assert_eq!(
        vm.selected_description,
        crate::theme_settings::SettingsRowKind::ALL[state.selected_row()].description()
    );
}

// R-5: while search is active, `settings_visible` carries the fuzzy-
// filtered subset (not the full row list), and the highlighted match is the
// row flagged `selected` — the two index spaces (search highlight vs
// `selected_row`) converge into one flag for rendering.
#[test]
fn view_model_settings_visible_reflects_the_active_search_filter() {
    let mut state = ThemeSettings::open(settings_init());
    state.toggle_settings_search();
    // "cursor style" (not a shorter prefix): `fuzzy_match` is a subsequence
    // matcher, and a shorter query also scatter-matches unrelated labels
    // (see `theme_settings::tests::search_filters_rows_by_label_fuzzy_match`'s
    // comment, including R-9's "Cursor Blink" row) — this asserts the
    // unambiguous single-match case.
    state.push_text("cursor style", std::time::Instant::now());
    let vm = theme_settings_view_model(&state);

    assert_eq!(vm.settings_visible.len(), 1);
    let row_idx = vm.settings_visible[0];
    assert_eq!(
        vm.rows[row_idx].label,
        crate::theme_settings::SettingsRowKind::CursorStyle.label()
    );
    assert!(vm.rows[row_idx].selected);
    assert!(vm.search_active);
    assert_eq!(vm.search_query, "cursor style");
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
