//! Re-runs in the verifier's own pane (L1-5): the widget writes
//! `r<N>/<verifier>/rerun/` — one `<n>.cmd` per command exactly as
//! reported, and `run.sh`, a fixed script made from constants and
//! widget-chosen paths only — the verifier's agent starts it, and the
//! widget watches `<n>.pid`, `<n>.exit` and `done`, kills a command's
//! process group past its limit, checks the files it wrote against their
//! logged digests, and ingests each `<n>.log`. [`script`] and [`outcome`]
//! are pure; the rest does I/O.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::{CStr, CString, OsStr};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::capture::digest_bytes;
use crate::evidence;
use crate::fsx;
use crate::model::types::{
    AgentKind, EvidenceKey, MAX_LOG_BYTES, Millis, PartId, RERUN_KILL_GRACE, Rerun, RerunItem,
    RerunRequest, RoundNo, Stored,
};
use crate::store;

/// The directory under the verifier's `r<N>/<verifier>/`.
pub const RERUN_DIR: &str = "rerun";
pub const SCRIPT_FILE: &str = "run.sh";
/// Written last, holding the nonce.
pub const DONE_FILE: &str = "done";

/// Why a command whose process is gone without an `.exit` is `not_run`
/// (L1-5 step 3).
pub const STOPPED: &str =
    "the re-run was stopped before it finished; the agent's tool may have ended it";
/// Why a command that never started after the one before it ended is
/// `not_run`, with every later one (L1-5 step 3).
pub const DID_NOT_FINISH: &str = "re-run did not finish";
/// Why every item of a script that never wrote its first `.pid` is
/// `not_run` once the re-run brief's start timeout has passed (L1-5 step
/// 4): [`poll`] leaves those items without an outcome, as the brief may be
/// sent again, and the runner ends them with this.
pub const DID_NOT_START: &str = "re-run did not start";
/// Why a command whose `cd` to its reported cwd failed is `not_run`: it
/// never ran (`cd`'s error is in its log).
pub const NO_CWD: &str =
    "the reported cwd could not be entered in the worktree; the command did not run";
/// Why every item of a script whose files differ from the ones written is
/// `not_run` (L1-5 step 5).
pub const SCRIPT_CHANGED: &str = "re-run script was changed";
/// What a stored re-run log cut at [`MAX_LOG_BYTES`] ends with.
pub const CUT_MARK: &str = "\n[Noa Verify: output cut at 8 MiB; the rest was not stored]\n";

/// Whether `kind`'s re-run brief starts `run.sh` detached (`nohup sh … &`)
/// rather than in the foreground. #TODO(agent): AC-35 settles, in a live
/// Noa, whether each kind's shell tool lets a detached process outlive the
/// tool call; a kind found to kill it changes to `false` here.
pub const fn detached(kind: AgentKind) -> bool {
    match kind {
        AgentKind::Claude => true,
        AgentKind::Codex => true,
        AgentKind::Agy => true,
    }
}

/// One command to run again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub key: EvidenceKey,
    /// Exactly as reported; written to `<n>.cmd`, never parsed or quoted.
    pub cmd: String,
    /// As reported, relative to the worktree.
    pub cwd: String,
}

/// What one command of a running script shows on disk, and what [`poll`]
/// made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemStatus {
    /// Counted from 1.
    pub n: u32,
    /// From `<n>.pid` as first read; a later rewrite of the file is not
    /// followed.
    pub pid: Option<i32>,
    /// Whether that process still runs as the run's command: it leads its
    /// own process group, in the session it was first seen in.
    pub alive: bool,
    /// From `<n>.exit`, once written.
    pub exit: Option<i32>,
    /// How it ended, once its turn is over (nothing of its process group
    /// left, its log final); `None` while it waits for its turn or runs,
    /// while what it left in its group is still there, and while its
    /// group is being killed past its limit.
    pub end: Option<End>,
}

/// A script's state as [`poll`] read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub items: Vec<ItemStatus>,
    /// What `done` holds, once written.
    pub done: Option<String>,
}

/// The time limits a script's commands are held to (L1-5 steps 3–4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long one command may run, counted from its `.pid` being first
    /// seen: [`RERUN_TIMEOUT`](crate::model::types::RERUN_TIMEOUT), or less
    /// for a kind whose re-run brief runs `run.sh` in the foreground (L1-5
    /// step 3).
    pub run: Millis,
    /// How long after one command ended the next one's `.pid` may take;
    /// past it, that command and every later one are `not_run` ("re-run did
    /// not finish"): the engine's start timeout. The last command, its
    /// group seen gone, waits as long for `done`.
    pub next_start: Millis,
}

/// What [`poll`] keeps between the polls of one script. The runner keeps
/// one per script it watches, in memory; a resume starts a new one, so a
/// command still running is timed from the first poll after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    limits: Limits,
    /// When each command's `.pid` was first seen, by `n`.
    seen: BTreeMap<u32, Millis>,
    /// Each command's process as its `.pid` was first read: the only one
    /// ever signalled for it.
    procs: BTreeMap<u32, Proc>,
    /// When each command's process group was sent SIGTERM past its limit.
    term: BTreeMap<u32, Millis>,
    /// How each command ended, once its files or the kill past its limit
    /// said: it stays, and is given once the command's turn is over.
    decided: BTreeMap<u32, End>,
    /// When the widget first saw nothing left of each decided command's
    /// process group.
    gone: BTreeMap<u32, Millis>,
    /// The outcome given for each command and when: once given, it stays.
    ended: BTreeMap<u32, (End, Millis)>,
}

impl Watch {
    pub fn new(limits: Limits) -> Watch {
        Watch {
            limits,
            seen: BTreeMap::new(),
            procs: BTreeMap::new(),
            term: BTreeMap::new(),
            decided: BTreeMap::new(),
            gone: BTreeMap::new(),
            ended: BTreeMap::new(),
        }
    }
}

/// A command's process as first read from its `<n>.pid`, which `run.sh`
/// writes as `<pid> <pid of run.sh>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Proc {
    pid: i32,
    /// The session the process was in when first seen, given only when it
    /// then led a process group of its own (pgid = pid) in the session of
    /// the `run.sh` named beside it. `None`: it was never the run's
    /// command (gone already, a reused or rewritten pid, or no group of its
    /// own); it is taken as gone and never signalled.
    sid: Option<i32>,
}

/// `getpgid` and `getsid` of `pid`; -1 for one that is gone.
fn group_and_session(pid: i32) -> (i32, i32) {
    // SAFETY: both only read the process table.
    unsafe { (libc::getpgid(pid), libc::getsid(pid)) }
}

impl Proc {
    /// Checks `pid` at first sight against the `run.sh` that wrote it.
    fn first_seen(pid: i32, run: Option<i32>) -> Proc {
        let sid = run.filter(|&run| run > 1 && run != pid).and_then(|run| {
            let (group, sid) = group_and_session(pid);
            let (run_group, run_sid) = group_and_session(run);
            (group == pid && sid > 0 && sid == run_sid && run_group != pid).then_some(sid)
        });
        Proc { pid, sid }
    }

    /// Whether the command still runs: its pid still leads its own group,
    /// in the session recorded (a pid given to another process since leads
    /// none, or another one).
    fn leads(&self) -> bool {
        self.sid
            .is_some_and(|sid| group_and_session(self.pid) == (self.pid, sid))
    }

