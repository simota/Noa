//! `spawn:<command line>`: a Noa extension (no Ghostty analog) that starts a
//! program in the background from a keybind or a command-palette entry.

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

/// Interned command lines, indexed by [`SpawnCommand`]. It grows only with
/// the distinct `spawn:` texts that config names, so entries are never
/// removed and an id stays valid for the life of the process.
static REGISTRY: Mutex<Vec<Arc<str>>> = Mutex::new(Vec::new());

const FALLBACK_SHELL: &str = "/bin/sh";

/// The command line of a `spawn:` action, as an id into a process-wide
/// registry so that [`super::AppCommand`] stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnCommand(usize);

impl SpawnCommand {
    /// The id for `text` with surrounding whitespace trimmed; `None` when
    /// nothing is left. The same text always gives the same id.
    pub(crate) fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut registry = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
        let index = match registry.iter().position(|known| &**known == text) {
            Some(index) => index,
            None => {
                registry.push(Arc::from(text));
                registry.len() - 1
            }
        };
        Some(Self(index))
    }

    pub fn text(self) -> Arc<str> {
        let registry = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(&registry[self.0])
    }
}

/// Start `command` detached from Noa, in `pane_cwd` when it is a directory,
/// else in the home directory. Failure is only logged.
pub(crate) fn spawn_in_background(command: SpawnCommand, pane_cwd: Option<&Path>) {
    let shell = std::env::var_os("SHELL");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // The outcome is already logged; nothing else is shown to the user.
    let _ = run(&command.text(), shell.as_deref(), pane_cwd, home.as_deref());
}

fn run(
    text: &str,
    shell: Option<&OsStr>,
    pane_cwd: Option<&Path>,
    home: Option<&Path>,
) -> std::io::Result<()> {
    let mut command = process_command(text, shell, pane_cwd, home);
    match command.spawn() {
        Ok(mut child) => {
            log::info!("spawn: started `{text}` (pid {})", child.id());
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(err) => {
            log::warn!("spawn: could not start `{text}`: {err}");
            Err(err)
        }
    }
}

/// A login shell, so that a Noa started from the Dock (with launchd's
/// minimal PATH) still finds what the user's login startup files put on PATH. Its own
/// process group keeps it out of the signals sent to Noa's group.
fn process_command(
    text: &str,
    shell: Option<&OsStr>,
    pane_cwd: Option<&Path>,
    home: Option<&Path>,
) -> Command {
    let shell = shell
        .filter(|shell| !shell.is_empty())
        .unwrap_or(OsStr::new(FALLBACK_SHELL));
    let mut command = Command::new(shell);
    command
        .args(["-l", "-c", text])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(cwd) = [pane_cwd, home]
        .into_iter()
        .flatten()
        .find(|dir| dir.is_dir())
    {
        command.current_dir(cwd);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn program_args_cwd(command: &Command) -> (String, Vec<String>, Option<PathBuf>) {
        (
            command.get_program().to_string_lossy().into_owned(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            command.get_current_dir().map(Path::to_path_buf),
        )
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-spawn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_command_runs_through_shell_as_a_login_shell_and_falls_back_to_sh() {
        let cases = [
            (Some(OsStr::new("/bin/zsh")), "/bin/zsh"),
            (None, "/bin/sh"),
            (Some(OsStr::new("")), "/bin/sh"),
        ];
        for (shell, program) in cases {
            let (actual, args, _) =
                program_args_cwd(&process_command("echo hi | wc", shell, None, None));
            assert_eq!(actual, program, "{shell:?}");
            assert_eq!(args, ["-l", "-c", "echo hi | wc"], "{shell:?}");
        }
    }

    #[test]
    fn the_cwd_is_the_pane_cwd_when_it_exists_else_home() {
        let pane = scratch_dir("cwd-pane");
        let home = scratch_dir("cwd-home");
        let missing = pane.join("missing");
        let cases = [
            (Some(pane.as_path()), &pane),
            (Some(missing.as_path()), &home),
            (None, &home),
        ];
        for (pane_cwd, expected) in cases {
            let (_, _, cwd) =
                program_args_cwd(&process_command("true", None, pane_cwd, Some(&home)));
            assert_eq!(cwd.as_ref(), Some(expected), "{pane_cwd:?}");
        }
        let (_, _, cwd) = program_args_cwd(&process_command("true", None, None, Some(&missing)));
        assert_eq!(cwd, None);
        let _ = std::fs::remove_dir_all(&pane);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_spawned_command_runs_in_the_pane_cwd() {
        let dir = scratch_dir("run");
        let text = r#"printf '%s' 'a=b,c:"d"' > out.txt"#;
        run(text, Some(OsStr::new("/bin/sh")), Some(&dir), None).unwrap();

        let out = dir.join("out.txt");
        let deadline = Instant::now() + Duration::from_secs(5);
        let content = loop {
            match std::fs::read_to_string(&out) {
                Ok(content) if !content.is_empty() => break content,
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                other => panic!("no output from the spawned command: {other:?}"),
            }
        };
        assert_eq!(content, r#"a=b,c:"d""#);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
