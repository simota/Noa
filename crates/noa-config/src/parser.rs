use std::path::Path;

use crate::ConfigOverrides;

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
            "keybind" | "palette" | "font-family" => {
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
        for key in ["keybind", "palette", "font-family"] {
            let (overrides, diagnostics) = parse_overrides(path(), &format!("{key} = value"));

            assert_eq!(overrides, ConfigOverrides::default());
            assert_eq!(diagnostics.len(), 1);
            assert!(diagnostics[0].message.contains(key));
            assert!(diagnostics[0].message.contains("list key"));
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
