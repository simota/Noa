//! The native helper accepts the same `--repo=` contract as Noa's other widgets.

mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;
use workflow::runner::Channel;

const USAGE: &str = "Usage: workflow --repo=PATH\n       workflow --demo[=editor|approval|failed] [--snapshot=PNG] [--size=1280x860]\n\nA native visual development workflow editor for Noa.\nSnapshots and demos never start agents or publish changes.";

#[derive(Debug, PartialEq)]
struct Args {
    repo: Option<PathBuf>,
    demo: Option<String>,
    snapshot: Option<PathBuf>,
    size: (u32, u32),
    help: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        repo: None,
        demo: None,
        snapshot: None,
        size: (1280, 860),
        help: false,
    };
    for arg in args {
        if matches!(arg.as_str(), "--help" | "-h") {
            parsed.help = true;
        } else if let Some(repo) = arg.strip_prefix("--repo=") {
            parsed.repo = Some(PathBuf::from(repo));
        } else if arg == "--demo" {
            parsed.demo = Some("editor".into());
        } else if let Some(demo) = arg.strip_prefix("--demo=") {
            if !matches!(demo, "editor" | "approval" | "failed") {
                return Err("unknown demo; use editor, approval or failed".into());
            }
            parsed.demo = Some(demo.into());
        } else if let Some(path) = arg.strip_prefix("--snapshot=") {
            parsed.snapshot = Some(PathBuf::from(path));
        } else if let Some(size) = arg.strip_prefix("--size=") {
            let (w, h) = size.split_once('x').ok_or("--size requires WIDTHxHEIGHT")?;
            parsed.size = (
                w.parse().map_err(|_| "invalid width")?,
                h.parse().map_err(|_| "invalid height")?,
            );
            if !(720..=3840).contains(&parsed.size.0) || !(600..=2160).contains(&parsed.size.1) {
                return Err("size must be between 720x600 and 3840x2160".into());
            }
        } else {
            return Err(format!("unknown argument: {arg}\n{USAGE}"));
        }
    }
    if parsed.help {
        return Ok(parsed);
    }
    if parsed.repo.is_some() == parsed.demo.is_some() {
        return Err("specify exactly one of --repo=PATH or --demo".into());
    }
    if parsed
        .repo
        .as_ref()
        .is_some_and(|p| p.as_os_str().is_empty())
    {
        return Err("--repo cannot be empty".into());
    }
    if parsed.snapshot.is_some() && parsed.demo.is_none() {
        return Err(
            "--snapshot requires --demo; live repositories are never executed for a screenshot"
                .into(),
        );
    }
    Ok(parsed)
}

fn main() -> Result<(), String> {
    let args = parse(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }
    let options = AppOptions {
        title: "Noa · Workflow".into(),
        size: (args.size.0 as f64, args.size.1 as f64),
        min_size: Some((720.0, 600.0)),
        ..AppOptions::new("Noa Workflow")
    };
    if let Some(demo) = args.demo {
        let mut screen = ui::WorkflowScreen::demo(&demo);
        if let Some(path) = args.snapshot {
            let mut renderer = Headless::software();
            let mut text = TextStore::new(Default::default());
            screen.prepare(Size::new(args.size.0 as f32, args.size.1 as f32), &mut text);
            renderer
                .render_screen(&mut screen, &mut text, args.size, 2.0)
                .map_err(|e| e.to_string())?
                .save_png(&path)
                .map_err(|e| e.to_string())?;
            println!("wrote {}", path.display());
            return Ok(());
        }
        return App::new(options, move |_| Box::new(screen))
            .run()
            .map_err(|e| e.to_string());
    }
    let repository = args.repo.ok_or("repository missing")?;
    let retained: Arc<Mutex<Option<Channel>>> = Arc::new(Mutex::new(None));
    let holder = retained.clone();
    App::new(options, move |setup| {
        let waker = setup.waker.clone();
        match Channel::open(repository.clone(), move || waker.wake()) {
            Ok(channel) => {
                *holder.lock().expect("channel holder") = Some(channel.clone());
                Box::new(ui::WorkflowScreen::live(repository, channel))
            }
            Err(error) => Box::new(ui::WorkflowScreen::unavailable(repository, error)),
        }
    })
    .run()
    .map_err(|e| e.to_string())?;

    // Closing the window does not strand an already-running autonomous phase.
    // Stop at the next human decision or terminal state; a later window reopens
    // the durable run and reconciles agent identities before continuing.
    let channel = retained
        .lock()
        .map_err(|_| "channel holder unavailable")?
        .take();
    if let Some(channel) = channel {
        channel.finish_autonomous()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(input: &[&str]) -> Result<Args, String> {
        parse(input.iter().map(|s| s.to_string()))
    }
    #[test]
    fn snapshots_never_open_a_live_repository() {
        assert!(args(&["--repo=.", "--snapshot=out.png"]).is_err());
        assert!(args(&["--repo=.", "--demo"]).is_err());
        assert!(args(&["--demo=approval", "--snapshot=out.png", "--size=1280x860"]).is_ok());
        assert!(args(&["--demo", "--size=999999x1"]).is_err());
        assert!(args(&["--repo="]).is_err());
        assert!(args(&["--help"]).unwrap().help);
    }
}
