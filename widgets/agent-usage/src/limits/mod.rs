//! Limits sources: one thread per agent (Claude Code, Codex, Antigravity),
//! each publishing normalised `UsageSource`s. A failure, a hang or a dead
//! thread affects only its own source: every child has a timeout, and a source
//! whose thread is gone is reported once as `Error("internal error")`.
//!
//! Read-only: credentials and logs are only read; no token is ever refreshed.

pub mod agy;
pub mod claude;
pub mod cli;
pub mod codex;
pub mod curl;

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::model::{Agent, SourceState, Unix, UsageSource};
use crate::paths::Roots;
use cli::Cancel;

pub type Wake = Arc<dyn Fn() + Send + Sync>;
/// Unix seconds; injected so tests drive the schedules.
pub type Clock = Arc<dyn Fn() -> Unix + Send + Sync>;

/// Every source polls at this interval.
pub const POLL_SECS: i64 = 300;
/// Back-off after a 429 or no connection: 30 → 60 → 120 → 300 s.
pub const RETRY_SECS: [i64; 4] = [30, 60, 120, 300];
/// How often a waiting source re-reads the clock (an upper bound on its
/// lateness; messages wake it at once).
const LIVE_TICK: Duration = Duration::from_secs(1);

pub(crate) enum Ctl<E> {
    Refresh,
    Stop,
    Event(E),
}

/// A source thread's side of its channels.
pub(crate) struct Link<E> {
    rx: Receiver<Ctl<E>>,
    /// For helper threads that feed this source (codex's stdout reader).
    pub(crate) tx: Sender<Ctl<E>>,
    out: Sender<UsageSource>,
    wake: Wake,
    pub(crate) cancel: Cancel,
    tick: Duration,
}

impl<E> Link<E> {
    pub(crate) fn publish(&self, source: UsageSource) {
        if self.out.send(source).is_ok() {
            (self.wake)();
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The next message, or `None` once `clock` reaches `deadline`. A closed
    /// channel reads as `Stop`.
    pub(crate) fn wait(&self, clock: &Clock, deadline: Option<Unix>) -> Option<Ctl<E>> {
        loop {
            if self.stopped() {
                return Some(Ctl::Stop);
            }
            if deadline.is_some_and(|d| clock() >= d) {
                return None;
            }
            match self.rx.recv_timeout(self.tick) {
                Ok(msg) => return Some(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Some(Ctl::Stop),
            }
        }
    }

    /// The next message, or `None` after one tick.
    pub(crate) fn wait_tick(&self) -> Option<Ctl<E>> {
        if self.stopped() {
            return Some(Ctl::Stop);
        }
        match self.rx.recv_timeout(self.tick) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Ctl::Stop),
        }
    }

    /// Drops queued refreshes (a fetch that just ran answers them) and
    /// reports whether a stop is among the queued messages. Other messages
    /// are put back in order.
    pub(crate) fn absorb_refreshes(&self) -> bool {
        let mut keep = Vec::new();
        let mut stop = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Ctl::Refresh => {}
                Ctl::Stop => stop = true,
                other => keep.push(other),
            }
        }
        for msg in keep {
            let _ = self.tx.send(msg);
        }
        if stop {
            let _ = self.tx.send(Ctl::Stop);
        }
        stop
    }
}

/// What the handle needs to run sources: cancellation, wake-up and tick.
#[derive(Clone)]
pub(crate) struct Spawner {
    pub(crate) cancel: Cancel,
    pub(crate) wake: Wake,
    pub(crate) tick: Duration,
}

pub(crate) struct Slot {
    agent: Agent,
    control: Box<dyn Fn(bool) + Send + Sync>,
    out: Receiver<UsageSource>,
    dead_reported: bool,
    thread: Option<JoinHandle<()>>,
}

/// Wakes the UI when a source thread ends, including by panic, so a dead
/// source is noticed without waiting for another update.
struct WakeOnExit(Wake);

impl Drop for WakeOnExit {
    fn drop(&mut self) {
        (self.0)();
    }
}

impl Spawner {
    pub(crate) fn spawn<E: Send + 'static>(
        &self,
        agent: Agent,
        body: impl FnOnce(Link<E>) + Send + 'static,
    ) -> Slot {
        let (tx, rx) = mpsc::channel::<Ctl<E>>();
        let (out_tx, out_rx) = mpsc::channel();
        let link = Link {
            rx,
            tx: tx.clone(),
            out: out_tx,
            wake: self.wake.clone(),
            cancel: self.cancel.clone(),
            tick: self.tick,
        };
        let exit = WakeOnExit(self.wake.clone());
        let thread = std::thread::Builder::new()
            .name(format!("agent-usage-{}", agent.id()))
            .spawn(move || {
                let _exit = exit;
                body(link);
            })
            .ok();
        Slot {
            agent,
            control: Box::new(move |stop| {
                let _ = tx.send(if stop { Ctl::Stop } else { Ctl::Refresh });
            }),
            out: out_rx,
            dead_reported: false,
            thread,
        }
    }
}

