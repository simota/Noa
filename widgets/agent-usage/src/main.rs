//! `agent-usage`: the Noa window for the coding agents' rate limits and
//! local-log statistics (`docs/specs/agent-usage.md`). Flags, window title,
//! the headless snapshot and the app.

#[path = "../../shared/appearance.rs"]
mod appearance;

mod demo;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use agent_usage::paths::Roots;
use agent_usage::stats::types::{AgentFilter, Period};

use ui::prefs::{Overrides, Prefs, PrefsStore, Tab};
use ui::{AgentUsage, Clock, LiveFeed};

const USAGE: &str = "usage: agent-usage [--demo] [--tab=limits|stats] [--period=today|7d|30d|all] \
                     [--agent=all|claude|codex|agy] [--prefs=FILE] \
                     [--snapshot=FILE.png [--size=WxH]]";
const DEFAULT_SIZE: (u32, u32) = (1280, 860);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How often the window redraws on its own, so ages and "today" move.
const TICK: Duration = Duration::from_secs(30);

fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · エージェント使用量",
        _ => "Noa · Agent Usage",
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

/// The window title. Off macOS, winit draws the title bar itself on Wayland
/// (GNOME) with a single font and no glyph fallback, so a Japanese title shows
/// as boxes there: it stays English.
fn window_title() -> &'static str {
    if cfg!(target_os = "macos") {
        title(|key| std::env::var(key).ok(), system_language)
    } else {
        "Noa · Agent Usage"
    }
}

fn options() -> AppOptions {
    AppOptions {
        title: window_title().into(),
        size: (DEFAULT_SIZE.0 as f64, DEFAULT_SIZE.1 as f64),
        min_size: Some((960.0, 640.0)),
        ..AppOptions::new("Noa Agent Usage")
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    demo: bool,
    overrides: Overrides,
    prefs: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    size: Option<(u32, u32)>,
    help: bool,
}

fn parse_size(arg: &str) -> Result<(u32, u32), String> {
    let bad = || format!("bad size {arg:?}, expected WxH\n{USAGE}");
    let (w, h) = arg.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if (1..=4096).contains(&w) && (1..=4096).contains(&h) => Ok((w, h)),
        _ => Err(bad()),
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let bad = |what: &str, v: &str| format!("unknown {what} {v:?}\n{USAGE}");
    for arg in args {
        if arg == "--demo" {
            a.demo = true;
        } else if let Some(v) = arg.strip_prefix("--tab=") {
            a.overrides.tab = Some(Tab::parse(v).ok_or_else(|| bad("tab", v))?);
        } else if let Some(v) = arg.strip_prefix("--period=") {
            a.overrides.period = Some(Period::parse(v).ok_or_else(|| bad("period", v))?);
        } else if let Some(v) = arg.strip_prefix("--agent=") {
            a.overrides.agent = Some(AgentFilter::parse(v).ok_or_else(|| bad("agent", v))?);
        } else if let Some(v) = arg.strip_prefix("--prefs=") {
            a.prefs = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--snapshot=") {
            a.snapshot = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--size=") {
            a.size = Some(parse_size(v)?);
        } else if arg == "--help" || arg == "-h" {
            a.help = true;
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    // A snapshot never starts CLIs or reads credentials.
    if a.snapshot.is_some() && !a.demo {
        return Err(format!("--snapshot needs --demo\n{USAGE}"));
    }
    if a.size.is_some() && a.snapshot.is_none() {
        return Err(format!("--size goes with --snapshot\n{USAGE}"));
    }
    Ok(a)
}

/// The view to start with: what `store` holds (defaults without one), the
/// flags winning over it.
fn initial_prefs(store: Option<&mut PrefsStore>, overrides: Overrides) -> Prefs {
    store.map(|s| s.load()).unwrap_or_default().with(overrides)
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
    if appearance::run_if_requested()? {
        return Ok(());
    }
    let args = parse_args(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }

    // The demo and the snapshot touch prefs only when `--prefs` names a file.
    let prefs_path = match (&args.prefs, args.demo) {
        (Some(path), _) => Some(path.clone()),
        (None, true) => None,
        (None, false) => ui::default_prefs_path(),
    };
    let mut store = prefs_path.map(PrefsStore::at);
    let prefs = initial_prefs(store.as_mut(), args.overrides);

    if let Some(path) = &args.snapshot {
        let mut screen = AgentUsage::demo(prefs, store);
        render_snapshot(path, &mut screen, args.size.unwrap_or(DEFAULT_SIZE))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let demo = args.demo;
    App::new(options(), move |setup| {
        let waker = setup.waker.clone();
        let ticker = setup.waker.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                ticker.wake();
            }
        });
        if demo {
            return Box::new(AgentUsage::demo(prefs, store));
        }
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.wake());
        let feed = LiveFeed::start(Roots::system(), wake);
        Box::new(AgentUsage::new(Box::new(feed), Clock::live(), prefs, store))
    })
    .run()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_usage::model::Agent;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn title_follows_the_locale() {
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
            "Noa · エージェント使用量"
        );
        assert_eq!(
            title(env(&[("LANG", "en_US.UTF-8")]), || None),
            "Noa · Agent Usage"
        );
        // LC_ALL wins over LANG; the system language is the fallback.
        assert_eq!(
            title(env(&[("LC_ALL", "en_GB"), ("LANG", "ja_JP")]), || None),
            "Noa · Agent Usage"
        );
        assert_eq!(
            title(env(&[]), || Some("ja-JP".into())),
            "Noa · エージェント使用量"
        );
        assert_eq!(title(env(&[]), || None), "Noa · Agent Usage");
    }

    #[test]
    fn snapshot_without_demo_is_refused() {
        let err = args(&["--snapshot=x.png"]).unwrap_err();
        assert!(err.contains("--snapshot needs --demo"), "{err}");
        let ok = args(&[
            "--demo",
            "--tab=stats",
            "--snapshot=x.png",
            "--size=800x600",
        ])
        .unwrap();
        assert!(ok.demo);
        assert_eq!(ok.size, Some((800, 600)));
        assert_eq!(ok.overrides.tab, Some(Tab::Stats));
        assert!(args(&["--tab=nope"]).is_err());
        assert!(args(&["--size=10x10"]).is_err());
    }

    #[test]
    fn flags_override_prefs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("view.json");
        std::fs::write(
            &path,
            r#"{"version":1,"tab":"stats","period":"all","agent":"codex"}"#,
        )
        .unwrap();
        let a = args(&["--tab=limits", "--agent=claude"]).unwrap();
        let mut store = PrefsStore::at(path.clone());
        let p = initial_prefs(Some(&mut store), a.overrides);
        assert_eq!(
            p,
            Prefs {
                tab: Tab::Limits,
                period: Period::All,
                agent: AgentFilter::One(Agent::Claude),
            }
        );
        // No flags: what was saved. No store: the defaults with the flags.
        let mut store = PrefsStore::at(path);
        assert_eq!(
            initial_prefs(Some(&mut store), Overrides::default()).tab,
            Tab::Stats
        );
        let p = initial_prefs(None, args(&["--period=today"]).unwrap().overrides);
        assert_eq!(
            p,
            Prefs {
                period: Period::Today,
                ..Prefs::default()
            }
        );
    }
}
