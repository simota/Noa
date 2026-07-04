use clap::Parser;

/// noa — a faithful Rust clone of the Ghostty terminal emulator.
#[derive(Parser, Debug)]
#[command(name = "noa", version, about)]
struct Args {
    /// Initial columns.
    #[arg(long)]
    cols: Option<u16>,
    /// Initial rows.
    #[arg(long)]
    rows: Option<u16>,
    /// Font size in points.
    #[arg(long)]
    font_size: Option<f32>,
    /// Import supported settings from Ghostty config into noa config.
    #[arg(long)]
    import_ghostty_config: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // `noa +<action>` runs a one-shot query and exits without the GUI, so it
    // must be dispatched before clap sees (and rejects) the `+` argument.
    let argv: Vec<String> = std::env::args().collect();
    match noa_app::parse_invocation(&argv) {
        noa_app::Invocation::Action(action) => return noa_app::run_action(action),
        noa_app::Invocation::Unknown(name) => {
            eprint!("{}", noa_app::unknown_action_message(&name));
            std::process::exit(1);
        }
        noa_app::Invocation::Gui => {}
    }

    let args = Args::parse();

    if args.import_ghostty_config {
        return run_import();
    }

    let (config, diagnostics) = noa_config::load_startup_config(noa_config::ConfigOverrides {
        cols: args.cols,
        rows: args.rows,
        font_size: args.font_size,
        theme: None,
        ..Default::default()
    })?;
    for diagnostic in diagnostics {
        eprintln!("{}", diagnostic.message);
    }
    if let Some(message) = import_hint(config_exists(), ghostty_config_exists()) {
        eprintln!("{message}");
    }
    // An explicit `--cols`/`--rows` means the user asked for specific
    // dimensions, which suppresses session restore (the saved topology would
    // otherwise override them).
    let cli_grid_override = args.cols.is_some() || args.rows.is_some();
    noa_app::run(app_config_from_startup(config, cli_grid_override))
}

fn run_import() -> anyhow::Result<()> {
    match noa_config::import_ghostty_config() {
        Ok(outcome) => {
            println!(
                "Imported Ghostty config to {} ({} supported, {} commented out)",
                outcome.target.display(),
                outcome.stats.supported,
                outcome.stats.commented_out
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}

fn app_config_from_startup(
    config: noa_config::StartupConfig,
    cli_grid_override: bool,
) -> noa_app::AppConfig {
    noa_app::AppConfig {
        cols: config.cols,
        rows: config.rows,
        font_size: config.font_size,
        theme: config.theme,
        font: config.font,
        clipboard_read: config.clipboard_read,
        clipboard_paste_protection: config.clipboard_paste_protection,
        window_padding_x: config.window_padding_x,
        window_padding_y: config.window_padding_y,
        background: config.background,
        foreground: config.foreground,
        cursor_color: config.cursor_color,
        selection_foreground: config.selection_foreground,
        selection_background: config.selection_background,
        cursor_style: config.cursor_style,
        cursor_style_blink: config.cursor_style_blink,
        background_opacity: config.background_opacity,
        background_blur_radius: config.background_blur_radius,
        scrollback_limit: config.scrollback_limit,
        window_save_state: config.window_save_state,
        cli_grid_override,
    }
}

fn config_exists() -> bool {
    noa_config::default_config_path().is_some_and(|path| path.exists())
}

fn ghostty_config_exists() -> bool {
    noa_config::ghostty_config_candidates()
        .iter()
        .any(|path| path.exists())
}

fn import_hint(config_exists: bool, any_candidate_exists: bool) -> Option<&'static str> {
    (!config_exists && any_candidate_exists).then_some(
        "Ghostty config detected. Run `noa --import-ghostty-config` to create a noa config.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_theme_is_forwarded_to_app_config() {
        let config = noa_config::StartupConfig {
            cols: 100,
            rows: 30,
            font_size: 15.0,
            theme: Some("3024 Day".to_string()),
            font: noa_config::FontConfig::default(),
            ..Default::default()
        };

        let app_config = app_config_from_startup(config, false);

        assert_eq!(app_config.cols, 100);
        assert_eq!(app_config.rows, 30);
        assert_eq!(app_config.font_size, 15.0);
        assert_eq!(app_config.theme.as_deref(), Some("3024 Day"));
        assert_eq!(app_config.font, noa_config::FontConfig::default());
    }

    #[test]
    fn theme_cli_input_is_not_defined() {
        let flag = ["--", "theme"].concat();

        assert!(Args::try_parse_from(["noa", flag.as_str(), "3024 Day"]).is_err());
    }

    #[test]
    fn import_flag_is_defined() {
        let args = Args::try_parse_from(["noa", "--import-ghostty-config"]).unwrap();

        assert!(args.import_ghostty_config);
    }

    #[test]
    fn plus_actions_must_be_dispatched_before_clap() {
        // clap rejects `+version` outright, which is why main() classifies
        // the invocation first and only falls through to clap for the GUI.
        assert!(Args::try_parse_from(["noa", "+version"]).is_err());
        assert_eq!(
            noa_app::parse_invocation(&["noa", "+version"]),
            noa_app::Invocation::Action(noa_app::CliAction::Version)
        );
        assert_eq!(
            noa_app::parse_invocation(&["noa", "--cols", "100"]),
            noa_app::Invocation::Gui
        );
    }

    #[test]
    fn import_hint_requires_missing_noa_config_and_existing_ghostty_config() {
        assert_eq!(
            import_hint(false, true),
            Some(
                "Ghostty config detected. Run `noa --import-ghostty-config` to create a noa config."
            )
        );
        assert_eq!(import_hint(false, false), None);
        assert_eq!(import_hint(true, false), None);
        assert_eq!(import_hint(true, true), None);
    }
}
