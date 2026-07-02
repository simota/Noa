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
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();
    let config = noa_config::load_startup_config(noa_config::ConfigOverrides {
        cols: args.cols,
        rows: args.rows,
        font_size: args.font_size,
        theme: None,
    })?;
    noa_app::run(app_config_from_startup(config))
}

fn app_config_from_startup(config: noa_config::StartupConfig) -> noa_app::AppConfig {
    noa_app::AppConfig {
        cols: config.cols,
        rows: config.rows,
        font_size: config.font_size,
        theme: config.theme,
    }
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
        };

        let app_config = app_config_from_startup(config);

        assert_eq!(app_config.cols, 100);
        assert_eq!(app_config.rows, 30);
        assert_eq!(app_config.font_size, 15.0);
        assert_eq!(app_config.theme.as_deref(), Some("3024 Day"));
    }

    #[test]
    fn theme_cli_input_is_not_defined() {
        let flag = ["--", "theme"].concat();

        assert!(Args::try_parse_from(["noa", flag.as_str(), "3024 Day"]).is_err());
    }
}
