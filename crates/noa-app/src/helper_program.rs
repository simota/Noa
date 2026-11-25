//! Start a noa-gpui widget program (`widgets/<crate>`, with its own event
//! loop) beside Noa: the agent dashboard (`agent-dashboard.open`), the repo
//! browser (`repo-browser.open`), the discussion (`discussion.open`), the
//! competition (`competition.open`), the review (`review.open`), the verify
//! (`verify.open`) and the agent usage (`agent-usage.open`). Each runs as a
//! process of its own. All but the agent usage read Noa's panes through the
//! control server; the agent usage needs neither the server nor
//! `server-enable`. Only the review and the verify get an argument: the
//! focused pane's directory.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// A widget program Noa can start.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HelperProgram {
    /// The name toasts use.
    pub title: &'static str,
    /// Its file name in `Noa.app`'s Helpers. macOS names a bare executable's
    /// process after its file in the menu bar and Dock, so the bundle ships
    /// it under the product name, not the crate's binary name.
    pub bundled: &'static str,
    /// The binary `cargo build` makes, and the directory under `widgets/`
    /// whose `target` holds it.
    pub crate_name: &'static str,
}

pub(crate) const AGENT_DASHBOARD: HelperProgram = HelperProgram {
    title: "Agent Dashboard",
    bundled: "Noa Agent Dashboard",
    crate_name: "agent-dashboard",
};

pub(crate) const REPO_BROWSER: HelperProgram = HelperProgram {
    title: "Repo Browser",
    bundled: "Noa Repo Browser",
    crate_name: "repo-browser",
};

pub(crate) const DISCUSSION: HelperProgram = HelperProgram {
    title: "Discussion",
    bundled: "Noa Discussion",
    crate_name: "discussion",
};

pub(crate) const COMPETITION: HelperProgram = HelperProgram {
    title: "Competition",
    bundled: "Noa Competition",
    crate_name: "competition",
};

pub(crate) const REVIEW: HelperProgram = HelperProgram {
    title: "Review",
    bundled: "Noa Review",
    crate_name: "review",
};

pub(crate) const VERIFY: HelperProgram = HelperProgram {
    title: "Verify",
    bundled: "Noa Verify",
    crate_name: "verify",
};

pub(crate) const AGENT_USAGE: HelperProgram = HelperProgram {
    title: "Agent Usage",
    bundled: "Noa Agent Usage",
    crate_name: "agent-usage",
};

/// What `helper` is started with, given the working directory of the
/// focused pane (local, an existing directory, as `App::focused_pane_cwd`
/// gives it): the review and the verify take it as `--repo=`, the place their
/// start views offer first; every other program starts with no argument.
pub(crate) fn launch_args(helper: HelperProgram, focused_cwd: Option<&str>) -> Vec<String> {
    match focused_cwd {
        Some(cwd)
            if (helper.crate_name == REVIEW.crate_name
                || helper.crate_name == VERIFY.crate_name)
                && Path::new(cwd).is_absolute() =>
        {
            vec![format!("--repo={cwd}")]
        }
        _ => Vec::new(),
    }
}

/// How much of the program's stderr is kept to say why it exited.
const STDERR_TAIL: usize = 4096;

