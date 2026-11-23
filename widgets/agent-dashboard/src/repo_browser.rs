//! 差分を見る: starts the Repo Browser widget (`widgets/repo-browser`) on
//! a worker's worktree (`--worktree=PATH`), found the way Noa finds its
//! helper programs (`crates/noa-app/src/helper_program.rs`), from this
//! dashboard's own executable: beside it in `Noa.app`'s `Contents/Helpers`,
//! else in the repo browser crate's own `target` of the same checkout.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The repo browser's file name in `Noa.app`'s Helpers (where this
/// dashboard is `Noa Agent Dashboard`).
const BUNDLED: &str = "Noa Repo Browser";
/// The binary `cargo build` makes, and its crate's directory under
/// `widgets/`.
const CRATE_NAME: &str = "repo-browser";

/// What the notice line says when there is no repo browser to start.
pub const NOT_FOUND: &str =
    "Repo Browser が見つかりません（widgets/repo-browser をビルドしてください）";

/// What the demo and a snapshot have in place of the repo browser: nothing
/// is ever started, the notice says so.
pub struct Offline(pub &'static str);

impl Launcher for Offline {
    fn launch(&mut self, _cwd: &str) -> Result<(), String> {
        Err(self.0.to_owned())
    }
}

/// Starts the repo browser on a worktree; the tests give one of their own.
pub trait Launcher {
    /// Starts it on the worktree holding `cwd`. `Err` says why not, for
    /// the notice line.
    fn launch(&mut self, cwd: &str) -> Result<(), String>;
}

/// The repo browser found from this dashboard's executable, started as a
/// process of its own.
pub struct System;

impl Launcher for System {
    fn launch(&mut self, cwd: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|_| NOT_FOUND.to_owned())?;
        let program = locate(&exe).ok_or_else(|| NOT_FOUND.to_owned())?;
        worktree_exists(cwd)?;
        spawn(&program, cwd)
    }
}

/// The worktree may be gone since the dashboard learned of it (`close
/// --prune`): say so rather than start on nothing.
fn worktree_exists(cwd: &str) -> Result<(), String> {
    if Path::new(cwd).is_dir() {
        Ok(())
    } else {
        Err(format!("作業ディレクトリがありません: {cwd}"))
    }
}

/// Where the repo browser is for the dashboard executable `exe`:
/// - in a bundle, beside it: `Contents/Helpers/Noa Repo Browser`;
/// - a plain `repo-browser` beside it (one Cargo target directory shared
///   by both, as `scripts/bundle-macos.sh` builds);
/// - in a checkout, `widgets/repo-browser/target/<profile>/repo-browser`,
///   from `widgets/agent-dashboard/target/<profile>/agent-dashboard`: the
///   dashboard's own profile first, then the other.
///
/// Only a regular file counts (a link is followed, as the platform's
/// launcher would).
pub fn locate(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let bundled = dir.join(BUNDLED);
    let shared = dir.join(CRATE_NAME);
    let dev = dev_build_dirs(dir)
        .into_iter()
        .map(|dir| dir.join(CRATE_NAME));
    [bundled, shared]
        .into_iter()
        .chain(dev)
        .find(|path| path.is_file())
}

/// The repo browser's target directories in the checkout `exe_dir` (the
/// dashboard's `target/<profile>`) belongs to.
fn dev_build_dirs(exe_dir: &Path) -> Vec<PathBuf> {
    let Some(profile) = exe_dir.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let Some(widgets) = exe_dir
        .parent()
        .filter(|target| target.file_name().is_some_and(|name| name == "target"))
        .and_then(Path::parent)
        .and_then(Path::parent)
    else {
        return Vec::new();
    };
    let target = widgets.join(CRATE_NAME).join("target");
    let other = if profile == "release" {
        "debug"
    } else {
        "release"
    };
    vec![target.join(profile), target.join(other)]
}

/// The arguments the repo browser is started with: one, the path as it is
/// (spaces and all: no shell reads it).
pub fn args(cwd: &str) -> Vec<OsString> {
    let mut arg = OsString::from("--worktree=");
    arg.push(cwd);
    vec![arg]
}

/// Starts `program` on `cwd`, detached: no shell, in a process group of its
/// own (a Ctrl-C meant for the dashboard's terminal does not reach it), no
/// stdio, from the home directory. A thread waits for it, so it leaves no
/// zombie behind.
fn spawn(program: &Path, cwd: &str) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args(cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("Repo Browser を起動できません: {err}"))?;
    reap_when_done(child, &UNREAPED, |wait| {
        std::thread::Builder::new()
            .name("repo-browser-reaper".into())
            .spawn(wait)
            .map(drop)
    });
    Ok(())
}