    /// Whether anything of its process group is left: the command itself,
    /// or, with its pid gone, processes it started. A process group id
    /// still in use is never given to a new process, so a group of that id
    /// whose leader is gone is still the command's.
    fn group_left(&self) -> bool {
        if self.sid.is_none() {
            return false;
        }
        if self.leads() {
            return true;
        }
        // SAFETY: only reads the process table.
        let gone = unsafe { libc::getpgid(self.pid) } == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        gone && group_exists(self.pid)
    }

    /// Whether nothing of its process group is left as far as the widget
    /// can tell: [`Proc::group_left`] says no for a command it checked at
    /// first sight; for a pid it never took for the run's command, no
    /// process may be in a group of that id (one there may be another's:
    /// the turn then waits for the script, or the run's limit).
    fn group_gone(&self) -> bool {
        if self.sid.is_some() {
            !self.group_left()
        } else {
            !group_exists(self.pid)
        }
    }

    /// Sends `signal` to its process group, checked just before as still
    /// the command's ([`Proc::group_left`]); nothing otherwise.
    fn signal(&self, signal: Signal) {
        if self.group_left() {
            let _ = kill_group(self.pid, signal);
        }
    }
}

/// How a command ended, as the runner saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    Exited(i32),
    TimedOut,
    NotRun(String),
}

/// Which signal [`kill_group`] sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

/// `text` as one single-quoted shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `run.sh` for `items` (`(n, cwd)` pairs): run again with bash unless bash
/// runs it, `set -m`, then per command `cd` to `<worktree>/<cwd>`, run
/// `"${SHELL:-/bin/sh}" <dir>/<n>.cmd` in the background with output to
/// `<n>.log`, its pid and the script's own to `<n>.pid`, wait, its exit
/// status to `<n>.exit` (temporary file + `mv`), then end what is left of
/// its process group and wait until nothing is; at the end `done` holding
/// `nonce`. Every path single-quoted; no reported text in it.
///
/// Each command runs in a background subshell, which `set -m` makes the
/// leader of a process group of its own (its pid is `<n>.pid`, and the
/// group the timeout kill signals). That takes job control, which bash
/// gives without a terminal while dash turns it off ("can't access tty")
/// and zsh as `sh` refuses `set -m` and ends the script, so the script
/// runs itself again with `/bin/bash` under any other `sh`. The subshell
/// writes `<n>.exit` itself before it ends, so a `<n>.pid` whose process
/// is gone without an `<n>.exit` means it was stopped, never that the
/// status is still on its way. A `cd` that fails writes `<n>.nocwd`
/// instead (its error in the log), and the command does not run.
///
/// The subshell ending is not the command ending: processes it started
/// stay in its group (a child that ignores the timeout's SIGTERM, a
/// background job). Once the subshell is gone, the script sends what is
/// left of the group SIGTERM, SIGKILL
/// [`RERUN_KILL_GRACE`](crate::model::types::RERUN_KILL_GRACE) later, and
/// starts the next command (or writes `done`) only once nothing of the
/// group is left, so two commands never run at once. A group id in use is
/// never given to a new process, so the group signalled is the command's.
pub fn script(nonce: &str, dir: &Path, worktree: &Path, items: &[(u32, &str)]) -> String {
    let mut out = String::from(
        "#!/bin/sh\n\
         # Noa Verify re-run script, generated: do not edit. Runs each reported\n\
         # command again, one after another, each in a process group of its own\n\
         # (job control, which only bash gives without a terminal).\n\
         [ -n \"${BASH_VERSION-}\" ] || exec /bin/bash \"$0\"\n\
         set -m\n",
    );
    let _ = writeln!(out, "d={}", quote(&dir.to_string_lossy()));
    let _ = writeln!(out, "w={}", quote(&worktree.to_string_lossy()));
    for (n, cwd) in items {
        let _ = writeln!(
            out,
            "( cd \"$w\"/{cwd} || {{ echo \"$?\" > \"$d/{n}.nocwd.tmp\" && \
             mv -f \"$d/{n}.nocwd.tmp\" \"$d/{n}.nocwd\"; exit 1; }}; \
             \"${{SHELL:-/bin/sh}}\" \"$d/{n}.cmd\"; s=$?; \
             echo \"$s\" > \"$d/{n}.exit.tmp\" && mv -f \"$d/{n}.exit.tmp\" \"$d/{n}.exit\" ) \
             < /dev/null > \"$d/{n}.log\" 2>&1 &",
            cwd = quote(cwd)
        );
        let _ = writeln!(out, "p=$!");
        let _ = writeln!(
            out,
            "echo \"$p $$\" > \"$d/{n}.pid.tmp\" && mv -f \"$d/{n}.pid.tmp\" \"$d/{n}.pid\""
        );
        let _ = writeln!(out, "wait \"$p\"");
        let _ = writeln!(
            out,
            "i=0; while kill -0 -- \"-$p\" 2>/dev/null; do \
             if [ \"$i\" -eq 0 ]; then kill -TERM -- \"-$p\" 2>/dev/null; fi; \
             if [ \"$i\" -ge {grace} ]; then kill -KILL -- \"-$p\" 2>/dev/null; fi; \
             i=$((i + 1)); sleep 0.1; done",
            grace = RERUN_KILL_GRACE / 100
        );
    }
    let _ = writeln!(
        out,
        "echo {} > \"$d/{DONE_FILE}.tmp\" && mv -f \"$d/{DONE_FILE}.tmp\" \"$d/{DONE_FILE}\"",
        quote(nonce)
    );
    out
}

/// 16 bytes from `/dev/urandom`, as 32 hex digits.
pub fn nonce() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes `<dir>/<n>.cmd` for each of `commands` (n from 1) and
/// `<dir>/run.sh`, creating `dir` below `base` exclusively, and returns
/// what [`crate::model::types::Event::RerunRequested`] logs.
///
/// A `dir` already there is taken only as what an earlier call, cut short
/// before its request was logged, left: holding nothing but regular files
/// of the names this writes (see [`clear_partial`]), which are written
/// afresh with a new nonce. Anything else in it refuses the request with
/// `AlreadyExists`, the files left as they are.
pub fn write_request(
    base: &Path,
    dir: &Path,
    worktree: &Path,
    commands: &[Command],
) -> io::Result<RerunRequest> {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rerun/ がありません",
        ));
    };
    let parent_fd = fsx::open_dir(base, parent, true)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "名前に NUL が含まれます"))?;
    // SAFETY: `parent_fd` is an open directory and `c_name` a C string.
    let made = unsafe { libc::mkdirat(parent_fd.as_raw_fd(), c_name.as_ptr(), 0o755) } == 0;
    if !made {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(err);
        }
    }
    let fd = fsx::open_dir(base, dir, false)?;
    if !made {
        clear_partial(&fd, commands.len())?;
    }
    let mut items = Vec::new();
    let mut pairs = Vec::new();
    for (i, command) in commands.iter().enumerate() {
        let n = i as u32 + 1;
        fsx::write_new_at(&fd, OsStr::new(&format!("{n}.cmd")), command.cmd.as_bytes())?;
        items.push(RerunItem {
            key: command.key.clone(),
            n,
            cmd_digest: digest_bytes(command.cmd.as_bytes()),
        });
        pairs.push((n, command.cwd.as_str()));
    }
    let nonce = nonce()?;
    let text = script(&nonce, dir, worktree, &pairs);
    fsx::write_new_at(&fd, OsStr::new(SCRIPT_FILE), text.as_bytes())?;
    Ok(RerunRequest {
        nonce,
        items,
        script_digest: digest_bytes(text.as_bytes()),
    })
}