/// Dropping it stops every source thread and child process.
pub struct LimitsHandle {
    slots: Mutex<Vec<Slot>>,
    cancel: Cancel,
}

pub fn start(roots: Roots, wake: Arc<dyn Fn() + Send + Sync>) -> LimitsHandle {
    let spawner = Spawner {
        cancel: Cancel::default(),
        wake,
        tick: LIVE_TICK,
    };
    let clock: Clock = Arc::new(crate::time::now);
    let cancel = spawner.cancel.clone();
    let slots = vec![
        claude::spawn(
            claude::Deps::system(&roots, clock.clone(), cancel.clone()),
            &spawner,
        ),
        codex::spawn(codex::Deps::system(&roots, clock.clone()), &spawner),
        agy::spawn(agy::Deps::system(clock, cancel), &spawner),
    ];
    LimitsHandle::from_slots(slots, spawner.cancel)
}

impl LimitsHandle {
    pub(crate) fn from_slots(slots: Vec<Slot>, cancel: Cancel) -> LimitsHandle {
        LimitsHandle {
            slots: Mutex::new(slots),
            cancel,
        }
    }

    /// Ask every source now (Claude: at least 180 s between requests).
    pub fn refresh(&self) {
        for slot in self.slots.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            (slot.control)(false);
        }
    }

    /// Updates since the last call, newest last per agent.
    pub fn drain(&self) -> Vec<UsageSource> {
        let mut out = Vec::new();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        for slot in slots.iter_mut() {
            loop {
                match slot.out.try_recv() {
                    Ok(source) => out.push(source),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if !slot.dead_reported && !self.cancel.load(Ordering::SeqCst) {
                            slot.dead_reported = true;
                            out.push(internal_error(slot.agent));
                        }
                        break;
                    }
                }
            }
        }
        out
    }
}

fn internal_error(agent: Agent) -> UsageSource {
    UsageSource {
        state: SourceState::Error("internal error".into()),
        notes: vec!["internal error".into()],
        ..UsageSource::placeholder(agent)
    }
}

