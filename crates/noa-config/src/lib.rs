//! Startup configuration discovery, parsing, validation, and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

mod ghostty;
mod import;
mod parser;

pub use ghostty::{ghostty_config_candidates, ghostty_config_candidates_from};
pub use import::{
    ImportOutcome, ImportStats, build_import_output, import_ghostty_config,
    import_ghostty_config_at,
};
pub use parser::{Diagnostic, Directive, parse_directives, parse_overrides};

pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// Resolved, validated startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            font_size: DEFAULT_FONT_SIZE,
            theme: None,
        }
    }
}

/// Optional values from a config file or explicit CLI flags.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConfigOverrides {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
}

impl ConfigOverrides {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            cols: higher_priority.cols.or(self.cols),
            rows: higher_priority.rows.or(self.rows),
            font_size: higher_priority.font_size.or(self.font_size),
            theme: higher_priority.theme.or(self.theme),
        }
    }

    pub fn apply_to(self, base: StartupConfig) -> StartupConfig {
        StartupConfig {
            cols: self.cols.unwrap_or(base.cols),
            rows: self.rows.unwrap_or(base.rows),
            font_size: self.font_size.unwrap_or(base.font_size),
            theme: self.theme.or(base.theme),
        }
    }
}

pub fn load_startup_config(
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (Some(config_path), Some(legacy_path)) = (default_config_path(), legacy_toml_config_path())
    else {
        let config = cli.apply_to(StartupConfig::default());
        validate_startup_config(&config, "resolved startup config")?;
        return Ok((config, Vec::new()));
    };
    load_startup_config_from(&config_path, &legacy_path, cli)
}

pub fn load_startup_config_from(
    config_path: &Path,
    legacy_path: &Path,
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (file, mut diagnostics) = if config_path.exists() {
        load_overrides_from_path(config_path)?
    } else {
        (ConfigOverrides::default(), Vec::new())
    };

    if legacy_path.exists() {
        diagnostics.push(Diagnostic {
            message: format!(
                "legacy TOML config {} is no longer read; move settings to {}",
                legacy_path.display(),
                config_path.display()
            ),
        });
    }

    let config = file.merge(cli).apply_to(StartupConfig::default());
    validate_startup_config(&config, "resolved startup config")?;
    Ok((config, diagnostics))
}

pub fn load_file_overrides() -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let Some(path) = default_config_path() else {
        return Ok((ConfigOverrides::default(), Vec::new()));
    };
    if !path.exists() {
        return Ok((ConfigOverrides::default(), Vec::new()));
    }
    load_overrides_from_path(&path)
}

pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| default_config_path_in(&path))
}

pub fn default_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config")
}

pub fn legacy_toml_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| legacy_toml_config_path_in(&path))
}

pub fn legacy_toml_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config.toml")
}

pub fn load_overrides_from_path(path: &Path) -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    Ok(parse_overrides(path, &source))
}

pub fn validate_startup_config(config: &StartupConfig, context: &str) -> anyhow::Result<()> {
    validate_grid_dimension(config.cols, context, "cols")?;
    validate_grid_dimension(config.rows, context, "rows")?;
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        bail!("invalid {context}: `font_size` must be a positive finite number");
    }
    Ok(())
}

