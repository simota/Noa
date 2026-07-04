//! `+action` CLI mode, mirroring `ghostty +<action>`: `noa +list-themes`
//! runs a one-shot query, prints to stdout, and exits without starting the
//! GUI event loop.
//!
//! Argv classification is the pure [`parse_invocation`] function and each
//! action's output is a pure `-> String` builder, so the binary stays a thin
//! dispatcher and every output shape is unit-testable without spawning a
//! process (or a window).

use noa_config::{
    AlphaBlendingMode, ClipboardAccess, CursorShape, MacosOptionAsAlt, MacosTitlebarStyle,
    StartupConfig, SyntheticStyleMode,
};
use noa_core::Rgb;

use crate::commands::KeybindEngine;

/// One-shot CLI actions (`noa +<action>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAction {
    Version,
    ListThemes,
    ListKeybinds,
    ListFonts,
    ShowConfig,
    ListActions,
    Help,
}

impl CliAction {
    /// Every action, in the order `+list-actions` presents them.
    pub const ALL: [CliAction; 7] = [
        CliAction::Version,
        CliAction::ListThemes,
        CliAction::ListKeybinds,
        CliAction::ListFonts,
        CliAction::ShowConfig,
        CliAction::ListActions,
        CliAction::Help,
    ];

    /// The action name as typed after `+` on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::ListThemes => "list-themes",
            Self::ListKeybinds => "list-keybinds",
            Self::ListFonts => "list-fonts",
            Self::ShowConfig => "show-config",
            Self::ListActions => "list-actions",
            Self::Help => "help",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Version => "Show version and build information.",
            Self::ListThemes => "List bundled themes (annotated light/dark).",
            Self::ListKeybinds => "List the effective keybindings (chord = action).",
            Self::ListFonts => "List system font families.",
            Self::ShowConfig => "Show the resolved effective configuration.",
            Self::ListActions => "List available +actions.",
            Self::Help => "Alias for +list-actions.",
        }
    }
}

/// How the process was invoked, decided purely from argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// No `+action` argument: launch the GUI (clap parses the flags).
    Gui,
    /// `argv[1]` named a known `+action`.
    Action(CliAction),
    /// `argv[1]` started with `+` but named no known action (the payload is
    /// the name without the `+`).
    Unknown(String),
}

/// Classify raw argv (`args[0]` is the program name). Only `argv[1]` can
/// select an action, matching Ghostty's `ghostty +<action>` shape; anything
/// else falls through to the GUI flag parser.
pub fn parse_invocation<S: AsRef<str>>(args: &[S]) -> Invocation {
    let Some(first) = args.get(1).map(AsRef::as_ref) else {
        return Invocation::Gui;
    };
    let Some(name) = first.strip_prefix('+') else {
        return Invocation::Gui;
    };
    match CliAction::from_name(name) {
        Some(action) => Invocation::Action(action),
        None => Invocation::Unknown(name.to_string()),
    }
}

/// Execute `action`, writing its report to stdout (and config diagnostics to
/// stderr). Returns once the output is printed; the caller exits.
pub fn run_action(action: CliAction) -> anyhow::Result<()> {
    match action {
        CliAction::Version => print!("{}", version_output()),
        CliAction::ListThemes => print!("{}", list_themes_output()),
        CliAction::ListKeybinds => print!("{}", list_keybinds_output()),
        CliAction::ListFonts => {
            let families = noa_font::list_families()?;
            print!("{}", list_fonts_output(&families));
        }
        CliAction::ShowConfig => {
            let (config, diagnostics) =
                noa_config::load_startup_config(noa_config::ConfigOverrides::default())?;
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.message);
            }
            print!("{}", show_config_output(&config));
        }
        CliAction::ListActions | CliAction::Help => print!("{}", list_actions_output()),
    }
    Ok(())
}

/// The stderr report for an unrecognized `+action` (ends with a newline; the
/// caller prints it verbatim and exits with status 1).
pub fn unknown_action_message(name: &str) -> String {
    format!("noa: unknown action: +{name}\n\n{}", list_actions_output())
}

fn version_output() -> String {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    format!(
        "noa {}\nA faithful Rust clone of the Ghostty terminal emulator.\nbuild: {}-{} ({profile})\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS,
    )
}

/// One theme per line, in catalog order (the generated catalog is sorted by
/// name), annotated with a light/dark classification derived from the
/// theme's default background luminance.
fn list_themes_output() -> String {
    noa_theme::THEMES
        .iter()
        .map(|(name, theme)| format!("{name} ({})\n", theme_variant(theme)))
        .collect()
}

