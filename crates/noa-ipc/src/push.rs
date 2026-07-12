//! Subscriptions and server -> client push (spec FR-15..17, NFR-2).
//!
//! Terminal-side callers (io_thread / main thread) only ever call
//! [`Broadcaster::broadcast_state_changed`] / `broadcast_output`, which
//! push into a bounded per-connection queue and never block. Each
//! connection's own thread drains its queue and writes WS frames — slow or
//! stalled clients only ever affect their own queue.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::protocol::{EventKind, Panel, Row};

const DEFAULT_QUEUE_CAP: usize = 256;

/// Hard cap on live subscriptions per connection (R-2). Without this, a
/// client that repeatedly calls `noa.subscribe` without ever unsubscribing
/// grows `ConnEntry::subs` without bound — cheap for an attacker, unbounded
/// memory for the server.
const MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventMask(u8);

impl EventMask {
    pub const STATE_CHANGED: EventMask = EventMask(1 << 0);
    pub const OUTPUT: EventMask = EventMask(1 << 1);

    pub fn contains(self, other: EventMask) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, other: EventMask) {
        self.0 |= other.0;
    }

    pub fn from_events(events: &[EventKind]) -> EventMask {
        let mut mask = EventMask::default();
        for e in events {
            mask.insert(match e {
                EventKind::StateChanged => EventMask::STATE_CHANGED,
                EventKind::Output => EventMask::OUTPUT,
            });
        }
        mask
    }
}

/// A queued notification, pre-serialization.
#[derive(Clone, Debug)]
pub enum QueuedNotification {
    StateChanged { panels: Vec<Panel>, dropped: bool },
    Output { pane_id: u64, lines: Vec<Row>, dropped: bool },
}

struct QueueInner {
    items: VecDeque<QueuedNotification>,
    /// Set when an item was evicted (drop-oldest) and not yet surfaced to
    /// the client. Carried forward across `drain()` calls until an `Output`
    /// notification is available to tag (FR-17).
    dropped_pending: bool,
}

/// A bounded, wait-free (mutex-protected, uncontended-fast) per-connection
/// notification queue. `push` never blocks and drops the oldest entry on
/// overflow.
pub struct PushQueue {
    inner: Mutex<QueueInner>,
    cap: usize,
}

impl PushQueue {
    fn new(cap: usize) -> Self {
        PushQueue {
            inner: Mutex::new(QueueInner { items: VecDeque::with_capacity(cap), dropped_pending: false }),
            cap,
        }
    }

    pub fn push(&self, item: QueuedNotification) {
        let mut guard = self.inner.lock();
        if guard.items.len() >= self.cap {
            guard.items.pop_front();
            guard.dropped_pending = true;
        }
        guard.items.push_back(item);
    }

    /// Drains all queued notifications, tagging the most recent `Output` or
    /// `StateChanged` entry with `dropped:true` if an eviction happened
    /// since the last drain and hasn't been surfaced yet (FR-17; F-5
    /// extends this to `stateChanged` too, additive per FR-19).
    pub fn drain(&self) -> Vec<QueuedNotification> {
        let mut guard = self.inner.lock();
        let mut out: Vec<QueuedNotification> = guard.items.drain(..).collect();
        if guard.dropped_pending && let Some(idx) = out.iter().rposition(|n| {
            matches!(n, QueuedNotification::Output { .. } | QueuedNotification::StateChanged { .. })
        }) {
            match &mut out[idx] {
                QueuedNotification::Output { dropped, .. } => *dropped = true,
                QueuedNotification::StateChanged { dropped, .. } => *dropped = true,
            }
            guard.dropped_pending = false;
        }
        out
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().items.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().items.is_empty()
    }
}

/// Failure modes for [`Broadcaster::add_subscription`] (R-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddSubscriptionError {
    /// `conn_id` isn't (or is no longer) a registered connection.
    ConnectionNotFound,
    /// The connection already holds `MAX_SUBSCRIPTIONS_PER_CONNECTION`
    /// subscriptions.
    LimitExceeded,
}

