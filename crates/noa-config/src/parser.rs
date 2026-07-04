use std::path::Path;

use noa_core::Rgb;

use crate::{
    AlphaBlendingMode, ClipboardAccess, ConfigOverrides, CursorShape, FontConfig, FontFeature,
    FontVariation, SyntheticStyleMode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub line: usize,
    pub key: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

pub fn parse_directives(source: &str) -> Vec<Directive> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_line(index + 1, line))
        .collect()
}

pub fn parse_overrides(path: &Path, source: &str) -> (ConfigOverrides, Vec<Diagnostic>) {
    let directives = parse_directives(source);
    build_overrides(path, &directives)
}

pub(crate) fn build_overrides(
    path: &Path,
    directives: &[Directive],
) -> (ConfigOverrides, Vec<Diagnostic>) {
    let mut cols = None;
    let mut rows = None;
    let mut font_size = None;
    let mut theme = None;
    let mut font = FontConfig::default();
    let mut clipboard_read = None;
    let mut clipboard_paste_protection = None;
    let mut window_padding_x = None;
    let mut window_padding_y = None;
    let mut background = None;
    let mut foreground = None;
    let mut cursor_color = None;
    let mut selection_foreground = None;
    let mut selection_background = None;
    let mut cursor_style = None;
    let mut cursor_style_blink = None;
    let mut background_opacity = None;
    let mut background_blur_radius = None;
    let mut diagnostics = Vec::new();

    for directive in directives {
        match directive.key.as_str() {
            "window-width" => {
                cols = parse_u16(path, directive, &mut diagnostics);
            }
            "window-height" => {
                rows = parse_u16(path, directive, &mut diagnostics);
            }
            "font-size" => {
                font_size = parse_font_size(path, directive, &mut diagnostics);
            }
            "theme" => {
                theme = parse_theme(path, directive, &mut diagnostics);
            }
            "font-family" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families);
            }
            "font-family-bold" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families_bold);
            }
            "font-family-italic" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families_italic);
            }
            "font-family-bold-italic" => {
                parse_family(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.families_bold_italic,
                );
            }
            "font-feature" => {
                parse_font_feature(path, directive, &mut diagnostics, &mut font.features);
            }
            "font-variation" => {
                parse_font_variation(path, directive, &mut diagnostics, &mut font.variations);
            }
            "font-variation-bold" => {
                parse_font_variation(path, directive, &mut diagnostics, &mut font.variations_bold);
            }
            "font-variation-italic" => {
                parse_font_variation(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.variations_italic,
                );
            }
            "font-variation-bold-italic" => {
                parse_font_variation(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.variations_bold_italic,
                );
            }
            "font-synthetic-style" => {
                font.synthetic_style = parse_synthetic_style(path, directive, &mut diagnostics);
            }
            "alpha-blending" => {
                font.alpha_blending = parse_alpha_blending(path, directive, &mut diagnostics);
            }
            "font-thicken" => {
                font.thicken = parse_font_thicken(path, directive, &mut diagnostics);
            }
            "font-thicken-strength" => {
                font.thicken_strength =
                    parse_font_thicken_strength(path, directive, &mut diagnostics);
            }
            "clipboard-read" => {
                clipboard_read = parse_clipboard_read(path, directive, &mut diagnostics);
            }
            "clipboard-paste-protection" => {
                clipboard_paste_protection =
                    parse_bool_directive(path, directive, &mut diagnostics);
            }
            "window-padding-x" => {
                window_padding_x = parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "window-padding-y" => {
                window_padding_y = parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "background" => {
                background = parse_color(path, directive, &mut diagnostics);
            }
            "foreground" => {
                foreground = parse_color(path, directive, &mut diagnostics);
            }
            "cursor-color" => {
                cursor_color = parse_color(path, directive, &mut diagnostics);
            }
            "selection-foreground" => {
                selection_foreground = parse_color(path, directive, &mut diagnostics);
            }
            "selection-background" => {
                selection_background = parse_color(path, directive, &mut diagnostics);
            }
            "cursor-style" => {
                cursor_style = parse_cursor_style(path, directive, &mut diagnostics);
            }
            "cursor-style-blink" => {
                cursor_style_blink = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "background-opacity" => {
                background_opacity = parse_opacity(path, directive, &mut diagnostics);
            }
            "background-blur-radius" => {
                background_blur_radius = parse_blur_radius(path, directive, &mut diagnostics);
            }
            "keybind" | "palette" => {
                diagnostics.push(list_key_diagnostic(path, &directive.key));
            }
            "config-file" => {
                diagnostics.push(config_file_diagnostic(path));
            }
            unknown => {
                diagnostics.push(unknown_key_diagnostic(path, unknown));
            }
        }
    }

    if cols.is_some() ^ rows.is_some() {
        diagnostics.push(window_pair_diagnostic(path));
        cols = None;
        rows = None;
    } else if let (Some(width), Some(height)) = (cols, rows) {
        cols = Some(width.max(10));
        rows = Some(height.max(4));
    }

    (
        ConfigOverrides {
            cols,
            rows,
            font_size,
            theme,
            font,
            clipboard_read,
            clipboard_paste_protection,
            window_padding_x,
            window_padding_y,
            background,
            foreground,
            cursor_color,
            selection_foreground,
            selection_background,
            cursor_style,
            cursor_style_blink,
            background_opacity,
            background_blur_radius,
        },
        diagnostics,
    )
}

