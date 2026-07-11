use super::*;
use noa_config::{BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosTitlebarStyle};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const FONT_SIZE_MIN: f32 = 6.0;

/// Opens in Theme mode by default. Row-editing tests go through
/// [`settings_init`] instead, which starts them already on
/// `Section::SettingsRows` — a session's section is fixed for its whole
/// lifetime now (DEC-2), so there is no longer a `toggle_section` call that
/// can move a Theme-mode session onto the rows.
fn init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        current_theme: "3024 Day".to_string(),
        font_size: 14.0,
        cursor_style: CursorShape::Block,
        background_opacity: 1.0,
        background_blur_radius: 0,
        background_image: String::new(),
        background_image_opacity: 1.0,
        background_image_position: BackgroundImagePosition::Center,
        background_image_fit: BackgroundImageFit::Contain,
        background_image_repeat: false,
        background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
        window_padding_x: 2.0,
        window_padding_y: 2.0,
        macos_titlebar_style: MacosTitlebarStyle::Native,
        sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
        // Matches `noa_config::DEFAULT_QUICK_TERMINAL_SIZE`'s 40% primary —
        // this row only ever edits a plain fraction (see
        // `quick_terminal_height_fraction` at the `App` layer).
        quick_terminal_size: 0.4,
        confirm_quit: true,
        font_family: "Menlo".to_string(),
        available_font_families: vec![
            "Menlo".to_string(),
            "Monaco".to_string(),
            "Courier New".to_string(),
        ],
        theme_pair: None,
    }
}

/// R-34/ADR-4: an `init()` variant opened under a `theme = light:X,dark:Y`
/// pair, with `current_theme` already resolved to the active side — mirrors
/// what `App::open_theme_settings` hands `ThemeSettings::open` post-FM-01.
fn theme_pair_init(active_is_light: bool, light: &str, dark: &str) -> ThemeSettingsInit {
    ThemeSettingsInit {
        current_theme: if active_is_light { light } else { dark }.to_string(),
        theme_pair: Some(ThemePairContext {
            active_is_light,
            light: light.to_string(),
            dark: dark.to_string(),
        }),
        ..init()
    }
}

fn assert_quick_terminal_height(draft: &RowDraft, expected: f32) {
    let RowDraft::QuickTerminalHeight(actual) = draft else {
        panic!("expected quick terminal height draft, got {draft:?}");
    };
    assert!((*actual - expected).abs() < 0.001, "got {actual}");
}

fn settings_init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        ..init()
    }
}

fn transparent_init() -> ThemeSettingsInit {
    ThemeSettingsInit {
        background_opacity: 0.9,
        ..settings_init()
    }
}

fn move_to_row(settings: &mut ThemeSettings, row: SettingsRowKind) {
    assert_eq!(
        settings.section(),
        Section::SettingsRows,
        "move_to_row requires a Settings-mode session"
    );
    let target = row_index(row);
    while settings.selected_row() < target {
        settings.move_down();
    }
    while settings.selected_row() > target {
        settings.move_up();
    }
}

fn row_index(row: SettingsRowKind) -> usize {
    SettingsRowKind::ALL
        .iter()
        .position(|kind| *kind == row)
        .expect("row kind is present in SettingsRowKind::ALL")
}

// AC-3 (R-5): the sample pane's data carries all 16 ANSI slots plus
// fg/bg/cursor/selection plus a truecolor sample, for a known theme.
#[test]
fn sample_swatches_cover_ansi_and_semantic_and_truecolor() {
    let theme = noa_theme::resolve("3024 Day").expect("bundled theme exists");
    let swatches = sample_swatches(theme);

    let ansi_count = swatches
        .iter()
        .filter(|s| matches!(s, Swatch::Ansi(_, _)))
        .count();
    assert_eq!(ansi_count, 16);
    for i in 0..16u8 {
        assert!(
            swatches
                .iter()
                .any(|s| matches!(s, Swatch::Ansi(idx, color) if *idx == i && *color == theme.palette[i as usize])),
            "missing ANSI slot {i}"
        );
    }
    assert!(swatches.contains(&Swatch::Foreground(theme.default_fg)));
    assert!(swatches.contains(&Swatch::Background(theme.default_bg)));
    assert!(swatches.contains(&Swatch::Cursor(theme.cursor)));
    assert!(swatches.contains(&Swatch::Selection(theme.selection_bg)));
    assert!(swatches.iter().any(|s| matches!(s, Swatch::Truecolor(_))));
}

// AC-21-adjacent (R-1): opening seeds the picker with the initial
// highlight on the currently active theme and previews nothing yet.
#[test]
fn open_highlights_current_theme_and_previews_nothing_until_moved() {
    let settings = ThemeSettings::open(init());
    assert_eq!(settings.highlighted_theme_name(), Some("3024 Day"));
    assert!(!settings.should_preview());
    assert!(!settings.badge_visible());
    assert_eq!(settings.section(), Section::ThemePicker);
}

