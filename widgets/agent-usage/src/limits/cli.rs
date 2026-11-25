//! Finds and runs the command-line tools the limits sources ask (`claude`,
//! `codex`, `agy`, `curl`, `security`).
//!
//! Noa started from Finder has a bare PATH, so a tool is looked up on the
//! inherited PATH, then the usual install prefixes, then (once per handle,
//! cached) the login shell's PATH. Lookups run on the asking source's thread
//! and never wait for one another, except that a tool missing everywhere else
//! waits for the one login-shell probe, which stops when the handle is
//! dropped. Every child runs in its own process group with a timeout, stdin
//! closed unless used, stderr dropped and stdout capped; on a timeout or
//! cancellation the whole group is killed and the child reaped.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A child's environment. Values may be any bytes (read with `vars_os`, so a
/// non-UTF-8 variable is carried, not a panic) and are never formatted:
/// `Debug` shows only how many there are.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env(HashMap<OsString, OsString>);

impl Env {
    /// This process's environment.
    pub fn inherited() -> Env {
        std::env::vars_os().collect()
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.0.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    pub fn set(&mut self, name: &str, value: impl Into<OsString>) {
        self.0.insert(name.into(), value.into());
    }

    pub fn remove(&mut self, name: &str) {
        self.0.remove(OsStr::new(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.0.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }
}

impl<K: Into<OsString>, V: Into<OsString>> FromIterator<(K, V)> for Env {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Env {
        Env(iter
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect())
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Env(<{} variables>)", self.0.len())
    }
}

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
    /// Could not be started (or the run was cancelled).
    NotRun,
    TimedOut,
    /// An exit status not accepted.
    Exit(i32),
    /// Killed by this signal.
    Signal(i32),
    TooLarge,
}

/// stdout cap for every child.
pub const MAX_OUTPUT: usize = 1 << 20;

/// Marker the login shell prints before its PATH.
const PATH_MARKER: &str = "noa-usage-path=";
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

fn default_prefixes(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin"),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        home.join(".npm-global/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
    ]
}

type ShellPathFn = Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// Tool lookups for one limits handle, shared by its source threads.
pub struct Resolver {
    inherited: Env,
    prefixes: Vec<PathBuf>,
    shell_path: ShellPathFn,
    /// Locked only to read or store a hit, never across a search.
    resolved: Mutex<HashMap<String, Resolution>>,
    /// The login shell's PATH, asked once; only a lookup that missed
    /// everywhere else waits for it.
    shell: OnceLock<Vec<PathBuf>>,
}

impl Resolver {
    pub fn new(
        inherited: Env,
        prefixes: Vec<PathBuf>,
        shell_path: impl Fn() -> Vec<PathBuf> + Send + Sync + 'static,
    ) -> Resolver {
        Resolver {
            inherited,
            prefixes,
            shell_path: Box::new(shell_path),
            resolved: Mutex::default(),
            shell: OnceLock::new(),
        }
    }

    /// The inherited environment, the usual prefixes and `$SHELL -lic`,
    /// whose probe is killed once `cancel` is set. Runs nothing itself.
    pub fn system(cancel: Cancel) -> Resolver {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|s| s.is_absolute())
            .unwrap_or_else(|| "/bin/sh".into());
        Resolver::new(Env::inherited(), default_prefixes(&home), move || {
            login_shell_path(&shell, &cancel)
        })
    }

    /// The environment children get when no lookup was needed.
    pub fn inherited(&self) -> &Env {
        &self.inherited
    }

    fn hits(&self) -> std::sync::MutexGuard<'_, HashMap<String, Resolution>> {
        self.resolved.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn resolve(&self, tool: &str) -> Option<Resolution> {
        if let Some(hit) = self.hits().get(tool) {
            return Some(hit.clone());
        }
        let inherited: Vec<PathBuf> = self
            .inherited
            .get("PATH")
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        let mut search = dedupe(inherited.into_iter().chain(self.prefixes.iter().cloned()));
        let mut hit = executable(tool, &search);
        if hit.is_none() {
            let shell = self.shell.get_or_init(|| (self.shell_path)());
            search = dedupe(search.into_iter().chain(shell.iter().cloned()));
            hit = executable(tool, &search);
        }
        let mut env = self.inherited.clone();
        env.set("PATH", join_paths(&search));
        let resolution = Resolution { path: hit?, env };
        self.hits().insert(tool.to_string(), resolution.clone());
        Some(resolution)
    }
}

