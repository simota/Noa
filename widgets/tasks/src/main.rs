//! `tasks`: the Noa window for a repository's tasks, local and GitHub
//! issues on one list, each handed to an agent in a Noa float
//! (`docs/specs/task-widget.md`). Flags, window title, the headless
//! snapshot and the app.

#[path = "../../shared/appearance.rs"]
mod appearance;
mod demo;
mod installed;
mod live;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use tasks::model::types::GitHubView;

use demo::DemoFeed;
use live::LiveFeed;
use ui::{Feed, Intent, Layout, Tasks, View};

const USAGE: &str = "usage: tasks [--demo | --repo=DIR] [--select=ID] \
                     [--layout=board|list|timeline|calendar] \
                     [--snapshot=FILE.png [--size=WxH]]";
const DEFAULT_SIZE: (u32, u32) = (1280, 860);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How often the window wakes on its own, so ages move.
const TICK: Duration = Duration::from_secs(2);

fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · タスク",
        _ => "Noa · Tasks",
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
        "Noa · Tasks"
    }
}

fn options() -> AppOptions {
    AppOptions {
        title: window_title().into(),
        size: (DEFAULT_SIZE.0 as f64, DEFAULT_SIZE.1 as f64),
        min_size: Some((720.0, 600.0)),
        ..AppOptions::new("Noa Tasks")
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    demo: bool,
    repo: Option<PathBuf>,
    select: Option<String>,
    layout: Option<Layout>,
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
    for arg in args {
        if arg == "--demo" {
            a.demo = true;
        } else if let Some(v) = arg.strip_prefix("--repo=") {
            if v.is_empty() {
                return Err(format!("--repo needs a directory\n{USAGE}"));
            }
            a.repo = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--select=") {
            a.select = Some(v.to_owned());
        } else if let Some(v) = arg.strip_prefix("--layout=") {
            a.layout =
                Some(Layout::from_word(v).ok_or_else(|| format!("unknown layout {v:?}\n{USAGE}"))?);
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
    if a.demo && a.repo.is_some() {
        return Err(format!("--demo and --repo do not go together\n{USAGE}"));
    }
    // A snapshot never reads a repository, `gh` or Noa.
    if a.snapshot.is_some() && !a.demo {
        return Err(format!("--snapshot needs --demo\n{USAGE}"));
    }
    if a.size.is_some() && a.snapshot.is_none() {
        return Err(format!("--size goes with --snapshot\n{USAGE}"));
    }
    Ok(a)
}

/// `--repo`, made absolute against `cwd`; `cwd` itself without one. A
/// trailing slash is dropped, so `--repo=/src/noa/` is `/src/noa`.
fn repo_dir(repo: Option<&Path>, cwd: &Path) -> PathBuf {
    let dir = match repo {
        Some(dir) if dir.is_absolute() => dir.to_owned(),
        Some(dir) => cwd.join(dir),
        None => cwd.to_owned(),
    };
    dir.components().collect()
}

/// The screen on `feed`, with `select`'s detail open.
fn screen(
    feed: Box<dyn Feed>,
    select: Option<&str>,
    layout: Option<Layout>,
) -> Result<Tasks, String> {
    let mut screen = Tasks::new(feed);
    screen.initialize_layout(layout, &mut TextStore::new(Default::default()));
    if let Some(id) = select
        && !screen.select(id, &mut TextStore::new(Default::default()))
    {
        return Err(format!("no task {id:?}"));
    }
    Ok(screen)
}

fn render_snapshot(path: &Path, screen: &mut dyn Screen, size: (u32, u32)) -> Result<(), String> {
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

/// The feed of a window whose live feed could not start: the reason, once.
struct Unavailable(Option<View>);

impl Unavailable {
    fn new(repo: PathBuf, why: String) -> Unavailable {
        Unavailable(Some(View {
            repo,
            tasks: Vec::new(),
            runs: Vec::new(),
            github: GitHubView::default(),
            agents: Some(Vec::new()),
            notice: Some(why),
            now: 0,
        }))
    }
}

impl Feed for Unavailable {
    fn poll(&mut self) -> Option<View> {
        self.0.take()
    }

    fn act(&mut self, _intent: Intent) {}
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

    if let Some(path) = &args.snapshot {
        let mut screen = screen(
            Box::<DemoFeed>::default(),
            args.select.as_deref(),
            args.layout,
        )?;
        render_snapshot(path, &mut screen, args.size.unwrap_or(DEFAULT_SIZE))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let repo = if args.demo {
        None
    } else {
        let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
        Some(repo_dir(args.repo.as_deref(), &cwd))
    };
    let select = args.select;
    let layout = args.layout;
    App::new(options(), move |setup| {
        let waker = setup.waker.clone();
        let ticker = setup.waker.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                ticker.wake();
            }
        });
        let feed: Box<dyn Feed> = match repo {
            None => Box::<DemoFeed>::default(),
            Some(repo) => {
                let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.wake());
                match LiveFeed::start(repo.clone(), wake) {
                    Ok(feed) => Box::new(feed),
                    Err(why) => Box::new(Unavailable::new(repo, why)),
                }
            }
        };
        let mut screen = Tasks::new(feed);
        screen.initialize_layout(layout, &mut TextStore::new(Default::default()));
        // Live tasks arrive after the window opens: the card opens once listed.
        if let Some(id) = &select {
            screen.select_when_listed(id, &mut TextStore::new(Default::default()));
        }
        Box::new(screen)
    })
    .run()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "Noa · タスク"
        );
        assert_eq!(
            title(env(&[("LANG", "en_US.UTF-8")]), || None),
            "Noa · Tasks"
        );
        assert_eq!(
            title(env(&[("LC_ALL", "en_GB"), ("LANG", "ja_JP")]), || None),
            "Noa · Tasks"
        );
        assert_eq!(title(env(&[]), || Some("ja-JP".into())), "Noa · タスク");
    }

    #[test]
    fn ac07_flags_parse_and_a_snapshot_needs_the_demo() {
        let err = args(&["--snapshot=x.png"]).unwrap_err();
        assert!(err.contains("--snapshot needs --demo"), "{err}");
        let err = args(&["--repo=/r", "--snapshot=x.png"]).unwrap_err();
        assert!(err.contains("--snapshot needs --demo"), "{err}");
        let ok = args(&[
            "--demo",
            "--select=#12",
            "--snapshot=x.png",
            "--size=800x600",
        ])
        .unwrap();
        assert!(ok.demo);
        assert_eq!(ok.select.as_deref(), Some("#12"));
        assert_eq!(ok.size, Some((800, 600)));
        assert_eq!(
            args(&["--repo=/r"]).unwrap().repo,
            Some(PathBuf::from("/r"))
        );
        assert!(args(&["--demo", "--repo=/r"]).is_err());
        assert!(args(&["--size=800x600"]).is_err());
        assert!(args(&["--size=10"]).is_err());
        assert!(args(&["--nope"]).is_err());
        assert_eq!(
            args(&["--layout=calendar"]).unwrap().layout,
            Some(Layout::Calendar)
        );
        assert_eq!(args(&[]).unwrap().layout, None);
        assert!(args(&["--layout=gantt"]).is_err());
    }

    #[test]
    fn the_repo_defaults_to_the_current_directory_and_is_made_absolute() {
        let cwd = Path::new("/home/me/src");
        assert_eq!(repo_dir(None, cwd), cwd);
        assert_eq!(repo_dir(Some(Path::new("/r")), cwd), Path::new("/r"));
        assert_eq!(repo_dir(Some(Path::new("noa")), cwd), cwd.join("noa"));
    }

    #[test]
    fn a_trailing_slash_on_the_repo_is_dropped() {
        let cwd = Path::new("/home/me/src");
        // Path equality ignores it: compare the bytes.
        assert_eq!(
            repo_dir(Some(Path::new("/src/noa/")), cwd).as_os_str(),
            "/src/noa"
        );
        assert_eq!(
            repo_dir(Some(Path::new("noa/")), cwd).as_os_str(),
            "/home/me/src/noa"
        );
        assert_eq!(repo_dir(None, Path::new("/home/me/src/")).as_os_str(), cwd);
    }

    #[test]
    fn select_names_a_demo_task_or_is_refused() {
        assert!(screen(Box::<DemoFeed>::default(), Some("#12"), Some(Layout::Board)).is_ok());
        assert!(screen(Box::<DemoFeed>::default(), Some("t-1"), Some(Layout::Board)).is_ok());
        assert!(
            screen(
                Box::<DemoFeed>::default(),
                Some("#9999"),
                Some(Layout::Board)
            )
            .is_err()
        );
    }
}