// DEC-2 (theme-settings-ui split): a Theme-mode session's section is
// permanently `ThemePicker` — Tab has nothing to toggle to, and ←→ stays a
// no-op since the settings rows don't exist in this session at all.
#[test]
fn theme_mode_session_never_reaches_settings_rows_and_tab_is_a_no_op() {
    let mut settings = ThemeSettings::open(init());
    assert_eq!(settings.section(), Section::ThemePicker);

    settings.toggle_section();
    assert_eq!(
        settings.section(),
        Section::ThemePicker,
        "Tab has nothing to toggle to in Theme mode"
    );

    // ←→ is a no-op while the theme list owns the (only) section.
    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert!(settings.rows().iter().all(|row| !row.touched));

    // ↑↓ still navigates the theme highlight as before.
    let initial_highlight = settings.highlighted_index();
    settings.move_down();
    assert_eq!(settings.highlighted_index(), initial_highlight + 1);
    assert!(settings.should_preview());
}

// DEC-2: a Settings-mode session's section is permanently `SettingsRows` —
// Tab has nothing to toggle to, and ↑↓ always navigates row selection, never
// a (nonexistent) theme highlight.
#[test]
fn settings_mode_session_never_reaches_theme_picker_and_tab_is_a_no_op() {
    let mut settings = ThemeSettings::open(settings_init());
    assert_eq!(settings.section(), Section::SettingsRows);
    assert_eq!(settings.selected_row(), 0);

    settings.toggle_section();
    assert_eq!(
        settings.section(),
        Section::SettingsRows,
        "Tab has nothing to toggle to in Settings mode"
    );

    settings.move_down();
    settings.move_down();
    assert_eq!(settings.selected_row(), 2);
}

// AC-5 (R-8, R-10): adjusting the cursor-style row cycles it and reports
// an immediate-apply effect.
#[test]
fn cursor_style_row_cycles_and_applies_immediately() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::CursorStyle);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Bar));
    assert!(settings.rows()[row_index(SettingsRowKind::CursorStyle)].touched);
    assert!(settings.badge_visible());

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Underline));

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::CursorStyle(CursorShape::BlockHollow));

    // Wraps back to the front.
    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Block));
}

// AC-7a (R-11): starting opaque disables live opacity/blur apply and
// flags the restart-required note, while the draft edit itself still
// proceeds (the value can still be committed later).
#[test]
fn opaque_startup_disables_live_opacity_and_blur_but_keeps_draft() {
    let mut settings = ThemeSettings::open(settings_init()); // opacity 1.0 = opaque
    assert!(settings.opaque_at_startup());
    assert!(settings.restart_note(SettingsRowKind::BackgroundOpacity));
    assert!(settings.restart_note(SettingsRowKind::BackgroundBlurRadius));
    assert!(!settings.restart_note(SettingsRowKind::CursorStyle));

    settings.move_down(); // row 1: BackgroundOpacity

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None, "no live apply while opaque");
    assert_eq!(
        settings.rows()[1].draft,
        RowDraft::BackgroundOpacity(1.0),
        "already at the 1.0 ceiling, so the draft itself doesn't move"
    );

    // A transparent start does apply live.
    let mut transparent = ThemeSettings::open(transparent_init());
    assert!(!transparent.opaque_at_startup());
    transparent.move_down();
    let effect = transparent.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::Opacity(0.95));
}

// Amended spec (UX consistency): the commit-only rows persist to
// config on commit but only take effect on the next launch, same as
// opaque-startup opacity/blur — so a touched edit shows the same
// "applies after restart" note. Untouched, no note; and the note is
// independent of `opaque_at_startup` (a transparent-started session
// still shows it for these rows).
#[test]
fn touched_commit_only_rows_show_restart_note() {
    let mut settings = ThemeSettings::open(transparent_init());
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImage));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageOpacity));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImagePosition));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageFit));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageRepeat));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageInterval));
    assert!(!settings.restart_note(SettingsRowKind::FontFamily));
    assert!(!settings.restart_note(SettingsRowKind::WindowPadding));
    assert!(!settings.restart_note(SettingsRowKind::MacosTitlebarStyle));
    assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));
    assert!(!settings.restart_note(SettingsRowKind::QuickTerminalHeight));
    assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));

    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.adjust(1, Instant::now());
    assert!(settings.restart_note(SettingsRowKind::FontFamily));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImage));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageOpacity));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImagePosition));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageFit));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageRepeat));
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImageInterval));
    assert!(!settings.restart_note(SettingsRowKind::WindowPadding));
    assert!(!settings.restart_note(SettingsRowKind::MacosTitlebarStyle));
    assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));
    assert!(!settings.restart_note(SettingsRowKind::QuickTerminalHeight));
    assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));
}

#[test]
fn background_image_row_accepts_file_or_directory_path_text_and_commits() {
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        background_image: "/tmp/old-wallpaper.png".to_string(),
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::BackgroundImage);

    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("/tmp/old-wallpaper.png".to_string())
    );

    let now = Instant::now();
    settings.push_text("/Users/example/Pictures/noa", now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("/Users/example/Pictures/noa".to_string())
    );
    assert!(settings.rows()[row_index(SettingsRowKind::BackgroundImage)].touched);
    assert!(!settings.restart_note(SettingsRowKind::BackgroundImage));

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "background-image"),
        Some(&(
            "background-image".to_string(),
            "/Users/example/Pictures/noa".to_string()
        ))
    );
}

#[test]
fn background_image_row_backspace_edits_the_existing_path() {
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        background_image: "/tmp/wall.png".to_string(),
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::BackgroundImage);

    settings.backspace(Instant::now());

    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("/tmp/wall.pn".to_string())
    );
    assert_eq!(
        settings.commit_updates(),
        vec![("background-image".to_string(), "/tmp/wall.pn".to_string())]
    );
}

