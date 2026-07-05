//! Central session registry — the single source of truth for the session
//! sidebar (spec `docs/specs/session-sidebar.md`, ADR 0001). Ghostty has no
//! analog; this is a noa addition.
//!
//! The store is a channel-delta model: the io thread posts [`SessionDelta`]s
//! (via `UserEvent`) and the main thread owns and [`apply`](SessionStore::apply)s
//! them — there is no cross-thread lock (FR-1). Every field here is pure data
//! and pure logic: this module is GUI-agnostic and must not import `winit` or
//! `wgpu` (NFR-6, enforced by the source-scan test below), so the sidebar's
//! state model stays unit-testable without a window or GPU.
//!
//! PR1 wires the module into the crate but nothing consumes it yet; the
//! `dead_code` allow is temporary and removed when the io thread and app
//! integrate the store (PR3).
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};

use crate::split_tree::PaneId;

/// FIFO cap on the tombstone map. Pane ids are monotonic and never reused, so a
/// tombstone is only ever consulted to reject stale/reordered `Upsert`s already
/// in flight for a just-removed id; once that id's queue has drained the entry
/// is inert. Retiring [`TOMBSTONE_CAP`] newer ids is far more than enough to
/// outlast any in-flight message, so evicting the oldest entry is safe and
/// keeps the map bounded rather than growing for the process lifetime.
const TOMBSTONE_CAP: usize = 64;

/// GUI-agnostic window identity. The app boundary (PR3) converts the winit
/// `WindowId` into this newtype so the store never sees a windowing type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionWindowId(pub u64);

/// Key for a session card: the window/pane it belongs to. Mirrors the shape of
/// `app::OverviewTileId` but is deliberately GUI-agnostic — [`SessionWindowId`]
/// stands in for the winit `WindowId`, and [`PaneId`] is already crate-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionCardId {
    pub window_id: SessionWindowId,
    pub pane_id: PaneId,
}

impl SessionCardId {
    pub const fn new(window_id: SessionWindowId, pane_id: PaneId) -> Self {
        Self { window_id, pane_id }
    }
}

/// Project icon inferred from the session's cwd (FR-9). Detection (the marker
/// first-match table) lands with the branch-poll thread in PR4; PR1 only
/// defines the type. Defaults to [`IconKind::Folder`] for an unclassified cwd.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IconKind {
    Rust,
    Node,
    Terraform,
    Go,
    Python,
    Git,
    #[default]
    Folder,
}

/// A civil (local) wall-clock timestamp, broken into calendar fields. The io
/// thread stamps this at last output (PR3); [`format_relative_time`] renders it
/// relative to a caller-supplied `now`. Kept as plain fields so the module
/// needs no date/time crate and stays pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallClock {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// The status dot color for a card (FR-11). Semantics: `Blue` = busy (a
/// program is running), `Green` = idle, `Yellow` = an unread bell is pending.
/// Precedence is bell > busy > idle (see [`status_dot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusDot {
    Blue,
    Green,
    Yellow,
}

/// One session's card state. `name` is the title reported by the shell (OSC
/// 0/2); `name_override` is a user rename (FR-7) that shadows it — see
/// [`SessionCard::display_name`]. `seq` is the per-card generation carried by
/// the last applied [`SessionDelta::Upsert`], used to drop stale/out-of-order
/// upserts (see [`SessionStore::apply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCard {
    pub name: String,
    pub name_override: Option<String>,
    pub cwd: String,
    pub branch: Option<String>,
    pub icon: IconKind,
    pub unread_bell: bool,
    pub busy: bool,
    pub updated_at: WallClock,
    /// Last-output preview lines (up to 2; FR-2). Placeholder in PR1; the io
    /// thread fills it from the overview snapshot in a later PR.
    pub preview: Vec<String>,
    seq: u64,
}

impl SessionCard {
    /// The name to display: the user rename if present, else the shell title.
    pub fn display_name(&self) -> &str {
        self.name_override.as_deref().unwrap_or(&self.name)
    }
}

