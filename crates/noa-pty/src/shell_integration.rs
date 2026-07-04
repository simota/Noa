//! Shell integration: ship OSC 133 / OSC 7 emitting startup scripts and
//! inject them automatically per shell (Ghostty parity).
//!
//! The scripts (embedded from the repo's `shell-integration/`) are
//! materialized once per process into a temp directory, and [`integration_for`]
//! computes the per-shell environment/argument changes that make the shell
//! load them without the user editing any config:
//!
//! - **zsh** — `ZDOTDIR` is pointed at our dir; our startup files source the
//!   user's real ones (carried in `NOA_ZDOTDIR`) then install the hooks.
//! - **fish** — our dir is prepended to `XDG_DATA_DIRS`, so fish auto-sources
//!   our `fish/vendor_conf.d` file.
//! - **bash** — launched with `--rcfile` pointing at our script (an
//!   interactive non-login bash reads it in place of `~/.bashrc`), which
//!   sources the user's config then installs the hooks.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Embedded startup scripts, materialized to disk at runtime. Each tuple is a
/// path relative to the integration base dir and the file's contents.
const EMBEDDED_SCRIPTS: &[(&str, &str)] = &[
    (
        "zsh/.zshenv",
        include_str!("../../../shell-integration/zsh/.zshenv"),
    ),
    (
        "zsh/.zprofile",
        include_str!("../../../shell-integration/zsh/.zprofile"),
    ),
    (
        "zsh/.zshrc",
        include_str!("../../../shell-integration/zsh/.zshrc"),
    ),
    (
        "zsh/.zlogin",
        include_str!("../../../shell-integration/zsh/.zlogin"),
    ),
    (
        "fish/vendor_conf.d/noa-integration.fish",
        include_str!("../../../shell-integration/fish/vendor_conf.d/noa-integration.fish"),
    ),
    (
        "bash/noa.bash",
        include_str!("../../../shell-integration/bash/noa.bash"),
    ),
];

/// The per-shell changes needed to auto-load noa's integration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ShellIntegration {
    /// Environment variables to set on the child.
    pub env: Vec<(String, String)>,
    /// Extra arguments to pass to the shell (e.g. `--posix` for bash).
    pub args: Vec<String>,
    /// When true, the caller must NOT add its usual login `-l` flag — this
    /// shell's integration handles login startup files itself (bash).
    pub suppress_login_flag: bool,
}

/// Materialize the embedded scripts once and return the base directory, or
/// `None` if writing failed (the shell then simply starts without
/// integration).
pub(crate) fn resources_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(materialize).as_deref()
}

fn materialize() -> Option<PathBuf> {
    let base = std::env::temp_dir().join(format!("noa-shell-integration-{}", std::process::id()));
    for (rel, contents) in EMBEDDED_SCRIPTS {
        let path = base.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(&path, contents).ok()?;
    }
    Some(base)
}

