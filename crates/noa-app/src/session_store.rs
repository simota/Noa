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

use std::collections::{HashMap, HashSet, VecDeque};

use noa_core::Color;

use crate::split_tree::PaneId;

/// One color run within a card's last-output preview (FR-2): a maximal span of
/// preview text sharing one foreground color. The io thread coalesces adjacent
/// cells with equal fg into a span so the sidebar can show last output in its
/// original ANSI colors rather than one flat gray. `fg` is the raw cell color
/// (`Color::Default` resolves to the sidebar's dim fg at draw time); carrying
/// the pure `noa_core::Color` keeps this module GUI-agnostic (no resolved RGB
/// or theme dependency here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewSpan {
    pub text: String,
    pub fg: Color,
}

/// One preview line: its color runs, left to right.
pub type PreviewLine = Vec<PreviewSpan>;

/// The plain text of a preview line — its spans concatenated. Used by tests and
/// any consumer that only needs the characters, not the colors.
pub fn preview_line_text(line: &[PreviewSpan]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// FIFO cap on the tombstone map. Pane ids are monotonic and never reused, so a
/// tombstone is only ever consulted to reject stale/reordered `Upsert`s already
/// in flight for a just-removed id; once that id's queue has drained the entry
/// is inert. Retiring [`TOMBSTONE_CAP`] newer ids is far more than enough to
/// outlast any in-flight message, so evicting the oldest entry is safe and
/// keeps the map bounded rather than growing for the process lifetime.
const TOMBSTONE_CAP: usize = 64;
const AUTO_APPROVE_AUDIT_CAPACITY: usize = 16;

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
/// program is running), `Green` = idle, `Yellow` = an unread bell is pending,
/// `Red` = the program requested user interaction (OSC 9/777) and is waiting.
/// Precedence is attention > bell > busy > idle (see [`status_dot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusDot {
    Blue,
    Green,
    Yellow,
    Red,
}

/// One auto-approve audit entry shown on the session card. The store keeps the
/// ring buffer as pure data; the app layer decides when an approval is valid
/// and records only a non-sensitive agent/prompt summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoApproveAuditEntry {
    pub at: WallClock,
    pub agent: String,
    pub prompt: String,
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
    /// The running program posted a desktop notification (OSC 9/777) while the
    /// window was unfocused — typically an AI agent (Claude Code / Codex / agy)
    /// waiting for the user's reply. Cleared, like `unread_bell`, when the
    /// card's window gains focus.
    pub attention: bool,
    pub busy: bool,
    /// Per-tab agent-prompt auto approval is enabled for this card's tab.
    pub auto_approve_enabled: bool,
    /// Rolling audit of injected approvals, capped by the store.
    pub auto_approve_audit: VecDeque<AutoApproveAuditEntry>,
    /// The tty's current foreground process name (FR — running-process display),
    /// e.g. `zsh` / `cargo` / `claude`. `None` until the session-metadata worker
    /// resolves it, or where detection is unavailable (non-macOS, NFR-5).
    pub process: Option<String>,
    /// This pane's foreground-process-tree metrics (panel-metrics-view
    /// FR-4/FR-7). `None` until the process-monitor overlay is open and the
    /// branch-poll metrics tick has posted at least one sample for it, and
    /// cleared again by [`SessionStore::clear_all_metrics`] on overlay close
    /// so a reopen never flashes stale numbers.
    pub metrics: Option<noa_pty::PaneMetrics>,
    pub updated_at: WallClock,
    /// Last-output preview lines (capped by `sidebar-preview-lines`; FR-2),
    /// each carrying its color runs so the sidebar renders output in its
    /// original ANSI colors. Filled by the io thread from the active screen's
    /// trailing rows.
    pub preview: Vec<PreviewLine>,
    seq: u64,
    /// Store-global monotonic stamp of the last applied `Upsert` (an upsert
    /// only fires on pty output, so this is "last output" order). Drives the
    /// recency auto-sort ([`refresh_auto_order`](SessionStore::refresh_auto_order));
    /// finer than `updated_at`, whose minute granularity would tie cards that
    /// updated within the same minute.
    activity: u64,
}

impl SessionCard {
    /// The name to display: the user rename if present, else the shell title.
    pub fn display_name(&self) -> &str {
        self.name_override.as_deref().unwrap_or(&self.name)
    }
}

