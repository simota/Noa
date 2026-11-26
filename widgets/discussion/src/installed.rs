//! Which agent CLIs are installed, judged by the PATH of the user's login
//! shell: the agents are started through it (`runner::agent_argv`), and
//! Noa started from Finder has a bare PATH of its own.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use discussion::model::types::AgentKind;

use crate::session::runner::login_shell;

/// The prefix of a line the probe prints for each agent found. Interactive
/// rc files may print anything on stdout; only these lines count.
const MARKER: &str = "noa-agent:";

/// How long the login shell may take before the check gives up.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The kinds on the login shell's PATH, in [`AgentKind::ALL`] order
    /// (possibly none).
    Found(Vec<AgentKind>),
    /// The login shell could not be run, or did not finish in time: why.
    Unknown(String),
}

impl Installed {
    /// Every kind: what the demo and one-off renders assume, without
    /// running a shell.
    pub fn all() -> Self {
        Installed::Found(AgentKind::ALL.to_vec())
    }

    /// The kinds to offer: the ones found, or every kind when the check
    /// could not tell.
    pub fn available(&self) -> Vec<AgentKind> {
        match self {
            Installed::Found(kinds) => kinds.clone(),
            Installed::Unknown(_) => AgentKind::ALL.to_vec(),
        }
    }
}

/// Runs [`detect`] on its own thread (the login shell can take seconds),
/// sends its result and calls `wake`.
pub fn spawn(wake: impl Fn() + Send + 'static) -> Receiver<Installed> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("discussion-installed".into())
        .spawn(move || {
            let _ = tx.send(detect());
            wake();
        })
        .expect("spawn discussion-installed thread");
    rx
}

/// The probe: prints a marked line for each agent command the login
/// shell finds. It ends in `exit 0`, so a POSIX shell that ran the loop
/// exits 0 whatever it found; a shell that cannot run it (csh, fish: not
/// POSIX) exits otherwise, and the check then cannot tell.
fn script() -> String {
    let names = AgentKind::ALL.map(AgentKind::command).join(" ");
    format!(
        "for c in {names}; do command -v \"$c\" >/dev/null 2>&1 && printf \"\\n{MARKER}%s\\n\" \"$c\"; done; exit 0"
    )
}

/// Asks the user's login shell which agent commands are on its PATH.
pub fn detect() -> Installed {
    let mut command = Command::new(login_shell());
    command.args(["-l", "-i", "-c", &script()]);
    run(command)
}

/// Runs the probe `command` (stdin and stderr to null) for at most
/// [`TIMEOUT`]: what it found if it exits 0, else why it could not tell.
fn run(mut command: Command) -> Installed {
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let shell = command.get_program().to_string_lossy().into_owned();
            return Installed::Unknown(format!("{shell} を起動できません: {err}"));
        }
    };
    let deadline = Instant::now() + TIMEOUT;
    // Read on the side, so a chatty rc file cannot fill the pipe and stall
    // the shell; a background job it leaves holding the pipe only costs
    // the wait below, never a hang.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (out_tx, out_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = out_tx.send(bytes);
    });
    let timed_out =
        || Installed::Unknown(format!("{} 秒以内に終わりませんでした", TIMEOUT.as_secs()));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return timed_out();
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Installed::Unknown(err.to_string());
            }
        }
    };
    if !status.success() {
        use std::os::unix::process::ExitStatusExt;
        let how = match (status.code(), status.signal()) {
            (Some(code), _) => format!("終了コード {code}"),
            (None, Some(signal)) => format!("シグナル {signal}"),
            (None, None) => status.to_string(),
        };
        return Installed::Unknown(format!("ログインシェルで確認できませんでした（{how}）"));
    }
    match out_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(bytes) => Installed::Found(parse(&String::from_utf8_lossy(&bytes))),
        Err(_) => timed_out(),
    }
}