/// Compute the integration for `shell` (a path or bare name) rooted at `dir`.
/// `login` is the caller's requested login-shell mode; `current_zdotdir` and
/// `current_xdg_data_dirs` are the inherited env values to preserve. Returns
/// `None` for a shell we don't integrate.
pub(crate) fn integration_for(
    shell: &str,
    dir: &Path,
    login: bool,
    current_zdotdir: Option<&str>,
    current_xdg_data_dirs: Option<&str>,
) -> Option<ShellIntegration> {
    let name = Path::new(shell).file_name()?.to_str()?;
    match name {
        "zsh" => {
            let mut env = vec![(
                "ZDOTDIR".to_string(),
                dir.join("zsh").to_string_lossy().into_owned(),
            )];
            // Carry the user's real ZDOTDIR so our .zshenv can hand back to it.
            if let Some(zdotdir) = current_zdotdir {
                env.push(("NOA_ZDOTDIR".to_string(), zdotdir.to_string()));
            }
            Some(ShellIntegration {
                env,
                args: Vec::new(),
                suppress_login_flag: false,
            })
        }
        "fish" => {
            let ours = dir.join("fish").to_string_lossy().into_owned();
            let value = match current_xdg_data_dirs {
                Some(existing) if !existing.is_empty() => format!("{ours}:{existing}"),
                _ => ours,
            };
            Some(ShellIntegration {
                env: vec![("XDG_DATA_DIRS".to_string(), value)],
                args: Vec::new(),
                suppress_login_flag: false,
            })
        }
        "bash" => {
            let mut env = vec![("NOA_BASH_INJECT".to_string(), "1".to_string())];
            if login {
                env.push(("NOA_BASH_LOGIN".to_string(), "1".to_string()));
            }
            Some(ShellIntegration {
                env,
                // `--rcfile` is honored only by an interactive, non-login bash,
                // so we suppress `-l`; the script sources the login profiles
                // itself when NOA_BASH_LOGIN is set.
                args: vec![
                    "--rcfile".to_string(),
                    dir.join("bash/noa.bash").to_string_lossy().into_owned(),
                ],
                suppress_login_flag: true,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_points_zdotdir_at_our_dir_and_preserves_the_users() {
        let dir = Path::new("/tmp/noa-si");
        let integration =
            integration_for("/bin/zsh", dir, true, Some("/home/u/.zdot"), None).unwrap();

        assert!(
            integration
                .env
                .contains(&("ZDOTDIR".to_string(), "/tmp/noa-si/zsh".to_string()))
        );
        assert!(
            integration
                .env
                .contains(&("NOA_ZDOTDIR".to_string(), "/home/u/.zdot".to_string()))
        );
        assert!(!integration.suppress_login_flag);
        assert!(integration.args.is_empty());
    }

    #[test]
    fn zsh_without_user_zdotdir_omits_the_carry_var() {
        let dir = Path::new("/tmp/noa-si");
        let integration = integration_for("zsh", dir, true, None, None).unwrap();

        assert!(!integration.env.iter().any(|(k, _)| k == "NOA_ZDOTDIR"));
    }

    #[test]
    fn fish_prepends_our_dir_to_existing_xdg_data_dirs() {
        let dir = Path::new("/tmp/noa-si");
        let integration =
            integration_for("/usr/local/bin/fish", dir, false, None, Some("/usr/share")).unwrap();

        assert_eq!(
            integration.env,
            vec![(
                "XDG_DATA_DIRS".to_string(),
                "/tmp/noa-si/fish:/usr/share".to_string()
            )]
        );
    }

    #[test]
    fn fish_with_no_existing_xdg_uses_our_dir_alone() {
        let dir = Path::new("/tmp/noa-si");
        let integration = integration_for("fish", dir, false, None, None).unwrap();

        assert_eq!(
            integration.env,
            vec![("XDG_DATA_DIRS".to_string(), "/tmp/noa-si/fish".to_string())]
        );
    }

    #[test]
    fn bash_uses_rcfile_bootstrap_and_suppresses_login_flag() {
        let dir = Path::new("/tmp/noa-si");
        let integration = integration_for("/bin/bash", dir, true, None, None).unwrap();

        assert_eq!(
            integration.args,
            vec![
                "--rcfile".to_string(),
                "/tmp/noa-si/bash/noa.bash".to_string()
            ]
        );
        assert!(integration.suppress_login_flag);
        assert!(
            integration
                .env
                .contains(&("NOA_BASH_INJECT".to_string(), "1".to_string()))
        );
        assert!(
            integration
                .env
                .contains(&("NOA_BASH_LOGIN".to_string(), "1".to_string()))
        );
    }

    #[test]
    fn bash_non_login_omits_login_marker() {
        let dir = Path::new("/tmp/noa-si");
        let integration = integration_for("bash", dir, false, None, None).unwrap();

        assert!(!integration.env.iter().any(|(k, _)| k == "NOA_BASH_LOGIN"));
    }

    #[test]
    fn unknown_shell_has_no_integration() {
        let dir = Path::new("/tmp/noa-si");
        assert_eq!(integration_for("/bin/tcsh", dir, true, None, None), None);
    }

    #[test]
    fn materialize_writes_all_scripts() {
        let dir = resources_dir().expect("materialize should succeed in temp dir");
        assert!(dir.join("zsh/.zshrc").is_file());
        assert!(
            dir.join("fish/vendor_conf.d/noa-integration.fish")
                .is_file()
        );
        assert!(dir.join("bash/noa.bash").is_file());
    }
}
