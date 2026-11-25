//! Starting another widget (Review, Verify, Tasks) as a process of its own,
//! found the way Noa finds its helper programs
//! (`crates/noa-app/src/helper_program.rs`), from this widget's own
//! executable: beside it in `Noa.app`'s `Contents/Helpers`, beside it in a
//! shared Cargo target, else in the other widget's own `target` of the same
//! checkout.
//!
//! This file is byte-identical in `widgets/tasks`, `widgets/review` and
//! `widgets/verify` (a test compares each copy with the Tasks one), so it
//! names nothing of its crate.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// A widget another one can start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Helper {
    /// Its file name in `Noa.app`'s Helpers.
    pub bundled: &'static str,
    /// The binary `cargo build` makes, and its crate's directory under
    /// `widgets/`.
    pub crate_name: &'static str,
    /// Its name in notices.
    pub title: &'static str,
}

pub const REVIEW: Helper = Helper {
    bundled: "Noa Review",
    crate_name: "review",
    title: "Review",
};

pub const VERIFY: Helper = Helper {
    bundled: "Noa Verify",
    crate_name: "verify",
    title: "Verify",
};

pub const TASKS: Helper = Helper {
    bundled: "Noa Tasks",
    crate_name: "tasks",
    title: "Tasks",
};

/// What the notice says when `helper` is not found.
fn not_found(helper: Helper) -> String {
    format!(
        "{} が見つかりません（widgets/{} をビルドしてください）",
        helper.title, helper.crate_name
    )
}

/// Where `helper` is for the widget executable `exe`:
/// - in a bundle, beside it: `Contents/Helpers/<bundled>`;
/// - a plain `<crate>` beside it (one Cargo target directory shared by
///   both, as `scripts/bundle-macos.sh` builds);
/// - in a checkout, `widgets/<crate>/target/<profile>/<crate>`, from
///   `widgets/<this crate>/target/<profile>/<this crate>`: this widget's
///   own profile first, then the other.
///
/// Only a regular file counts (a link is followed, as the platform's
/// launcher would).
pub fn locate(exe: &Path, helper: Helper) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let bundled = dir.join(helper.bundled);
    let shared = dir.join(helper.crate_name);
    let dev = dev_build_dirs(dir, helper)
        .into_iter()
        .map(|dir| dir.join(helper.crate_name));
    [bundled, shared]
        .into_iter()
        .chain(dev)
        .find(|path| path.is_file())
}

/// `helper`'s target directories in the checkout `exe_dir` (this widget's
/// `target/<profile>`) belongs to.
fn dev_build_dirs(exe_dir: &Path, helper: Helper) -> Vec<PathBuf> {
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
    let target = widgets.join(helper.crate_name).join("target");
    let other = if profile == "release" {
        "debug"
    } else {
        "release"
    };
    vec![target.join(profile), target.join(other)]
}

/// Starts another widget; the tests give one of their own.
pub trait Launcher {
    /// Starts `helper` with `args`. `Err` says why not, for the notice.
    fn launch(&mut self, helper: Helper, args: Vec<OsString>) -> Result<(), String>;
}

/// The widget found from this one's executable, started as a process of
/// its own.
pub struct System;

impl Launcher for System {
    fn launch(&mut self, helper: Helper, args: Vec<OsString>) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|_| not_found(helper))?;
        let program = locate(&exe, helper).ok_or_else(|| not_found(helper))?;
        spawn(&program, helper, args)
    }
}

/// What the demo and a snapshot have in place of other widgets: nothing is
/// ever started, the notice says this.
pub struct Offline(pub &'static str);

impl Launcher for Offline {
    fn launch(&mut self, _helper: Helper, _args: Vec<OsString>) -> Result<(), String> {
        Err(self.0.to_owned())
    }
}

/// Starts `program` with `args`, detached: no shell, in a process group of
/// its own (a Ctrl-C meant for this widget's terminal does not reach it),
/// no stdio, from the home directory. A thread waits for it, so it leaves
/// no zombie behind.
fn spawn(program: &Path, helper: Helper, args: Vec<OsString>) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("{} を起動できません: {err}", helper.title))?;
    reap_when_done(child, &UNREAPED, |wait| {
        std::thread::Builder::new()
            .name(format!("{}-reaper", helper.crate_name))
            .spawn(wait)
            .map(drop)
    });
    Ok(())
}