#[test]
fn background_image_display_value_marks_editing_state() {
    let draft = RowDraft::BackgroundImage("/tmp/wall.png".to_string());

    assert_eq!(
        settings_row_display_value(SettingsRowKind::BackgroundImage, &draft, false),
        "/tmp/wall.png"
    );
    assert_eq!(
        settings_row_display_value(SettingsRowKind::BackgroundImage, &draft, true),
        "/tmp/wall.png|"
    );
    assert_eq!(
        settings_row_display_value(
            SettingsRowKind::BackgroundImage,
            &RowDraft::BackgroundImage(String::new()),
            true,
        ),
        "|"
    );
}

#[test]
fn background_image_option_rows_adjust_and_commit_canonical_values() {
    let mut settings = ThemeSettings::open(settings_init());
    let now = Instant::now();

    move_to_row(&mut settings, SettingsRowKind::BackgroundImageOpacity);
    settings.adjust(-1, now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImageOpacity)].draft,
        RowDraft::BackgroundImageOpacity(0.95)
    );

    move_to_row(&mut settings, SettingsRowKind::BackgroundImagePosition);
    settings.adjust(1, now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImagePosition)].draft,
        RowDraft::BackgroundImagePosition(BackgroundImagePosition::CenterRight)
    );

    move_to_row(&mut settings, SettingsRowKind::BackgroundImageFit);
    settings.adjust(1, now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImageFit)].draft,
        RowDraft::BackgroundImageFit(BackgroundImageFit::Cover)
    );

    move_to_row(&mut settings, SettingsRowKind::BackgroundImageRepeat);
    settings.adjust(1, now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImageRepeat)].draft,
        RowDraft::BackgroundImageRepeat(true)
    );

    move_to_row(&mut settings, SettingsRowKind::BackgroundImageInterval);
    settings.adjust(1, now);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImageInterval)].draft,
        RowDraft::BackgroundImageInterval(noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS + 5)
    );

    let updates = settings.commit_updates();
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "background-image-opacity"),
        Some(&("background-image-opacity".to_string(), "0.95".to_string()))
    );
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "background-image-position"),
        Some(&(
            "background-image-position".to_string(),
            "center-right".to_string()
        ))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "background-image-fit"),
        Some(&("background-image-fit".to_string(), "cover".to_string()))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "background-image-repeat"),
        Some(&("background-image-repeat".to_string(), "true".to_string()))
    );
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "background-image-interval"),
        Some(&("background-image-interval".to_string(), "35".to_string()))
    );
}

// AC-4a: the badge is invisible until either the theme highlight moves
// or a live row is actually edited, and stays visible afterward.
#[test]
fn badge_tracks_preview_and_live_row_edits() {
    let mut settings = ThemeSettings::open(init());
    assert!(!settings.badge_visible());

    settings.move_down(); // theme highlight moves
    assert!(settings.badge_visible());
}

#[test]
fn badge_visible_from_a_live_row_edit_alone() {
    let mut settings = ThemeSettings::open(transparent_init());
    settings.move_down(); // BackgroundOpacity row
    assert!(!settings.badge_visible());
    settings.adjust(1, Instant::now());
    assert!(settings.badge_visible());
}

// touched-flag discipline: navigation alone (no value-changing key) must
// never mark any row touched, live or commit-only — proven separately per
// mode now that a session's section (and so what ↑↓ actually moves) is
// fixed for its whole lifetime (DEC-2).
#[test]
fn theme_mode_navigation_alone_never_marks_a_row_touched() {
    let mut settings = ThemeSettings::open(init());
    for _ in 0..10 {
        settings.move_down();
        settings.move_up();
    }
    assert!(settings.rows().iter().all(|row| !row.touched));
}

#[test]
fn settings_mode_navigation_alone_never_marks_a_row_touched() {
    let mut settings = ThemeSettings::open(settings_init());
    for _ in 0..10 {
        settings.move_down();
        settings.move_up();
    }
    assert!(settings.rows().iter().all(|row| !row.touched));
}

// AC-16 (R-4): filtering to zero matches empties the list without
// resetting the preview flag that a prior highlight change had already
// set — `App` simply keeps whatever `gpu.preview_theme` it last set,
// since `highlighted_theme_name` returns `None` and `App` never
// overwrites the preview on a `None`.
#[test]
fn zero_match_filter_keeps_previous_preview_state() {
    let mut settings = ThemeSettings::open(init());
    settings.move_down(); // establish a preview
    assert!(settings.should_preview());

    settings.push_text("zzzzzznosuchtheme", Instant::now());
    assert_eq!(settings.filtered_len(), 0);
    assert_eq!(settings.highlighted_theme_name(), None);
    // The flag that gates whether `App` resolves a preview at all stays
    // set — `App` just has nothing new to resolve into it this frame.
    assert!(settings.should_preview());
}

