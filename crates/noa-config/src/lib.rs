//! Startup configuration discovery, parsing, validation, and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use noa_core::Rgb;

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

/// `clipboard-read` policy for OSC 52 clipboard *read* (query) requests.
/// Mirrors Ghostty, whose default is `ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardAccess {
    /// Never honor a read request.
    Deny,
    /// Prompt the user before revealing clipboard contents.
    #[default]
    Ask,
    /// Always honor a read request.
    Allow,
}

/// A single OpenType feature toggle, e.g. `calt` (enabled) or `-liga`
/// (`enabled: false`, explicitly disabled). Consumed for real in WP2; WP0
/// only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub enabled: bool,
}

/// A single variable-font axis coordinate, e.g. `wght=700`. Consumed for
/// real in WP2; WP0 only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f32,
}

/// `font-synthetic-style` mode: whether faux-bold/faux-italic synthesis is
/// enabled, and whether either style is individually disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticStyleMode {
    Both,
    Neither,
    NoBold,
    NoItalic,
}

/// `cursor-style` shape. Ghostty also has `block_hollow`, which noa does not
/// render yet (the parser emits a diagnostic and ignores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// `alpha-blending` mode. `Native` is a real value; `Linear` /
/// `LinearCorrected` are parsed-but-fallback (REQ-CFG-4) — `noa-config`
/// emits a diagnostic and the renderer falls back to `Native` (WP3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaBlendingMode {
    Native,
    Linear,
    LinearCorrected,
}

/// Font configuration parsed from `font-*` / `alpha-blending` directives.
///
/// This is a `noa-config`-local type, distinct from `noa_font::FontConfig`
/// (ADR-R1): `noa-config` must not depend on `noa-font`/swash/font-kit, so
/// the two crates' `FontConfig` types stay separate. The `noa-app` layer
/// maps this type to `noa_font::FontConfig` before calling `FontGrid::new`.
///
/// Repeatable keys (`font-family*`, `font-feature`, `font-variation*`)
/// accumulate into `Vec`s across directives in one source (parser.rs); a
/// higher-priority source (CLI over file) replaces a base source's list
/// wholesale rather than concatenating, mirroring this file's scalar
/// last-wins semantics. Scalar keys (`font-synthetic-style`,
/// `alpha-blending`, `font-thicken`, `font-thicken-strength`) are
/// straightforward last-wins `Option`s.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FontConfig {
    pub families: Vec<String>,
    pub families_bold: Vec<String>,
    pub families_italic: Vec<String>,
    pub families_bold_italic: Vec<String>,
    pub features: Vec<FontFeature>,
    pub variations: Vec<FontVariation>,
    pub variations_bold: Vec<FontVariation>,
    pub variations_italic: Vec<FontVariation>,
    pub variations_bold_italic: Vec<FontVariation>,
    pub synthetic_style: Option<SyntheticStyleMode>,
    pub alpha_blending: Option<AlphaBlendingMode>,
    pub thicken: Option<bool>,
    pub thicken_strength: Option<u8>,
}

impl FontConfig {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            families: merge_list(self.families, higher_priority.families),
            families_bold: merge_list(self.families_bold, higher_priority.families_bold),
            families_italic: merge_list(self.families_italic, higher_priority.families_italic),
            families_bold_italic: merge_list(
                self.families_bold_italic,
                higher_priority.families_bold_italic,
            ),
            features: merge_list(self.features, higher_priority.features),
            variations: merge_list(self.variations, higher_priority.variations),
            variations_bold: merge_list(self.variations_bold, higher_priority.variations_bold),
            variations_italic: merge_list(
                self.variations_italic,
                higher_priority.variations_italic,
            ),
            variations_bold_italic: merge_list(
                self.variations_bold_italic,
                higher_priority.variations_bold_italic,
            ),
            synthetic_style: higher_priority.synthetic_style.or(self.synthetic_style),
            alpha_blending: higher_priority.alpha_blending.or(self.alpha_blending),
            thicken: higher_priority.thicken.or(self.thicken),
            thicken_strength: higher_priority.thicken_strength.or(self.thicken_strength),
        }
    }

    pub fn apply_to(self, base: Self) -> Self {
        // `apply_to` composes the same way `merge` does: `self` (the
        // override) wins over `base` (the resolved default).
        base.merge(self)
    }
}