pub(crate) fn is_supported_scalar_key(key: &str) -> bool {
    matches!(
        key,
        "window-width"
            | "window-height"
            | "font-size"
            | "theme"
            | "clipboard-read"
            | "clipboard-paste-protection"
            | "window-padding-x"
            | "window-padding-y"
            | "background"
            | "foreground"
            | "cursor-color"
            | "selection-foreground"
            | "selection-background"
            | "cursor-style"
            | "cursor-style-blink"
            | "background-opacity"
            | "background-blur-radius"
    )
}

fn parse_line(line_number: usize, line: &str) -> Option<Directive> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let trimmed_start = line.trim_start();
    if trimmed_start.is_empty() || trimmed_start.starts_with('#') {
        return None;
    }

    let (key, raw_value) = line.split_once('=')?;
    let key = key.trim();
    let value = parse_value(raw_value);

    Some(Directive {
        line: line_number,
        key: key.to_string(),
        value,
    })
}

fn parse_value(raw_value: &str) -> Option<String> {
    let value = raw_value.trim();
    if value.is_empty() {
        return None;
    }

    if is_well_quoted(value) {
        return Some(value[1..value.len() - 1].to_string());
    }

    Some(value.to_string())
}

fn is_well_quoted(value: &str) -> bool {
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value[1..value.len() - 1].contains('"')
}

fn parse_u16(path: &Path, directive: &Directive, diagnostics: &mut Vec<Diagnostic>) -> Option<u16> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<i64>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    let Ok(parsed) = u16::try_from(parsed) else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    Some(parsed)
}

fn parse_font_size(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || parsed <= 0.0 {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

fn parse_theme(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let value = directive.value.as_deref()?;
    if value.starts_with("light:") || value.starts_with("dark:") {
        diagnostics.push(theme_pair_diagnostic(path));
        return None;
    }
    Some(value.to_string())
}

fn parse_family(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<String>,
) {
    match directive.value.as_deref() {
        Some(value) if !value.is_empty() => target.push(value.to_string()),
        _ => diagnostics.push(empty_family_diagnostic(path, &directive.key)),
    }
}

fn parse_font_feature(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<FontFeature>,
) {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_font_feature_diagnostic(path, ""));
        return;
    };
    let (enabled, tag_str) = match value.strip_prefix('-') {
        Some(rest) => (false, rest),
        None => (true, value),
    };
    let Some(tag) = ascii_tag4(tag_str) else {
        diagnostics.push(invalid_font_feature_diagnostic(path, value));
        return;
    };
    target.push(FontFeature { tag, enabled });
}

fn parse_font_variation(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<FontVariation>,
) {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_font_variation_diagnostic(path, &directive.key, ""));
        return;
    };
    let Some((axis, value_str)) = value.split_once('=') else {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    };
    let (Some(tag), Ok(parsed)) = (ascii_tag4(axis), value_str.parse::<f32>()) else {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    };
    if !parsed.is_finite() {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    }
    target.push(FontVariation { tag, value: parsed });
}