pub fn validate_grid_dimension(value: u16, context: &str, key: &'static str) -> anyhow::Result<()> {
    if value == 0 {
        bail!(
            "invalid {context}: `{key}` must be an integer between 1 and {}",
            u16::MAX
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> &'static Path {
        Path::new("/tmp/noa-test-config")
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-config-lib-{name}-{}", std::process::id()))
    }

    #[test]
    fn defaults_match_existing_startup_behavior() {
        assert_eq!(
            StartupConfig::default(),
            StartupConfig {
                cols: 80,
                rows: 24,
                font_size: 14.0,
                theme: None,
            }
        );
    }

    #[test]
    fn parses_supported_config_keys() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            r#"
window-width = 100
window-height = 30
font-size = 15.5
"#,
        );

        assert!(diagnostics.is_empty());
        assert_eq!(
            overrides,
            ConfigOverrides {
                cols: Some(100),
                rows: Some(30),
                font_size: Some(15.5),
                theme: None,
            }
        );
    }

    #[test]
    fn cli_overrides_config_file_values() {
        let file = ConfigOverrides {
            cols: Some(100),
            rows: Some(30),
            font_size: Some(15.5),
            theme: Some("3024 Day".to_string()),
        };
        let cli = ConfigOverrides {
            cols: Some(120),
            rows: None,
            font_size: Some(16.0),
            theme: None,
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(
            config,
            StartupConfig {
                cols: 120,
                rows: 30,
                font_size: 16.0,
                theme: Some("3024 Day".to_string()),
            }
        );
    }

    #[test]
    fn theme_key_is_accepted() {
        for source in ["theme = 3024 Day", "theme = \"3024 Day\""] {
            let (overrides, diagnostics) = parse_overrides(test_path(), source);

            assert!(diagnostics.is_empty());
            assert_eq!(
                overrides,
                ConfigOverrides {
                    cols: None,
                    rows: None,
                    font_size: None,
                    theme: Some("3024 Day".to_string()),
                }
            );
        }
    }

    #[test]
    fn invalid_file_value_warns_and_uses_default() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "window-width = abc\nwindow-height = 30");

        assert_eq!(overrides.cols, None);
        assert_eq!(overrides.rows, None);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("window-width"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("abc"))
        );
    }

    #[test]
    fn invalid_type_warns_and_uses_default() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "font-size = large");

        assert_eq!(overrides.font_size, None);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("font-size"));
        assert!(diagnostics[0].message.contains("large"));
    }

    #[test]
    fn unknown_key_warns_and_parsing_continues() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "bogus-key = x\nfont-size = 15");

        assert_eq!(overrides.font_size, Some(15.0));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("bogus-key"));
    }

    #[test]
    fn light_dark_syntax_is_rejected() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "theme = light:Foo,dark:Bar");

        assert_eq!(overrides.theme, None);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("light:"));
        assert!(message.contains("dark:"));
        assert!(message.contains("not supported"));
        assert!(message.contains("single theme name"));
    }

    #[test]
    fn invalid_file_values_are_non_fatal() {
        for (source, key) in [
            ("font-size = -1.0", "font-size"),
            ("font-size = inf", "font-size"),
            ("window-height = abc", "window-height"),
        ] {
            let (_, diagnostics) = parse_overrides(test_path(), source);

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(key)),
                "{source:?} should produce {key} diagnostic: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn default_and_legacy_paths_are_hermetic() {
        let base = Path::new("/tmp/noa-config-root");

        assert_eq!(
            default_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config")
        );
        assert_eq!(
            legacy_toml_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config.toml")
        );
    }

    #[test]
    fn load_startup_config_from_preserves_precedence_and_diagnostics() {
        let dir = unique_temp_dir("precedence");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &config_path,
            "bogus-key = x\nfont-size = bad\nfont-size = 16",
        )
        .unwrap();
        let cli = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(18.0),
            theme: None,
        };

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, cli).unwrap();

        assert_eq!(config.font_size, 18.0);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].message.contains("bogus-key"));
        assert!(diagnostics[1].message.contains("font-size"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_startup_config_from_uses_defaults_when_files_are_absent() {
        let dir = unique_temp_dir("defaults");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cli_cols_remain_independent_of_config_pair_rule() {
        let dir = unique_temp_dir("cli-cols");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let cli = ConfigOverrides {
            cols: Some(50),
            rows: None,
            font_size: None,
            theme: None,
        };

        let (config, diagnostics) = load_startup_config_from(&config_path, &legacy_path, cli)
            .expect("CLI-only config is valid");

        assert_eq!(config.cols, 50);
        assert_eq!(config.rows, DEFAULT_ROWS);
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_toml_config_warns_without_being_read() {
        let dir = unique_temp_dir("legacy");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_toml_config_warns_even_when_new_config_exists() {
        let dir = unique_temp_dir("legacy-and-new");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config_path, "font-size = 16").unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config.font_size, 16.0);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_structs_do_not_carry_diagnostics() {
        let StartupConfig {
            cols,
            rows,
            font_size,
            theme,
        } = StartupConfig::default();
        let ConfigOverrides {
            cols: override_cols,
            rows: override_rows,
            font_size: override_font_size,
            theme: override_theme,
        } = ConfigOverrides::default();

        assert_eq!((cols, rows, font_size, theme), (80, 24, 14.0, None));
        assert_eq!(
            (
                override_cols,
                override_rows,
                override_font_size,
                override_theme
            ),
            (None, None, None, None)
        );
    }

    #[test]
    fn validates_cli_grid_values_after_merge() {
        let error = validate_startup_config(
            &StartupConfig {
                cols: 0,
                rows: 24,
                font_size: 14.0,
                theme: None,
            },
            "resolved startup config",
        )
        .unwrap_err();

        assert!(error.to_string().contains("cols"));
    }

    #[test]
    fn validates_cli_font_size_after_merge() {
        let config = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(f32::NAN),
            theme: None,
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("font_size"));
    }
}
