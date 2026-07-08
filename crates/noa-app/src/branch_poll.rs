//! Dedicated session-metadata worker (spec `docs/specs/session-sidebar.md`
//! FR-8/FR-9 + running-process display, ADR 0001 constraint a; NFR-2/NFR-3).
//! Ghostty has no analog.
//!
//! Two off-the-read-loop jobs share this one thread: cwd-driven git branch /
//! project-icon polling (below), and an adaptive tick that reads each live
//! session's foreground process name from a [`ForegroundProcessProbe`] and posts
//! a [`SessionDelta::Process`] on change (running-process display).
//!
//! git must never be spawned on the io read loop (`io_thread::feed_terminal`,
//! AC-18): a slow `git` on a network filesystem would stall pty draining. So a
//! separate thread — modeled on [`crate::io_thread::IoThreadHandle`] (its own
//! `JoinHandle` + shutdown channel, joined at teardown, Omen T6) — receives
//! OSC-7-driven cwd-change requests, runs `git -C <cwd> branch --show-current`
//! with a per-cwd throttle + negative cache, detects the project icon from cwd
//! markers (FR-9), and posts the result back as a [`SessionDelta::Branch`] over
//! the same `UserEvent` channel the io threads use.
//!
//! The pure decision ([`decide_branch_poll`]) and icon table
//! ([`detect_icon_with`]) take their inputs as parameters (no `Instant::now()`
//! or filesystem inside) so both are unit-testable without wall-clock sleeps or
//! a real directory (AC-10/AC-11).

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use noa_pty::ForegroundProcessProbe;
use winit::event_loop::EventLoopProxy;

use crate::events::UserEvent;
use crate::session_store::{IconKind, SessionCardId, SessionDelta};

/// Minimum poll interval for each live session's foreground process
/// (running-process display + agent-bell classification). A changed process
/// name resets to this cadence; stable names back off up to
/// [`PROCESS_POLL_MAX_INTERVAL`].
///
/// It is *not* gated on sidebar visibility: the store's `process` field feeds
/// the agent-bell → attention escalation (FR-A3), which must work with every
/// sidebar hidden.
pub const PROCESS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum stable-process poll interval. Keep this deliberately short:
/// agent-bell escalation is classified synchronously from the last posted
/// process name, so a transition from `zsh` to an agent can be missed until the
/// next poll. Four seconds cuts many-pane idle wakeups while bounding that stale
/// classification window.
pub const PROCESS_POLL_MAX_INTERVAL: Duration = Duration::from_secs(4);

/// A registration change for a session's foreground-process probe. The io/main
/// thread hands over a probe when a pane spawns and prunes dead ones on GC.
pub(crate) enum ProbeControl {
    Register {
        id: SessionCardId,
        probe: ForegroundProcessProbe,
    },
    /// Drop every probe whose id is not in this live set (GC choke point).
    Retain(HashSet<SessionCardId>),
}

/// Minimum interval between two `git` spawns for the *same* cwd (NFR-3). A
/// cwd-change request that arrives within this window of the last poll for that
/// cwd reuses the cached branch instead of re-spawning.
pub const BRANCH_POLL_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Wall-clock ceiling on one `git` invocation. A hung `git` (e.g. a stuck
/// network mount) is killed rather than wedging the worker — the poll degrades
/// to "no branch" (NFR-5) instead of blocking every later cwd change.
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Hard cap on distinct cwds retained in the branch cache. Without one the
/// cache grows with every directory ever visited across the app's whole life.
/// Evicting the least-recently-polled entry only costs a re-probe on revisit,
/// and 1024 far exceeds any realistic working set.
const BRANCH_CACHE_CAP: usize = 1024;

/// A cwd-change notification the worker acts on: re-detect the icon and
/// (throttled) re-poll the branch for `cwd`, then post a [`SessionDelta::Branch`]
/// keyed by `id`.
pub(crate) struct BranchPollRequest {
    pub(crate) id: SessionCardId,
    pub(crate) cwd: String,
}

/// Per-cwd cache entry: the last branch we resolved and when, plus whether the
/// directory is a git repo at all. A `non_git` entry is a negative cache — it
/// is reused without ever re-spawning `git` (NFR-3), so a directory that will
/// never be a repo costs at most one probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCache {
    pub branch: Option<String>,
    pub at: Instant,
    pub non_git: bool,
}

