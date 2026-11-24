//! Re-runs in the verifier's own pane (L1-5): the widget writes
//! `r<N>/<verifier>/rerun/` — one `<n>.cmd` per command exactly as
//! reported, and `run.sh`, a fixed script made from constants and
//! widget-chosen paths only — the verifier's agent starts it, and the
//! widget watches `<n>.pid`, `<n>.exit` and `done`, kills a command's
//! process group past its limit, checks the files it wrote against their
//! logged digests, and ingests each `<n>.log`. [`script`] and [`outcome`]
//! are pure; the rest does I/O.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
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
    /// How it ended, once it has; `None` while it waits for its turn or
    /// runs (also while its group is being killed past its limit).
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
    /// not finish"): the engine's start timeout.
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
    /// How each command ended and when that was first seen: once decided,
    /// an outcome stays.
    ended: BTreeMap<u32, (End, Millis)>,
}

impl Watch {
    pub fn new(limits: Limits) -> Watch {
        Watch {
            limits,
            seen: BTreeMap::new(),
            procs: BTreeMap::new(),
            term: BTreeMap::new(),
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
/// status to `<n>.exit` (temporary file + `mv`); at the end `done` holding
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
    if unsafe { libc::mkdirat(parent_fd.as_raw_fd(), c_name.as_ptr(), 0o755) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = fsx::open_dir(base, dir, false)?;
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
/// outcome once it has one (L1-5 steps 3–4), timed by `watch`:
/// - an `.exit` ends it [`End::Exited`];
/// - a `.nocwd` (its `cd` failed) ends it [`End::NotRun`] ([`NO_CWD`]);
/// - a process gone without an `.exit` ends it [`End::NotRun`] at once
///   ("the re-run was stopped before it finished; …");
/// - past `watch`'s run limit, its process group gets SIGTERM, SIGKILL
///   [`RERUN_KILL_GRACE`](crate::model::types::RERUN_KILL_GRACE) later
///   (again at every poll while any of the group is left), and it ends
///   [`End::TimedOut`] once nothing of its group is left;
/// - no next `.pid` within the next-start limit after one ended ends that
///   command and every later one [`End::NotRun`] ("re-run did not
///   finish").
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
    let mut items = Vec::new();
    let mut previous_end: Option<Millis> = None;
    let mut stalled = false;
    for item in &request.items {
        let n = item.n;
        if !watch.procs.contains_key(&n)
            && let Some((pid, run)) = read_pid(dir, n)?
        {
            watch.seen.insert(n, now);
            watch.procs.insert(n, Proc::first_seen(pid, run));
        }
        let proc = watch.procs.get(&n).copied();
        let alive = proc.is_some_and(|proc| proc.leads());
        // Read after the liveness check: the subshell writes them before
        // it ends, so a process found gone has written one by now if ever.
        let exit = read_number(dir, &format!("{n}.exit"))?;
        let no_cwd = read_small(dir, dir, &format!("{n}.nocwd"), 64)?.is_some();
        if !watch.ended.contains_key(&n) {
            let decided = if let (Some(proc), Some(&term_at)) = (proc, watch.term.get(&n)) {
                if !proc.group_left() {
                    Some(End::TimedOut)
                } else {
                    if now.saturating_sub(term_at) >= RERUN_KILL_GRACE {
                        proc.signal(Signal::Kill);
                    }
                    None
                }
            } else if let Some(code) = exit {
                Some(End::Exited(code))
            } else if no_cwd {
                Some(End::NotRun(NO_CWD.to_owned()))
            } else if let Some(proc) = proc {
                if !alive {
                    Some(End::NotRun(STOPPED.to_owned()))
                } else if now.saturating_sub(watch.seen[&n]) >= limits.run {
                    proc.signal(Signal::Term);
                    watch.term.insert(n, now);
                    None
                } else {
                    None
                }
            } else if stalled
                || previous_end.is_some_and(|at| now.saturating_sub(at) >= limits.next_start)
            {
                stalled = true;
                Some(End::NotRun(DID_NOT_FINISH.to_owned()))
            } else {
                None
            };
            if let Some(end) = decided {
                watch.ended.insert(n, (end, now));
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
    let done = read_small(dir, dir, DONE_FILE, 256)?
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned());
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
/// following a link and the copy created exclusively.
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
                fsx::write_new_at(&fd, OsStr::new(&name), &bytes)
                    .map_err(|e| format!("{name} を保存できません: {e}"))?;
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
        assert!(text.contains("echo '00ff' > \"$d/done.tmp\""));
        assert!(nonce().unwrap() != nonce().unwrap());
        assert!(nonce().unwrap().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn every_kind_starts_its_script_detached_until_ac35_says_otherwise() {
        assert!(AgentKind::ALL.into_iter().all(detached));
    }
}
