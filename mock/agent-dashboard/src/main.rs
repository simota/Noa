mod dashboard;
mod fleet;
mod live;

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use noa_gpui_core::{Event, Key, Modifiers};
use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::dashboard::Dashboard;
use crate::fleet::Grouping;
use crate::live::{Link, Snapshot};

const USAGE: &str = "usage: agent-dashboard-mock [--demo] [--group=parent|agent|model|none] \
                     [--snapshot=FILE.png [--size=WxH] [--keys=SEQ]]\n  \
                     --size: snapshot size in logical pixels (default 1440x900); \
                     --keys: keys sent before the snapshot, e.g. 'jj/codex<Enter>' \
                     (<Enter> <Esc> <Space> <Down> <Up> are named keys)\n  \
                     live data from the running Noa's control server by default; \
                     --demo shows canned data";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
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

/// `"1100x700"` as (1100, 700).
fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(bad()),
    }
}

/// Each character becomes a key press plus its text; `<Enter>`, `<Esc>`,
/// `<Space>`, `<Down>` and `<Up>` are named keys.
fn parse_keys(seq: &str) -> Result<Vec<Event>, String> {
    let press = |key| Event::KeyDown {
        key,
        modifiers: Modifiers::default(),
    };
    let mut events = Vec::new();
    let mut rest = seq;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let key = match &rest[1..end] {
                "Enter" => Key::Enter,
                "Esc" => Key::Escape,
                "Space" => Key::Space,
                "Down" => Key::ArrowDown,
                "Up" => Key::ArrowUp,
                name => return Err(format!("unknown key <{name}>\n{USAGE}")),
            };
            events.push(press(key));
            rest = &rest[end + 1..];
            continue;
        }
        events.push(press(Key::Char(c)));
        events.push(Event::Text(c.to_string()));
        rest = &rest[c.len_utf8()..];
    }
    Ok(events)
}

fn snapshot(
    path: &PathBuf,
    screen: &mut dyn Screen,
    size: (u32, u32),
    keys: Vec<Event>,
) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let mut text = TextStore::new(Default::default());
    let viewport = Size::new(size.0 as f32, size.1 as f32);
    screen.prepare(viewport, &mut text);
    for key in keys {
        screen.dispatch(key, &mut text);
        screen.prepare(viewport, &mut text);
    }
    headless
        .render_screen(screen, &mut text, size, SNAPSHOT_SCALE)
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
    let mut size = DEFAULT_SIZE;
    let mut keys = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(name) = arg.strip_prefix("--group=") {
            grouping =
                Grouping::parse(name).ok_or_else(|| format!("unknown group {name:?}\n{USAGE}"))?;
        } else if let Some(arg) = arg.strip_prefix("--size=") {
            size = parse_size(arg)?;
        } else if let Some(seq) = arg.strip_prefix("--keys=") {
            keys = parse_keys(seq)?;
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
        snapshot(&path, &mut screen, size, keys)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_into_events() {
        let press = |key| Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        };
        let mut expected = Vec::new();
        for c in ['j', '/', 'c', 'o'] {
            expected.push(press(Key::Char(c)));
            expected.push(Event::Text(c.to_string()));
        }
        expected.push(press(Key::Enter));
        let got = parse_keys("j/co<Enter>").unwrap();
        assert_eq!(format!("{got:?}"), format!("{expected:?}"));
    }

    #[test]
    fn size_parses() {
        assert_eq!(parse_size("1100x700"), Ok((1100, 700)));
        let err = parse_size("1100").unwrap_err();
        assert!(err.contains("usage:"), "{err}");
    }
}
