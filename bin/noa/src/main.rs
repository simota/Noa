use clap::Parser;

/// Noa — a faithful Rust clone of the Ghostty terminal emulator.
#[derive(Parser, Debug)]
#[command(name = "Noa", version, about)]
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
    /// Whether to load the default config files (Ghostty parity). Pass
    /// `--config-default-files=false` to run with built-in defaults + CLI
    /// flags only; live config reload stays disabled for the process.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL"
    )]
    config_default_files: bool,
    /// Run this command instead of the login shell (Ghostty parity). Greedy:
    /// everything after `-e` becomes the command's argv, so it must be the
    /// last flag on the line.
    #[arg(short = 'e', value_name = "COMMAND", num_args = 1.., allow_hyphen_values = true)]
    command: Option<Vec<String>>,
}

fn main() -> anyhow::Result<()> {
    noa_app::startup_trace::init();
    env_logger::init();

    // `Noa +<action>` runs a one-shot query and exits without the GUI, so it
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

    let cli_overrides = noa_config::ConfigOverrides {
        cols: args.cols,
        rows: args.rows,
        font_size: args.font_size,
        theme: None,
        ..Default::default()
    };
    let (config, diagnostics) = if args.config_default_files {
        noa_config::load_startup_config(cli_overrides.clone())?
    } else {
        noa_config::load_startup_config_without_files(cli_overrides.clone())?
    };
    noa_app::startup_trace::mark("config-loaded");
    for diagnostic in diagnostics {
        eprintln!("{}", diagnostic.message);
    }
    if args.config_default_files
        && let Some(message) = import_hint(config_exists(), ghostty_config_exists())
    {
        eprintln!("{message}");
    }
    // An explicit `--cols`/`--rows` means the user asked for specific
    // dimensions, which suppresses session restore (the saved topology would
    // otherwise override them). `-e` suppresses it too: a saved topology
    // would respawn every restored pane running the one-shot command
    // (Ghostty likewise starts `-e` in a fresh single-surface window).
    let cli_grid_override = args.cols.is_some() || args.rows.is_some() || args.command.is_some();
    let mut app_config = noa_app::AppConfig::from_startup(config, cli_grid_override, cli_overrides);
    app_config.launch_command = args.command;
    app_config.config_default_files = args.config_default_files;
    noa_app::run(app_config)
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
        "Ghostty config detected. Run `Noa --import-ghostty-config` to create a noa config.",
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
            minimum_contrast: 3.0,
            confirm_quit: false,
            macos_option_as_alt: noa_config::MacosOptionAsAlt::Both,
            macos_titlebar_style: noa_config::MacosTitlebarStyle::Transparent,
            macos_non_native_fullscreen: true,
            sidebar_preview_lines: 4,
            audible_bell: true,
            audible_bell_when_unfocused: true,
            audible_bell_dock_bounce: true,
            auto_approve: true,
            ..Default::default()
        };

        let app_config =
            noa_app::AppConfig::from_startup(config, false, noa_config::ConfigOverrides::default());

        assert_eq!(app_config.cols, 100);
        assert_eq!(app_config.rows, 30);
        assert_eq!(app_config.font_size, 15.0);
        assert_eq!(app_config.theme.as_deref(), Some("3024 Day"));
        assert_eq!(app_config.font, noa_config::FontConfig::default());
        assert_eq!(app_config.minimum_contrast, 3.0);
        assert!(!app_config.confirm_quit);
        assert_eq!(
            app_config.macos_option_as_alt,
            noa_config::MacosOptionAsAlt::Both
        );
        assert_eq!(
            app_config.macos_titlebar_style,
            noa_config::MacosTitlebarStyle::Transparent
        );
        assert!(app_config.macos_non_native_fullscreen);
        assert_eq!(app_config.sidebar_preview_lines, 4);
        assert!(app_config.audible_bell);
        assert!(app_config.audible_bell_when_unfocused);
        assert!(app_config.audible_bell_dock_bounce);
        assert!(app_config.auto_approve);
    }

    #[test]
    fn client_keys_flow_to_app_config_without_debug_secret_exposure() {
        let config = noa_config::StartupConfig {
            server_token: Some("server-secret-literal".to_string()),
            client_remote: Some("remote.example:61771".to_string()),
            client_token: Some("client-secret-literal".to_string()),
            client_token_file: Some(std::path::PathBuf::from("/tmp/client-token")),
            ..Default::default()
        };
        let cli_overrides = noa_config::ConfigOverrides {
            client_token: Some("cli-secret-literal".to_string()),
            ..Default::default()
        };

        let app_config = noa_app::AppConfig::from_startup(config, false, cli_overrides);

        assert_eq!(
            app_config.client_remote.as_deref(),
            Some("remote.example:61771")
        );
        assert_eq!(
            app_config.client_token.as_deref(),
            Some("client-secret-literal")
        );
        assert_eq!(
            app_config.client_token_file.as_deref(),
            Some(std::path::Path::new("/tmp/client-token"))
        );

        let debug = format!("{app_config:?}");
        assert!(debug.contains("client_token: Some(\"<redacted>\")"));
        assert!(debug.contains("server_token: Some(\"<redacted>\")"));
        assert!(debug.contains("client_token_file: Some(\"/tmp/client-token\")"));
        assert!(!debug.contains("client-secret-literal"));
        assert!(!debug.contains("server-secret-literal"));
        assert!(!debug.contains("cli-secret-literal"));
    }

    // AC-7: a config carrying all background-image keys resolves through
    // `AppConfig::from_startup` into an `AppConfig` holding those values.
    #[test]
    fn background_image_keys_flow_from_startup_config_to_app_config() {
        let config = noa_config::StartupConfig {
            background_image: Some(std::path::PathBuf::from("/tmp/wall.png")),
            background_image_opacity: 0.5,
            background_image_position: noa_config::BackgroundImagePosition::TopRight,
            background_image_fit: noa_config::BackgroundImageFit::Cover,
            background_image_repeat: true,
            background_image_interval_secs: 12,
            ..Default::default()
        };

        let app_config =
            noa_app::AppConfig::from_startup(config, false, noa_config::ConfigOverrides::default());

        assert_eq!(
            app_config.background_image,
            Some(std::path::PathBuf::from("/tmp/wall.png"))
        );
        assert_eq!(app_config.background_image_opacity, 0.5);
        assert_eq!(
            app_config.background_image_position,
            noa_config::BackgroundImagePosition::TopRight
        );
        assert_eq!(
            app_config.background_image_fit,
            noa_config::BackgroundImageFit::Cover
        );
        assert!(app_config.background_image_repeat);
        assert_eq!(app_config.background_image_interval_secs, 12);
    }

    // AC-7 (end-to-end from a config file): parsing a config source with all
    // five keys and applying it yields the five resolved values.
    #[test]
    fn background_image_keys_parse_and_apply_from_config_source() {
        let (overrides, diagnostics) = noa_config::parse_overrides(
            std::path::Path::new("/tmp/noa-test-config"),
            "background-image = /tmp/wall.png\n\
             background-image-opacity = 0.25\n\
             background-image-position = bottom-left\n\
             background-image-fit = stretch\n\
             background-image-repeat = true\n\
             background-image-interval = 45",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let startup = overrides.apply_to(noa_config::StartupConfig::default());
        let app_config = noa_app::AppConfig::from_startup(
            startup,
            false,
            noa_config::ConfigOverrides::default(),
        );

        assert_eq!(
            app_config.background_image,
            Some(std::path::PathBuf::from("/tmp/wall.png"))
        );
        assert_eq!(app_config.background_image_opacity, 0.25);
        assert_eq!(
            app_config.background_image_position,
            noa_config::BackgroundImagePosition::BottomLeft
        );
        assert_eq!(
            app_config.background_image_fit,
            noa_config::BackgroundImageFit::Stretch
        );
        assert!(app_config.background_image_repeat);
        assert_eq!(app_config.background_image_interval_secs, 45);
    }

    #[test]
    fn theme_cli_input_is_not_defined() {
        let flag = ["--", "theme"].concat();

        assert!(Args::try_parse_from(["Noa", flag.as_str(), "3024 Day"]).is_err());
    }

    #[test]
    fn dash_e_consumes_the_rest_of_the_line_as_the_command() {
        // `-e` is greedy (Ghostty parity): everything after it — including
        // hyphen-prefixed tokens like `-c` — belongs to the command's argv,
        // while flags placed before it still parse as noa's own.
        let args = Args::try_parse_from(["Noa", "--cols", "120", "-e", "/bin/sh", "-c", "echo hi"])
            .unwrap();

        assert_eq!(args.cols, Some(120));
        assert_eq!(
            args.command,
            Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string()
            ])
        );
    }

    #[test]
    fn import_flag_is_defined() {
        let args = Args::try_parse_from(["Noa", "--import-ghostty-config"]).unwrap();

        assert!(args.import_ghostty_config);
    }

    /// Ghostty-parity `--config-default-files`: defaults to true, and both
    /// the `=false` and bare-flag forms parse (the bench harness and scripts
    /// rely on `--config-default-files=false` for config-free launches).
    #[test]
    fn config_default_files_flag_parses_ghostty_style() {
        assert!(Args::try_parse_from(["Noa"]).unwrap().config_default_files);
        assert!(
            !Args::try_parse_from(["Noa", "--config-default-files=false"])
                .unwrap()
                .config_default_files
        );
        assert!(
            Args::try_parse_from(["Noa", "--config-default-files=true"])
                .unwrap()
                .config_default_files
        );
        assert!(
            Args::try_parse_from(["Noa", "--config-default-files"])
                .unwrap()
                .config_default_files
        );
    }

    #[test]
    fn plus_actions_must_be_dispatched_before_clap() {
        // clap rejects `+version` outright, which is why main() classifies
        // the invocation first and only falls through to clap for the GUI.
        assert!(Args::try_parse_from(["Noa", "+version"]).is_err());
        assert_eq!(
            noa_app::parse_invocation(&["Noa", "+version"]),
            noa_app::Invocation::Action(noa_app::CliAction::Version)
        );
        assert_eq!(
            noa_app::parse_invocation(&["Noa", "--cols", "100"]),
            noa_app::Invocation::Gui
        );
    }

    #[test]
    fn import_hint_requires_missing_noa_config_and_existing_ghostty_config() {
        assert_eq!(
            import_hint(false, true),
            Some(
                "Ghostty config detected. Run `Noa --import-ghostty-config` to create a noa config."
            )
        );
        assert_eq!(import_hint(false, false), None);
        assert_eq!(import_hint(true, false), None);
        assert_eq!(import_hint(true, true), None);
    }
}
