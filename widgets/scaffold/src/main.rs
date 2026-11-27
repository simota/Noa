//! `scaffold`: the Noa window for a repository's development assets and the
//! widget-registration gaps found in it (`docs/specs/scaffold-widget.md`).
//! Flags, window title, the headless snapshot and the app.

mod demo;
mod ui;

use std::path::{Path, PathBuf};
use std::time::Duration;

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use ui::{Live, Scaffold};

const USAGE: &str =
    "Scaffold — see a project's setup, register its development assets, and plan the next step.

usage: scaffold [DIR | --repo DIR] [--demo]
       scaffold --demo --snapshot FILE.png [--size WxH]

  DIR, --repo DIR    Project directory (default: current directory; finds the Git root)
  --demo            Explore sample data without reading or changing a project
  --snapshot FILE   Save the demo as a PNG without opening a window (requires --demo)
  --size WxH        Snapshot size in logical pixels (default: 1280x860)
  -h, --help        Show this help

Options also accept --name=value. Opening a project only reads its files;
asset registration and task creation require an explicit action in the window.

examples:
  scaffold .
  scaffold --repo ~/src/my-project
  scaffold --demo";
const DEFAULT_SIZE: (u32, u32) = (1280, 860);
const SNAPSHOT_SCALE: f32 = 2.0;
/// How often the window wakes on its own, to rescan.
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
        Some(tag) if tag.starts_with("ja") => "Noa · 足場",
        _ => "Noa · Scaffold",
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
        "Noa · Scaffold"
    }
}

