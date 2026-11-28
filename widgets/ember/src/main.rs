//! Ember's native window is a client of the durable, separately owned service.

#[path = "../../shared/appearance.rs"]
mod appearance;
mod demo;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

const DEFAULT_SIZE: (u32, u32) = (1120, 780);
const MIN_SIZE: (u32, u32) = (720, 640);
const USAGE: &str = "Usage: ember [--repo=PATH]\n       ember --demo[=conversation|requests|memory|settings|error] [--snapshot=PNG] [--size=1120x780]\n\nA personal assistant window for Noa.\nDemos and snapshots never start the service, read projects, or call a provider.\n--widget-settings opens the shared widget appearance settings.";

#[derive(Debug, PartialEq)]
struct Args {
    repo: Option<PathBuf>,
    demo: Option<String>,
    snapshot: Option<PathBuf>,
    size: (u32, u32),
    service: bool,
    data_dir: Option<PathBuf>,
    help: bool,
}

fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        repo: None,
        demo: None,
        snapshot: None,
        size: DEFAULT_SIZE,
        service: false,
        data_dir: None,
        help: false,
    };
    let mut size_set = false;
    for argument in arguments {
        if matches!(argument.as_str(), "--help" | "-h") {
            parsed.help = true;
        } else if let Some(path) = argument.strip_prefix("--repo=") {
            if path.is_empty() || parsed.repo.is_some() {
                return Err("--repo requires one nonempty path".into());
            }
            parsed.repo = Some(PathBuf::from(path));
        } else if argument == "--demo" || argument.starts_with("--demo=") {
            if parsed.demo.is_some() {
                return Err("--demo may be specified once".into());
            }
            let kind = argument.strip_prefix("--demo=").unwrap_or("conversation");
            if !demo::KINDS.contains(&kind) {
                return Err(
                    "unknown demo; use conversation, requests, memory, settings or error".into(),
                );
            }
            parsed.demo = Some(kind.into());
        } else if let Some(path) = argument.strip_prefix("--snapshot=") {
            if path.is_empty() || parsed.snapshot.is_some() {
                return Err("--snapshot requires one nonempty path".into());
            }
            parsed.snapshot = Some(PathBuf::from(path));
        } else if let Some(size) = argument.strip_prefix("--size=") {
            if size_set {
                return Err("--size may be specified once".into());
            }
            let (width, height) = size.split_once('x').ok_or("--size requires WIDTHxHEIGHT")?;
            parsed.size = (
                width.parse().map_err(|_| "invalid width")?,
                height.parse().map_err(|_| "invalid height")?,
            );
            if !(MIN_SIZE.0..=3840).contains(&parsed.size.0)
                || !(MIN_SIZE.1..=2160).contains(&parsed.size.1)
            {
                return Err("size must be between 720x640 and 3840x2160".into());
            }
            size_set = true;
        } else if argument == "--service" {
            if parsed.service {
                return Err("--service may be specified once".into());
            }
            parsed.service = true;
        } else if let Some(path) = argument.strip_prefix("--data-dir=") {
            if path.is_empty() || parsed.data_dir.is_some() || !PathBuf::from(path).is_absolute() {
                return Err("--data-dir requires one absolute path".into());
            }
            parsed.data_dir = Some(PathBuf::from(path));
        } else {
            return Err(format!("unknown argument: {argument}\n{USAGE}"));
        }
    }
    if parsed.help {
        return Ok(parsed);
    }
    if parsed.demo.is_some() && parsed.repo.is_some() {
        return Err("--demo cannot read a live --repo".into());
    }
    if parsed.snapshot.is_some() && parsed.demo.is_none() {
        return Err("--snapshot requires --demo".into());
    }
    if size_set && parsed.demo.is_none() {
        return Err("--size requires --demo".into());
    }
    if parsed.service
        && (parsed.repo.is_some() || parsed.demo.is_some() || parsed.snapshot.is_some() || size_set)
    {
        return Err("--service cannot be combined with window or preview options".into());
    }
    if parsed.service != parsed.data_dir.is_some() {
        return Err(
            "internal --service mode requires --data-dir, and only service mode accepts it".into(),
        );
    }
    Ok(parsed)
}

fn main() -> Result<(), String> {
    if appearance::run_if_requested()? {
        return Ok(());
    }
    let args = parse(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }
    if args.service {
        let root = args.data_dir.ok_or("service data directory missing")?;
        return ember::service::run(root, Arc::new(ember::backend::OpenAiBackend::new()));
    }
    let options = AppOptions {
        title: "Noa · エンバー".into(),
        size: (args.size.0 as f64, args.size.1 as f64),
        min_size: Some((MIN_SIZE.0 as f64, MIN_SIZE.1 as f64)),
        ..AppOptions::new("Noa Ember")
    };
    if let Some(kind) = args.demo {
        let mut screen = ui::EmberScreen::demo(&kind);
        if let Some(path) = args.snapshot {
            let mut renderer = Headless::software();
            let mut text = TextStore::new(Default::default());
            screen.prepare(Size::new(args.size.0 as f32, args.size.1 as f32), &mut text);
            renderer
                .render_screen(&mut screen, &mut text, args.size, 2.0)
                .map_err(|error| error.to_string())?
                .save_png(&path)
                .map_err(|error| error.to_string())?;
            println!("wrote {}", path.display());
            return Ok(());
        }
        return App::new(options, move |_| Box::new(screen))
            .run()
            .map_err(|error| error.to_string());
    }
    let root = ember::client::default_root()?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    App::new(options, move |setup| {
        let waker = setup.waker.clone();
        Box::new(ui::EmberScreen::live(
            root,
            executable,
            args.repo,
            Arc::new(move || waker.wake()),
        ))
    })
    .run()
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<Args, String> {
        parse(values.iter().map(|value| (*value).into()))
    }

    #[test]
    fn projectless_startup_is_supported() {
        assert!(args(&[]).unwrap().repo.is_none());
        assert!(args(&["--repo=/work/noa"]).is_ok());
        assert!(args(&["--repo="]).is_err());
    }

    #[test]
    fn every_offline_mode_rejects_live_inputs_before_initialization() {
        for kind in demo::KINDS {
            assert!(args(&[&format!("--demo={kind}"), "--snapshot=x.png"]).is_ok());
            assert!(args(&[&format!("--demo={kind}"), "--repo=/work"]).is_err());
            assert!(args(&[&format!("--demo={kind}"), "--service", "--data-dir=/data"]).is_err());
        }
        assert!(args(&["--snapshot=x.png"]).is_err());
        assert!(args(&["--demo=unknown"]).is_err());
        assert!(args(&["--demo", "--size=720x640"]).is_ok());
        assert!(args(&["--demo", "--size=719x640"]).is_err());
        assert!(args(&["--demo", "--size=99999x99999"]).is_err());
    }

    #[test]
    fn internal_service_arguments_do_not_accept_window_actions() {
        assert!(args(&["--service", "--data-dir=/data/ember"]).is_ok());
        assert!(args(&["--service"]).is_err());
        assert!(args(&["--data-dir=/data/ember"]).is_err());
        assert!(args(&["--service", "--data-dir=relative"]).is_err());
        assert!(args(&["--service", "--data-dir=/data", "--repo=/work"]).is_err());
        assert!(args(&["--service", "--data-dir=/data", "--snapshot=x.png"]).is_err());
    }
}
