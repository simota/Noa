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
        quick_terminal_size: 0.4,
        window_padding_x: 2.0,
        window_padding_y: 2.0,
        macos_titlebar_style: MacosTitlebarStyle::Native,
        confirm_quit: true,
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
