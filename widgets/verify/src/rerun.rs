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
    /// From `<n>.pid`, once written.
    pub pid: Option<i32>,
    /// Whether that process still exists (`kill(pid, 0)`).
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
            term: BTreeMap::new(),
            ended: BTreeMap::new(),
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

/// `run.sh` for `items` (`(n, cwd)` pairs): `set -m`, then per command `cd`
/// to `<worktree>/<cwd>`, run `"${SHELL:-/bin/sh}" <dir>/<n>.cmd` in the
/// background with output to `<n>.log`, its pid to `<n>.pid`, wait, its
/// exit status to `<n>.exit` (temporary file + `mv`); at the end `done`
/// holding `nonce`. Every path single-quoted; no reported text in it.
///
/// Each command runs in a background subshell, which `set -m` makes the
/// leader of a process group of its own (its pid is `<n>.pid`, and the
/// group the timeout kill signals); the subshell writes `<n>.exit` itself
/// before it ends, so a `<n>.pid` whose process is gone without an
/// `<n>.exit` means it was stopped, never that the status is still on its
/// way. A `cd` that fails ends the command with `cd`'s status, its error in
/// the log.
pub fn script(nonce: &str, dir: &Path, worktree: &Path, items: &[(u32, &str)]) -> String {
    let mut out = String::from(
        "#!/bin/sh\n\
         # Noa Verify re-run script, generated: do not edit. Runs each reported\n\
         # command again, one after another, each in a process group of its own.\n\
         set -m\n",
    );
    let _ = writeln!(out, "d={}", quote(&dir.to_string_lossy()));
    let _ = writeln!(out, "w={}", quote(&worktree.to_string_lossy()));
    for (n, cwd) in items {
        let _ = writeln!(
            out,
            "( cd \"$w\"/{cwd} && \"${{SHELL:-/bin/sh}}\" \"$d/{n}.cmd\"; s=$?; \
             echo \"$s\" > \"$d/{n}.exit.tmp\" && mv -f \"$d/{n}.exit.tmp\" \"$d/{n}.exit\" ) \
             < /dev/null > \"$d/{n}.log\" 2>&1 &",
            cwd = quote(cwd)
        );
        let _ = writeln!(out, "p=$!");
        let _ = writeln!(
            out,
            "echo \"$p\" > \"$d/{n}.pid.tmp\" && mv -f \"$d/{n}.pid.tmp\" \"$d/{n}.pid\""
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

/// Whether process `pid` exists (`kill(pid, 0)`; one we may not signal
/// exists too).
fn exists(pid: i32) -> bool {
    // SAFETY: signal 0 only checks.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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
/// - a process gone without an `.exit` ends it [`End::NotRun`] at once
///   ("the re-run was stopped before it finished; …");
/// - past `watch`'s run limit, its process group gets SIGTERM, SIGKILL
///   [`RERUN_KILL_GRACE`](crate::model::types::RERUN_KILL_GRACE) later,
///   and it ends [`End::TimedOut`];
/// - no next `.pid` within the next-start limit after one ended ends that
///   command and every later one [`End::NotRun`] ("re-run did not
///   finish").
///
/// A timed-out command ends once its whole group is gone, or when SIGKILL
/// is sent; an outcome once given stays, whatever the files show later.
/// The first command waiting for its `.pid` stays without an outcome:
/// whether the script started at all is the re-run brief's start timeout.
/// Files are read without following a link; a `.pid` of 0 or 1 is never
/// taken for a process.
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
        let pid = read_number(dir, &format!("{n}.pid"))?.filter(|pid| *pid > 1);
        let alive = pid.is_some_and(exists);
        // Read after the liveness check: the subshell writes it before it
        // ends, so a process found gone has written it by now if ever.
        let exit = read_number(dir, &format!("{n}.exit"))?;
        if pid.is_some() {
            watch.seen.entry(n).or_insert(now);
        }
        if !watch.ended.contains_key(&n) {
            let decided = if let (Some(pid), Some(&term_at)) = (pid, watch.term.get(&n)) {
                if !group_exists(pid) {
                    Some(End::TimedOut)
                } else if now.saturating_sub(term_at) >= RERUN_KILL_GRACE {
                    let _ = kill_group(pid, Signal::Kill);
                    Some(End::TimedOut)
                } else {
                    None
                }
            } else if let Some(code) = exit {
                Some(End::Exited(code))
            } else if let Some(pid) = pid {
                if !alive {
                    Some(End::NotRun(STOPPED.to_owned()))
                } else if now.saturating_sub(watch.seen[&n]) >= limits.run {
                    let _ = kill_group(pid, Signal::Term);
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
            pid,
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
        std::process::Command::new("sh")
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
        let mut groups = Vec::new();
        let started = Instant::now();
        let status = watch_all(&dir, &request, limits, |status| {
            for item in &status.items {
                if let (Some(pid), true, None) = (item.pid, item.alive, &item.end)
                    && !groups.contains(&pid)
                {
                    // SAFETY: only reads the process group id.
                    assert_eq!(unsafe { libc::getpgid(pid) }, pid, "not its own group");
                    groups.push(pid);
                }
            }
        });
        child.wait().unwrap();
        let ends: Vec<End> = status
            .items
            .iter()
            .map(|i| i.end.clone().unwrap())
            .collect();
        assert_eq!(ends, [End::TimedOut, End::TimedOut, End::Exited(0)]);
        assert!(
            started.elapsed() >= Duration::from_millis(RERUN_KILL_GRACE),
            "the TERM-proof command ended before its SIGKILL"
        );
        assert_eq!(groups.len(), 2, "{groups:?}");
        std::thread::sleep(Duration::from_millis(200));
        for pid in groups {
            assert!(!group_exists(pid), "process group {pid} still runs");
        }
        assert!(matches!(
            outcome(0, End::TimedOut, None),
            Rerun::TimedOut { log: None }
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
        // Nothing written yet: no outcome (the brief's start timeout).
        let mut watch = Watch::new(LIMITS);
        let status = poll(&dir, &request, &mut watch, 0).unwrap();
        assert!(
            status
                .items
                .iter()
                .all(|i| i.end.is_none() && i.pid.is_none())
        );
        assert_eq!(status.done, None);

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
        assert!(text.contains("cd \"$w\"/'a b' &&"), "{text}");
        assert!(text.contains("cd \"$w\"/'x'\\''y' &&"), "{text}");
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("\nset -m\n"));
        assert!(text.contains("echo '00ff' > \"$d/done.tmp\""));
        assert!(nonce().unwrap() != nonce().unwrap());
        assert!(nonce().unwrap().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn every_kind_starts_its_script_detached_until_ac35_says_otherwise() {
        assert!(AgentKind::ALL.into_iter().all(detached));
    }
}
