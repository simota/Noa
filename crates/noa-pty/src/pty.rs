//! [`Pty`] — owns the PTY master, spawns the child, and wires up the
//! reader/waiter threads.

use crossbeam_channel::{Receiver, Sender};
use noa_core::GridSize;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::reader::{spawn_reader, spawn_waiter};
use crate::shell_integration;
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
    /// Explicit command argv to run *instead of* the shell (the CLI `-e`
    /// flag). When set, `shell`, `login`, and `shell_integration` are all
    /// ignored: the argv is spawned verbatim as the pty child, exactly like
    /// Ghostty's `-e`. Must be non-empty when `Some`.
    pub command: Option<Vec<String>>,
    /// Working directory for the child. `None` inherits the current one.
    pub cwd: Option<String>,
    /// Value for the `TERM` environment variable.
    pub term: String,
    /// Run the shell as a login shell (passes the `-l` flag).
    pub login: bool,
    /// Automatically inject noa's OSC 133 / OSC 7 shell integration for
    /// supported shells (zsh/bash/fish).
    pub shell_integration: bool,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            size: GridSize::new(80, 24),
            shell: None,
            command: None,
            cwd: None,
            term: "xterm-256color".to_string(),
            login: true,
            shell_integration: true,
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

        // Build the child command: an explicit `-e` argv verbatim, or the
        // (login) shell with optional integration.
        let mut cmd = if let Some(argv) = config.command.as_deref().filter(|argv| !argv.is_empty())
        {
            // Explicit command (`-e`): no login flag, no shell integration —
            // the argv is the child, exactly as given (Ghostty parity).
            let mut cmd = CommandBuilder::new(&argv[0]);
            for arg in &argv[1..] {
                cmd.arg(arg);
            }
            cmd
        } else {
            let shell = config
                .shell
                .clone()
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "/bin/zsh".to_string());

            let mut cmd = CommandBuilder::new(&shell);

            // Resolve shell integration first: it can add flags/env and, for
            // bash, take over login-shell startup (so we skip our own `-l`).
            let integration = if config.shell_integration {
                shell_integration::resources_dir().and_then(|dir| {
                    shell_integration::integration_for(
                        &shell,
                        dir,
                        config.login,
                        std::env::var("ZDOTDIR").ok().as_deref(),
                        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
                    )
                })
            } else {
                None
            };
            let suppress_login = integration.as_ref().is_some_and(|i| i.suppress_login_flag);

            if config.login && !suppress_login {
                // Login shell. NOTE: portable-pty's CommandBuilder does not let us
                // override argv[0], so the classic "-<shell>" argv[0] convention
                // isn't available — passing "-zsh" as an argument makes zsh treat
                // it as options ("bad option: -z") and exit. Use the `-l` flag,
                // which zsh/bash/sh all accept. Interactivity comes from stdin
                // being a tty (the pty slave), so no explicit `-i` is needed.
                cmd.arg("-l");
            }
            if let Some(integration) = &integration {
                for arg in &integration.args {
                    cmd.arg(arg);
                }
                for (key, value) in &integration.env {
                    cmd.env(key, value);
                }
            }
            cmd
        };

        cmd.env("TERM", &config.term);
        cmd.env("COLORTERM", "truecolor");
        // Replace any inherited terminal identity with Noa's so child
        // programs can identify the terminal without seeing the parent.
        cmd.env("TERM_PROGRAM", "Noa");
        cmd.env_remove("TERM_PROGRAM_VERSION");
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

        // Independent dup of the master fd for the reader's readability
        // polling (congestion coalescing — see `reader.rs`). Owned by the
        // reader thread so its lifetime is decoupled from this `Pty`'s.
        #[cfg(unix)]
        let poll_fd = master.as_raw_fd().and_then(|raw| {
            use std::os::fd::FromRawFd as _;
            // SAFETY: `raw` is a live fd owned by the master; `dup` returns a
            // fresh, exclusively-owned fd (or -1, handled below).
            let dup = unsafe { libc::dup(raw) };
            (dup >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(dup) })
        });
        #[cfg(not(unix))]
        let poll_fd = None;

        let (tx, event_rx) = pty_event_channel();
        spawn_reader(reader, poll_fd, tx.clone())
            .map_err(|e| PtyError::SpawnThread(e.to_string()))?;
        spawn_waiter(child, tx).map_err(|e| PtyError::SpawnThread(e.to_string()))?;

        let writer = PtyWriter::spawn(writer).map_err(|e| PtyError::SpawnThread(e.to_string()))?;

        Ok(Self {
            master,
            writer,
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

    /// A `Send` probe for the pty's foreground process, usable off the io
    /// thread (FR — running-process display). It holds an independent `dup` of
    /// the master fd so a poller can read the tty's foreground process group
    /// without touching the `Pty` or the io read loop. `None` when the master
    /// exposes no fd or the `dup` fails.
    pub fn foreground_probe(&self) -> Option<ForegroundProcessProbe> {
        ForegroundProcessProbe::from_master(self.master.as_ref())
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

/// A `Send` handle for polling a pty's foreground process, decoupled from the
/// [`Pty`] (which is not `Sync` and lives on the io thread). It owns a `dup` of
/// the master fd, so it can be handed to a background poller and outlives
/// nothing it shouldn't: closing it just drops that extra fd.
pub struct ForegroundProcessProbe {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    /// Foreground-tree CPU-time diff state for [`Self::poll_metrics`]
    /// (panel-metrics-view FR-4): the previous tick's summed tree CPU time
    /// and when it was sampled, so CPU% is the delta over elapsed wall time
    /// (1 core = 100%, ASSUME-1). `None` until a first sample lands, which is
    /// exactly when `poll_metrics` reports `cpu_permille: None` (FR-8).
    /// `prev_pgid` pins the sample to the foreground group it measured: a
    /// job switch between ticks (different pgid) resets to first-sample
    /// semantics instead of diffing across two unrelated trees (which would
    /// read as a bogus spike or 0%).
    prev_pgid: Option<u32>,
    prev_cpu_ns: Option<u64>,
    prev_sampled_at: Option<std::time::Instant>,
}

impl ForegroundProcessProbe {
    #[cfg(unix)]
    fn from_master(master: &(dyn MasterPty + Send)) -> Option<Self> {
        use std::os::fd::FromRawFd;
        let raw = master.as_raw_fd()?;
        // Duplicate so the probe's fd is independent of the master's lifetime.
        // SAFETY: `raw` is a live fd owned by the master; `dup` returns a new
        // owned fd (or -1, handled below).
        let dup = unsafe { libc::dup(raw) };
        if dup < 0 {
            return None;
        }
        // SAFETY: `dup` is a fresh, exclusively-owned fd from `libc::dup`.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(dup) };
        Some(Self {
            fd,
            prev_pgid: None,
            prev_cpu_ns: None,
            prev_sampled_at: None,
        })
    }

    #[cfg(not(unix))]
    fn from_master(_master: &(dyn MasterPty + Send)) -> Option<Self> {
        None
    }

    /// The name of the tty's foreground process — the leader of the current
    /// foreground process group (e.g. `zsh`, `cargo`, `claude`). Known generic
    /// runtime wrappers (`bun`, `node`, …) are canonicalized only when their
    /// argv identifies a direct Codex launch. `None` when there is no foreground
    /// group (the session ended) or on a platform without the query (only macOS
    /// is implemented; NFR-5 graceful degradation).
    pub fn poll(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            foreground_process_name(self.fd.as_raw_fd())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// Poll the tty's foreground process-group *tree* metrics
    /// (panel-metrics-view FR-4/FR-7/FR-8): CPU% (1 core = 100%, `None`
    /// before this probe's first sample), summed physical-footprint memory,
    /// process count, and the tree's start time. `snap` is a whole-process
    /// snapshot the caller captures once per tick and shares across every
    /// pane's probe (NFR-2) — this call makes no syscalls of its own beyond
    /// `tcgetpgrp` and one `proc_pid_rusage` per tree member. `None` when
    /// there is no foreground group (session ended) or off macOS (FR-8,
    /// AC-15).
    pub fn poll_metrics(&mut self, snap: &crate::ProcSnapshot) -> Option<crate::PaneMetrics> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let pgid = unsafe { libc::tcgetpgrp(self.fd.as_raw_fd()) };
            if pgid <= 0 {
                self.reset_cpu_diff();
                return None;
            }
            let pgid = pgid as u32;
            // A foreground job switch since the last tick: the stored CPU sum
            // belongs to a different tree, so drop back to first-sample
            // semantics (`cpu_permille: None`) rather than diffing across it.
            if self.prev_pgid != Some(pgid) {
                self.reset_cpu_diff();
            }
            let tree = crate::foreground_tree(pgid, &snap.procs);
            if tree.is_empty() {
                self.reset_cpu_diff();
                return None;
            }

            let mut cpu_ns_total: u64 = 0;
            let mut mem_bytes: u64 = 0;
            for &pid in &tree {
                // A pid that vanished between the snapshot and this rusage
                // call contributes 0 rather than aborting the tree sum (FR-8).
                if let Some((cpu_ns, footprint)) = crate::metrics::rusage_ns_and_footprint(pid) {
                    cpu_ns_total = cpu_ns_total.saturating_add(cpu_ns);
                    mem_bytes = mem_bytes.saturating_add(footprint);
                }
            }

            let now = std::time::Instant::now();
            let cpu_permille = match (self.prev_cpu_ns, self.prev_sampled_at) {
                (Some(prev_ns), Some(prev_at)) => {
                    let elapsed_ns = now.duration_since(prev_at).as_nanos();
                    (elapsed_ns > 0).then(|| {
                        let delta_ns = cpu_ns_total.saturating_sub(prev_ns) as u128;
                        ((delta_ns * 1000) / elapsed_ns) as u32
                    })
                }
                _ => None,
            };
            self.prev_pgid = Some(pgid);
            self.prev_cpu_ns = Some(cpu_ns_total);
            self.prev_sampled_at = Some(now);

            let started_at = crate::metrics::tree_started_at(pgid, &snap.procs, &tree);
            Some(crate::PaneMetrics {
                cpu_permille,
                mem_bytes,
                proc_count: tree.len() as u32,
                started_at,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// Drop the CPU-diff state back to first-sample semantics (no foreground
    /// group, an empty tree, or a foreground job switch).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn reset_cpu_diff(&mut self) {
        self.prev_pgid = None;
        self.prev_cpu_ns = None;
        self.prev_sampled_at = None;
    }
}

impl std::fmt::Debug for ForegroundProcessProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForegroundProcessProbe")
            .finish_non_exhaustive()
    }
}

/// The foreground process name for `fd`'s tty (macOS): `tcgetpgrp` gives the
/// foreground process-group id, then `proc_name` maps that pid to its command
/// name. Any failure (no foreground group, dead pid) degrades to `None`.
#[cfg(target_os = "macos")]
fn foreground_process_name(fd: std::os::fd::RawFd) -> Option<String> {
    // SAFETY: `fd` is a valid tty fd; `tcgetpgrp` returns -1 on error.
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    if pgid <= 0 {
        return None;
    }
    // `proc_name` copies the (NUL-terminated) accounting name; its buffer is up
    // to 2*MAXCOMLEN. SAFETY: valid pid + writable buffer + its length.
    let mut buf = [0u8; libc::MAXCOMLEN * 2 + 1];
    let len = unsafe {
        libc::proc_name(
            pgid,
            buf.as_mut_ptr() as *mut libc::c_void,
            (buf.len() - 1) as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    let name = std::str::from_utf8(&buf[..len as usize]).ok()?.trim();
    if name.is_empty() {
        return None;
    }
    let args = if wrapper_can_host_codex(name) {
        foreground_process_args(pgid)
    } else {
        None
    };
    Some(canonical_process_name(name, args.as_deref()))
}

#[cfg(target_os = "macos")]
fn foreground_process_args(pid: libc::pid_t) -> Option<Vec<String>> {
    let arg_max = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    if arg_max <= 0 {
        return None;
    }
    let mut buf = vec![0u8; arg_max as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

fn canonical_process_name(name: &str, args: Option<&[String]>) -> String {
    if wrapper_can_host_codex(name) && args.is_some_and(argv_mentions_codex) {
        return "codex".to_string();
    }
    name.to_string()
}

fn wrapper_can_host_codex(name: &str) -> bool {
    matches!(
        executable_basename(name).as_str(),
        "bun" | "bunx" | "node" | "npx"
    )
}

fn argv_mentions_codex(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let lower = arg.trim().to_ascii_lowercase();
        lower == "@openai/codex"
            || lower.starts_with("@openai/codex@")
            || lower.starts_with("@openai/codex/")
            || lower.contains("/@openai/codex/")
            || lower.ends_with("/@openai/codex")
            || looks_like_codex_executable(&lower)
    })
}

fn looks_like_codex_executable(value: &str) -> bool {
    let base = executable_basename(value);
    let mut tokens = base
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty());
    let first = tokens.next().unwrap_or(&base);
    if first == "openai" && tokens.next() == Some("codex") {
        return true;
    }
    first == "codex"
}

fn executable_basename(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
}

#[cfg(target_os = "macos")]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc_len = std::mem::size_of::<libc::c_int>();
    if buf.len() <= argc_len {
        return None;
    }
    let argc = libc::c_int::from_ne_bytes(buf[..argc_len].try_into().ok()?) as usize;
    if argc == 0 {
        return None;
    }

    let mut index = argc_len;
    while index < buf.len() && buf[index] != 0 {
        index += 1;
    }
    while index < buf.len() && buf[index] == 0 {
        index += 1;
    }

    let mut args = Vec::new();
    while index < buf.len() && args.len() < argc {
        let start = index;
        while index < buf.len() && buf[index] != 0 {
            index += 1;
        }
        if start < index {
            let arg = std::str::from_utf8(&buf[start..index]).ok()?.to_string();
            args.push(arg);
        }
        while index < buf.len() && buf[index] == 0 {
            index += 1;
        }
    }

    (!args.is_empty()).then_some(args)
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
            command: None,
            cwd: None,
            term: "xterm-256color".to_string(),
            login: false,
            shell_integration: false,
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

    // `-e` path: an explicit command argv is spawned verbatim as the pty
    // child (no shell wrapping, args passed through) and its exit is
    // delivered like any child's.
    #[test]
    fn explicit_command_argv_runs_instead_of_the_shell() {
        let cfg = PtyConfig {
            size: GridSize::new(80, 24),
            // A shell that would loop forever proves `command` wins over it.
            shell: Some("/bin/zsh".to_string()),
            command: Some(vec![
                "/bin/echo".to_string(),
                "argv-passthrough".to_string(),
            ]),
            cwd: None,
            term: "xterm-256color".to_string(),
            login: true,
            shell_integration: true,
        };
        let pty = Pty::spawn(cfg).expect("spawn pty");

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
            text.contains("argv-passthrough"),
            "expected the command's own output: {text:?}"
        );
        assert!(saw_exit, "expected an Exit event when the command finishes");
    }

    #[test]
    fn terminal_environment_is_exported_to_the_child() {
        // noa-vt fully supports 24-bit truecolor SGR, so the child should see
        // COLORTERM=truecolor alongside TERM (needed by tools that gate
        // truecolor support on COLORTERM rather than the terminfo entry).
        let cfg = PtyConfig {
            size: GridSize::new(80, 24),
            shell: Some("/bin/sh".to_string()),
            command: None,
            cwd: None,
            term: "xterm-256color".to_string(),
            login: false,
            shell_integration: false,
        };
        let pty = Pty::spawn(cfg).expect("spawn pty");

        let w = pty.writer();
        w.write(b"echo \"$TERM $COLORTERM $TERM_PROGRAM\"\nexit\n")
            .expect("write");
        w.flush().expect("flush");

        let mut collected = Vec::new();
        let deadline = Duration::from_secs(5);
        loop {
            match pty.event_rx().recv_timeout(deadline) {
                Ok(PtyEvent::Data(chunk)) => collected.extend_from_slice(&chunk),
                Ok(PtyEvent::Exit(_)) => break,
                Ok(PtyEvent::Error(e)) => panic!("pty error: {e}"),
                Err(_) => break, // timeout guard so we never hang
            }
        }

        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains("xterm-256color truecolor Noa"),
            "expected Noa's terminal environment in child: {text:?}"
        );
    }

    #[test]
    fn foreground_probe_reports_the_shell_process() {
        // The probe reads the tty's foreground process group leader, which for a
        // freshly spawned interactive shell is the shell itself. macOS-only; on
        // other platforms `poll` degrades to `None` (NFR-5).
        let pty = Pty::spawn(PtyConfig {
            size: GridSize::new(80, 24),
            shell: Some("/bin/sh".to_string()),
            command: None,
            cwd: None,
            term: "xterm-256color".to_string(),
            login: false,
            shell_integration: false,
        })
        .expect("spawn");
        let probe = pty.foreground_probe().expect("a unix master exposes an fd");

        if !cfg!(target_os = "macos") {
            return; // poll() is a documented `None` off macOS.
        }

        // Wait for the child to become the tty's foreground group leader (it
        // sets its controlling terminal in a pre-exec step, so immediately after
        // spawn the foreground group can still be this test's).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut last = None;
        while std::time::Instant::now() < deadline {
            let name = probe.poll();
            if name.as_deref().is_some_and(|n| n.contains("sh")) {
                return; // saw the shell as the foreground process
            }
            last = name;
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("never saw the shell as the foreground process; last = {last:?}");
    }

    #[test]
    fn canonical_process_name_detects_codex_through_runtime_wrappers() {
        assert_eq!(
            canonical_process_name(
                "bun",
                Some(&[
                    "bun".to_string(),
                    "x".to_string(),
                    "@openai/codex".to_string()
                ])
            ),
            "codex"
        );
        assert_eq!(
            canonical_process_name(
                "node",
                Some(&[
                    "node".to_string(),
                    "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js".to_string()
                ])
            ),
            "codex"
        );
        assert_eq!(
            canonical_process_name(
                "bun",
                Some(&["bun".to_string(), "/tmp/openai-codex".to_string()])
            ),
            "codex"
        );

        assert_eq!(
            canonical_process_name(
                "bun",
                Some(&["bun".to_string(), "run".to_string(), "build".to_string()])
            ),
            "bun"
        );
        assert_eq!(
            canonical_process_name(
                "bun",
                Some(&["bun".to_string(), "my-openai-codex".to_string()])
            ),
            "bun"
        );
        assert_eq!(
            canonical_process_name("node", Some(&["node".to_string(), "codexify".to_string()])),
            "node"
        );
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
    fn zsh_shell_integration_emits_osc133_and_osc7() {
        // End-to-end: spawn real zsh with integration and confirm the injected
        // hooks emit OSC 133 prompt marks and an OSC 7 cwd report. Skips where
        // zsh isn't installed so the suite stays portable.
        if !std::path::Path::new("/bin/zsh").exists() {
            return;
        }
        let pty = Pty::spawn(PtyConfig {
            shell: Some("/bin/zsh".to_string()),
            login: true,
            shell_integration: true,
            ..Default::default()
        })
        .expect("spawn");

        let w = pty.writer();
        w.write(b"print noa-marker\n").expect("write");
        w.flush().expect("flush");

        let mut collected = Vec::new();
        let start = std::time::Instant::now();
        // zsh's interactive init (async plugins/prompt) can pause between
        // chunks, so keep polling until the overall deadline rather than
        // stopping on the first idle gap.
        while start.elapsed() < Duration::from_secs(8) {
            match pty.event_rx().recv_timeout(Duration::from_millis(300)) {
                Ok(PtyEvent::Data(chunk)) => {
                    collected.extend_from_slice(&chunk);
                    if collected.windows(7).any(|w| w == b"\x1b]133;A")
                        && collected
                            .windows(22)
                            .any(|w| w == b"\x1b]7;kitty-shell-cwd://".as_ref())
                    {
                        break;
                    }
                }
                Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
                Err(_) => {} // idle gap during shell init — keep waiting
            }
        }
        w.write(b"exit\n").ok();

        assert!(
            collected.windows(7).any(|w| w == b"\x1b]133;A"),
            "expected an OSC 133;A prompt mark in zsh output"
        );
        assert!(
            collected
                .windows(22)
                .any(|w| w == b"\x1b]7;kitty-shell-cwd://"),
            "expected an OSC 7 cwd report in zsh output"
        );
    }

    #[test]
    fn bash_shell_integration_emits_osc133_and_osc7() {
        // End-to-end: spawn real bash with integration and confirm the hooks
        // emit OSC 133 + OSC 7. Skips where bash isn't installed.
        let bash = ["/bin/bash", "/opt/homebrew/bin/bash", "/usr/local/bin/bash"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists());
        let Some(bash) = bash else {
            return;
        };
        let pty = Pty::spawn(PtyConfig {
            shell: Some(bash.to_string()),
            login: true,
            shell_integration: true,
            ..Default::default()
        })
        .expect("spawn");

        let w = pty.writer();
        w.write(b"echo noa-marker\n").expect("write");
        w.flush().expect("flush");

        let mut collected = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            match pty.event_rx().recv_timeout(Duration::from_millis(300)) {
                Ok(PtyEvent::Data(chunk)) => {
                    collected.extend_from_slice(&chunk);
                    if collected.windows(7).any(|w| w == b"\x1b]133;A")
                        && collected
                            .windows(22)
                            .any(|w| w == b"\x1b]7;kitty-shell-cwd://".as_ref())
                    {
                        break;
                    }
                }
                Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
                Err(_) => {}
            }
        }
        w.write(b"exit\n").ok();

        assert!(
            collected.windows(7).any(|w| w == b"\x1b]133;A"),
            "expected an OSC 133;A prompt mark in bash output"
        );
        assert!(
            collected
                .windows(22)
                .any(|w| w == b"\x1b]7;kitty-shell-cwd://"),
            "expected an OSC 7 cwd report in bash output"
        );
    }

    #[test]
    fn write_never_blocks_on_a_raw_mode_child_that_stops_reading() {
        // Regression: pty writes used to run as a blocking `write_all` on the
        // caller's thread (the io read loop). macOS caps the raw-mode tty
        // input queue at ~1KB, so >1KB of input (a paste) against a child not
        // reading stdin blocked the write — freezing the pane's reads and
        // redraws, and deadlocking permanently once the child also blocked
        // writing output. Writes must queue to the writer thread and return
        // immediately instead.
        let pty = Pty::spawn(PtyConfig {
            shell: Some("/bin/sh".to_string()),
            login: false,
            shell_integration: false,
            ..Default::default()
        })
        .expect("spawn");
        let w = pty.writer();

        // Put the slave tty in raw mode and stop reading stdin, like a busy
        // full-screen app (vim/fzf/an AI CLI) mid-computation.
        w.write(b"stty raw -echo; sleep 5\n")
            .expect("write command");
        std::thread::sleep(Duration::from_millis(500));

        let start = std::time::Instant::now();
        w.write(&vec![b'x'; 4096]).expect("write 4KB");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a 4KB write against a non-reading raw-mode child must not block the caller"
        );
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
