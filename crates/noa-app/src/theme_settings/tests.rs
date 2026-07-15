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
        sidebar_width: noa_config::DEFAULT_SIDEBAR_WIDTH,
        sidebar_font_size: noa_config::DEFAULT_SIDEBAR_FONT_SIZE,
        // Matches `noa_config::DEFAULT_QUICK_TERMINAL_SIZE`'s 40% primary —
        // this row only ever edits a plain fraction (see
        // `quick_terminal_height_fraction` at the `App` layer).
        quick_terminal_size: 0.4,
        confirm_quit: true,
        send_selection_send_enter: false,
        font_family: "Menlo".to_string(),
        available_font_families: vec![
            "Menlo".to_string(),
            "Monaco".to_string(),
            "Courier New".to_string(),
        ],
        scrollback_limit: noa_config::DEFAULT_SCROLLBACK_LIMIT,
        cursor_style_blink: None,
        minimum_contrast: noa_config::DEFAULT_MINIMUM_CONTRAST,
        macos_option_as_alt: noa_config::MacosOptionAsAlt::None,
        server_enable: false,
        server_port: noa_config::DEFAULT_SERVER_PORT,
        server_bind: noa_config::DEFAULT_SERVER_BIND.to_string(),
        server_scopes: "read".to_string(),
        server_status: "Stopped".to_string(),
        theme_pair: None,
        carryover: None,
        favorites: std::sync::Arc::new(std::collections::HashSet::new()),
        favorites_epoch: 0,
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

/// Navigate the Theme-mode highlight to `pos` and guarantee
/// `highlight_moved` becomes true — a plain `move_down` loop is a no-op
/// (and so never flips `highlight_moved`) when `pos` is already the
/// initial highlight (e.g. `open()` auto-positioned it there), so this
/// "wiggles" once in that case without changing the final position.
fn move_highlight_to(settings: &mut ThemeSettings, pos: usize) {
    while settings.highlighted_index() < pos {
        settings.move_down();
    }
    while settings.highlighted_index() > pos {
        settings.move_up();
    }
    if !settings.should_preview() {
        settings.move_down();
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
// permanently `ThemePicker` for its whole lifetime — there is no method
// that mutates it after `open()` (R-25's Tab reopens a fresh session in the
// other mode at the `App` layer instead of toggling this session's section
// in place; see `open_theme_settings_session`/`tab_theme_settings`). ←→
// stays a no-op since the settings rows don't exist in this session at all.
#[test]
fn theme_mode_session_never_reaches_settings_rows() {
    let mut settings = ThemeSettings::open(init());
    assert_eq!(settings.section(), Section::ThemePicker);

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
// ↑↓ always navigates row selection, never a (nonexistent) theme highlight.
#[test]
fn settings_mode_session_never_reaches_theme_picker() {
    let mut settings = ThemeSettings::open(settings_init());
    assert_eq!(settings.section(), Section::SettingsRows);
    assert_eq!(settings.selected_row(), 0);

    settings.move_down();
    settings.move_down();
    assert_eq!(settings.selected_row(), 2);
}

// AC-34 (R-25): a Tab-driven mode switch carries the theme picker's filter
// text across — Settings has no filter concept of its own (nothing renders
// or edits it there), but the field survives the hop because `carryover()`
// always captures it regardless of which mode is currently open, so a
// second Tab back to Theme finds it restored.
#[test]
fn tab_carryover_restores_the_theme_filter_after_a_settings_round_trip() {
    let mut theme = ThemeSettings::open(init());
    theme.push_text("abc", Instant::now());
    assert_eq!(theme.filter(), "abc");

    let carry = theme.carryover();
    let settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        carryover: Some(carry),
        ..init()
    });
    assert_eq!(settings.section(), Section::SettingsRows);

    let carry_back = settings.carryover();
    let theme_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        carryover: Some(carry_back),
        ..init()
    });
    assert_eq!(theme_again.filter(), "abc");
}

// AC-35 (R-25): a Settings-mode `selected_row` survives a Theme round trip
// the same way the picker's filter does.
#[test]
fn tab_carryover_restores_the_settings_selected_row_after_a_theme_round_trip() {
    let mut settings = ThemeSettings::open(settings_init());
    for _ in 0..5 {
        settings.move_down();
    }
    assert_eq!(settings.selected_row(), 5);

    let carry = settings.carryover();
    let theme = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        carryover: Some(carry),
        ..init()
    });

    let carry_back = theme.carryover();
    let settings_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        carryover: Some(carry_back),
        ..init()
    });
    assert_eq!(settings_again.selected_row(), 5);
}

// AC-36 (R-25): a row touched and live-applied in one mode must still show
// as touched (and keep its edited value) after a Tab hop away and back —
// otherwise a value already live on screen would silently never reach
// `commit_updates()`'s output because the freshly reopened session would
// reseed every row as untouched. Deliberately hands the intermediate
// `ThemeSettingsInit` a *different* `font_size` than the touched draft to
// prove the carryover path ignores `init`'s live fields in favor of the
// carried rows (the real `App::tab_theme_settings` never lets these two
// diverge — this only isolates the pure state machine's contract).
#[test]
fn tab_carryover_preserves_row_values_and_touched_flags_across_the_hop() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontSize);
    settings.adjust(4, Instant::now());
    let touched_draft = settings.rows()[row_index(SettingsRowKind::FontSize)]
        .draft
        .clone();
    assert!(settings.rows()[row_index(SettingsRowKind::FontSize)].touched);

    let carry = settings.carryover();
    let theme = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        font_size: 99.0,
        carryover: Some(carry),
        ..init()
    });

    let carry_back = theme.carryover();
    let settings_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        font_size: 99.0,
        carryover: Some(carry_back),
        ..init()
    });
    assert_eq!(
        settings_again.rows()[row_index(SettingsRowKind::FontSize)].draft,
        touched_draft
    );
    assert!(settings_again.rows()[row_index(SettingsRowKind::FontSize)].touched);
}

// AC-59 (FM-04): Esc after *multiple* Tab round trips reverts to the very
// first `open()`'s snapshot, not to whichever mode happened to be open most
// recently — even when the theme highlight kept changing along the way.
#[test]
fn esc_after_multiple_tab_hops_reverts_to_the_first_open_snapshot() {
    let mut theme = ThemeSettings::open(init());
    let original_theme_name = init().current_theme;
    theme.move_down();
    assert_ne!(
        theme.highlighted_theme_name(),
        Some(original_theme_name.as_str()),
        "test setup should actually move the highlight off the original theme"
    );

    let carry1 = theme.carryover();
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        carryover: Some(carry1),
        ..init()
    });
    move_to_row(&mut settings, SettingsRowKind::FontSize);
    settings.adjust(2, Instant::now());

    let carry2 = settings.carryover();
    let mut theme_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        carryover: Some(carry2),
        ..init()
    });
    theme_again.move_down();

    let reverted = theme_again.revert();
    assert_eq!(reverted.theme_name, original_theme_name);
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
fn sidebar_width_row_adjusts_clamps_and_commits() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::SidebarWidth);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::SidebarWidth(370.0));
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarWidth)].draft,
        RowDraft::SidebarWidth(370.0)
    );
    assert!(!settings.restart_note(SettingsRowKind::SidebarWidth));

    for _ in 0..30 {
        settings.adjust(1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarWidth)].draft,
        RowDraft::SidebarWidth(noa_config::MAX_SIDEBAR_WIDTH)
    );
    for _ in 0..50 {
        settings.adjust(-1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarWidth)].draft,
        RowDraft::SidebarWidth(noa_config::MIN_SIDEBAR_WIDTH)
    );

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "sidebar-width"),
        Some(&(
            "sidebar-width".to_string(),
            format!("{}", noa_config::MIN_SIDEBAR_WIDTH)
        ))
    );
}