/// The kinds the probe reported, in [`AgentKind::ALL`] order, each once;
/// lines without the marker and unknown names are ignored.
pub fn parse(stdout: &str) -> Vec<AgentKind> {
    let found: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(MARKER))
        .map(str::trim)
        .collect();
    AgentKind::ALL
        .into_iter()
        .filter(|kind| found.contains(&kind.command()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_marked_lines_of_known_agents_count_in_their_usual_order() {
        let out = "Welcome back!\n\
                   noa-agent:agy\n\
                   echo noa-agent:codex\n\
                   noa-agent:nope\n\
                   noa-agent:claude\r\n\
                   noa-agent:agy\n";
        assert_eq!(parse(out), [AgentKind::Claude, AgentKind::Agy]);
        assert_eq!(parse(""), []);
        assert_eq!(parse("noa-agent:\nclaude\ncodex\n"), []);
    }

    #[test]
    fn the_probe_names_every_kind_and_marks_its_output() {
        assert_eq!(
            script(),
            "for c in claude codex agy; do command -v \"$c\" >/dev/null 2>&1 \
             && printf \"\\nnoa-agent:%s\\n\" \"$c\"; done; exit 0"
        );
    }

    #[test]
    fn the_probe_finds_the_commands_on_the_shells_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("discussion-installed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["codex", "agy"] {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = Command::new("/bin/sh")
            .args(["-c", &script()])
            .env("PATH", &dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            parse(&String::from_utf8_lossy(&out.stdout)),
            [AgentKind::Codex, AgentKind::Agy]
        );
    }

    /// An rc file that prints without a trailing newline (a terminal-title
    /// escape, say) must not swallow the first marker.
    #[test]
    fn output_left_without_a_newline_does_not_hide_the_first_agent() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("discussion-installed-nl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("codex");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut shell = Command::new("/bin/sh");
        shell
            .args(["-c", &format!("printf '\\033]0;title\\007'; {}", script())])
            .env("PATH", &dir);
        let got = run(shell);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Installed::Found(vec![AgentKind::Codex]));
    }

    #[test]
    fn a_shell_that_cannot_run_the_probe_cannot_tell() {
        // A syntax error, as a non-POSIX login shell meets the probe.
        let mut broken = Command::new("/bin/sh");
        broken.args(["-c", "for c in; do"]);
        assert!(matches!(
            run(broken),
            Installed::Unknown(why) if why.starts_with("ログインシェルで確認できませんでした（終了コード ")
        ));
        assert_eq!(
            run(Command::new("/usr/bin/false")),
            Installed::Unknown("ログインシェルで確認できませんでした（終了コード 1）".into())
        );
        let mut killed = Command::new("/bin/sh");
        killed.args(["-c", "kill -9 $$"]);
        assert_eq!(
            run(killed),
            Installed::Unknown("ログインシェルで確認できませんでした（シグナル 9）".into())
        );
        // Csh cannot parse the probe at all, `exit 0` or not.
        if std::path::Path::new("/bin/csh").exists() {
            let mut csh = Command::new("/bin/csh");
            csh.args(["-f", "-c", &script()]);
            assert!(matches!(run(csh), Installed::Unknown(_)));
        }
        // A POSIX shell that finds nothing still exits 0: none installed.
        let mut sh = Command::new("/bin/sh");
        sh.args(["-c", &script()]).env("PATH", "/nonexistent");
        assert_eq!(run(sh), Installed::Found(Vec::new()));
        let mut missing = Command::new("/nonexistent/shell");
        missing.arg("-c");
        assert!(
            matches!(run(missing), Installed::Unknown(why) if why.contains("を起動できません"))
        );
    }

    #[test]
    fn an_unknown_result_offers_every_kind() {
        assert_eq!(
            Installed::Unknown("x".into()).available(),
            AgentKind::ALL.to_vec()
        );
        assert_eq!(
            Installed::Found(vec![AgentKind::Codex]).available(),
            [AgentKind::Codex]
        );
    }
}