fn merge_list<T>(base: Vec<T>, higher_priority: Vec<T>) -> Vec<T> {
    if higher_priority.is_empty() {
        base
    } else {
        higher_priority
    }
}

/// Resolved, validated startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
    pub font: FontConfig,
    /// OSC 52 clipboard read (query) policy.
    pub clipboard_read: ClipboardAccess,
    /// Whether to confirm before pasting content that could run commands
    /// (`clipboard-paste-protection`). Ghostty default is on.
    pub clipboard_paste_protection: bool,
    /// `window-padding-x`: horizontal padding (left = right) in physical
    /// pixels. `None` keeps the built-in default for that axis; the concrete
    /// `GridPadding` is derived in `noa-app`.
    pub window_padding_x: Option<f32>,
    /// `window-padding-y`: vertical padding (top = bottom) in physical pixels.
    pub window_padding_y: Option<f32>,
    /// `background` / `foreground`: theme default color overrides. `None`
    /// keeps the resolved theme's value.
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    /// `cursor-color`: theme cursor color override.
    pub cursor_color: Option<Rgb>,
    /// `selection-foreground` / `selection-background`: theme selection color
    /// overrides.
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    /// `cursor-style` shape and `cursor-style-blink` toggle. `None` keeps the
    /// terminal default (Ghostty: blinking block).
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    /// `background-opacity`: 0.0..=1.0, clamped. Consumed by the transparency
    /// follow-up; plumbed through for now. Default is fully opaque.
    pub background_opacity: f32,
    /// `background-blur-radius`: native macOS window background blur radius in
    /// points, `0..=64` (0 = no blur). Only visible with `background_opacity`
    /// below 1.0. No-op on non-macOS.
    pub background_blur_radius: u16,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            font_size: DEFAULT_FONT_SIZE,
            theme: None,
            font: FontConfig::default(),
            clipboard_read: ClipboardAccess::default(),
            clipboard_paste_protection: true,
            window_padding_x: None,
            window_padding_y: None,
            background: None,
            foreground: None,
            cursor_color: None,
            selection_foreground: None,
            selection_background: None,
            cursor_style: None,
            cursor_style_blink: None,
            background_opacity: 1.0,
            background_blur_radius: 0,
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
    pub font: FontConfig,
    pub clipboard_read: Option<ClipboardAccess>,
    pub clipboard_paste_protection: Option<bool>,
    pub window_padding_x: Option<f32>,
    pub window_padding_y: Option<f32>,
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor_color: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    pub background_opacity: Option<f32>,
    pub background_blur_radius: Option<u16>,
}