#[test]
fn sidebar_font_size_row_adjusts_clamps_and_commits() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::SidebarFontSize);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::SidebarFontSize(12.0));
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarFontSize)].draft,
        RowDraft::SidebarFontSize(12.0)
    );
    assert!(!settings.restart_note(SettingsRowKind::SidebarFontSize));
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarFontSize)]
            .draft
            .display_value(),
        "12.0"
    );

    for _ in 0..30 {
        settings.adjust(1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarFontSize)].draft,
        RowDraft::SidebarFontSize(noa_config::MAX_SIDEBAR_FONT_SIZE)
    );
    for _ in 0..50 {
        settings.adjust(-1, Instant::now());
    }
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SidebarFontSize)].draft,
        RowDraft::SidebarFontSize(noa_config::MIN_SIDEBAR_FONT_SIZE)
    );

    let updates = settings.commit_updates();
    assert_eq!(
        updates.iter().find(|(k, _)| k == "sidebar-font-size"),
        Some(&(
            "sidebar-font-size".to_string(),
            format!("{}", noa_config::MIN_SIDEBAR_FONT_SIZE)
        ))
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

#[test]
fn send_selection_send_enter_row_toggles_and_commits_without_restart_note() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::SendSelectionSendEnter);

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::SendSelectionSendEnter)].draft,
        RowDraft::SendSelectionSendEnter(true)
    );
    assert!(!settings.restart_note(SettingsRowKind::SendSelectionSendEnter));

    let updates = settings.commit_updates();
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "send-selection-send-enter"),
        Some(&("send-selection-send-enter".to_string(), "true".to_string()))
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
// every live row's `liveness()` matches its static `is_live()`
// classification outside the opaque downgrade (a transparent-started
// session) — updated by fix F1: a non-live row is `OnSave` when it's one
// of the reload-exempt rows `is_reload_exempt` names, `OnLaunch` only for
// the three genuine persist-only rows (see
// `liveness_reports_on_save_for_every_reload_exempt_row` for the full
// enumeration this mirrors).
#[test]
fn liveness_matches_is_live_outside_the_opaque_downgrade() {
    let settings = ThemeSettings::open(transparent_init());
    for kind in SettingsRowKind::ALL {
        let expected = if kind.is_live() {
            Liveness::Live
        } else if matches!(
            kind,
            SettingsRowKind::FontFamily
                | SettingsRowKind::WindowPadding
                | SettingsRowKind::MacosTitlebarStyle
                | SettingsRowKind::MacosOptionAsAlt
        ) {
            Liveness::OnLaunch
        } else {
            Liveness::OnSave
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
    assert_eq!(
        settings.liveness(SettingsRowKind::CursorStyle),
        Liveness::Live
    );
    assert_eq!(
        settings.liveness(SettingsRowKind::SidebarPreviewLines),
        Liveness::Live
    );
    assert_eq!(
        settings.liveness(SettingsRowKind::SidebarWidth),
        Liveness::Live
    );
    assert_eq!(
        settings.liveness(SettingsRowKind::SidebarFontSize),
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

// AC-13: fuzzy-filtering by label, best match first. "cursor style" (rather
// than a shorter prefix like "curs" or "cursor") is chosen deliberately:
// `fuzzy_match` is a subsequence matcher, and a shorter query also
// scatter-matches unrelated labels ("curs" subsequence-matches "Background
// Blur Radius" too; "cursor" also matches R-9's "Cursor Blink" row) — this
// asserts the single, unambiguous match a real user query would produce,
// not `fuzzy_match`'s own scoring order (already covered by its existing
// test suite in `command_palette.rs`).
#[test]
fn search_filters_rows_by_label_fuzzy_match() {
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("cursor style", Instant::now());
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
    settings.push_text("cursor style", Instant::now());
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
        settings.rows()[idx].draft,
        before,
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

// G1: resetting FontFamily writes nothing on commit (its default is the
// empty string, and `commit_updates()` deliberately skips an empty
// FontFamily key — fix F2) — flashing anyway would be a false "it worked"
// cue for a save that changes nothing on disk, so this one reset must not
// start the flash.
#[test]
fn reset_font_family_does_not_start_a_flash_because_it_writes_nothing_on_commit() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    let now = Instant::now();
    assert!(!settings.reset_flash_active(now));

    settings.reset_selected_row(now);

    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily(String::new())
    );
    assert!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].touched,
        "the row itself still resets and marks touched — only the flash is suppressed"
    );
    assert!(
        !settings.reset_flash_active(now),
        "no misfire cue for a reset that writes nothing on commit"
    );
    let updates = settings.commit_updates();
    assert!(!updates.iter().any(|(key, _)| key == "font-family"));
}

// G1 companion: every other row's reset still writes on commit, so it
// keeps the flash — proving the suppression is specific to FontFamily's
// empty-default case, not a general regression.
#[test]
fn reset_other_rows_still_start_a_flash() {
    for kind in [
        SettingsRowKind::CursorStyle,
        SettingsRowKind::WindowPadding,
        SettingsRowKind::MacosTitlebarStyle,
        SettingsRowKind::ScrollbackLimit,
    ] {
        let mut settings = ThemeSettings::open(settings_init());
        move_to_row(&mut settings, kind);
        let now = Instant::now();

        settings.reset_selected_row(now);

        assert!(settings.reset_flash_active(now), "{kind:?}");
    }
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

// -----------------------------------------------------------------------
// Radar (settings-panel-enrichment edge-case pass): the tests above cover
// the AC-numbered happy paths; the tests below close boundary/branch gaps
// the AC list didn't spell out (search × adjust/backspace interaction, the
// restart_reason "reload-exempt" carve-out, Reset's opaque-startup gating,
// and every RowDraft variant's default_for round-trip). New tests only.
// -----------------------------------------------------------------------

// R-7: `default_for` is the pure function every reset ultimately reads
// from — this pins its literal output against noa-config's documented
// defaults, independent of `reset_selected_row`'s own wiring (covered
// separately below), so a mismatch between the two is caught even if the
// match arms happened to agree with each other but not with the real
// default.
#[test]
fn default_for_maps_every_row_kind_to_its_documented_startup_default() {
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::FontSize),
        RowDraft::FontSize(noa_config::DEFAULT_FONT_SIZE)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundOpacity),
        RowDraft::BackgroundOpacity(1.0)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundBlurRadius),
        RowDraft::BackgroundBlurRadius(0)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImage),
        RowDraft::BackgroundImage(String::new())
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImageOpacity),
        RowDraft::BackgroundImageOpacity(1.0)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImagePosition),
        RowDraft::BackgroundImagePosition(BackgroundImagePosition::Center)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImageFit),
        RowDraft::BackgroundImageFit(BackgroundImageFit::Contain)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImageRepeat),
        RowDraft::BackgroundImageRepeat(false)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::BackgroundImageInterval),
        RowDraft::BackgroundImageInterval(noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::CursorStyle),
        RowDraft::CursorStyle(CursorShape::Block)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::FontFamily),
        RowDraft::FontFamily(String::new())
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::WindowPadding),
        RowDraft::WindowPadding(0.0, 0.0)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::MacosTitlebarStyle),
        RowDraft::MacosTitlebarStyle(MacosTitlebarStyle::Native)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::SidebarPreviewLines),
        RowDraft::SidebarPreviewLines(noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::SidebarWidth),
        RowDraft::SidebarWidth(noa_config::DEFAULT_SIDEBAR_WIDTH)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::SidebarFontSize),
        RowDraft::SidebarFontSize(noa_config::DEFAULT_SIDEBAR_FONT_SIZE)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::QuickTerminalHeight),
        RowDraft::QuickTerminalHeight(0.4)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ConfirmQuit),
        RowDraft::ConfirmQuit(true)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::SendSelectionSendEnter),
        RowDraft::SendSelectionSendEnter(false)
    );
    // R-9.
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ScrollbackLimit),
        RowDraft::ScrollbackLimit(noa_config::DEFAULT_SCROLLBACK_LIMIT)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::CursorStyleBlink),
        RowDraft::CursorStyleBlink(true)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::MinimumContrast),
        RowDraft::MinimumContrast(noa_config::DEFAULT_MINIMUM_CONTRAST)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::MacosOptionAsAlt),
        RowDraft::MacosOptionAsAlt(noa_config::MacosOptionAsAlt::None)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ServerEnable),
        RowDraft::ServerEnable(false)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ServerPort),
        RowDraft::ServerPort(noa_config::DEFAULT_SERVER_PORT)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ServerScopes),
        RowDraft::ServerScopes("read".to_string())
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ServerTokenCopy),
        RowDraft::ServerTokenCopy(TokenCopyStatus::Idle)
    );
    assert_eq!(
        RowDraft::default_for(SettingsRowKind::ServerStatus),
        RowDraft::ServerStatus("Stopped".to_string())
    );
}

// R-9: `SettingsRowKind::COUNT` is type-enforced at 20 (16 + the 4 new
// keys) via `ALL`'s array literal length — this pins the value so a future
// accidental drop of an entry fails loudly instead of silently shrinking
// the overlay. The server-settings-panel-row addition brings it to 23 (+3),
// the token-copy action row brings it to 24 (+1), the read-only status row
// (settings-panel-server-status) brings it to 25 (+1), the LAN bind-
// address row (server-bind) brings it to 26 (+1), the sidebar-width row
// brings it to 27 (+1), the sidebar-font-size row brings it to 28 (+1),
// the send-selection-send-enter row brings it to 29 (+1), and the Remote
// App QR action brings it to 30 (+1).
#[test]
fn settings_row_kind_count_includes_remote_app_qr_action() {
    assert_eq!(SettingsRowKind::COUNT, 30);
    assert_eq!(SettingsRowKind::ALL.len(), 30);
}

// settings-panel-server-status: the status row is read-only (mirrors
// `ServerTokenCopy`'s "no value" contract) — `adjust`/`reset_selected_row`
// never touch it, `commit_updates` never writes it, and `App`'s out-of-band
// `set_server_status` is the only thing that ever changes its draft.
#[test]
fn server_status_row_is_read_only_and_never_committed() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::ServerStatus));
    assert_eq!(SettingsRowKind::ServerStatus.label(), "Server Status");
    assert!(SettingsRowKind::ServerStatus.is_live());

    move_to_row(&mut settings, SettingsRowKind::ServerStatus);
    let idx = row_index(SettingsRowKind::ServerStatus);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerStatus("Stopped".to_string())
    );

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert!(!settings.rows()[idx].touched);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerStatus("Stopped".to_string()),
        "adjust must not change a read-only row's draft"
    );

    let reset_effect = settings.reset_selected_row(Instant::now());
    assert_eq!(reset_effect, RowEffect::None);
    assert!(!settings.rows()[idx].touched);

    settings.set_server_status("Running (127.0.0.1:61771, 2 client(s))".to_string());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerStatus("Running (127.0.0.1:61771, 2 client(s))".to_string())
    );
    assert!(
        !settings.rows()[idx].touched,
        "App's out-of-band refresh must never mark the row touched"
    );

    let updates = settings.commit_updates();
    assert!(
        !updates
            .iter()
            .any(|(key, _)| key.starts_with("server-status")),
        "{updates:?}"
    );
}