// AC-6 (R-9), exercised through the overlay's own font-size row rather
// than `Debouncer` directly (already covered in `debounce.rs`): a burst
// of ←→ presses fires once, 150ms after the last one, with the final
// value.
#[test]
fn font_size_row_debounces_a_burst_of_adjustments() {
    let mut settings = ThemeSettings::open(settings_init()); // row 0 = FontSize, already selected
    let t0 = Instant::now();

    settings.adjust(1, t0); // 14.5
    settings.adjust(1, t0 + Duration::from_millis(50)); // 15.0
    settings.adjust(1, t0 + Duration::from_millis(100)); // 15.5

    assert_eq!(
        settings.poll_font_size(t0 + Duration::from_millis(200)),
        None
    );
    assert_eq!(
        settings.poll_font_size(t0 + Duration::from_millis(250)),
        Some(15.5)
    );
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(15.5),
        "the draft tracks live, independent of when the debounce fires"
    );
}

// Direct digit entry (R-2's "数値行は直接入力も可"): typing digits sets
// the font-size row directly, and Backspace edits the same buffer.
#[test]
fn font_size_row_accepts_direct_digit_entry() {
    let mut settings = ThemeSettings::open(settings_init());
    let now = Instant::now();

    settings.push_text("2", now);
    settings.push_text("2", now);
    assert_eq!(settings.rows()[0].draft, RowDraft::FontSize(22.0));

    settings.backspace(now);
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(FONT_SIZE_MIN),
        "typed \"2\" clamps up to the row's minimum"
    );
}

// AC-8-partial (R-16), Theme mode: Esc reverts to the pre-open snapshot
// values even after the highlight has drifted — no writer/config call is
// involved at this layer at all (the pure module has no way to reach one).
#[test]
fn theme_mode_revert_returns_the_pre_open_snapshot() {
    let mut settings = ThemeSettings::open(init());
    settings.move_down(); // preview drifted

    let values = settings.revert();
    assert_eq!(values.theme_name, "3024 Day");
    assert_eq!(values.font_size, 14.0);
    assert_eq!(values.cursor_style, CursorShape::Block);
    assert_eq!(values.background_opacity, 1.0);
    assert_eq!(values.background_blur_radius, 0);
    assert_eq!(
        values.sidebar_preview_lines,
        noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES
    );
    assert_eq!(values.quick_terminal_size, 0.4);
}

// AC-8-partial (R-16), Settings mode: Esc cancels a pending font-size
// debounce so it can never fire afterward.
#[test]
fn settings_mode_revert_cancels_pending_font_size_debounce() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(1, Instant::now()); // font-size debounce now pending

    let values = settings.revert();
    assert_eq!(
        values.font_size, 14.0,
        "reports the pre-open snapshot, not the pending edit"
    );

    // The pending font-size value must never fire after revert.
    assert_eq!(
        settings.poll_font_size(Instant::now() + Duration::from_secs(1)),
        None
    );
}

// Font-family and titlebar-style rows cycle through their fixed/injected
// option sets and wrap both directions (commit-only rows still track
// touched correctly).
#[test]
fn font_family_and_titlebar_rows_cycle_and_wrap() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily("Monaco".to_string())
    );
    settings.adjust(-1, Instant::now());
    settings.adjust(-1, Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily("Courier New".to_string()),
        "wraps backward past the front"
    );

    move_to_row(&mut settings, SettingsRowKind::MacosTitlebarStyle);
    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::MacosTitlebarStyle)].draft,
        RowDraft::MacosTitlebarStyle(MacosTitlebarStyle::Transparent)
    );
    assert!(settings.rows()[row_index(SettingsRowKind::MacosTitlebarStyle)].touched);
}

// Window-padding row moves both axes together on one ←→ step (the
// documented single-row-two-values simplification).
#[test]
fn window_padding_row_adjusts_both_axes_together() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::WindowPadding);
    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::WindowPadding)].draft,
        RowDraft::WindowPadding(3.0, 3.0)
    );
}

#[test]
fn sidebar_preview_lines_row_adjusts_clamps_and_commits() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::SidebarPreviewLines);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::SidebarPreviewLines(6));
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarPreviewLines)].draft,
        RowDraft::SidebarPreviewLines(6)
    );
    assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));

    for _ in 0..20 {
        settings.adjust(1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarPreviewLines)].draft,
        RowDraft::SidebarPreviewLines(noa_config::MAX_SIDEBAR_PREVIEW_LINES)
    );
    for _ in 0..20 {
        settings.adjust(-1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarPreviewLines)].draft,
        RowDraft::SidebarPreviewLines(0)
    );

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "sidebar-preview-lines"),
        Some(&("sidebar-preview-lines".to_string(), "0".to_string()))
    );
}

#[test]
fn quick_terminal_height_row_adjusts_clamps_and_commits() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::QuickTerminalHeight);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_quick_terminal_height(
        &settings.rows()[row_index(SettingsRowKind::QuickTerminalHeight)].draft,
        0.45,
    );
    assert!(!settings.restart_note(SettingsRowKind::QuickTerminalHeight));
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::QuickTerminalHeight)]
            .draft
            .display_value(),
        "45%"
    );

    for _ in 0..20 {
        settings.adjust(1, Instant::now());
    }
    assert_quick_terminal_height(
        &settings.rows()[row_index(SettingsRowKind::QuickTerminalHeight)].draft,
        1.0,
    );
    for _ in 0..40 {
        settings.adjust(-1, Instant::now());
    }
    assert_quick_terminal_height(
        &settings.rows()[row_index(SettingsRowKind::QuickTerminalHeight)].draft,
        0.1,
    );

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "quick-terminal-size"),
        Some(&("quick-terminal-size".to_string(), "0.10".to_string()))
    );
}

