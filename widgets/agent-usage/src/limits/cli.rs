//! Finds and runs the command-line tools the limits sources ask (`claude`,
//! `codex`, `agy`, `curl`, `security`).
//!
//! Noa started from Finder has a bare PATH, so a tool is looked up on the
//! inherited PATH, then the usual install prefixes, then (once, cached) the
//! login shell's PATH. Every child runs in its own process group with a
//! timeout, stdin closed unless used, stderr dropped and stdout capped; on a
//! timeout or cancellation the whole group is killed and the child reaped.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub type Env = HashMap<String, String>;

/// Set when the limits handle is dropped: running children are killed.
pub type Cancel = Arc<AtomicBool>;

#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub path: PathBuf,
    /// The inherited environment with PATH set to the search path that found
    /// the tool, so its own children (node, a version manager shim) resolve.
    pub env: Env,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunFailure {
    /// Could not be started (or the run was cancelled before it started).
    NotRun,
    TimedOut,
    /// An exit status not accepted, or the signal number of a signalled child.
    Exit(i32),
    TooLarge,
}

/// stdout cap for every child.
pub const MAX_OUTPUT: usize = 1 << 20;

/// Marker the login shell prints before its PATH.
const PATH_MARKER: &str = "noa-usage-path=";
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

fn default_prefixes(home: &str) -> Vec<String> {
    vec![
        format!("{home}/.local/bin"),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        format!("{home}/.npm-global/bin"),
        format!("{home}/.bun/bin"),
        format!("{home}/.cargo/bin"),
    ]
}

type ShellPathFn = Box<dyn Fn() -> Vec<String> + Send + Sync>;

#[derive(Default)]
struct ResolverState {
    resolved: HashMap<String, Resolution>,
    /// The login shell's PATH, asked once.
    shell: Option<Vec<String>>,
}

pub struct Resolver {
    inherited: Env,
    prefixes: Vec<String>,
    shell_path: ShellPathFn,
    state: Mutex<ResolverState>,
}

impl Resolver {
    pub fn new(
        inherited: Env,
        prefixes: Vec<String>,
        shell_path: impl Fn() -> Vec<String> + Send + Sync + 'static,
    ) -> Resolver {
        Resolver {
            inherited,
            prefixes,
            shell_path: Box::new(shell_path),
            state: Mutex::default(),
        }
    }

    pub fn resolve(&self, tool: &str) -> Option<Resolution> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = state.resolved.get(tool) {
            return Some(hit.clone());
        }
        let inherited = self
            .inherited
            .get("PATH")
            .map(|p| p.split(':').map(String::from).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut search = dedupe(inherited.into_iter().chain(self.prefixes.iter().cloned()));
        let mut hit = executable(tool, &search);
        if hit.is_none() {
            let shell = state
                .shell
                .get_or_insert_with(|| (self.shell_path)())
                .clone();
            search = dedupe(search.into_iter().chain(shell));
            hit = executable(tool, &search);
        }
        let mut env = self.inherited.clone();
        env.insert("PATH".into(), search.join(":"));
        let resolution = Resolution { path: hit?, env };
        state.resolved.insert(tool.to_string(), resolution.clone());
        Some(resolution)
    }
}

fn dedupe(paths: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .filter(|p| p.starts_with('/') && seen.insert(p.clone()))
        .collect()
}

pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn executable(tool: &str, dirs: &[String]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| Path::new(d).join(tool))
        .find(|p| is_executable(p))
}

