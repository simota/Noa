//! 差分を見る: starts the Repo Browser widget (`widgets/repo-browser`) on
//! a worker's worktree (`--worktree=PATH`), found the way Noa finds its
//! helper programs (`crates/noa-app/src/helper_program.rs`), from this
//! dashboard's own executable: beside it in `Noa.app`'s `Contents/Helpers`,
//! else in the repo browser crate's own `target` of the same checkout.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The repo browser's file name in `Noa.app`'s Helpers (where this
/// dashboard is `Noa Agent Dashboard`).
const BUNDLED: &str = "Noa Repo Browser";
/// The binary `cargo build` makes, and its crate's directory under
/// `widgets/`.
const CRATE_NAME: &str = "repo-browser";

/// What the notice line says when there is no repo browser to start.
pub const NOT_FOUND: &str =
    "Repo Browser が見つかりません（widgets/repo-browser をビルドしてください）";

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
        spawn(&program, cwd)
    }
}

/// Where the repo browser is for the dashboard executable `exe`:
/// - in a bundle, beside it: `Contents/Helpers/Noa Repo Browser`;
/// - in a checkout, `widgets/repo-browser/target/<profile>/repo-browser`,
///   from `widgets/agent-dashboard/target/<profile>/agent-dashboard`: the
///   dashboard's own profile first, then the other.
///
/// Only a regular file counts (a link is followed, as the platform's
/// launcher would).
pub fn locate(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let bundled = dir.join(BUNDLED);
    let dev = dev_build_dirs(dir)
        .into_iter()
        .map(|dir| dir.join(CRATE_NAME));
    std::iter::once(bundled)
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
    let mut child = command
        .spawn()
        .map_err(|err| format!("Repo Browser を起動できません: {err}"))?;
    std::thread::Builder::new()
        .name("repo-browser-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map(drop)
        .map_err(|err| format!("Repo Browser を見守れません: {err}"))
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
}
