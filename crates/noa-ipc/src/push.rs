//! Subscriptions and server -> client push (spec FR-15..17, NFR-2).
//!
//! Terminal-side callers (io_thread / main thread) only ever call
//! [`Broadcaster::broadcast_state_delta`] (or its wrapper
//! `broadcast_state_changed`) / `broadcast_output`, which
//! push into a bounded per-connection queue and never block. Each
//! connection's own thread drains its queue and writes WS frames — slow or
//! stalled clients only ever affect their own queue.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::attach::AttachRegistry;
use crate::protocol::{EventKind, FieldMask, Panel, Row};

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
    StateChanged {
        panels: Vec<Panel>,
        removed_pane_ids: Vec<u64>,
        dropped: bool,
    },
    Output {
        pane_id: u64,
        coordinate_generation: u64,
        lines: Vec<Row>,
        dropped: bool,
    },
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
            inner: Mutex::new(QueueInner {
                items: VecDeque::with_capacity(cap),
                dropped_pending: false,
            }),
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
        if guard.dropped_pending
            && let Some(idx) = out.iter().rposition(|n| {
                matches!(
                    n,
                    QueuedNotification::Output { .. } | QueuedNotification::StateChanged { .. }
                )
            })
        {
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

/// Failure modes for [`Broadcaster::update_subscription`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateSubscriptionError {
    /// `conn_id` isn't (or is no longer) a registered connection.
    ConnectionNotFound,
    /// The connection holds no subscription with that id. An id owned by
    /// another connection lands here too, by construction.
    SubscriptionNotFound,
}

/// An in-place change to one subscription. `None` keeps the current value;
/// for `pane_ids` / `fields`, `Some(None)` clears the filter.
#[derive(Clone, Debug, Default)]
pub struct SubscriptionUpdate {
    pub events: Option<EventMask>,
    pub pane_ids: Option<Option<HashSet<u64>>>,
    pub fields: Option<Option<FieldMask>>,
}

/// One panel of a `noa.stateChanged` delta, with the fields that differ
/// from the previous listing. An added panel carries [`FieldMask::ALL`].
#[derive(Clone, Debug)]
pub struct ChangedPanel {
    pub panel: Panel,
    pub fields: FieldMask,
}

struct SubFilter {
    subscription_id: u64,
    events: EventMask,
    pane_ids: Option<HashSet<u64>>,
    /// `state_changed` only: `Some` delivers a changed panel only when one
    /// of these fields changed. Added panels and removals always pass.
    fields: Option<FieldMask>,
}