impl Drop for LimitsHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        for slot in slots.iter() {
            (slot.control)(true);
        }
        // Each thread sees the stop within a tick; running children poll the
        // cancel flag and are killed and reaped before the thread returns.
        for slot in slots.iter_mut() {
            if let Some(thread) = slot.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// The last good value republished as stale with `reason` as its note, or,
/// with none, a placeholder in `Error(reason)` (or `Unconfigured`).
pub(crate) fn unavailable(
    agent: Agent,
    last_good: Option<&UsageSource>,
    reason: &str,
    unconfigured: bool,
) -> UsageSource {
    match last_good {
        Some(good) => {
            let mut s = good.clone();
            s.notes = vec![reason.to_string()];
            s.state = match s.last_updated {
                Some(since) => SourceState::Stale { since },
                None => SourceState::Error(reason.to_string()),
            };
            s
        }
        None => UsageSource {
            state: if unconfigured {
                SourceState::Unconfigured
            } else {
                SourceState::Error(reason.to_string())
            },
            notes: vec![reason.to_string()],
            ..UsageSource::placeholder(agent)
        },
    }
}

/// "in 45s" / "in 3m" for a short delay.
pub(crate) fn in_secs(secs: i64) -> String {
    if secs < 60 {
        format!("in {}s", secs.max(0))
    } else {
        format!("in {}m", (secs + 59) / 60)
    }
}

/// "pro" → "Pro", "self_serve_business" → "Self Serve Business".
pub(crate) fn plan_name(kind: &str) -> Option<String> {
    let words: Vec<String> = kind
        .split(['_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;
    use std::time::Instant;

    pub(crate) const T0: Unix = 1_800_000_000;

    /// A clock the test moves by hand.
    pub(crate) fn fake_clock() -> (Arc<AtomicI64>, Clock) {
        let at = Arc::new(AtomicI64::new(T0));
        let read = at.clone();
        (at, Arc::new(move || read.load(Ordering::SeqCst)))
    }

    pub(crate) fn spawner() -> Spawner {
        Spawner {
            cancel: Cancel::default(),
            wake: Arc::new(|| {}),
            tick: Duration::from_millis(5),
        }
    }

    /// A handle plus the updates drained but not yet looked at.
    pub(crate) struct Probe {
        pub(crate) handle: LimitsHandle,
        buffer: std::cell::RefCell<std::collections::VecDeque<UsageSource>>,
    }

    impl std::ops::Deref for Probe {
        type Target = LimitsHandle;
        fn deref(&self) -> &LimitsHandle {
            &self.handle
        }
    }

    pub(crate) fn handle(slots: Vec<Slot>, spawner: &Spawner) -> Probe {
        Probe {
            handle: LimitsHandle::from_slots(slots, spawner.cancel.clone()),
            buffer: Default::default(),
        }
    }

    /// The next update from any source, waiting up to 5 s.
    pub(crate) fn next(h: &Probe) -> UsageSource {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            h.buffer.borrow_mut().extend(h.handle.drain());
            if let Some(s) = h.buffer.borrow_mut().pop_front() {
                return s;
            }
            assert!(Instant::now() < until, "no update within 5 s");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Asserts nothing is published for a short while.
    pub(crate) fn quiet(h: &Probe) {
        std::thread::sleep(Duration::from_millis(150));
        h.buffer.borrow_mut().extend(h.handle.drain());
        let got = h.buffer.borrow();
        assert!(got.is_empty(), "unexpected update: {got:?}");
    }

    /// Waits until `cond` holds, up to 5 s.
    pub(crate) fn until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn all_sources(
        spawner: &Spawner,
        agy_resolve: Arc<dyn Fn() -> Option<cli::Resolution> + Send + Sync>,
    ) -> (Probe, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (_, clock) = fake_clock();
        let claude =
            claude::tests::deps(clock.clone(), vec![claude::tests::ok(claude::tests::USAGE)]).0;
        let codex = codex::Deps {
            make_process: Arc::new(|| None),
            sessions: vec![dir.path().join("sessions")],
            now: clock.clone(),
            request_timeout: Duration::from_secs(5),
        };
        let agy = agy::Deps {
            resolve: agy_resolve,
            now: clock,
            cancel: spawner.cancel.clone(),
            version_timeout: Duration::from_secs(3),
            usage_timeout: Duration::from_secs(12),
        };
        let slots = vec![
            claude::spawn(claude, spawner),
            codex::spawn(codex, spawner),
            agy::spawn(agy, spawner),
        ];
        (handle(slots, spawner), dir)
    }

    fn collect(h: &Probe, n: usize) -> Vec<UsageSource> {
        let mut got = Vec::new();
        let until = Instant::now() + Duration::from_secs(5);
        while got.len() < n && Instant::now() < until {
            got.extend(h.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    }

    fn by_agent(got: &[UsageSource], agent: Agent) -> &UsageSource {
        got.iter().rev().find(|s| s.agent == agent).unwrap()
    }

    #[test]
    fn one_source_failing_leaves_the_others() {
        let spawner = spawner();
        // agy is installed but broken: its version check exits non-zero.
        let bin = tempfile::tempdir().unwrap();
        let agy = cli::tests::script(bin.path(), "agy", "exit 7");
        let (h, _dir) = all_sources(
            &spawner,
            Arc::new(move || {
                Some(cli::Resolution {
                    path: agy.clone(),
                    env: Default::default(),
                })
            }),
        );
        let got = collect(&h, 3);
        let claude = by_agent(&got, Agent::Claude);
        assert_eq!(claude.state, SourceState::Ok);
        assert_eq!(claude.windows.len(), 2);
        let codex = by_agent(&got, Agent::Codex);
        assert_eq!(codex.state, SourceState::Unconfigured);
        assert_eq!(codex.notes, ["codex not found in PATH"]);
        let agy = by_agent(&got, Agent::Agy);
        assert!(matches!(agy.state, SourceState::Error(_)), "{agy:?}");
        drop(h);
    }

    #[test]
    fn dead_source_thread_reports_internal_error() {
        let spawner = spawner();
        let (h, _dir) = all_sources(
            &spawner,
            Arc::new(|| panic!("synthetic failure in the agy source")),
        );
        let got = collect(&h, 3);
        let agy = by_agent(&got, Agent::Agy);
        assert_eq!(agy.state, SourceState::Error("internal error".into()));
        assert_eq!(by_agent(&got, Agent::Claude).state, SourceState::Ok);
        // Reported once; the others keep working.
        h.refresh();
        let later = collect(&h, 2);
        assert!(later.iter().all(|s| s.agent != Agent::Agy), "{later:?}");
        assert!(later.iter().any(|s| s.agent == Agent::Codex));
    }

    #[test]
    fn helpers_format_plans_and_delays() {
        assert_eq!(plan_name("max").as_deref(), Some("Max"));
        assert_eq!(
            plan_name("self_serve_business").as_deref(),
            Some("Self Serve Business")
        );
        assert_eq!(plan_name(""), None);
        assert_eq!(in_secs(30), "in 30s");
        assert_eq!(in_secs(120), "in 2m");
        assert_eq!(in_secs(150), "in 3m");
    }
}