// settings-panel-server-status: the pure formatting function backing the
// row's display text, covering all three states.
#[test]
fn format_server_status_covers_all_three_states() {
    assert_eq!(
        format_server_status(Some(("127.0.0.1".to_string(), 61771, 0)), None),
        "Running (127.0.0.1:61771, 0 client(s))"
    );
    assert_eq!(
        format_server_status(Some(("127.0.0.1".to_string(), 8080, 3)), None),
        "Running (127.0.0.1:8080, 3 client(s))"
    );
    assert_eq!(format_server_status(None, None), "Stopped");
    assert_eq!(
        format_server_status(None, Some("address already in use")),
        "Bind failed: address already in use"
    );
    // `running` always wins over a stale `last_error`.
    assert_eq!(
        format_server_status(
            Some(("127.0.0.1".to_string(), 61771, 1)),
            Some("stale error")
        ),
        "Running (127.0.0.1:61771, 1 client(s))"
    );
}

// The bind-address interpolation is real, not hardcoded to loopback — a
// `server-bind = 0.0.0.0` LAN opt-in must show up in the status row too.
#[test]
fn format_server_status_interpolates_a_non_loopback_bind_address() {
    assert_eq!(
        format_server_status(Some(("0.0.0.0".to_string(), 61771, 2)), None),
        "Running (0.0.0.0:61771, 2 client(s))"
    );
}

// The token-copy row: an action row, not a value row (see its doc comment
// on `SettingsRowKind::ServerTokenCopy`). `adjust` must report the
// `CopyServerToken` effect for `App` to act on, without ever marking the
// row `touched` — `commit_updates` must stay empty of any server-token
// entry regardless of how many times the row is activated.
#[test]
fn server_token_copy_row_reports_effect_without_touching_or_committing() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::ServerTokenCopy));
    assert_eq!(SettingsRowKind::ServerTokenCopy.label(), "Server Token");
    assert!(SettingsRowKind::ServerTokenCopy.is_live());

    move_to_row(&mut settings, SettingsRowKind::ServerTokenCopy);
    let idx = row_index(SettingsRowKind::ServerTokenCopy);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerTokenCopy(TokenCopyStatus::Idle)
    );
    assert_eq!(
        RowDraft::ServerTokenCopy(TokenCopyStatus::Idle).display_value(),
        "Copy to clipboard"
    );

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::CopyServerToken);
    assert!(
        !settings.rows()[idx].touched,
        "an action row must never be marked touched"
    );
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerTokenCopy(TokenCopyStatus::Idle),
        "adjust alone doesn't flip display state — only App's reported outcome does"
    );

    // `App`'s reported outcome flips the display state; still never touched.
    settings.set_server_token_copy_status(TokenCopyStatus::Copied);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerTokenCopy(TokenCopyStatus::Copied)
    );
    assert_eq!(
        RowDraft::ServerTokenCopy(TokenCopyStatus::Copied).display_value(),
        "Copied \u{2713}"
    );
    assert!(!settings.rows()[idx].touched);
    assert_eq!(
        RowDraft::ServerTokenCopy(TokenCopyStatus::Failed).display_value(),
        "Copy failed"
    );

    let updates = settings.commit_updates();
    assert!(
        !updates
            .iter()
            .any(|(key, _)| key.starts_with("server-token")),
        "{updates:?}"
    );

    // Delete/⌘Backspace is a no-op on this row (no value to reset).
    let reset_effect = settings.reset_selected_row(Instant::now());
    assert_eq!(reset_effect, RowEffect::None);
    assert!(!settings.rows()[idx].touched);
}

#[test]
fn remote_app_qr_row_reports_effect_without_touching_or_committing() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::ServerRemoteAppQr);
    let idx = row_index(SettingsRowKind::ServerRemoteAppQr);

    assert_eq!(
        settings_row_display_value(
            SettingsRowKind::ServerRemoteAppQr,
            &settings.rows()[idx].draft,
            false
        ),
        "Show QR Code"
    );
    assert_eq!(
        settings.adjust(1, Instant::now()),
        RowEffect::ShowRemoteAppQr
    );
    assert!(!settings.rows()[idx].touched);
    assert!(settings.commit_updates().is_empty());
    assert_eq!(settings.reset_selected_row(Instant::now()), RowEffect::None);
}

// R-9's 6-point set, part 1/4 (scrollback-limit): ALL entry / label /
// is_live / RowDraft variant / RowEffect+apply path / restart_reason
// classification.
#[test]
fn scrollback_limit_row_is_persist_only_and_adjusts_in_one_mb_steps() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::ScrollbackLimit));
    assert_eq!(SettingsRowKind::ScrollbackLimit.label(), "Scrollback Limit");
    assert!(!SettingsRowKind::ScrollbackLimit.is_live());

    move_to_row(&mut settings, SettingsRowKind::ScrollbackLimit);
    let idx = row_index(SettingsRowKind::ScrollbackLimit);
    let RowDraft::ScrollbackLimit(before) = settings.rows()[idx].draft else {
        panic!("expected a ScrollbackLimit draft");
    };
    let effect = settings.adjust(1, Instant::now());
    assert_eq!(
        effect,
        RowEffect::None,
        "no runtime-apply path from this row"
    );
    let RowDraft::ScrollbackLimit(after) = settings.rows()[idx].draft else {
        panic!("expected a ScrollbackLimit draft");
    };
    assert_eq!(after, before + 1_000_000);
    assert!(settings.rows()[idx].touched);
    // Reload-exempt (Addendum D-1): no restart note despite being touched.
    assert_eq!(
        settings.restart_reason(SettingsRowKind::ScrollbackLimit),
        RestartReason::None
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("scrollback-limit".to_string(), after.to_string())),
        "{updates:?}"
    );
}

// G2: a config-set `scrollback-limit` above the row's own 1GB UI ceiling
// (this row's own `+` steps can never produce that, but a hand-edited
// config file can) must not have the increase key silently *decrease* it
// by clamping down to the ceiling — it's a no-op instead. The decrease key
// still works normally from any starting point.
#[test]
fn scrollback_limit_increase_is_a_no_op_above_the_ceiling_but_decrease_still_works() {
    let above_ceiling = 2_000_000_000_usize; // > SCROLLBACK_LIMIT_MAX (1GB)
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        scrollback_limit: above_ceiling,
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::ScrollbackLimit);
    let idx = row_index(SettingsRowKind::ScrollbackLimit);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ScrollbackLimit(above_ceiling)
    );

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ScrollbackLimit(above_ceiling),
        "the increase key must never decrease an already-above-ceiling value"
    );
    assert!(
        !settings.rows()[idx].touched,
        "a true no-op must not mark the row touched"
    );

    settings.adjust(-1, Instant::now());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ScrollbackLimit(above_ceiling - 1_000_000),
        "decrease still works normally from above the ceiling"
    );
    assert!(settings.rows()[idx].touched);
}

// R-9's 6-point set, part 2/4 (cursor-style-blink): a plain bool toggle.
#[test]
fn cursor_style_blink_row_is_persist_only_and_toggles() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::CursorStyleBlink));
    assert_eq!(SettingsRowKind::CursorStyleBlink.label(), "Cursor Blink");
    assert!(!SettingsRowKind::CursorStyleBlink.is_live());

    move_to_row(&mut settings, SettingsRowKind::CursorStyleBlink);
    let idx = row_index(SettingsRowKind::CursorStyleBlink);
    assert_eq!(settings.rows()[idx].draft, RowDraft::CursorStyleBlink(true));

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::CursorStyleBlink(false)
    );
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::CursorStyleBlink),
        RestartReason::None
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("cursor-style-blink".to_string(), "false".to_string())),
        "{updates:?}"
    );
}

// R-9's 6-point set, part 3/4 (minimum-contrast): steps within the
// documented 1.0..=21.0 WCAG ratio range and clamps at both ends.
#[test]
fn minimum_contrast_row_is_persist_only_and_clamps_to_the_wcag_range() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::MinimumContrast));
    assert_eq!(SettingsRowKind::MinimumContrast.label(), "Minimum Contrast");
    assert!(!SettingsRowKind::MinimumContrast.is_live());

    move_to_row(&mut settings, SettingsRowKind::MinimumContrast);
    let idx = row_index(SettingsRowKind::MinimumContrast);
    assert_eq!(settings.rows()[idx].draft, RowDraft::MinimumContrast(1.0));

    // Below the floor clamps to 1.0 (a no-op from the default, so `touched`
    // must stay false — mirrors every other clamped-at-the-floor row).
    let effect = settings.adjust(-5, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(settings.rows()[idx].draft, RowDraft::MinimumContrast(1.0));
    assert!(!settings.rows()[idx].touched);

    settings.adjust(30, Instant::now()); // far past the ceiling
    assert_eq!(settings.rows()[idx].draft, RowDraft::MinimumContrast(21.0));
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::MinimumContrast),
        RestartReason::None
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("minimum-contrast".to_string(), "21".to_string())),
        "{updates:?}"
    );
}

// R-9's 6-point set, part 4/4 (macos-option-as-alt): the genuinely
// persist-only key — cycles through the 4 modes and, unlike its 3 siblings
// above, DOES show a restart note once touched.
#[test]
fn macos_option_as_alt_row_is_genuinely_persist_only_and_cycles() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::MacosOptionAsAlt));
    assert_eq!(SettingsRowKind::MacosOptionAsAlt.label(), "Option as Alt");
    assert!(!SettingsRowKind::MacosOptionAsAlt.is_live());

    move_to_row(&mut settings, SettingsRowKind::MacosOptionAsAlt);
    let idx = row_index(SettingsRowKind::MacosOptionAsAlt);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::MacosOptionAsAlt(noa_config::MacosOptionAsAlt::None)
    );
    assert_eq!(
        settings.restart_reason(SettingsRowKind::MacosOptionAsAlt),
        RestartReason::None,
        "untouched: no note yet"
    );

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::MacosOptionAsAlt(noa_config::MacosOptionAsAlt::Left)
    );
    assert!(settings.rows()[idx].touched);
    // Genuinely persist-only: touching it DOES show a restart note, unlike
    // ScrollbackLimit/CursorStyleBlink/MinimumContrast above.
    assert_eq!(
        settings.restart_reason(SettingsRowKind::MacosOptionAsAlt),
        RestartReason::CommitOnly
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("macos-option-as-alt".to_string(), "left".to_string())),
        "{updates:?}"
    );
}