/// A single mutation to the [`SessionStore`]. This enum is a **closed set** by
/// design (ADR 0001): a closed set lets [`SessionStore::apply`] match
/// exhaustively, and any future state becomes an explicit new variant rather
/// than an implicit field mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionDelta {
    /// Create or refresh a card's per-tick state. `seq` is the card generation
    /// (monotonic per card); an `Upsert` older than what the store last saw for
    /// that card — or one for a card already removed — is dropped. `preview` is
    /// `None` when the io thread skipped the extraction (sidebar hidden —
    /// FR-A4's lightweight upsert): the card's existing preview is kept rather
    /// than cleared.
    Upsert {
        id: SessionCardId,
        seq: u64,
        name: String,
        cwd: String,
        busy: bool,
        updated_at: WallClock,
        preview: Option<Vec<PreviewLine>>,
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
    /// Mark a pending interaction request (FR-16): the running program posted a
    /// desktop notification (OSC 9/777) while its window was unfocused. Cleared
    /// alongside bells when the card's window gains focus.
    Attention { id: SessionCardId },
    /// Update the tty's foreground process name (session-metadata worker). Posted
    /// on a poll tick, so it carries only the process; other fields are untouched.
    Process {
        id: SessionCardId,
        process: Option<String>,
    },
    /// Update a pane's foreground-process-tree metrics (panel-metrics-view
    /// FR-4/FR-7, branch-poll metrics tick). Posted once per tick while the
    /// process-monitor overlay is open; `None` when the tick could not
    /// resolve a tree for this pane (FR-8, e.g. no foreground group).
    Metrics {
        id: SessionCardId,
        metrics: Option<noa_pty::PaneMetrics>,
    },
}