#[test]
fn confirm_quit_row_toggles_and_commits_without_restart_note() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::ConfirmQuit);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::ConfirmQuit)].draft,
        RowDraft::ConfirmQuit(false)
    );
    assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "confirm-quit"),
        Some(&("confirm-quit".to_string(), "false".to_string()))
    );
}

// R-17/NFR-6, Theme mode: `commit_updates` can only ever contain the
// `theme` key now — the settings section doesn't exist in this mode, so no
// row can ever become `touched` (an untouched row's draft can equal the
// live session value even when that value came from a CLI override, but
// `commit_updates` only ever reads touched rows either way).
#[test]
fn theme_mode_commit_updates_contains_only_the_changed_theme() {
    let settings = ThemeSettings::open(init());
    // Highlight never moved: no updates at all.
    assert!(settings.commit_updates().is_empty());

    let mut settings = ThemeSettings::open(init());
    settings.move_down(); // theme highlight moves away from the snapshot

    let updates = settings.commit_updates();
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "theme")
            .map(|(_, v)| v.as_str()),
        settings.highlighted_theme_name(),
        "theme update carries the new highlight, not the snapshot"
    );
    assert_eq!(updates.len(), 1, "Theme mode can never touch a row");
}

// R-17/NFR-6, Settings mode: `commit_updates` can only ever contain
// touched-row keys — the theme picker doesn't exist in this mode, so the
// highlighted theme can never drift from the snapshot. Only a real edit
// (`touched`) makes a row eligible for the update list.
#[test]
fn settings_mode_commit_updates_never_includes_a_theme_change() {
    let settings = ThemeSettings::open(settings_init());
    // Nothing touched: no updates at all.
    assert!(settings.commit_updates().is_empty());

    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(1, Instant::now()); // touches row 0: FontSize 14.0 -> 14.5

    let updates = settings.commit_updates();
    assert!(
        !updates.iter().any(|(k, _)| k == "theme"),
        "the theme picker doesn't exist in this mode"
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "font-size"),
        Some(&("font-size".to_string(), "14.5".to_string()))
    );
    // Every other row stayed untouched and must not appear, even though
    // e.g. cursor-style's draft is a perfectly valid config value.
    assert!(!updates.iter().any(|(k, _)| k == "cursor-style"));
    assert!(!updates.iter().any(|(k, _)| k == "background-opacity"));
    assert_eq!(updates.len(), 1, "font-size only");
}

// Re-highlighting back onto the snapshot theme must not emit a `theme`
// update — `commit_updates` compares against the pre-open value, not
// "did the highlight ever move".
#[test]
fn commit_updates_omits_theme_when_highlight_returns_to_the_snapshot() {
    let mut settings = ThemeSettings::open(init());
    settings.move_down();
    settings.move_up();
    assert_eq!(settings.highlighted_theme_name(), Some("3024 Day"));
    assert!(!settings.commit_updates().iter().any(|(k, _)| k == "theme"));
}

// Window-padding is the one row that writes two keys from a single
// touched flag.
#[test]
fn commit_updates_writes_both_padding_axes_from_one_row() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::WindowPadding);
    settings.adjust(1, Instant::now());
    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "window-padding-x"),
        Some(&("window-padding-x".to_string(), "3".to_string()))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "window-padding-y"),
        Some(&("window-padding-y".to_string(), "3".to_string()))
    );
}

// AC-23: a failing injected writer records the display error, is called
// exactly once, and leaves every other observable bit of state — rows,
// touched flags, highlight/preview selection — exactly as it was. The
// production caller (`App::commit_theme_settings`) never gets a
// `Some(updates)` to act on, so it structurally cannot reach the
// theme/chrome swap either.
#[test]
fn commit_with_failing_writer_sets_error_and_changes_nothing_else() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(1, Instant::now()); // touch FontSize
    let before_rows = settings.rows().clone();
    let before_highlighted = settings.highlighted_index();
    assert!(settings.commit_error().is_none());

    let mut calls = 0;
    let mut writer = |_: &Path, _: &[(String, String)]| {
        calls += 1;
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
    };
    let result = settings.commit(Path::new("/nonexistent/noa/config"), &mut writer);

    assert!(result.is_none());
    assert_eq!(calls, 1);
    assert!(settings.commit_error().is_some());
    assert_eq!(
        *settings.rows(),
        before_rows,
        "drafts/touched untouched on failure"
    );
    assert_eq!(
        settings.highlighted_index(),
        before_highlighted,
        "preview selection untouched on failure"
    );
}

// A successful commit clears any error left over from an earlier failed
// attempt (retry-after-fix flow) and hands back exactly the updates that
// were passed to the writer.
#[test]
fn commit_success_clears_a_prior_error_and_returns_the_written_updates() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(1, Instant::now()); // touch FontSize

    let mut fail_once = true;
    let mut writer = |_: &Path, _: &[(String, String)]| {
        if fail_once {
            fail_once = false;
            Err(io::Error::other("transient"))
        } else {
            Ok(())
        }
    };
    assert!(settings.commit(Path::new("/x"), &mut writer).is_none());
    assert!(settings.commit_error().is_some());

    let result = settings.commit(Path::new("/x"), &mut writer);
    assert!(
        settings.commit_error().is_none(),
        "success clears the error"
    );
    assert_eq!(
        result,
        Some(vec![("font-size".to_string(), "14.5".to_string())])
    );
}

