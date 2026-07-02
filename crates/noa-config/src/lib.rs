//! Startup configuration discovery, parsing, validation, and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use toml_edit::{DocumentMut, Item};

pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

const SUPPORTED_KEYS: &[&str] = &["cols", "rows", "font_size"];

/// Resolved, validated startup settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StartupConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

/// Optional values from a config file or explicit CLI flags.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConfigOverrides {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub font_size: Option<f32>,
}

impl ConfigOverrides {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            cols: higher_priority.cols.or(self.cols),
            rows: higher_priority.rows.or(self.rows),
            font_size: higher_priority.font_size.or(self.font_size),
        }
    }

    pub fn apply_to(self, base: StartupConfig) -> StartupConfig {
        StartupConfig {
            cols: self.cols.unwrap_or(base.cols),
            rows: self.rows.unwrap_or(base.rows),
            font_size: self.font_size.unwrap_or(base.font_size),
        }
    }
}

pub fn load_startup_config(cli: ConfigOverrides) -> anyhow::Result<StartupConfig> {
    let config = load_file_overrides()?
        .merge(cli)
        .apply_to(StartupConfig::default());
    validate_startup_config(config, "resolved startup config")?;
    Ok(config)
}

pub fn load_file_overrides() -> anyhow::Result<ConfigOverrides> {
    let Some(path) = default_config_path() else {
        return Ok(ConfigOverrides::default());
    };
    if !path.exists() {
        return Ok(ConfigOverrides::default());
    }
    load_overrides_from_path(&path)
}

pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| path.join("noa").join("config.toml"))
}

pub fn find_first_existing_config_path<I, P>(candidates: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    candidates
        .into_iter()
        .map(|path| path.as_ref().to_path_buf())
        .find(|path| path.exists())
}

pub fn load_overrides_from_path(path: &Path) -> anyhow::Result<ConfigOverrides> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_overrides(path, &source)
}

pub fn parse_overrides(path: &Path, source: &str) -> anyhow::Result<ConfigOverrides> {
    let document = source
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    reject_unknown_keys(path, &document)?;

    Ok(ConfigOverrides {
        cols: parse_u16(path, &document, "cols")?,
        rows: parse_u16(path, &document, "rows")?,
        font_size: parse_font_size(path, &document)?,
    })
}

pub fn validate_startup_config(config: StartupConfig, context: &str) -> anyhow::Result<()> {
    validate_grid_dimension(config.cols, context, "cols")?;
    validate_grid_dimension(config.rows, context, "rows")?;
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        bail!("invalid {context}: `font_size` must be a positive finite number");
    }
    Ok(())
}

fn reject_unknown_keys(path: &Path, document: &DocumentMut) -> anyhow::Result<()> {
    for (key, _) in document.iter() {
        if !SUPPORTED_KEYS.contains(&key) {
            bail!(
                "invalid config file {}: unsupported key `{key}`; supported keys are {}",
                path.display(),
                SUPPORTED_KEYS.join(", ")
            );
        }
    }
    Ok(())
}

fn parse_u16(
    path: &Path,
    document: &DocumentMut,
    key: &'static str,
) -> anyhow::Result<Option<u16>> {
    let Some(item) = document.get(key) else {
        return Ok(None);
    };
    let value = item
        .as_integer()
        .ok_or_else(|| invalid_type(path, key, item))?;
    if !(1..=i64::from(u16::MAX)).contains(&value) {
        bail!(
            "invalid config value in {}: `{key}` must be an integer between 1 and {}",
            path.display(),
            u16::MAX
        );
    }
    Ok(Some(value as u16))
}

fn parse_font_size(path: &Path, document: &DocumentMut) -> anyhow::Result<Option<f32>> {
    let key = "font_size";
    let Some(item) = document.get(key) else {
        return Ok(None);
    };
    let value = item
        .as_float()
        .or_else(|| item.as_integer().map(|value| value as f64))
        .ok_or_else(|| invalid_type(path, key, item))?;
    if !value.is_finite() || value <= 0.0 || value > f64::from(f32::MAX) {
        bail!(
            "invalid config value in {}: `{key}` must be a positive finite number",
            path.display()
        );
    }
    Ok(Some(value as f32))
}

fn validate_grid_dimension(value: u16, context: &str, key: &'static str) -> anyhow::Result<()> {
    if value == 0 {
        bail!(
            "invalid {context}: `{key}` must be an integer between 1 and {}",
            u16::MAX
        );
    }
    Ok(())
}

fn invalid_type(path: &Path, key: &'static str, item: &Item) -> anyhow::Error {
    anyhow::anyhow!(
        "invalid config value in {}: `{key}` has unsupported type `{}`",
        path.display(),
        item.type_name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> &'static Path {
        Path::new("/tmp/noa-test-config.toml")
    }

    #[test]
    fn defaults_match_existing_startup_behavior() {
        assert_eq!(
            StartupConfig::default(),
            StartupConfig {
                cols: 80,
                rows: 24,
                font_size: 14.0,
            }
        );
    }

    #[test]
    fn parses_supported_config_keys() {
        let overrides = parse_overrides(
            test_path(),
            r#"
cols = 100
rows = 30
font_size = 15.5
"#,
        )
        .unwrap();

        assert_eq!(
            overrides,
            ConfigOverrides {
                cols: Some(100),
                rows: Some(30),
                font_size: Some(15.5),
            }
        );
    }

    #[test]
    fn cli_overrides_config_file_values() {
        let file = ConfigOverrides {
            cols: Some(100),
            rows: Some(30),
            font_size: Some(15.5),
        };
        let cli = ConfigOverrides {
            cols: Some(120),
            rows: None,
            font_size: Some(16.0),
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(
            config,
            StartupConfig {
                cols: 120,
                rows: 30,
                font_size: 16.0,
            }
        );
    }

    #[test]
    fn finds_first_existing_config_candidate() {
        let dir = std::env::temp_dir().join(format!("noa-config-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.toml");
        let existing = dir.join("config.toml");
        fs::write(&existing, "").unwrap();

        let found = find_first_existing_config_path([&missing, &existing]);

        assert_eq!(found, Some(existing));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_file_value_includes_path_and_key() {
        let error = parse_overrides(test_path(), "cols = 0").unwrap_err();
        let message = error.to_string();

        assert!(message.contains("/tmp/noa-test-config.toml"));
        assert!(message.contains("cols"));
    }

    #[test]
    fn invalid_type_includes_path_and_key() {
        let error = parse_overrides(test_path(), "font_size = \"large\"").unwrap_err();
        let message = error.to_string();

        assert!(message.contains("/tmp/noa-test-config.toml"));
        assert!(message.contains("font_size"));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let error = parse_overrides(test_path(), "theme = \"Builtin Dark\"").unwrap_err();
        let message = error.to_string();

        assert!(message.contains("/tmp/noa-test-config.toml"));
        assert!(message.contains("theme"));
        assert!(message.contains("supported keys"));
    }

    #[test]
    fn invalid_file_values_are_rejected() {
        for (source, key) in [
            ("rows = 0", "rows"),
            ("font_size = -1.0", "font_size"),
            ("font_size = inf", "font_size"),
        ] {
            let error = parse_overrides(test_path(), source).unwrap_err();
            let message = error.to_string();

            assert!(message.contains("/tmp/noa-test-config.toml"));
            assert!(message.contains(key));
        }
    }

    #[test]
    fn validates_cli_grid_values_after_merge() {
        let error = validate_startup_config(
            StartupConfig {
                cols: 0,
                rows: 24,
                font_size: 14.0,
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
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("font_size"));
    }
}