// Server settings panel rows, part 1/3 (server-enable): reload-exempt
// (ConfigWatcher's poll picks it up and restarts the server), so no restart
// note despite being touched — same shape as ScrollbackLimit/
// CursorStyleBlink/MinimumContrast above.
#[test]
fn server_enable_row_toggles_and_is_reload_exempt() {
    let mut settings = ThemeSettings::open(settings_init());
    assert!(SettingsRowKind::ALL.contains(&SettingsRowKind::ServerEnable));
    assert_eq!(SettingsRowKind::ServerEnable.label(), "Server");
    assert!(!SettingsRowKind::ServerEnable.is_live());

    move_to_row(&mut settings, SettingsRowKind::ServerEnable);
    let idx = row_index(SettingsRowKind::ServerEnable);
    assert_eq!(settings.rows()[idx].draft, RowDraft::ServerEnable(false));

    let effect = settings.adjust(1, Instant::now());
    assert_eq!(effect, RowEffect::None);
    assert_eq!(settings.rows()[idx].draft, RowDraft::ServerEnable(true));
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::ServerEnable),
        RestartReason::None
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("server-enable".to_string(), "true".to_string())),
        "{updates:?}"
    );
}

// Server settings panel rows, part 2/3 (server-port): steps by 1 and
// clamps to the documented 1024..=65535 valid TCP range.
#[test]
fn server_port_row_steps_by_one_and_clamps_to_the_valid_port_range() {
    let mut settings = ThemeSettings::open(settings_init());
    assert_eq!(SettingsRowKind::ServerPort.label(), "Server Port");
    assert!(!SettingsRowKind::ServerPort.is_live());

    move_to_row(&mut settings, SettingsRowKind::ServerPort);
    let idx = row_index(SettingsRowKind::ServerPort);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerPort(noa_config::DEFAULT_SERVER_PORT)
    );

    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerPort(noa_config::DEFAULT_SERVER_PORT + 1)
    );
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::ServerPort),
        RestartReason::None
    );
    let updates = settings.commit_updates();
    assert!(
        updates.contains(&(
            "server-port".to_string(),
            (noa_config::DEFAULT_SERVER_PORT + 1).to_string()
        )),
        "{updates:?}"
    );

    // Clamp at the floor: a session started right at 1024 must not step
    // below it.
    let mut floor = ThemeSettings::open(ThemeSettingsInit {
        server_port: 1024,
        ..settings_init()
    });
    move_to_row(&mut floor, SettingsRowKind::ServerPort);
    let idx = row_index(SettingsRowKind::ServerPort);
    floor.adjust(-1, Instant::now());
    assert_eq!(floor.rows()[idx].draft, RowDraft::ServerPort(1024));
    assert!(!floor.rows()[idx].touched, "a floor clamp is a true no-op");

    // Clamp at the ceiling: a session started right at 65535 must not step
    // above it.
    let mut ceiling = ThemeSettings::open(ThemeSettingsInit {
        server_port: 65535,
        ..settings_init()
    });
    move_to_row(&mut ceiling, SettingsRowKind::ServerPort);
    ceiling.adjust(1, Instant::now());
    assert_eq!(ceiling.rows()[idx].draft, RowDraft::ServerPort(65535));
    assert!(
        !ceiling.rows()[idx].touched,
        "a ceiling clamp is a true no-op"
    );
}

// Server settings panel rows, part 3/3 (server-scopes): cycles through the
// 8 documented presets both directions, and a non-preset config value
// (e.g. hand-edited "input,read") falls back to the first preset ("read")
// on the first press rather than panicking or getting stuck.
#[test]
fn server_scopes_row_cycles_presets_and_falls_back_from_a_non_preset_value() {
    let mut settings = ThemeSettings::open(settings_init());
    assert_eq!(SettingsRowKind::ServerScopes.label(), "Server Scopes");
    assert!(!SettingsRowKind::ServerScopes.is_live());

    move_to_row(&mut settings, SettingsRowKind::ServerScopes);
    let idx = row_index(SettingsRowKind::ServerScopes);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerScopes("read".to_string())
    );

    let forward = [
        "read,control",
        "read,input",
        "read,control,input",
        "read,attach",
        "read,control,attach",
        "read,input,attach",
        "read,control,input,attach",
        "read",
    ];
    for expected in forward {
        settings.adjust(1, Instant::now());
        assert_eq!(
            settings.rows()[idx].draft,
            RowDraft::ServerScopes(expected.to_string())
        );
    }
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::ServerScopes),
        RestartReason::None
    );

    let backward = [
        "read,control,input,attach",
        "read,input,attach",
        "read,control,attach",
        "read,attach",
        "read,control,input",
        "read,input",
        "read,control",
        "read",
    ];
    for expected in backward {
        settings.adjust(-1, Instant::now());
        assert_eq!(
            settings.rows()[idx].draft,
            RowDraft::ServerScopes(expected.to_string())
        );
    }

    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("server-scopes".to_string(), "read".to_string())),
        "{updates:?}"
    );

    // Non-preset fallback: a hand-edited config value that isn't one of the
    // 8 cycle presets doesn't panic or get stuck — `cycle`'s shared
    // not-found fallback (`state.rs`) treats it as sitting at preset index 0
    // ("read") and steps from there, landing on a real preset immediately
    // (index 1, "read,control", for a `+1` press) rather than requiring two
    // presses to reach a known state.
    let mut off_preset = ThemeSettings::open(ThemeSettingsInit {
        server_scopes: "input,read".to_string(),
        ..settings_init()
    });
    move_to_row(&mut off_preset, SettingsRowKind::ServerScopes);
    let idx = row_index(SettingsRowKind::ServerScopes);
    assert_eq!(
        off_preset.rows()[idx].draft,
        RowDraft::ServerScopes("input,read".to_string())
    );
    off_preset.adjust(1, Instant::now());
    assert_eq!(
        off_preset.rows()[idx].draft,
        RowDraft::ServerScopes("read,control".to_string())
    );
    assert!(off_preset.rows()[idx].touched);
}

// server-bind (v2 LAN opt-in): the 2-preset cycle mirrors `ServerScopes`'s
// shape exactly — placed immediately after `ServerPort` in row order, same
// off-preset fallback semantics, same `ON SAVE`/reload-exempt treatment,
// same `commit_updates` write-back.
#[test]
fn server_bind_row_cycles_loopback_and_all_interfaces_and_falls_back_from_a_non_preset_value() {
    let mut settings = ThemeSettings::open(settings_init());
    assert_eq!(SettingsRowKind::ServerBind.label(), "Server Bind");
    assert!(!SettingsRowKind::ServerBind.is_live());
    assert_eq!(
        row_index(SettingsRowKind::ServerBind),
        row_index(SettingsRowKind::ServerPort) + 1,
        "ServerBind must sit immediately after ServerPort in row order"
    );

    move_to_row(&mut settings, SettingsRowKind::ServerBind);
    let idx = row_index(SettingsRowKind::ServerBind);
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerBind("127.0.0.1".to_string())
    );

    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerBind("0.0.0.0".to_string())
    );
    assert!(settings.rows()[idx].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::ServerBind),
        RestartReason::None
    );

    settings.adjust(1, Instant::now());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerBind("127.0.0.1".to_string())
    );

    settings.adjust(-1, Instant::now());
    assert_eq!(
        settings.rows()[idx].draft,
        RowDraft::ServerBind("0.0.0.0".to_string())
    );

    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("server-bind".to_string(), "0.0.0.0".to_string())),
        "{updates:?}"
    );

    // Non-preset fallback: a hand-edited config value that isn't one of the
    // 2 cycle presets displays as-is and the first ←→ press lands on a real
    // preset (index 0 fallback, then +1) instead of panicking or sticking.
    let mut off_preset = ThemeSettings::open(ThemeSettingsInit {
        server_bind: "192.168.1.50".to_string(),
        ..settings_init()
    });
    move_to_row(&mut off_preset, SettingsRowKind::ServerBind);
    let idx = row_index(SettingsRowKind::ServerBind);
    assert_eq!(
        off_preset.rows()[idx].draft,
        RowDraft::ServerBind("192.168.1.50".to_string())
    );
    off_preset.adjust(1, Instant::now());
    assert_eq!(
        off_preset.rows()[idx].draft,
        RowDraft::ServerBind("0.0.0.0".to_string())
    );
    assert!(off_preset.rows()[idx].touched);
}

// R-7/AC-18/AC-19: the state-machine half of the default_for round trip —
// every one of the 16 row kinds, not just the 3 the AC-numbered tests
// above happen to exercise (CursorStyle/FontSize/MacosTitlebarStyle),
// resets to exactly `RowDraft::default_for(kind)` and marks touched.
// `ServerTokenCopy`/`ServerStatus` are the deliberate exceptions (see their
// doc comments on `SettingsRowKind`): an action row and a read-only display
// row, neither with a persisted value, so reset stays a no-op for both
// instead.
#[test]
fn reset_selected_row_writes_default_for_and_marks_touched_for_every_row_kind() {
    for kind in SettingsRowKind::ALL {
        let mut settings = ThemeSettings::open(settings_init());
        move_to_row(&mut settings, kind);

        settings.reset_selected_row(Instant::now());

        let idx = row_index(kind);
        if matches!(
            kind,
            SettingsRowKind::ServerTokenCopy
                | SettingsRowKind::ServerRemoteAppQr
                | SettingsRowKind::ServerStatus
        ) {
            assert_eq!(
                settings.rows()[idx].draft,
                RowDraft::default_for(kind),
                "{kind:?}"
            );
            assert!(!settings.rows()[idx].touched, "{kind:?}");
            continue;
        }
        assert_eq!(
            settings.rows()[idx].draft,
            RowDraft::default_for(kind),
            "{kind:?}"
        );
        assert!(settings.rows()[idx].touched, "{kind:?}");
    }
}

