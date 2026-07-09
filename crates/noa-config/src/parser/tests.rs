use std::path::{Path, PathBuf};

use noa_core::Rgb;

use crate::{
    AlphaBlendingMode, BackgroundImageFit, BackgroundImagePosition, ClipboardAccess,
    ConfigOverrides, CursorShape, FontConfig, FontFeature, FontVariation, KeybindConfig,
    MacosOptionAsAlt, MacosTitlebarStyle, SyntheticStyleMode, WindowSaveState,
};

use super::*;

fn path() -> &'static Path {
    Path::new("/tmp/noa-test-config")
}

fn diagnostic_messages(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

#[test]
fn parse_directives_handles_ghostty_line_rules() {
    let directives = parse_directives(
        "\u{feff}window-width   =   120\r\n  # a comment\nfont-size = 14 # not a comment\nnot-a-directive\nwindow-height = \"30\"\n",
    );

    assert_eq!(
        directives,
        vec![
            Directive {
                line: 1,
                key: "window-width".to_string(),
                value: Some("120".to_string()),
            },
            Directive {
                line: 3,
                key: "font-size".to_string(),
                value: Some("14 # not a comment".to_string()),
            },
            Directive {
                line: 5,
                key: "window-height".to_string(),
                value: Some("30".to_string()),
            },
        ]
    );
}

#[test]
fn parse_directives_keeps_malformed_quotes_literal() {
    let directives =
        parse_directives("window-width = \"120\nfont-size = \"ab\"cd\"\nwindow-height = \"\"");

    assert_eq!(directives[0].value.as_deref(), Some("\"120"));
    assert_eq!(directives[1].value.as_deref(), Some("\"ab\"cd\""));
    assert_eq!(directives[2].value.as_deref(), Some(""));
}

#[test]
fn scalar_values_are_last_wins_and_empty_resets() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "font-size = 14\nfont-size = 16\nwindow-width = 120\nwindow-height = 30\nwindow-height =",
    );

    assert_eq!(overrides.font_size, Some(16.0));
    assert_eq!(overrides.cols, None);
    assert_eq!(overrides.rows, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("window-width"));
    assert!(diagnostics[0].message.contains("window-height"));
}

#[test]
fn quoted_empty_numeric_value_is_invalid_not_reset() {
    let (overrides, diagnostics) = parse_overrides(path(), "window-width = \"\"");

    assert_eq!(overrides.cols, None);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("window-width") && diagnostic.message.contains("invalid")
    }));
}

#[test]
fn unknown_list_and_config_file_diagnostics_are_distinct() {
    let (_, unknown) = parse_overrides(path(), "bogus-key = x");
    let (_, list) = parse_overrides(path(), "palette = 1=#ffffff");
    let (_, config_file) = parse_overrides(path(), "config-file = ~/.config/ghostty/extra");

    let messages = [
        unknown[0].message.as_str(),
        list[0].message.as_str(),
        config_file[0].message.as_str(),
    ];
    assert_ne!(messages[0], messages[1]);
    assert_ne!(messages[0], messages[2]);
    assert_ne!(messages[1], messages[2]);
}

#[test]
fn unsupported_list_keys_are_recognized_but_not_retained() {
    let (overrides, diagnostics) = parse_overrides(path(), "palette = value");

    assert_eq!(overrides, ConfigOverrides::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("palette"));
    assert!(diagnostics[0].message.contains("list key"));
}

#[test]
fn keybind_entries_are_retained_in_order() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "keybind = cmd+i=prompt_surface_title\n\
         keybind = cmd+t=unbind\n\
         keybind = clear",
    );

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        overrides.keybinds,
        vec![
            KeybindConfig::Bind {
                trigger: "cmd+i".to_string(),
                action: "prompt_surface_title".to_string(),
            },
            KeybindConfig::Unbind {
                trigger: "cmd+t".to_string(),
            },
            KeybindConfig::Clear,
        ]
    );
}