/// A single mutation to the [`SessionStore`]. This enum is **closed at five
/// variants** by design (ADR 0001): a closed set lets [`SessionStore::apply`]
/// match exhaustively, and any future state becomes an explicit new variant
/// rather than an implicit field mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionDelta {
    /// Create or refresh a card's per-tick state. `seq` is the card generation
    /// (monotonic per card); an `Upsert` older than what the store last saw for
    /// that card — or one for a card already removed — is dropped.
    Upsert {
        id: SessionCardId,
        seq: u64,
        name: String,
        cwd: String,
        busy: bool,
        updated_at: WallClock,
        preview: Vec<String>,
    },
    /// Remove a card (session teardown). Records a tombstone so a late/queued
    /// `Upsert` for the same id cannot resurrect it.
    Remove { id: SessionCardId },
    /// Update the git branch and project icon (branch-poll thread, FR-8/FR-9).
    Branch {
        id: SessionCardId,
        branch: Option<String>,
        icon: IconKind,
    },
    /// Apply a user rename (FR-7): sets `name_override`, which survives later
    /// `Upsert`s.
    Rename { id: SessionCardId, name: String },
    /// Mark an unread bell (FR-11). Cleared by the main thread when the card's
    /// window gains focus.
    Bell { id: SessionCardId },
}

impl SessionDelta {
    /// The card this delta targets.
    pub fn id(&self) -> SessionCardId {
        match self {
            SessionDelta::Upsert { id, .. }
            | SessionDelta::Remove { id }
            | SessionDelta::Branch { id, .. }
            | SessionDelta::Rename { id, .. }
            | SessionDelta::Bell { id } => *id,
        }
    }
}

/// The central session registry (FR-1). Owned by the main thread; mutated only
/// through [`apply`](Self::apply) and [`reconcile_sessions`](Self::reconcile_sessions).
#[derive(Debug, Default)]
pub struct SessionStore {
    cards: HashMap<SessionCardId, SessionCard>,
    /// Ids that have been removed, with the `seq` seen at removal. An `Upsert`
    /// whose `seq` does not exceed the tombstone is a stale/reordered message
    /// and is dropped (Omen T10). Bounded to [`TOMBSTONE_CAP`] entries by FIFO
    /// eviction (see [`SessionStore::tombstone`]): because pane ids are
    /// monotonic and never reused, an evicted entry is never revisited, so the
    /// map would otherwise grow unbounded for the process lifetime.
    tombstones: HashMap<SessionCardId, u64>,
    /// Insertion order of the live [`tombstones`](Self::tombstones) keys, used
    /// to evict the oldest when the cap is exceeded. Kept exactly in sync with
    /// the map: every key appears here once, and only keys present in the map.
    tombstone_order: VecDeque<SessionCardId>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    pub fn get(&self, id: &SessionCardId) -> Option<&SessionCard> {
        self.cards.get(id)
    }

    /// Every live card id in a stable, deterministic order (by window id then
    /// pane id). The sidebar's render and hit-test paths both read this so the
    /// on-screen card order and the click target agree — a `HashMap` iteration
    /// order would let them drift.
    pub fn ordered_ids(&self) -> Vec<SessionCardId> {
        let mut ids: Vec<SessionCardId> = self.cards.keys().copied().collect();
        ids.sort_by_key(|id| (id.window_id.0, id.pane_id.get()));
        ids
    }

    /// Cards in [`ordered_ids`](Self::ordered_ids) order, paired with their id.
    pub fn ordered_cards(&self) -> Vec<(SessionCardId, &SessionCard)> {
        self.ordered_ids()
            .into_iter()
            .filter_map(|id| self.cards.get(&id).map(|card| (id, card)))
            .collect()
    }

    /// Clear the unread-bell flag on every card belonging to `window_id`
    /// (FR-11). Called by the main thread when that window gains focus, so a
    /// bell raised while the window was in the background stops flagging its
    /// cards once the user is looking at them. Not a [`SessionDelta`]: the
    /// main thread owns the store and clears directly.
    pub fn clear_bell_for_window(&mut self, window_id: SessionWindowId) {
        for (id, card) in self.cards.iter_mut() {
            if id.window_id == window_id {
                card.unread_bell = false;
            }
        }
    }

    /// The number of live cards whose program is running (FR-5 header status).
    pub fn busy_count(&self) -> usize {
        self.cards.values().filter(|card| card.busy).count()
    }

