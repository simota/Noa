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

// -----------------------------------------------------------------------
// settings-panel-enrichment: R-1 (restart_reason), R-3 (liveness), R-5
// (search), R-6 (description), R-7 (reset). New tests only — nothing above
// this line is touched (R-8).
// -----------------------------------------------------------------------

// AC-1: opaque startup's live opacity/blur row reports `OpaqueStartup`, not
// just a bare boolean.
#[test]
fn restart_reason_opaque_startup_row_reports_opaque_startup() {
    let settings = ThemeSettings::open(settings_init()); // opacity 1.0 = opaque
    assert_eq!(
        settings.restart_reason(SettingsRowKind::BackgroundOpacity),
        RestartReason::OpaqueStartup
    );
    assert_eq!(
        settings.restart_reason(SettingsRowKind::BackgroundBlurRadius),
        RestartReason::OpaqueStartup
    );
    assert_eq!(
        settings.restart_reason(SettingsRowKind::CursorStyle),
        RestartReason::None
    );
}

// AC-2: a touched commit-only row reports `CommitOnly`, a distinct variant
// from AC-1's `OpaqueStartup` — and the `restart_note` bool wrapper (C-2,
// the 28 existing call sites above) still agrees with it.
#[test]
fn restart_reason_touched_commit_only_row_reports_commit_only_and_differs_from_opaque() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.adjust(1, Instant::now());

    assert_eq!(
        settings.restart_reason(SettingsRowKind::FontFamily),
        RestartReason::CommitOnly
    );
    assert_ne!(
        settings.restart_reason(SettingsRowKind::FontFamily),
        settings.restart_reason(SettingsRowKind::BackgroundOpacity),
    );
    assert!(settings.restart_note(SettingsRowKind::FontFamily));
}

// AC-7 analog (state-level source of truth for the view-model's badge):
// every row's `liveness()` matches its static `is_live()` classification
// outside the opaque downgrade (a transparent-started session).
#[test]
fn liveness_matches_is_live_outside_the_opaque_downgrade() {
    let settings = ThemeSettings::open(transparent_init());
    for kind in SettingsRowKind::ALL {
        let expected = if kind.is_live() {
            Liveness::Live
        } else {
            Liveness::OnLaunch
        };
        assert_eq!(settings.liveness(kind), expected, "{kind:?}");
    }
}

// AC-8: editing a live row never changes its `liveness()` classification —
// independent of `touched`, unlike `restart_reason`.
#[test]
fn liveness_is_independent_of_touched() {
    let mut settings = ThemeSettings::open(transparent_init());
    move_to_row(&mut settings, SettingsRowKind::CursorStyle);
    assert_eq!(
        settings.liveness(SettingsRowKind::CursorStyle),
        Liveness::Live
    );
    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.liveness(SettingsRowKind::CursorStyle),
        Liveness::Live,
        "liveness must not depend on touched"
    );
}

// C-6: a live-class row downgraded by an opaque startup reports its
// *effective* liveness (`OnLaunch`), not the static classification — the
// other live rows (no runtime-apply dependency on transparency) stay
// `Live` even in the same opaque session.
#[test]
fn liveness_downgrades_opaque_opacity_and_blur_to_on_launch() {
    let settings = ThemeSettings::open(settings_init()); // opaque
    assert_eq!(
        settings.liveness(SettingsRowKind::BackgroundOpacity),
        Liveness::OnLaunch
    );
    assert_eq!(
        settings.liveness(SettingsRowKind::BackgroundBlurRadius),
        Liveness::OnLaunch
    );
    assert_eq!(settings.liveness(SettingsRowKind::FontSize), Liveness::Live);
    assert_eq!(settings.liveness(SettingsRowKind::CursorStyle), Liveness::Live);
    assert_eq!(
        settings.liveness(SettingsRowKind::SidebarPreviewLines),
        Liveness::Live
    );
}

// AC-16: every row's description is non-empty and distinct from its label.
#[test]
fn every_row_has_a_nonempty_description_distinct_from_its_label() {
    for kind in SettingsRowKind::ALL {
        let description = kind.description();
        assert!(!description.is_empty(), "{kind:?}");
        assert_ne!(description, kind.label(), "{kind:?}");
    }
}

// AC-12: Tab in Settings mode enters search.
#[test]
fn tab_enters_settings_search() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(!settings.settings_search_active());
    settings.toggle_settings_search();
    assert!(settings.settings_search_active());
}