/// Children no thread could be given to wait for (the thread could not be
/// made): they are `try_wait`ed for at the next start, so they do not stay
/// zombies for the dashboard's whole life.
static UNREAPED: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// Has `start_thread` run a wait for `child`; when it cannot, `child` goes
/// to `leftovers` (after reaping those already done there) rather than
/// being dropped unwaited.
fn reap_when_done(
    child: Child,
    leftovers: &Mutex<Vec<Child>>,
    start_thread: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) {
    let shared = Arc::new(Mutex::new(Some(child)));
    let handed = Arc::clone(&shared);
    let started = start_thread(Box::new(move || {
        let child = handed.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child {
            let _ = child.wait();
        }
    }));
    let mut leftovers = leftovers.lock().unwrap_or_else(|e| e.into_inner());
    leftovers.retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
    if started.is_err() {
        let child = shared.lock().ok().and_then(|mut slot| slot.take());
        leftovers.extend(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-dashboard-repo-browser-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn in_a_bundle_it_is_beside_the_dashboard_in_helpers() {
        let root = scratch("bundle");
        let helpers = root.join("Noa.app/Contents/Helpers");
        let exe = helpers.join("Noa Agent Dashboard");
        touch(&exe);
        assert_eq!(locate(&exe), None, "not bundled yet");
        touch(&helpers.join(BUNDLED));
        assert_eq!(locate(&exe), Some(helpers.join(BUNDLED)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn in_a_checkout_it_is_the_repo_browsers_own_build_the_same_profile_first() {
        let root = scratch("dev");
        let widgets = root.join("widgets");
        let exe = widgets.join("agent-dashboard/target/release/agent-dashboard");
        touch(&exe);
        let debug = widgets.join("repo-browser/target/debug/repo-browser");
        let release = widgets.join("repo-browser/target/release/repo-browser");
        assert_eq!(locate(&exe), None, "not built");
        touch(&debug);
        assert_eq!(locate(&exe), Some(debug.clone()), "the other profile");
        touch(&release);
        assert_eq!(locate(&exe), Some(release.clone()), "its own profile first");
        let debug_exe = widgets.join("agent-dashboard/target/debug/agent-dashboard");
        touch(&debug_exe);
        assert_eq!(locate(&debug_exe), Some(debug));
        // A directory under that name is not a program.
        std::fs::remove_file(&release).unwrap();
        std::fs::create_dir(&release).unwrap();
        assert_eq!(
            locate(&exe),
            Some(widgets.join("repo-browser/target/debug/repo-browser"))
        );
        // Outside any `target`: no checkout to look in.
        let loose = root.join("bin/agent-dashboard");
        touch(&loose);
        assert_eq!(locate(&loose), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_path_with_spaces_is_one_argument() {
        assert_eq!(
            args("/Users/me/My Projects/noa wt/feat x"),
            [OsString::from(
                "--worktree=/Users/me/My Projects/noa wt/feat x"
            )]
        );
    }

    /// A real start, no shell between: a script stands in for the repo
    /// browser and writes down its arguments.
    #[test]
    fn the_program_is_started_with_its_argument_and_no_shell() {
        let root = scratch("spawn");
        let program = root.join("fake repo browser");
        let out = root.join("argv.txt");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        spawn(&program, "/w/a b; rm -rf x").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !out.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let written = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(written, "--worktree=/w/a b; rm -rf x\n");
    }

    #[test]
    fn a_program_that_cannot_start_says_why() {
        let root = scratch("broken");
        let program = root.join("not executable");
        std::fs::write(&program, "").unwrap();
        let err = spawn(&program, "/w").unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(err.starts_with("Repo Browser を起動できません"), "{err}");
    }

    #[test]
    fn a_plain_repo_browser_beside_a_shared_target_is_found_after_the_bundle_name() {
        let root = scratch("shared");
        let dir = root.join("target/release");
        let exe = dir.join("agent-dashboard");
        touch(&exe);
        assert_eq!(locate(&exe), None);
        touch(&dir.join(CRATE_NAME));
        assert_eq!(locate(&exe), Some(dir.join(CRATE_NAME)));
        touch(&dir.join(BUNDLED));
        assert_eq!(locate(&exe), Some(dir.join(BUNDLED)), "bundle name first");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_worktree_that_is_gone_is_said_not_started() {
        let err = worktree_exists("/definitely/not/a/worktree-agent-dashboard-test").unwrap_err();
        assert_eq!(
            worktree_exists(&std::env::temp_dir().to_string_lossy()),
            Ok(())
        );
        assert!(err.starts_with("作業ディレクトリがありません"), "{err}");
    }

    /// The repo browser's own parser (another crate: its source is read)
    /// takes the option this dashboard builds.
    #[test]
    fn the_argument_is_one_the_repo_browser_parses() {
        let arg = args("/w");
        let arg = arg[0].to_str().unwrap();
        let (option, _) = arg.split_once('=').unwrap();
        let parser = include_str!("../../repo-browser/src/main.rs");
        assert!(
            parser.contains(&format!("strip_prefix(\"{option}=\")")),
            "repo-browser does not parse {option}="
        );
    }

    fn short_lived() -> Child {
        Command::new("true").spawn().unwrap()
    }

    #[test]
    fn a_child_whose_reaper_thread_cannot_start_is_kept_and_reaped_later() {
        let leftovers = Mutex::new(Vec::new());
        reap_when_done(short_lived(), &leftovers, |_| {
            Err(std::io::Error::other("no threads"))
        });
        assert_eq!(leftovers.lock().unwrap().len(), 1, "kept, not dropped");
        std::thread::sleep(std::time::Duration::from_millis(200));
        // The next start sweeps the finished one.
        reap_when_done(short_lived(), &leftovers, |wait| {
            wait();
            Ok(())
        });
        assert!(leftovers.lock().unwrap().is_empty(), "reaped");
    }

    #[test]
    fn a_reaper_thread_that_starts_takes_the_child() {
        let leftovers = Mutex::new(Vec::new());
        reap_when_done(short_lived(), &leftovers, |wait| {
            wait();
            Ok(())
        });
        assert!(leftovers.lock().unwrap().is_empty());
    }
}