    /// Apply one delta. This is the only mutation entry point for deltas, so
    /// the stale-message and rename-override rules live in exactly one place.
    pub fn apply(&mut self, delta: SessionDelta) {
        match delta {
            SessionDelta::Upsert {
                id,
                seq,
                name,
                cwd,
                busy,
                updated_at,
                preview,
            } => {
                // Drop an upsert for an already-removed card, or one older than
                // the last generation we saw for it.
                if let Some(&tomb) = self.tombstones.get(&id)
                    && seq <= tomb
                {
                    return;
                }
                match self.cards.get_mut(&id) {
                    Some(card) => {
                        if seq < card.seq {
                            return;
                        }
                        // Refresh per-tick fields; preserve the rename override,
                        // branch/icon (owned by the branch-poll thread), and the
                        // unread-bell flag (cleared only on focus).
                        card.name = name;
                        card.cwd = cwd;
                        card.busy = busy;
                        card.updated_at = updated_at;
                        card.preview = preview;
                        card.seq = seq;
                    }
                    None => {
                        self.untombstone(id);
                        self.cards.insert(
                            id,
                            SessionCard {
                                name,
                                name_override: None,
                                cwd,
                                branch: None,
                                icon: IconKind::default(),
                                unread_bell: false,
                                busy,
                                updated_at,
                                preview,
                                seq,
                            },
                        );
                    }
                }
            }
            SessionDelta::Remove { id } => {
                self.retire(id);
            }
            SessionDelta::Branch { id, branch, icon } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.branch = branch;
                    card.icon = icon;
                }
            }
            SessionDelta::Rename { id, name } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.name_override = Some(name);
                }
            }
            SessionDelta::Bell { id } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.unread_bell = true;
                }
            }
        }
    }

    /// Drop every card whose id is not in `live_ids` (GC choke point, FR-12).
    /// Called from all five teardown sites so the store cannot outlive the
    /// panes it mirrors; removed ids are tombstoned like an explicit
    /// [`SessionDelta::Remove`]. After this returns, `len()` equals the number
    /// of live ids that had a card.
    pub fn reconcile_sessions(&mut self, live_ids: &[SessionCardId]) {
        let live: std::collections::HashSet<SessionCardId> = live_ids.iter().copied().collect();
        let dead: Vec<SessionCardId> = self
            .cards
            .keys()
            .filter(|id| !live.contains(id))
            .copied()
            .collect();
        for id in dead {
            self.retire(id);
        }
    }

    /// Remove a card and, if it existed, record a tombstone for its `seq` so a
    /// late/queued `Upsert` cannot resurrect it. The single choke point for the
    /// remove-then-tombstone invariant, shared by [`SessionDelta::Remove`] and
    /// [`reconcile_sessions`](Self::reconcile_sessions).
    fn retire(&mut self, id: SessionCardId) {
        if let Some(card) = self.cards.remove(&id) {
            self.tombstone(id, card.seq);
        }
    }

    /// Insert or refresh a tombstone, evicting the oldest entry when the map
    /// exceeds [`TOMBSTONE_CAP`]. Refreshing an existing key keeps its queue
    /// position (defensive: monotonic ids are normally retired once).
    fn tombstone(&mut self, id: SessionCardId, seq: u64) {
        // Monotonic, never-reused pane ids are retired exactly once, so an id
        // already in the map here would be a logic error rather than a normal
        // refresh — assert it in debug, and keep the existing queue position
        // (no double-push) in release.
        if self.tombstones.insert(id, seq).is_some() {
            debug_assert!(false, "tombstone refreshed for a never-reused id {id:?}");
            return;
        }
        self.tombstone_order.push_back(id);
        if self.tombstone_order.len() > TOMBSTONE_CAP
            && let Some(evicted) = self.tombstone_order.pop_front()
        {
            self.tombstones.remove(&evicted);
        }
    }

    /// Drop a tombstone from both the map and the order queue, keeping them in
    /// sync. Called when an `Upsert` legitimately recreates a removed id.
    fn untombstone(&mut self, id: SessionCardId) {
        if self.tombstones.remove(&id).is_some() {
            self.tombstone_order.retain(|&pending| pending != id);
        }
    }
}