/// Start `helper` as found by [`locate`]. `Err` says why it could not be
/// started; `on_failure` is called (on another thread) with why it exited, if
/// it ends unsuccessfully.
pub(crate) fn open(
    helper: HelperProgram,
    args: &[String],
    on_failure: impl FnOnce(String) + Send + 'static,
) -> Result<(), String> {
    let HelperProgram {
        title,
        bundled,
        crate_name,
    } = helper;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let helpers = exe_dir.as_deref().and_then(helpers_dir);
    let Some(program) = locate(helper, helpers.as_deref(), &dev_build_dirs(helper)) else {
        let message = format!("{title} is not installed (build widgets/{crate_name})");
        log::error!(
            "{crate_name}: no {bundled:?} in Noa.app's Helpers or {crate_name} in \
             widgets/{crate_name}/target \
             (build it with `cargo build --manifest-path widgets/{crate_name}/Cargo.toml`)"
        );
        return Err(message);
    };
    let mut command = Command::new(&program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        command.current_dir(home);
    }
    let mut child = command.spawn().map_err(|err| {
        log::error!("{crate_name}: could not start {}: {err}", program.display());
        format!("Could not start {title}: {err}")
    })?;
    log::info!(
        "{crate_name}: started {} (pid {})",
        program.display(),
        child.id()
    );
    let stderr = child.stderr.take();
    std::thread::Builder::new()
        .name(format!("{crate_name}-reaper"))
        .spawn(move || {
            let tail = stderr.map(read_tail).unwrap_or_default();
            let Ok(status) = child.wait() else { return };
            if let Some(message) = exit_message(title, status, &tail) {
                log::error!("{crate_name}: {message}");
                on_failure(message);
            }
        })
        .map_err(|err| format!("Could not watch {title}: {err}"))?;
    Ok(())
}

/// Where an installed Noa keeps its helpers, given the directory of Noa's
/// executable. On macOS, `Contents/Helpers` of the app bundle whose
/// `Contents/MacOS` holds Noa: helpers outside `Contents/MacOS` do not take
/// Noa.app as their main bundle, so the program gets no `com.simota.noa`
/// identity of its own. Elsewhere, `<prefix>/libexec/noa` beside the
/// `<prefix>/bin` that holds Noa (where `scripts/package-linux.sh` and its
/// `install.sh` put them).
fn helpers_dir(exe_dir: &Path) -> Option<PathBuf> {
    let prefix = exe_dir.parent()?;
    Some(if cfg!(target_os = "macos") {
        prefix.join("Helpers")
    } else {
        prefix.join("libexec").join("noa")
    })
}

/// Reads `stream` to its end, keeping only the last [`STDERR_TAIL`] bytes.
fn read_tail(mut stream: impl Read) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut chunk = [0u8; 1024];
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..n]);
        if tail.len() > STDERR_TAIL {
            tail.drain(..tail.len() - STDERR_TAIL);
        }
    }
    tail
}

/// Why the program ended, when it did not end cleanly: its status and the
/// last line it wrote to stderr.
fn exit_message(title: &str, status: ExitStatus, stderr_tail: &[u8]) -> Option<String> {
    if status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(stderr_tail);
    let last = text.lines().map(str::trim).rfind(|line| !line.is_empty());
    Some(match last {
        Some(line) => {
            let line: String = line.chars().take(200).collect();
            format!("{title} exited ({status}): {line}")
        }
        None => format!("{title} exited ({status})"),
    })
}

/// `helper` under its bundled name in `helpers` (see [`helpers_dir`]), else the first of `dev_dirs` that holds one.
fn locate(helper: HelperProgram, helpers: Option<&Path>, dev_dirs: &[PathBuf]) -> Option<PathBuf> {
    helpers
        .map(|dir| dir.join(helper.bundled))
        .into_iter()
        .chain(dev_dirs.iter().map(|dir| dir.join(helper.crate_name)))
        .find(|path| path.is_file())
}

