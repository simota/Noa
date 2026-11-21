//! `agent-dashboard.open`: start the agent dashboard (`mock/agent-dashboard`,
//! a noa-gpui program with its own event loop) beside Noa. It reads Noa's
//! panes through the control server, so it runs as a process of its own.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PROGRAM: &str = "agent-dashboard-mock";

/// Start the dashboard found by [`locate`]. Failure is only logged, as for
/// `spawn:`.
pub(crate) fn open() {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let Some(program) = locate(exe_dir.as_deref(), &dev_build_dirs()) else {
        log::warn!(
            "agent dashboard: no {PROGRAM} beside Noa or in mock/agent-dashboard/target \
             (build it with `cargo build --manifest-path mock/agent-dashboard/Cargo.toml`)"
        );
        return;
    };
    let mut command = Command::new(&program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        command.current_dir(home);
    }
    let _ = crate::commands::start_detached(command, &program.to_string_lossy());
}

/// The dashboard beside Noa's executable (where `scripts/bundle-macos.sh`
/// puts it in `Noa.app`), else the first of `dev_dirs` that holds one.
fn locate(exe_dir: Option<&Path>, dev_dirs: &[PathBuf]) -> Option<PathBuf> {
    exe_dir
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
    fn the_dashboard_beside_noa_wins_over_a_dev_build() {
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
    fn a_dev_build_is_used_when_none_is_beside_noa_and_none_is_found_without_either() {
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
}