/// The names in the open directory `dir`, `.` and `..` left out, read
/// through a duplicate of its descriptor (never its path).
fn names_in(dir: &OwnedFd) -> io::Result<Vec<CString>> {
    // SAFETY: `dir` is an open descriptor; the duplicate is owned by the
    // directory stream made from it below.
    let dup = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is an open directory descriptor nothing else owns.
    let stream = unsafe { libc::fdopendir(dup) };
    if stream.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(err);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is open; the entry is read before the next call.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is NUL-terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name.to_owned());
        }
    }
    // SAFETY: `stream` is open; this closes `dup` with it.
    unsafe { libc::closedir(stream) };
    Ok(names)
}

/// Makes `rerun/` (open as `dir`), found already there, ready for a
/// request written again after an earlier write was cut short past making
/// it: when it holds only regular files of the names [`write_request`]
/// writes for `commands` commands (`<n>.cmd`, `run.sh`), they are removed
/// (never followed); anything else there (a run's files, the verifier's,
/// a link, a folder) refuses with `AlreadyExists` and nothing is removed.
fn clear_partial(dir: &OwnedFd, commands: usize) -> io::Result<()> {
    let ours: Vec<String> = (1..=commands)
        .map(|n| format!("{n}.cmd"))
        .chain([SCRIPT_FILE.to_owned()])
        .collect();
    let names = names_in(dir)?;
    for name in &names {
        // SAFETY: `stat` is plain data the call fills in.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `dir` is open, `name` a C string, `st` writable.
        let found = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        let regular = found == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFREG;
        let known = name
            .to_str()
            .is_ok_and(|name| ours.iter().any(|o| o == name));
        if !(regular && known) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{RERUN_DIR}/ に書きかけの依頼ではない {} があります",
                    name.to_string_lossy()
                ),
            ));
        }
    }
    for name in &names {
        // SAFETY: as above; removes the entry itself, never what it names.
        if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A small file the script writes in `dir` (below `base`), read without
/// following a link and never blocking on a FIFO; `None` when it is not
/// there (yet) or is not a regular file.
fn read_small(base: &Path, dir: &Path, name: &str, cap: u64) -> io::Result<Option<Vec<u8>>> {
    match evidence::open_file(base, dir, Path::new(name)) {
        Ok(Some((file, _))) => Ok(Some(fsx::read_capped(file, cap)?.0)),
        Ok(None) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_number(dir: &Path, name: &str) -> io::Result<Option<i32>> {
    Ok(read_small(dir, dir, name, 64)?
        .and_then(|bytes| String::from_utf8_lossy(&bytes).trim().parse().ok()))
}

/// `<n>.pid`: the command's pid (never 0 or 1) and, beside it, the pid of
/// the `run.sh` that started it.
fn read_pid(dir: &Path, n: u32) -> io::Result<Option<(i32, Option<i32>)>> {
    Ok(
        read_small(dir, dir, &format!("{n}.pid"), 64)?.and_then(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            let mut words = text.split_whitespace().map(|word| word.parse::<i32>().ok());
            let pid = words.next().flatten().filter(|pid| *pid > 1)?;
            Some((pid, words.next().flatten()))
        }),
    )
}

/// Whether any process of the group `pid` leads still exists.
fn group_exists(pid: i32) -> bool {
    // SAFETY: as above.
    let found = unsafe { libc::kill(-pid, 0) } == 0;
    found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// What `dir` shows of `request`'s commands at `now`, each with its
/// outcome once it has one (L1-5 steps 3–4), timed by `watch`.
///
/// How a command ended is decided as its files say: an `.exit` ends it
/// [`End::Exited`], a `.nocwd` (its `cd` failed) [`End::NotRun`]
/// ([`NO_CWD`]), and its process gone without either [`End::NotRun`]
/// ([`STOPPED`]). That outcome is given only once the command's turn is
/// over, its log final: once nothing of its process group is left, which
/// the script says by starting the next command (its `.pid`) or writing
/// `done`, as it does only then, or which the widget sees itself (for the
/// last command, only past the next-start limit after that: `done` is
/// what says the run is through, and a script gone without writing it is
/// waited for no longer than it would be for a next `.pid`). So every
/// command has an outcome before `done` only when the script is gone.
///
/// While a command's turn is not over, past `watch`'s run limit (counted
/// from its `.pid` first seen) its process group gets SIGTERM, SIGKILL
/// [`RERUN_KILL_GRACE`](crate::model::types::RERUN_KILL_GRACE) later
/// (again at every poll while any of the group is left); one with no
/// outcome by then is [`End::TimedOut`], one that had exited keeps its
/// status, and either is given once nothing of its group is left. No next
/// `.pid` within the next-start limit after one command's outcome was
/// given ends that command and every later one [`End::NotRun`] ("re-run
/// did not finish"), or gives the outcome of one whose `.exit` came
/// before its `.pid`.
///
/// A command's pid is read once, when its `.pid` is first seen, and
/// checked then against the `run.sh` pid beside it: it must lead a process
/// group of its own in that script's session. A pid that does not is
/// never the run's command (taken as gone, never signalled), and a later
/// rewrite of the file is not read. Before each signal the process is
/// checked again (still leading its group, in the same session), or, its
/// leader gone, its group must still have members. An outcome once given
/// stays, whatever the files show later. The first command waiting for
/// its `.pid` stays without an outcome: whether the script started at all
/// is the re-run brief's start timeout ([`DID_NOT_START`]). Files are read
/// without following a link; a `.pid` of 0 or 1 is never taken for a
/// process.
pub fn poll(
    dir: &Path,
    request: &RerunRequest,
    watch: &mut Watch,
    now: Millis,
) -> io::Result<Status> {
    let limits = watch.limits;
    // `done`, then every `.pid`, then each `.exit`: the script writes each
    // of them only after the ones before it, so none read later is older.
    let done = read_small(dir, dir, DONE_FILE, 256)?
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned());
    for item in &request.items {
        if !watch.procs.contains_key(&item.n)
            && let Some((pid, run)) = read_pid(dir, item.n)?
        {
            watch.seen.insert(item.n, now);
            watch.procs.insert(item.n, Proc::first_seen(pid, run));
        }
    }
    let mut items = Vec::new();
    let mut previous_end: Option<Millis> = None;
    let mut stalled = false;
    for (i, item) in request.items.iter().enumerate() {
        let n = item.n;
        let proc = watch.procs.get(&n).copied();
        let alive = proc.is_some_and(|proc| proc.leads());
        // Read after the liveness check: the subshell writes them before
        // it ends, so a process found gone has written one by now if ever.
        let exit = read_number(dir, &format!("{n}.exit"))?;
        let no_cwd = read_small(dir, dir, &format!("{n}.nocwd"), 64)?.is_some();
        if !watch.ended.contains_key(&n) {
            if let Entry::Vacant(slot) = watch.decided.entry(n) {
                let end = if let Some(code) = exit {
                    Some(End::Exited(code))
                } else if no_cwd {
                    Some(End::NotRun(NO_CWD.to_owned()))
                } else if proc.is_some() && !alive {
                    Some(End::NotRun(STOPPED.to_owned()))
                } else {
                    None
                };
                if let Some(end) = end {
                    slot.insert(end);
                }
            }
            if let Some(proc) = proc
                && watch.decided.contains_key(&n)
                && !watch.gone.contains_key(&n)
                && proc.group_gone()
            {
                watch.gone.insert(n, now);
            }
            // The script gone before this command's `.pid` (none within the
            // next-start limit after the one before ended).
            let overdue = stalled
                || previous_end.is_some_and(|at| now.saturating_sub(at) >= limits.next_start);
            let last = i + 1 == request.items.len();
            let next_seen = request
                .items
                .get(i + 1)
                .is_some_and(|next| watch.procs.contains_key(&next.n));
            let mut over = done.is_some()
                || next_seen
                || watch
                    .gone
                    .get(&n)
                    .is_some_and(|&at| !last || now.saturating_sub(at) >= limits.next_start)
                || (proc.is_none() && overdue);
            if let Some(proc) = proc
                && !(over && watch.decided.contains_key(&n))
            {
                match watch.term.get(&n) {
                    None if now.saturating_sub(watch.seen[&n]) >= limits.run => {
                        proc.signal(Signal::Term);
                        watch.term.insert(n, now);
                        watch.decided.entry(n).or_insert(End::TimedOut);
                    }
                    Some(&at) if now.saturating_sub(at) >= RERUN_KILL_GRACE => {
                        proc.signal(Signal::Kill);
                    }
                    _ => {}
                }
                // Killed: over once nothing of its group is left (at once for
                // a pid never the run's command, which is never signalled).
                over |= watch.term.contains_key(&n) && !proc.group_left();
            }
            if let Some(end) = watch.decided.get(&n).filter(|_| over) {
                watch.ended.insert(n, (end.clone(), now));
            } else if proc.is_none() && overdue {
                stalled = true;
                watch
                    .ended
                    .insert(n, (End::NotRun(DID_NOT_FINISH.to_owned()), now));
            }
        }
        let ended = watch.ended.get(&n);
        previous_end = ended.map(|(_, at)| *at);
        items.push(ItemStatus {
            n,
            pid: proc.map(|proc| proc.pid),
            alive,
            exit,
            end: ended.map(|(end, _)| end.clone()),
        });
    }
    Ok(Status { items, done })
}

/// Sends `signal` to the process group `pid` leads.
///
/// Refuses a `pid` of 0 or 1, which would signal the caller's own group
/// or every process it may signal.
pub fn kill_group(pid: i32, signal: Signal) -> io::Result<()> {
    if pid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("pid {pid} にはシグナルを送りません"),
        ));
    }
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: a negative pid names the process group; no memory involved.
    if unsafe { libc::kill(-pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Why the files in `dir` are not the ones written for `request`
/// (`run.sh` or a `<n>.cmd` changed, or `done` holding another nonce).
///
/// The error starts with [`SCRIPT_CHANGED`] and names the file; a `done`
/// not written yet is no error.
pub fn check_digests(base: &Path, dir: &Path, request: &RerunRequest) -> Result<(), String> {
    let digest_of = |name: &str| -> Option<u64> {
        read_small(base, dir, name, 1024 * 1024)
            .ok()
            .flatten()
            .map(|bytes| digest_bytes(&bytes))
    };
    if digest_of(SCRIPT_FILE) != Some(request.script_digest) {
        return Err(format!("{SCRIPT_CHANGED} ({SCRIPT_FILE})"));
    }
    for item in &request.items {
        let name = format!("{}.cmd", item.n);
        if digest_of(&name) != Some(item.cmd_digest) {
            return Err(format!("{SCRIPT_CHANGED} ({name})"));
        }
    }
    match read_small(base, dir, DONE_FILE, 256) {
        Ok(None) => Ok(()),
        Ok(Some(bytes)) if String::from_utf8_lossy(&bytes).trim() == request.nonce => Ok(()),
        _ => Err(format!(
            "{SCRIPT_CHANGED} ({DONE_FILE} holds another nonce than the one written)"
        )),
    }
}

/// Copies each `<n>.log` of `request` from `dir` (below `src_base`) to
/// `<reply_dir>/evidence/r<round>/<part>.rerun/<n>.log`
/// ([`crate::store::rerun_log_path`], beside and never inside the
/// directory [`crate::evidence::ingest`] fills), cut at
/// [`crate::model::types::MAX_LOG_BYTES`] with the cut marked.
///
/// The cut copy ends with [`CUT_MARK`]; the source is read without
/// following a link and the copy created exclusively. A file already at
/// the copy's path (an ingest a crash cut short before `RerunFinished`
/// was logged) is taken when it is a regular file of the same size and
/// bytes, and refused, left as it is, otherwise.
pub fn ingest_logs(
    src_base: &Path,
    dir: &Path,
    reply_base: &Path,
    reply_dir: &Path,
    round: RoundNo,
    part: &PartId,
    request: &RerunRequest,
) -> Vec<(EvidenceKey, Result<Stored, String>)> {
    request
        .items
        .iter()
        .map(|item| {
            let name = format!("{}.log", item.n);
            let stored = (|| {
                let (file, _) = evidence::open_file(src_base, dir, Path::new(&name))
                    .map_err(|e| format!("{name} を読めません: {e}"))?
                    .ok_or_else(|| format!("{name} がありません"))?;
                let (mut bytes, over) = fsx::read_capped(file, MAX_LOG_BYTES)
                    .map_err(|e| format!("{name} を読めません: {e}"))?;
                if over {
                    bytes.extend_from_slice(CUT_MARK.as_bytes());
                }
                let to = store::rerun_log_path(reply_dir, round, part, item.n);
                let folder = to.parent().expect("a log path has a folder");
                let fd = fsx::open_dir(reply_base, folder, true)
                    .map_err(|e| format!("{name} を保存できません: {e}"))?;
                match fsx::write_new_at(&fd, OsStr::new(&name), &bytes) {
                    Ok(()) => {}
                    // A copy an earlier ingest made and a crash kept from
                    // being logged: taken when it is this one.
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        evidence::same_copy(reply_base, folder, Path::new(&name), &bytes)
                            .map_err(|why| format!("{name} を保存できません: {why}"))?;
                    }
                    Err(e) => return Err(format!("{name} を保存できません: {e}")),
                }
                Ok(Stored {
                    path: store::rerun_log_path(Path::new(""), round, part, item.n),
                    bytes: bytes.len() as u64,
                    digest: digest_bytes(&bytes),
                })
            })();
            (item.key.clone(), stored)
        })
        .collect()
}

/// The re-run outcome of a command the verifier reported with exit status
/// `reported`, given how it ended and its stored log, kept on whatever
/// outcome it gives (`None` when no log was written or it could not be
/// stored; the outcome stands either way).
pub fn outcome(reported: i32, end: End, log: Option<Stored>) -> Rerun {
    match end {
        End::Exited(exit) if exit == reported => Rerun::Matched { exit, log },
        End::Exited(exit) => Rerun::Differed {
            exit: Some(exit),
            log,
        },
        End::TimedOut => Rerun::TimedOut { log },
        End::NotRun(why) => Rerun::NotRun { why, log },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::tests::temp;
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};

    fn part() -> PartId {
        PartId::new("codex")
    }

    fn command(id: &str, cmd: &str, cwd: &str) -> Command {
        Command {
            key: EvidenceKey::reported(&part(), 1, id),
            cmd: cmd.into(),
            cwd: cwd.into(),
        }
    }

    /// A verifier's worktree and its `rerun/` dir, as the runner lays them
    /// out (the reply dir below the worktree).
    fn layout(name: &str) -> (PathBuf, PathBuf) {
        let worktree = temp("rerun-wt").join(name);
        fs::create_dir_all(worktree.join("sub")).unwrap();
        let dir = worktree.join(".noa/verify/ver-1/r1/codex/rerun");
        (worktree, dir)
    }

    /// The fake agent: starts `run.sh` as the re-run brief says, detached
    /// from nothing but the test, with `sh`.
    fn start(dir: &Path) -> Child {
        start_with(Path::new("sh"), dir)
    }

    /// As [`start`], with `shell` as the `sh` that runs it.
    fn start_with(shell: &Path, dir: &Path) -> Child {
        std::process::Command::new(shell)
            .arg(dir.join(SCRIPT_FILE))
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Polls like the runner until every item has an outcome; calls
    /// `each` on every status.
    fn watch_all(
        dir: &Path,
        request: &RerunRequest,
        limits: Limits,
        mut each: impl FnMut(&Status),
    ) -> Status {
        let started = Instant::now();
        let mut watch = Watch::new(limits);
        loop {
            let now = started.elapsed().as_millis() as Millis;
            let status = poll(dir, request, &mut watch, now).unwrap();
            each(&status);
            if status.items.iter().all(|i| i.end.is_some()) {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "{status:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    const LIMITS: Limits = Limits {
        run: 60_000,
        next_start: 10_000,
    };

    /// Whether process `pid` exists (one we may not signal exists too).
    fn running(pid: i32) -> bool {
        // SAFETY: signal 0 only checks.
        let found = unsafe { libc::kill(pid, 0) } == 0;
        found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// The pid of the `run.sh` that wrote `dir`'s `<n>.pid`.
    fn run_pid(dir: &Path, n: u32) -> i32 {
        read_pid(dir, n).unwrap().unwrap().1.unwrap()
    }

    #[test]
    fn ac10_same_status_is_matched_another_differed_and_a_long_log_cut_and_marked() {
        let (worktree, dir) = layout("plain");
        let commands = [
            command("e1", "exit 0", "."),
            command("e2", "pwd > where.txt; exit 3", "sub"),
            command(
                "e3",
                "head -c 9000000 /dev/zero | tr '\\0' a; echo; exit 0",
                ".",
            ),
        ];
        let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
        assert_eq!(request.nonce.len(), 32);
        assert_eq!(
            request.items.iter().map(|i| i.n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let mut child = start(&dir);
        let status = watch_all(&dir, &request, LIMITS, |_| {});
        child.wait().unwrap();
        let ends: Vec<End> = status
            .items
            .iter()
            .map(|i| i.end.clone().unwrap())
            .collect();
        assert_eq!(ends, [End::Exited(0), End::Exited(3), End::Exited(0)]);
        assert_eq!(
            fs::read_to_string(worktree.join("sub/where.txt"))
                .unwrap()
                .trim(),
            worktree.join("sub").to_string_lossy()
        );
        // `done` holds the nonce once the script is through.
        let done = poll(&dir, &request, &mut Watch::new(LIMITS), 0)
            .unwrap()
            .done;
        assert_eq!(done.as_deref(), Some(request.nonce.as_str()));
        check_digests(&worktree, &dir, &request).unwrap();

        let reply_base = temp("rerun-repo");
        let reply_dir = reply_base.join(".noa/verify/ver-1");
        let logs = ingest_logs(
            &worktree,
            &dir,
            &reply_base,
            &reply_dir,
            1,
            &part(),
            &request,
        );
        let logs: Vec<Stored> = logs.into_iter().map(|(_, r)| r.unwrap()).collect();
        assert_eq!(logs[0].path, PathBuf::from("evidence/r1/codex.rerun/1.log"));
        let long = fs::read(reply_dir.join(&logs[2].path)).unwrap();
        assert_eq!(long.len() as u64, MAX_LOG_BYTES + CUT_MARK.len() as u64);
        assert_eq!(logs[2].bytes, long.len() as u64);
        assert_eq!(logs[2].digest, digest_bytes(&long));
        assert!(long.ends_with(CUT_MARK.as_bytes()));
        assert!(long[..MAX_LOG_BYTES as usize].iter().all(|&b| b == b'a'));

        let outcomes: Vec<Rerun> = ends
            .into_iter()
            .zip(logs)
            .map(|(end, log)| outcome(0, end, Some(log)))
            .collect();
        assert!(matches!(
            outcomes[0],
            Rerun::Matched {
                exit: 0,
                log: Some(_)
            }
        ));
        assert!(matches!(
            outcomes[1],
            Rerun::Differed {
                exit: Some(3),
                log: Some(_)
            }
        ));
        assert!(matches!(outcomes[2], Rerun::Matched { exit: 0, .. }));
    }

    #[test]
    fn a_log_copy_left_by_a_crash_before_it_was_logged_is_taken_when_identical() {
        let (worktree, dir) = layout("again");
        let request = write_request(
            &worktree,
            &dir,
            &worktree,
            &[command("e1", "exit 0", "."), command("e2", "exit 0", ".")],
        )
        .unwrap();
        fs::write(dir.join("1.log"), "one\n").unwrap();
        fs::write(dir.join("2.log"), "two\n").unwrap();
        let reply_base = temp("rerun-repo");
        let reply_dir = reply_base.join(".noa/verify/ver-1");
        let ingest = || {
            ingest_logs(
                &worktree,
                &dir,
                &reply_base,
                &reply_dir,
                1,
                &part(),
                &request,
            )
        };
        let first: Vec<Stored> = ingest().into_iter().map(|(_, r)| r.unwrap()).collect();
        // The copies were made, then the widget stopped before it logged
        // `RerunFinished`: the next ingest finds them and takes them as
        // they are, with the same `Stored`.
        let again: Vec<Stored> = ingest().into_iter().map(|(_, r)| r.unwrap()).collect();
        assert_eq!(again, first);
        // A copy holding something else is refused and left as it is.
        let copy = reply_dir.join(&first[1].path);
        fs::write(&copy, "TWO\n").unwrap();
        let third = ingest();
        assert_eq!(third[0].1.as_ref().unwrap(), &first[0]);
        let why = third[1].1.as_ref().unwrap_err();
        assert!(why.contains("別の内容"), "{why}");
        assert_eq!(fs::read_to_string(&copy).unwrap(), "TWO\n");
    }

    #[test]
    fn ac10_a_command_past_its_limit_is_timed_out_and_its_whole_process_group_killed() {
        let (worktree, dir) = layout("sleepy");
        let commands = [
            command("e1", "sleep 30", "."),
            // Ignores SIGTERM: only the SIGKILL after the grace ends it.
            command("e2", "trap '' TERM; sleep 30; sleep 30", "."),
            command("e3", "exit 0", "."),
        ];
        let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
        let mut child = start(&dir);
        let limits = Limits {
            run: 500,
            next_start: 10_000,
        };
        let started = Instant::now();
        // `alive` is checked by `poll` itself (the pid leads a process
        // group of its own), so seeing it says each had its own group.
        let mut own_group = [false; 2];
        let status = watch_all(&dir, &request, limits, |status| {
            for (seen, item) in own_group.iter_mut().zip(&status.items) {
                *seen |= item.alive;
            }
        });
        child.wait().unwrap();
        let ends: Vec<End> = status
            .items
            .iter()
            .map(|i| i.end.clone().unwrap())
            .collect();
        assert_eq!(ends, [End::TimedOut, End::TimedOut, End::Exited(0)]);
        assert_eq!(own_group, [true; 2], "not each in a group of its own");
        assert!(
            started.elapsed() >= Duration::from_millis(RERUN_KILL_GRACE),
            "the TERM-proof command ended before its SIGKILL"
        );
        // `timed_out` is given only once nothing of the group is left: the
        // command and the `sleep` it started are gone by then.
        for item in &status.items[..2] {
            let pid = item.pid.unwrap();
            assert!(!group_exists(pid), "process group {pid} still runs");
            assert!(!running(pid), "command {pid} still runs");
        }
        assert!(matches!(
            outcome(0, End::TimedOut, None),
            Rerun::TimedOut { log: None }
        ));
    }

    #[test]
    fn ac10_the_next_command_starts_only_once_nothing_of_the_one_before_is_left() {
        // A command past its limit whose child ignores SIGTERM (it outlives
        // the subshell until the SIGKILL), and one that exits leaving a
        // background job running (ended by the script, its status kept).
        for (first, end) in [
            ("trap '' TERM; sleep 30; sleep 30", End::TimedOut),
            ("sleep 30 & exit 0", End::Exited(0)),
        ] {
            let (worktree, dir) = layout("left-behind");
            let commands = [command("e1", first, "."), command("e2", "exit 0", ".")];
            let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
            let mut child = start(&dir);
            let limits = Limits {
                run: 500,
                next_start: 10_000,
            };
            let mut overlapped = false;
            let status = watch_all(&dir, &request, limits, |status| {
                if let (Some(first), Some(_)) = (status.items[0].pid, status.items[1].pid) {
                    overlapped |= group_exists(first);
                }
            });
            child.wait().unwrap();
            let pid = status.items[0].pid.unwrap();
            assert!(
                !overlapped,
                "{first:?}: command 2 started beside group {pid}"
            );
            assert!(
                !group_exists(pid),
                "{first:?}: `done` was written while group {pid} still ran"
            );
            let ends: Vec<End> = status
                .items
                .iter()
                .map(|i| i.end.clone().unwrap())
                .collect();
            assert_eq!(ends, [end, End::Exited(0)], "{first:?}");
        }
    }

    #[test]
    fn ac10_a_command_ends_only_once_its_log_is_final_and_the_run_only_at_done() {
        // Each exits 0 at once, leaving a child that ignores SIGTERM and
        // writes to the log a second later (the script's SIGTERM does not
        // stop it; it ends before the SIGKILL).
        let writer = "echo early; ( trap '' TERM; sleep 1; echo late ) & exit 0";
        let (worktree, dir) = layout("late-writer");
        let commands = [command("e1", writer, "."), command("e2", writer, ".")];
        let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
        let mut child = start(&dir);
        // Each log as it was when its command was first given an outcome,
        // and whether `done` was there then.
        let mut at_end: [Option<(String, bool)>; 2] = [None, None];
        let status = watch_all(&dir, &request, LIMITS, |status| {
            for (seen, item) in at_end.iter_mut().zip(&status.items) {
                if seen.is_none() && item.end.is_some() {
                    let log = fs::read_to_string(dir.join(format!("{}.log", item.n))).unwrap();
                    *seen = Some((log, status.done.is_some()));
                }
            }
        });
        child.wait().unwrap();
        for (n, seen) in at_end.iter().enumerate() {
            let (log, _) = seen.as_ref().unwrap();
            assert_eq!(log, "early\nlate\n", "command {}", n + 1);
        }
        assert!(at_end[1].as_ref().unwrap().1, "the run ended before `done`");
        let ends: Vec<End> = status
            .items
            .iter()
            .map(|i| i.end.clone().unwrap())
            .collect();
        assert_eq!(ends, [End::Exited(0), End::Exited(0)]);

        // A `run.sh` gone after the last command's `.exit` (it never
        // writes `done`): the command ends once nothing of its group is
        // left and the start timeout has passed since, as a next command
        // would have been `not_run` by then.
        let (worktree, dir) = layout("no-done");
        let request =
            write_request(&worktree, &dir, &worktree, &[command("e1", "exit 0", ".")]).unwrap();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id() as i32;
        gone.wait().unwrap();
        fs::write(dir.join("1.pid"), format!("{gone_pid}\n")).unwrap();
        fs::write(dir.join("1.exit"), "0\n").unwrap();
        let mut watch = Watch::new(LIMITS);
        for now in [0, LIMITS.next_start - 1] {
            let status = poll(&dir, &request, &mut watch, now).unwrap();
            assert_eq!(status.items[0].end, None, "at {now}");
        }
        let status = poll(&dir, &request, &mut watch, LIMITS.next_start).unwrap();
        assert_eq!(status.items[0].end, Some(End::Exited(0)));
        // With `done` there, at once.
        fs::write(dir.join(DONE_FILE), format!("{}\n", request.nonce)).unwrap();
        let status = poll(&dir, &request, &mut Watch::new(LIMITS), 0).unwrap();
        assert_eq!(status.items[0].end, Some(End::Exited(0)));
    }

    #[test]
    fn ac10_the_timeout_kill_works_whatever_sh_starts_the_script() {
        // macOS's `/bin/sh` (bash), dash, which turns job control off
        // without a terminal, and zsh as `sh`, which refuses `set -m`.
        let zsh = temp("zsh-as-sh").join("sh");
        std::os::unix::fs::symlink("/bin/zsh", &zsh).unwrap();
        for shell in [PathBuf::from("/bin/sh"), PathBuf::from("/bin/dash"), zsh] {
            let (worktree, dir) = layout("shells");
            let commands = [command("e1", "sleep 30", "."), command("e2", "exit 0", ".")];
            let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
            let mut child = start_with(&shell, &dir);
            let limits = Limits {
                run: 300,
                next_start: 10_000,
            };
            let status = watch_all(&dir, &request, limits, |_| {});
            child.wait().unwrap();
            let ends: Vec<End> = status
                .items
                .iter()
                .map(|i| i.end.clone().unwrap())
                .collect();
            assert_eq!(ends, [End::TimedOut, End::Exited(0)], "{shell:?}");
            let pid = status.items[0].pid.unwrap();
            assert!(!running(pid), "{shell:?}: command {pid} still runs");
            assert!(!group_exists(pid), "{shell:?}: group {pid} still runs");
        }
    }

    #[test]
    fn ac10_signals_go_only_to_the_process_first_read_while_it_is_still_the_runs() {
        // A bystander leading a process group of its own, as a command does.
        let mut bystander = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let other = bystander.id() as i32;

        // `.pid` rewritten to name the bystander while the command runs
        // (between its SIGTERM and its SIGKILL): the pid first read is the
        // one killed, the bystander is left.
        let (worktree, dir) = layout("rewritten");
        let commands = [command("e1", "trap '' TERM; sleep 30", ".")];
        let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
        let mut child = start(&dir);
        let limits = Limits {
            run: 300,
            next_start: 10_000,
        };
        let started = Instant::now();
        let mut rewritten = false;
        let status = watch_all(&dir, &request, limits, |status| {
            if !rewritten && status.items[0].pid.is_some() && started.elapsed().as_millis() > 1500 {
                let run = run_pid(&dir, 1);
                fs::write(dir.join("1.pid"), format!("{other} {run}\n")).unwrap();
                rewritten = true;
            }
        });
        child.wait().unwrap();
        assert!(rewritten);
        assert_eq!(status.items[0].end, Some(End::TimedOut));
        assert_ne!(status.items[0].pid, Some(other));
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "the bystander was killed"
        );

        // A `.pid` naming the bystander from the start, beside no `run.sh`
        // or one of another session: never the run's command, stopped at
        // once and never signalled.
        let mut stranger = std::process::Command::new("sleep");
        stranger.arg("30");
        // SAFETY: `setsid` is async-signal-safe and touches no memory.
        unsafe {
            stranger.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let stranger = stranger.spawn().unwrap();
        for content in [format!("{other}\n"), format!("{other} {}\n", stranger.id())] {
            let (worktree, dir) = layout("stranger");
            let request =
                write_request(&worktree, &dir, &worktree, &[command("e1", "exit 0", ".")]).unwrap();
            fs::write(dir.join("1.pid"), content).unwrap();
            let mut watch = Watch::new(Limits {
                run: 0,
                next_start: 10_000,
            });
            let status = poll(&dir, &request, &mut watch, 0).unwrap();
            assert!(!status.items[0].alive);
            assert_eq!(status.items[0].end, Some(End::NotRun(STOPPED.into())));
            poll(&dir, &request, &mut watch, RERUN_KILL_GRACE * 2).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                bystander.try_wait().unwrap().is_none(),
                "the bystander was signalled"
            );
        }
        for mut process in [bystander, stranger] {
            let _ = process.kill();
            let _ = process.wait();
        }
    }

    #[test]
    fn ac10_a_cwd_that_cannot_be_entered_is_not_run_rather_than_cds_status() {
        let (worktree, dir) = layout("nocwd");
        let commands = [
            command("e1", "touch ran.txt; exit 1", "missing"),
            command("e2", "exit 0", "sub"),
        ];
        let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
        let mut child = start(&dir);
        let status = watch_all(&dir, &request, LIMITS, |_| {});
        child.wait().unwrap();
        let ends: Vec<End> = status
            .items
            .iter()
            .map(|i| i.end.clone().unwrap())
            .collect();
        assert_eq!(ends, [End::NotRun(NO_CWD.into()), End::Exited(0)]);
        assert!(!dir.join("1.exit").exists());
        assert!(!worktree.join("ran.txt").exists() && !dir.join("ran.txt").exists());
        // Reported with `cd`'s status (1), it is still not `matched`.
        let log = fs::read_to_string(dir.join("1.log")).unwrap();
        assert!(log.contains("missing"), "{log}");
        assert!(matches!(
            outcome(1, ends[0].clone(), None),
            Rerun::NotRun { ref why, .. } if why == NO_CWD
        ));
    }

    #[test]
    fn ac12_a_command_reaches_the_shell_only_as_its_cmd_file_and_runs_as_written() {
        let (worktree, dir) = layout("it's a tree");
        let cmd = "printf '%s|' \"it's\" \"$(echo sub)\" '\"q\"'\necho \"second $((1 + 1))\"";
        let request =
            write_request(&worktree, &dir, &worktree, &[command("e1", cmd, "sub")]).unwrap();
        assert_eq!(fs::read_to_string(dir.join("1.cmd")).unwrap(), cmd);
        let run_sh = fs::read_to_string(dir.join(SCRIPT_FILE)).unwrap();
        for reported in ["printf", "$(echo sub)", "it's\"", "second"] {
            assert!(!run_sh.contains(reported), "run.sh holds {reported:?}");
        }
        assert_eq!(request.script_digest, digest_bytes(run_sh.as_bytes()));
        assert_eq!(request.items[0].cmd_digest, digest_bytes(cmd.as_bytes()));
        let mut child = start(&dir);
        let status = watch_all(&dir, &request, LIMITS, |_| {});
        child.wait().unwrap();
        assert_eq!(status.items[0].end, Some(End::Exited(0)));
        assert_eq!(
            fs::read_to_string(dir.join("1.log")).unwrap(),
            "it's|sub|\"q\"|second 2\n"
        );
        check_digests(&worktree, &dir, &request).unwrap();

        // A changed run.sh, a changed .cmd and a done with another nonce.
        let not_run = |err: String| outcome(0, End::NotRun(err), None);
        fs::write(dir.join(SCRIPT_FILE), format!("{run_sh}\ntouch x\n")).unwrap();
        let err = check_digests(&worktree, &dir, &request).unwrap_err();
        assert!(
            err.starts_with(SCRIPT_CHANGED) && err.contains(SCRIPT_FILE),
            "{err}"
        );
        assert!(matches!(not_run(err), Rerun::NotRun { ref why, .. } if why.contains("run.sh")));
        fs::write(dir.join(SCRIPT_FILE), &run_sh).unwrap();
        fs::write(dir.join("1.cmd"), "exit 0").unwrap();
        let err = check_digests(&worktree, &dir, &request).unwrap_err();
        assert!(err.contains("1.cmd"), "{err}");
        fs::write(dir.join("1.cmd"), cmd).unwrap();
        fs::write(dir.join(DONE_FILE), "0123456789abcdef0123456789abcdef\n").unwrap();
        let err = check_digests(&worktree, &dir, &request).unwrap_err();
        assert!(err.contains("nonce"), "{err}");
        // The request dir is made exclusively: a second one is refused.
        assert_eq!(
            write_request(&worktree, &dir, &worktree, &[command("e1", cmd, ".")])
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn ac12_a_script_that_stops_early_or_never_writes_the_next_pid_ends_its_items_not_run() {
        let (worktree, dir) = layout("stopped");
        let request = write_request(
            &worktree,
            &dir,
            &worktree,
            &[
                command("e1", "exit 0", "."),
                command("e2", "exit 0", "."),
                command("e3", "exit 0", "."),
            ],
        )
        .unwrap();
        // Nothing written yet: no outcome from `poll`, however long it
        // waits (the brief's start timeout decides, and the brief may be
        // sent again) ...
        let mut watch = Watch::new(LIMITS);
        for now in [0, LIMITS.next_start, 10 * (LIMITS.run + LIMITS.next_start)] {
            let status = poll(&dir, &request, &mut watch, now).unwrap();
            assert!(
                status
                    .items
                    .iter()
                    .all(|i| i.end.is_none() && i.pid.is_none() && !i.alive)
            );
            assert_eq!(status.done, None);
        }
        // ... and once it has passed, every item ends `not_run` with the
        // reason, without a log: the script wrote none.
        let reply_base = temp("rerun-repo");
        let reply_dir = reply_base.join(".noa/verify/ver-1");
        let logs = ingest_logs(
            &worktree,
            &dir,
            &reply_base,
            &reply_dir,
            1,
            &part(),
            &request,
        );
        let outcomes: Vec<Rerun> = logs
            .into_iter()
            .map(|(_, log)| outcome(0, End::NotRun(DID_NOT_START.into()), log.ok()))
            .collect();
        assert_eq!(
            outcomes,
            vec![
                Rerun::NotRun {
                    why: DID_NOT_START.into(),
                    log: None
                };
                3
            ]
        );
        check_digests(&worktree, &dir, &request).unwrap();

        // Command 1 ran and ended; command 2's process died without an
        // `.exit` (stopped), and command 3 never got a `.pid`.
        let gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id() as i32;
        let mut gone = gone;
        gone.wait().unwrap();
        fs::write(dir.join("1.pid"), format!("{gone_pid}\n")).unwrap();
        fs::write(dir.join("1.exit"), "0\n").unwrap();
        fs::write(dir.join("2.pid"), format!("{gone_pid}\n")).unwrap();
        let status = poll(&dir, &request, &mut watch, 100).unwrap();
        assert_eq!(status.items[0].end, Some(End::Exited(0)));
        assert_eq!(status.items[1].end, Some(End::NotRun(STOPPED.into())));
        assert_eq!(status.items[2].end, None);
        let status = poll(&dir, &request, &mut watch, 100 + LIMITS.next_start).unwrap();
        assert_eq!(
            status.items[2].end,
            Some(End::NotRun(DID_NOT_FINISH.into()))
        );

        // No next `.pid` after command 1 ended: 2 and 3 both, at once.
        let mut watch = Watch::new(LIMITS);
        fs::remove_file(dir.join("2.pid")).unwrap();
        poll(&dir, &request, &mut watch, 0).unwrap();
        let status = poll(&dir, &request, &mut watch, LIMITS.next_start - 1).unwrap();
        assert_eq!(status.items[1].end, None);
        let status = poll(&dir, &request, &mut watch, LIMITS.next_start).unwrap();
        let ends: Vec<Option<End>> = status.items.iter().map(|i| i.end.clone()).collect();
        assert_eq!(
            ends,
            [
                Some(End::Exited(0)),
                Some(End::NotRun(DID_NOT_FINISH.into())),
                Some(End::NotRun(DID_NOT_FINISH.into()))
            ]
        );
        // An outcome stays once given, whatever the files say later.
        fs::write(dir.join("2.exit"), "0\n").unwrap();
        let status = poll(&dir, &request, &mut watch, LIMITS.next_start + 1).unwrap();
        assert_eq!(
            status.items[1].end,
            Some(End::NotRun(DID_NOT_FINISH.into()))
        );
        // A pid of 0 or 1 is never a process, nor ever signalled.
        fs::write(dir.join("3.pid"), "1\n").unwrap();
        let status = poll(&dir, &request, &mut Watch::new(LIMITS), 0).unwrap();
        assert_eq!(status.items[2].pid, None);
        assert!(kill_group(1, Signal::Term).is_err());
        assert!(kill_group(0, Signal::Term).is_err());
    }

    #[test]
    fn a_request_cut_short_is_written_again_over_only_its_own_files() {
        let (worktree, dir) = layout("retry");
        let commands = [command("e1", "exit 0", "."), command("e2", "exit 3", ".")];
        // Cut short after `rerun/` was made: empty, then with a whole and a
        // half-written `.cmd`, then with a half-written `run.sh` as well.
        for partial in [
            &[][..],
            &[("1.cmd", "exit 0"), ("2.cmd", "ex")],
            &[
                ("1.cmd", "exit 0"),
                ("2.cmd", "exit 3"),
                (SCRIPT_FILE, "#!/bin/sh\n"),
            ],
        ] {
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            for (name, text) in partial {
                fs::write(dir.join(name), text).unwrap();
            }
            let request = write_request(&worktree, &dir, &worktree, &commands).unwrap();
            assert_eq!(fs::read_to_string(dir.join("2.cmd")).unwrap(), "exit 3");
            check_digests(&worktree, &dir, &request).unwrap();
        }
        // Anything else there (the verifier's files, a run's files, a
        // `.cmd` of no command, a link, a folder) refuses it, and is left.
        let elsewhere = temp("rerun-elsewhere").join("x");
        fs::write(&elsewhere, "exit 0").unwrap();
        for (what, make) in [
            (
                "a .pid",
                &(|dir: &Path| fs::write(dir.join("1.pid"), "1 1\n").unwrap()) as &dyn Fn(&Path),
            ),
            ("notes", &|dir: &Path| {
                fs::write(dir.join("notes.txt"), "").unwrap()
            }),
            ("a 3.cmd", &|dir: &Path| {
                fs::write(dir.join("3.cmd"), "").unwrap()
            }),
            ("a link", &|dir: &Path| {
                std::os::unix::fs::symlink(&elsewhere, dir.join("1.cmd")).unwrap()
            }),
            ("a folder", &|dir: &Path| {
                fs::create_dir(dir.join("2.cmd")).unwrap()
            }),
        ] {
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(SCRIPT_FILE), "#!/bin/sh\n").unwrap();
            make(&dir);
            let before: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let err = write_request(&worktree, &dir, &worktree, &commands).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{what}: {err}");
            let mut after: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let mut before = before;
            before.sort();
            after.sort();
            assert_eq!(before, after, "{what}");
        }
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "exit 0");
        // `rerun/` itself a link: refused, nothing written through it.
        let _ = fs::remove_dir_all(&dir);
        let target = temp("rerun-link-target");
        std::os::unix::fs::symlink(&target, &dir).unwrap();
        assert!(write_request(&worktree, &dir, &worktree, &commands).is_err());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn the_script_quotes_every_path_and_nothing_else_varies() {
        let text = script(
            "00ff",
            Path::new("/w/it's/.noa/rerun"),
            Path::new("/w/it's"),
            &[(1, "a b"), (2, "x'y")],
        );
        assert!(text.contains("d='/w/it'\\''s/.noa/rerun'\n"), "{text}");
        assert!(text.contains("w='/w/it'\\''s'\n"), "{text}");
        assert!(text.contains("cd \"$w\"/'a b' ||"), "{text}");
        assert!(text.contains("cd \"$w\"/'x'\\''y' ||"), "{text}");
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("\n[ -n \"${BASH_VERSION-}\" ] || exec /bin/bash \"$0\"\nset -m\n"));
        assert!(text.contains("echo \"$p $$\" > \"$d/2.pid.tmp\""), "{text}");
        assert!(
            text.contains("wait \"$p\"\ni=0; while kill -0 -- \"-$p\" 2>/dev/null; do "),
            "{text}"
        );
        assert!(
            text.contains("if [ \"$i\" -ge 50 ]; then kill -KILL -- \"-$p\""),
            "{text}"
        );
        assert!(text.contains("echo '00ff' > \"$d/done.tmp\""));
        assert!(nonce().unwrap() != nonce().unwrap());
        assert!(nonce().unwrap().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn every_kind_starts_its_script_detached_until_ac35_says_otherwise() {
        assert!(AgentKind::ALL.into_iter().all(detached));
    }
}
