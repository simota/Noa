//! `agent-dashboard.open`: start the agent dashboard (`mock/agent-dashboard`,
//! a noa-gpui program with its own event loop) beside Noa. It reads Noa's
//! panes through the control server, so it runs as a process of its own.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const PROGRAM: &str = "agent-dashboard-mock";

/// How much of the dashboard's stderr is kept to say why it exited.
const STDERR_TAIL: usize = 4096;

/// Start the dashboard found by [`locate`]. `Err` says why it could not be
/// started; `on_failure` is called (on another thread) with why it exited, if
/// it ends unsuccessfully.
pub(crate) fn open(on_failure: impl FnOnce(String) + Send + 'static) -> Result<(), String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let helpers = exe_dir.as_deref().and_then(helpers_dir);
    let Some(program) = locate(helpers.as_deref(), &dev_build_dirs()) else {
        let message = "Agent Dashboard is not installed (build mock/agent-dashboard)".to_owned();
        log::error!(
            "agent dashboard: no {PROGRAM} in Noa.app's Helpers or in mock/agent-dashboard/target \
             (build it with `cargo build --manifest-path mock/agent-dashboard/Cargo.toml`)"
        );
        return Err(message);
    };
    let mut command = Command::new(&program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        command.current_dir(home);
    }
    let mut child = command.spawn().map_err(|err| {
        log::error!(
            "agent dashboard: could not start {}: {err}",
            program.display()
        );
        format!("Could not start Agent Dashboard: {err}")
    })?;
    log::info!(
        "agent dashboard: started {} (pid {})",
        program.display(),
        child.id()
    );
    let stderr = child.stderr.take();
    std::thread::Builder::new()
        .name("agent-dashboard-reaper".into())
        .spawn(move || {
            let tail = stderr.map(read_tail).unwrap_or_default();
            let Ok(status) = child.wait() else { return };
            if let Some(message) = exit_message(status, &tail) {
                log::error!("agent dashboard: {message}");
                on_failure(message);
            }
        })
        .map_err(|err| format!("Could not watch Agent Dashboard: {err}"))?;
    Ok(())
}

/// `Contents/Helpers` of the app bundle whose `Contents/MacOS` holds Noa.
/// Helpers outside `Contents/MacOS` do not take Noa.app as their main bundle,
/// so the dashboard gets no `com.simota.noa` identity of its own.
fn helpers_dir(exe_dir: &Path) -> Option<PathBuf> {
    Some(exe_dir.parent()?.join("Helpers"))
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

/// Why the dashboard ended, when it did not end cleanly: its status and the
/// last line it wrote to stderr.
fn exit_message(status: ExitStatus, stderr_tail: &[u8]) -> Option<String> {
    if status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(stderr_tail);
    let last = text.lines().map(str::trim).rfind(|line| !line.is_empty());
    Some(match last {
        Some(line) => {
            let line: String = line.chars().take(200).collect();
            format!("Agent Dashboard exited ({status}): {line}")
        }
        None => format!("Agent Dashboard exited ({status})"),
    })
}

/// The dashboard in `helpers` (where `scripts/bundle-macos.sh` puts it in
/// `Noa.app`), else the first of `dev_dirs` that holds one.
fn locate(helpers: Option<&Path>, dev_dirs: &[PathBuf]) -> Option<PathBuf> {
    helpers
        .into_iter()
        .chain(dev_dirs.iter().map(PathBuf::as_path))
        .map(|dir| dir.join(PROGRAM))
        .find(|path| path.is_file())
}

/// The dashboard crate's own target directories in this checkout, the one
/// matching Noa's build profile first.
fn dev_build_dirs() -> Vec<PathBuf> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mock/agent-dashboard/target");
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
        std::fs::write(exe_dir.join(PROGRAM), "").unwrap();
        std::fs::write(dev.join(PROGRAM), "").unwrap();
        assert_eq!(
            locate(Some(&exe_dir), std::slice::from_ref(&dev)),
            Some(exe_dir.join(PROGRAM))
        );
        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&dev);
    }

    #[test]
    fn a_dev_build_is_used_when_none_is_bundled_and_none_is_found_without_either() {
        let exe_dir = scratch_dir("bare-exe");
        let missing = scratch_dir("missing");
        let dev = scratch_dir("dev-only");
        std::fs::write(dev.join(PROGRAM), "").unwrap();
        assert_eq!(
            locate(Some(&exe_dir), &[missing.clone(), dev.clone()]),
            Some(dev.join(PROGRAM))
        );
        assert_eq!(locate(Some(&exe_dir), std::slice::from_ref(&missing)), None);
        assert_eq!(locate(None, &[]), None);
        for dir in [exe_dir, missing, dev] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn the_dev_build_dirs_are_the_dashboard_crates_target_dirs() {
        let dirs = dev_build_dirs();
        assert_eq!(dirs.len(), 2);
        for dir in &dirs {
            assert!(
                dir.ends_with("mock/agent-dashboard/target/debug")
                    || dir.ends_with("mock/agent-dashboard/target/release"),
                "{dir:?}"
            );
        }
        assert!(
            dirs[0]
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("Cargo.toml")
                .is_file()
        );
    }

    #[test]
    fn the_helpers_dir_is_beside_the_macos_dir() {
        assert_eq!(
            helpers_dir(Path::new("/Applications/Noa.app/Contents/MacOS")),
            Some(PathBuf::from("/Applications/Noa.app/Contents/Helpers"))
        );
    }

    fn status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn a_clean_exit_is_not_reported_and_a_failed_one_names_its_last_stderr_line() {
        assert_eq!(exit_message(status(0), b"noise\n"), None);
        assert_eq!(
            exit_message(status(1), b"first\nno GPU adapter\n\n"),
            Some("Agent Dashboard exited (exit status: 1): no GPU adapter".to_owned())
        );
        assert_eq!(
            exit_message(status(101), b""),
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