/// Parse a 4-ASCII-char OpenType tag (feature tag or variation axis).
fn ascii_tag4(tag_str: &str) -> Option<[u8; 4]> {
    let bytes = tag_str.as_bytes();
    if bytes.len() != 4 || !tag_str.is_ascii() {
        return None;
    }
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn parse_synthetic_style(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SyntheticStyleMode> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(SyntheticStyleMode::Both),
        "false" => Some(SyntheticStyleMode::Neither),
        "no-bold" => Some(SyntheticStyleMode::NoBold),
        "no-italic" => Some(SyntheticStyleMode::NoItalic),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

fn parse_alpha_blending(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<AlphaBlendingMode> {
    let value = directive.value.as_deref()?;
    match value {
        "native" => Some(AlphaBlendingMode::Native),
        "linear" => {
            diagnostics.push(alpha_blending_fallback_diagnostic(path, value));
            Some(AlphaBlendingMode::Linear)
        }
        "linear-corrected" => {
            diagnostics.push(alpha_blending_fallback_diagnostic(path, value));
            Some(AlphaBlendingMode::LinearCorrected)
        }
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

fn parse_font_thicken(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    let value = directive.value.as_deref()?;
    let parsed = match value {
        "true" => true,
        "false" => false,
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            return None;
        }
    };
    Some(parsed)
}

fn parse_font_thicken_strength(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u8> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<u8>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    Some(parsed)
}

fn parse_clipboard_read(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ClipboardAccess> {
    let value = directive.value.as_deref()?;
    match value {
        "deny" | "false" => Some(ClipboardAccess::Deny),
        "ask" => Some(ClipboardAccess::Ask),
        "allow" | "true" => Some(ClipboardAccess::Allow),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

fn parse_bool_directive(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(true),
        "false" => Some(false),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

fn parse_non_negative_f32(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || parsed < 0.0 {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Clamp an `f32` to `0.0..=1.0`. Ghostty clamps out-of-range values without
/// complaint, so only an unparseable value produces a diagnostic.
fn parse_opacity(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed.clamp(0.0, 1.0))
}

/// Maximum `background-blur-radius`. Beyond this the CGS blur stops looking
/// different and just costs compositor time.
const MAX_BLUR_RADIUS: u16 = 64;
/// Ghostty maps the boolean `true` form of `background-blur-radius` to 20.
const DEFAULT_BLUR_RADIUS: u16 = 20;

/// Parse `background-blur-radius`: a non-negative integer, or the boolean
/// shorthand Ghostty also accepts (`true` = 20, `false` = 0). Out-of-range
/// integers clamp to `0..=MAX_BLUR_RADIUS`; only an unparseable value produces
/// a diagnostic.
fn parse_blur_radius(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u16> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(DEFAULT_BLUR_RADIUS),
        "false" => Some(0),
        _ => match value.parse::<u32>() {
            Ok(parsed) => Some(parsed.min(u32::from(MAX_BLUR_RADIUS)) as u16),
            Err(_) => {
                diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
                None
            }
        },
    }
}

/// Parse a `#RRGGBB` or `RRGGBB` (case-insensitive) hex color.
fn parse_color(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Rgb> {
    let value = directive.value.as_deref()?;
    match rgb_from_hex(value) {
        Some(rgb) => Some(rgb),
        None => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
            None
        }
    }
}

fn rgb_from_hex(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb::new(r, g, b))
}

fn parse_cursor_style(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<CursorShape> {
    let value = directive.value.as_deref()?;
    match value {
        "block" => Some(CursorShape::Block),
        "bar" => Some(CursorShape::Bar),
        "underline" => Some(CursorShape::Underline),
        "block_hollow" => {
            diagnostics.push(cursor_style_unsupported_diagnostic(path, value));
            None
        }
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

fn cursor_style_unsupported_diagnostic(path: &Path, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `cursor-style = {value}` is not supported yet; value ignored",
            path.display()
        ),
    }
}

fn unknown_key_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!("config {}: unsupported key `{key}` ignored", path.display()),
    }
}

fn list_key_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: list key `{key}` is recognized but not supported yet; value ignored",
            path.display()
        ),
    }
}

fn config_file_diagnostic(path: &Path) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `config-file` includes are recognized but not supported yet; value ignored",
            path.display()
        ),
    }
}

fn invalid_value_diagnostic(path: &Path, key: &str, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `{key}`: `{value}`; using default",
            path.display()
        ),
    }
}

fn theme_pair_diagnostic(path: &Path) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `light:`/`dark:` theme pair syntax is not supported yet; specify a single theme name",
            path.display()
        ),
    }
}

fn empty_family_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `{key}` requires a non-empty font family name; value ignored",
            path.display()
        ),
    }
}

fn invalid_font_feature_diagnostic(path: &Path, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `font-feature`: `{value}`; expected a 4-character \
             OpenType tag, optionally prefixed with `-` to disable (e.g. `calt`, `-liga`)",
            path.display()
        ),
    }
}

fn invalid_font_variation_diagnostic(path: &Path, key: &str, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `{key}`: `{value}`; expected `AXIS=VALUE` with a \
             4-character axis tag and a numeric value (e.g. `wght=700`)",
            path.display()
        ),
    }
}

fn alpha_blending_fallback_diagnostic(path: &Path, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `alpha-blending = {value}` is not implemented yet; falling back to `native`",
            path.display()
        ),
    }
}

fn window_pair_diagnostic(path: &Path) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `window-width` and `window-height` must be set together; ignoring both",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
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
        let (_, list) = parse_overrides(path(), "keybind = cmd+n=new_tab");
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
    fn list_keys_are_recognized_but_not_retained() {
        for key in ["keybind", "palette"] {
            let (overrides, diagnostics) = parse_overrides(path(), &format!("{key} = value"));

            assert_eq!(overrides, ConfigOverrides::default());
            assert_eq!(diagnostics.len(), 1);
            assert!(diagnostics[0].message.contains(key));
            assert!(diagnostics[0].message.contains("list key"));
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
            let (overrides, diagnostics) =
                parse_overrides(path(), &format!("font-feature = {value}"));

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
        let (overrides, diagnostics) =
            parse_overrides(path(), "clipboard-paste-protection = false");

        assert_eq!(overrides.clipboard_paste_protection, Some(false));
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
            let (overrides, diagnostics) =
                parse_overrides(path(), &format!("cursor-style = {value}"));

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

            assert_eq!(overrides.background_blur_radius, Some(expected), "{value:?}");
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
        let source = include_str!("parser.rs");
        for forbidden in [
            ["std::", "fs"].concat(),
            ["std::", "env"].concat(),
            ["dirs", "::"].concat(),
        ] {
            assert!(!source.contains(&forbidden), "{forbidden}");
        }
    }
}
