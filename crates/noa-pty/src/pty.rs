//! [`Pty`] — owns the PTY master, spawns the child, and wires up the
//! reader/waiter threads.

use crossbeam_channel::{Receiver, Sender};
use noa_core::GridSize;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::reader::{spawn_reader, spawn_waiter};
use crate::writer::PtyWriter;
use crate::{PtyError, PtyEvent, Result};

const PTY_EVENT_QUEUE_CAPACITY: usize = 1024;

fn pty_event_channel() -> (Sender<PtyEvent>, Receiver<PtyEvent>) {
    crossbeam_channel::bounded(PTY_EVENT_QUEUE_CAPACITY)
}

/// Configuration for spawning a [`Pty`].
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// Initial terminal size in cells.
    pub size: GridSize,
    /// Shell program to run. `None` uses `$SHELL`, falling back to `/bin/zsh`.
    pub shell: Option<String>,
    /// Working directory for the child. `None` inherits the current one.
    pub cwd: Option<String>,
    /// Value for the `TERM` environment variable.
    pub term: String,
    /// Run the shell as a login shell (passes the `-l` flag).
    pub login: bool,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            size: GridSize::new(80, 24),
            shell: None,
            cwd: None,
            term: "xterm-256color".to_string(),
            login: true,
        }
    }
}

/// A spawned PTY + child shell.
///
/// Holds the master end alive, exposes an event stream of [`PtyEvent`]s, a
/// cloneable [`PtyWriter`], and a [`resize`](Pty::resize) method. Dropping the
/// `Pty` kills the child and tears down the master.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: PtyWriter,
    event_rx: Receiver<PtyEvent>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Pty {
    /// Spawn a PTY and a child shell per `config`.
    ///
    /// The child is spawned **before** any resize (a portable-pty/macOS
    /// quirk), and the slave handle is dropped immediately afterwards so that
    /// EOF is delivered on the master when the child exits.
    pub fn spawn(config: PtyConfig) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.size.rows,
                cols: config.size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::OpenPty(e.to_string()))?;

        // Build the shell command.
        let shell = config
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/zsh".to_string());

        let mut cmd = CommandBuilder::new(&shell);
        if config.login {
            // Login shell. NOTE: portable-pty's CommandBuilder does not let us
            // override argv[0], so the classic "-<shell>" argv[0] convention
            // isn't available — passing "-zsh" as an argument makes zsh treat
            // it as options ("bad option: -z") and exit. Use the `-l` flag,
            // which zsh/bash/sh all accept. Interactivity comes from stdin
            // being a tty (the pty slave), so no explicit `-i` is needed.
            cmd.arg("-l");
        }
        cmd.env("TERM", &config.term);
        if let Some(cwd) = &config.cwd {
            cmd.cwd(cwd);
        }

        // Spawn the child FIRST (macOS quirk), then we may resize.
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        // Drop the slave so EOF is delivered on the master at child exit.
        drop(pair.slave);
        let master = pair.master;

        // A killer handle so Drop can terminate the child even though the
        // child itself is owned by the waiter thread.
        let killer = child.clone_killer();

        let reader = master
            .try_clone_reader()
            .map_err(|e| PtyError::CloneReader(e.to_string()))?;
        let writer = master
            .take_writer()
            .map_err(|e| PtyError::TakeWriter(e.to_string()))?;

        let (tx, event_rx) = pty_event_channel();
        spawn_reader(reader, tx.clone());
        spawn_waiter(child, tx);

        Ok(Self {
            master,
            writer: PtyWriter::new(writer),
            event_rx,
            killer,
        })
    }

    /// Receiver for the PTY's output/lifecycle events.
    pub fn event_rx(&self) -> &Receiver<PtyEvent> {
        &self.event_rx
    }

    /// A cloneable, sendable handle for writing input to the PTY.
    pub fn writer(&self) -> PtyWriter {
        self.writer.clone()
    }

    /// Resize the PTY, informing the kernel and signalling the child.
    pub fn resize(&self, size: GridSize) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Resize(e.to_string()))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Best-effort: terminate the child so the waiter thread unblocks.
        let _ = self.killer.kill();
    }
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn event_channel_is_bounded_for_reader_backpressure() {
        let (tx, rx) = pty_event_channel();
        for code in 0..PTY_EVENT_QUEUE_CAPACITY {
            tx.try_send(PtyEvent::Exit(code as i32))
                .expect("queue has capacity");
        }

        match tx.try_send(PtyEvent::Exit(-1)) {
            Err(crossbeam_channel::TrySendError::Full(PtyEvent::Exit(code))) => {
                assert_eq!(code, -1);
            }
            other => panic!("expected a full event queue, got {other:?}"),
        }
        assert_eq!(rx.len(), PTY_EVENT_QUEUE_CAPACITY);
    }

    #[test]
    fn echo_hello_then_exit() {
        // Run a shell that echoes "hello" and exits, so we exercise the same
        // spawn path the real terminal uses.
        let cfg = PtyConfig {
            size: GridSize::new(80, 24),
            shell: Some("/bin/sh".to_string()),
            cwd: None,
            term: "xterm-256color".to_string(),
            login: false,
        };
        let pty = Pty::spawn(cfg).expect("spawn pty");

        // Feed a command into the shell's stdin.
        let w = pty.writer();
        w.write(b"echo hello\nexit\n").expect("write");
        w.flush().expect("flush");

        let mut collected = Vec::new();
        let mut saw_exit = false;
        let deadline = Duration::from_secs(5);

        loop {
            match pty.event_rx().recv_timeout(deadline) {
                Ok(PtyEvent::Data(chunk)) => collected.extend_from_slice(&chunk),
                Ok(PtyEvent::Exit(_)) => {
                    saw_exit = true;
                    break;
                }
                Ok(PtyEvent::Error(e)) => panic!("pty error: {e}"),
                Err(_) => break, // timeout guard so we never hang
            }
        }

        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains("hello"),
            "expected 'hello' in output: {text:?}"
        );
        assert!(saw_exit, "expected an Exit event");
    }

    #[test]
    fn default_login_shell_stays_interactive() {
        // Regression: the default config spawns a login shell; a broken login
        // argument once made zsh exit 1 immediately ("bad option: -z"), so the
        // app quit on launch. An interactive shell must wait for input, not
        // exit on its own.
        let pty = Pty::spawn(PtyConfig::default()).expect("spawn");
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            match pty.event_rx().recv_timeout(Duration::from_millis(300)) {
                Ok(PtyEvent::Exit(c)) => panic!("login shell exited early with code {c}"),
                Ok(PtyEvent::Error(e)) => panic!("pty error: {e}"),
                _ => {} // Data (a prompt) or a timeout — both mean it's alive
            }
        }
    }

    #[test]
    fn resize_ok() {
        let pty = Pty::spawn(PtyConfig {
            shell: Some("/bin/sh".to_string()),
            login: false,
            ..Default::default()
        })
        .expect("spawn");
        pty.resize(GridSize::new(120, 40)).expect("resize");
    }
}
