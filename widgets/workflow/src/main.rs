//! The native helper accepts the same `--repo=` contract as Noa's other widgets.

#[path = "../../shared/appearance.rs"]
mod appearance;

mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use noa_gpui_platform::{App, AppOptions, Headless, Screen};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;
use workflow::runner::Channel;

const USAGE: &str = "Usage: workflow --repo=PATH [--task-origin=JSON | --ember-origin=JSON | --run=RUN_ID]\n       workflow --demo[=editor|approval|failed|saved] [--snapshot=PNG] [--size=1280x860]\n\nA native visual development workflow editor for Noa.\nTask and Ember handoffs open the start form; --run opens a saved run read-only.\nSnapshots and demos never start agents or publish changes.";

#[derive(Debug, PartialEq)]
struct Args {
    repo: Option<PathBuf>,
    demo: Option<String>,
    snapshot: Option<PathBuf>,
    size: (u32, u32),
    help: bool,
    task_origin: Option<PathBuf>,
    ember_origin: Option<PathBuf>,
    run: Option<String>,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        repo: None,
        demo: None,
        snapshot: None,
        size: (1280, 860),
        help: false,
        task_origin: None,
        ember_origin: None,
        run: None,
    };
    for arg in args {
        if matches!(arg.as_str(), "--help" | "-h") {
            parsed.help = true;
        } else if let Some(repo) = arg.strip_prefix("--repo=") {
            parsed.repo = Some(PathBuf::from(repo));
        } else if let Some(path) = arg.strip_prefix("--task-origin=") {
            parsed.task_origin = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--ember-origin=") {
            parsed.ember_origin = Some(PathBuf::from(path));
        } else if let Some(id) = arg.strip_prefix("--run=") {
            if !workflow::model::valid_id(id) {
                return Err("invalid run id".into());
            }
            parsed.run = Some(id.into());
        } else if arg == "--demo" {
            parsed.demo = Some("editor".into());
        } else if let Some(demo) = arg.strip_prefix("--demo=") {
            if !matches!(demo, "editor" | "approval" | "failed" | "saved") {
                return Err("unknown demo; use editor, approval, failed or saved".into());
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
    let modes = [
        parsed.task_origin.is_some(),
        parsed.ember_origin.is_some(),
        parsed.run.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if modes > 1
        || modes > 0 && parsed.repo.is_none()
        || [&parsed.task_origin, &parsed.ember_origin]
            .into_iter()
            .flatten()
            .any(|p| !p.is_absolute())
    {
        return Err("handoffs and saved runs require --repo, are mutually exclusive, and handoff paths must be absolute".into());
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
    if appearance::run_if_requested()? {
        return Ok(());
    }
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
    if let Some(id) = args.run {
        let run = workflow::runner::read_run(&repository, &id)?;
        return App::new(options, move |_| Box::new(ui::WorkflowScreen::saved(run)))
            .run()
            .map_err(|e| e.to_string());
    }
    let handoff = args
        .task_origin
        .as_ref()
        .map(|path| workflow::tasks::read_handoff(path, &repository))
        .transpose()?;
    let ember_handoff = args
        .ember_origin
        .as_ref()
        .map(|path| workflow::ember::read_handoff(path, &repository))
        .transpose()?;
    let retained: Arc<Mutex<Option<Channel>>> = Arc::new(Mutex::new(None));
    let holder = retained.clone();
    App::new(options, move |setup| {
        let waker = setup.waker.clone();
        let channel = if handoff.is_some() || ember_handoff.is_some() {
            Channel::open_deferred(repository.clone(), move || waker.wake())
        } else {
            Channel::open(repository.clone(), move || waker.wake())
        };
        match channel {
            Ok(channel) => {
                *holder.lock().expect("channel holder") = Some(channel.clone());
                let screen = ui::WorkflowScreen::live(repository, channel);
                Box::new(match handoff {
                    Some((origin, objective)) => screen.with_task(origin, objective),
                    None => match ember_handoff {
                        Some(objective) => screen.with_ember(objective),
                        None => screen,
                    },
                })
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
        assert!(args(&["--demo=saved", "--snapshot=out.png"]).is_ok());
        assert!(args(&["--demo", "--size=999999x1"]).is_err());
        assert!(args(&["--repo="]).is_err());
        assert!(args(&["--help"]).unwrap().help);
    }

    #[test]
    fn task_handoffs_and_saved_runs_cannot_be_combined_or_traverse_paths() {
        assert!(args(&["--repo=.", "--task-origin=/tmp/task.json"]).is_ok());
        assert!(args(&["--repo=.", "--run=run-123"]).is_ok());
        assert!(args(&["--repo=.", "--task-origin=task.json"]).is_err());
        assert!(args(&["--demo", "--task-origin=/tmp/task.json"]).is_err());
        assert!(args(&["--demo", "--run=run-123"]).is_err());
        assert!(args(&["--repo=.", "--run=run-123", "--task-origin=/tmp/task.json"]).is_err());
        assert!(args(&["--repo=.", "--run=../run-123"]).is_err());
    }

    #[test]
    fn ember_handoffs_are_explicit_and_never_combine_with_demo_or_resumption() {
        assert!(args(&["--repo=.", "--ember-origin=/tmp/ember.json"]).is_ok());
        assert!(args(&["--repo=.", "--ember-origin=ember.json"]).is_err());
        assert!(args(&["--demo", "--ember-origin=/tmp/ember.json"]).is_err());
        assert!(
            args(&[
                "--repo=.",
                "--run=run-123",
                "--ember-origin=/tmp/ember.json"
            ])
            .is_err()
        );
        assert!(
            args(&[
                "--repo=.",
                "--task-origin=/tmp/task.json",
                "--ember-origin=/tmp/ember.json"
            ])
            .is_err()
        );
    }
}