/// Map a card's state to its status dot color (FR-11). Precedence is
/// bell > busy > idle: an unread bell wins over a running program, which wins
/// over idle.
pub fn status_dot(card: &SessionCard) -> StatusDot {
    if card.unread_bell {
        StatusDot::Yellow
    } else if card.busy {
        StatusDot::Blue
    } else {
        StatusDot::Green
    }
}

/// Serial day number for a civil date (Howard Hinnant's `days_from_civil`),
/// days since the Unix epoch. Pure integer math — used to compare calendar
/// days without a date/time crate.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Civil (proleptic Gregorian) date from a serial day number, days since the
/// Unix epoch (Howard Hinnant's `civil_from_days`, the inverse of
/// [`days_from_civil`]). Pure integer math.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (
        (if m <= 2 { y + 1 } else { y }) as i32,
        m as u32,
        d as u32,
    )
}

/// Break a count of seconds-since-the-Unix-epoch into calendar [`WallClock`]
/// fields. `secs` is expected to already carry the viewer's local UTC offset
/// (the io thread adds it before stamping), so the fields read as local civil
/// time. Pure and testable via known epoch anchors.
pub fn civil_from_unix_secs(secs: i64) -> WallClock {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    WallClock {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        minute: ((rem % 3600) / 60) as u32,
    }
}