impl SubFilter {
    fn covers_pane(&self, pane_id: u64) -> bool {
        self.pane_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&pane_id))
    }

    fn takes_state_changed_for(&self, pane_id: u64) -> bool {
        self.events.contains(EventMask::STATE_CHANGED) && self.covers_pane(pane_id)
    }

    fn takes_changed_panel(&self, changed: &ChangedPanel) -> bool {
        self.takes_state_changed_for(changed.panel.pane_id.0)
            && self.fields.is_none_or(|fields| {
                changed.fields == FieldMask::ALL || fields.intersects(changed.fields)
            })
    }
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
    /// Attach ownership belongs to the long-lived IPC runtime, not to one
    /// listener instance. Config reloads reuse the broadcaster while old
    /// connection threads wind down, so every server generation must consult
    /// the same lease registry.
    attach_registry: AttachRegistry,
    /// Count of live subscriptions (across every connection) whose
    /// `events` include `OUTPUT` (R-3). Maintained on every
    /// `add_subscription`/`update_subscription`/`remove_subscription`/
    /// `unregister_connection` call, always while `conns` is held: every
    /// change is then serialized with the registry edit it reflects, so a
    /// load sees a count that was true at some point (two racing updates
    /// can't pass through a spurious `0`). A pane's io thread hot path reads only this atomic
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
            attach_registry: AttachRegistry::default(),
            output_subscribers: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn attach_registry(&self) -> AttachRegistry {
        self.attach_registry.clone()
    }

    /// Registers a new connection, returning its id and the queue its
    /// thread should drain to produce outbound WS frames.
    pub fn register_connection(&self) -> (u64, Arc<PushQueue>) {
        let id = self.next_conn_id.fetch_add(1, Ordering::SeqCst);
        let queue = Arc::new(PushQueue::new(DEFAULT_QUEUE_CAP));
        self.conns.lock().insert(
            id,
            ConnEntry {
                queue: queue.clone(),
                subs: Vec::new(),
            },
        );
        (id, queue)
    }

    pub fn unregister_connection(&self, conn_id: u64) {
        // R-3: a dropped connection's own output subscriptions go with it —
        // without this, a subscriber that disconnects without explicitly
        // unsubscribing would leave the gate permanently open (or, on a
        // shared count, over-counted forever).
        let mut conns = self.conns.lock();
        if let Some(entry) = conns.remove(&conn_id) {
            let removed_output_subs = entry
                .subs
                .iter()
                .filter(|s| s.events.contains(EventMask::OUTPUT))
                .count();
            if removed_output_subs > 0 {
                self.output_subscribers
                    .fetch_sub(removed_output_subs, Ordering::Relaxed);
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

    /// Whether any live subscription takes `noa.stateChanged`, i.e. whether
    /// a [`Self::broadcast_state_changed`] now could reach anyone. Taken
    /// under the registry lock, so a subscription acknowledged before this
    /// call is seen.
    pub fn has_state_changed_subscribers(&self) -> bool {
        self.conns.lock().values().any(|entry| {
            entry
                .subs
                .iter()
                .any(|sub| sub.events.contains(EventMask::STATE_CHANGED))
        })
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
        self.add_subscription_filtered(conn_id, events, pane_ids, None)
    }

    /// [`Self::add_subscription`] with a `state_changed` field filter.
    pub fn add_subscription_filtered(
        &self,
        conn_id: u64,
        events: EventMask,
        pane_ids: Option<HashSet<u64>>,
        fields: Option<FieldMask>,
    ) -> Result<u64, AddSubscriptionError> {
        let mut conns = self.conns.lock();
        let entry = conns
            .get_mut(&conn_id)
            .ok_or(AddSubscriptionError::ConnectionNotFound)?;
        if entry.subs.len() >= MAX_SUBSCRIPTIONS_PER_CONNECTION {
            return Err(AddSubscriptionError::LimitExceeded);
        }
        let sub_id = self.next_sub_id.fetch_add(1, Ordering::SeqCst);
        entry.subs.push(SubFilter {
            subscription_id: sub_id,
            events,
            pane_ids,
            fields,
        });
        // Adjusted while `conns` is held, so the count only ever moves
        // between values that match the registry (see `output_subscribers`).
        if events.contains(EventMask::OUTPUT) {
            self.output_subscribers.fetch_add(1, Ordering::Relaxed);
        }
        Ok(sub_id)
    }

    pub fn remove_subscription(&self, conn_id: u64, subscription_id: u64) {
        let mut conns = self.conns.lock();
        let Some(entry) = conns.get_mut(&conn_id) else {
            return;
        };
        let had_output = entry
            .subs
            .iter()
            .any(|s| s.subscription_id == subscription_id && s.events.contains(EventMask::OUTPUT));
        entry.subs.retain(|s| s.subscription_id != subscription_id);
        if had_output {
            self.output_subscribers.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Applies `update` to subscription `subscription_id` of `conn_id` in
    /// place, keeping its id and position. Everything happens under one
    /// `conns` acquisition, so a broadcast sees the old filter or the new one,
    /// never a mix, and the `output_subscribers` adjustment cannot interleave
    /// with another update's. No push queue is touched (lock order `conns` →
    /// queue).
    pub fn update_subscription(
        &self,
        conn_id: u64,
        subscription_id: u64,
        update: SubscriptionUpdate,
    ) -> Result<(), UpdateSubscriptionError> {
        let mut conns = self.conns.lock();
        let entry = conns
            .get_mut(&conn_id)
            .ok_or(UpdateSubscriptionError::ConnectionNotFound)?;
        let sub = entry
            .subs
            .iter_mut()
            .find(|s| s.subscription_id == subscription_id)
            .ok_or(UpdateSubscriptionError::SubscriptionNotFound)?;
        let had_output = sub.events.contains(EventMask::OUTPUT);
        if let Some(events) = update.events {
            sub.events = events;
        }
        if let Some(pane_ids) = update.pane_ids {
            sub.pane_ids = pane_ids;
        }
        if let Some(fields) = update.fields {
            sub.fields = fields;
        }
        let has_output = sub.events.contains(EventMask::OUTPUT);
        match (had_output, has_output) {
            (false, true) => {
                self.output_subscribers.fetch_add(1, Ordering::Relaxed);
            }
            (true, false) => {
                self.output_subscribers.fetch_sub(1, Ordering::Relaxed);
            }
            _ => {}
        }
        Ok(())
    }

    /// [`Self::broadcast_state_delta`] with every panel treated as fully
    /// changed and no removals.
    pub fn broadcast_state_changed(&self, panels: Vec<Panel>) {
        let changed = panels
            .into_iter()
            .map(|panel| ChangedPanel {
                panel,
                fields: FieldMask::ALL,
            })
            .collect();
        self.broadcast_state_delta(changed, Vec::new());
    }

    /// Enqueues at most one `noa.stateChanged` per connection per broadcast
    /// (R-5): a connection with two or more overlapping `state_changed`
    /// subscriptions previously got one notification per matching
    /// subscription — duplicates that inflate queue pressure and can each
    /// individually evict older, unrelated entries. The union of panels and
    /// removed pane ids matched by any of the connection's subscriptions is
    /// computed once and sent as a single notification, preserving input
    /// order; a connection whose union is empty gets nothing.
    ///
    /// A changed panel matches a subscription by scope and, when the
    /// subscription has `fields`, by an intersecting field mask (an added
    /// panel, [`FieldMask::ALL`], always passes). A removed id matches by
    /// scope alone. `removed` is deduplicated here, keeping each id's first
    /// position (AC-1), so a caller that repeats an id still puts it on the
    /// wire once.
    pub fn broadcast_state_delta(&self, changed: Vec<ChangedPanel>, mut removed: Vec<u64>) {
        let mut seen = HashSet::with_capacity(removed.len());
        removed.retain(|&id| seen.insert(id));
        let conns = self.conns.lock();
        for entry in conns.values() {
            let panels: Vec<Panel> = changed
                .iter()
                .filter(|c| entry.subs.iter().any(|sub| sub.takes_changed_panel(c)))
                .map(|c| c.panel.clone())
                .collect();
            let removed_pane_ids: Vec<u64> = removed
                .iter()
                .copied()
                .filter(|&id| entry.subs.iter().any(|sub| sub.takes_state_changed_for(id)))
                .collect();
            if panels.is_empty() && removed_pane_ids.is_empty() {
                continue;
            }
            entry.queue.push(QueuedNotification::StateChanged {
                panels,
                removed_pane_ids,
                dropped: false,
            });
        }
    }

    /// Enqueues at most one `noa.output` per connection per broadcast (R-5,
    /// same defect and fix as `broadcast_state_changed`: overlapping
    /// `output` subscriptions on one connection that both match `pane_id`
    /// previously each pushed their own copy of the same `lines`).
    pub fn broadcast_output(&self, pane_id: u64, coordinate_generation: u64, lines: Vec<Row>) {
        let conns = self.conns.lock();
        for entry in conns.values() {
            let matches = entry.subs.iter().any(|s| {
                s.events.contains(EventMask::OUTPUT)
                    && s.pane_ids.as_ref().is_none_or(|ids| ids.contains(&pane_id))
            });
            if matches {
                entry.queue.push(QueuedNotification::Output {
                    pane_id,
                    coordinate_generation,
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
    use crate::protocol::PanelField;

    #[test]
    fn push_never_blocks_and_drops_oldest() {
        let q = PushQueue::new(4);
        for i in 0..10u64 {
            q.push(QueuedNotification::Output {
                pane_id: 1,
                coordinate_generation: 0,
                lines: vec![],
                dropped: false,
            });
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
            q.push(QueuedNotification::StateChanged {
                panels: vec![],
                removed_pane_ids: vec![],
                dropped: false,
            });
        }
        let drained = q.drain();
        match drained.last().unwrap() {
            QueuedNotification::StateChanged { dropped, .. } => {
                assert!(*dropped, "F-5: dropped must also surface on stateChanged")
            }
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
        q.push(QueuedNotification::StateChanged {
            panels: vec![],
            removed_pane_ids: vec![],
            dropped: false,
        });
        q.push(QueuedNotification::StateChanged {
            panels: vec![],
            removed_pane_ids: vec![],
            dropped: false,
        });
        q.push(QueuedNotification::StateChanged {
            panels: vec![],
            removed_pane_ids: vec![],
            dropped: false,
        }); // evicts oldest
        let drained = q.drain();
        match drained.last().unwrap() {
            QueuedNotification::StateChanged { dropped, .. } => assert!(*dropped),
            _ => panic!("expected state changed"),
        }

        // A later, unrelated overflow of a different kind is independently
        // tagged too.
        q.push(QueuedNotification::Output {
            pane_id: 1,
            coordinate_generation: 0,
            lines: vec![],
            dropped: false,
        });
        q.push(QueuedNotification::Output {
            pane_id: 1,
            coordinate_generation: 0,
            lines: vec![],
            dropped: false,
        });
        q.push(QueuedNotification::Output {
            pane_id: 1,
            coordinate_generation: 0,
            lines: vec![],
            dropped: false,
        }); // evicts oldest
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

        b.broadcast_output(1, 0, vec![]);
        assert_eq!(queue.len(), 0, "non-matching pane must not enqueue");

        b.broadcast_output(42, 0, vec![]);
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
            attachable: true,
            preview: vec![],
            surface_id: None,
            agent: None,
            exit: None,
            parent_pane_id: None,
            label: None,
            spawn: None,
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
        assert_eq!(
            queue.len(),
            0,
            "no panel matches the filter, so nothing is queued"
        );
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
    fn overlapping_state_changed_subscriptions_on_one_connection_deliver_one_notification_with_the_union()
     {
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
        assert_eq!(
            drained.len(),
            1,
            "two overlapping subscriptions must not each enqueue their own copy"
        );
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => {
                let ids: Vec<u64> = panels.iter().map(|p| p.pane_id.0).collect();
                assert_eq!(
                    ids,
                    vec![1, 2],
                    "the union of both subscriptions' matches, in input order"
                );
            }
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn disjoint_state_changed_subscriptions_on_one_connection_deliver_one_notification_with_both_panels()
     {
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

        b.broadcast_output(42, 0, vec![]);
        assert_eq!(
            queue.len(),
            1,
            "two overlapping output subscriptions must not each enqueue their own copy"
        );
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
    fn state_changed_subscribers_are_seen_on_any_connection_until_they_go() {
        let b = Broadcaster::new();
        assert!(!b.has_state_changed_subscribers());
        let (output_only, _q1) = b.register_connection();
        b.add_subscription(output_only, EventMask::OUTPUT, None)
            .unwrap();
        assert!(!b.has_state_changed_subscribers());
        let (waiter, _q2) = b.register_connection();
        let sub = b
            .add_subscription(waiter, EventMask::STATE_CHANGED, Some(HashSet::from([3])))
            .unwrap();
        assert!(b.has_state_changed_subscribers());
        b.remove_subscription(waiter, sub);
        assert!(!b.has_state_changed_subscribers());
        let both = EventMask::from_events(&[EventKind::StateChanged, EventKind::Output]);
        b.add_subscription(waiter, both, None).unwrap();
        assert!(b.has_state_changed_subscribers());
        b.unregister_connection(waiter);
        assert!(!b.has_state_changed_subscribers());
    }

    #[test]
    fn has_output_subscribers_is_false_with_no_subscriptions_at_all() {
        let b = Broadcaster::new();
        assert!(!b.has_output_subscribers());
        let (conn_id, _queue) = b.register_connection();
        assert!(
            !b.has_output_subscribers(),
            "a connection with no subscriptions is not an output subscriber"
        );
        // A state_changed-only subscription must not flip the output gate.
        let _ = b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);
        assert!(!b.has_output_subscribers());
    }

    #[test]
    fn has_output_subscribers_flips_true_on_subscribe_and_false_again_on_unsubscribe() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub_id = b
            .add_subscription(conn_id, EventMask::OUTPUT, None)
            .unwrap();
        assert!(b.has_output_subscribers());

        b.remove_subscription(conn_id, sub_id);
        assert!(
            !b.has_output_subscribers(),
            "the only output subscription was removed"
        );
    }

    #[test]
    fn has_output_subscribers_stays_true_while_any_overlapping_subscription_remains() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub_a = b
            .add_subscription(conn_id, EventMask::OUTPUT, None)
            .unwrap();
        let _sub_b = b
            .add_subscription(conn_id, EventMask::OUTPUT, None)
            .unwrap();
        assert!(b.has_output_subscribers());

        b.remove_subscription(conn_id, sub_a);
        assert!(
            b.has_output_subscribers(),
            "one output subscription is still live"
        );
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
        assert!(
            !b.has_output_subscribers(),
            "the connection's own output subscriptions must go with it"
        );
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
        assert!(
            b.has_output_subscribers(),
            "conn_b's output subscription is still live"
        );

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
                .unwrap_or_else(|_| {
                    panic!("subscription {i} of {MAX_SUBSCRIPTIONS_PER_CONNECTION} must succeed")
                });
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
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                .unwrap();
        }
        assert!(
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                .is_err()
        );

        // The connection itself is still registered and its existing
        // subscriptions still deliver — a rejected `noa.subscribe` call must
        // not tear anything down.
        b.broadcast_state_changed(vec![panel_with_id(1)]);
        assert_eq!(
            queue.len(),
            1,
            "existing subscriptions keep working after a rejected one"
        );
    }

    #[test]
    fn unsubscribe_frees_a_slot_at_the_cap() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let mut sub_ids = Vec::new();
        for _ in 0..MAX_SUBSCRIPTIONS_PER_CONNECTION {
            sub_ids.push(
                b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                    .unwrap(),
            );
        }
        assert!(
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                .is_err()
        );

        b.remove_subscription(conn_id, sub_ids[0]);
        assert!(
            b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
                .is_ok(),
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
        assert!(
            !b.has_output_subscriber_for(1),
            "a state_changed-only subscription must not open the output gate"
        );
    }

    #[test]
    fn has_output_subscriber_for_matches_only_the_subscribed_pane() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let mut ids = HashSet::new();
        ids.insert(42u64);
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, Some(ids));

        assert!(b.has_output_subscriber_for(42));
        assert!(
            !b.has_output_subscriber_for(1),
            "pane 1 was never subscribed to"
        );
    }

    #[test]
    fn has_output_subscriber_for_matches_every_pane_with_a_none_filter() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let _ = b.add_subscription(conn_id, EventMask::OUTPUT, None);

        assert!(b.has_output_subscriber_for(1));
        assert!(b.has_output_subscriber_for(999));
    }

    // ---- AC-1: removal notices ----

    fn changed(pane_id: u64, fields: FieldMask) -> ChangedPanel {
        ChangedPanel {
            panel: panel_with_id(pane_id),
            fields,
        }
    }

    /// Drains exactly one `stateChanged` and returns its panel ids and
    /// removed ids.
    fn drain_one_state_changed(queue: &PushQueue) -> (Vec<u64>, Vec<u64>) {
        let drained = queue.drain();
        assert_eq!(drained.len(), 1, "expected exactly one notification");
        match &drained[0] {
            QueuedNotification::StateChanged {
                panels,
                removed_pane_ids,
                ..
            } => (
                panels.iter().map(|p| p.pane_id.0).collect(),
                removed_pane_ids.clone(),
            ),
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn a_removal_in_scope_is_delivered_and_one_out_of_scope_is_not() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(HashSet::from([13])))
            .unwrap();

        b.broadcast_state_delta(vec![], vec![14]);
        assert!(queue.is_empty(), "pane 14 is out of scope");

        b.broadcast_state_delta(vec![], vec![13, 14]);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![13]));
    }

    #[test]
    fn a_removal_only_delta_enqueues_exactly_one_notification_with_empty_panels() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();

        b.broadcast_state_delta(vec![], vec![5]);
        assert_eq!(queue.len(), 1);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![5]));
    }

    #[test]
    fn a_subscription_without_pane_ids_receives_every_removal() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();

        b.broadcast_state_delta(vec![], vec![3, 1, 2]);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![3, 1, 2]));
    }

    #[test]
    fn repeated_removed_ids_are_delivered_once_in_first_occurrence_order() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();

        b.broadcast_state_delta(vec![], vec![13, 14, 13]);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![13, 14]));

        b.broadcast_state_delta(vec![], vec![2, 1, 2, 1, 3, 2]);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![2, 1, 3]));
    }

    #[test]
    fn two_subscriptions_on_one_connection_give_one_notification_with_the_union_of_removals() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(
            conn_id,
            EventMask::STATE_CHANGED,
            Some(HashSet::from([1, 3])),
        )
        .unwrap();
        b.add_subscription(
            conn_id,
            EventMask::STATE_CHANGED,
            Some(HashSet::from([1, 2])),
        )
        .unwrap();

        // Neither subscription alone covers [3, 2, 1]: the first gives [3, 1]
        // and the second [2, 1]. Pane 4 is in neither.
        b.broadcast_state_delta(vec![changed(1, FieldMask::ALL)], vec![3, 2, 1, 4]);
        assert_eq!(drain_one_state_changed(&queue), (vec![1], vec![3, 2, 1]));
    }

    #[test]
    fn an_output_only_subscription_receives_no_removal() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::OUTPUT, None)
            .unwrap();

        b.broadcast_state_delta(vec![], vec![1]);
        assert!(queue.is_empty());
    }

    // ---- AC-2: field filters ----

    #[test]
    fn a_field_filter_delivers_only_panels_whose_named_fields_changed_or_that_were_added() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let agent = FieldMask::from_fields(&[PanelField::Agent]);
        b.add_subscription_filtered(conn_id, EventMask::STATE_CHANGED, None, Some(agent))
            .unwrap();

        b.broadcast_state_delta(
            vec![changed(1, FieldMask::from_fields(&[PanelField::Preview]))],
            vec![],
        );
        assert!(
            queue.is_empty(),
            "a preview-only change must not reach an agent filter"
        );

        b.broadcast_state_delta(
            vec![
                changed(1, FieldMask::from_fields(&[PanelField::Preview])),
                changed(
                    2,
                    FieldMask::from_fields(&[PanelField::Agent, PanelField::Cwd]),
                ),
                changed(3, FieldMask::ALL),
            ],
            vec![],
        );
        assert_eq!(drain_one_state_changed(&queue), (vec![2, 3], vec![]));
    }

    #[test]
    fn an_empty_field_filter_still_receives_additions_and_removals() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription_filtered(
            conn_id,
            EventMask::STATE_CHANGED,
            None,
            Some(FieldMask::EMPTY),
        )
        .unwrap();

        b.broadcast_state_delta(
            vec![changed(1, FieldMask::from_fields(&[PanelField::Agent]))],
            vec![],
        );
        assert!(queue.is_empty(), "fields: [] takes no field change");

        b.broadcast_state_delta(vec![], vec![9]);
        assert_eq!(drain_one_state_changed(&queue), (vec![], vec![9]));

        b.broadcast_state_delta(vec![changed(4, FieldMask::ALL)], vec![]);
        assert_eq!(drain_one_state_changed(&queue), (vec![4], vec![]));
    }

    #[test]
    fn a_filtered_and_an_unfiltered_subscription_on_one_connection_deliver_the_panel() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let agent = FieldMask::from_fields(&[PanelField::Agent]);
        b.add_subscription_filtered(conn_id, EventMask::STATE_CHANGED, None, Some(agent))
            .unwrap();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();

        b.broadcast_state_delta(
            vec![changed(1, FieldMask::from_fields(&[PanelField::Preview]))],
            vec![],
        );
        assert_eq!(drain_one_state_changed(&queue), (vec![1], vec![]));
    }

    #[test]
    fn fields_on_an_output_subscription_do_not_change_broadcast_output() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription_filtered(
            conn_id,
            EventMask::OUTPUT,
            Some(HashSet::from([42])),
            Some(FieldMask::EMPTY),
        )
        .unwrap();

        b.broadcast_output(1, 0, vec![]);
        assert!(queue.is_empty());
        b.broadcast_output(42, 0, vec![]);
        assert_eq!(queue.len(), 1);
    }

    // ---- AC-3: update_subscription ----

    #[test]
    fn an_update_replaces_pane_ids_in_place() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let sub = b
            .add_subscription(conn_id, EventMask::STATE_CHANGED, Some(HashSet::from([1])))
            .unwrap();

        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                pane_ids: Some(Some(HashSet::from([2]))),
                ..Default::default()
            },
        )
        .unwrap();

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(drain_one_state_changed(&queue), (vec![2], vec![]));

        // The same id is still the one to unsubscribe.
        b.remove_subscription(conn_id, sub);
        b.broadcast_state_changed(vec![panel_with_id(2)]);
        assert!(queue.is_empty());
    }

    #[test]
    fn an_update_with_some_none_clears_the_pane_filter() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let sub = b
            .add_subscription(conn_id, EventMask::STATE_CHANGED, Some(HashSet::from([1])))
            .unwrap();

        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                pane_ids: Some(None),
                ..Default::default()
            },
        )
        .unwrap();

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(drain_one_state_changed(&queue), (vec![1, 2], vec![]));
    }

    #[test]
    fn an_absent_key_keeps_its_value() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        let agent = FieldMask::from_fields(&[PanelField::Agent]);
        let sub = b
            .add_subscription_filtered(
                conn_id,
                EventMask::STATE_CHANGED,
                Some(HashSet::from([1])),
                Some(agent),
            )
            .unwrap();

        // Only `fields` changes; `events` and `pane_ids` stay.
        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                fields: Some(None),
                ..Default::default()
            },
        )
        .unwrap();

        b.broadcast_state_delta(
            vec![
                changed(1, FieldMask::from_fields(&[PanelField::Preview])),
                changed(2, FieldMask::from_fields(&[PanelField::Preview])),
            ],
            vec![],
        );
        assert_eq!(drain_one_state_changed(&queue), (vec![1], vec![]));

        b.update_subscription(conn_id, sub, SubscriptionUpdate::default())
            .unwrap();
        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(drain_one_state_changed(&queue), (vec![1], vec![]));
    }

    #[test]
    fn an_id_from_another_connection_or_an_unknown_id_is_subscription_not_found() {
        let b = Broadcaster::new();
        let (owner, owner_queue) = b.register_connection();
        let (other, _other_queue) = b.register_connection();
        let sub = b
            .add_subscription(owner, EventMask::STATE_CHANGED, Some(HashSet::from([1])))
            .unwrap();

        let widen = SubscriptionUpdate {
            pane_ids: Some(None),
            ..Default::default()
        };
        assert_eq!(
            b.update_subscription(other, sub, widen.clone()),
            Err(UpdateSubscriptionError::SubscriptionNotFound)
        );
        assert_eq!(
            b.update_subscription(owner, sub + 1000, widen.clone()),
            Err(UpdateSubscriptionError::SubscriptionNotFound)
        );
        assert_eq!(
            b.update_subscription(999, sub, widen),
            Err(UpdateSubscriptionError::ConnectionNotFound)
        );

        // The rejected updates left the owner's filter alone.
        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(drain_one_state_changed(&owner_queue), (vec![1], vec![]));
    }

    #[test]
    fn changing_events_flips_both_gates_and_back() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub = b
            .add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();
        assert!(b.has_state_changed_subscribers());
        assert!(!b.has_output_subscribers());

        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                events: Some(EventMask::OUTPUT),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!b.has_state_changed_subscribers());
        assert!(b.has_output_subscribers());

        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                events: Some(EventMask::STATE_CHANGED),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(b.has_state_changed_subscribers());
        assert!(!b.has_output_subscribers());
    }

    #[test]
    fn racing_updates_never_hide_another_connections_output_subscription() {
        // Connection A keeps an output subscription for pane 1 throughout;
        // two threads keep toggling connection B's subscription in and out
        // of OUTPUT. The counter must never pass through 0 while A holds its
        // subscription (it did when the adjustment ran after `conns` was
        // released: a `-1` could land before the racing `+1`).
        const ROUNDS: usize = 20_000;
        let b = Broadcaster::new();
        let (a, _a_queue) = b.register_connection();
        let sub_a = b
            .add_subscription(a, EventMask::OUTPUT, Some(HashSet::from([1])))
            .unwrap();
        let (conn_b, _b_queue) = b.register_connection();
        let sub_b = b
            .add_subscription(conn_b, EventMask::STATE_CHANGED, None)
            .unwrap();
        let toggle = |first: EventMask, second: EventMask| {
            for _ in 0..ROUNDS {
                for events in [first, second] {
                    b.update_subscription(
                        conn_b,
                        sub_b,
                        SubscriptionUpdate {
                            events: Some(events),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                }
            }
        };
        std::thread::scope(|scope| {
            let workers = [
                scope.spawn(|| toggle(EventMask::OUTPUT, EventMask::STATE_CHANGED)),
                scope.spawn(|| toggle(EventMask::STATE_CHANGED, EventMask::OUTPUT)),
            ];
            // Watch until both workers have ended, by finishing or by
            // panicking, then re-raise a worker's panic so the test fails
            // with it instead of waiting forever.
            while !workers.iter().all(|worker| worker.is_finished()) {
                assert!(
                    b.has_output_subscriber_for(1),
                    "A's output subscription vanished mid-race"
                );
            }
            for worker in workers {
                if let Err(panic) = worker.join() {
                    std::panic::resume_unwind(panic);
                }
            }
        });
        b.update_subscription(
            conn_b,
            sub_b,
            SubscriptionUpdate {
                events: Some(EventMask::STATE_CHANGED),
                ..Default::default()
            },
        )
        .unwrap();
        b.remove_subscription(a, sub_a);
        assert!(
            !b.has_output_subscribers(),
            "the output counter must not drift"
        );
    }

    #[test]
    fn unregistering_after_an_update_to_output_leaves_no_output_subscriber() {
        let b = Broadcaster::new();
        let (conn_id, _queue) = b.register_connection();
        let sub = b
            .add_subscription(conn_id, EventMask::STATE_CHANGED, None)
            .unwrap();
        b.update_subscription(
            conn_id,
            sub,
            SubscriptionUpdate {
                events: Some(EventMask::OUTPUT),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(b.has_output_subscribers());

        b.unregister_connection(conn_id);
        assert!(
            !b.has_output_subscribers(),
            "the output counter must not drift"
        );
    }
}