impl ConfigOverrides {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            cols: higher_priority.cols.or(self.cols),
            rows: higher_priority.rows.or(self.rows),
            font_size: higher_priority.font_size.or(self.font_size),
            theme: higher_priority.theme.or(self.theme),
            font: self.font.merge(higher_priority.font),
            clipboard_read: higher_priority.clipboard_read.or(self.clipboard_read),
            clipboard_paste_protection: higher_priority
                .clipboard_paste_protection
                .or(self.clipboard_paste_protection),
            window_padding_x: higher_priority.window_padding_x.or(self.window_padding_x),
            window_padding_y: higher_priority.window_padding_y.or(self.window_padding_y),
            background: higher_priority.background.or(self.background),
            foreground: higher_priority.foreground.or(self.foreground),
            cursor_color: higher_priority.cursor_color.or(self.cursor_color),
            selection_foreground: higher_priority
                .selection_foreground
                .or(self.selection_foreground),
            selection_background: higher_priority
                .selection_background
                .or(self.selection_background),
            cursor_style: higher_priority.cursor_style.or(self.cursor_style),
            cursor_style_blink: higher_priority
                .cursor_style_blink
                .or(self.cursor_style_blink),
            background_opacity: higher_priority
                .background_opacity
                .or(self.background_opacity),
            background_blur_radius: higher_priority
                .background_blur_radius
                .or(self.background_blur_radius),
        }
    }

    pub fn apply_to(self, base: StartupConfig) -> StartupConfig {
        StartupConfig {
            cols: self.cols.unwrap_or(base.cols),
            rows: self.rows.unwrap_or(base.rows),
            font_size: self.font_size.unwrap_or(base.font_size),
            theme: self.theme.or(base.theme),
            font: self.font.apply_to(base.font),
            clipboard_read: self.clipboard_read.unwrap_or(base.clipboard_read),
            clipboard_paste_protection: self
                .clipboard_paste_protection
                .unwrap_or(base.clipboard_paste_protection),
            window_padding_x: self.window_padding_x.or(base.window_padding_x),
            window_padding_y: self.window_padding_y.or(base.window_padding_y),
            background: self.background.or(base.background),
            foreground: self.foreground.or(base.foreground),
            cursor_color: self.cursor_color.or(base.cursor_color),
            selection_foreground: self.selection_foreground.or(base.selection_foreground),
            selection_background: self.selection_background.or(base.selection_background),
            cursor_style: self.cursor_style.or(base.cursor_style),
            cursor_style_blink: self.cursor_style_blink.or(base.cursor_style_blink),
            background_opacity: self.background_opacity.unwrap_or(base.background_opacity),
            background_blur_radius: self
                .background_blur_radius
                .unwrap_or(base.background_blur_radius),
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
                font: FontConfig::default(),
                clipboard_read: ClipboardAccess::Ask,
                clipboard_paste_protection: true,
                window_padding_x: None,
                window_padding_y: None,
                background: None,
                foreground: None,
                cursor_color: None,
                selection_foreground: None,
                selection_background: None,
                cursor_style: None,
                cursor_style_blink: None,
                background_opacity: 1.0,
                background_blur_radius: 0,
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
                font: FontConfig::default(),
                ..Default::default()
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
            font: FontConfig::default(),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            cols: Some(120),
            rows: None,
            font_size: Some(16.0),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(
            config,
            StartupConfig {
                cols: 120,
                rows: 30,
                font_size: 16.0,
                theme: Some("3024 Day".to_string()),
                font: FontConfig::default(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn appearance_keys_flow_through_parse_and_apply() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "window-padding-x = 8\n\
             window-padding-y = 4\n\
             background = #101010\n\
             cursor-style = bar\n\
             cursor-style-blink = false\n\
             background-opacity = 0.8",
        );
        assert!(diagnostics.is_empty());

        let config = overrides.apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(8.0));
        assert_eq!(config.window_padding_y, Some(4.0));
        assert_eq!(config.background, Some(Rgb::new(0x10, 0x10, 0x10)));
        assert_eq!(config.cursor_style, Some(CursorShape::Bar));
        assert_eq!(config.cursor_style_blink, Some(false));
        assert_eq!(config.background_opacity, 0.8);
    }

    #[test]
    fn cli_overrides_win_for_appearance_keys() {
        let file = ConfigOverrides {
            window_padding_x: Some(2.0),
            background_opacity: Some(0.5),
            cursor_style: Some(CursorShape::Block),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            window_padding_x: Some(9.0),
            background_opacity: Some(0.9),
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(9.0));
        assert_eq!(config.background_opacity, 0.9);
        // Not overridden by CLI: the file value survives.
        assert_eq!(config.cursor_style, Some(CursorShape::Block));
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
                    font: FontConfig::default(),
                    ..Default::default()
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
            font: FontConfig::default(),
            ..Default::default()
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
            font: FontConfig::default(),
            ..Default::default()
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
            font,
            ..
        } = StartupConfig::default();
        let ConfigOverrides {
            cols: override_cols,
            rows: override_rows,
            font_size: override_font_size,
            theme: override_theme,
            font: override_font,
            ..
        } = ConfigOverrides::default();

        assert_eq!((cols, rows, font_size, theme), (80, 24, 14.0, None));
        assert_eq!(font, FontConfig::default());
        assert_eq!(
            (
                override_cols,
                override_rows,
                override_font_size,
                override_theme
            ),
            (None, None, None, None)
        );
        assert_eq!(override_font, FontConfig::default());
    }

    #[test]
    fn validates_cli_grid_values_after_merge() {
        let error = validate_startup_config(
            &StartupConfig {
                cols: 0,
                rows: 24,
                font_size: 14.0,
                theme: None,
                font: FontConfig::default(),
                ..Default::default()
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
            font: FontConfig::default(),
            ..Default::default()
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("font_size"));
    }
}