// AC-8: Esc (`revert`) takes no writer parameter at all, so it is
// structurally impossible for the Esc path to invoke one — this pins
// that down with an actual spy closure that stays untouched across the
// same edit sequence AC-23's failing-writer test exercises.
#[test]
fn esc_path_never_reaches_the_writer() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(1, Instant::now());

    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let spy_calls = calls.clone();
    let _writer = move |_: &Path, _: &[(String, String)]| -> io::Result<()> {
        spy_calls.set(spy_calls.get() + 1);
        Ok(())
    };

    let _ = settings.revert();
    assert_eq!(calls.get(), 0);
    assert!(
        settings.commit_error().is_none(),
        "Esc must not touch the commit-error flag either"
    );
}

// AC-14 (R-17, NFR-6) [integration, tempdir]: a config file on disk has
// `font-size = 12` (X). The session opens with a CLI-overridden runtime
// value of 20 (Y) — the overlay seeds its font-size draft from that
// live value, exactly like a real `--font-size 20` launch would. The
// user edits a *different* row (cursor-style) and commits: the written
// file must keep `font-size = 12` (X), never `20` (Y) — the CLI value
// never leaked in just because it was active. A second session then
// edits font-size itself to Z and commits: now the file must contain
// the edited value, not X or Y.
#[test]
fn ac14_cli_override_value_never_leaks_only_touched_rows_reach_disk() {
    let dir = std::env::temp_dir().join(format!(
        "noa-theme-settings-ac14-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config");
    std::fs::write(&config_path, "font-size = 12\ntheme = 3024 Day\n").unwrap();

    // Session 1: runtime font-size is 20 (as if `--font-size 20` had
    // overridden the file's 12), untouched; the user only edits
    // cursor-style.
    let mut untouched_session = ThemeSettings::open(ThemeSettingsInit {
        font_size: 20.0,
        ..settings_init()
    });
    move_to_row(&mut untouched_session, SettingsRowKind::CursorStyle);
    assert_eq!(
        SettingsRowKind::ALL[untouched_session.selected_row()],
        SettingsRowKind::CursorStyle
    );
    untouched_session.adjust(1, Instant::now());
    let mut writer =
        |path: &Path, updates: &[(String, String)]| noa_config::write_config_updates(path, updates);
    assert!(
        untouched_session
            .commit(&config_path, &mut writer)
            .is_some()
    );

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains("font-size = 12"),
        "the CLI-overridden runtime value (20) must never leak in; got: {contents:?}"
    );
    assert!(contents.contains("cursor-style = bar"));

    // Session 2: the user now edits font-size itself and commits — the
    // new value must land, replacing X.
    let mut font_session = ThemeSettings::open(ThemeSettingsInit {
        font_size: 20.0,
        ..settings_init()
    });
    font_session.adjust(2, Instant::now()); // 20.0 -> 21.0
    assert!(font_session.commit(&config_path, &mut writer).is_some());

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains("font-size = 21"),
        "the user's committed edit must land; got: {contents:?}"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

// AC-19a (NFR-1): the preview-resolution path — resolving the
// highlighted theme plus deriving the four color families R-6 calls
// out — comfortably fits the 16ms@60Hz frame budget. Timed over many
// iterations to smooth out one-off scheduling noise (the spec's Open
// Questions explicitly allow relaxing this bound if CI proves flaky).
#[test]
fn preview_resolution_path_is_well_under_one_frame_budget() {
    let mut settings = ThemeSettings::open(init());
    let overrides = crate::theme::ThemeOverrides {
        background: None,
        foreground: None,
        cursor: None,
        selection_fg: None,
        selection_bg: None,
        minimum_contrast: 1.0,
    };

    let iterations = 100;
    let start = Instant::now();
    for i in 0..iterations {
        if i % 2 == 0 {
            settings.move_down();
        } else {
            settings.move_up();
        }
        let Some(name) = settings.highlighted_theme_name().map(str::to_string) else {
            continue;
        };
        let theme = crate::theme::resolve_theme_with_overrides(Some(&name), &overrides);
        let _ = noa_render::OverlayStyle::from_theme(&theme);
    }
    let mean = start.elapsed() / iterations;
    assert!(
        mean < Duration::from_millis(16),
        "mean preview-resolution time {mean:?} exceeded the 16ms@60Hz budget"
    );
}

// ---------------------------------------------------------------------
// theme-settings-v2: Stage A (performance, ADR-1/2/3)
// ---------------------------------------------------------------------

// AC-25/ADR-1 (R-19): `ThemeSettings::clone()` — the redraw snapshot path,
// `App::redraw`'s `session.state.clone()` — must not deep-copy the
// catalog-sized `filtered` list. It shares the same `Arc` allocation
// instead, witnessed by the strong count rising on clone and falling back
// on drop rather than a second allocation appearing.
#[test]
fn clone_shares_the_filtered_list_instead_of_deep_copying_it() {
    let settings = ThemeSettings::open(init());
    let before = settings.filtered_arc_strong_count();

    let cloned = settings.clone();
    assert_eq!(settings.filtered_arc_strong_count(), before + 1);

    drop(cloned);
    assert_eq!(settings.filtered_arc_strong_count(), before);
}

// AC-28/NFR-8 (R-21, ADR-3): a burst of prefix-extending keystrokes rescans
// the full 574-entry catalog only on the first keystroke; every subsequent
// forward-extension keystroke rescans just the previous `filtered` result
// set.
#[test]
fn forward_filter_edits_never_rescan_the_full_catalog_after_the_first_keystroke() {
    let mut settings = ThemeSettings::open(init());
    take_scan_count(); // drain whatever `open()`'s initial recompute recorded

    settings.push_text("3", Instant::now());
    assert_eq!(
        take_scan_count(),
        noa_theme::THEMES.len(),
        "the first keystroke has no previous result set to narrow from"
    );

    let after_3 = settings.filtered_len();
    settings.push_text("0", Instant::now()); // "30" extends "3"
    assert_eq!(
        take_scan_count(),
        after_3,
        "a forward edit only rescans the previous filtered result set"
    );

    let after_30 = settings.filtered_len();
    settings.push_text("2", Instant::now()); // "302" extends "30"
    assert_eq!(take_scan_count(), after_30);
}

// AC-29 (R-21, ADR-3): Backspace breaks the prefix relationship, so the
// next filter falls back to a full catalog rescan rather than narrowing —
// narrowing here could hide a theme the shorter filter would have matched.
#[test]
fn backspace_falls_back_to_a_full_catalog_rescan() {
    let mut settings = ThemeSettings::open(init());
    settings.push_text("30", Instant::now());
    take_scan_count();

    settings.backspace(Instant::now()); // "3"
    assert_eq!(
        take_scan_count(),
        noa_theme::THEMES.len(),
        "backspace must fall back to a full rescan, not a narrowed one"
    );
}

// AC-60/ADR-2 (FM-02): every mutator that can change what the ViewModel
// shows must also change `view_fingerprint` — `sync_theme_settings`'s whole
// "zero rebuilds on an idempotent frame" guarantee (AC-26) depends on this
// holding for every mutator, present and future.
#[test]
fn every_mutator_that_changes_state_changes_the_fingerprint() {
    // Theme mode: highlight navigation and filter edits.
    let mut settings = ThemeSettings::open(init());
    let fp0 = settings.view_fingerprint_u64();
    settings.move_down();
    let fp1 = settings.view_fingerprint_u64();
    assert_ne!(fp0, fp1, "move_down");

    settings.push_text("3", Instant::now());
    let fp2 = settings.view_fingerprint_u64();
    assert_ne!(fp1, fp2, "push_text (filter)");

    settings.backspace(Instant::now());
    let fp3 = settings.view_fingerprint_u64();
    assert_ne!(fp2, fp3, "backspace (filter)");

    // Settings mode: row navigation plus every row kind's value-changing
    // mutator. `BackgroundOpacity`/`BackgroundImageOpacity` start at their
    // 1.0 ceiling under `transparent_init()`'s 0.9 opacity only for the
    // former — both get an explicit decrement so the edit always lands
    // regardless of clamp direction, and `BackgroundImage` (a no-op under
    // `adjust`, edited only via text entry) goes through `push_text`.
    fn exercise(settings: &mut ThemeSettings, kind: SettingsRowKind, now: Instant) {
        match kind {
            SettingsRowKind::BackgroundImage => {
                settings.push_text("x", now);
            }
            SettingsRowKind::BackgroundOpacity | SettingsRowKind::BackgroundImageOpacity => {
                settings.adjust(-1, now);
            }
            _ => {
                settings.adjust(1, now);
            }
        }
    }

    let mut settings = ThemeSettings::open(transparent_init());
    for (i, kind) in SettingsRowKind::ALL.iter().enumerate() {
        let before_nav = settings.view_fingerprint_u64();
        move_to_row(&mut settings, *kind);
        if i > 0 {
            assert_ne!(
                before_nav,
                settings.view_fingerprint_u64(),
                "move_to_row({kind:?})"
            );
        }

        let before_edit = settings.view_fingerprint_u64();
        exercise(&mut settings, *kind, Instant::now());
        assert_ne!(
            before_edit,
            settings.view_fingerprint_u64(),
            "mutator({kind:?})"
        );
    }

    // Direct digit entry into the focused font-size row.
    let mut settings = ThemeSettings::open(settings_init()); // row 0 = FontSize
    let fp0 = settings.view_fingerprint_u64();
    settings.push_text("2", Instant::now());
    let fp1 = settings.view_fingerprint_u64();
    assert_ne!(fp0, fp1, "push_text (font-size digits)");
    settings.backspace(Instant::now());
    let fp2 = settings.view_fingerprint_u64();
    assert_ne!(fp1, fp2, "backspace (font-size digits)");

    // `commit_error`: a failing commit must change the fingerprint (the
    // footer's Danger/Muted tone depends on it) — isolated from every other
    // mutator so this assertion can't pass "by accident" from an unrelated
    // change.
    let mut settings = ThemeSettings::open(settings_init());
    let fp_before = settings.view_fingerprint_u64();
    let mut writer = |_: &Path, _: &[(String, String)]| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
    };
    let _ = settings.commit(Path::new("/nonexistent/noa/config"), &mut writer);
    let fp_after = settings.view_fingerprint_u64();
    assert_ne!(fp_before, fp_after, "commit_error");
}