/// What [`decide_branch_poll`] resolves a cwd-change request to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchPollAction {
    /// The same cwd was polled within [`BRANCH_POLL_MIN_INTERVAL`]; reuse the
    /// cached branch and do not spawn `git`.
    Skip,
    /// Spawn `git` now (no cache, or the cache is stale).
    Spawn,
    /// A known non-git directory (negative cache); report the cached (empty)
    /// branch without spawning `git`.
    Hit(Option<String>),
}

/// Pure poll decision (AC-10). `now` and `last_poll` are parameters so the
/// throttle boundaries are testable without a wall-clock sleep. A negative
/// (non-git) cache short-circuits to [`BranchPollAction::Hit`] regardless of
/// timing; otherwise the throttle window since `last_poll` gates a
/// [`BranchPollAction::Skip`] versus a [`BranchPollAction::Spawn`].
pub fn decide_branch_poll(
    now: Instant,
    last_poll: Option<Instant>,
    cached: Option<&BranchCache>,
) -> BranchPollAction {
    if let Some(entry) = cached
        && entry.non_git
    {
        return BranchPollAction::Hit(entry.branch.clone());
    }
    match last_poll {
        Some(last) if now.saturating_duration_since(last) < BRANCH_POLL_MIN_INTERVAL => {
            BranchPollAction::Skip
        }
        _ => BranchPollAction::Spawn,
    }
}

