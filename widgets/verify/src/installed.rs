//! Which agent CLIs are installed, as the login shell sees them.
//!
//! Agents start in Noa panes through the user's login shell
//! (`$SHELL -l -i -c "exec claude …"`), because Noa launched from Finder has
//! a bare `PATH`. So the check asks that same shell, not this process.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use verify::model::types::AgentKind;

use crate::session::runner::login_shell;

/// What the probe's lines start with; an interactive rc file may print
/// anything else.
const MARKER: &str = "noa-agent:";
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
/// How long the output is waited for after the shell exited (a background
/// job of an rc file may hold the pipe open).
const DRAIN: Duration = Duration::from_millis(500);
// The probe emits one short marker per kind. A shell startup message must
// not make detection retain an unbounded line or its complete output.
const LINE_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The kinds found, in [`AgentKind::ALL`] order; possibly none.
    Found(Vec<AgentKind>),
    /// The shell could not be run or did not answer in time.
    Unknown(String),
}

/// Asks the login shell which agents it can run. Blocks up to [`TIMEOUT`]
/// plus [`DRAIN`]: call it off the UI thread.
pub fn detect() -> Installed {
    run(&login_shell(), &probe_script(), TIMEOUT)
}

/// Runs [`detect`] on a background thread; the result arrives on the
/// returned channel, after which `wake` is called.
pub fn spawn(wake: impl Fn() + Send + 'static) -> Receiver<Installed> {
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("verify-installed".into())
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
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Installed::Unknown(format!("{shell} を起動できません: {e}")),
    };
    let Some(mut stdout) = child.stdout.take() else {
        stop(&mut child);
        return Installed::Unknown("ログインシェルの出力を読めません".into());
    };
    if let Err(error) = nonblocking(stdout.as_raw_fd()) {
        stop(&mut child);
        return Installed::Unknown(format!("ログインシェルの出力を読めません: {error}"));
    }
    // Read and wait on this thread. In particular, a background job in an
    // rc file cannot retain a reader after the probe's drain deadline.
    let mut markers = Markers::default();
    let mut chunk = [0u8; 4096];
    let mut eof = false;
    let mut drain_until = None;
    let deadline = Instant::now() + timeout;
    loop {
        let mut received = false;
        if !eof {
            match stdout.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => {
                    markers.push(&chunk[..n]);
                    received = true;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => {
                    stop(&mut child);
                    return Installed::Unknown(format!(
                        "ログインシェルの出力を読めません: {error}"
                    ));
                }
            }
        }
        if drain_until.is_none() {
            match child.try_wait() {
                // The probe ends with `exit 0`; a failure is not an empty
                // installation list (for example, incompatible shell syntax).
                Ok(Some(status)) if !status.success() => {
                    return Installed::Unknown(match status.code() {
                        Some(code) => {
                            format!("ログインシェルで確認できませんでした（終了コード {code}）")
                        }
                        None => "ログインシェルで確認できませんでした（シグナルで終了）".to_owned(),
                    });
                }
                Ok(Some(_)) => drain_until = Some(Instant::now() + DRAIN),
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    stop(&mut child);
                    return Installed::Unknown(format!(
                        "{}秒以内に応答がありません",
                        timeout.as_secs()
                    ));
                }
                Err(error) => {
                    stop(&mut child);
                    return Installed::Unknown(format!("{shell} の終了を確認できません: {error}"));
                }
            }
        }
        if drain_until.is_some_and(|until| eof || Instant::now() >= until) {
            break;
        }
        // Check the deadlines after every chunk, even when an rc job
        // continuously fills the pipe. Do not throttle a large finite banner.
        if !received {
            std::thread::sleep(POLL);
        }
    }
    Installed::Found(markers.finish())
}

fn nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: fd belongs to our live ChildStdout; these fcntl commands take
    // no pointer and only change this probe's read end of the pipe.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn stop(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    // The shell started in its own process group. Stop its foreground and
    // background children too, instead of leaving a timed-out rc job behind.
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: a negative child PID addresses only its new process group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct Markers {
    line: [u8; LINE_LIMIT],
    len: usize,
    over: bool,
    found: Vec<AgentKind>,
}

