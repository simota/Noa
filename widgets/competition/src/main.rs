//! The competition widget's binary: the noa-gpui window over the library's
//! model, engine, store and comparison viewer, fed by the live layer.

mod demo;
mod live;
#[path = "live"]
mod session {
    pub mod ipc;
    pub mod runner;
}
mod ui;

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use crate::live::{Link, Snapshot};
use crate::ui::Competition;

const USAGE: &str = "usage: competition [--demo] [--snapshot=FILE.png [--size=WxH]]\n  \
                     the live competition by default; \
                     --demo shows a canned multi-round competition";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · コンペティション",
        _ => "Noa · Competition",
    }
}

#[cfg(target_os = "macos")]
fn system_language() -> Option<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|tag| tag.to_string())
}

#[cfg(not(target_os = "macos"))]
fn system_language() -> Option<String> {
    None
}

fn options() -> AppOptions {
    AppOptions {
        title: title(|key| std::env::var(key).ok(), system_language).into(),
        size: (1440.0, 900.0),
        min_size: Some(ui::MIN_WINDOW),
        ..AppOptions::new("Noa Competition")
    }
}

fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if (1..=4096).contains(&w) && (1..=4096).contains(&h) => Ok((w, h)),
        _ => Err(bad()),
    }
}

fn first_live_snapshot() -> Result<Snapshot, String> {
    let channel = live::start(|| {});
    let until = Instant::now() + FIRST_SNAPSHOT_WAIT;
    let mut reason = "Noa から 3 秒以内に応答がありません".to_owned();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match channel.snapshots.recv_timeout(left) {
            Ok(snapshot) if snapshot.session.is_none() => return Ok(snapshot),
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

fn render_snapshot(
    path: &PathBuf,
    screen: &mut dyn Screen,
    size: (u32, u32),
) -> Result<(), String> {
    let mut headless = Headless::new_or_software();
    let mut text = TextStore::new(Default::default());
    let viewport = Size::new(size.0 as f32, size.1 as f32);
    screen.prepare(viewport, &mut text);
    headless
        .render_screen(screen, &mut text, size, SNAPSHOT_SCALE)
        .map_err(|e| e.to_string())?
        .save_png(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn main() -> Result<(), String> {
    let mut snapshot_path = None;
    let mut demo = false;
    let mut size = DEFAULT_SIZE;

    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(raw) = arg.strip_prefix("--size=") {
            size = parse_size(raw)?;
        } else if arg == "--demo" {
            demo = true;
        } else if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(());
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }

    if let Some(path) = snapshot_path {
        let mut screen = if demo {
            Competition::demo()
        } else {
            Competition::of_snapshot(first_live_snapshot()?)
        };
        render_snapshot(&path, &mut screen, size)?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    App::new(options(), move |setup| {
        if demo {
            return Box::new(Competition::demo());
        }
        let waker = setup.waker.clone();
        let channel = live::start(move || waker.wake());
        Box::new(Competition::live(channel))
    })
    .run()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_follows_the_locale() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            title(env(&[("LANG", "ja_JP.UTF-8")]), || None),
            "Noa · コンペティション"
        );
        assert_eq!(
            title(env(&[("LANG", "en_US.UTF-8")]), || None),
            "Noa · Competition"
        );
    }

    #[test]
    fn sizes_parse_correctly() {
        assert_eq!(parse_size("1280x800"), Ok((1280, 800)));
        assert!(parse_size("0x800").is_err());
    }
}
