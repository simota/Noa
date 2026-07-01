use clap::Parser;

/// noa — a faithful Rust clone of the Ghostty terminal emulator.
#[derive(Parser, Debug)]
#[command(name = "noa", version, about)]
struct Args {
    /// Initial columns.
    #[arg(long, default_value_t = 80)]
    cols: u16,
    /// Initial rows.
    #[arg(long, default_value_t = 24)]
    rows: u16,
    /// Font size in points.
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();
    noa_app::run(noa_app::AppConfig {
        cols: args.cols,
        rows: args.rows,
        font_size: args.font_size,
    })
}
