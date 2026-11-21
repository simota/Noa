mod dashboard;
mod fleet;
mod live;

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_text::TextStore;

use crate::dashboard::Dashboard;
use crate::fleet::Grouping;
use crate::live::{Link, Snapshot};

const USAGE: &str = "usage: agent-dashboard-mock [--demo] [--group=parent|agent|model|none] \
                     [--snapshot=FILE.png]\n  \
                     live data from the running Noa's control server by default; \
                     --demo shows canned data";
const SNAPSHOT_SCALE: f32 = 2.0;
/// How long `--snapshot` waits for the live feed's first snapshot.
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

fn options() -> AppOptions {
    AppOptions {
        title: "Noa · エージェント運用ダッシュボード（モック）".into(),
        size: (1440.0, 900.0),
        ..AppOptions::new("Noa Agent Dashboard")
    }
}

fn snapshot(path: &PathBuf, screen: &mut dyn Screen) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let mut text = TextStore::new(Default::default());
    let (width, height) = options().size;
    headless
        .render_screen(
            screen,
            &mut text,
            (width as u32, height as u32),
            SNAPSHOT_SCALE,
        )
        .map_err(|e| e.to_string())?
        .save_png(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// The live feed's first snapshot with a connection, or why none came.
fn first_live_snapshot() -> Result<Snapshot, String> {
    let channel = live::start(|| {});
    let until = Instant::now() + FIRST_SNAPSHOT_WAIT;
    let mut reason = "Noa から 3 秒以内に応答がありません".to_owned();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match channel.snapshots.recv_timeout(left) {
            Ok(snapshot) => match &snapshot.link {
                Link::Live => return Ok(snapshot),
                Link::Down(why) => reason.clone_from(why),
                Link::Connecting => {}
            },
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return Err(reason);
            }
        }
    }
}

fn main() -> Result<(), String> {
    let mut snapshot_path = None;
    let mut grouping = Grouping::Orchestrator;
    let mut demo = false;
    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(name) = arg.strip_prefix("--group=") {
            grouping =
                Grouping::parse(name).ok_or_else(|| format!("unknown group {name:?}\n{USAGE}"))?;
        } else if arg == "--demo" {
            demo = true;
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    if let Some(path) = snapshot_path {
        let mut screen = if demo {
            Dashboard::new(grouping)
        } else {
            Dashboard::of_snapshot(grouping, first_live_snapshot()?)
        };
        snapshot(&path, &mut screen)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    App::new(options(), move |setup| {
        if demo {
            return Box::new(Dashboard::new(grouping));
        }
        let waker = setup.waker.clone();
        let channel = live::start(move || waker.wake());
        Box::new(Dashboard::live(grouping, channel))
    })
    .run()
    .map_err(|e| e.to_string())
}
