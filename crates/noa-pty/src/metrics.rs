//! Pane metrics collection (spec `docs/specs/panel-metrics-view.md`,
//! FR-4/FR-8): the foreground-process-tree CPU/memory/process-count/elapsed
//! numbers behind the process-monitor overlay. macOS-only measurement;
//! everything degrades to `None`/empty off macOS (ASSUME-3, NFR-4/5) rather
//! than adding a new dependency (`sysinfo` was deliberately rejected).
//!
//! Capturing a snapshot ([`ProcSnapshot::capture`]) is the one syscall-heavy
//! step (`proc_listallpids` + one `proc_pidinfo` per pid) — it runs once per
//! branch-poll tick and is shared by every pane's
//! `ForegroundProcessProbe::poll_metrics` call (NFR-2). [`foreground_tree`]
//! is pure (no syscalls) so it is unit-testable against a hand-built
//! snapshot: a dead group leader, a reparented grandchild, and a vanished
//! pid are all exercised in `tests` below without touching a real process
//! table.

use std::time::SystemTime;

/// One process's identity/lineage/start-time, as read from
/// `proc_pidinfo(PROC_PIDTBSDINFO)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcRecord {
    pub pid: i32,
    pub ppid: u32,
    pub pgid: u32,
    pub start_tvsec: u64,
}

/// A whole-system process-table snapshot, captured once per branch-poll tick
/// (NFR-2) and shared read-only by every pane's [`foreground_tree`] lookup
/// that tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcSnapshot {
    pub procs: Vec<ProcRecord>,
}

impl ProcSnapshot {
    /// Capture every live process's `(pid, ppid, pgid, start_tvsec)`. macOS:
    /// `proc_listallpids` (sized, then filled, with margin for processes
    /// spawned between the two calls) followed by one `proc_pidinfo` per pid;
    /// a pid whose info lookup fails (already exited) is silently skipped
    /// rather than aborting the whole capture. Off macOS: always empty
    /// (AC-15).
    pub fn capture() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                procs: macos::capture_all(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }
}

/// A pane's foreground-process-tree metrics (FR-4): CPU as 1-core-=-100%
/// permille (`None` before the first two-sample diff, or measurement
/// unavailable), the tree's summed physical-footprint memory, its process
/// count, and its start time (group leader, falling back to the oldest
/// surviving member).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneMetrics {
    pub cpu_permille: Option<u32>,
    pub mem_bytes: u64,
    pub proc_count: u32,
    pub started_at: Option<SystemTime>,
}

/// The foreground process-group tree (FR-4): every process whose `pgid`
/// equals `pgid`, unioned with their descendants (walked via `ppid`, BFS).
/// The union guards a descendant whose own `pgid` has drifted from its
/// ancestor's (rare, but the spec calls it out — e.g. an explicit `setpgid`
/// deep in a pipeline); ordinary job-control children already match by `pgid`
/// directly, so a leader that has since exited (but left live group members
/// behind) or a grandchild reparented to `launchd` (which does not change its
/// `pgid`) are both still captured. Pure — no syscalls — so it's directly
/// unit-testable against a hand-built [`ProcRecord`] slice. Order is
/// unspecified; callers that need determinism should sort the result.
pub fn foreground_tree(pgid: u32, procs: &[ProcRecord]) -> Vec<i32> {
    use std::collections::{HashMap, HashSet};

    let mut children: HashMap<u32, Vec<i32>> = HashMap::new();
    for proc in procs {
        children.entry(proc.ppid).or_default().push(proc.pid);
    }

    let mut visited: HashSet<i32> = HashSet::new();
    let mut queue: Vec<i32> = Vec::new();
    for proc in procs.iter().filter(|p| p.pgid == pgid) {
        if visited.insert(proc.pid) {
            queue.push(proc.pid);
        }
    }

    let mut head = 0;
    while head < queue.len() {
        let pid = queue[head];
        head += 1;
        if let Some(kids) = children.get(&(pid as u32)) {
            for &kid in kids {
                if visited.insert(kid) {
                    queue.push(kid);
                }
            }
        }
    }

    queue
}

/// The tree's elapsed-time anchor (FR-4 L2): the group leader's
/// `start_tvsec` when the leader (`pid == pgid`) is present in `tree`;
/// otherwise the oldest (minimum `start_tvsec`) surviving member; `None` if
/// `tree` is empty or none of its pids resolve in `procs`.
pub(crate) fn tree_started_at(pgid: u32, procs: &[ProcRecord], tree: &[i32]) -> Option<SystemTime> {
    let by_pid: std::collections::HashMap<i32, &ProcRecord> =
        procs.iter().map(|p| (p.pid, p)).collect();
    let leader_start = tree
        .contains(&(pgid as i32))
        .then(|| by_pid.get(&(pgid as i32)).map(|p| p.start_tvsec))
        .flatten();
    let start_tvsec = leader_start.or_else(|| {
        tree.iter()
            .filter_map(|pid| by_pid.get(pid).map(|p| p.start_tvsec))
            .min()
    })?;
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(start_tvsec))
}