#[test]
fn keybind_rejects_malformed_values() {
    for source in ["keybind =", "keybind = cmd+t", "keybind = =tab.new"] {
        let (overrides, diagnostics) = parse_overrides(path(), source);

        assert!(overrides.keybinds.is_empty(), "{source:?}");
        assert_eq!(diagnostics.len(), 1, "{source:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("keybind"));
        assert!(diagnostics[0].message.contains("invalid value"));
    }
}

// AC-WP0-01: `font-family` and its per-style variants parse for real
// (no "not yet supported" diagnostic) and land in `FontConfig`; an
// empty value yields a precise diagnostic instead.
#[test]
fn font_family_and_style_variants_are_retained_for_real() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "font-family = JetBrains Mono\n\
             font-family-bold = JetBrains Mono Bold\n\
             font-family-italic = JetBrains Mono Italic\n\
             font-family-bold-italic = JetBrains Mono Bold Italic",
    );

    assert!(diagnostics.is_empty());
    assert_eq!(overrides.font.families, vec!["JetBrains Mono".to_string()]);
    assert_eq!(
        overrides.font.families_bold,
        vec!["JetBrains Mono Bold".to_string()]
    );
    assert_eq!(
        overrides.font.families_italic,
        vec!["JetBrains Mono Italic".to_string()]
    );
    assert_eq!(
        overrides.font.families_bold_italic,
        vec!["JetBrains Mono Bold Italic".to_string()]
    );
}

#[test]
fn font_family_accumulates_a_stack_across_directives() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "font-family = JetBrains Mono\nfont-family = Fira Code",
    );

    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.font.families,
        vec!["JetBrains Mono".to_string(), "Fira Code".to_string()]
    );
}

#[test]
fn empty_font_family_value_produces_a_precise_diagnostic() {
    for key in [
        "font-family",
        "font-family-bold",
        "font-family-italic",
        "font-family-bold-italic",
    ] {
        let (overrides, diagnostics) = parse_overrides(path(), &format!("{key} ="));

        assert_eq!(overrides.font, FontConfig::default());
        assert_eq!(diagnostics.len(), 1, "{key}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains(key));
        assert!(diagnostics[0].message.contains("non-empty"));
    }
}

// AC-WP0-03: `font-feature`, `font-variation` (+ per-style variants), and
// `font-synthetic-style` parse and validate into `FontConfig`; malformed
// values yield a precise diagnostic.
#[test]
fn font_feature_parses_enabled_and_disabled_tags() {
    let (overrides, diagnostics) =
        parse_overrides(path(), "font-feature = calt\nfont-feature = -liga");

    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.font.features,
        vec![
            FontFeature {
                tag: *b"calt",
                enabled: true
            },
            FontFeature {
                tag: *b"liga",
                enabled: false
            },
        ]
    );
}

#[test]
fn malformed_font_feature_tag_produces_a_diagnostic() {
    for value in ["ca", "toolong", ""] {
        let (overrides, diagnostics) = parse_overrides(path(), &format!("font-feature = {value}"));

        assert!(overrides.font.features.is_empty(), "{value:?}");
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("font-feature"));
    }
}

#[test]
fn font_variation_and_style_variants_parse_axis_value_pairs() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "font-variation = wght=700\n\
             font-variation-bold = wght=800\n\
             font-variation-italic = slnt=-10\n\
             font-variation-bold-italic = wght=800",
    );

    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.font.variations,
        vec![FontVariation {
            tag: *b"wght",
            value: 700.0
        }]
    );
    assert_eq!(
        overrides.font.variations_bold,
        vec![FontVariation {
            tag: *b"wght",
            value: 800.0
        }]
    );
    assert_eq!(
        overrides.font.variations_italic,
        vec![FontVariation {
            tag: *b"slnt",
            value: -10.0
        }]
    );
    assert_eq!(
        overrides.font.variations_bold_italic,
        vec![FontVariation {
            tag: *b"wght",
            value: 800.0
        }]
    );
}

#[test]
fn font_variation_missing_value_produces_a_diagnostic() {
    for value in ["wght", "wght=", "toolong=700", "wght=not-a-number"] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("font-variation = {value}"));

        assert!(overrides.font.variations.is_empty(), "{value:?}");
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("font-variation"));
    }
}

#[test]
fn font_synthetic_style_accepts_all_documented_modes() {
    for (value, expected) in [
        ("true", SyntheticStyleMode::Both),
        ("false", SyntheticStyleMode::Neither),
        ("no-bold", SyntheticStyleMode::NoBold),
        ("no-italic", SyntheticStyleMode::NoItalic),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("font-synthetic-style = {value}"));

        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
        assert_eq!(overrides.font.synthetic_style, Some(expected));
    }
}

#[test]
fn font_synthetic_style_rejects_unknown_values() {
    let (overrides, diagnostics) = parse_overrides(path(), "font-synthetic-style = maybe");

    assert_eq!(overrides.font.synthetic_style, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("font-synthetic-style"));
    assert!(diagnostics[0].message.contains("maybe"));
}

