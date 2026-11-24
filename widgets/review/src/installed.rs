//! Which agent CLIs are installed, as the login shell sees them.
//!
//! Agents start in Noa panes through the user's login shell
//! (`$SHELL -l -i -c "exec claude …"`), because Noa launched from Finder has
//! a bare `PATH`. So the check asks that same shell, not this process.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use review::model::types::AgentKind;

use crate::session::runner::login_shell;

/// What the probe's lines start with; an interactive rc file may print
/// anything else.
const MARKER: &str = "noa-agent:";
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
/// How long the output is waited for after the shell exited (a background
/// job of an rc file may hold the pipe open).
const DRAIN: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The kinds found, in [`AgentKind::ALL`] order; possibly none.
    Found(Vec<AgentKind>),
    /// The shell could not be run or did not answer in time.
    Unknown(String),
}

/// Asks the login shell which agents it can run. Blocks up to
/// [`TIMEOUT`]: call it off the UI thread.
pub fn detect() -> Installed {
    run(&login_shell(), &probe_script(), TIMEOUT)
}

/// Runs [`detect`] on a background thread; the result arrives on the
/// returned channel, after which `wake` is called.
pub fn spawn(wake: impl Fn() + Send + 'static) -> Receiver<Installed> {
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("review-installed".into())
        .spawn(move || {
            let _ = tx.send(detect());
            wake();
        });
    if let Err(e) = started {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(Installed::Unknown(format!("スレッドを開始できません: {e}")));
        return rx;
    }
    rx
}

fn probe_script() -> String {
    let names: Vec<&str> = AgentKind::ALL.iter().map(|k| k.command()).collect();
    format!(
        "for c in {}; do command -v \"$c\" >/dev/null 2>&1 && printf \"\\n{MARKER}%s\\n\" \"$c\"; done; exit 0",
        names.join(" ")
    )
}

fn run(shell: &str, script: &str, timeout: Duration) -> Installed {
    let mut child = match Command::new(shell)
        .args(["-l", "-i", "-c", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Installed::Unknown(format!("{shell} を起動できません: {e}")),
    };
    // Read as it comes, so a chatty rc file cannot fill the pipe and stall
    // the shell, and what was printed survives a pipe that stays open.
    let out = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        let out = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                out.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(&chunk[..n]);
            }
            let _ = done_tx.send(());
        });
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Installed::Unknown(format!(
                    "{}秒以内に応答がありません",
                    timeout.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Installed::Unknown(format!("{shell} の終了を確認できません: {e}"));
            }
        }
    };
    // The probe ends with `exit 0`: anything else means the shell could
    // not run it (a syntax it does not take, a failing rc file).
    if !status.success() {
        return Installed::Unknown(match status.code() {
            Some(code) => format!("ログインシェルで確認できませんでした（終了コード {code}）"),
            None => "ログインシェルで確認できませんでした（シグナルで終了）".to_owned(),
        });
    }
    let _ = done_rx.recv_timeout(DRAIN);
    let bytes = out.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Installed::Found(parse(&String::from_utf8_lossy(&bytes)))
}

/// The kinds `stdout` of the probe names, in [`AgentKind::ALL`] order and
/// once each. Lines without the marker, and unknown names, are ignored.
pub fn parse(stdout: &str) -> Vec<AgentKind> {
    let named: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(MARKER))
        .map(str::trim)
        .collect();
    AgentKind::ALL
        .into_iter()
        .filter(|kind| named.contains(&kind.command()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_marked_lines_of_known_agents_count() {
        let out = "Welcome back!\r\nnoa-agent:codex\nnoa-agent:vim\n  noa-agent:agy\nclaude\nnoa-agent:agy\r\n";
        assert_eq!(parse(out), [AgentKind::Codex, AgentKind::Agy]);
    }

    #[test]
    fn kinds_come_back_in_the_fixed_order_once_each() {
        let out = "noa-agent:agy\nnoa-agent:claude\nnoa-agent:agy\nnoa-agent:claude\n";
        assert_eq!(parse(out), [AgentKind::Claude, AgentKind::Agy]);
    }

    #[test]
    fn nothing_found_is_empty() {
        assert!(parse("").is_empty());
        assert!(parse("junk\nnoa-agent:\nnoa-agent:nope\n").is_empty());
    }

    #[test]
    fn the_probe_names_every_kind() {
        let script = probe_script();
        assert!(
            script.starts_with("for c in claude codex agy; do"),
            "{script}"
        );
        assert!(script.contains("noa-agent:%s"));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_prints_the_marker_is_read() {
        let script = "echo banner; echo noa-agent:codex; echo noa-agent:claude";
        assert_eq!(
            run("/bin/sh", script, Duration::from_secs(5)),
            Installed::Found(vec![AgentKind::Claude, AgentKind::Codex])
        );
    }

    /// An rc file that prints without a trailing newline (a terminal-title
    /// escape, say) must not swallow the first marker.
    #[cfg(unix)]
    #[test]
    fn output_left_without_a_newline_does_not_hide_the_first_agent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("review-installed-nl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("codex");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = format!(
            "PATH='{}'; printf '\\033]0;title\\007'; {}",
            dir.display(),
            probe_script()
        );
        let got = run("/bin/sh", &script, Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Installed::Found(vec![AgentKind::Codex]));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_does_not_answer_is_killed_and_unknown() {
        let started = Instant::now();
        let got = run("/bin/sh", "sleep 30", Duration::from_millis(200));
        assert!(matches!(got, Installed::Unknown(_)), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_fails_is_unknown_not_empty() {
        let got = run("/bin/sh", "if then fi", Duration::from_secs(5));
        assert!(
            matches!(&got, Installed::Unknown(why) if why.contains("終了コード")),
            "{got:?}"
        );
        let got = run("/bin/sh", "exit 3", Duration::from_secs(5));
        assert_eq!(
            got,
            Installed::Unknown("ログインシェルで確認できませんでした（終了コード 3）".into())
        );
        assert_eq!(
            run(
                "/bin/sh",
                "echo noa-agent:codex; exit 0",
                Duration::from_secs(5)
            ),
            Installed::Found(vec![AgentKind::Codex])
        );
    }

    #[test]
    fn a_missing_shell_is_unknown() {
        assert!(matches!(
            run("/nonexistent/shell", "true", Duration::from_secs(1)),
            Installed::Unknown(_)
        ));
    }
}