/// Convert mach absolute time units to nanoseconds via the machine's
/// `mach_timebase_info` ratio (`ticks * numer / denom`), widening through
/// `u128` so a long-lived process's accumulated CPU time cannot overflow the
/// multiply. Pure — the ratio is a parameter — so the Apple Silicon (125/3)
/// and Intel (1/1) cases are unit-testable on any platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mach_ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    if denom == 0 {
        return ticks;
    }
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

#[cfg(target_os = "macos")]
mod macos {
    use super::ProcRecord;
    use std::mem::size_of;
    use std::os::raw::c_void;

    /// `proc_listallpids` sized, then filled, with margin for processes
    /// spawned between the two calls (the count can only grow, never shrink,
    /// between them in practice, but a defensive retry loop handles it
    /// either way).
    pub(super) fn capture_all() -> Vec<ProcRecord> {
        let Some(pids) = list_all_pids() else {
            return Vec::new();
        };
        pids.into_iter().filter_map(bsdinfo_for).collect()
    }

    fn list_all_pids() -> Option<Vec<libc::pid_t>> {
        // `proc_listallpids` returns a COUNT of pids (both for the sizing
        // call and the fill call), not a byte length — dividing by
        // `size_of::<pid_t>()` here would under-allocate and truncate the
        // list to a fraction of the real process table.
        let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return None;
        }
        let mut capacity = (needed as usize).saturating_add(64);
        loop {
            let mut buf: Vec<libc::pid_t> = vec![0; capacity];
            let ret = unsafe {
                libc::proc_listallpids(
                    buf.as_mut_ptr() as *mut c_void,
                    (buf.len() * size_of::<libc::pid_t>()) as libc::c_int,
                )
            };
            if ret <= 0 {
                return None;
            }
            let written = ret as usize;
            if written < buf.len() {
                buf.truncate(written);
                buf.retain(|&pid| pid > 0);
                return Some(buf);
            }
            // The buffer filled exactly (the table grew past the margin
            // mid-call) — double and retry rather than risk a truncated list.
            capacity *= 2;
        }
    }

    fn bsdinfo_for(pid: libc::pid_t) -> Option<ProcRecord> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                size,
            )
        };
        if written != size {
            return None; // exited between listing and lookup, or denied.
        }
        Some(ProcRecord {
            pid,
            ppid: info.pbi_ppid,
            pgid: info.pbi_pgid,
            start_tvsec: info.pbi_start_tvsec,
        })
    }

    /// `mach_timebase_info`'s out-struct, declared locally: libc's own
    /// binding is deprecated in favor of the `mach2` crate, and NFR-4 rules
    /// out a new dependency for one constant-per-boot syscall.
    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
    }

    /// `mach_timebase_info` numer/denom, resolved once — the conversion
    /// factor from mach absolute time units to nanoseconds. Constant for the
    /// machine's lifetime, so a `OnceLock` avoids re-issuing the syscall per
    /// rusage lookup. On Apple Silicon this is 125/3 (not 1/1 as on Intel),
    /// so skipping the conversion underreports CPU time ~41.7x.
    fn timebase() -> (u32, u32) {
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        *TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: plain out-parameter struct matching the C layout; a
            // failure leaves it zeroed and we fall back to identity.
            let ret = unsafe { mach_timebase_info(&mut info) };
            if ret != 0 || info.denom == 0 {
                (1, 1)
            } else {
                (info.numer, info.denom)
            }
        })
    }

    /// `proc_pid_rusage(RUSAGE_INFO_V4)` for one pid: `(user + system CPU
    /// time in ns, physical-footprint bytes)`. `ri_user_time`/`ri_system_time`
    /// are in mach absolute time units and converted here via
    /// [`super::mach_ticks_to_ns`]. `None` for a vanished pid or a lookup
    /// denied by the OS (unrelated user, sandboxing) — the caller treats that
    /// pid as contributing zero rather than aborting the tree sum (FR-8).
    pub(crate) fn rusage_ns_and_footprint(pid: libc::pid_t) -> Option<(u64, u64)> {
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                &mut info as *mut _ as *mut libc::rusage_info_t,
            )
        };
        if ret != 0 {
            return None;
        }
        let raw = info.ri_user_time.saturating_add(info.ri_system_time);
        let (numer, denom) = timebase();
        Some((super::mach_ticks_to_ns(raw, numer, denom), info.ri_phys_footprint))
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos::rusage_ns_and_footprint;

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: i32, ppid: u32, pgid: u32, start: u64) -> ProcRecord {
        ProcRecord {
            pid,
            ppid,
            pgid,
            start_tvsec: start,
        }
    }

    // AC-10 groundwork: a plain job — leader + one child sharing the same
    // pgid — resolves to both pids via the direct pgid match.
    #[test]
    fn foreground_tree_includes_plain_job_control_group() {
        let procs = vec![rec(100, 1, 100, 0), rec(200, 100, 100, 0)];
        let mut tree = foreground_tree(100, &procs);
        tree.sort();
        assert_eq!(tree, vec![100, 200]);
    }

    // The group leader has already exited (no ProcRecord for pid 100), but a
    // surviving member still reports pgid 100 — it must still be captured.
    #[test]
    fn foreground_tree_survives_a_dead_leader() {
        let procs = vec![rec(200, 100, 100, 0), rec(300, 200, 100, 0)];
        let mut tree = foreground_tree(100, &procs);
        tree.sort();
        assert_eq!(tree, vec![200, 300]);
    }

    // A grandchild reparented to launchd (ppid == 1) but whose pgid is
    // unchanged is captured directly by the pgid match (the common case);
    // the ppid-BFS union additionally covers a descendant whose pgid has
    // drifted from its ancestor's.
    #[test]
    fn foreground_tree_keeps_reparented_grandchild_via_pgid() {
        let procs = vec![
            rec(100, 1, 100, 0),
            // Intermediate parent (200) has already exited — no record.
            rec(300, 1, 100, 0), // reparented to launchd, pgid unchanged.
        ];
        let mut tree = foreground_tree(100, &procs);
        tree.sort();
        assert_eq!(tree, vec![100, 300]);
    }

    // A descendant whose own pgid differs from the group's is still pulled
    // in via the ppid-BFS union.
    #[test]
    fn foreground_tree_unions_in_a_descendant_with_a_different_pgid() {
        let procs = vec![
            rec(100, 1, 100, 0),
            rec(200, 100, 100, 0),
            rec(300, 200, 999, 0), // pgid drifted, but ppid chains to 200.
        ];
        let mut tree = foreground_tree(100, &procs);
        tree.sort();
        assert_eq!(tree, vec![100, 200, 300]);
    }

    // A dangling ppid reference (points at a pid absent from the snapshot —
    // it vanished between listing and lookup) must not panic.
    #[test]
    fn foreground_tree_ignores_a_vanished_ppid_reference() {
        let procs = vec![rec(100, 1, 100, 0), rec(200, 9999, 100, 0)];
        let mut tree = foreground_tree(100, &procs);
        tree.sort();
        assert_eq!(tree, vec![100, 200]);
    }

    #[test]
    fn foreground_tree_empty_when_pgid_has_no_members() {
        let procs = vec![rec(100, 1, 100, 0)];
        assert!(foreground_tree(999, &procs).is_empty());
    }

    // AC-10: the elapsed-time anchor prefers the leader; falls back to the
    // oldest surviving member when the leader is gone; `None` when nothing in
    // the tree resolves.
    // CRITICAL-2 regression: `proc_listallpids` returns pid COUNTS, not
    // bytes — dividing by `size_of::<pid_t>()` truncated the capture to a
    // fraction of the process table. A real macOS system always has far more
    // live processes than the truncated capture's ~1/16 would leave, and our
    // own process must be present. (pid 1 is NOT asserted: launchd's
    // `proc_pidinfo` is permission-denied for unprivileged callers and is
    // legitimately skipped.)
    #[cfg(target_os = "macos")]
    #[test]
    fn capture_returns_the_full_process_table() {
        let snap = ProcSnapshot::capture();
        assert!(
            snap.procs.len() > 100,
            "expected a full process table, got {} entries",
            snap.procs.len()
        );
        let me = std::process::id() as i32;
        assert!(
            snap.procs.iter().any(|p| p.pid == me),
            "our own pid missing from the capture"
        );
    }

    // CRITICAL-1 regression: mach absolute time units must be scaled by the
    // timebase ratio — Apple Silicon's 125/3 means raw units understate ns
    // by ~41.7x if passed through unconverted.
    #[test]
    fn mach_ticks_to_ns_applies_the_timebase_ratio() {
        // Intel identity ratio: unchanged.
        assert_eq!(mach_ticks_to_ns(1_000_000, 1, 1), 1_000_000);
        // Apple Silicon ratio (125/3): 24 ticks → 1000 ns.
        assert_eq!(mach_ticks_to_ns(24, 125, 3), 1000);
        // One full core-second of ticks converts exactly.
        assert_eq!(mach_ticks_to_ns(24_000_000, 125, 3), 1_000_000_000);
        // Widening through u128: a huge accumulated tick count must not
        // overflow the multiply.
        assert_eq!(
            mach_ticks_to_ns(u64::MAX / 125, 125, 1),
            (u64::MAX / 125) * 125
        );
        // A zeroed (failed) timebase degrades to identity, never divides by 0.
        assert_eq!(mach_ticks_to_ns(42, 0, 0), 42);
    }

    #[test]
    fn tree_started_at_prefers_leader_then_oldest_member_then_none() {
        let procs = vec![rec(100, 1, 100, 500), rec(200, 100, 100, 800)];
        let tree = vec![100, 200];
        assert_eq!(
            tree_started_at(100, &procs, &tree),
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(500))
        );

        // Leader gone: falls back to the oldest surviving member.
        let procs_no_leader = vec![rec(200, 100, 100, 800), rec(300, 200, 100, 900)];
        let tree_no_leader = vec![200, 300];
        assert_eq!(
            tree_started_at(100, &procs_no_leader, &tree_no_leader),
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(800))
        );

        // Nothing resolves.
        assert_eq!(tree_started_at(100, &[], &[]), None);
    }
}