// R-7: Reset's `RowEffect` for the two opaque-startup-sensitive rows must
// respect the same `opaque_at_startup` gate `adjust` uses — untested by the
// existing AC-18 cases (CursorStyle/FontSize never gate on this at all).
#[test]
fn reset_background_opacity_and_blur_effect_respects_opaque_startup_gating() {
    let mut opaque = ThemeSettings::open(settings_init()); // opacity 1.0 = opaque
    move_to_row(&mut opaque, SettingsRowKind::BackgroundOpacity);
    opaque.adjust(-4, Instant::now());
    assert_eq!(
        opaque.reset_selected_row(Instant::now()),
        RowEffect::None,
        "opaque-at-startup must suppress the live effect even though the draft resets"
    );
    assert_eq!(
        opaque.rows()[row_index(SettingsRowKind::BackgroundOpacity)].draft,
        RowDraft::BackgroundOpacity(1.0)
    );

    move_to_row(&mut opaque, SettingsRowKind::BackgroundBlurRadius);
    opaque.adjust(5, Instant::now());
    assert_eq!(opaque.reset_selected_row(Instant::now()), RowEffect::None);

    // Transparent startup: the same reset does report the live effect,
    // proving the gate is conditional on `opaque_at_startup`, not an
    // unconditional suppression for these two rows.
    let mut transparent = ThemeSettings::open(transparent_init());
    move_to_row(&mut transparent, SettingsRowKind::BackgroundOpacity);
    transparent.adjust(-4, Instant::now());
    assert_eq!(
        transparent.reset_selected_row(Instant::now()),
        RowEffect::Opacity(1.0)
    );

    move_to_row(&mut transparent, SettingsRowKind::BackgroundBlurRadius);
    transparent.adjust(5, Instant::now());
    assert_eq!(
        transparent.reset_selected_row(Instant::now()),
        RowEffect::Blur(0)
    );
}

// R-7: the written config side of a reset — `commit_updates()` must carry
// the just-reset default value for a touched row, not merely flip the
// `touched` bit (which the AC-19 test already covers). CursorStyle (not
// FontFamily — see `commit_updates_skips_the_font_family_key_when_reset_to_the_empty_default`
// just below) has a non-empty default, so this is the row that actually
// proves a reset value round-trips into `commit_updates()`.
#[test]
fn commit_updates_includes_the_reset_default_value_for_a_touched_row() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::CursorStyle);
    settings.adjust(1, Instant::now()); // Block -> Bar, away from the default

    settings.reset_selected_row(Instant::now());

    let updates = settings.commit_updates();
    assert!(
        updates.contains(&("cursor-style".to_string(), "block".to_string())),
        "{updates:?}"
    );
}

// Fix F2: `RowDraft::default_for(FontFamily)` is the empty string
// (`StartupConfig::default().font.families` is empty — "no override"), and
// an empty `font-family = ` line isn't a valid "unset" signal to noa-config's
// parser. `commit_updates()` must skip the key entirely for an empty
// FontFamily draft, even though the row is touched (contrast with
// `reset_marks_touched_even_when_the_default_equals_the_current_value`,
// which proves `touched` itself is unaffected).
#[test]
fn commit_updates_skips_the_font_family_key_when_reset_to_the_empty_default() {
    let mut settings = ThemeSettings::open(settings_init()); // font_family = "Menlo"
    move_to_row(&mut settings, SettingsRowKind::FontFamily);

    settings.reset_selected_row(Instant::now());

    assert!(settings.rows()[row_index(SettingsRowKind::FontFamily)].touched);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily(String::new())
    );
    let updates = settings.commit_updates();
    assert!(
        !updates.iter().any(|(key, _)| key == "font-family"),
        "an empty FontFamily draft must never reach the config writer: {updates:?}"
    );
}

// Fix F2 companion: a *non-empty* FontFamily edit (the ordinary cycle-
// through-available-families path, not a reset) still writes normally —
// the skip is specific to the empty-string reset value, not FontFamily in
// general.
#[test]
fn commit_updates_still_writes_a_nonempty_font_family_edit() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.adjust(1, Instant::now()); // cycles to the next available family

    let updates = settings.commit_updates();
    assert!(
        updates
            .iter()
            .any(|(key, value)| key == "font-family" && !value.is_empty()),
        "{updates:?}"
    );
}

// R-1: `restart_reason`'s explicit exemption list (BackgroundImage and its
// 5 sibling rows, plus ConfirmQuit/QuickTerminalHeight) never reports
// `CommitOnly` even once touched — untested by name until now; the
// existing `touched_commit_only_rows_show_restart_note` test only checks
// these rows while they stay untouched (touching a *different* row,
// FontFamily, in that test).
#[test]
fn restart_reason_never_reports_commit_only_for_the_reload_exempt_rows_even_when_touched() {
    let mut settings = ThemeSettings::open(settings_init());

    move_to_row(&mut settings, SettingsRowKind::BackgroundImage);
    settings.push_text("/tmp/wall.png", Instant::now());
    assert!(settings.rows()[row_index(SettingsRowKind::BackgroundImage)].touched);
    assert_eq!(
        settings.restart_reason(SettingsRowKind::BackgroundImage),
        RestartReason::None
    );

    // BackgroundImageOpacity starts at 1.0 (its clamp ceiling) in
    // `settings_init`, so `adjust(1, ..)` alone would clamp back to the
    // same value and never flip `touched` — that row uses -1 instead, the
    // rest use +1 (cycle/toggle rows change either direction).
    let adjust_exempt = [
        (SettingsRowKind::BackgroundImageOpacity, -1),
        (SettingsRowKind::BackgroundImagePosition, 1),
        (SettingsRowKind::BackgroundImageFit, 1),
        (SettingsRowKind::BackgroundImageRepeat, 1),
        (SettingsRowKind::BackgroundImageInterval, 1),
        (SettingsRowKind::ConfirmQuit, 1),
        (SettingsRowKind::SendSelectionSendEnter, 1),
        (SettingsRowKind::QuickTerminalHeight, 1),
    ];
    for (kind, delta) in adjust_exempt {
        move_to_row(&mut settings, kind);
        settings.adjust(delta, Instant::now());
        assert!(settings.rows()[row_index(kind)].touched, "{kind:?}");
        assert_eq!(
            settings.restart_reason(kind),
            RestartReason::None,
            "{kind:?} is exempt from the commit-only restart note"
        );
    }
}

// R-1/R-3: cross-checks the two independent signals against every row
// under both opaque and transparent startup (32 combinations) — the
// AC-numbered tests only exercise a handful of named rows; this closes the
// remaining ones (the 8 background-image/misc rows, WindowPadding,
// SidebarPreviewLines under opaque) with the documented invariant: a row
// reads `Live` iff it's statically live AND not downgraded by an opaque
// startup. Among the rest, `OnLaunch` covers two cases — an opaque-
// downgraded live row (C-6: still needs a restart to preview, same as a
// genuine restart-only row) and the three genuinely persist-only rows
// (`FontFamily`/`WindowPadding`/`MacosTitlebarStyle`) — every other
// non-live row reads `OnSave` (fix F1: these have no live-preview path,
// but `commit_theme_settings` re-applies them the moment they're saved, so
// `OnLaunch`'s "needs a restart" story never applied to them — this
// replaces the original `... only Live/OnLaunch are constructed pre-R-9`
// assumption, which encoded the pre-fix bug where every non-live row
// collapsed to `OnLaunch`).
#[test]
fn liveness_and_restart_reason_agree_on_every_row_under_opaque_and_transparent_startup() {
    for init in [settings_init(), transparent_init()] {
        let settings = ThemeSettings::open(init);
        for kind in SettingsRowKind::ALL {
            let liveness = settings.liveness(kind);
            let reason = settings.restart_reason(kind);
            let expect_live = kind.is_live() && reason != RestartReason::OpaqueStartup;
            assert_eq!(
                liveness == Liveness::Live,
                expect_live,
                "{kind:?}: liveness={liveness:?} reason={reason:?}"
            );
            if !expect_live {
                let expect_on_launch = reason == RestartReason::OpaqueStartup
                    || matches!(
                        kind,
                        SettingsRowKind::FontFamily
                            | SettingsRowKind::WindowPadding
                            | SettingsRowKind::MacosTitlebarStyle
                            | SettingsRowKind::MacosOptionAsAlt
                    );
                let expected = if expect_on_launch {
                    Liveness::OnLaunch
                } else {
                    Liveness::OnSave
                };
                assert_eq!(liveness, expected, "{kind:?}: reason={reason:?}");
            }
        }
    }
}

