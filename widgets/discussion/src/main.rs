//! The discussion widget's binary: the noa-gpui window over the library's
//! model, engine and store (`lib.rs`), fed by the live layer (`live.rs`).

mod demo;
mod live;
/// The live session runner (`src/live/`): `ipc` is the Noa port and its
/// `noa-ipc` implementation, `runner` drives one discussion against it.
/// [`live::start`] bridges it to the UI.
#[path = "live"]
mod session {
    pub mod ipc;
    pub mod runner;
}
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use discussion::store::Summary;

use crate::live::{Link, Snapshot};
use crate::ui::Discussion;

const USAGE: &str = "usage: discussion [--demo | --demo-start | --session=DIR] \
                     [--snapshot=FILE.png [--size=WxH]]\n  \
                     the live discussion by default; \
                     --demo shows a canned multi-round discussion, \
                     --demo-start the start view with canned stored discussions, \
                     --session the discussion stored at DIR, read-only";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How long `--snapshot` waits for the live feed's first snapshot.
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

/// The window title in the person's language: Japanese where the locale is
/// Japanese, English anywhere else. The locale variables in POSIX order
/// (`LC_ALL`, `LC_MESSAGES`, `LANG`: the first one set wins), else the
/// system's preferred language, as the agent dashboard decides.
fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · 司会付きディスカッション",
        _ => "Noa · Moderated Discussion",
    }
}

/// The first of the person's preferred languages, as a BCP 47 tag (`ja-JP`).
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
        ..AppOptions::new("Noa Discussion")
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

/// The live feed's first snapshot, and the stored discussions it listed.
fn first_live_snapshot() -> Result<(Snapshot, Arc<Vec<Summary>>), String> {
    let channel = live::start(|| {});
    let history = || channel.history.try_recv().unwrap_or_default();
    let until = Instant::now() + FIRST_SNAPSHOT_WAIT;
    let mut reason = "Noa から 3 秒以内に応答がありません".to_owned();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match channel.snapshots.recv_timeout(left) {
            // Before a session starts there is no link to wait for: the
            // start view is what the window shows.
            Ok(snapshot) if snapshot.session.is_none() => return Ok((snapshot, history())),
            Ok(snapshot) => match &snapshot.link {
                Link::Live | Link::Stored => return Ok((snapshot, history())),
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
    let mut demo_start = false;
    let mut stored = None;
    let mut size = DEFAULT_SIZE;

    for arg in std::env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot_path = Some(PathBuf::from(path));
        } else if let Some(raw) = arg.strip_prefix("--size=") {
            size = parse_size(raw)?;
        } else if let Some(dir) = arg.strip_prefix("--session=") {
            stored = Some(PathBuf::from(dir));
        } else if arg == "--demo" {
            demo = true;
        } else if arg == "--demo-start" {
            demo_start = true;
        } else if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(());
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }

    if usize::from(demo) + usize::from(demo_start) + usize::from(stored.is_some()) > 1 {
        return Err(format!(
            "--demo, --demo-start and --session do not go together\n{USAGE}"
        ));
    }
    // Folded read-only: no agent or runner starts.
    let stored = match stored {
        Some(dir) => {
            let session = discussion::store::load(&dir)
                .map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
            Some((dir, session))
        }
        None => None,
    };

    if let Some(path) = snapshot_path {
        let mut screen = if demo {
            Discussion::demo()
        } else if demo_start {
            Discussion::demo_start()
        } else if let Some((dir, session)) = stored {
            Discussion::of_stored(dir, session)
        } else {
            let (snapshot, history) = first_live_snapshot()?;
            Discussion::of_snapshot(snapshot).with_history(history)
        };
        render_snapshot(&path, &mut screen, size)?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    App::new(options(), move |setup| {
        if demo {
            return Box::new(Discussion::demo());
        }
        if demo_start {
            return Box::new(Discussion::demo_start());
        }
        if let Some((dir, session)) = stored {
            return Box::new(Discussion::of_stored(dir, session));
        }
        let waker = setup.waker.clone();
        let channel = live::start(move || waker.wake());
        Box::new(Discussion::live(channel))
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
        let ja = "Noa · 司会付きディスカッション";
        let en = "Noa · Moderated Discussion";
        assert_eq!(title(env(&[("LANG", "ja_JP.UTF-8")]), || None), ja);
        assert_eq!(title(env(&[("LANG", "en_US.UTF-8")]), || None), en);
        // LC_ALL wins over LANG; an empty one is skipped.
        assert_eq!(
            title(
                env(&[("LC_ALL", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")]),
                || None
            ),
            en
        );
        assert_eq!(
            title(env(&[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")]), || None),
            ja
        );
        // No locale variables: the system's preferred language.
        assert_eq!(title(env(&[]), || Some("ja-JP".into())), ja);
        assert_eq!(title(env(&[]), || Some("en-US".into())), en);
        assert_eq!(title(env(&[]), || None), en);
    }

    #[test]
    fn sizes_parse_like_repo_browser() {
        assert_eq!(parse_size("1280x800"), Ok((1280, 800)));
        assert!(parse_size("0x800").is_err());
        assert!(parse_size("1280").is_err());
    }
}