fn options() -> AppOptions {
    AppOptions {
        title: window_title().into(),
        size: (DEFAULT_SIZE.0 as f64, DEFAULT_SIZE.1 as f64),
        min_size: Some((960.0, 600.0)),
        ..AppOptions::new("Noa Scaffold")
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    demo: bool,
    repo: Option<PathBuf>,
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
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            a.help = true;
            return Ok(a);
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        let mut value = || -> Result<String, String> {
            let value = inline.map(str::to_owned).or_else(|| args.next());
            value
                .filter(|v| !v.is_empty() && (inline.is_some() || !v.starts_with('-')))
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        if arg == "--demo" {
            a.demo = true;
        } else if name == "--repo" {
            if a.repo.is_some() {
                return Err(format!("choose one project directory\n{USAGE}"));
            }
            a.repo = Some(PathBuf::from(value()?));
        } else if name == "--snapshot" {
            a.snapshot = Some(PathBuf::from(value()?));
        } else if name == "--size" {
            a.size = Some(parse_size(&value()?)?);
        } else if !arg.starts_with('-') {
            if a.repo.is_some() || arg.is_empty() {
                return Err(format!("choose one project directory\n{USAGE}"));
            }
            a.repo = Some(PathBuf::from(arg));
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    if a.demo && a.repo.is_some() {
        return Err(format!("--demo and --repo do not go together\n{USAGE}"));
    }
    // A snapshot never reads a repository or the data folder.
    if a.snapshot.is_some() && !a.demo {
        return Err(format!("--snapshot needs --demo\n{USAGE}"));
    }
    if a.size.is_some() && a.snapshot.is_none() {
        return Err(format!("--size goes with --snapshot\n{USAGE}"));
    }
    Ok(a)
}

/// `--repo`, made absolute against `cwd`; `cwd` itself without one. A
/// trailing slash is dropped.
fn repo_dir(repo: Option<&Path>, cwd: &Path) -> PathBuf {
    let dir = match repo {
        Some(dir) if dir.is_absolute() => dir.to_owned(),
        Some(dir) => cwd.join(dir),
        None => cwd.to_owned(),
    };
    dir.components().collect()
}

fn validate_repo(repo: &Path) -> Result<(), String> {
    match std::fs::metadata(repo) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "{} is not a directory; choose a project folder",
            repo.display()
        )),
        Err(e) => Err(format!(
            "cannot open project directory {}: {e}",
            repo.display()
        )),
    }
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

fn main() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }

    if let Some(path) = &args.snapshot {
        let mut screen = Scaffold::new(Box::new(demo::Demo));
        render_snapshot(path, &mut screen, args.size.unwrap_or(DEFAULT_SIZE))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let repo = if args.demo {
        None
    } else {
        let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
        let repo = repo_dir(args.repo.as_deref(), &cwd);
        validate_repo(&repo)?;
        Some(repo)
    };
    App::new(options(), move |setup| {
        let ticker = setup.waker.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                ticker.wake();
            }
        });
        let screen = match repo {
            None => Scaffold::new(Box::new(demo::Demo)),
            Some(repo) => {
                let data = scaffold::store::data_dir();
                Scaffold::new(Box::new(Live::new(repo, data.as_deref())))
            }
        };
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
            "Noa · 足場"
        );
        assert_eq!(
            title(env(&[("LANG", "en_US.UTF-8")]), || None),
            "Noa · Scaffold"
        );
        assert_eq!(
            title(env(&[("LC_ALL", "en_GB"), ("LANG", "ja_JP")]), || None),
            "Noa · Scaffold"
        );
        assert_eq!(title(env(&[]), || Some("ja-JP".into())), "Noa · 足場");
        assert_eq!(title(env(&[]), || None), "Noa · Scaffold");
    }

    #[test]
    fn flags_parse_and_a_snapshot_needs_the_demo() {
        let err = args(&["--snapshot=x.png"]).unwrap_err();
        assert!(err.contains("--snapshot needs --demo"), "{err}");
        let err = args(&["--repo=/r", "--snapshot=x.png"]).unwrap_err();
        assert!(err.contains("--snapshot needs --demo"), "{err}");
        let ok = args(&["--demo", "--snapshot=x.png", "--size=800x600"]).unwrap();
        assert!(ok.demo);
        assert_eq!(ok.size, Some((800, 600)));
        assert_eq!(
            args(&["--repo=/r"]).unwrap().repo,
            Some(PathBuf::from("/r"))
        );
        assert!(args(&["--demo", "--repo=/r"]).is_err());
        assert!(args(&["--repo="]).is_err());
        assert!(args(&["--size=800x600"]).is_err());
        assert!(args(&["--size=10"]).is_err());
        assert!(args(&["--nope"]).is_err());
    }

    #[test]
    fn the_repo_defaults_to_the_current_directory_and_is_made_absolute() {
        let cwd = Path::new("/home/me/src");
        assert_eq!(repo_dir(None, cwd), cwd);
        assert_eq!(repo_dir(Some(Path::new("/r")), cwd), Path::new("/r"));
        assert_eq!(repo_dir(Some(Path::new("noa")), cwd), cwd.join("noa"));
        assert_eq!(
            repo_dir(Some(Path::new("/src/noa/")), cwd).as_os_str(),
            "/src/noa"
        );
    }

    #[test]
    fn common_cli_spellings_and_help_work_without_a_window() {
        assert_eq!(
            args(&["--repo", "my project"]).unwrap(),
            args(&["--repo=my project"]).unwrap()
        );
        assert_eq!(
            args(&["my project"]).unwrap(),
            args(&["--repo=my project"]).unwrap()
        );
        assert_eq!(
            args(&["--demo", "--snapshot", "out.png", "--size", "960x600"]).unwrap(),
            args(&["--demo", "--snapshot=out.png", "--size=960x600"]).unwrap()
        );
        assert!(args(&["--help"]).unwrap().help);
        for invalid in [
            vec!["--repo"],
            vec!["--repo", "--demo"],
            vec!["--repo=a", "b"],
            vec!["a", "--repo=b"],
            vec!["--snapshot="],
            vec!["--demo", "--snapshot"],
        ] {
            assert!(args(&invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn a_project_must_be_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_repo(dir.path()).is_ok());
        assert!(validate_repo(&dir.path().join("missing")).is_err());
        let file = dir.path().join("README.md");
        std::fs::write(&file, "hello").unwrap();
        assert!(
            validate_repo(&file)
                .unwrap_err()
                .contains("not a directory")
        );
    }
}