// AC-WP0-04: `alpha-blending = linear` parses AND produces a fallback
// diagnostic; `alpha-blending = native` produces no diagnostic.
// `font-thicken` / `font-thicken-strength` are now consumed (glyph
// coverage dilation in noa-font), so they parse with no diagnostic.
#[test]
fn alpha_blending_native_is_real_with_no_diagnostic() {
    let (overrides, diagnostics) = parse_overrides(path(), "alpha-blending = native");

    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.font.alpha_blending,
        Some(AlphaBlendingMode::Native)
    );
}

#[test]
fn alpha_blending_linear_parses_and_falls_back_with_a_diagnostic() {
    for (value, expected) in [
        ("linear", AlphaBlendingMode::Linear),
        ("linear-corrected", AlphaBlendingMode::LinearCorrected),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("alpha-blending = {value}"));

        assert_eq!(overrides.font.alpha_blending, Some(expected));
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("alpha-blending"));
        assert!(diagnostics[0].message.contains("native"));
    }
}

#[test]
fn font_thicken_parses_with_no_diagnostic() {
    let (overrides, diagnostics) = parse_overrides(path(), "font-thicken = true");

    assert_eq!(overrides.font.thicken, Some(true));
    assert!(diagnostics.is_empty(), "consumed key emits no diagnostic");
}

#[test]
fn font_thicken_strength_parses_with_no_diagnostic() {
    let (overrides, diagnostics) = parse_overrides(path(), "font-thicken-strength = 128");

    assert_eq!(overrides.font.thicken_strength, Some(128));
    assert!(diagnostics.is_empty(), "consumed key emits no diagnostic");
}

#[test]
fn clipboard_read_parses_each_mode() {
    for (value, expected) in [
        ("deny", ClipboardAccess::Deny),
        ("ask", ClipboardAccess::Ask),
        ("allow", ClipboardAccess::Allow),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("clipboard-read = {value}"));
        assert_eq!(overrides.clipboard_read, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn clipboard_read_invalid_value_warns() {
    let (overrides, diagnostics) = parse_overrides(path(), "clipboard-read = maybe");

    assert_eq!(overrides.clipboard_read, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("clipboard-read"));
}

#[test]
fn clipboard_paste_protection_parses_bool() {
    let (overrides, diagnostics) = parse_overrides(path(), "clipboard-paste-protection = false");

    assert_eq!(overrides.clipboard_paste_protection, Some(false));
    assert!(diagnostics.is_empty());
}

#[test]
fn confirm_quit_parses_bool() {
    let (overrides, diagnostics) = parse_overrides(path(), "confirm-quit = false");

    assert_eq!(overrides.confirm_quit, Some(false));
    assert!(diagnostics.is_empty());
}

#[test]
fn title_report_parses_bool() {
    let (overrides, diagnostics) = parse_overrides(path(), "title-report = true");

    assert_eq!(overrides.title_report, Some(true));
    assert!(diagnostics.is_empty());
}

#[test]
fn window_padding_parses_non_negative_floats() {
    let (overrides, diagnostics) =
        parse_overrides(path(), "window-padding-x = 8\nwindow-padding-y = 4.5");

    assert!(diagnostics.is_empty());
    assert_eq!(overrides.window_padding_x, Some(8.0));
    assert_eq!(overrides.window_padding_y, Some(4.5));
}

#[test]
fn window_padding_rejects_negative_and_non_numeric() {
    for source in ["window-padding-x = -1", "window-padding-y = wide"] {
        let (overrides, diagnostics) = parse_overrides(path(), source);

        assert_eq!(overrides.window_padding_x, None);
        assert_eq!(overrides.window_padding_y, None);
        assert_eq!(diagnostics.len(), 1, "{source:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("window-padding"));
    }
}

#[test]
fn colors_parse_both_hex_forms_case_insensitively() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "background = #1a2B3c\n\
             foreground = FFEEdd\n\
             cursor-color = #00FF00\n\
             selection-foreground = 010203\n\
             selection-background = #abcdef",
    );

    assert!(diagnostics.is_empty());
    assert_eq!(overrides.background, Some(Rgb::new(0x1a, 0x2b, 0x3c)));
    assert_eq!(overrides.foreground, Some(Rgb::new(0xff, 0xee, 0xdd)));
    assert_eq!(overrides.cursor_color, Some(Rgb::new(0x00, 0xff, 0x00)));
    assert_eq!(overrides.selection_foreground, Some(Rgb::new(1, 2, 3)));
    assert_eq!(
        overrides.selection_background,
        Some(Rgb::new(0xab, 0xcd, 0xef))
    );
}

#[test]
fn minimum_contrast_accepts_wcag_ratio_range() {
    for (value, expected) in [("1", 1.0), ("1.1", 1.1), ("21", 21.0)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("minimum-contrast = {value}"));

        assert!(diagnostics.is_empty(), "{value}: {diagnostics:?}");
        assert_eq!(overrides.minimum_contrast, Some(expected));
    }
}