// ---------------------------------------------------------------------
// theme-settings-v2: Stage B (data safety, ADR-4/R-34)
// ---------------------------------------------------------------------

// AC-49/NFR-9 (R-34, ADR-4): committing a highlighted theme under a
// `light:A,dark:B` pair with Light active rewrites only the light side,
// keeping the dark side's value verbatim — never a bare `("theme", name)`
// overwrite, which would silently drop the pair syntax.
#[test]
fn commit_updates_rewrites_only_the_active_side_of_a_theme_pair() {
    let light = noa_theme::THEMES[0].0;
    let dark = noa_theme::THEMES[1].0;
    let target = noa_theme::THEMES[2].0;
    let mut settings = ThemeSettings::open(theme_pair_init(true, light, dark));
    while settings.highlighted_theme_name() != Some(target) {
        settings.move_down();
    }

    let updates = settings.commit_updates();
    let expected = format!("light:{target},dark:{dark}");
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "theme")
            .map(|(_, v)| v.as_str()),
        Some(expected.as_str())
    );
    assert!(
        !updates.iter().any(|(k, v)| k == "theme" && v == target),
        "must never emit a bare single-name overwrite for a pair config"
    );
}

// AC-50/NFR-9 (R-34, ADR-4) [integration]: AC-49's commit, written to an
// actual file via the real `write_config_updates` writer, re-parses as a
// valid `light:X,dark:Y` pair through noa-config's real public loader —
// the light side holds the new value, the dark side is byte-identical to
// what was on disk before the commit.
#[test]
fn ac50_committed_pair_round_trips_through_the_real_config_parser() {
    let dir = std::env::temp_dir().join(format!(
        "noa-theme-settings-ac50-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config");
    let light = noa_theme::THEMES[0].0;
    let dark = noa_theme::THEMES[1].0;
    let target = noa_theme::THEMES[2].0;
    std::fs::write(&config_path, format!("theme = light:{light},dark:{dark}\n")).unwrap();

    let mut settings = ThemeSettings::open(theme_pair_init(true, light, dark));
    while settings.highlighted_theme_name() != Some(target) {
        settings.move_down();
    }
    let mut writer =
        |path: &Path, updates: &[(String, String)]| noa_config::write_config_updates(path, updates);
    assert!(settings.commit(&config_path, &mut writer).is_some());

    let (overrides, diagnostics) = noa_config::load_overrides_from_path(&config_path).unwrap();
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
    assert_eq!(
        overrides.theme_appearance,
        Some(noa_config::ThemeAppearancePair {
            light: target.to_string(),
            dark: dark.to_string(),
        })
    );

    std::fs::remove_dir_all(dir).unwrap();
}

// AC-51 (R-34): unchanged behavior when `theme_pair` is `None` (a plain,
// non-paired `theme = NAME` config) — the regression guard for ADR-4's
// `else` branch.
#[test]
fn commit_updates_uses_the_plain_theme_key_when_not_a_pair() {
    let mut settings = ThemeSettings::open(init()); // theme_pair: None
    settings.move_down();
    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "theme"),
        Some(&(
            "theme".to_string(),
            settings.highlighted_theme_name().unwrap().to_string()
        ))
    );
}

