use std::path::Path;

use crate::{
    AlphaBlendingMode, ConfigOverrides, FontConfig, FontFeature, FontVariation, SyntheticStyleMode,
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
        },
        diagnostics,
    )
}

pub(crate) fn is_supported_scalar_key(key: &str) -> bool {
    matches!(
        key,
        "window-width" | "window-height" | "font-size" | "theme"
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
    diagnostics.push(deferred_diagnostic(path, &directive.key));
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
    diagnostics.push(deferred_diagnostic(path, &directive.key));
    Some(parsed)
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

fn deferred_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `{key}` is accepted but has no effect yet (deferred)",
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

    // AC-WP0-04: `alpha-blending = linear`, `font-thicken = true`, and
    // `font-thicken-strength = 128` each parse AND produce a fallback /
    // deferred diagnostic; `alpha-blending = native` produces no diagnostic.
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
    fn font_thicken_parses_and_produces_a_deferred_diagnostic() {
        let (overrides, diagnostics) = parse_overrides(path(), "font-thicken = true");

        assert_eq!(overrides.font.thicken, Some(true));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("font-thicken"));
        assert!(diagnostics[0].message.contains("deferred"));
    }

    #[test]
    fn font_thicken_strength_parses_and_produces_a_deferred_diagnostic() {
        let (overrides, diagnostics) = parse_overrides(path(), "font-thicken-strength = 128");

        assert_eq!(overrides.font.thicken_strength, Some(128));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("font-thicken-strength"));
        assert!(diagnostics[0].message.contains("deferred"));
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