#[test]
fn minimum_contrast_rejects_out_of_range_values() {
    for value in ["0.9", "22", "nan", "hard"] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("minimum-contrast = {value}"));

        assert_eq!(overrides.minimum_contrast, None, "{value}");
        assert_eq!(diagnostics.len(), 1, "{value}");
        assert!(diagnostics[0].message.contains("minimum-contrast"));
    }
}

#[test]
fn invalid_color_warns_and_falls_back() {
    for source in [
        "background = #12345",
        "foreground = #12345g",
        "cursor-color = ghostty",
        "selection-background = #1234567",
    ] {
        let (overrides, diagnostics) = parse_overrides(path(), source);

        assert_eq!(overrides.background, None);
        assert_eq!(overrides.foreground, None);
        assert_eq!(overrides.cursor_color, None);
        assert_eq!(overrides.selection_background, None);
        assert_eq!(diagnostics.len(), 1, "{source:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("invalid value"));
    }
}

#[test]
fn cursor_style_parses_each_shape() {
    for (value, expected) in [
        ("block", CursorShape::Block),
        ("bar", CursorShape::Bar),
        ("underline", CursorShape::Underline),
    ] {
        let (overrides, diagnostics) = parse_overrides(path(), &format!("cursor-style = {value}"));

        assert_eq!(overrides.cursor_style, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn cursor_style_block_hollow_warns_and_is_ignored() {
    let (overrides, diagnostics) = parse_overrides(path(), "cursor-style = block_hollow");

    assert_eq!(overrides.cursor_style, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("block_hollow"));
    assert!(diagnostics[0].message.contains("not supported"));
}

#[test]
fn cursor_style_rejects_unknown_shape() {
    let (overrides, diagnostics) = parse_overrides(path(), "cursor-style = beam");

    assert_eq!(overrides.cursor_style, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("cursor-style"));
    assert!(diagnostics[0].message.contains("beam"));
}

#[test]
fn cursor_style_blink_parses_bool() {
    let (overrides, diagnostics) = parse_overrides(path(), "cursor-style-blink = false");

    assert_eq!(overrides.cursor_style_blink, Some(false));
    assert!(diagnostics.is_empty());
}

#[test]
fn background_opacity_clamps_without_diagnostic() {
    for (value, expected) in [("0.5", 0.5), ("-0.2", 0.0), ("1.4", 1.0)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-opacity = {value}"));

        assert_eq!(overrides.background_opacity, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn background_opacity_rejects_non_numeric() {
    let (overrides, diagnostics) = parse_overrides(path(), "background-opacity = opaque");

    assert_eq!(overrides.background_opacity, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("background-opacity"));
}

#[test]
fn background_blur_radius_parses_int_bool_and_clamps() {
    for (value, expected) in [
        ("0", 0),
        ("20", 20),
        ("true", 20),
        ("false", 0),
        ("999", 64),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-blur-radius = {value}"));

        assert_eq!(
            overrides.background_blur_radius,
            Some(expected),
            "{value:?}"
        );
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn background_blur_radius_rejects_non_integer() {
    let (overrides, diagnostics) = parse_overrides(path(), "background-blur-radius = blurry");

    assert_eq!(overrides.background_blur_radius, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("background-blur-radius"));
}

// AC-1: `background-image` parses to a path stored verbatim (tilde
// expansion is deferred to noa-app to keep this module IO-free).
#[test]
fn background_image_parses_path_verbatim() {
    let (overrides, diagnostics) = parse_overrides(path(), "background-image = /tmp/wall.png");
    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.background_image,
        Some(PathBuf::from("/tmp/wall.png"))
    );

    let (overrides, diagnostics) = parse_overrides(path(), "background-image = ~/pics/wall.png");
    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.background_image,
        Some(PathBuf::from("~/pics/wall.png"))
    );
}

// AC-2: `background-image-opacity` clamps like `background-opacity`.
#[test]
fn background_image_opacity_clamps_without_diagnostic() {
    for (value, expected) in [("0.5", 0.5), ("2.0", 1.0), ("-1", 0.0)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-image-opacity = {value}"));
        assert_eq!(
            overrides.background_image_opacity,
            Some(expected),
            "{value:?}"
        );
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
    // Absent → default 1.0 through apply_to.
    assert_eq!(
        ConfigOverrides::default()
            .apply_to(crate::StartupConfig::default())
            .background_image_opacity,
        1.0
    );
}

// AC-3: each of the 9 position tokens parses; invalid → diagnostic;
// absent → center default.
#[test]
fn background_image_position_parses_nine_anchors() {
    for (value, expected) in [
        ("top-left", BackgroundImagePosition::TopLeft),
        ("top-center", BackgroundImagePosition::TopCenter),
        ("top-right", BackgroundImagePosition::TopRight),
        ("center-left", BackgroundImagePosition::CenterLeft),
        ("center", BackgroundImagePosition::Center),
        ("center-right", BackgroundImagePosition::CenterRight),
        ("bottom-left", BackgroundImagePosition::BottomLeft),
        ("bottom-center", BackgroundImagePosition::BottomCenter),
        ("bottom-right", BackgroundImagePosition::BottomRight),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-image-position = {value}"));
        assert_eq!(
            overrides.background_image_position,
            Some(expected),
            "{value:?}"
        );
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }

    let (overrides, diagnostics) = parse_overrides(path(), "background-image-position = middle");
    assert_eq!(overrides.background_image_position, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("background-image-position"));

    assert_eq!(
        ConfigOverrides::default()
            .apply_to(crate::StartupConfig::default())
            .background_image_position,
        BackgroundImagePosition::Center
    );
}

// AC-4: each of none|contain|cover|stretch parses; invalid → diagnostic;
// absent → contain default.
#[test]
fn background_image_fit_parses_each_mode() {
    for (value, expected) in [
        ("none", BackgroundImageFit::None),
        ("contain", BackgroundImageFit::Contain),
        ("cover", BackgroundImageFit::Cover),
        ("stretch", BackgroundImageFit::Stretch),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-image-fit = {value}"));
        assert_eq!(overrides.background_image_fit, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }

    let (overrides, diagnostics) = parse_overrides(path(), "background-image-fit = fill");
    assert_eq!(overrides.background_image_fit, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("background-image-fit"));

    assert_eq!(
        ConfigOverrides::default()
            .apply_to(crate::StartupConfig::default())
            .background_image_fit,
        BackgroundImageFit::Contain
    );
}

// AC-5: `background-image-repeat` parses bool; absent → false.
#[test]
fn background_image_repeat_parses_bool() {
    for (value, expected) in [("true", true), ("false", false)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("background-image-repeat = {value}"));
        assert_eq!(
            overrides.background_image_repeat,
            Some(expected),
            "{value:?}"
        );
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
    assert!(
        !ConfigOverrides::default()
            .apply_to(crate::StartupConfig::default())
            .background_image_repeat
    );
}

// AC-6: all five keys are supported scalar keys for Ghostty import.
#[test]
fn background_image_keys_are_supported_scalar_keys_for_import() {
    for key in [
        "background-image",
        "background-image-opacity",
        "background-image-position",
        "background-image-fit",
        "background-image-repeat",
    ] {
        assert!(is_supported_scalar_key(key), "{key}");
    }
}

#[test]
fn scrollback_limit_parses_byte_counts_including_zero() {
    for (value, expected) in [("10000000", 10_000_000), ("0", 0), ("512", 512)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("scrollback-limit = {value}"));

        assert_eq!(overrides.scrollback_limit, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn scrollback_limit_rejects_negative_and_non_numeric() {
    for value in ["-1", "lots", "1.5"] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("scrollback-limit = {value}"));

        assert_eq!(overrides.scrollback_limit, None, "{value:?}");
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("scrollback-limit"));
    }
}

#[test]
fn scrollback_limit_is_a_supported_scalar_key_for_import() {
    assert!(is_supported_scalar_key("scrollback-limit"));
}

#[test]
fn alpha_blending_is_a_supported_scalar_key_for_import() {
    assert!(is_supported_scalar_key("alpha-blending"));
}

#[test]
fn window_save_state_parses_each_mode() {
    for (value, expected) in [
        ("default", WindowSaveState::Default),
        ("never", WindowSaveState::Never),
        ("always", WindowSaveState::Always),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("window-save-state = {value}"));
        assert_eq!(overrides.window_save_state, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn window_save_state_rejects_unknown_value() {
    let (overrides, diagnostics) = parse_overrides(path(), "window-save-state = sometimes");

    assert_eq!(overrides.window_save_state, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("window-save-state"));
    assert!(diagnostics[0].message.contains("sometimes"));
}

#[test]
fn window_save_state_is_a_supported_scalar_key_for_import() {
    assert!(is_supported_scalar_key("window-save-state"));
}

#[test]
fn macos_option_as_alt_parses_modes() {
    for (value, expected) in [
        ("false", MacosOptionAsAlt::None),
        ("none", MacosOptionAsAlt::None),
        ("true", MacosOptionAsAlt::Both),
        ("both", MacosOptionAsAlt::Both),
        ("left", MacosOptionAsAlt::Left),
        ("only-left", MacosOptionAsAlt::Left),
        ("right", MacosOptionAsAlt::Right),
        ("only-right", MacosOptionAsAlt::Right),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("macos-option-as-alt = {value}"));
        assert_eq!(overrides.macos_option_as_alt, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn macos_option_as_alt_rejects_unknown_value() {
    let (overrides, diagnostics) = parse_overrides(path(), "macos-option-as-alt = maybe");

    assert_eq!(overrides.macos_option_as_alt, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("macos-option-as-alt"));
    assert!(diagnostics[0].message.contains("maybe"));
}

#[test]
fn macos_titlebar_style_parses_modes() {
    for (value, expected) in [
        ("native", MacosTitlebarStyle::Native),
        ("tabs", MacosTitlebarStyle::Native),
        ("transparent", MacosTitlebarStyle::Transparent),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("macos-titlebar-style = {value}"));
        assert_eq!(overrides.macos_titlebar_style, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn macos_titlebar_style_rejects_unknown_value() {
    let (overrides, diagnostics) = parse_overrides(path(), "macos-titlebar-style = glass");

    assert_eq!(overrides.macos_titlebar_style, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("macos-titlebar-style"));
    assert!(diagnostics[0].message.contains("glass"));
}

#[test]
fn macos_non_native_fullscreen_parses_bool() {
    for (value, expected) in [("true", true), ("false", false)] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("macos-non-native-fullscreen = {value}"));

        assert_eq!(
            overrides.macos_non_native_fullscreen,
            Some(expected),
            "{value:?}"
        );
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn macos_non_native_fullscreen_rejects_unknown_value() {
    let (overrides, diagnostics) = parse_overrides(path(), "macos-non-native-fullscreen = maybe");

    assert_eq!(overrides.macos_non_native_fullscreen, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0]
            .message
            .contains("macos-non-native-fullscreen")
    );
    assert!(diagnostics[0].message.contains("maybe"));
}

#[test]
fn macos_native_keys_are_supported_scalar_keys_for_import() {
    assert!(is_supported_scalar_key("macos-option-as-alt"));
    assert!(is_supported_scalar_key("macos-titlebar-style"));
    assert!(is_supported_scalar_key("macos-non-native-fullscreen"));
}

#[test]
fn quick_terminal_hotkey_is_retained_verbatim() {
    let (overrides, diagnostics) = parse_overrides(path(), "quick-terminal-hotkey = cmd+grave");

    assert!(diagnostics.is_empty());
    assert_eq!(
        overrides.quick_terminal_hotkey.as_deref(),
        Some("cmd+grave")
    );
}

#[test]
fn quick_terminal_hotkey_none_disables_via_empty_sentinel() {
    // `none` (and empty) normalize to the empty-string sentinel so they
    // override the built-in default hotkey through the `.or()` merge.
    for input in [
        "quick-terminal-hotkey = none",
        "quick-terminal-hotkey = off",
        "quick-terminal-hotkey =",
    ] {
        let (overrides, diagnostics) = parse_overrides(path(), input);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics for {input:?}"
        );
        assert_eq!(
            overrides.quick_terminal_hotkey.as_deref(),
            Some(""),
            "{input:?} should disable via empty sentinel"
        );
    }
}

#[test]
fn quick_terminal_size_parses_fraction_and_percent_and_clamps() {
    for (value, expected) in [
        ("0.4", 0.4),
        ("40%", 0.4),
        ("100%", 1.0),
        ("2.0", 1.0),
        ("0.01", 0.1),
    ] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("quick-terminal-size = {value}"));

        assert_eq!(overrides.quick_terminal_size, Some(expected), "{value:?}");
        assert!(diagnostics.is_empty(), "{value:?}: {diagnostics:?}");
    }
}

#[test]
fn quick_terminal_size_rejects_non_numeric() {
    let (overrides, diagnostics) = parse_overrides(path(), "quick-terminal-size = tall");

    assert_eq!(overrides.quick_terminal_size, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("quick-terminal-size"));
}

#[test]
fn quick_terminal_autohide_parses_bool() {
    let (overrides, diagnostics) = parse_overrides(path(), "quick-terminal-autohide = false");

    assert_eq!(overrides.quick_terminal_autohide, Some(false));
    assert!(diagnostics.is_empty());
}

#[test]
fn quick_terminal_keys_are_supported_scalar_keys_for_import() {
    assert!(is_supported_scalar_key("quick-terminal-hotkey"));
    assert!(is_supported_scalar_key("quick-terminal-size"));
    assert!(is_supported_scalar_key("quick-terminal-autohide"));
}

// AC-15a: `sidebar-enabled`/`sidebar-width` parse; the default width (360)
// and preview line count (3) are applied when the keys are absent.
#[test]
fn sidebar_enabled_and_width_parse_and_default() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "sidebar-enabled = true\nsidebar-width = 300\nsidebar-preview-lines = 4\nauto-approve = true",
    );

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(overrides.sidebar_enabled, Some(true));
    assert_eq!(overrides.sidebar_width, Some(300.0));
    assert_eq!(overrides.sidebar_preview_lines, Some(4));
    assert_eq!(overrides.auto_approve, Some(true));

    // Absent width falls back to the default via `apply_to`.
    let resolved = ConfigOverrides::default().apply_to(crate::StartupConfig::default());
    assert!(!resolved.sidebar_enabled);
    assert_eq!(resolved.sidebar_width, crate::DEFAULT_SIDEBAR_WIDTH);
    assert_eq!(resolved.sidebar_width, 360.0);
    assert_eq!(
        resolved.sidebar_preview_lines,
        crate::DEFAULT_SIDEBAR_PREVIEW_LINES
    );
    assert_eq!(resolved.sidebar_preview_lines, 5);
}

#[test]
fn sidebar_width_rejects_negative_and_non_numeric() {
    for value in ["-1", "wide"] {
        let (overrides, diagnostics) = parse_overrides(path(), &format!("sidebar-width = {value}"));

        assert_eq!(overrides.sidebar_width, None, "{value:?}");
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("sidebar-width"));
    }
}

#[test]
fn sidebar_preview_lines_rejects_negative_non_numeric_and_too_large() {
    for value in ["-1", "many", "11"] {
        let (overrides, diagnostics) =
            parse_overrides(path(), &format!("sidebar-preview-lines = {value}"));

        assert_eq!(overrides.sidebar_preview_lines, None, "{value:?}");
        assert_eq!(diagnostics.len(), 1, "{value:?}: {diagnostics:?}");
        assert!(diagnostics[0].message.contains("sidebar-preview-lines"));
    }
}

// AC-15b: a valid chord is accepted verbatim through the same path as
// `quick-terminal-hotkey`; `none`/empty normalize to the disabled sentinel.
// Chord *semantics* are validated by the app-layer `parse_hotkey` at
// registration (noa-config cannot depend on noa-app), matching how
// `quick-terminal-hotkey` defers validation.
#[test]
fn sidebar_hotkey_is_retained_verbatim() {
    let (overrides, diagnostics) = parse_overrides(path(), "sidebar-hotkey = cmd+shift+s");

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(overrides.sidebar_hotkey.as_deref(), Some("cmd+shift+s"));
}

#[test]
fn sidebar_hotkey_none_disables_via_empty_sentinel() {
    for input in [
        "sidebar-hotkey = none",
        "sidebar-hotkey = off",
        "sidebar-hotkey =",
    ] {
        let (overrides, diagnostics) = parse_overrides(path(), input);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics for {input:?}"
        );
        assert_eq!(
            overrides.sidebar_hotkey.as_deref(),
            Some(""),
            "{input:?} should disable via empty sentinel"
        );
    }
}

#[test]
fn sidebar_keys_are_supported_scalar_keys_for_import() {
    assert!(is_supported_scalar_key("sidebar-enabled"));
    assert!(is_supported_scalar_key("sidebar-width"));
    assert!(is_supported_scalar_key("sidebar-hotkey"));
    assert!(is_supported_scalar_key("sidebar-preview-lines"));
    assert!(is_supported_scalar_key("auto-approve"));
    assert!(is_supported_scalar_key("confirm-quit"));
}

#[test]
fn bell_keys_parse_and_are_supported_scalar_keys_for_import() {
    let (overrides, diagnostics) = parse_overrides(
        path(),
        "visual-bell = true\n\
         audible-bell = true\n\
         audible-bell-when-unfocused = true\n\
         audible-bell-dock-bounce = true",
    );

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(overrides.visual_bell, Some(true));
    assert_eq!(overrides.audible_bell, Some(true));
    assert_eq!(overrides.audible_bell_when_unfocused, Some(true));
    assert_eq!(overrides.audible_bell_dock_bounce, Some(true));
    for key in [
        "visual-bell",
        "audible-bell",
        "audible-bell-when-unfocused",
        "audible-bell-dock-bounce",
    ] {
        assert!(is_supported_scalar_key(key), "{key}");
    }
}

#[test]
fn invalid_values_warn_and_fall_back() {
    let (overrides, diagnostics) = parse_overrides(path(), "font-size = not-a-number");

    assert_eq!(overrides.font_size, None);
    assert_eq!(diagnostics.len(), 1);
    let message = &diagnostics[0].message;
    assert!(message.contains("/tmp/noa-test-config"));
    assert!(message.contains("font-size"));
    assert!(message.contains("not-a-number"));
}

#[test]
fn theme_accepts_unquoted_and_quoted_names() {
    for source in ["theme = 3024 Day", "theme = \"3024 Day\""] {
        let (overrides, diagnostics) = parse_overrides(path(), source);

        assert!(diagnostics.is_empty());
        assert_eq!(overrides.theme.as_deref(), Some("3024 Day"));
    }
}

#[test]
fn theme_pair_syntax_warns_without_partial_acceptance() {
    let (overrides, diagnostics) = parse_overrides(path(), "theme = light:Foo,dark:Bar");

    assert_eq!(overrides.theme, None);
    assert_eq!(diagnostics.len(), 1);
    let message = &diagnostics[0].message;
    assert!(message.contains("light:"));
    assert!(message.contains("dark:"));
    assert!(message.contains("single theme name"));
    assert!(!message.contains("unsupported key"));
    assert!(!message.contains("invalid value"));
}

#[test]
fn window_size_requires_pairs_and_clamps_lower_bounds() {
    let (width_only, width_only_diagnostics) = parse_overrides(path(), "window-width = 120");
    assert_eq!(width_only.cols, None);
    assert_eq!(width_only.rows, None);
    assert_eq!(width_only_diagnostics.len(), 1);

    let (height_only, height_only_diagnostics) = parse_overrides(path(), "window-height = 30");
    assert_eq!(height_only.cols, None);
    assert_eq!(height_only.rows, None);
    assert_eq!(height_only_diagnostics.len(), 1);

    let (clamped, diagnostics) = parse_overrides(path(), "window-width = 9\nwindow-height = 2");
    assert!(diagnostics.is_empty());
    assert_eq!(clamped.cols, Some(10));
    assert_eq!(clamped.rows, Some(4));
}

#[test]
fn invalid_window_value_flows_to_pair_missing_rule() {
    let (overrides, diagnostics) =
        parse_overrides(path(), "window-width = abc\nwindow-height = 30");
    let messages = diagnostic_messages(&diagnostics);

    assert_eq!(overrides.cols, None);
    assert_eq!(overrides.rows, None);
    assert!(messages.iter().any(|message| message.contains("abc")));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("must be set together"))
    );
}

#[test]
fn parse_overrides_is_deterministic() {
    let source =
        "bogus-key = x\nfont-size = 15\nwindow-width = 120\nwindow-height = 30\ntheme = Foo";

    assert_eq!(
        parse_overrides(path(), source),
        parse_overrides(path(), source)
    );
}

#[test]
fn parser_module_stays_io_free() {
    let source = [
        include_str!("../parser.rs"),
        include_str!("diagnostics.rs"),
        include_str!("directives.rs"),
        include_str!("overrides.rs"),
        include_str!("values.rs"),
    ]
    .join("\n");
    for forbidden in [
        ["std::", "fs"].concat(),
        ["std::", "env"].concat(),
        ["dirs", "::"].concat(),
    ] {
        assert!(!source.contains(&forbidden), "{forbidden}");
    }
}