/// Format a wall-clock timestamp relative to `now` (FR-10). `now` is a
/// parameter (no `Instant::now()` inside) so the formatter is pure and its
/// boundaries are directly testable. Rules, keyed off the calendar-day gap:
/// - same day: `たった今` / `N分前` / `N時間前`
/// - yesterday: `昨日 HH:MM`
/// - older: `M月D日`
pub fn format_relative_time(now: WallClock, updated: WallClock) -> String {
    let day_diff =
        days_from_civil(now.year, now.month, now.day) - days_from_civil(updated.year, updated.month, updated.day);

    if day_diff <= 0 {
        // Same day (day_diff < 0 would be clock skew; treat as "just now").
        let now_min = (now.hour * 60 + now.minute) as i64;
        let updated_min = (updated.hour * 60 + updated.minute) as i64;
        let elapsed = (now_min - updated_min).max(0);
        if elapsed < 1 {
            "たった今".to_string()
        } else if elapsed < 60 {
            format!("{elapsed}分前")
        } else {
            format!("{}時間前", elapsed / 60)
        }
    } else if day_diff == 1 {
        format!("昨日 {:02}:{:02}", updated.hour, updated.minute)
    } else {
        format!("{}月{}日", updated.month, updated.day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_id(window: u64, pane: u64) -> SessionCardId {
        SessionCardId::new(SessionWindowId(window), PaneId::new(pane))
    }

    fn wall(hour: u32, minute: u32) -> WallClock {
        WallClock {
            year: 2026,
            month: 7,
            day: 5,
            hour,
            minute,
        }
    }

    fn upsert(id: SessionCardId, seq: u64, name: &str) -> SessionDelta {
        SessionDelta::Upsert {
            id,
            seq,
            name: name.to_string(),
            cwd: "/repo".to_string(),
            busy: false,
            updated_at: wall(10, 0),
            preview: Vec::new(),
        }
    }

    // AC-1 (FR-1): Upsert then Remove grows then shrinks the store, and the id
    // is gone afterwards.
    #[test]
    fn upsert_then_remove_lifecycle() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        assert_eq!(store.len(), 0);
        store.apply(upsert(id, 1, "shell"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&id).unwrap().display_name(), "shell");

        store.apply(SessionDelta::Remove { id });
        assert_eq!(store.len(), 0);
        assert!(store.get(&id).is_none());
    }

    // AC-9 (FR-7): a rename override survives a subsequent Upsert.
    #[test]
    fn rename_override_survives_upsert() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        store.apply(upsert(id, 1, "old-title"));
        store.apply(SessionDelta::Rename {
            id,
            name: "my session".to_string(),
        });
        assert_eq!(store.get(&id).unwrap().display_name(), "my session");

        // A later Upsert refreshes the shell title but not the display name.
        store.apply(upsert(id, 2, "new-title"));
        let card = store.get(&id).unwrap();
        assert_eq!(card.name, "new-title");
        assert_eq!(card.display_name(), "my session");
    }

    // Omen T10: an Upsert that arrives after the card was removed (stale/queued
    // message with an older-or-equal seq) is dropped, not resurrected.
    #[test]
    fn stale_upsert_after_remove_is_dropped() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        store.apply(upsert(id, 5, "shell"));
        store.apply(SessionDelta::Remove { id });
        // A reordered Upsert with an older generation must not recreate it.
        store.apply(upsert(id, 4, "ghost"));
        assert_eq!(store.len(), 0);
        assert!(store.get(&id).is_none());

        // A genuinely newer Upsert (fresh generation) is allowed to recreate.
        store.apply(upsert(id, 6, "reborn"));
        assert_eq!(store.get(&id).unwrap().name, "reborn");
    }

    #[test]
    fn out_of_order_upsert_does_not_overwrite_newer() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        store.apply(upsert(id, 2, "newer"));
        store.apply(upsert(id, 1, "older"));
        assert_eq!(store.get(&id).unwrap().name, "newer");
    }

    // AC-14 (FR-12): reconcile drops ids absent from the live set; the store
    // size equals the number of live ids (all of which had cards here).
    #[test]
    fn reconcile_drops_absent_ids() {
        let mut store = SessionStore::new();
        let live = [card_id(1, 1), card_id(1, 2)];
        let dead = card_id(1, 3);

        store.apply(upsert(live[0], 1, "a"));
        store.apply(upsert(live[1], 1, "b"));
        store.apply(upsert(dead, 1, "c"));
        assert_eq!(store.len(), 3);

        store.reconcile_sessions(&live);
        assert_eq!(store.len(), live.len());
        assert!(store.get(&dead).is_none());
        assert!(store.get(&live[0]).is_some());
        assert!(store.get(&live[1]).is_some());
    }

    #[test]
    fn reconcile_tombstones_removed_ids() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        store.apply(upsert(id, 3, "a"));
        store.reconcile_sessions(&[]);
        assert_eq!(store.len(), 0);

        // A stale upsert for the reconciled id is dropped like an explicit Remove.
        store.apply(upsert(id, 2, "ghost"));
        assert_eq!(store.len(), 0);
    }

    // The tombstone map is FIFO-bounded: retiring more than TOMBSTONE_CAP
    // distinct (never-reused) ids evicts the oldest and keeps the map and its
    // order queue in lockstep at the cap.
    #[test]
    fn tombstone_map_is_bounded_by_fifo_eviction() {
        let mut store = SessionStore::new();
        let total = TOMBSTONE_CAP + 8;

        for i in 0..total {
            let id = card_id(1, i as u64);
            store.apply(upsert(id, 1, "s"));
            store.apply(SessionDelta::Remove { id });
        }

        assert_eq!(store.tombstones.len(), TOMBSTONE_CAP);
        assert_eq!(store.tombstone_order.len(), TOMBSTONE_CAP);
        // The oldest-retired ids were evicted; the most recent CAP survive.
        assert!(!store.tombstones.contains_key(&card_id(1, 0)));
        assert!(store.tombstones.contains_key(&card_id(1, (total - 1) as u64)));
    }

    // Recreating a removed id un-tombstones it in both the map and the order
    // queue, so the two never drift out of sync.
    #[test]
    fn untombstone_keeps_map_and_order_in_sync() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);

        store.apply(upsert(id, 1, "s"));
        store.apply(SessionDelta::Remove { id });
        assert_eq!(store.tombstones.len(), 1);
        assert_eq!(store.tombstone_order.len(), 1);

        // A fresh-generation upsert recreates the card and clears the tombstone.
        store.apply(upsert(id, 2, "reborn"));
        assert_eq!(store.tombstones.len(), 0);
        assert_eq!(store.tombstone_order.len(), 0);
        assert_eq!(store.get(&id).unwrap().name, "reborn");
    }

    // AC-13 (FR-11): status-dot color mapping with bell > busy > idle.
    #[test]
    fn status_dot_maps_state_to_color() {
        let base = SessionCard {
            name: "s".to_string(),
            name_override: None,
            cwd: "/repo".to_string(),
            branch: None,
            icon: IconKind::default(),
            unread_bell: false,
            busy: false,
            updated_at: wall(10, 0),
            preview: Vec::new(),
            seq: 1,
        };

        assert_eq!(status_dot(&base), StatusDot::Green);

        let busy = SessionCard {
            busy: true,
            ..base.clone()
        };
        assert_eq!(status_dot(&busy), StatusDot::Blue);

        let bell = SessionCard {
            unread_bell: true,
            ..base.clone()
        };
        assert_eq!(status_dot(&bell), StatusDot::Yellow);

        // Bell wins over busy.
        let bell_and_busy = SessionCard {
            unread_bell: true,
            busy: true,
            ..base
        };
        assert_eq!(status_dot(&bell_and_busy), StatusDot::Yellow);
    }

    // AC-12 (FR-10): relative-time formatting at each boundary.
    #[test]
    fn relative_time_formats_each_boundary() {
        let now = wall(10, 3);

        // Same day, 3 minutes earlier.
        assert_eq!(format_relative_time(now, wall(10, 0)), "3分前");
        // Same day, exact same minute.
        assert_eq!(format_relative_time(now, wall(10, 3)), "たった今");
        // Same day, 2 hours earlier.
        assert_eq!(format_relative_time(wall(12, 0), wall(10, 0)), "2時間前");

        // Yesterday at 23:47.
        let yesterday = WallClock {
            day: 4,
            hour: 23,
            minute: 47,
            ..now
        };
        assert_eq!(format_relative_time(now, yesterday), "昨日 23:47");

        // Older than yesterday → date form.
        let older = WallClock {
            day: 1,
            hour: 8,
            minute: 15,
            ..now
        };
        assert_eq!(format_relative_time(now, older), "7月1日");
    }

    #[test]
    fn civil_from_unix_secs_round_trips_known_anchors() {
        // Unix epoch.
        assert_eq!(
            civil_from_unix_secs(0),
            WallClock { year: 1970, month: 1, day: 1, hour: 0, minute: 0 }
        );
        // 2026-07-05 23:47:12 UTC → 1751759232.
        let secs = (days_from_civil(2026, 7, 5) * 86_400) + 23 * 3600 + 47 * 60 + 12;
        assert_eq!(
            civil_from_unix_secs(secs),
            WallClock { year: 2026, month: 7, day: 5, hour: 23, minute: 47 }
        );
        // A negative offset (before the epoch) still decomposes correctly.
        assert_eq!(
            civil_from_unix_secs(-1),
            WallClock { year: 1969, month: 12, day: 31, hour: 23, minute: 59 }
        );
    }

    #[test]
    fn ordered_ids_are_sorted_by_window_then_pane() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(2, 1), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "a3"));
        store.apply(upsert(card_id(1, 1), 1, "a1"));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 3), card_id(2, 1)]
        );
    }

    #[test]
    fn clear_bell_for_window_only_clears_that_window() {
        let mut store = SessionStore::new();
        let (a, b) = (card_id(1, 1), card_id(2, 1));
        store.apply(upsert(a, 1, "a"));
        store.apply(upsert(b, 1, "b"));
        store.apply(SessionDelta::Bell { id: a });
        store.apply(SessionDelta::Bell { id: b });

        store.clear_bell_for_window(SessionWindowId(1));
        assert!(!store.get(&a).unwrap().unread_bell);
        assert!(store.get(&b).unwrap().unread_bell);
    }

    #[test]
    fn days_from_civil_matches_known_epoch_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // Across a month boundary within the same serial arithmetic.
        assert_eq!(
            days_from_civil(2026, 7, 5) - days_from_civil(2026, 6, 30),
            5
        );
    }

    // AC-2 / AC-22 (NFR-6): this module must stay GUI-agnostic. Assert its
    // source imports no windowing/GPU crate. The needles are assembled at
    // runtime so this test file does not trip its own scan.
    #[test]
    fn session_store_is_gui_agnostic() {
        let source = include_str!("session_store.rs");
        for forbidden in [
            ["use ", "winit"].concat(),
            ["use ", "wgpu"].concat(),
            ["winit", "::"].concat(),
            ["wgpu", "::"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "session_store.rs must not reference `{forbidden}`"
            );
        }
    }
}