/// Children no thread could be given to wait for (the thread could not be
/// made): they are `try_wait`ed for at the next start, so they do not stay
/// zombies for this widget's whole life.
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
            "sibling-{}-{name}-{}",
            env!("CARGO_PKG_NAME"),
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
    fn links_ac5_the_helpers_are_named_as_noa_names_them() {
        let noa = include_str!("../../../crates/noa-app/src/helper_program.rs");
        for helper in [REVIEW, VERIFY, TASKS] {
            let entry = format!(
                "title: \"{}\",\n    bundled: \"{}\",\n    crate_name: \"{}\",",
                helper.title, helper.bundled, helper.crate_name
            );
            assert!(noa.contains(&entry), "{helper:?} is not Noa's");
        }
    }

    #[test]
    fn links_ac5_locate_finds_the_bundled_name_then_the_shared_target_then_the_dev_build() {
        let root = scratch("locate");
        // Bundled: Contents/Helpers/<bundled>, before a plain <crate>.
        let helpers = root.join("Noa.app/Contents/Helpers");
        let exe = helpers.join("Noa Tasks");
        touch(&exe);
        assert_eq!(locate(&exe, REVIEW), None, "not bundled yet");
        touch(&helpers.join("review"));
        assert_eq!(locate(&exe, REVIEW), Some(helpers.join("review")), "shared");
        touch(&helpers.join("Noa Review"));
        assert_eq!(
            locate(&exe, REVIEW),
            Some(helpers.join("Noa Review")),
            "bundle name first"
        );
        // Dev: the other widget's own target, this profile first.
        let widgets = root.join("widgets");
        let exe = widgets.join("review/target/release/review");
        touch(&exe);
        let debug = widgets.join("tasks/target/debug/tasks");
        let release = widgets.join("tasks/target/release/tasks");
        assert_eq!(locate(&exe, TASKS), None, "not built");
        touch(&debug);
        assert_eq!(
            locate(&exe, TASKS),
            Some(debug.clone()),
            "the other profile"
        );
        touch(&release);
        assert_eq!(
            locate(&exe, TASKS),
            Some(release.clone()),
            "its own profile"
        );
        let debug_exe = widgets.join("review/target/debug/review");
        touch(&debug_exe);
        assert_eq!(locate(&debug_exe, TASKS), Some(debug.clone()));
        // A directory under that name is not a program.
        std::fs::remove_file(&release).unwrap();
        std::fs::create_dir(&release).unwrap();
        assert_eq!(locate(&exe, TASKS), Some(debug));
        // Outside any `target`: no checkout to look in.
        let loose = root.join("bin/review");
        touch(&loose);
        assert_eq!(locate(&loose, TASKS), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A real start, no shell between: a script stands in for the widget
    /// and writes down its arguments.
    #[test]
    fn links_ac5_the_program_is_started_with_its_arguments_and_no_shell() {
        let root = scratch("spawn");
        let program = root.join("fake tasks");
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
        let args = vec![
            OsString::from("--repo=/w/a b; rm -rf x"),
            OsString::from("--select=#12"),
        ];
        spawn(&program, TASKS, args).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !out.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let written = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(written, "--repo=/w/a b; rm -rf x\n--select=#12\n");
    }

    #[test]
    fn links_ac5_a_program_that_cannot_start_says_why() {
        let root = scratch("broken");
        let program = root.join("not executable");
        std::fs::write(&program, "").unwrap();
        let err = spawn(&program, VERIFY, Vec::new()).unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(err.starts_with("Verify を起動できません"), "{err}");
    }

    #[test]
    fn links_ac5_not_found_and_offline_say_so() {
        assert_eq!(
            not_found(REVIEW),
            "Review が見つかりません（widgets/review をビルドしてください）"
        );
        assert_eq!(
            Offline("デモでは開きません").launch(TASKS, Vec::new()),
            Err("デモでは開きません".to_owned())
        );
    }

    fn short_lived() -> Child {
        Command::new("true").spawn().unwrap()
    }

    #[test]
    fn links_a_child_whose_reaper_thread_cannot_start_is_kept_and_reaped_later() {
        let leftovers = Mutex::new(Vec::new());
        reap_when_done(short_lived(), &leftovers, |_| {
            Err(std::io::Error::other("no threads"))
        });
        assert_eq!(leftovers.lock().unwrap().len(), 1, "kept, not dropped");
        std::thread::sleep(std::time::Duration::from_millis(200));
        reap_when_done(short_lived(), &leftovers, |wait| {
            wait();
            Ok(())
        });
        assert!(leftovers.lock().unwrap().is_empty(), "reaped");
    }

    #[test]
    fn links_copy_of_sibling_is_byte_identical_to_the_tasks_master() {
        assert!(
            include_str!("sibling.rs") == include_str!("../../tasks/src/sibling.rs"),
            "src/sibling.rs differs from widgets/tasks/src/sibling.rs: copy it over"
        );
    }
}