struct SubFilter {
    subscription_id: u64,
    events: EventMask,
    pane_ids: Option<HashSet<u64>>,
}

struct ConnEntry {
    queue: Arc<PushQueue>,
    subs: Vec<SubFilter>,
}

/// The server-wide push handle. Cheap to clone; every clone shares the same
/// connection registry.
#[derive(Clone)]
pub struct Broadcaster {
    conns: Arc<Mutex<HashMap<u64, ConnEntry>>>,
    next_conn_id: Arc<AtomicU64>,
    next_sub_id: Arc<AtomicU64>,
    /// Count of live subscriptions (across every connection) whose
    /// `events` include `OUTPUT` (R-3). Maintained on every
    /// `add_subscription`/`remove_subscription`/`unregister_connection`
    /// call — a pane's io thread hot path reads only this atomic
    /// (`has_output_subscribers`) instead of walking `conns`, so "server
    /// running but nobody subscribed to output" costs one relaxed load, the
    /// same as "server not running" used to. `Relaxed` because this is a
    /// hint for skipping work, not something correctness depends on: a
    /// stale `0` just delays a push by one throttle window; a stale nonzero
    /// just costs one wasted diff computation.
    output_subscribers: Arc<AtomicUsize>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Broadcaster {
    pub fn new() -> Self {
        Broadcaster {
            conns: Arc::new(Mutex::new(HashMap::new())),
            next_conn_id: Arc::new(AtomicU64::new(1)),
            next_sub_id: Arc::new(AtomicU64::new(1)),
            output_subscribers: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Registers a new connection, returning its id and the queue its
    /// thread should drain to produce outbound WS frames.
    pub fn register_connection(&self) -> (u64, Arc<PushQueue>) {
        let id = self.next_conn_id.fetch_add(1, Ordering::SeqCst);
        let queue = Arc::new(PushQueue::new(DEFAULT_QUEUE_CAP));
        self.conns.lock().insert(id, ConnEntry { queue: queue.clone(), subs: Vec::new() });
        (id, queue)
    }

    pub fn unregister_connection(&self, conn_id: u64) {
        // R-3: a dropped connection's own output subscriptions go with it —
        // without this, a subscriber that disconnects without explicitly
        // unsubscribing would leave the gate permanently open (or, on a
        // shared count, over-counted forever).
        if let Some(entry) = self.conns.lock().remove(&conn_id) {
            let removed_output_subs = entry.subs.iter().filter(|s| s.events.contains(EventMask::OUTPUT)).count();
            if removed_output_subs > 0 {
                self.output_subscribers.fetch_sub(removed_output_subs, Ordering::Relaxed);
            }
        }
    }

    /// Number of currently-registered connections. Exposed for tests
    /// verifying that a server restart which reuses this `Broadcaster`
    /// leaves no stale connection entries behind once every connection from
    /// the old server has torn down.
    pub fn connection_count(&self) -> usize {
        self.conns.lock().len()
    }

    /// Whether at least one live subscription (on any connection) includes
    /// `EventKind::Output` (R-3). A pane's io thread consults this instead
    /// of whether a tap exists at all: every spawned pane now always
    /// carries a tap (so a server enabled after spawn, or a config-reload
    /// restart, doesn't leave pre-existing panes permanently silent — see
    /// `noa-app`'s `ipc_output_tap`), and this atomic is what actually gates
    /// the per-feed row-diff work down to zero when nobody is listening.
    pub fn has_output_subscribers(&self) -> bool {
        self.output_subscribers.load(Ordering::Relaxed) > 0
    }

    /// Whether at least one live subscription would receive `noa.output`
    /// notifications for `pane_id` specifically (R-3). `has_output_subscribers`
    /// is global — it forces every producing pane's io thread to pay for
    /// span-conversion + row hashing each throttle window even when only one
    /// pane out of many is actually subscribed to. This narrows the gate to
    /// the pane in question: the atomic fast path still short-circuits to
    /// `false` for "nobody subscribed to output at all" without locking; only
    /// when at least one output subscription exists anywhere do we take the
    /// `conns` lock and check whether any of them matches this pane
    /// (`pane_ids: None` subscriptions match every pane). That lock is taken
    /// at most once per pane per throttle window (16ms), not per byte, so
    /// it's cheap relative to the per-byte parsing/hashing work it gates.
    pub fn has_output_subscriber_for(&self, pane_id: u64) -> bool {
        if self.output_subscribers.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let conns = self.conns.lock();
        conns.values().any(|entry| {
            entry.subs.iter().any(|s| {
                s.events.contains(EventMask::OUTPUT)
                    && s.pane_ids.as_ref().is_none_or(|ids| ids.contains(&pane_id))
            })
        })
    }

    /// Registers a subscription filter on `conn_id`.
    ///
    /// Returns `Err(AddSubscriptionError::ConnectionNotFound)` if `conn_id`
    /// isn't registered, or `Err(AddSubscriptionError::LimitExceeded)` if
    /// the connection already holds `MAX_SUBSCRIPTIONS_PER_CONNECTION`
    /// subscriptions (R-2) — the connection itself stays open either way.
    pub fn add_subscription(
        &self,
        conn_id: u64,
        events: EventMask,
        pane_ids: Option<HashSet<u64>>,
    ) -> Result<u64, AddSubscriptionError> {
        let mut conns = self.conns.lock();
        let entry = conns.get_mut(&conn_id).ok_or(AddSubscriptionError::ConnectionNotFound)?;
        if entry.subs.len() >= MAX_SUBSCRIPTIONS_PER_CONNECTION {
            return Err(AddSubscriptionError::LimitExceeded);
        }
        let sub_id = self.next_sub_id.fetch_add(1, Ordering::SeqCst);
        entry.subs.push(SubFilter { subscription_id: sub_id, events, pane_ids });
        drop(conns);
        if events.contains(EventMask::OUTPUT) {
            self.output_subscribers.fetch_add(1, Ordering::Relaxed);
        }
        Ok(sub_id)
    }

    pub fn remove_subscription(&self, conn_id: u64, subscription_id: u64) {
        let mut conns = self.conns.lock();
        let Some(entry) = conns.get_mut(&conn_id) else { return };
        let had_output =
            entry.subs.iter().any(|s| s.subscription_id == subscription_id && s.events.contains(EventMask::OUTPUT));
        entry.subs.retain(|s| s.subscription_id != subscription_id);
        drop(conns);
        if had_output {
            self.output_subscribers.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Enqueues at most one `noa.stateChanged` per connection per broadcast
    /// (R-5): a connection with two or more overlapping `state_changed`
    /// subscriptions previously got one notification per matching
    /// subscription — duplicates that inflate queue pressure and can each
    /// individually evict older, unrelated entries. The union of panels
    /// matched by any of the connection's subscriptions is computed once
    /// and sent as a single notification, preserving `panels`' input order;
    /// a connection whose union is empty gets nothing, exactly as before.
    pub fn broadcast_state_changed(&self, panels: Vec<Panel>) {
        let conns = self.conns.lock();
        for entry in conns.values() {
            let matched: Vec<Panel> = panels
                .iter()
                .filter(|panel| {
                    entry.subs.iter().any(|sub| {
                        sub.events.contains(EventMask::STATE_CHANGED)
                            && sub.pane_ids.as_ref().is_none_or(|ids| ids.contains(&panel.pane_id.0))
                    })
                })
                .cloned()
                .collect();
            if matched.is_empty() {
                continue;
            }
            entry.queue.push(QueuedNotification::StateChanged { panels: matched, dropped: false });
        }
    }

    /// Enqueues at most one `noa.output` per connection per broadcast (R-5,
    /// same defect and fix as `broadcast_state_changed`: overlapping
    /// `output` subscriptions on one connection that both match `pane_id`
    /// previously each pushed their own copy of the same `lines`).
    pub fn broadcast_output(&self, pane_id: u64, lines: Vec<Row>) {
        let conns = self.conns.lock();
        for entry in conns.values() {
            let matches = entry.subs.iter().any(|s| {
                s.events.contains(EventMask::OUTPUT)
                    && s.pane_ids.as_ref().is_none_or(|ids| ids.contains(&pane_id))
            });
            if matches {
                entry.queue.push(QueuedNotification::Output {
                    pane_id,
                    lines: lines.clone(),
                    dropped: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_never_blocks_and_drops_oldest() {
        let q = PushQueue::new(4);
        for i in 0..10u64 {
            q.push(QueuedNotification::Output { pane_id: 1, lines: vec![], dropped: false });
            let _ = i;
        }
        assert_eq!(q.len(), 4);
        let drained = q.drain();
        assert_eq!(drained.len(), 4);
        // the eviction should have tagged the last Output entry as dropped.
        match drained.last().unwrap() {
            QueuedNotification::Output { dropped, .. } => assert!(*dropped),
            _ => panic!("expected output"),
        }
    }

    #[test]
    fn dropped_marker_can_tag_a_state_changed_entry_too() {
        let q = PushQueue::new(4);
        for _ in 0..10 {
            q.push(QueuedNotification::StateChanged { panels: vec![], dropped: false });
        }
        let drained = q.drain();
        match drained.last().unwrap() {
            QueuedNotification::StateChanged { dropped, .. } => assert!(*dropped, "F-5: dropped must also surface on stateChanged"),
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn dropped_marker_tags_the_next_taggable_entry_even_of_a_different_kind() {
        // F-5 made `StateChanged` taggable alongside `Output`, so an
        // eviction can no longer "hide" behind a batch of the other kind —
        // this drains and tags within the very same call now, unlike
        // before F-5 (when only `Output` was taggable and a same-call batch
        // of pure `StateChanged` entries would leave `dropped_pending` set
        // until a later `Output` arrived).
        let q = PushQueue::new(2);
        q.push(QueuedNotification::StateChanged { panels: vec![], dropped: false });
        q.push(QueuedNotification::StateChanged { panels: vec![], dropped: false });
        q.push(QueuedNotification::StateChanged { panels: vec![], dropped: false }); // evicts oldest
        let drained = q.drain();
        match drained.last().unwrap() {
            QueuedNotification::StateChanged { dropped, .. } => assert!(*dropped),
            _ => panic!("expected state changed"),
        }

        // A later, unrelated overflow of a different kind is independently
        // tagged too.
        q.push(QueuedNotification::Output { pane_id: 1, lines: vec![], dropped: false });
        q.push(QueuedNotification::Output { pane_id: 1, lines: vec![], dropped: false });
        q.push(QueuedNotification::Output { pane_id: 1, lines: vec![], dropped: false }); // evicts oldest
        let second = q.drain();
        match second.last().unwrap() {
            QueuedNotification::Output { dropped, .. } => assert!(*dropped),
            _ => panic!("expected output"),
        }
    }

    #[test]
    fn broadcaster_pane_filter() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids = HashSet::new();
        ids.insert(42u64);
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, Some(ids));

        b.broadcast_output(1, vec![]);
        assert_eq!(queue.len(), 0, "non-matching pane must not enqueue");

        b.broadcast_output(42, vec![]);
        assert_eq!(queue.len(), 1);
    }

    fn panel_with_id(pane_id: u64) -> Panel {
        Panel {
            window_group_id: 0.into(),
            window_id: 0.into(),
            pane_id: pane_id.into(),
            name: String::new(),
            cwd: String::new(),
            branch: None,
            process: None,
            busy: false,
            attention: false,
            preview: vec![],
        }
    }

    #[test]
    fn state_changed_pane_filter_delivers_only_matching_panels() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids = HashSet::new();
        ids.insert(42u64);
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids));

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(42)]);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => {
                assert_eq!(panels.len(), 1);
                assert_eq!(panels[0].pane_id.0, 42);
            }
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn state_changed_pane_filter_with_zero_matches_delivers_nothing() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids = HashSet::new();
        ids.insert(99u64);
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids));

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(queue.len(), 0, "no panel matches the filter, so nothing is queued");
    }

    #[test]
    fn state_changed_none_filter_delivers_all_panels() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => assert_eq!(panels.len(), 2),
            _ => panic!("expected state changed"),
        }
    }

    // ---- R-5: overlapping subscriptions on one connection consolidate ----

    #[test]
    fn overlapping_state_changed_subscriptions_on_one_connection_deliver_one_notification_with_the_union() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids_a = HashSet::new();
        ids_a.insert(1u64);
        let mut ids_b = HashSet::new();
        ids_b.insert(1u64);
        ids_b.insert(2u64);
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids_a));
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids_b));

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2), panel_with_id(3)]);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1, "two overlapping subscriptions must not each enqueue their own copy");
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => {
                let ids: Vec<u64> = panels.iter().map(|p| p.pane_id.0).collect();
                assert_eq!(ids, vec![1, 2], "the union of both subscriptions' matches, in input order");
            }
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn disjoint_state_changed_subscriptions_on_one_connection_deliver_one_notification_with_both_panels() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids_a = HashSet::new();
        ids_a.insert(1u64);
        let mut ids_b = HashSet::new();
        ids_b.insert(2u64);
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids_a));
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids_b));

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => {
                let ids: Vec<u64> = panels.iter().map(|p| p.pane_id.0).collect();
                assert_eq!(ids, vec![1, 2]);
            }
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn overlapping_output_subscriptions_on_one_connection_deliver_one_notification() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let mut ids_a = HashSet::new();
        ids_a.insert(42u64);
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, Some(ids_a));
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, None); // matches everything, including 42

        b.broadcast_output(42, vec![]);
        assert_eq!(queue.len(), 1, "two overlapping output subscriptions must not each enqueue their own copy");
    }

    #[test]
    fn unregister_drops_future_broadcasts() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);
        b.unregister_connection(conn_id);
        b.broadcast_state_changed(vec![]);
        assert_eq!(queue.len(), 0);
    }

    // ---- R-3: has_output_subscribers() gate ----

    #[test]
    fn has_output_subscribers_is_false_with_no_subscriptions_at_all() {
        let b = Broadcaster::new();
        assert!(!b.has_output_subscribers());
        let (conn_id, _queue) = b.register_connection();
        assert!(!b.has_output_subscribers(), "a connection with no subscriptions is not an output subscriber");
        // A state_changed-only subscription must not flip the output gate.
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);
        assert!(!b.has_output_subscribers());
    }

    #[test]
    fn has_output_subscribers_flips_true_on_subscribe_and_false_again_on_unsubscribe() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub_id = b.add_subscription(conn_id, EventMask::OUTPUT, None).unwrap();
        assert!(b.has_output_subscribers());

        b.remove_subscription(conn_id, sub_id);
        assert!(!b.has_output_subscribers(), "the only output subscription was removed");
    }

    #[test]
    fn has_output_subscribers_stays_true_while_any_overlapping_subscription_remains() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub_a = b.add_subscription(conn_id, EventMask::OUTPUT, None).unwrap();
        let _sub_b = b.add_subscription(conn_id, EventMask::OUTPUT, None).unwrap();
        assert!(b.has_output_subscribers());

        b.remove_subscription(conn_id, sub_a);
        assert!(b.has_output_subscribers(), "one output subscription is still live");
    }

    #[test]
    fn has_output_subscribers_flips_false_when_the_subscribing_connection_drops() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, None);
        assert!(b.has_output_subscribers());

        // The connection drops without ever calling `noa.unsubscribe` —
        // mirrors a client that just closes its socket.
        b.unregister_connection(conn_id);
        assert!(!b.has_output_subscribers(), "the connection's own output subscriptions must go with it");
    }

    #[test]
    fn has_output_subscribers_reflects_the_union_across_multiple_connections() {
        let b = Broadcaster::new();
        let (conn_a, _queue_a) = b.register_connection();
        let (conn_b, _queue_b) = b.register_connection();
        let _ = b.add_subscription(conn_a, EventMask::OUTPUT, None);
        assert!(b.has_output_subscribers());

        let _ = b.add_subscription(conn_b, EventMask::OUTPUT, None);
        b.unregister_connection(conn_a);
        assert!(b.has_output_subscribers(), "conn_b's output subscription is still live");

        b.unregister_connection(conn_b);
        assert!(!b.has_output_subscribers());
    }

    // ---- R-2: per-connection subscription cap ----

    #[test]
    fn add_subscription_up_to_the_cap_succeeds_the_next_one_fails() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        for i in 0..MAX_SUBSCRIPTIONS_PER_CONNECTION {
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                .unwrap_or_else(|_| panic!("subscription {i} of {MAX_SUBSCRIPTIONS_PER_CONNECTION} must succeed"));
        }
        assert_eq!(
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None),
            Err(AddSubscriptionError::LimitExceeded),
            "the (MAX_SUBSCRIPTIONS_PER_CONNECTION + 1)-th subscription must be rejected"
        );
    }

    #[test]
    fn add_subscription_limit_exceeded_leaves_the_connection_usable() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        for _ in 0..MAX_SUBSCRIPTIONS_PER_CONNECTION {
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None).unwrap();
        }
        assert!(b.add_subscription(conn_id, EventMask::STATE_CHANGED, None).is_err());

        // The connection itself is still registered and its existing
        // subscriptions still deliver — a rejected `noa.subscribe` call must
        // not tear anything down.
        b.broadcast_state_changed(vec![panel_with_id(1)]);
        assert_eq!(queue.len(), 1, "existing subscriptions keep working after a rejected one");
    }

    #[test]
    fn unsubscribe_frees_a_slot_at_the_cap() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let mut sub_ids = Vec::new();
        for _ in 0..MAX_SUBSCRIPTIONS_PER_CONNECTION {
            sub_ids.push(b.add_subscription(conn_id, EventMask::STATE_CHANGED, None).unwrap());
        }
        assert!(b.add_subscription(conn_id, EventMask::STATE_CHANGED, None).is_err());

        b.remove_subscription(conn_id, sub_ids[0]);
        assert!(
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None).is_ok(),
            "unsubscribing must free a slot for a new subscription"
        );
    }

    #[test]
    fn add_subscription_on_unregistered_connection_is_a_distinct_error_from_the_limit() {
        let b = Broadcaster::new();
        assert_eq!(
            b.add_subscription(999, EventMask::STATE_CHANGED, None),
            Err(AddSubscriptionError::ConnectionNotFound)
        );
    }

    // ---- R-3: has_output_subscriber_for(pane_id) narrows the gate per-pane ----

    #[test]
    fn has_output_subscriber_for_is_false_for_every_pane_with_no_subscriptions() {
        let b = Broadcaster::new();
        assert!(!b.has_output_subscriber_for(1));
        let (conn_id, _queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);
        assert!(!b.has_output_subscriber_for(1), "a state_changed-only subscription must not open the output gate");
    }

    #[test]
    fn has_output_subscriber_for_matches_only_the_subscribed_pane() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let mut ids = HashSet::new();
        ids.insert(42u64);
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, Some(ids));

        assert!(b.has_output_subscriber_for(42));
        assert!(!b.has_output_subscriber_for(1), "pane 1 was never subscribed to");
    }

    #[test]
    fn has_output_subscriber_for_matches_every_pane_with_a_none_filter() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, None);

        assert!(b.has_output_subscriber_for(1));
        assert!(b.has_output_subscriber_for(999));
    }
}