// AC-55 (R-34): the pair-resolution fixture double of
// `settings_mode_commit_updates_never_includes_a_theme_change` — a
// Settings-mode session opened under a pair config (current_theme
// correctly resolved to the active side, post-FM-01) must still never emit
// a `theme` key when only a non-theme row is touched. Before FM-01, an
// unresolved (empty) `current_theme` made this fire spuriously (`filtered`
// never contains an empty-named theme, so `highlighted` never re-aligned
// onto the snapshot).
#[test]
fn settings_mode_commit_updates_never_includes_a_theme_change_under_a_pair() {
    let light = noa_theme::THEMES[0].0;
    let dark = noa_theme::THEMES[1].0;

    let settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        ..theme_pair_init(true, light, dark)
    });
    assert!(settings.commit_updates().is_empty());

    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        ..theme_pair_init(true, light, dark)
    });
    settings.adjust(1, Instant::now()); // touches FontSize only

    let updates = settings.commit_updates();
    assert!(!updates.iter().any(|(k, _)| k == "theme"));
    assert_eq!(
        updates.iter().find(|(k, _)| k == "font-size"),
        Some(&("font-size".to_string(), "14.5".to_string()))
    );
}

// AC-56 (FM-01 defense-in-depth): `highlight_moved` can never become true
// in Settings mode — the theme picker section doesn't exist in this
// session at all, so `should_preview()` (which gates whether `App`
// resolves a live preview) must stay false no matter what the user does.
// This is why a Settings-mode session can never legitimately emit a
// `theme` diff from highlight drift in the first place — FM-01's actual
// bug was in `current_theme`'s derivation, not here, but this pins the
// invariant down as a second line of defense.
#[test]
fn settings_mode_highlight_moved_is_always_false() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(!settings.should_preview());

    for kind in SettingsRowKind::ALL {
        move_to_row(&mut settings, kind);
        settings.adjust(1, Instant::now());
        assert!(
            !settings.should_preview(),
            "highlight_moved must stay false in Settings mode ({kind:?})"
        );
    }
    settings.push_text("x", Instant::now());
    settings.backspace(Instant::now());
    assert!(!settings.should_preview());
}