/// The PATH the login shell printed after the marker; empty when absent.
pub fn parse_login_shell_path(output: &str) -> Vec<String> {
    output
        .lines()
        .rev()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix(PATH_MARKER))
        .map(|p| {
            p.split(':')
                .filter(|d| d.starts_with('/'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn login_shell_path() -> Vec<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| "/bin/sh".into());
    let script = format!("printf \"\\n{PATH_MARKER}%s\\n\" \"$PATH\"");
    match run(
        Path::new(&shell),
        &["-lic", &script],
        None,
        None,
        SHELL_TIMEOUT,
        &[0],
        None,
    ) {
        Ok(out) => parse_login_shell_path(&String::from_utf8_lossy(&out)),
        Err(_) => Vec::new(),
    }
}

/// The process-wide resolver (inherited env, usual prefixes, login shell).
pub fn resolve(tool: &str) -> Option<Resolution> {
    static RESOLVER: OnceLock<Resolver> = OnceLock::new();
    RESOLVER
        .get_or_init(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            Resolver::new(
                std::env::vars().collect(),
                default_prefixes(&home),
                login_shell_path,
            )
        })
        .resolve(tool)
}

/// A spawned child in its own process group; dropping it kills the group and
/// reaps the child.
pub struct Guarded {
    child: Option<Child>,
}

impl Guarded {
    /// Spawns `command` as the leader of a new process group.
    pub fn spawn(mut command: Command) -> std::io::Result<Guarded> {
        command.process_group(0);
        Ok(Guarded {
            child: Some(command.spawn()?),
        })
    }

    pub fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("child is present until drop")
    }

    /// Whether the leader has exited, without reaping it: while it is an
    /// unreaped zombie its pid (and so the group id) cannot be reused.
    pub fn has_exited(&mut self) -> bool {
        exited_unreaped(self.child()) != Some(false)
    }

    /// Kills whatever is left of the group (descendants holding pipes) and
    /// reaps the leader.
    pub fn reap(mut self) -> std::process::ExitStatus {
        let mut child = self.child.take().expect("child is present until drop");
        if exited_unreaped(&child).is_some() {
            kill_group(&child, libc::SIGKILL);
        }
        match child.wait() {
            Ok(status) => status,
            Err(_) => std::process::ExitStatus::from_raw(-1),
        }
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate(&mut child);
        }
    }
}

/// `Some(exited)` for an unreaped child; `None` once it has been reaped
/// (its pid, and so the group id, may then belong to someone else).
fn exited_unreaped(child: &Child) -> Option<bool> {
    // SAFETY: zeroed siginfo is a valid out-parameter; WNOWAIT leaves the
    // child waitable.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let rc = libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        // WNOHANG with nothing to report leaves si_pid zero.
        (rc == 0).then(|| info.si_pid() != 0)
    }
}

fn kill_group(child: &Child, signal: libc::c_int) {
    // SAFETY: called only while the leader is unreaped, so the group id is
    // still ours and reaches only its members.
    unsafe { libc::kill(-(child.id() as libc::pid_t), signal) };
}