impl SessionDelta {
    /// The card this delta targets.
    pub fn id(&self) -> SessionCardId {
        match self {
            SessionDelta::Upsert { id, .. }
            | SessionDelta::Remove { id }
            | SessionDelta::Branch { id, .. }
            | SessionDelta::Rename { id, .. }
            | SessionDelta::Bell { id }
            | SessionDelta::Attention { id }
            | SessionDelta::Process { id, .. }
            | SessionDelta::Metrics { id, .. } => *id,
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
    /// User-defined card sequence, set by a sidebar drag-reorder
    /// ([`move_card_before`](Self::move_card_before)). Empty until the user first
    /// reorders — while empty, [`ordered_ids`](Self::ordered_ids) falls back to
    /// the natural window/pane sort so behavior is unchanged. Once set, it is the
    /// source of truth for the base sequence; live cards missing from it (freshly
    /// spawned after a reorder) append in natural order, and dead ids are pruned
    /// in [`retire`](Self::retire). Not persisted across restarts (v1 scope).
    manual_order: Vec<SessionCardId>,
    /// The recency auto-sort sequence (most recently updated first) — the base
    /// order while the user has not manually reordered. A snapshot rather than
    /// a live sort: it only changes when the app calls
    /// [`refresh_auto_order`](Self::refresh_auto_order) (a fixed ~5s cadence),
    /// so cards don't shuffle under the pointer on every output tick. Kept in
    /// lockstep with the live card set: a freshly created card is inserted at
    /// the front (its `activity` is the global max, so the front *is* its
    /// sorted position), and [`retire`](Self::retire) prunes dead ids.
    auto_order: Vec<SessionCardId>,
    /// Monotonic counter behind [`SessionCard::activity`], bumped on every
    /// applied `Upsert`.
    activity_counter: u64,
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

    /// Every live card id in a stable, deterministic order: cards with a
    /// pending attention request float to the top (so an agent waiting for the
    /// user is never buried below the scroll fold), the rest follow the base
    /// order — the user's manual drag order when set, else the recency
    /// auto-sort snapshot (most recently updated first, re-sorted only on the
    /// app's [`refresh_auto_order`](Self::refresh_auto_order) cadence so cards
    /// do not jump around on ordinary output ticks). The sidebar's render and
    /// hit-test paths both read this so the on-screen card order and the click
    /// target agree — a `HashMap` iteration order would let them drift.
    pub fn ordered_ids(&self) -> Vec<SessionCardId> {
        let mut ids = self.base_order_ids();
        // Stable partition: cards with a pending attention request float to the
        // top while every card keeps its base-order position among its peers.
        // `sort_by_key` is a stable sort, so the manual/natural sequence within
        // each group survives.
        ids.sort_by_key(|id| !self.cards.get(id).is_some_and(|card| card.attention));
        ids
    }

    /// The base card sequence *before* the attention float: the user's manual
    /// order when set (dead ids filtered, freshly-spawned cards appended in
    /// natural order), else the recency auto-sort snapshot (which `apply`/
    /// `retire` keep exactly in lockstep with the live card set). Split out so
    /// [`move_card_before`](Self::move_card_before) can materialize a full,
    /// attention-independent sequence to edit.
    fn base_order_ids(&self) -> Vec<SessionCardId> {
        if self.manual_order.is_empty() {
            return self.auto_order.clone();
        }
        let mut seq: Vec<SessionCardId> = self
            .manual_order
            .iter()
            .copied()
            .filter(|id| self.cards.contains_key(id))
            .collect();
        let placed: HashSet<SessionCardId> = seq.iter().copied().collect();
        // Cards spawned after the last reorder aren't in `manual_order` yet;
        // append them in natural order so they land in a deterministic spot.
        for id in self.natural_order_ids() {
            if !placed.contains(&id) {
                seq.push(id);
            }
        }
        seq
    }

    /// Every live card id sorted by window id then pane id — the deterministic
    /// tie-break sequence used to append freshly-spawned cards to a manual
    /// order and to break `activity` ties in the recency sort.
    fn natural_order_ids(&self) -> Vec<SessionCardId> {
        let mut ids: Vec<SessionCardId> = self.cards.keys().copied().collect();
        ids.sort_by_key(|id| (id.window_id.0, id.pane_id.get()));
        ids
    }

    /// Re-sort the auto-order snapshot by update recency (most recently
    /// updated first; window/pane order breaks the — in practice impossible —
    /// `activity` tie). Returns `true` when the sequence actually changed (a
    /// redraw is worth requesting). Called by the app on a fixed cadence
    /// (`SIDEBAR_AUTOSORT_INTERVAL`) rather than from `apply`, so the visible
    /// order is stable between ticks no matter how often cards update.
    pub fn refresh_auto_order(&mut self) -> bool {
        let mut seq: Vec<SessionCardId> = self.cards.keys().copied().collect();
        seq.sort_by_key(|id| {
            (
                std::cmp::Reverse(self.cards[id].activity),
                id.window_id.0,
                id.pane_id.get(),
            )
        });
        if seq == self.auto_order {
            return false;
        }
        self.auto_order = seq;
        true
    }

    /// Move `moved` to sit immediately before `anchor` in the card sequence, or
    /// to the end when `anchor` is `None` (a drop past the last card). Called by
    /// the sidebar drag-reorder. Neighbor-relative rather than index-based so it
    /// works regardless of the per-window filtering the sidebar applies to the
    /// visible list. Materializes the current base order into `manual_order` on
    /// the first reorder so unmoved cards keep their on-screen position. Returns
    /// `true` when the sequence actually changed (a redraw is worth requesting).
    pub fn move_card_before(
        &mut self,
        moved: SessionCardId,
        anchor: Option<SessionCardId>,
    ) -> bool {
        if !self.cards.contains_key(&moved) || anchor == Some(moved) {
            return false;
        }
        let base = self.base_order_ids();
        let mut seq = base.clone();
        let Some(from) = seq.iter().position(|&id| id == moved) else {
            return false;
        };
        seq.remove(from);
        let to = match anchor {
            Some(anchor) => seq.iter().position(|&id| id == anchor).unwrap_or(seq.len()),
            None => seq.len(),
        };
        seq.insert(to, moved);
        if seq == base {
            return false;
        }
        self.manual_order = seq;
        true
    }

    /// [`ordered_ids`](Self::ordered_ids), filtered to cards whose `window_id`
    /// is in `windows` (per-window sidebar, spec `sidebar-per-window-sessions`
    /// R1/R3/R6): the relative order (attention float → window_id → pane_id) is
    /// preserved because it's derived from the same sort, just filtered after.
    pub fn ordered_ids_for_windows(
        &self,
        windows: &HashSet<SessionWindowId>,
    ) -> Vec<SessionCardId> {
        self.ordered_ids()
            .into_iter()
            .filter(|id| windows.contains(&id.window_id))
            .collect()
    }

    /// Cards in [`ordered_ids`](Self::ordered_ids) order, paired with their id.
    pub fn ordered_cards(&self) -> Vec<(SessionCardId, &SessionCard)> {
        self.ordered_ids()
            .into_iter()
            .filter_map(|id| self.cards.get(&id).map(|card| (id, card)))
            .collect()
    }

    /// Clear the unread-bell and pending-attention flags on every card
    /// belonging to `window_id` (FR-11/FR-16). Called by the main thread when
    /// that window gains focus, so a bell or interaction request raised while
    /// the window was in the background stops flagging its cards once the user
    /// is looking at them. Not a [`SessionDelta`]: the main thread owns the
    /// store and clears directly.
    pub fn clear_bell_for_window(&mut self, window_id: SessionWindowId) {
        for (id, card) in self.cards.iter_mut() {
            if id.window_id == window_id {
                card.unread_bell = false;
                card.attention = false;
            }
        }
    }

    /// Mirror a per-tab auto-approve toggle into every card in that tab.
    pub fn set_auto_approve_for_window(&mut self, window_id: SessionWindowId, enabled: bool) {
        for (id, card) in self.cards.iter_mut() {
            if id.window_id == window_id {
                card.auto_approve_enabled = enabled;
            }
        }
    }

    /// Append one audit item to a card's bounded ring buffer.
    pub fn record_auto_approve(&mut self, id: SessionCardId, entry: AutoApproveAuditEntry) {
        let Some(card) = self.cards.get_mut(&id) else {
            return;
        };
        card.auto_approve_audit.push_back(entry);
        if card.auto_approve_audit.len() > AUTO_APPROVE_AUDIT_CAPACITY {
            card.auto_approve_audit.pop_front();
        }
    }

    /// The number of live cards whose program is running (FR-5 header status).
    pub fn busy_count(&self) -> usize {
        self.cards.values().filter(|card| card.busy).count()
    }

    /// The number of live cards with a pending attention request (FR-16),
    /// surfaced as a header badge so a request whose card has scrolled out of
    /// the viewport is still noticeable.
    pub fn attention_count(&self) -> usize {
        self.cards.values().filter(|card| card.attention).count()
    }

    /// The `(busy, attention)` counts among cards whose `window_id` is in
    /// `windows` (per-window sidebar header counts, R5) — the filtered
    /// counterpart of [`busy_count`](Self::busy_count)/[`attention_count`](Self::attention_count).
    pub fn counts_for_windows(&self, windows: &HashSet<SessionWindowId>) -> (usize, usize) {
        self.cards
            .iter()
            .filter(|(id, _)| windows.contains(&id.window_id))
            .fold((0, 0), |(busy, attention), (_, card)| {
                (
                    busy + usize::from(card.busy),
                    attention + usize::from(card.attention),
                )
            })
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
                        // branch/icon (owned by the branch-poll thread), the
                        // unread-bell/attention flags (cleared only on focus),
                        // and — on a lightweight upsert — the preview.
                        card.name = name;
                        card.cwd = cwd;
                        card.busy = busy;
                        card.updated_at = updated_at;
                        if let Some(preview) = preview {
                            card.preview = preview;
                        }
                        card.seq = seq;
                        // Stamp recency, but leave `auto_order` alone: the
                        // visible order only re-sorts on `refresh_auto_order`.
                        self.activity_counter += 1;
                        card.activity = self.activity_counter;
                    }
                    None => {
                        self.untombstone(id);
                        self.activity_counter += 1;
                        self.cards.insert(
                            id,
                            SessionCard {
                                name,
                                name_override: None,
                                cwd,
                                branch: None,
                                icon: IconKind::default(),
                                unread_bell: false,
                                attention: false,
                                busy,
                                auto_approve_enabled: false,
                                auto_approve_audit: VecDeque::new(),
                                process: None,
                                metrics: None,
                                updated_at,
                                preview: preview.unwrap_or_default(),
                                seq,
                                activity: self.activity_counter,
                            },
                        );
                        // A brand-new card carries the global-max activity, so
                        // the front of the recency snapshot is exactly its
                        // sorted position — insert now rather than leaving it
                        // invisible until the next refresh tick.
                        self.auto_order.insert(0, id);
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
            SessionDelta::Attention { id } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.attention = true;
                }
            }
            SessionDelta::Process { id, process } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.process = process;
                }
            }
            SessionDelta::Metrics { id, metrics } => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.metrics = metrics;
                }
            }
        }
    }

    /// Clear every card's metrics (panel-metrics-view: process-monitor
    /// overlay close). Called from the overlay's single close choke point so
    /// a reopen never briefly shows the previous session's stale numbers
    /// before the first fresh tick lands.
    pub fn clear_all_metrics(&mut self) {
        for card in self.cards.values_mut() {
            card.metrics = None;
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
            // Keep the manual order free of dead ids so it doesn't grow for the
            // process lifetime (mirrors the tombstone cap's intent). `ordered_ids`
            // already filters to live cards, so this is housekeeping, not
            // correctness.
            self.manual_order.retain(|&pending| pending != id);
            // The auto order, by contrast, is consumed unfiltered — it must
            // mirror the live card set exactly.
            self.auto_order.retain(|&pending| pending != id);
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

/// Map a card's state to its status dot color (FR-11/FR-16). Precedence is
/// attention > bell > busy > idle: a pending interaction request wins over an
/// unread bell, which wins over a running program, which wins over idle.
pub fn status_dot(card: &SessionCard) -> StatusDot {
    if card.attention {
        StatusDot::Red
    } else if card.unread_bell {
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
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u32, d as u32)
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
    let day_diff = days_from_civil(now.year, now.month, now.day)
        - days_from_civil(updated.year, updated.month, updated.day);

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
            preview: None,
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
        assert!(
            store
                .tombstones
                .contains_key(&card_id(1, (total - 1) as u64))
        );
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
            attention: false,
            busy: false,
            auto_approve_enabled: false,
            auto_approve_audit: VecDeque::new(),
            process: None,
            metrics: None,
            updated_at: wall(10, 0),
            preview: Vec::new(),
            seq: 1,
            activity: 0,
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
            ..base.clone()
        };
        assert_eq!(status_dot(&bell_and_busy), StatusDot::Yellow);

        // Attention (FR-16) wins over everything.
        let attention = SessionCard {
            attention: true,
            unread_bell: true,
            busy: true,
            ..base
        };
        assert_eq!(status_dot(&attention), StatusDot::Red);
    }

    // FR-16: `Attention` sets the flag, an `Upsert` preserves it, and a window
    // focus clears it together with the bell.
    #[test]
    fn attention_is_set_preserved_and_cleared_on_focus() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);
        store.apply(upsert(id, 1, "s"));
        assert!(!store.get(&id).unwrap().attention);

        store.apply(SessionDelta::Attention { id });
        assert!(store.get(&id).unwrap().attention);

        // A later per-tick upsert keeps the flag (like the unread bell).
        store.apply(upsert(id, 2, "s"));
        assert!(store.get(&id).unwrap().attention);

        store.apply(SessionDelta::Bell { id });
        store.clear_bell_for_window(id.window_id);
        let card = store.get(&id).unwrap();
        assert!(!card.attention);
        assert!(!card.unread_bell);
    }

    #[test]
    fn auto_approve_toggle_and_audit_are_card_state() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);
        store.apply(upsert(id, 1, "s"));
        assert!(!store.get(&id).unwrap().auto_approve_enabled);

        store.set_auto_approve_for_window(id.window_id, true);
        assert!(store.get(&id).unwrap().auto_approve_enabled);
        store.apply(upsert(id, 2, "s2"));
        assert!(
            store.get(&id).unwrap().auto_approve_enabled,
            "Upsert must not revert the main-thread toggle state"
        );

        for index in 0..(AUTO_APPROVE_AUDIT_CAPACITY + 2) {
            store.record_auto_approve(
                id,
                AutoApproveAuditEntry {
                    at: wall(10, index as u32),
                    agent: "Claude Code".to_string(),
                    prompt: format!("Edit {index}"),
                },
            );
        }
        let card = store.get(&id).unwrap();
        assert_eq!(card.auto_approve_audit.len(), AUTO_APPROVE_AUDIT_CAPACITY);
        assert_eq!(card.auto_approve_audit.front().unwrap().prompt, "Edit 2");
        assert_eq!(
            card.auto_approve_audit.back().unwrap().prompt,
            format!("Edit {}", AUTO_APPROVE_AUDIT_CAPACITY + 1)
        );
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
            WallClock {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0
            }
        );
        // 2026-07-05 23:47:12 UTC → 1751759232.
        let secs = (days_from_civil(2026, 7, 5) * 86_400) + 23 * 3600 + 47 * 60 + 12;
        assert_eq!(
            civil_from_unix_secs(secs),
            WallClock {
                year: 2026,
                month: 7,
                day: 5,
                hour: 23,
                minute: 47
            }
        );
        // A negative offset (before the epoch) still decomposes correctly.
        assert_eq!(
            civil_from_unix_secs(-1),
            WallClock {
                year: 1969,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59
            }
        );
    }

    // The default order is update recency, newest first: a freshly created
    // card enters at the top (its activity is the global max).
    #[test]
    fn ordered_ids_default_to_update_recency() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(2, 1), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "a3"));
        store.apply(upsert(card_id(1, 1), 1, "a1"));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 3), card_id(2, 1)]
        );
    }

    // The recency auto-sort is throttled: an upsert to an existing card stamps
    // its activity but does not move it until `refresh_auto_order`, which
    // re-sorts newest-first and reports whether anything changed.
    #[test]
    fn refresh_auto_order_resorts_by_recency_and_reports_change() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "c"));
        // Creation order, newest first.
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 3), card_id(1, 2), card_id(1, 1)]
        );

        // `a` produces output: its stamp advances, the visible order does not.
        store.apply(upsert(card_id(1, 1), 2, "a"));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 3), card_id(1, 2), card_id(1, 1)]
        );

        // The throttle tick re-sorts: a floats to the top.
        assert!(store.refresh_auto_order());
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 3), card_id(1, 2)]
        );
        // Nothing changed since → no redraw owed.
        assert!(!store.refresh_auto_order());
    }

    // A removed card is pruned from the auto order (which is consumed
    // unfiltered), so the sequence always mirrors the live card set.
    #[test]
    fn auto_order_prunes_removed_cards() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.apply(SessionDelta::Remove { id: card_id(1, 2) });
        assert_eq!(store.ordered_ids(), vec![card_id(1, 1)]);
        assert!(!store.auto_order.contains(&card_id(1, 2)));
    }

    // A pending attention request floats its card to the top of the order, and
    // clearing it (window focus) restores the recency base order.
    #[test]
    fn ordered_ids_float_attention_cards_to_the_top() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.apply(upsert(card_id(2, 1), 1, "c"));
        // Base (recency) order: c, b, a.

        store.apply(SessionDelta::Attention { id: card_id(1, 1) });
        assert_eq!(store.attention_count(), 1);
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(2, 1), card_id(1, 2)]
        );

        store.clear_bell_for_window(SessionWindowId(1));
        assert_eq!(store.attention_count(), 0);
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(2, 1), card_id(1, 2), card_id(1, 1)]
        );
    }

    // Drag-reorder: moving a card before another rewrites the sequence, and the
    // manual order becomes the source of truth (overriding the window/pane sort).
    #[test]
    fn move_card_before_reorders_and_sticks() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "c"));
        // Base (recency) order is c, b, a.
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 3), card_id(1, 2), card_id(1, 1)]
        );

        // Drag a above c: a, c, b.
        assert!(store.move_card_before(card_id(1, 1), Some(card_id(1, 3))));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 3), card_id(1, 2)]
        );

        // Drop past the last card (anchor None) sends c to the end: a, b, c.
        assert!(store.move_card_before(card_id(1, 3), None));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 2), card_id(1, 3)]
        );
    }

    // A no-op move (dropping a card onto itself or into its own slot) reports no
    // change so the caller can skip the redraw.
    #[test]
    fn move_card_before_noop_returns_false() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        // Onto itself.
        assert!(!store.move_card_before(card_id(1, 1), Some(card_id(1, 1))));
        // Into its own slot (b before a is already the recency order).
        assert!(!store.move_card_before(card_id(1, 2), Some(card_id(1, 1))));
        // Unknown card.
        assert!(!store.move_card_before(card_id(9, 9), None));
    }

    // Attention float still wins over a manual order: a reordered list keeps its
    // relative sequence, but an attention card rises to the top.
    #[test]
    fn manual_order_preserved_under_attention_float() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "c"));
        // Manual order: c, b, a.
        store.move_card_before(card_id(1, 3), Some(card_id(1, 1)));
        store.move_card_before(card_id(1, 2), Some(card_id(1, 1)));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 3), card_id(1, 2), card_id(1, 1)]
        );
        // a raises attention → floats above the manual sequence, which is
        // otherwise preserved (c, b).
        store.apply(SessionDelta::Attention { id: card_id(1, 1) });
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 3), card_id(1, 2)]
        );
    }

    // A card spawned after a reorder appends in natural order; a removed card is
    // pruned from the manual sequence.
    #[test]
    fn manual_order_handles_spawn_and_remove() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(1, 2), 1, "b"));
        store.move_card_before(card_id(1, 1), Some(card_id(1, 2))); // a, b (recency was b, a)
        // A new card appends after the manual sequence.
        store.apply(upsert(card_id(1, 3), 1, "c"));
        assert_eq!(
            store.ordered_ids(),
            vec![card_id(1, 1), card_id(1, 2), card_id(1, 3)]
        );
        // Removing the manually-placed head drops it from the sequence.
        store.apply(SessionDelta::Remove { id: card_id(1, 1) });
        assert_eq!(store.ordered_ids(), vec![card_id(1, 2), card_id(1, 3)]);
        assert!(!store.manual_order.contains(&card_id(1, 1)));
    }

    // AC-1: filtering to one group's windows excludes the other group's cards
    // entirely.
    #[test]
    fn ordered_ids_for_windows_excludes_other_group() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(2, 1), 1, "b"));

        let group_a: HashSet<SessionWindowId> = [SessionWindowId(1)].into_iter().collect();
        assert_eq!(store.ordered_ids_for_windows(&group_a), vec![card_id(1, 1)]);
    }

    // AC-2: multiple SessionWindowIds belonging to the same logical group
    // (native tabs) all appear in the filtered result.
    #[test]
    fn ordered_ids_for_windows_includes_every_window_in_the_set() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(1, 1), 1, "a"));
        store.apply(upsert(card_id(2, 1), 1, "b"));
        store.apply(upsert(card_id(3, 1), 1, "c"));

        let windows: HashSet<SessionWindowId> = [SessionWindowId(1), SessionWindowId(2)]
            .into_iter()
            .collect();
        assert_eq!(
            store.ordered_ids_for_windows(&windows),
            vec![card_id(2, 1), card_id(1, 1)]
        );
    }

    // AC-3: the filtered order matches ordered_ids's relative order (attention
    // float still wins over the plain window/pane sort).
    #[test]
    fn ordered_ids_for_windows_preserves_ordered_ids_relative_order() {
        let mut store = SessionStore::new();
        store.apply(upsert(card_id(2, 1), 1, "b"));
        store.apply(upsert(card_id(1, 3), 1, "a3"));
        store.apply(upsert(card_id(1, 1), 1, "a1"));
        store.apply(SessionDelta::Attention { id: card_id(2, 1) });

        let windows: HashSet<SessionWindowId> = [SessionWindowId(1), SessionWindowId(2)]
            .into_iter()
            .collect();
        assert_eq!(
            store.ordered_ids_for_windows(&windows),
            vec![card_id(2, 1), card_id(1, 1), card_id(1, 3)]
        );
    }

    // AC-6: counts_for_windows returns (0, 0) when the busy/attention cards
    // only exist in a different group's windows.
    #[test]
    fn counts_for_windows_is_zero_when_activity_is_in_another_group() {
        let mut store = SessionStore::new();
        store.apply(SessionDelta::Upsert {
            id: card_id(2, 1),
            seq: 1,
            name: "busy".to_string(),
            cwd: "/repo".to_string(),
            busy: true,
            updated_at: wall(10, 0),
            preview: None,
        });
        store.apply(SessionDelta::Attention { id: card_id(2, 1) });
        store.apply(upsert(card_id(1, 1), 1, "idle"));

        let group_a: HashSet<SessionWindowId> = [SessionWindowId(1)].into_iter().collect();
        assert_eq!(store.counts_for_windows(&group_a), (0, 0));

        let group_b: HashSet<SessionWindowId> = [SessionWindowId(2)].into_iter().collect();
        assert_eq!(store.counts_for_windows(&group_b), (1, 1));
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
