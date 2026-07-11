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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::{EventKind, Panel, Row};

const DEFAULT_QUEUE_CAP: usize = 256;

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
        self.conns.lock().remove(&conn_id);
    }

    pub fn add_subscription(
        &self,
        conn_id: u64,
        events: EventMask,
        pane_ids: Option<HashSet<u64>>,
    ) -> Option<u64> {
        let sub_id = self.next_sub_id.fetch_add(1, Ordering::SeqCst);
        let mut conns = self.conns.lock();
        let entry = conns.get_mut(&conn_id)?;
        entry.subs.push(SubFilter { subscription_id: sub_id, events, pane_ids });
        Some(sub_id)
    }

    pub fn remove_subscription(&self, conn_id: u64, subscription_id: u64) {
        if let Some(entry) = self.conns.lock().get_mut(&conn_id) {
            entry.subs.retain(|s| s.subscription_id != subscription_id);
        }
    }

    pub fn broadcast_state_changed(&self, panels: Vec<Panel>) {
        let conns = self.conns.lock();
        for entry in conns.values() {
            for sub in &entry.subs {
                if !sub.events.contains(EventMask::STATE_CHANGED) {
                    continue;
                }
                let filtered: Vec<Panel> = match &sub.pane_ids {
                    Some(ids) => panels
                        .iter()
                        .filter(|panel| ids.contains(&panel.pane_id.0))
                        .cloned()
                        .collect(),
                    None => panels.clone(),
                };
                if filtered.is_empty() {
                    continue;
                }
                entry.queue.push(QueuedNotification::StateChanged {
                    panels: filtered,
                    dropped: false,
                });
            }
        }
    }

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
        b.add_subscription(conn_id, EventMask::OUTPUT, Some(ids));

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
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids));

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
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, Some(ids));

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        assert_eq!(queue.len(), 0, "no panel matches the filter, so nothing is queued");
    }

    #[test]
    fn state_changed_none_filter_delivers_all_panels() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);

        b.broadcast_state_changed(vec![panel_with_id(1), panel_with_id(2)]);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            QueuedNotification::StateChanged { panels, .. } => assert_eq!(panels.len(), 2),
            _ => panic!("expected state changed"),
        }
    }

    #[test]
    fn unregister_drops_future_broadcasts() {
        let b = Broadcaster::new();
        let (conn_id, queue) = b.register_connection();
        b.add_subscription(conn_id, EventMask::STATE_CHANGED, None);
        b.unregister_connection(conn_id);
        b.broadcast_state_changed(vec![]);
        assert_eq!(queue.len(), 0);
    }
}