fn theme_variant(theme: &noa_theme::ThemeDef) -> &'static str {
    // Rec. 709 relative luminance of the default background: bright
    // backgrounds read as light themes, dark backgrounds as dark themes.
    let bg = theme.default_bg;
    let luma = 0.2126 * f32::from(bg.r) + 0.7152 * f32::from(bg.g) + 0.0722 * f32::from(bg.b);
    if luma >= 128.0 { "light" } else { "dark" }
}

/// One `chord = action` line per binding. noa's config format has no
/// `keybind` key yet (noa-config diagnoses it as unsupported), so the
/// default engine *is* the effective binding set.
fn list_keybinds_output() -> String {
    KeybindEngine::default()
        .list()
        .into_iter()
        .map(|(chord, action)| format!("{chord} = {action}\n"))
        .collect()
}

fn list_fonts_output(families: &[String]) -> String {
    families
        .iter()
        .map(|family| format!("{family}\n"))
        .collect()
}

/// The resolved effective configuration as `key = value` lines using the
/// config-file key names. Every key is printed (unset optionals render an
/// empty value, like Ghostty's `+show-config`); repeatable keys print one
/// line per entry, or a single empty line when the list is empty.
fn show_config_output(config: &StartupConfig) -> String {
    let mut out = String::new();
    push_line(&mut out, "window-width", &config.cols.to_string());
    push_line(&mut out, "window-height", &config.rows.to_string());
    push_line(&mut out, "font-size", &config.font_size.to_string());
    push_line(&mut out, "theme", config.theme.as_deref().unwrap_or(""));

    push_family_lines(&mut out, "font-family", &config.font.families);
    push_family_lines(&mut out, "font-family-bold", &config.font.families_bold);
    push_family_lines(&mut out, "font-family-italic", &config.font.families_italic);
    push_family_lines(
        &mut out,
        "font-family-bold-italic",
        &config.font.families_bold_italic,
    );
    push_repeatable_lines(
        &mut out,
        "font-feature",
        config.font.features.iter().map(|feature| {
            let sign = if feature.enabled { "" } else { "-" };
            format!("{sign}{}", tag_str(feature.tag))
        }),
    );
    for (key, variations) in [
        ("font-variation", &config.font.variations),
        ("font-variation-bold", &config.font.variations_bold),
        ("font-variation-italic", &config.font.variations_italic),
        (
            "font-variation-bold-italic",
            &config.font.variations_bold_italic,
        ),
    ] {
        push_repeatable_lines(
            &mut out,
            key,
            variations
                .iter()
                .map(|variation| format!("{}={}", tag_str(variation.tag), variation.value)),
        );
    }
    push_line(
        &mut out,
        "font-synthetic-style",
        match config.font.synthetic_style {
            None => "",
            Some(SyntheticStyleMode::Both) => "true",
            Some(SyntheticStyleMode::Neither) => "false",
            Some(SyntheticStyleMode::NoBold) => "no-bold",
            Some(SyntheticStyleMode::NoItalic) => "no-italic",
        },
    );
    push_line(
        &mut out,
        "alpha-blending",
        match config.font.alpha_blending {
            None => "",
            Some(AlphaBlendingMode::Native) => "native",
            Some(AlphaBlendingMode::Linear) => "linear",
            Some(AlphaBlendingMode::LinearCorrected) => "linear-corrected",
        },
    );
    push_optional_line(&mut out, "font-thicken", config.font.thicken);
    push_optional_line(
        &mut out,
        "font-thicken-strength",
        config.font.thicken_strength,
    );

    push_line(
        &mut out,
        "clipboard-read",
        match config.clipboard_read {
            ClipboardAccess::Deny => "deny",
            ClipboardAccess::Ask => "ask",
            ClipboardAccess::Allow => "allow",
        },
    );
    push_line(
        &mut out,
        "clipboard-paste-protection",
        &config.clipboard_paste_protection.to_string(),
    );
    push_optional_line(&mut out, "window-padding-x", config.window_padding_x);
    push_optional_line(&mut out, "window-padding-y", config.window_padding_y);
    push_color_line(&mut out, "background", config.background);
    push_color_line(&mut out, "foreground", config.foreground);
    push_color_line(&mut out, "cursor-color", config.cursor_color);
    push_color_line(
        &mut out,
        "selection-foreground",
        config.selection_foreground,
    );
    push_color_line(
        &mut out,
        "selection-background",
        config.selection_background,
    );
    push_line(
        &mut out,
        "minimum-contrast",
        &config.minimum_contrast.to_string(),
    );
    push_line(
        &mut out,
        "cursor-style",
        match config.cursor_style {
            None => "",
            Some(CursorShape::Block) => "block",
            Some(CursorShape::Bar) => "bar",
            Some(CursorShape::Underline) => "underline",
        },
    );
    push_optional_line(&mut out, "cursor-style-blink", config.cursor_style_blink);
    push_line(
        &mut out,
        "background-opacity",
        &config.background_opacity.to_string(),
    );
    push_line(
        &mut out,
        "background-blur-radius",
        &config.background_blur_radius.to_string(),
    );
    push_line(
        &mut out,
        "scrollback-limit",
        &config.scrollback_limit.to_string(),
    );
    push_line(
        &mut out,
        "window-save-state",
        match config.window_save_state {
            noa_config::WindowSaveState::Default => "default",
            noa_config::WindowSaveState::Never => "never",
            noa_config::WindowSaveState::Always => "always",
        },
    );
    push_line(
        &mut out,
        "macos-option-as-alt",
        match config.macos_option_as_alt {
            MacosOptionAsAlt::None => "false",
            MacosOptionAsAlt::Left => "left",
            MacosOptionAsAlt::Right => "right",
            MacosOptionAsAlt::Both => "true",
        },
    );
    push_line(
        &mut out,
        "macos-titlebar-style",
        match config.macos_titlebar_style {
            MacosTitlebarStyle::Native => "native",
            MacosTitlebarStyle::Transparent => "transparent",
            MacosTitlebarStyle::Hidden => "hidden",
        },
    );
    push_line(
        &mut out,
        "quick-terminal-hotkey",
        config.quick_terminal_hotkey.as_deref().unwrap_or(""),
    );
    push_line(
        &mut out,
        "quick-terminal-size",
        &config.quick_terminal_size.to_string(),
    );
    push_line(
        &mut out,
        "quick-terminal-autohide",
        &config.quick_terminal_autohide.to_string(),
    );
    out
}

fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push('\n');
}

fn push_optional_line<T: std::fmt::Display>(out: &mut String, key: &str, value: Option<T>) {
    match value {
        Some(value) => push_line(out, key, &value.to_string()),
        None => push_line(out, key, ""),
    }
}

fn push_color_line(out: &mut String, key: &str, color: Option<Rgb>) {
    match color {
        Some(color) => push_line(
            out,
            key,
            &format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b),
        ),
        None => push_line(out, key, ""),
    }
}

fn push_family_lines(out: &mut String, key: &str, families: &[String]) {
    push_repeatable_lines(out, key, families.iter().cloned());
}

fn push_repeatable_lines(
    out: &mut String,
    key: &str,
    values: impl ExactSizeIterator<Item = String>,
) {
    if values.len() == 0 {
        push_line(out, key, "");
        return;
    }
    for value in values {
        push_line(out, key, &value);
    }
}

/// OpenType tags are 4 ASCII bytes by construction (the config parser
/// validates them), so lossy conversion never actually loses anything.
fn tag_str(tag: [u8; 4]) -> String {
    String::from_utf8_lossy(&tag).into_owned()
}

fn list_actions_output() -> String {
    let mut out = String::from("Available actions:\n");
    for action in CliAction::ALL {
        out.push_str(&format!("  +{:<13}  {}\n", action.name(), action.summary()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::AppCommand;

    #[test]
    fn plus_argv1_selects_each_known_action() {
        for action in CliAction::ALL {
            let argv1 = format!("+{}", action.name());
            assert_eq!(
                parse_invocation(&["noa", argv1.as_str()]),
                Invocation::Action(action),
                "{argv1} should parse"
            );
        }
    }

    #[test]
    fn non_plus_argv_launches_the_gui() {
        assert_eq!(parse_invocation(&["noa"]), Invocation::Gui);
        assert_eq!(
            parse_invocation(&["noa", "--cols", "100", "--rows", "30"]),
            Invocation::Gui
        );
        assert_eq!(
            parse_invocation(&["noa", "--import-ghostty-config"]),
            Invocation::Gui
        );
        // A `+` later in argv is not an action selector.
        assert_eq!(parse_invocation(&["noa", "--cols", "+1"]), Invocation::Gui);
    }

    #[test]
    fn unknown_plus_action_is_reported_by_name() {
        assert_eq!(
            parse_invocation(&["noa", "+bogus"]),
            Invocation::Unknown("bogus".to_string())
        );
        assert_eq!(
            parse_invocation(&["noa", "+"]),
            Invocation::Unknown(String::new())
        );
    }

    #[test]
    fn action_names_round_trip() {
        for action in CliAction::ALL {
            assert_eq!(CliAction::from_name(action.name()), Some(action));
        }
        assert_eq!(CliAction::from_name("bogus"), None);
    }

    #[test]
    fn version_output_names_the_binary_and_version() {
        let output = version_output();

        assert!(output.starts_with(&format!("noa {}\n", env!("CARGO_PKG_VERSION"))));
        assert!(output.contains("build: "));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn list_themes_output_is_sorted_annotated_and_complete() {
        let output = list_themes_output();
        let lines: Vec<&str> = output.lines().collect();

        assert_eq!(lines.len(), noa_theme::THEMES.len());
        assert!(
            lines
                .iter()
                .all(|line| line.ends_with(" (light)") || line.ends_with(" (dark)")),
            "every theme line must carry a light/dark annotation"
        );
        let names: Vec<&str> = lines
            .iter()
            .map(|line| line.rsplit_once(" (").expect("annotated line").0)
            .collect();
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "theme names must be strictly sorted"
        );
        assert!(output.contains("3024 Day (light)"));
        assert!(output.contains("3024 Night (dark)"));
    }

    #[test]
    fn list_keybinds_output_matches_the_default_engine() {
        let output = list_keybinds_output();

        assert!(!output.is_empty());
        assert!(output.contains("cmd+t = tab.new\n"));
        assert!(output.contains("cmd+ctrl+arrowleft = split.focus-left\n"));
        assert!(output.contains("cmd+ctrl+shift+arrowright = split.resize-right\n"));
        assert!(output.contains("cmd+shift+p = command-palette.toggle\n"));
        // Every line is `chord = action` where the action name round-trips
        // through the command registry.
        for line in output.lines() {
            let (_, action) = line.split_once(" = ").expect("chord = action shape");
            assert!(
                AppCommand::from_action_name(action).is_some(),
                "{action} must be a registered action name"
            );
        }
        assert_eq!(
            output.lines().count(),
            KeybindEngine::default().list().len()
        );
    }

    #[test]
    fn list_fonts_output_is_one_family_per_line() {
        let families = vec!["Menlo".to_string(), "Monaco".to_string()];

        assert_eq!(list_fonts_output(&families), "Menlo\nMonaco\n");
        assert_eq!(list_fonts_output(&[]), "");
    }

    #[test]
    fn show_config_output_renders_defaults_as_key_value_lines() {
        let output = show_config_output(&StartupConfig::default());

        assert!(output.contains("window-width = 80\n"));
        assert!(output.contains("window-height = 24\n"));
        assert!(output.contains("font-size = 14\n"));
        assert!(output.contains("theme = \n"));
        assert!(output.contains("font-family = \n"));
        assert!(output.contains("clipboard-read = ask\n"));
        assert!(output.contains("clipboard-paste-protection = true\n"));
        assert!(output.contains("minimum-contrast = 1\n"));
        assert!(output.contains("background-opacity = 1\n"));
        assert!(output.contains("background-blur-radius = 0\n"));
        assert!(output.contains("window-save-state = default\n"));
        assert!(output.contains("macos-option-as-alt = false\n"));
        assert!(output.contains("macos-titlebar-style = native\n"));
        assert!(
            output.lines().all(|line| line.contains(" = ")),
            "every line must be `key = value`"
        );
    }

    #[test]
    fn show_config_output_renders_set_values() {
        let config = StartupConfig {
            theme: Some("3024 Day".to_string()),
            background: Some(Rgb::new(0x10, 0x20, 0x30)),
            minimum_contrast: 3.0,
            cursor_style: Some(CursorShape::Bar),
            macos_option_as_alt: MacosOptionAsAlt::Right,
            macos_titlebar_style: MacosTitlebarStyle::Hidden,
            font: noa_config::FontConfig {
                families: vec!["JetBrains Mono".to_string(), "Menlo".to_string()],
                features: vec![noa_config::FontFeature {
                    tag: *b"liga",
                    enabled: false,
                }],
                variations: vec![noa_config::FontVariation {
                    tag: *b"wght",
                    value: 700.0,
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let output = show_config_output(&config);

        assert!(output.contains("theme = 3024 Day\n"));
        assert!(output.contains("background = #102030\n"));
        assert!(output.contains("minimum-contrast = 3\n"));
        assert!(output.contains("cursor-style = bar\n"));
        assert!(output.contains("macos-option-as-alt = right\n"));
        assert!(output.contains("macos-titlebar-style = hidden\n"));
        assert!(output.contains("font-family = JetBrains Mono\n"));
        assert!(output.contains("font-family = Menlo\n"));
        assert!(output.contains("font-feature = -liga\n"));
        assert!(output.contains("font-variation = wght=700\n"));
    }

    #[test]
    fn list_actions_output_names_every_action() {
        let output = list_actions_output();

        for action in CliAction::ALL {
            assert!(
                output.contains(&format!("+{}", action.name())),
                "+{} must be listed",
                action.name()
            );
        }
    }

    #[test]
    fn unknown_action_message_names_the_action_and_lists_alternatives() {
        let message = unknown_action_message("bogus");

        assert!(message.contains("unknown action: +bogus"));
        assert!(message.contains("+list-themes"));
        assert!(message.ends_with('\n'));
    }
}