// Fix F1: the 8 reload-exempt rows badge `OnSave`, not `OnLaunch` — they
// have no live-preview-while-editing path, but `commit_theme_settings`
// fully applies them the instant the overlay saves (see
// `restart_reason_never_reports_commit_only_for_the_reload_exempt_rows_even_when_touched`
// for the matching `RestartReason::None` guarantee on the same row set).
#[test]
fn liveness_reports_on_save_for_every_reload_exempt_row() {
    let settings = ThemeSettings::open(settings_init());
    for kind in [
        SettingsRowKind::BackgroundImage,
        SettingsRowKind::BackgroundImageOpacity,
        SettingsRowKind::BackgroundImagePosition,
        SettingsRowKind::BackgroundImageFit,
        SettingsRowKind::BackgroundImageRepeat,
        SettingsRowKind::BackgroundImageInterval,
        SettingsRowKind::ConfirmQuit,
        SettingsRowKind::SendSelectionSendEnter,
        SettingsRowKind::QuickTerminalHeight,
        // R-9 (Addendum D-1): the three reload-applied keys join the same
        // OnSave class as the pre-existing 8.
        SettingsRowKind::ScrollbackLimit,
        SettingsRowKind::CursorStyleBlink,
        SettingsRowKind::MinimumContrast,
        SettingsRowKind::ServerEnable,
        SettingsRowKind::ServerPort,
        SettingsRowKind::ServerScopes,
    ] {
        assert_eq!(settings.liveness(kind), Liveness::OnSave, "{kind:?}");
    }
    // The four genuine restart-only rows remain `OnLaunch` — R-9's
    // `macos-option-as-alt` joins the pre-existing 3 (it's read only at pty
    // spawn, unlike its 3 reload-applied siblings above).
    for kind in [
        SettingsRowKind::FontFamily,
        SettingsRowKind::WindowPadding,
        SettingsRowKind::MacosTitlebarStyle,
        SettingsRowKind::MacosOptionAsAlt,
    ] {
        assert_eq!(settings.liveness(kind), Liveness::OnLaunch, "{kind:?}");
    }
}

// R-5/Addendum D-3/FM-02: `adjust` must no-op while search owns the
// keyboard, mirroring the guard `reset_selected_row` already has a test
// for (`reset_is_a_no_op_while_search_is_active`) — untested for `adjust`
// itself until now.
#[test]
fn adjust_is_a_no_op_while_search_is_active() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::CursorStyle);
    settings.toggle_settings_search();
    let before = settings.rows()[row_index(SettingsRowKind::CursorStyle)]
        .draft
        .clone();

    let effect = settings.adjust(1, Instant::now());

    assert_eq!(effect, RowEffect::None);
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::CursorStyle)].draft,
        before,
        "adjust must no-op while search owns the keyboard"
    );
}

// Addendum B: confirming a zero-match search must still exit cleanly
// without panicking or mutating the selection — AC-14 only proves ↑↓ is a
// no-op on zero matches, not that Enter/confirm is too.
#[test]
fn search_enter_with_zero_matches_exits_search_without_changing_selection() {
    let mut settings = ThemeSettings::open(settings_init());
    move_to_row(&mut settings, SettingsRowKind::FontFamily);
    settings.toggle_settings_search();
    settings.push_text("zzzzzz", Instant::now());
    assert_eq!(settings.settings_filtered_len(), 0);

    settings.confirm_settings_search();

    assert!(!settings.settings_search_active());
    assert_eq!(
        SettingsRowKind::ALL[settings.selected_row()],
        SettingsRowKind::FontFamily,
        "a zero-match confirm must leave the pre-search selection untouched"
    );
}

// R-5: backspacing a search query all the way back to empty must restore
// the full unfiltered row list, in `ALL` order — AC-15 only covers a query
// that starts empty, not one that returns to empty via edits.
#[test]
fn search_backspace_to_empty_query_restores_every_row() {
    // "cursor style" (not the shorter "cursor"): R-9 added a "Cursor Blink"
    // row, which the shorter query also subsequence-matches (see
    // `search_filters_rows_by_label_fuzzy_match`'s comment for the same
    // reasoning against the pre-R-9 "Background Blur Radius" ambiguity).
    let mut settings = ThemeSettings::open(settings_init());
    settings.toggle_settings_search();
    settings.push_text("cursor style", Instant::now());
    assert_eq!(settings.settings_filtered_len(), 1);

    for _ in 0.."cursor style".len() {
        settings.backspace(Instant::now());
    }

    assert_eq!(settings.settings_filter(), "");
    assert_eq!(settings.settings_filtered_len(), SettingsRowKind::COUNT);
    for i in 0..SettingsRowKind::COUNT {
        assert_eq!(settings.settings_filtered_row_index(i), Some(i));
    }
}

// R-5 L2's open question, resolved for the other text-entry row type: the
// existing `search_enter_and_exit_clear_in_progress_font_size_digit_entry`
// only proves this for FontSize's digit buffer. BackgroundImage has its
// own `background_image_text` buffer with the same "cleared means the next
// keystroke replaces rather than resumes" contract (see
// `push_background_image_text`'s empty-seed behavior, distinct from
// `backspace`'s draft-seeded one).
#[test]
fn search_enter_and_exit_clear_in_progress_background_image_text_entry() {
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        background_image: "/tmp/old.png".to_string(),
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::BackgroundImage);
    settings.backspace(Instant::now()); // seeds the buffer from the draft: "/tmp/old.pn"
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("/tmp/old.pn".to_string())
    );

    settings.toggle_settings_search();
    settings.toggle_settings_search(); // back out, still on BackgroundImage

    settings.push_text("X", Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("X".to_string()),
        "a cleared buffer means the next keystroke replaces the path, not resumes editing it"
    );
}

// Addendum D-3/FM-06's compound test, for BackgroundImage: reset must clear
// the in-progress text buffer exactly like it does for FontSize's digit
// buffer (`reset_clears_in_progress_font_size_digit_entry`), so a stale
// buffer can't resurrect the pre-reset path on the next keystroke.
#[test]
fn reset_clears_in_progress_background_image_text_entry() {
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        background_image: "/tmp/old.png".to_string(),
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::BackgroundImage);
    settings.backspace(Instant::now()); // "/tmp/old.pn", buffer seeded from the draft

    settings.reset_selected_row(Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage(String::new())
    );

    settings.push_text("Y", Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::BackgroundImage)].draft,
        RowDraft::BackgroundImage("Y".to_string()),
        "derives from the post-reset draft, not a resumed stale buffer"
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
            // `adjust` alone never changes this row's draft (see its doc
            // comment on `SettingsRowKind::ServerTokenCopy`) — the
            // fingerprint-relevant mutator is `App`'s reported outcome.
            SettingsRowKind::ServerTokenCopy => {
                settings.set_server_token_copy_status(TokenCopyStatus::Copied);
            }
            // This action mutates no pure state: App presents the QR outside
            // the state machine, so its RowEffect is tested separately.
            SettingsRowKind::ServerRemoteAppQr => {}
            // Same shape as `ServerTokenCopy` above, but for the read-only
            // status row's own out-of-band refresh (see its doc comment on
            // `SettingsRowKind::ServerStatus`).
            SettingsRowKind::ServerStatus => {
                settings.set_server_status("Running (127.0.0.1:61771, 1 client(s))".to_string());
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
        if *kind == SettingsRowKind::ServerRemoteAppQr {
            continue;
        }
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

    // R-29/R-30 (AC-60 extension): the favorites/attribute mutators.
    let mut settings = ThemeSettings::open(init());
    let fp0 = settings.view_fingerprint_u64();
    settings.toggle_favorites_only();
    let fp1 = settings.view_fingerprint_u64();
    assert_ne!(fp0, fp1, "toggle_favorites_only");

    settings.cycle_attribute_filter();
    let fp2 = settings.view_fingerprint_u64();
    assert_ne!(fp1, fp2, "cycle_attribute_filter");

    let mut favorites = std::collections::HashSet::new();
    favorites.insert("3024 Day".to_string());
    settings.set_favorites(std::sync::Arc::new(favorites), 1);
    let fp3 = settings.view_fingerprint_u64();
    assert_ne!(fp2, fp3, "set_favorites");

    // R-5 (AC-60 extension): the search sub-state mutators — the native
    // ViewModel renders the query line, filtered subset, and search
    // highlight, so every one of these must move the fingerprint or the
    // native panel freezes mid-search.
    let mut settings = ThemeSettings::open(settings_init());
    let fp0 = settings.view_fingerprint_u64();
    settings.toggle_settings_search();
    let fp1 = settings.view_fingerprint_u64();
    assert_ne!(fp0, fp1, "toggle_settings_search (enter)");

    settings.move_down();
    let fp2 = settings.view_fingerprint_u64();
    assert_ne!(fp1, fp2, "move_down (search highlight)");

    settings.push_text("font", Instant::now());
    let fp3 = settings.view_fingerprint_u64();
    assert_ne!(fp2, fp3, "push_text (search query)");

    settings.confirm_settings_search();
    let fp4 = settings.view_fingerprint_u64();
    assert_ne!(fp3, fp4, "confirm_settings_search");
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

// AC-46 (R-32): wheel deltas accumulate and step by exactly one row per
// threshold crossing — sub-threshold deltas move nothing, the carry
// survives across calls, and a single oversized delta still steps only
// once.
#[test]
fn apply_wheel_accumulates_and_steps_one_row_per_threshold_crossing() {
    let mut settings = ThemeSettings::open(init());
    let start = settings.highlighted_index();

    assert!(
        !settings.apply_wheel(-10.0),
        "sub-threshold delta moves nothing"
    );
    assert_eq!(settings.highlighted_index(), start);

    assert!(
        settings.apply_wheel(-35.0),
        "the accumulated -45 crosses -40"
    );
    assert_eq!(settings.highlighted_index(), start + 1);

    // A single oversized delta still steps only once (never cascades).
    let before = settings.highlighted_index();
    assert!(settings.apply_wheel(-400.0));
    assert_eq!(settings.highlighted_index(), before + 1);
}

// AC-45/46 (R-32): a positive (scroll-up) delta moves the highlight up.
#[test]
fn apply_wheel_positive_delta_moves_up() {
    let mut settings = ThemeSettings::open(init());
    settings.move_down();
    settings.move_down();
    let start = settings.highlighted_index();

    assert!(settings.apply_wheel(45.0));
    assert_eq!(settings.highlighted_index(), start - 1);
}

// AC-56 (FM-01 defense-in-depth, sharpened by the AC-57 integration test):
// a Tab carryover never carries `highlight_moved` forward into *any* mode,
// even from a Theme session that had genuinely moved its highlight. This is
// a deliberately conservative choice, not an oversight: `highlight_moved`'s
// only real job is gating whether the *next* navigation-triggered sync
// resolves a preview (`App::sync_theme_settings_preview`), and
// `App::tab_theme_settings`/`open_theme_settings_session` never touch
// `gpu.preview_theme` at all — so AC-36's actual guarantee (runtime values
// unchanged across Tab) already holds with no help from this flag. Carrying
// it *would* additionally have to stay false through a Settings-mode leg
// (AC-56) with no way to resurrect "was it ever moved" on the far side of
// that leg — multi-hop carryover has no consistent semantics to give it, so
// the safe, simple, always-false-on-open default applies uniformly.
#[test]
fn tab_carryover_never_carries_highlight_moved_into_either_mode() {
    let mut theme = ThemeSettings::open(init());
    theme.move_down();
    assert!(theme.should_preview());

    let carry = theme.carryover();
    let settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        carryover: Some(carry),
        ..init()
    });
    assert!(!settings.should_preview());

    let carry_back = settings.carryover();
    let theme_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        carryover: Some(carry_back),
        ..init()
    });
    assert!(!theme_again.should_preview());
}