// AC-13: fuzzy-filtering by label, best match first. "cursor" (rather than
// a shorter prefix like "curs") is chosen deliberately: `fuzzy_match` is a
// subsequence matcher, and a shorter query also scatter-matches unrelated
// labels (e.g. "curs" subsequence-matches "Background Blur Radius" too) —
// this asserts the single, unambiguous match a real user query would
// produce, not `fuzzy_match`'s own scoring order (already covered by its
// existing test suite in `command_palette.rs`).
#[test]
fn search_filters_rows_by_label_fuzzy_match() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("cursor", Instant::now());
    assert_eq!(settings.settings_filtered_len(), 1);
    let idx = settings.settings_filtered_row_index(0).unwrap();
    assert_eq!(SettingsRowKind::ALL[idx], SettingsRowKind::CursorStyle);
}

// AC-14: zero matches is an empty list, and ↑↓ never panics (no-op) —
// mirrors the theme picker's own empty-filter guard.
#[test]
fn search_zero_matches_is_empty_and_navigation_is_a_no_op() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("zzzzzz", Instant::now());
    assert_eq!(settings.settings_filtered_len(), 0);
    settings.move_up();
    settings.move_down();
    assert_eq!(settings.settings_highlighted_index(), 0);
}

// AC-15: an empty query shows every row, in `ALL` order.
#[test]
fn search_empty_query_shows_every_row_in_all_order() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    assert_eq!(settings.settings_filtered_len(), SettingsRowKind::COUNT);
    for i in 0..SettingsRowKind::COUNT {
        assert_eq!(settings.settings_filtered_row_index(i), Some(i));
    }
}

// Addendum B: Enter while searching confirms the highlighted row and exits
// search (never commits — see `confirm_settings_search_never_touches_commit_state`
// below for the pure-state half of Addendum D-3/FM-02's contract).
#[test]
fn search_enter_confirms_the_highlighted_row_and_exits_search() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("cursor", Instant::now());
    assert_eq!(settings.settings_filtered_len(), 1);

    settings.confirm_settings_search();

    assert!(!settings.settings_search_active());
    assert_eq!(
        SettingsRowKind::ALL[settings.selected_row()],
        SettingsRowKind::CursorStyle
    );
}

// Addendum B: Tab while searching exits *without* confirming, restoring the
// pre-search selection — distinct from Enter's confirm-and-exit.
#[test]
fn search_tab_exit_restores_the_pre_search_selection() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.toggle_settings_search(); // enter
    settings.push_text("curs", Instant::now());
    settings.move_down(); // move the search highlight, never confirmed

    settings.toggle_settings_search(); // exit without confirming

    assert!(!settings.settings_search_active());
    assert_eq!(
        SettingsRowKind::ALL[settings.selected_row()],
        SettingsRowKind::FontFamily,
        "Tab-exit must restore the selection search started from"
    );
}

// Addendum D-3/FM-02 (pure-state half — the App-level guarantee that Enter
// mid-search routes here instead of `App::commit_theme_settings` is a
// router change, code-reviewed at its call site in
// `app/input_ops/theme_settings.rs::handle_theme_settings_key`): confirming
// a search never touches commit machinery — no `commit_error`, no row
// `touched` flips.
#[test]
fn confirm_settings_search_never_touches_commit_state() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("curs", Instant::now());

    settings.confirm_settings_search();

    assert!(settings.commit_error().is_none());
    assert!(!settings.rows()[row_index(SettingsRowKind::CursorStyle)].touched);
}

// R-5 L2's open question, resolved: entering (and exiting) search discards
// an in-progress font-size digit entry, so it can't resurrect a stale value
// on the next keystroke.
#[test]
fn search_enter_and_exit_clear_in_progress_font_size_digit_entry() {
    let mut settings = ThemeSettings::open(settings_init()); // selected_row = 0 = FontSize
    settings.push_text("1", Instant::now()); // starts a digit entry ("1")

    settings.toggle_settings_search();
    settings.toggle_settings_search(); // back out, still on the FontSize row

    settings.push_text("9", Instant::now());
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(9.0),
        "a fresh single digit, not a resumed \"19\""
    );
}

// AC-18: resetting a live row with an immediate `RowEffect` (CursorStyle)
// restores the `StartupConfig::default()` value, marks touched, and
// reports the effect for live application.
#[test]
fn reset_cursor_style_row_restores_default_and_returns_a_live_effect() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::CursorStyle);
    settings.adjust(1, Instant::now()); // Block -> Bar
    let idx = row_index(SettingsRowKind::CursorStyle);
    assert_ne!(
        settings.rows()[idx].draft,
        RowDraft::CursorStyle(noa_config::CursorShape::Block)
    );

    let effect = settings.reset_selected_row(Instant::now());

    assert_eq!(
        effect,
        RowEffect::CursorStyle(noa_config::CursorShape::Block)
    );
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::CursorStyle(noa_config::CursorShape::Block)
    );
    assert!(settings.rows()[idx].touched);
}