/// The program crate's own target directories in this checkout, the one
/// matching Noa's build profile first.
fn dev_build_dirs(helper: HelperProgram) -> Vec<PathBuf> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../widgets")
        .join(helper.crate_name)
        .join("target");
    let profiles = if cfg!(debug_assertions) {
        ["debug", "release"]
    } else {
        ["release", "debug"]
    };
    profiles
        .iter()
        .map(|profile| target.join(profile))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-dashboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_bundled_dashboard_wins_over_a_dev_build() {
        let exe_dir = scratch_dir("exe");
        let dev = scratch_dir("dev");
        std::fs::write(exe_dir.join(AGENT_DASHBOARD.bundled), "").unwrap();
        std::fs::write(dev.join(AGENT_DASHBOARD.crate_name), "").unwrap();
        assert_eq!(
            locate(AGENT_DASHBOARD, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join(AGENT_DASHBOARD.bundled))
        );
        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&dev);
    }

    #[test]
    fn a_dev_build_is_used_when_none_is_bundled_and_none_is_found_without_either() {
        let exe_dir = scratch_dir("bare-exe");
        let missing = scratch_dir("missing");
        let dev = scratch_dir("dev-only");
        std::fs::write(dev.join(AGENT_DASHBOARD.crate_name), "").unwrap();
        assert_eq!(
            locate(
                AGENT_DASHBOARD,
                Some(&exe_dir),
                &[missing.clone(), dev.clone()]
            ),
            Some(dev.join(AGENT_DASHBOARD.crate_name))
        );
        assert_eq!(
            locate(
                AGENT_DASHBOARD,
                Some(&exe_dir),
                std::slice::from_ref(&missing)
            ),
            None
        );
        assert_eq!(locate(AGENT_DASHBOARD, None, &[]), None);
        for dir in [exe_dir, missing, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn the_dev_build_dirs_are_each_programs_own_crate_target_dirs() {
        for helper in [
            AGENT_DASHBOARD,
            REPO_BROWSER,
            DISCUSSION,
            COMPETITION,
            REVIEW,
            VERIFY,
            AGENT_USAGE,
        ] {
            let dirs = dev_build_dirs(helper);
            assert_eq!(dirs.len(), 2);
            let crate_dir = format!("widgets/{}/target", helper.crate_name);
            // The profile this build was made in comes first.
            let (first, second) = if cfg!(debug_assertions) {
                ("debug", "release")
            } else {
                ("release", "debug")
            };
            assert!(
                dirs[0].ends_with(format!("{crate_dir}/{first}")),
                "{dirs:?}"
            );
            assert!(
                dirs[1].ends_with(format!("{crate_dir}/{second}")),
                "{dirs:?}"
            );
            assert!(
                dirs[0]
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("Cargo.toml")
                    .is_file(),
                "{dirs:?}"
            );
        }
    }

    #[test]
    fn verify_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("verify-exe");
        let dev = scratch_dir("verify-dev");
        std::fs::write(dev.join(REVIEW.crate_name), "").unwrap();
        assert_eq!(
            locate(VERIFY, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(dev.join("verify"), "").unwrap();
        assert_eq!(
            locate(VERIFY, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(dev.join("verify"))
        );
        std::fs::write(exe_dir.join("Noa Review"), "").unwrap();
        assert_eq!(
            locate(VERIFY, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(dev.join("verify"))
        );
        std::fs::write(exe_dir.join("Noa Verify"), "").unwrap();
        assert_eq!(
            locate(VERIFY, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join("Noa Verify"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn repo_browser_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("browser-exe");
        let dev = scratch_dir("browser-dev");
        std::fs::write(dev.join(AGENT_DASHBOARD.crate_name), "").unwrap();
        assert_eq!(
            locate(REPO_BROWSER, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(dev.join("repo-browser"), "").unwrap();
        assert_eq!(
            locate(REPO_BROWSER, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(dev.join("repo-browser"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn discussion_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("discussion-exe");
        let dev = scratch_dir("discussion-dev");
        std::fs::write(dev.join(REPO_BROWSER.crate_name), "").unwrap();
        assert_eq!(
            locate(DISCUSSION, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(exe_dir.join("Noa Discussion"), "").unwrap();
        std::fs::write(dev.join("discussion"), "").unwrap();
        assert_eq!(
            locate(DISCUSSION, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join("Noa Discussion"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn competition_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("competition-exe");
        let dev = scratch_dir("competition-dev");
        std::fs::write(dev.join(DISCUSSION.crate_name), "").unwrap();
        assert_eq!(
            locate(COMPETITION, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(exe_dir.join("Noa Competition"), "").unwrap();
        std::fs::write(dev.join("competition"), "").unwrap();
        assert_eq!(
            locate(COMPETITION, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join("Noa Competition"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn review_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("review-exe");
        let dev = scratch_dir("review-dev");
        std::fs::write(dev.join(COMPETITION.crate_name), "").unwrap();
        assert_eq!(
            locate(REVIEW, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(dev.join("review"), "").unwrap();
        assert_eq!(
            locate(REVIEW, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(dev.join("review"))
        );
        std::fs::write(exe_dir.join("Noa Review"), "").unwrap();
        assert_eq!(
            locate(REVIEW, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join("Noa Review"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn agent_usage_is_found_by_its_own_binary_name() {
        let exe_dir = scratch_dir("agent-usage-exe");
        let dev = scratch_dir("agent-usage-dev");
        std::fs::write(dev.join(REVIEW.crate_name), "").unwrap();
        assert_eq!(
            locate(AGENT_USAGE, Some(&exe_dir), std::slice::from_ref(&dev)),
            None
        );
        std::fs::write(dev.join("agent-usage"), "").unwrap();
        assert_eq!(
            locate(AGENT_USAGE, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(dev.join("agent-usage"))
        );
        std::fs::write(exe_dir.join("Noa Agent Usage"), "").unwrap();
        assert_eq!(
            locate(AGENT_USAGE, Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join("Noa Agent Usage"))
        );
        for dir in [exe_dir, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn only_the_review_and_the_verify_are_given_the_focused_panes_directory() {
        for helper in [REVIEW, VERIFY] {
            assert_eq!(
                launch_args(helper, Some("/Users/me/src/noa")),
                ["--repo=/Users/me/src/noa"],
                "{}",
                helper.crate_name
            );
            assert!(
                launch_args(helper, None).is_empty(),
                "{}",
                helper.crate_name
            );
            assert!(
                launch_args(helper, Some("relative/dir")).is_empty(),
                "{}",
                helper.crate_name
            );
        }
        for helper in [
            AGENT_DASHBOARD,
            REPO_BROWSER,
            DISCUSSION,
            COMPETITION,
            AGENT_USAGE,
        ] {
            assert!(
                launch_args(helper, Some("/Users/me/src/noa")).is_empty(),
                "{}",
                helper.crate_name
            );
            assert!(
                launch_args(helper, None).is_empty(),
                "{}",
                helper.crate_name
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_helpers_dir_is_beside_the_macos_dir() {
        assert_eq!(
            helpers_dir(Path::new("/Applications/Noa.app/Contents/MacOS")),
            Some(PathBuf::from("/Applications/Noa.app/Contents/Helpers"))
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_helpers_dir_is_libexec_noa_beside_bin() {
        assert_eq!(
            helpers_dir(Path::new("/home/u/.local/bin")),
            Some(PathBuf::from("/home/u/.local/libexec/noa"))
        );
    }

    fn status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn a_clean_exit_is_not_reported_and_a_failed_one_names_its_last_stderr_line() {
        assert_eq!(exit_message("Agent Dashboard", status(0), b"noise\n"), None);
        assert_eq!(
            exit_message("Agent Dashboard", status(1), b"first\nno GPU adapter\n\n"),
            Some("Agent Dashboard exited (exit status: 1): no GPU adapter".to_owned())
        );
        assert_eq!(
            exit_message("Agent Dashboard", status(101), b""),
            Some("Agent Dashboard exited (exit status: 101)".to_owned())
        );
    }

    #[test]
    fn only_the_tail_of_stderr_is_kept() {
        let long = vec![b'x'; STDERR_TAIL * 3];
        let mut input = long.clone();
        input.extend_from_slice(b"\nlast");
        let tail = read_tail(input.as_slice());
        assert_eq!(tail.len(), STDERR_TAIL);
        assert!(tail.ends_with(b"\nlast"));
    }
}