fn sample_revert(theme_name: &str) -> RevertValues {
    RevertValues {
        theme_name: theme_name.to_string(),
        font_size: 16.0,
        cursor_style: CursorShape::Bar,
        background_opacity: 0.8,
        background_blur_radius: 5,
        background_image: "/tmp/wall.png".to_string(),
        background_image_opacity: 0.5,
        background_image_position: BackgroundImagePosition::Center,
        background_image_fit: BackgroundImageFit::Cover,
        background_image_repeat: true,
        background_image_interval_secs: 30,
        sidebar_preview_lines: 3,
        sidebar_width: 360.0,
        sidebar_font_size: 11.5,
        quick_terminal_size: 0.4,
        window_padding_x: 2.0,
        window_padding_y: 2.0,
        macos_titlebar_style: MacosTitlebarStyle::Native,
        confirm_quit: true,
        send_selection_send_enter: false,
        font_family: "Menlo".to_string(),
    }
}

// AC-44 (R-31): `revert_updates` writes every snapshot field unconditionally
// (an absolute restore, not a touched-gated diff).
#[test]
fn revert_updates_writes_every_snapshot_field_unconditionally() {
    let updates = revert_updates(&sample_revert("3024 Day"), None);
    assert_eq!(
        updates.iter().find(|(k, _)| k == "theme"),
        Some(&("theme".to_string(), "3024 Day".to_string()))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "font-size"),
        Some(&("font-size".to_string(), "16".to_string()))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "cursor-style"),
        Some(&("cursor-style".to_string(), "bar".to_string()))
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "background-blur-radius"),
        Some(&("background-blur-radius".to_string(), "5".to_string()))
    );
}

// TSV2-1 (judge, CONFIRMED): the 5 commit-only rows were missing from
// `RevertValues`/`revert_updates` entirely, so a commit of any of
// font-family / window-padding-x / window-padding-y / macos-titlebar-style
// / confirm-quit followed by an Undo silently left the disk value at the
// committed (unwanted) value instead of restoring the pre-open snapshot —
// asymmetric with `commit_updates`, which does write all of them.
#[test]
fn revert_updates_restores_all_five_commit_only_rows() {
    let updates = revert_updates(&sample_revert("3024 Day"), None);
    assert_eq!(
        updates.iter().find(|(k, _)| k == "font-family"),
        Some(&("font-family".to_string(), "Menlo".to_string())),
        "font-family must revert"
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "window-padding-x"),
        Some(&("window-padding-x".to_string(), "2".to_string())),
        "window-padding-x must revert"
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "window-padding-y"),
        Some(&("window-padding-y".to_string(), "2".to_string())),
        "window-padding-y must revert"
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "macos-titlebar-style"),
        Some(&("macos-titlebar-style".to_string(), "native".to_string())),
        "macos-titlebar-style must revert"
    );
    assert_eq!(
        updates.iter().find(|(k, _)| k == "confirm-quit"),
        Some(&("confirm-quit".to_string(), "true".to_string())),
        "confirm-quit must revert"
    );
    assert_eq!(
        updates
            .iter()
            .find(|(k, _)| k == "send-selection-send-enter"),
        Some(&("send-selection-send-enter".to_string(), "false".to_string())),
        "send-selection-send-enter must revert"
    );
}

// TSV2-1: `macos-titlebar-style`'s serializer must agree with
// `commit_updates`'s for *both* variants, not just whichever one
// `sample_revert` happens to use — a literal `"native"`/`"transparent"`
// string mismatch between the two paths would silently corrupt the file on
// undo even though this same test file's `commit_updates` tests pass.
#[test]
fn revert_updates_macos_titlebar_style_serialization_matches_commit_updates() {
    let mut revert = sample_revert("3024 Day");
    revert.macos_titlebar_style = MacosTitlebarStyle::Transparent;
    let updates = revert_updates(&revert, None);
    assert_eq!(
        updates.iter().find(|(k, _)| k == "macos-titlebar-style"),
        Some(&(
            "macos-titlebar-style".to_string(),
            "transparent".to_string()
        ))
    );
}

// AC-44/R-34: an Undo of a pair-config commit must restore pair syntax, not
// clobber it with a bare name — the same guarantee `commit_updates` gives
// on the forward path.
#[test]
fn revert_updates_preserves_pair_syntax_when_undoing_a_pair_commit() {
    let pair = ThemePairContext {
        active_is_light: true,
        light: "A".to_string(),
        dark: "B".to_string(),
    };
    let updates = revert_updates(&sample_revert("A"), Some(&pair));
    assert_eq!(
        updates.iter().find(|(k, _)| k == "theme"),
        Some(&("theme".to_string(), "light:A,dark:B".to_string()))
    );
}

// FM-01-adjacent: an unresolvable original theme (empty name) never writes
// an empty `theme` value.
#[test]
fn revert_updates_omits_theme_key_when_snapshot_theme_name_is_empty() {
    let updates = revert_updates(&sample_revert(""), None);
    assert!(updates.iter().all(|(k, _)| k != "theme"));
}

// AC-39 (R-28): `filter_font_families` produces exactly the same
// scoring/highlight positions `command_palette::fuzzy_match` would for each
// family name — proof no second matcher exists.
#[test]
fn filter_font_families_matches_command_palette_fuzzy_match_scoring() {
    let settings = ThemeSettings::open(ThemeSettingsInit {
        available_font_families: vec![
            "Menlo".to_string(),
            "Monaco".to_string(),
            "Courier New".to_string(),
            "SF Mono".to_string(),
        ],
        ..init()
    });

    let results = settings.filter_font_families("mo");
    let mut expected: Vec<(i32, String, Vec<usize>)> = [
        "Menlo".to_string(),
        "Monaco".to_string(),
        "Courier New".to_string(),
        "SF Mono".to_string(),
    ]
    .into_iter()
    .filter_map(|name| {
        crate::command_palette::fuzzy_match("mo", &name)
            .map(|(score, positions)| (score, name, positions))
    })
    .collect();
    expected.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));

    assert_eq!(results.len(), expected.len());
    for (result, (_, name, positions)) in results.iter().zip(expected.iter()) {
        assert_eq!(&result.name, name);
        assert_eq!(&result.positions, positions);
    }
}

// R-28: typing into the focused `FontFamily` row live-selects the best
// fuzzy match, and Backspace re-resolves from the shortened query.
#[test]
fn font_family_row_typing_live_selects_the_best_fuzzy_match() {
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        available_font_families: vec![
            "Menlo".to_string(),
            "Monaco".to_string(),
            "SF Mono".to_string(),
        ],
        ..settings_init()
    });
    move_to_row(&mut settings, SettingsRowKind::FontFamily);

    settings.push_text("mono", Instant::now());
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily("Monaco".to_string())
    );
    assert!(settings.rows()[row_index(SettingsRowKind::FontFamily)].touched);

    settings.push_text("c", Instant::now()); // "monoc" no longer matches "Monaco"
    // no match for "monoc" in this fixture set — draft holds its last value
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily("Monaco".to_string())
    );

    settings.backspace(Instant::now()); // back to "mono"
    assert_eq!(
        settings.rows()[row_index(SettingsRowKind::FontFamily)].draft,
        RowDraft::FontFamily("Monaco".to_string())
    );
}

// AC-40 (R-29): favorites-only shows the intersection of the favorites set
// and the fuzzy query, and `commit_updates()` never carries a
// favorites-related key.
#[test]
fn favorites_only_filter_shows_only_the_favorited_theme_and_never_leaks_into_commit_updates() {
    let mut favorites = std::collections::HashSet::new();
    let theme_a = noa_theme::THEMES[10].0.to_string();
    favorites.insert(theme_a.clone());
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        favorites: std::sync::Arc::new(favorites),
        favorites_epoch: 1,
        ..init()
    });

    settings.toggle_favorites_only();
    assert!(settings.favorites_only());
    assert_eq!(settings.filtered_len(), 1);
    assert_eq!(
        settings.filtered_entry(0).map(|(name, _)| name),
        Some(theme_a.as_str())
    );

    let updates = settings.commit_updates();
    assert!(
        updates.iter().all(|(k, _)| !k.contains("favorite")),
        "commit_updates must never carry a favorites-related key: {updates:?}"
    );
}