impl Default for Markers {
    fn default() -> Self {
        Self {
            line: [0; LINE_LIMIT],
            len: 0,
            over: false,
            found: Vec::new(),
        }
    }
}

impl Markers {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
            } else if self.len < LINE_LIMIT {
                self.line[self.len] = byte;
                self.len += 1;
            } else {
                self.over = true;
            }
        }
    }

    fn end_line(&mut self) {
        if !self.over {
            for kind in parse(&String::from_utf8_lossy(&self.line[..self.len])) {
                if !self.found.contains(&kind) {
                    self.found.push(kind);
                }
            }
        }
        self.len = 0;
        self.over = false;
    }

    fn finish(mut self) -> Vec<AgentKind> {
        self.end_line();
        AgentKind::ALL
            .into_iter()
            .filter(|kind| self.found.contains(kind))
            .collect()
    }
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

    #[test]
    fn marked_output_is_folded_with_bounded_lines_across_chunks() {
        let mut markers = Markers::default();
        for _ in 0..1024 {
            markers.push(&[b'x'; 4096]);
        }
        assert_eq!(markers.len, LINE_LIMIT);
        assert!(markers.over);
        assert!(markers.found.is_empty());
        markers.push(b"noa-agent:agy\nnoa-agent:co");
        markers.push(b"dex\r\nnoa-agent:unknown\nnoa-agent: claude \n");
        markers.push(b"noa-agent:codex\nnoa-agent:a");
        markers.push(b"gy");
        assert_eq!(markers.finish(), AgentKind::ALL);
    }

    #[test]
    fn an_overlong_marker_line_is_not_partially_accepted() {
        let mut markers = Markers::default();
        markers.push(b"noa-agent:codex");
        markers.push(&[b' '; LINE_LIMIT]);
        markers.push(b"\nnoa-agent:claude\n");
        assert_eq!(markers.finish(), [AgentKind::Claude]);
    }

    #[cfg(unix)]
    #[test]
    fn a_background_writer_loses_the_pipe_when_detection_returns() {
        for shell in ["/bin/sh", "/bin/bash"] {
            let name = std::path::Path::new(shell)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let dir = std::env::temp_dir().join(format!(
                "verify-installed-background-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let release = dir.join("release");
            let result = dir.join("result");
            let quote = |path: &std::path::Path| {
                format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
            };
            // The child cannot test the pipe until run has returned. Ignoring
            // SIGPIPE lets it report whether the reader was actually closed.
            // Isolate printf's stdio in another subshell, whose stdout stays
            // on the pipe: a failed builtin write may retain buffered bytes
            // on macOS, which must not be flushed into the result file.
            let script = format!(
                "(trap '' PIPE; exec 3>&1; n=0; \
                 while [ ! -e {} ] && [ \"$n\" -lt 500 ]; do n=$((n + 1)); sleep 0.01; done; \
                 if (exec 1>&3; printf 'noa-agent:claude\\n'); then printf open; else printf closed; fi > {}) & \
                 printf 'noa-agent:codex\\n'; exit 0",
                quote(&release),
                quote(&result),
            );
            let started = Instant::now();
            let got = run(shell, &script, Duration::from_secs(5));
            std::fs::write(&release, b"go").unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            let output = loop {
                if let Ok(output) = std::fs::read_to_string(&result)
                    && !output.is_empty()
                {
                    break output;
                }
                assert!(
                    Instant::now() < until,
                    "{shell}: background writer did not answer"
                );
                std::thread::sleep(POLL);
            };
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(got, Installed::Found(vec![AgentKind::Codex]), "{shell}");
            assert!(started.elapsed() < Duration::from_secs(5), "{shell}");
            assert_eq!(
                output, "closed",
                "{shell}: detection left a pipe reader behind"
            );
        }
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
        let dir = std::env::temp_dir().join(format!("verify-installed-nl-{}", std::process::id()));
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
    fn continuous_output_does_not_postpone_the_timeout() {
        let started = Instant::now();
        let got = run(
            "/bin/sh",
            "while :; do printf 'noa-agent:codex\\n'; done",
            Duration::from_millis(200),
        );
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