/// Owned handle for stopping and joining the branch-poll worker, mirroring
/// [`crate::io_thread::IoThreadHandle`]. Dropping it (via [`Self::shutdown`])
/// closes the request channel and joins the thread.
pub(crate) struct BranchPollHandle {
    request_tx: Sender<BranchPollRequest>,
    probe_tx: Sender<ProbeControl>,
    shutdown_tx: Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl BranchPollHandle {
    /// Join budget at teardown. Kept above [`GIT_TIMEOUT`] with margin so a
    /// `git` finishing near its own ceiling still lets the worker return and
    /// join cleanly rather than being reported as hung.
    const JOIN_TIMEOUT: Duration = Duration::from_secs(3);

    /// Queue a cwd-change poll request. Non-blocking: the channel is unbounded
    /// and requests are already deduplicated at the source (only sent when a
    /// card's cwd actually changes), so it never backs up in practice.
    pub(crate) fn request(&self, id: SessionCardId, cwd: String) {
        let _ = self.request_tx.send(BranchPollRequest { id, cwd });
    }

    /// Register a session's foreground-process probe (running-process display
    /// and agent-bell classification). The worker polls it on
    /// [`PROCESS_POLL_INTERVAL`] and posts a [`SessionDelta::Process`] whenever
    /// the name changes.
    pub(crate) fn register_process_probe(&self, id: SessionCardId, probe: ForegroundProcessProbe) {
        let _ = self.probe_tx.send(ProbeControl::Register { id, probe });
    }

    /// Prune probes for sessions no longer live (GC choke point, mirrors
    /// [`crate::session_store::SessionStore::reconcile_sessions`]).
    pub(crate) fn retain_process_probes(&self, live: &[SessionCardId]) {
        let _ = self
            .probe_tx
            .send(ProbeControl::Retain(live.iter().copied().collect()));
    }

    /// Signal shutdown and join the worker within a bounded timeout, matching
    /// the io thread's teardown discipline (Omen T6).
    pub(crate) fn shutdown(&mut self) {
        let _ = self.shutdown_tx.send(());
        let deadline = Instant::now() + Self::JOIN_TIMEOUT;
        while self.join.as_ref().is_some_and(|join| !join.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let Some(join) = self.join.take() else {
            return;
        };
        if !join.is_finished() {
            self.join = Some(join);
            log::warn!(
                "branch-poll thread did not stop within {:?}",
                Self::JOIN_TIMEOUT
            );
            return;
        }
        if let Err(err) = join.join() {
            log::warn!("branch-poll thread panicked during shutdown: {err:?}");
        }
    }
}

/// Spawn the branch-poll worker (FR-8/FR-9). Runs until [`BranchPollHandle`] is
/// shut down or the event loop is gone.
pub(crate) fn spawn(proxy: EventLoopProxy<UserEvent>) -> BranchPollHandle {
    let (request_tx, request_rx) = crossbeam_channel::unbounded::<BranchPollRequest>();
    let (probe_tx, probe_rx) = crossbeam_channel::unbounded::<ProbeControl>();
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded::<()>(1);
    let join = std::thread::Builder::new()
        .name("noa-session-meta".to_string())
        .spawn(move || worker_loop(&proxy, &request_rx, &probe_rx, &shutdown_rx))
        .expect("failed to spawn session-metadata thread");
    BranchPollHandle {
        request_tx,
        probe_tx,
        shutdown_tx,
        join: Some(join),
    }
}

fn worker_loop(
    proxy: &EventLoopProxy<UserEvent>,
    request_rx: &Receiver<BranchPollRequest>,
    probe_rx: &Receiver<ProbeControl>,
    shutdown_rx: &Receiver<()>,
) {
    let mut cache: HashMap<String, BranchCache> = HashMap::new();
    // Per-session foreground-process probe + last-posted name (so only changes
    // are posted) + per-probe adaptive schedule.
    let mut probes: HashMap<SessionCardId, ProcessProbeEntry> = HashMap::new();
    loop {
        let mut sel = crossbeam_channel::Select::new();
        let shutdown_op = sel.recv(shutdown_rx);
        let request_op = sel.recv(request_rx);
        let probe_op = sel.recv(probe_rx);

        // Tick at the earliest due probe instead of a fixed 1s cadence for every
        // pane. Not gated on sidebar visibility — see [`PROCESS_POLL_INTERVAL`].
        let selected = match next_process_poll_at(&probes) {
            Some(deadline) => sel
                .select_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok(),
            None => Some(sel.select()),
        };
        let Some(oper) = selected else {
            // At least one process poll is due: re-read due probes and post changes.
            if !poll_due_processes(&mut probes, proxy, Instant::now()) {
                break; // event loop gone
            }
            continue;
        };

        match oper.index() {
            i if i == shutdown_op => {
                let _ = oper.recv(shutdown_rx);
                break;
            }
            i if i == request_op => {
                let Ok(request) = oper.recv(request_rx) else {
                    break; // main thread / App dropped
                };
                let (branch, icon) = resolve(&mut cache, &request.cwd);
                if proxy
                    .send_event(UserEvent::SessionDelta(SessionDelta::Branch {
                        id: request.id,
                        branch,
                        icon,
                    }))
                    .is_err()
                {
                    break; // event loop gone
                }
            }
            i if i == probe_op => match oper.recv(probe_rx) {
                Ok(ProbeControl::Register { id, probe }) => {
                    probes.insert(id, ProcessProbeEntry::new(probe, Instant::now()));
                }
                Ok(ProbeControl::Retain(live)) => {
                    probes.retain(|id, _| live.contains(id));
                }
                Err(_) => break, // main thread / App dropped
            },
            _ => unreachable!("select only registers shutdown, request, and probe"),
        }
    }
}

struct ProcessProbeEntry {
    probe: ForegroundProcessProbe,
    state: ProcessPollState,
}

impl ProcessProbeEntry {
    fn new(probe: ForegroundProcessProbe, now: Instant) -> Self {
        Self {
            probe,
            state: ProcessPollState::new(now),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessPollState {
    last: Option<String>,
    schedule: ProcessPollSchedule,
}

impl ProcessPollState {
    fn new(now: Instant) -> Self {
        Self {
            last: None,
            schedule: ProcessPollSchedule::new(now),
        }
    }

    fn record_name(&mut self, now: Instant, name: Option<String>) -> bool {
        let changed = self.last != name;
        self.schedule.record_poll(now, changed);
        if changed {
            self.last = name;
        }
        changed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessPollSchedule {
    interval: Duration,
    next_poll_at: Instant,
}

impl ProcessPollSchedule {
    fn new(now: Instant) -> Self {
        Self {
            interval: PROCESS_POLL_INTERVAL,
            next_poll_at: now,
        }
    }

    fn record_poll(&mut self, now: Instant, changed: bool) {
        self.interval = next_process_poll_interval(self.interval, changed);
        self.next_poll_at = now + self.interval;
    }
}

fn next_process_poll_interval(current: Duration, changed: bool) -> Duration {
    if changed {
        PROCESS_POLL_INTERVAL
    } else {
        current.saturating_mul(2).min(PROCESS_POLL_MAX_INTERVAL)
    }
}

fn next_process_poll_at(probes: &HashMap<SessionCardId, ProcessProbeEntry>) -> Option<Instant> {
    probes
        .values()
        .map(|entry| entry.state.schedule.next_poll_at)
        .min()
}

/// Poll due probes and post a [`SessionDelta::Process`] for any whose foreground
/// process name changed since the last post. Returns `false` when the event loop
/// is gone (caller should stop).
fn poll_due_processes(
    probes: &mut HashMap<SessionCardId, ProcessProbeEntry>,
    proxy: &EventLoopProxy<UserEvent>,
    now: Instant,
) -> bool {
    for (id, entry) in probes.iter_mut() {
        if entry.state.schedule.next_poll_at > now {
            continue;
        }
        let name = entry.probe.poll();
        if !entry.state.record_name(now, name.clone()) {
            continue;
        }
        if proxy
            .send_event(UserEvent::SessionDelta(SessionDelta::Process {
                id: *id,
                process: name,
            }))
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Resolve the branch (throttled/cached, FR-8) and icon (FR-9) for a cwd,
/// updating `cache` when `git` is spawned.
fn resolve(cache: &mut HashMap<String, BranchCache>, cwd: &str) -> (Option<String>, IconKind) {
    let icon = detect_icon(Path::new(cwd));
    let entry = cache.get(cwd);
    let branch = match decide_branch_poll(Instant::now(), entry.map(|c| c.at), entry) {
        BranchPollAction::Hit(branch) => branch,
        BranchPollAction::Skip => entry.and_then(|c| c.branch.clone()),
        BranchPollAction::Spawn => {
            let probe = run_git_branch(cwd, GIT_TIMEOUT);
            evict_branch_cache_if_full(cache, cwd);
            cache.insert(
                cwd.to_string(),
                BranchCache {
                    branch: probe.branch.clone(),
                    at: Instant::now(),
                    non_git: !probe.is_git,
                },
            );
            probe.branch
        }
    };
    (branch, icon)
}

/// Drop the least-recently-polled entry when inserting `cwd` would push the
/// cache past [`BRANCH_CACHE_CAP`]. The linear scan is fine: it runs at most
/// once per `git` spawn, which is already throttled and process-spawn priced.
fn evict_branch_cache_if_full(cache: &mut HashMap<String, BranchCache>, cwd: &str) {
    if cache.len() < BRANCH_CACHE_CAP || cache.contains_key(cwd) {
        return;
    }
    if let Some(oldest) = cache
        .iter()
        .min_by_key(|(_, entry)| entry.at)
        .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest);
    }
}

/// Outcome of one `git branch --show-current` probe. `is_git` is false when git
/// failed or the directory is not a repo (drives the negative cache); a repo on
/// a detached HEAD is `is_git = true` with `branch = None`.
struct BranchProbe {
    branch: Option<String>,
    is_git: bool,
}

impl BranchProbe {
    fn not_git() -> Self {
        Self {
            branch: None,
            is_git: false,
        }
    }
}

/// Run `git -C <cwd> branch --show-current`, killing it after `timeout`. A
/// spawn error, non-zero exit, or timeout all degrade to "not a git repo"
/// (NFR-5) so the card simply shows no branch.
fn run_git_branch(cwd: &str, timeout: Duration) -> BranchProbe {
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("branch")
        .arg("--show-current")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return BranchProbe::not_git(),
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return BranchProbe::not_git();
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                let name = out.trim();
                return BranchProbe {
                    branch: (!name.is_empty()).then(|| name.to_string()),
                    is_git: true,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return BranchProbe::not_git();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return BranchProbe::not_git(),
        }
    }
}

/// Detect a card's project icon from its cwd markers (FR-9), first-match in the
/// spec's order. Does the filesystem checks, delegating the ordering to the
/// pure [`detect_icon_with`].
fn detect_icon(cwd: &Path) -> IconKind {
    detect_icon_with(
        |marker| cwd.join(marker).exists(),
        || dir_has_extension(cwd, "tf"),
    )
}

/// Pure first-match icon table (AC-11): `has(marker)` reports whether a named
/// marker file/dir is present, `has_tf` whether any `*.tf` file exists. Order is
/// the spec's: Cargo.toml → package.json → *.tf → go.mod → pyproject.toml →
/// .git → folder.
fn detect_icon_with(has: impl Fn(&str) -> bool, has_tf: impl Fn() -> bool) -> IconKind {
    if has("Cargo.toml") {
        IconKind::Rust
    } else if has("package.json") {
        IconKind::Node
    } else if has_tf() {
        IconKind::Terraform
    } else if has("go.mod") {
        IconKind::Go
    } else if has("pyproject.toml") {
        IconKind::Python
    } else if has(".git") {
        IconKind::Git
    } else {
        IconKind::Folder
    }
}

/// Whether `dir` directly contains any file with the given extension.
fn dir_has_extension(dir: &Path, ext: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // AC-10 (FR-8, NFR-3): the pure poll decision, exercised at explicit `now`
    // values with no wall-clock sleep.
    #[test]
    fn decide_branch_poll_skips_spawns_and_hits() {
        let now = Instant::now();

        // No cache, no prior poll → spawn.
        assert_eq!(decide_branch_poll(now, None, None), BranchPollAction::Spawn);

        // Polled < 1s ago (a git dir cache) → skip, don't re-spawn.
        let fresh = BranchCache {
            branch: Some("main".to_string()),
            at: now - BRANCH_POLL_MIN_INTERVAL / 2,
            non_git: false,
        };
        assert_eq!(
            decide_branch_poll(now, Some(fresh.at), Some(&fresh)),
            BranchPollAction::Skip
        );

        // Polled ≥ 1s ago → spawn again.
        let stale = BranchCache {
            branch: Some("main".to_string()),
            at: now - BRANCH_POLL_MIN_INTERVAL,
            non_git: false,
        };
        assert_eq!(
            decide_branch_poll(now, Some(stale.at), Some(&stale)),
            BranchPollAction::Spawn
        );

        // Negative-cached non-git dir → hit (reuse) regardless of timing.
        let negative = BranchCache {
            branch: None,
            at: now - BRANCH_POLL_MIN_INTERVAL * 100,
            non_git: true,
        };
        assert_eq!(
            decide_branch_poll(now, Some(negative.at), Some(&negative)),
            BranchPollAction::Hit(None)
        );
    }

    #[test]
    fn process_poll_state_posts_changes_backs_off_and_resets() {
        let now = Instant::now();
        let mut state = ProcessPollState::new(now);
        assert_eq!(state.last, None);
        assert_eq!(state.schedule.interval, PROCESS_POLL_INTERVAL);
        assert_eq!(state.schedule.next_poll_at, now);

        assert!(state.record_name(now, Some("zsh".to_string())));
        assert_eq!(state.last.as_deref(), Some("zsh"));
        assert_eq!(state.schedule.interval, PROCESS_POLL_INTERVAL);
        assert_eq!(state.schedule.next_poll_at, now + PROCESS_POLL_INTERVAL);

        assert!(!state.record_name(now + Duration::from_secs(1), Some("zsh".to_string())));
        assert_eq!(state.schedule.interval, Duration::from_secs(2));
        assert_eq!(state.schedule.next_poll_at, now + Duration::from_secs(3));

        assert!(!state.record_name(now + Duration::from_secs(3), Some("zsh".to_string())));
        assert_eq!(state.schedule.interval, PROCESS_POLL_MAX_INTERVAL);
        assert_eq!(state.schedule.next_poll_at, now + Duration::from_secs(7));

        assert!(!state.record_name(now + Duration::from_secs(7), Some("zsh".to_string())));
        assert_eq!(state.schedule.interval, PROCESS_POLL_MAX_INTERVAL);
        assert_eq!(state.schedule.next_poll_at, now + Duration::from_secs(11));

        assert!(state.record_name(now + Duration::from_secs(11), Some("codex".to_string())));
        assert_eq!(state.last.as_deref(), Some("codex"));
        assert_eq!(state.schedule.interval, PROCESS_POLL_INTERVAL);
        assert_eq!(state.schedule.next_poll_at, now + Duration::from_secs(12));
    }

    #[test]
    fn stable_process_poll_rate_scales_with_max_backoff() {
        // A stable pane polls once per max interval instead of once per minimum
        // interval, so many-pane idle poll pressure drops by the same ratio.
        let min_secs = PROCESS_POLL_INTERVAL.as_secs();
        let max_secs = PROCESS_POLL_MAX_INTERVAL.as_secs();
        assert_eq!(min_secs, 1);
        assert_eq!(max_secs, 4);

        for panes in [1, 10, 50] {
            let fixed_interval_polls_per_max_window = panes * max_secs / min_secs;
            let stable_backoff_polls_per_max_window = panes;

            assert_eq!(
                stable_backoff_polls_per_max_window * 4,
                fixed_interval_polls_per_max_window
            );
        }
    }

    // The branch cache never outgrows its cap: inserting a new cwd at the cap
    // evicts the least-recently-polled entry, and a re-poll of a cached cwd
    // evicts nothing.
    #[test]
    fn branch_cache_evicts_oldest_at_cap() {
        let now = Instant::now();
        let mut cache: HashMap<String, BranchCache> = (0..BRANCH_CACHE_CAP)
            .map(|i| {
                (
                    format!("/dir/{i}"),
                    BranchCache {
                        branch: None,
                        at: now + Duration::from_secs(i as u64),
                        non_git: false,
                    },
                )
            })
            .collect();

        // Refreshing an existing cwd at the cap evicts nothing.
        evict_branch_cache_if_full(&mut cache, "/dir/5");
        assert_eq!(cache.len(), BRANCH_CACHE_CAP);

        // A new cwd at the cap evicts the entry with the oldest poll time.
        evict_branch_cache_if_full(&mut cache, "/dir/new");
        assert_eq!(cache.len(), BRANCH_CACHE_CAP - 1);
        assert!(!cache.contains_key("/dir/0"));
        assert!(cache.contains_key("/dir/1"));
    }

    // AC-11 (FR-9): the icon table returns the first matching marker.
    #[test]
    fn detect_icon_first_match_table() {
        let icon = |markers: &[&str]| {
            let set: HashSet<String> = markers.iter().map(|m| m.to_string()).collect();
            let has_tf = set.iter().any(|m| m.ends_with(".tf"));
            detect_icon_with(|m| set.contains(m), || has_tf)
        };

        assert_eq!(icon(&["Cargo.toml"]), IconKind::Rust);
        assert_eq!(icon(&["package.json"]), IconKind::Node);
        assert_eq!(icon(&["main.tf"]), IconKind::Terraform);
        assert_eq!(icon(&["go.mod"]), IconKind::Go);
        assert_eq!(icon(&["pyproject.toml"]), IconKind::Python);
        assert_eq!(icon(&[".git"]), IconKind::Git);
        assert_eq!(icon(&[]), IconKind::Folder);

        // First-match precedence: Cargo.toml wins over a co-present package.json
        // and .git; *.tf wins over go.mod.
        assert_eq!(
            icon(&["Cargo.toml", "package.json", ".git"]),
            IconKind::Rust
        );
        assert_eq!(icon(&["main.tf", "go.mod"]), IconKind::Terraform);
        // A repo with only .git (no language marker) → git.
        assert_eq!(icon(&[".git", "README.md"]), IconKind::Git);
    }

    // A real non-git temp dir probes as "not git" (drives the negative cache).
    #[test]
    fn run_git_branch_on_a_non_repo_is_not_git() {
        let dir = std::env::temp_dir();
        let probe = run_git_branch(dir.to_str().unwrap(), Duration::from_secs(2));
        // The invariant the negative cache relies on: a not-git result (git
        // absent, non-zero exit, or timeout) carries no branch.
        if !probe.is_git {
            assert!(probe.branch.is_none());
        }
    }
}