// AC-42 (R-30): the attribute filter excludes the opposite polarity and
// restores the full catalog when cycled back to "All".
#[test]
fn attribute_filter_excludes_the_opposite_polarity() {
    let light_theme = noa_theme::resolve("3024 Day").expect("bundled theme exists");
    let dark_theme = noa_theme::resolve("3024 Night").expect("bundled theme exists");
    assert_eq!(attribute_of(light_theme), Attribute::Light);
    assert_eq!(attribute_of(dark_theme), Attribute::Dark);

    let mut settings = ThemeSettings::open(init());
    let total = settings.filtered_len();

    settings.cycle_attribute_filter(); // All -> Dark
    assert_eq!(settings.attribute_filter(), Some(Attribute::Dark));
    let dark_names: Vec<&str> = (0..settings.filtered_len())
        .filter_map(|i| settings.filtered_entry(i).map(|(name, _)| name))
        .collect();
    assert!(!dark_names.contains(&"3024 Day"));
    assert!(dark_names.contains(&"3024 Night"));

    settings.cycle_attribute_filter(); // Dark -> Light
    assert_eq!(settings.attribute_filter(), Some(Attribute::Light));
    let light_names: Vec<&str> = (0..settings.filtered_len())
        .filter_map(|i| settings.filtered_entry(i).map(|(name, _)| name))
        .collect();
    assert!(!light_names.contains(&"3024 Night"));
    assert!(light_names.contains(&"3024 Day"));

    settings.cycle_attribute_filter(); // Light -> All
    assert_eq!(settings.attribute_filter(), None);
    assert_eq!(settings.filtered_len(), total);
}

// AC-52a (Addendum A-2): a highlighted theme that survives a condition
// change keeps tracking it, and the preview does not reset.
#[test]
fn ac52_condition_change_tracks_a_surviving_highlight_without_resetting_preview() {
    let mut favorites = std::collections::HashSet::new();
    favorites.insert("3024 Day".to_string());
    let mut settings = ThemeSettings::open(ThemeSettingsInit {
        favorites: std::sync::Arc::new(favorites),
        favorites_epoch: 1,
        ..init()
    });
    let target_pos = (0..settings.filtered_len())
        .find(|&i| settings.filtered_entry(i).map(|(name, _)| name) == Some("3024 Day"))
        .expect("3024 Day is in the catalog");
    move_highlight_to(&mut settings, target_pos);
    assert!(settings.should_preview());

    settings.toggle_favorites_only(); // "3024 Day" survives (it's favorited)
    assert_eq!(settings.highlighted_theme_name(), Some("3024 Day"));
    assert!(
        settings.should_preview(),
        "a still-present highlight must not reset highlight_moved"
    );
}

// AC-52b (Addendum A-2): a highlighted theme excluded by a condition change
// jumps to index 0 without firing a new preview.
#[test]
fn ac52_condition_change_resets_highlight_moved_when_the_highlight_is_excluded() {
    let mut settings = ThemeSettings::open(init());
    let target_pos = (0..settings.filtered_len())
        .find(|&i| settings.filtered_entry(i).map(|(name, _)| name) == Some("3024 Day"))
        .expect("3024 Day is in the catalog");
    move_highlight_to(&mut settings, target_pos);
    assert!(settings.should_preview());

    settings.cycle_attribute_filter(); // All -> Dark: excludes the Light "3024 Day"
    assert_eq!(settings.highlighted_index(), 0);
    assert!(
        !settings.should_preview(),
        "excluding the highlighted theme must reset highlight_moved (AC-52b)"
    );
}

// AC-52c (Addendum A-2): a condition change that empties the list keeps
// AC-16's existing "list empty, last preview stands" semantics.
#[test]
fn ac52_condition_change_to_zero_matches_keeps_ac16_empty_list_semantics() {
    let mut settings = ThemeSettings::open(init()); // no favorites configured
    settings.move_down();
    assert!(settings.should_preview());

    settings.toggle_favorites_only(); // empty favorites set -> filtered becomes empty
    assert_eq!(settings.filtered_len(), 0);
    assert_eq!(settings.highlighted_theme_name(), None);
}

// AC-57 (FM-03, Stage E): pair config × Tab carryover × a favorites toggle
// × commit, all in one editing task — the full v2 story exercised end to
// end at the pure-state level (no `App` needed; every piece here is either
// `ThemeSettings` itself or an injectable writer, per R-12/AC-8's existing
// seam). Verifies:
//   - a favorites-only view filter narrows correctly and never reaches
//     `commit_updates()`'s output (AC-40's guarantee, replayed alongside
//     everything else rather than in isolation);
//   - Tab carries the new highlight across a Settings-mode round trip
//     (R-25) while the pair's original snapshot (light:A,dark:B) — not
//     anything touched mid-session — stays the Esc/commit-diff baseline
//     (FM-04);
//   - the final commit rewrites only the active (light) side of the pair,
//     preserving the untouched dark side verbatim (R-34/ADR-4), and still
//     carries no favorites-related key.
#[test]
fn ac57_pair_carryover_favorites_toggle_and_commit_integration() {
    let light_a = noa_theme::THEMES[0].0.to_string();
    let dark_b = noa_theme::THEMES[1].0.to_string();
    let favorite_c = noa_theme::THEMES[2].0.to_string();
    let new_light_d = noa_theme::THEMES[3].0.to_string();

    let mut favorites = std::collections::HashSet::new();
    favorites.insert(favorite_c.clone());

    let mut theme = ThemeSettings::open(ThemeSettingsInit {
        favorites: std::sync::Arc::new(favorites),
        favorites_epoch: 1,
        ..theme_pair_init(true, &light_a, &dark_b)
    });

    // Favorites-only narrows to exactly the favorited theme, and never
    // leaks into commit_updates (AC-40, replayed here).
    theme.toggle_favorites_only();
    assert_eq!(theme.filtered_len(), 1);
    assert_eq!(
        theme.filtered_entry(0).map(|(name, _)| name),
        Some(favorite_c.as_str())
    );
    assert!(
        theme
            .commit_updates()
            .iter()
            .all(|(k, _)| !k.contains("favorite"))
    );
    theme.toggle_favorites_only(); // back to the full catalog

    // Move the highlight to the theme this task will actually commit.
    let target_pos = (0..theme.filtered_len())
        .find(|&i| theme.filtered_entry(i).map(|(name, _)| name) == Some(new_light_d.as_str()))
        .expect("the fixture theme is in the catalog");
    move_highlight_to(&mut theme, target_pos);
    assert!(theme.should_preview());

    // Tab to Settings and back — R-25's carryover must survive the round
    // trip (AC-34/35) without disturbing the pair's original snapshot
    // (FM-04): a later Esc from this chain would still revert to A/B, not
    // to anything touched along the way.
    let carry_to_settings = theme.carryover();
    let settings = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Settings,
        carryover: Some(carry_to_settings),
        ..theme_pair_init(true, &light_a, &dark_b)
    });
    assert!(settings.commit_updates().is_empty());
    // AC-56's invariant must hold even carried through Tab from a
    // moved-highlight Theme session — this is the second bug this
    // integration test caught (the first was `commit_updates`'s missing
    // section gate above).
    assert!(!settings.should_preview());
    let carry_back = settings.carryover();
    let mut theme_again = ThemeSettings::open(ThemeSettingsInit {
        mode: ThemeSettingsMode::Theme,
        carryover: Some(carry_back),
        ..theme_pair_init(true, &light_a, &dark_b)
    });
    assert_eq!(
        theme_again.highlighted_theme_name(),
        Some(new_light_d.as_str())
    );
    // `should_preview()` itself resets to false on every fresh open,
    // carryover included (see `tab_carryover_never_carries_highlight_moved_into_either_mode`)
    // — this doesn't weaken AC-36: `gpu.preview_theme` is an `App`-level
    // value Tab never touches at all, so it stays whatever it already was
    // regardless of this flag. What matters here is that the *highlighted
    // position* survived the round trip (asserted above) — `commit_updates`
    // reads that directly, not `should_preview()`.
    assert!(!theme_again.should_preview());

    // Commit: only the active (light) side changes; the dark side and the
    // pair syntax itself survive untouched, and no favorites key leaks in.
    let dir = std::env::temp_dir().join(format!(
        "noa-theme-settings-ac57-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config");
    std::fs::write(
        &config_path,
        format!("theme = light:{light_a},dark:{dark_b}\n"),
    )
    .unwrap();

    let mut writer =
        |path: &Path, updates: &[(String, String)]| noa_config::write_config_updates(path, updates);
    let updates = theme_again
        .commit(&config_path, &mut writer)
        .expect("commit should succeed");
    assert!(updates.iter().all(|(k, _)| !k.contains("favorite")));
    assert_eq!(
        updates.iter().find(|(k, _)| k == "theme"),
        Some(&(
            "theme".to_string(),
            format!("light:{new_light_d},dark:{dark_b}")
        ))
    );

    let (overrides, diagnostics) = noa_config::load_overrides_from_path(&config_path).unwrap();
    assert!(
        diagnostics.is_empty(),
        "committed config should still parse as a valid pair: {diagnostics:?}"
    );
    assert_eq!(
        overrides.theme_appearance,
        Some(noa_config::ThemeAppearancePair {
            light: new_light_d,
            dark: dark_b,
        })
    );

    std::fs::remove_dir_all(dir).unwrap();
}
