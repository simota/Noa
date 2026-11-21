mod dashboard;
mod fleet;

use std::path::PathBuf;

use noa_gpui_platform::{App, AppOptions, Headless};
use noa_gpui_text::TextStore;

use crate::dashboard::Dashboard;
use crate::fleet::Grouping;

const USAGE: &str =
    "usage: agent-dashboard-mock [--group=parent|agent|model|none] [--snapshot=FILE.png]";
const SNAPSHOT_SCALE: f32 = 2.0;

fn options() -> AppOptions {
    AppOptions {
        title: "Noa · エージェント運用ダッシュボード（モック）".into(),
        size: (1440.0, 900.0),
        ..AppOptions::new("Noa Agent Dashboard")
    }
}

fn snapshot(path: &PathBuf, grouping: Grouping) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let mut screen = Dashboard::new(grouping);
    let mut text = TextStore::new(Default::default());
    let (width, height) = options().size;
    headless
        .render_screen(
            &mut screen,
            &mut text,
            (width as u32, height as u32),
            SNAPSHOT_SCALE,
        )
        .map_err(|e| e.to_string())?
        .save_png(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn main() -> Result<(), String> {
    let mut snapshot_path = None;
    let mut grouping = Grouping::Orchestrator;
    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(name) = arg.strip_prefix("--group=") {
            grouping = Grouping::parse(name)
                .ok_or_else(|| format!("unknown group {name:?}\n{USAGE}"))?;
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    if let Some(path) = snapshot_path {
        snapshot(&path, grouping)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    App::new(options(), move |_setup| Box::new(Dashboard::new(grouping)))
        .run()
        .map_err(|e| e.to_string())
}