fn dedupe(paths: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .filter(|p| p.is_absolute() && seen.insert(p.clone()))
        .collect()
}

fn join_paths(paths: &[PathBuf]) -> OsString {
    let mut out = OsString::new();
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            out.push(":");
        }
        out.push(p);
    }
    out
}

pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn executable(tool: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(tool)).find(|p| is_executable(p))
}

/// The PATH the login shell printed after the marker; empty when absent.
pub fn parse_login_shell_path(output: &[u8]) -> Vec<PathBuf> {
    output
        .split(|b| *b == b'\n')
        .rev()
        .find_map(|line| {
            line.strip_suffix(b"\r")
                .unwrap_or(line)
                .strip_prefix(PATH_MARKER.as_bytes())
        })
        .map(|p| {
            std::env::split_paths(OsStr::from_bytes(p))
                .filter(|d| d.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// `shell -lic` printing its PATH, killed after 5 s or once `cancel` is set.
pub fn login_shell_path(shell: &Path, cancel: &Cancel) -> Vec<PathBuf> {
    let script = format!("printf \"\\n{PATH_MARKER}%s\\n\" \"$PATH\"");
    match run(
        shell,
        &["-lic", &script],
        None,
        None,
        SHELL_TIMEOUT,
        &[0],
        Some(cancel),
    ) {
        Ok(out) => parse_login_shell_path(&out),
        Err(_) => Vec::new(),
    }
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
        command.env_clear().envs(env.iter());
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
    match status.code() {
        Some(code) if ok_codes.contains(&code) => Ok(out),
        Some(code) => Err(RunFailure::Exit(code)),
        None => Err(RunFailure::Signal(status.signal().unwrap_or(-1))),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes an executable shell script into `dir`.
    pub(crate) fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ -n \"$AGENT_USAGE_TEST_WARM\" ] && exit 0\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // macOS checks a newly written executable the first time it runs,
        // which has taken about 0.4 s here: longer than the short timeouts
        // these tests give the script. Run it once, exiting at once, so that
        // check happens before any timed run.
        let warm = Command::new(&path)
            .env("AGENT_USAGE_TEST_WARM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(warm.success(), "warm-up run of {}", path.display());
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
        // A signal is not an exit status, whatever the accepted codes.
        assert_eq!(
            run(sh, &["-c", "kill -TERM $$"], None, None, t, &[0, 15], None),
            Err(RunFailure::Signal(15))
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
        let out = b"motd line\nnoa-usage-path=not-this\n\x1b]7;x\x07\nnoa-usage-path=/a/bin:relative:/b/bin:\n";
        assert_eq!(
            parse_login_shell_path(out),
            [PathBuf::from("/a/bin"), PathBuf::from("/b/bin")]
        );
        assert!(parse_login_shell_path(b"no marker\n").is_empty());
        // The resolver asks the shell only when the usual search misses, once.
        let dir = tempfile::tempdir().unwrap();
        let login = dir.path().join("login");
        std::fs::create_dir(&login).unwrap();
        script(&login, "fake-tool", "exit 0");
        let asked = Arc::new(Mutex::new(0));
        let count = asked.clone();
        let login_dir = login.clone();
        let resolver = Resolver::new(
            [("PATH", "/usr/bin:/bin")].into_iter().collect(),
            vec![],
            move || {
                *count.lock().unwrap() += 1;
                let mut out = format!("\n{PATH_MARKER}").into_bytes();
                out.extend_from_slice(login_dir.as_os_str().as_bytes());
                parse_login_shell_path(&out)
            },
        );
        assert!(resolver.resolve("sh").is_some());
        assert_eq!(*asked.lock().unwrap(), 0);
        let hit = resolver.resolve("fake-tool").unwrap();
        assert_eq!(hit.path, login.join("fake-tool"));
        let path = hit.env.get("PATH").unwrap().as_bytes();
        assert!(path.ends_with(login.as_os_str().as_bytes()));
        assert!(resolver.resolve("no-such-tool-anywhere").is_none());
        assert_eq!(*asked.lock().unwrap(), 1);
    }

    #[test]
    fn resolution_debug_hides_environment_values() {
        const TOKEN: &str = "sk-ant-oat01-SYNTHETIC-env-token";
        let env: Env = [
            ("CLAUDE_CODE_OAUTH_TOKEN", TOKEN),
            ("PATH", "/synthetic/secret-bin"),
        ]
        .into_iter()
        .collect();
        let r = Resolution {
            path: PathBuf::from("/usr/bin/curl"),
            env: env.clone(),
        };
        for text in [format!("{r:?}"), format!("{r:#?}"), format!("{env:?}")] {
            assert!(!text.contains(TOKEN), "{text}");
            assert!(!text.contains("/synthetic/secret-bin"), "{text}");
        }
        assert!(format!("{r:?}").contains("/usr/bin/curl"));
        // The child still gets the values.
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "printf %s \"$CLAUDE_CODE_OAUTH_TOKEN\""],
            Some(&env),
            None,
            Duration::from_secs(5),
            &[0],
            None,
        );
        assert_eq!(out, Ok(TOKEN.as_bytes().to_vec()));
    }

    #[test]
    fn a_path_hit_does_not_wait_for_another_tools_shell_probe() {
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(released);
        let resolver = Arc::new(Resolver::new(
            [("PATH", "/usr/bin:/bin")].into_iter().collect(),
            vec![],
            move || {
                let _ = entered_tx.send(());
                let _ = released.lock().unwrap().recv();
                Vec::new()
            },
        ));
        let missing = resolver.clone();
        let probe = std::thread::spawn(move || missing.resolve("no-such-tool-anywhere"));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        // The probe is still running: a tool on the PATH resolves anyway.
        let start = Instant::now();
        assert!(resolver.resolve("sh").is_some());
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        assert!(probe.join().unwrap().is_none());
    }

    #[test]
    fn login_shell_probe_stops_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        // A login shell whose profile hangs (and ignores TERM).
        let shell = script(dir.path(), "slow-shell", "trap '' TERM\nsleep 30");
        let cancel: Cancel = Arc::default();
        let probe_cancel = cancel.clone();
        let resolver = Arc::new(Resolver::new(
            [("PATH", "/usr/bin:/bin")].into_iter().collect(),
            vec![],
            move || login_shell_path(&shell, &probe_cancel),
        ));
        let r = resolver.clone();
        let start = Instant::now();
        let lookup = std::thread::spawn(move || r.resolve("no-such-tool-anywhere"));
        std::thread::sleep(Duration::from_millis(150));
        cancel.store(true, Ordering::SeqCst);
        assert!(lookup.join().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
        // A shell that answers is read.
        let answering = script(
            dir.path(),
            "answering-shell",
            &format!("printf 'motd\\n{PATH_MARKER}/x/bin:/y/bin\\n'"),
        );
        assert_eq!(
            login_shell_path(&answering, &Cancel::default()),
            [PathBuf::from("/x/bin"), PathBuf::from("/y/bin")]
        );
    }

    const NON_UTF8_VAR: &str = "NOA_USAGE_SYNTHETIC_NON_UTF8";
    const MARKER_VAR: &str = "NOA_USAGE_SYNTHETIC_MARKER";

    /// Runs `reads_a_non_utf8_environment` in a child test process whose
    /// environment holds a non-UTF-8 value (setting one here would race the
    /// other tests' threads).
    #[test]
    fn non_utf8_environment_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "limits::cli::tests::reads_a_non_utf8_environment",
                "--ignored",
                "--test-threads=1",
            ])
            .env(NON_UTF8_VAR, OsStr::from_bytes(b"syn\xff\xfethetic"))
            .env(MARKER_VAR, &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(marker.exists(), "the child test did not run");
    }

    #[test]
    #[ignore = "run by non_utf8_environment_does_not_panic in a child process"]
    fn reads_a_non_utf8_environment() {
        let Some(marker) = std::env::var_os(MARKER_VAR) else {
            return;
        };
        let value = b"syn\xff\xfethetic";
        let env = Env::inherited();
        assert_eq!(env.get(NON_UTF8_VAR).map(OsStr::as_bytes), Some(&value[..]));
        let resolver = Resolver::new(env, vec![], Vec::new);
        let sh = resolver.resolve("sh").unwrap();
        assert_eq!(
            sh.env.get(NON_UTF8_VAR).map(OsStr::as_bytes),
            Some(&value[..])
        );
        // curl's environment is built from the same inherited one.
        let _ = crate::limits::curl::system_http(&resolver, Cancel::default());
        // And a child receives the value unchanged.
        let out = run(
            &sh.path,
            &["-c", &format!("printf %s \"${NON_UTF8_VAR}\"")],
            Some(&sh.env),
            None,
            Duration::from_secs(5),
            &[0],
            None,
        );
        assert_eq!(out, Ok(value.to_vec()));
        std::fs::write(marker, "ok").unwrap();
    }
}