// AC-18 (font-size never returns a live `RowEffect` directly — see
// `RowEffect`'s doc comment): reset still restores the default and marks
// touched, routing the live apply through the existing debounce path
// exactly like `adjust` does.
#[test]
fn reset_font_size_row_restores_default_marks_touched_and_stays_debounced() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.adjust(4, Instant::now()); // +2.0 from the 14.0 fixture -> 16.0
    assert_eq!(settings.rows()[0].draft, RowDraft::FontSize(16.0));

    let effect = settings.reset_selected_row(Instant::now());

    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(noa_config::DEFAULT_FONT_SIZE)
    );
    assert!(settings.rows()[0].touched);
}

// AC-19: resetting a row whose current draft already equals the default
// (untouched) still marks it touched — an explicit reset's intent must not
// be silently dropped by `commit_updates()`'s touched-gate just because the
// value didn't move.
#[test]
fn reset_marks_touched_even_when_the_default_equals_the_current_value() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::MacosTitlebarStyle);
    let idx = row_index(SettingsRowKind::MacosTitlebarStyle);
    assert!(!settings.rows()[idx].touched);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::MacosTitlebarStyle(noa_config::MacosTitlebarStyle::Native)
    );

    settings.reset_selected_row(Instant::now());

    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::MacosTitlebarStyle(noa_config::MacosTitlebarStyle::Native),
        "value unchanged"
    );
    assert!(
        settings.rows()[idx].touched,
        "an explicit reset always marks touched, even with no value change"
    );
}

// Addendum D-3/FM-06 (compound test): `reset_selected_row` calls
// `clear_row_input_state()` (mirroring `move_up`/`move_down`) — an
// in-progress digit entry can't resurrect the pre-reset value on the next
// keystroke.
#[test]
fn reset_clears_in_progress_font_size_digit_entry() {
    let mut settings = ThemeSettings::open(settings_init()); // selected_row = 0 = FontSize
    settings.push_text("2", Instant::now());
    settings.push_text("0", Instant::now()); // digits = "20" -> 20.0
    assert_eq!(settings.rows()[0].draft, RowDraft::FontSize(20.0));

    settings.reset_selected_row(Instant::now());
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(noa_config::DEFAULT_FONT_SIZE)
    );

    settings.push_text("9", Instant::now());
    assert_eq!(
        settings.rows()[0].draft,
        RowDraft::FontSize(9.0),
        "derives from the post-reset draft, not a resumed \"20\"+\"9\" buffer"
    );
}

// Reset is scoped to the router's Delete/Cmd+Backspace gesture, which only
// ever fires outside search (Addendum D-3/FM-02's router gate) — this pure-
// state guard is the defense-in-depth backstop if that ever changes.
#[test]
fn reset_is_a_no_op_while_search_is_active() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.adjust(1, Instant::now());
    let idx = row_index(SettingsRowKind::FontFamily);
    let before = settings.rows()[idx].draft.clone();

    settings.toggle_settings_search();
    let effect = settings.reset_selected_row(Instant::now());

    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[idx].draft, before,
        "reset must no-op while search owns the keyboard"
    );
}

// C-5: Reset starts a brief flash — the only misfire-detection cue for a
// confirmation-free destructive-ish action.
#[test]
fn reset_starts_a_brief_flash_that_expires() {
    let mut settings = ThemeSettings::open(settings_init());
    let now = Instant::now();
    assert!(!settings.reset_flash_active(now));

    settings.reset_selected_row(now);

    assert!(settings.reset_flash_active(now));
    assert!(!settings.reset_flash_active(now + Duration::from_secs(1)));
}

#[test]
fn poll_reset_flash_clears_once_elapsed_and_reports_the_transition_once() {
    let mut settings = ThemeSettings::open(settings_init());
    let now = Instant::now();
    settings.reset_selected_row(now);

    assert!(
        !settings.poll_reset_flash(now),
        "still pending, no transition yet"
    );
    let later = now + Duration::from_secs(1);
    assert!(
        settings.poll_reset_flash(later),
        "elapsed: reports the transition exactly once"
    );
    assert!(
        !settings.poll_reset_flash(later),
        "already cleared: no further transition"
    );
}
