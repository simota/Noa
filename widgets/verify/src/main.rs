//! The verify widget's binary: the noa-gpui window over the library's
//! capture, worktree, evidence, re-run, model, engine and store (`lib.rs`),
//! fed by the live layer.

#[path = "../../shared/appearance.rs"]
mod appearance;
mod demo;
mod installed;
mod live;
mod native_preview;
/// The live session runner (`src/live/`): `ipc` is the Noa port and its
/// `noa-ipc` implementation, `runner` drives one verification against it,
/// `fix` starts an agent on fixing a verification's failed criteria and
/// defects.
#[path = "live"]
mod session {
    pub mod fix;
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
use crate::ui::{CenterTab, Verify};

const USAGE: &str = "usage: verify [--demo | --demo-start | --demo-held | --demo-criteria | \
                     --session=DIR | --repo=DIR] \
                     [--tab=criteria|evidence|gallery|defects|report] [--criterion=ID] \
                     [--base=SHA --revision=SHA] [--origin-repo=DIR --origin-task=ID --origin-run=ID] \
                     [--snapshot=FILE.png [--size=WxH]]\n  \
                     the live verification by default; \
                     --repo puts DIR (absolute) in the start view's repository field \
                     (Noa passes its focused pane's); \
                     --demo shows a canned two-round verification, \
                     --demo-start the start view with canned past verifications, \
                     --demo-held the demo held after a round, \
                     --demo-criteria the criteria held for confirmation, \
                     --session the verification stored at DIR \
                     (<data>/noa/verifications/ver-*), read-only; \
                     --tab and --criterion pick the center tab (gallery: the evidence tab's \
                     ギャラリー) and the selected criterion";
const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SNAPSHOT_SCALE: f32 = 2.0;
const FIRST_SNAPSHOT_WAIT: Duration = Duration::from_secs(3);

/// The window title in the person's language: Japanese where the locale is
/// Japanese (`LC_ALL`, `LC_MESSAGES`, `LANG`, then the system's preferred
/// language), English anywhere else.
fn title(
    var: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> &'static str {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| var(key).filter(|value| !value.is_empty()))
        .or_else(system);
    match tag {
        Some(tag) if tag.starts_with("ja") => "Noa · 多角検証",
        _ => "Noa · Multi-Agent Verification",
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
        ..AppOptions::new("Noa Verify")
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
    let channel = live::start(|| {}, live::startup_link());
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

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    snapshot: Option<PathBuf>,
    size: Option<(u32, u32)>,
    demo: bool,
    demo_start: bool,
    demo_held: bool,
    demo_criteria: bool,
    session: Option<PathBuf>,
    /// What the start view's repository field holds.
    repo: Option<PathBuf>,
    base: Option<String>,
    revision: Option<String>,
    origin_repo: Option<PathBuf>,
    origin_task: Option<String>,
    origin_run: Option<String>,
    tab: Option<CenterTab>,
    criterion: Option<String>,
    help: bool,
}

impl Args {
    fn origin(&self) -> Option<verify::task_links::OriginHint> {
        Some(verify::task_links::OriginHint {
            repo: self.origin_repo.clone()?,
            base: self.base.clone()?,
            task: self.origin_task.clone()?,
            run: self.origin_run.clone()?,
            commit: self.revision.clone()?,
        })
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    for arg in args {
        if let Some(path) = arg.strip_prefix("--snapshot=") {
            out.snapshot = Some(PathBuf::from(path));
        } else if let Some(raw) = arg.strip_prefix("--size=") {
            out.size = Some(parse_size(raw)?);
        } else if let Some(dir) = arg.strip_prefix("--session=") {
            // The log's reply paths are absolute; so is what is checked
            // against them.
            out.session =
                Some(std::path::absolute(dir).map_err(|e| format!("--session={dir}: {e}"))?);
        } else if let Some(dir) = arg.strip_prefix("--repo=") {
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() {
                return Err(format!("--repo needs an absolute path\n{USAGE}"));
            }
            out.repo = Some(dir);
        } else if let Some(value) = arg.strip_prefix("--base=") {
            out.base = Some(value.into());
        } else if let Some(value) = arg.strip_prefix("--revision=") {
            out.revision = Some(value.into());
        } else if let Some(value) = arg.strip_prefix("--origin-repo=") {
            out.origin_repo = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--origin-task=") {
            out.origin_task = Some(value.into());
        } else if let Some(value) = arg.strip_prefix("--origin-run=") {
            out.origin_run = Some(value.into());
        } else if let Some(tab) = arg.strip_prefix("--tab=") {
            out.tab =
                Some(CenterTab::parse(tab).ok_or_else(|| format!("unknown tab {tab:?}\n{USAGE}"))?);
        } else if let Some(id) = arg.strip_prefix("--criterion=") {
            out.criterion = Some(id.to_owned());
        } else if arg == "--demo" {
            out.demo = true;
        } else if arg == "--demo-start" {
            out.demo_start = true;
        } else if arg == "--demo-held" {
            out.demo_held = true;
        } else if arg == "--demo-criteria" {
            out.demo_criteria = true;
        } else if arg == "--help" || arg == "-h" {
            out.help = true;
        } else {
            return Err(format!("unknown option {arg:?}\n{USAGE}"));
        }
    }
    let sources = [
        out.demo,
        out.demo_start,
        out.demo_held,
        out.demo_criteria,
        out.session.is_some(),
    ]
    .iter()
    .filter(|on| **on)
    .count();
    if sources > 1 {
        return Err(format!(
            "--demo, --demo-start, --demo-held, --demo-criteria and --session do not go together\n{USAGE}"
        ));
    }
    if sources > 0 && out.repo.is_some() {
        return Err(format!(
            "--repo fills the live start view; it does not go with --demo, --demo-start, \
             --demo-held, --demo-criteria or --session\n{USAGE}"
        ));
    }
    let target = out.base.is_some() || out.revision.is_some();
    let origin = out.origin_repo.is_some() || out.origin_task.is_some() || out.origin_run.is_some();
    let sha = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|v| matches!(v.len(), 40 | 64) && v.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if target && (out.repo.is_none() || sources > 0 || !sha(&out.base) || !sha(&out.revision)) {
        return Err("--base and --revision require full commit IDs and a live --repo".into());
    }
    if origin
        && (!target
            || !out.origin_repo.as_ref().is_some_and(|p| p.is_absolute())
            || !out.origin_task.as_ref().is_some_and(|s| !s.is_empty())
            || !out.origin_run.as_ref().is_some_and(|s| !s.is_empty()))
    {
        return Err("a task handoff requires --origin-repo, --origin-task, --origin-run, --base and --revision".into());
    }
    Ok(out)
}

/// The live window's first screen: with `--session`, the stored
/// verification read-only, offering nothing that acts on Noa; else the
/// start view, its repository field holding `--repo`'s folder.
fn live_screen(
    args: &Args,
    channel: live::Channel,
    installed: std::sync::mpsc::Receiver<installed::Installed>,
) -> Verify {
    match (&args.session, &args.repo) {
        (Some(dir), _) => Verify::session_window(channel, installed, dir),
        (None, Some(repo)) => Verify::live(channel, installed).with_place(repo),
        (None, None) => Verify::live(channel, installed),
    }
}

fn main() -> Result<(), String> {
    if native_preview::run_if_requested()? {
        return Ok(());
    }
    if appearance::run_if_requested()? {
        return Ok(());
    }
    let args = parse_args(std::env::args().skip(1))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }
    let size = args.size.unwrap_or(DEFAULT_SIZE);
    if let Some(dir) = &args.session {
        // Fails here, before a window opens, on a log that cannot be read.
        verify::store::read_session(dir)
            .map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
    }
    let (tab, criterion) = (args.tab, args.criterion.clone());

    if let Some(path) = &args.snapshot {
        let screen = if args.demo {
            Verify::demo()
        } else if args.demo_start {
            Verify::demo_start()
        } else if args.demo_held {
            Verify::demo_held()
        } else if args.demo_criteria {
            Verify::demo_criteria()
        } else if let Some(dir) = &args.session {
            Verify::stored(dir)?
        } else {
            Verify::of_snapshot(first_live_snapshot()?)
        };
        let screen = match &args.repo {
            Some(repo) => screen.with_place(repo),
            None => screen,
        };
        let mut screen = screen
            .with_handoff(
                args.base.as_deref(),
                args.revision.as_deref(),
                args.origin(),
            )
            .with_view(tab, criterion);
        render_snapshot(path, &mut screen, size)?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    App::new(options(), move |setup| {
        let screen = if args.demo {
            Verify::demo()
        } else if args.demo_start {
            Verify::demo_start()
        } else if args.demo_held {
            Verify::demo_held()
        } else if args.demo_criteria {
            Verify::demo_criteria()
        } else {
            let waker = setup.waker.clone();
            let channel = live::start(move || waker.wake(), live::startup_link());
            let waker = setup.waker.clone();
            let installed = installed::spawn(move || waker.wake());
            live_screen(&args, channel, installed)
        };
        Box::new(
            screen
                .with_handoff(
                    args.base.as_deref(),
                    args.revision.as_deref(),
                    args.origin(),
                )
                .with_view(tab, criterion.clone()),
        )
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
        let ja = "Noa · 多角検証";
        let en = "Noa · Multi-Agent Verification";
        assert_eq!(title(env(&[("LANG", "ja_JP.UTF-8")]), || None), ja);
        assert_eq!(title(env(&[("LANG", "en_US.UTF-8")]), || None), en);
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
        assert_eq!(title(env(&[]), || Some("ja-JP".into())), ja);
        assert_eq!(title(env(&[]), || None), en);
    }

    #[test]
    fn sizes_parse_correctly() {
        assert_eq!(parse_size("1280x800"), Ok((1280, 800)));
        assert!(parse_size("0x800").is_err());
        assert!(parse_size("1280").is_err());
    }

    #[test]
    fn the_flags_parse_and_sources_exclude_each_other() {
        let args = |list: &[&str]| parse_args(list.iter().map(|s| s.to_string()));
        let a = args(&[
            "--demo",
            "--tab=gallery",
            "--criterion=AC-2",
            "--snapshot=/tmp/x.png",
        ])
        .unwrap();
        assert!(a.demo);
        assert_eq!(a.tab, Some(CenterTab::Gallery));
        assert_eq!(a.criterion.as_deref(), Some("AC-2"));
        assert!(args(&["--tab=nope"]).is_err());
        assert!(args(&["--demo", "--demo-start"]).is_err());
        assert!(args(&["--demo-held", "--session=/x"]).is_err());
        assert!(args(&["--demo-criteria", "--demo"]).is_err());
        assert!(args(&["--what"]).is_err());
    }

    #[test]
    fn a_session_window_offers_nothing_that_acts_on_noa() {
        use crate::installed::Installed;
        use crate::live::{Command, OpenPanes};
        use crate::ui::Action;
        use noa_gpui_platform::Screen as _;

        let root = std::env::temp_dir().join(format!("noa-verify-main-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(demo::DONE_ID);
        let log = demo::log();
        let mut store = verify::store::Store::create(&dir, &log[0]).unwrap();
        store.append_all(&log[1..]).unwrap();
        drop(store);
        let open = |session: bool| {
            let (channel, link) = live::Channel::fake();
            let (tx, installed) = std::sync::mpsc::channel();
            tx.send(Installed::Found(
                verify::model::types::AgentKind::ALL.to_vec(),
            ))
            .unwrap();
            let mut screen = if session {
                let args = parse_args([format!("--session={}", dir.display())]).unwrap();
                live_screen(&args, channel, installed)
            } else {
                // The same verification opened from the live window's list.
                Verify::live_stored(channel, installed, &dir)
            };
            let mut text = TextStore::new(Default::default());
            screen.woken(&mut text);
            let asked: Vec<Command> = link.commands.try_iter().map(|(_, c)| c).collect();
            // Noa says three of its panes are open.
            link.panes
                .send(OpenPanes {
                    generation: 1,
                    dir: dir.clone(),
                    count: 3,
                })
                .unwrap();
            screen.woken(&mut text);
            (asked, screen.offered())
        };
        let acts_on_noa = |a: &Action| {
            matches!(
                a,
                Action::ClosePanes
                    | Action::ResumeShown
                    | Action::Extend(_)
                    | Action::FocusPane(_)
                    | Action::RequestFix { .. }
            )
        };
        let (asked, offered) = open(true);
        assert!(
            !asked
                .iter()
                .any(|c| matches!(c, Command::ProbePanes { .. })),
            "{asked:?}"
        );
        let noa: Vec<&Action> = offered.iter().filter(|a| acts_on_noa(a)).collect();
        assert!(noa.is_empty(), "{noa:?}");
        // The live window's own stored view still offers them.
        let (asked, offered) = open(false);
        assert!(asked.contains(&Command::ProbePanes { dir: dir.clone() }));
        assert!(offered.contains(&Action::ClosePanes));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repo_takes_an_absolute_dir_and_only_for_the_live_view() {
        let args = |list: &[&str]| parse_args(list.iter().map(|s| s.to_string()));
        assert_eq!(
            args(&["--repo=/Users/me/src/noa"]).unwrap().repo,
            Some(PathBuf::from("/Users/me/src/noa"))
        );
        assert!(args(&["--repo=src/noa"]).is_err());
        assert!(args(&["--repo="]).is_err());
        for source in [
            "--demo",
            "--demo-start",
            "--demo-held",
            "--demo-criteria",
            "--session=/x",
        ] {
            assert!(args(&["--repo=/r", source]).is_err(), "{source}");
        }
    }
}

#[cfg(test)]
mod task_handoff_args_tests {
    use super::*;
    fn handoff() -> Vec<String> {
        vec![
            "--repo=/tmp/repo".into(),
            format!("--base={}", "a".repeat(40)),
            format!("--revision={}", "b".repeat(40)),
            "--origin-repo=/tmp/repo".into(),
            "--origin-task=#12".into(),
            "--origin-run=run-5".into(),
        ]
    }
    #[test]
    fn immutable_target_and_origin_are_parsed_together() {
        let args = parse_args(handoff()).unwrap();
        let origin = args.origin().unwrap();
        assert_eq!(origin.task, "#12");
        assert_eq!(origin.run, "run-5");
        assert_eq!(origin.commit, "b".repeat(40));
    }
    #[test]
    fn incomplete_or_mutable_handoffs_are_rejected() {
        for missing in 0..6 {
            let mut args = handoff();
            args.remove(missing);
            assert!(parse_args(args).is_err(), "missing {missing}");
        }
        let mut args = handoff();
        args[2] = "--revision=HEAD".into();
        assert!(parse_args(args).is_err());
        let mut args = handoff();
        args.push("--demo".into());
        assert!(parse_args(args).is_err());
    }
}
