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
    })?;
    noa_app::run(noa_app::AppConfig {
        cols: config.cols,
        rows: config.rows,
        font_size: config.font_size,
    })
}