/// SIGTERM to the group, SIGKILL after 200 ms, then reap.
pub fn terminate(child: &mut Child) {
    match exited_unreaped(child) {
        None => return,
        Some(false) => {
            kill_group(child, libc::SIGTERM);
            let until = Instant::now() + Duration::from_millis(200);
            while Instant::now() < until && exited_unreaped(child) == Some(false) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Some(true) => {}
    }
    kill_group(child, libc::SIGKILL);
    let _ = child.wait();
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().or(status.signal()).unwrap_or(-1)
}

/// Runs a tool and returns its stdout. `env` replaces the environment when
/// given. `stdin` is written then closed (the null device when `None`). The
/// read is non-blocking so a descendant holding stdout open cannot defeat the
/// deadline; output is never written anywhere.
pub fn run(
    path: &Path,
    args: &[&str],
    env: Option<&Env>,
    stdin: Option<&[u8]>,
    timeout: Duration,
    ok_codes: &[i32],
    cancel: Option<&Cancel>,
) -> Result<Vec<u8>, RunFailure> {
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::SeqCst));
    if cancelled() {
        return Err(RunFailure::NotRun);
    }
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    let mut guard = Guarded::spawn(command).map_err(|_| RunFailure::NotRun)?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, guard.child().stdin.take()) {
        // Small (a curl config); a child that exits without reading gives EPIPE.
        let _ = pipe.write_all(bytes);
    }
    let mut stdout = guard.child().stdout.take().expect("stdout is piped");
    let fd = stdout.as_raw_fd();
    // SAFETY: fd belongs to `stdout`, open for the whole loop.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let status = loop {
        if cancelled() {
            return Err(RunFailure::NotRun);
        }
        if Instant::now() >= deadline {
            return Err(RunFailure::TimedOut);
        }
        match stdout.read(&mut buf) {
            Ok(n) if n > 0 => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_OUTPUT {
                    return Err(RunFailure::TooLarge);
                }
                continue;
            }
            _ => {
                if guard.has_exited() {
                    // Whatever is still buffered in the pipe.
                    while let Ok(n) = stdout.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        out.extend_from_slice(&buf[..n]);
                        if out.len() > MAX_OUTPUT {
                            return Err(RunFailure::TooLarge);
                        }
                    }
                    break guard.reap();
                }
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd, bounded wait. A closed pipe polls
                // as POLLHUP at once, so sleep instead of spinning.
                let ready = unsafe { libc::poll(&mut pfd, 1, 20) };
                if ready > 0 && pfd.revents & libc::POLLIN == 0 {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    };
    if status.code().is_none() || !ok_codes.contains(&exit_code(status)) {
        return Err(RunFailure::Exit(exit_code(status)));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes an executable shell script into `dir`.
    pub(crate) fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn timeout_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        // The child ignores TERM and leaves a grandchild in its group.
        let body = format!(
            "trap '' TERM\nsleep 30 &\necho $$ $! > '{}'\nwait",
            pids.display()
        );
        let tool = script(dir.path(), "hang", &body);
        let start = Instant::now();
        let result = run(
            &tool,
            &[],
            None,
            None,
            Duration::from_millis(300),
            &[0],
            None,
        );
        assert_eq!(result, Err(RunFailure::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(3));
        let text = std::fs::read_to_string(&pids).unwrap();
        let ids: Vec<libc::pid_t> = text
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        // Child reaped by us (no zombie); grandchild killed (and reaped by
        // init, which may take a moment).
        assert!(!alive(ids[0]), "child still exists");
        let until = Instant::now() + Duration::from_secs(3);
        while alive(ids[1]) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(ids[1]), "grandchild survived");
    }

    #[test]
    fn cancellation_kills_a_running_child() {
        let cancel: Cancel = Arc::default();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = run(
            Path::new("/bin/sh"),
            &["-c", "trap '' TERM; sleep 10"],
            None,
            None,
            Duration::from_secs(10),
            &[0],
            Some(&cancel),
        );
        assert_eq!(result, Err(RunFailure::NotRun));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn exit_codes_stdin_and_output_cap() {
        let sh = Path::new("/bin/sh");
        let t = Duration::from_secs(5);
        assert_eq!(
            run(sh, &["-c", "printf x; exit 3"], None, None, t, &[0], None),
            Err(RunFailure::Exit(3))
        );
        assert_eq!(
            run(
                sh,
                &["-c", "printf x; exit 3"],
                None,
                None,
                t,
                &[0, 3],
                None
            ),
            Ok(b"x".to_vec())
        );
        assert_eq!(
            run(sh, &["-c", "cat"], None, Some(b"secret-in"), t, &[0], None),
            Ok(b"secret-in".to_vec())
        );
        assert_eq!(
            run(sh, &["-c", "kill -TERM $$"], None, None, t, &[0, 15], None),
            Err(RunFailure::Exit(15))
        );
        assert_eq!(
            run(
                sh,
                &["-c", "head -c 2000000 /dev/zero"],
                None,
                None,
                t,
                &[0],
                None
            ),
            Err(RunFailure::TooLarge)
        );
        assert_eq!(
            run(
                Path::new("/nonexistent/tool"),
                &[],
                None,
                None,
                t,
                &[0],
                None
            ),
            Err(RunFailure::NotRun)
        );
        // A descendant holding stdout open does not hold the run.
        let start = Instant::now();
        assert_eq!(
            run(
                sh,
                &["-c", "(sleep 3) & printf done"],
                None,
                None,
                t,
                &[0],
                None
            ),
            Ok(b"done".to_vec())
        );
        assert!(start.elapsed() < Duration::from_millis(1500));
    }

    #[test]
    fn login_shell_path_is_parsed_from_marker() {
        let out = "motd line\nnoa-usage-path=not-this\n\x1b]7;x\x07\nnoa-usage-path=/a/bin:relative:/b/bin:\n";
        assert_eq!(parse_login_shell_path(out), ["/a/bin", "/b/bin"]);
        assert!(parse_login_shell_path("no marker\n").is_empty());
        // The resolver asks the shell only when the usual search misses, once.
        let dir = tempfile::tempdir().unwrap();
        let login = dir.path().join("login");
        std::fs::create_dir(&login).unwrap();
        script(&login, "fake-tool", "exit 0");
        let asked = Arc::new(Mutex::new(0));
        let count = asked.clone();
        let login_dir = login.to_string_lossy().into_owned();
        let resolver = Resolver::new(
            [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
            vec![],
            move || {
                *count.lock().unwrap() += 1;
                parse_login_shell_path(&format!("\n{PATH_MARKER}{login_dir}\n"))
            },
        );
        assert!(resolver.resolve("sh").is_some());
        assert_eq!(*asked.lock().unwrap(), 0);
        let hit = resolver.resolve("fake-tool").unwrap();
        assert_eq!(hit.path, login.join("fake-tool"));
        assert!(hit.env["PATH"].ends_with(&login.to_string_lossy().into_owned()));
        assert!(resolver.resolve("no-such-tool-anywhere").is_none());
        assert_eq!(*asked.lock().unwrap(), 1);
    }
}
