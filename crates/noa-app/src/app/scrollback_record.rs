use super::*;

use std::collections::HashSet;

use crate::record_view;
use crate::scrollback_persist as store;

/// How long a pane must be quiet before an idle checkpoint captures it.
/// Long enough that a burst of output settles first, short enough that a
/// crash rarely costs more than the last few seconds.
pub(super) const CHECKPOINT_QUIESCENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Ceiling on the gap between checkpoints. Quiescence alone would never fire
/// under sustained output — exactly the long-running build whose tail the user
/// most wants back — so a flood still gets checkpointed this often.
pub(super) const CHECKPOINT_MAX_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Floor on how soon the ceiling may fire, so a burst arriving after a long
/// idle still gets a moment to settle instead of being captured mid-stream.
const CHECKPOINT_MIN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Scrollback persistence (`scrollback-persist`): capture each pane's tail to
/// disk, restore it on launch as a marked record region, and keep the snapshot
/// directory inside its budgets.
///
/// Spec: `docs/specs/scrollback-persistence.md`.
impl App {
    /// Whether scrollback should be captured at all.
    ///
    /// Gated on `window-save-state` as well as `scrollback-persist`: the key
    /// that makes a snapshot reachable is written by `persist_session`, which
    /// is a no-op while session state is disabled. Capturing anyway would write
    /// terminal output to disk that nothing can ever restore — cost with no
    /// benefit, which is the worst trade available for this feature.
    pub(super) fn scrollback_persist_enabled(&self) -> bool {
        self.config.scrollback_persist.persists()
            && self.config.window_save_state.restores()
            && self.scrollback_persister.is_some()
    }

    /// Mint this pane's snapshot key if it does not have one yet, and return
    /// it. Keys are stable for the pane's life so repeated checkpoints
    /// overwrite one file.
    fn scrollback_key_for(&mut self, window_id: WindowId, pane: PaneId) -> Option<String> {
        let counter = {
            self.scrollback_key_counter = self.scrollback_key_counter.wrapping_add(1);
            self.scrollback_key_counter
        };
        let surface = self.windows.get_mut(&window_id)?.surfaces.get_mut(&pane)?;
        if surface.scrollback_key.is_none() {
            surface.scrollback_key = Some(store::mint_key(counter));
        }
        surface.scrollback_key.clone()
    }

    /// Every pane eligible for persistence, as `(window, pane)`.
    ///
    /// Walks `window_order` rather than `windows`, which excludes the scratch
    /// terminal and the quick terminal for free — they are deliberately kept
    /// out of that list, and a deliberately disposable popup is the last thing
    /// that should leave its output on disk. Remote panes are skipped too:
    /// their contents belong to the machine serving them.
    fn persistable_panes(&self) -> Vec<(WindowId, PaneId)> {
        let mut out = Vec::new();
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            for (pane, surface) in &state.surfaces {
                if surface.is_remote() {
                    continue;
                }
                out.push((*window_id, *pane));
            }
        }
        out
    }

    /// Encode and queue a snapshot for every eligible pane.
    ///
    /// `dirty_only` skips panes that produced no output since their last
    /// capture — the idle checkpoint's normal mode. Quit passes `false` so the
    /// final state of every pane lands even if it has been quiet.
    pub(super) fn capture_scrollback_snapshots(&mut self, dirty_only: bool) {
        if !self.scrollback_persist_enabled() {
            return;
        }
        let limit = self.config.scrollback_persist_limit;
        if limit == 0 {
            // Not "skip this round": a zero budget is the user saying to retain
            // nothing, and leaving the previous file in place would restore
            // output captured before they said so.
            self.purge_scrollback_snapshots();
            return;
        }
        let saved_at = store::now_unix();

        for (window_id, pane) in self.persistable_panes() {
            if dirty_only
                && !self
                    .windows
                    .get(&window_id)
                    .and_then(|state| state.surfaces.get(&pane))
                    .is_some_and(|surface| surface.scrollback_dirty)
            {
                continue;
            }
            let Some(key) = self.scrollback_key_for(window_id, pane) else {
                continue;
            };
            let Some(surface) = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane))
            else {
                continue;
            };
            let encoded = {
                let terminal = surface.terminal.lock();
                // A stale index would exclude an unrelated *live* row from the
                // record instead of the separator.
                let annotation = (surface.record_generation
                    == terminal.grid_coordinate_generation())
                .then_some(surface.annotation_row)
                .flatten();
                terminal.scrollback_snapshot_bytes(limit, saved_at, annotation)
            };
            surface.scrollback_dirty = false;
            let Some(persister) = self.scrollback_persister.as_ref() else {
                continue;
            };
            match encoded {
                Some(bytes) => persister.save(key, bytes),
                // A pane with nothing to show must not restore last week's
                // output: drop any snapshot it previously wrote.
                None => persister.discard(key),
            }
        }
        self.last_scrollback_checkpoint = Some(Instant::now());
    }

    /// Command entry point: capture every pane now.
    ///
    /// Pairs the capture with `persist_session` for the same reason the timer
    /// does — a key minted here but absent from `session.json` is an orphan the
    /// next launch's collector deletes, which is precisely the crash this
    /// command is invoked to survive.
    pub(super) fn checkpoint_scrollback_now(&mut self) {
        self.capture_scrollback_snapshots(false);
        self.persist_session();
    }

    /// Note that `pane` produced output, so the next checkpoint captures it.
    pub(super) fn mark_scrollback_dirty(&mut self, window_id: WindowId, pane: PaneId) {
        if !self.scrollback_persist_enabled() {
            return;
        }
        if let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane))
        {
            surface.scrollback_dirty = true;
        }
        self.scrollback_dirty_since.get_or_insert_with(Instant::now);
        self.arm_scrollback_checkpoint();
    }

    /// (Re-)arm the idle checkpoint: normally [`CHECKPOINT_QUIESCENCE`] after
    /// the last output, but never later than [`CHECKPOINT_MAX_INTERVAL`] past
    /// the previous checkpoint, so sustained output cannot starve it.
    fn arm_scrollback_checkpoint(&mut self) {
        let now = Instant::now();
        let quiescent = now + CHECKPOINT_QUIESCENCE;
        // Anchor the ceiling on the start of the current dirty streak, not on
        // the last checkpoint: before the very first one there is no last
        // checkpoint, and a pane emitting faster than the quiescence window
        // would otherwise push its deadline forever and never be captured at
        // all — exactly the long first build whose tail matters most.
        let anchor = *self
            .scrollback_dirty_since
            .get_or_insert(self.last_scrollback_checkpoint.unwrap_or(now));
        // Clamp from below as well: after a long idle the ceiling is already in
        // the past, and firing on the first byte of a new burst is the mid-burst
        // stall the quiescence window exists to avoid.
        let ceiling = (anchor + CHECKPOINT_MAX_INTERVAL).max(now + CHECKPOINT_MIN_GRACE);
        self.scrollback_checkpoint_deadline = Some(quiescent.min(ceiling));
    }

    /// Fire the idle checkpoint when due. Returns the next deadline for
    /// `about_to_wait`'s control-flow calculation, mirroring the other tickers.
    pub(super) fn tick_scrollback_checkpoint(&mut self) -> Option<Instant> {
        let deadline = self.scrollback_checkpoint_deadline?;
        if Instant::now() < deadline {
            return Some(deadline);
        }
        self.scrollback_checkpoint_deadline = None;
        self.scrollback_dirty_since = None;
        self.capture_scrollback_snapshots(true);
        // A checkpoint can mint a pane's first key. Without re-writing the
        // topology, a crash would leave `session.json` claiming that pane has
        // no snapshot and the collector would delete the file we just wrote —
        // exactly the crash the checkpoint exists to survive.
        self.persist_session();
        None
    }

    /// Delete snapshots no saved session references, plus anything expired or
    /// over budget. Run at launch *before* restore, so a record the user asked
    /// to expire is never shown and then deleted behind them.
    pub(super) fn collect_scrollback_snapshots(&self, referenced: HashSet<String>) {
        let Some(dir) = noa_config::scrollback_dir() else {
            return;
        };
        if !dir.exists() {
            return;
        }
        // With persistence off, nothing is referenced and the whole directory
        // drains on the next launch.
        let (total_limit, max_age) = if self.scrollback_persist_enabled() {
            (
                self.config.scrollback_persist_total_limit as u64,
                self.config.scrollback_persist_max_age_days,
            )
        } else {
            (0, 0)
        };
        store::collect(&dir, &referenced, total_limit, max_age);
    }

    /// Push a restored record into a freshly spawned pane, followed by the
    /// separator that marks where live output begins.
    ///
    /// `key` is the leaf's saved snapshot key, if it had one. When there is no
    /// record to show — persistence off, no key, a missing or corrupt file —
    /// the pane instead gets the Stage 0 notice, because a restored layout
    /// with an empty pane is a silent broken promise.
    pub(super) fn restore_pane_record(
        &mut self,
        window_id: WindowId,
        pane: PaneId,
        key: Option<String>,
    ) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane))
        else {
            return;
        };
        if surface.is_remote() {
            return;
        }
        let cols = surface.grid_size.cols;

        let snapshot = key
            .as_deref()
            .filter(|_| self.scrollback_persist_enabled())
            .and_then(|key| {
                let dir = noa_config::scrollback_dir()?;
                store::read(&dir, key)
            })
            .and_then(|bytes| noa_grid::snapshot::decode(&bytes));

        let (mut history, hyperlinks, saved_at) = match snapshot {
            Some(snapshot) => (snapshot.rows, snapshot.hyperlinks, Some(snapshot.saved_at)),
            None => (Vec::new(), Vec::new(), None),
        };
        match saved_at {
            Some(saved_at) => history.push(record_view::separator_row(
                saved_at,
                crate::localtime::local_offset_seconds(),
                cols,
            )),
            None => history.push(record_view::not_persisted_notice_row(cols)),
        }

        let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane))
        else {
            return;
        };
        let record_rows = {
            let mut terminal = surface.terminal.lock();
            let inserted = terminal.restore_scrollback_snapshot(noa_grid::ScrollbackSnapshot {
                cols,
                saved_at: saved_at.unwrap_or(0),
                rows: history,
                // Carried through so the restored cells' OSC 8 links resolve
                // in *this* terminal's registry; dropping the table here
                // would silently unlink every restored link.
                hyperlinks,
            });
            (inserted > 0).then(|| {
                let start = terminal.active_oldest_row();
                start..start + inserted
            })
        };
        // The notice is a live annotation about *this* launch, not recovered
        // history — marking it as record would claim a record exists.
        surface.record_rows = saved_at.and(record_rows.clone());
        // Both the separator and the notice are the last row inserted.
        surface.annotation_row = record_rows.map(|rows| rows.end - 1);
        // Stamped *after* the insert, which bumps the generation itself.
        surface.record_generation = surface.terminal.lock().grid_coordinate_generation();
        // Restoring is not output; without this the pane would be captured
        // again immediately, rewriting the file it was just restored from.
        surface.scrollback_dirty = false;
        if let Some(key) = key
            && store::is_valid_key(&key)
        {
            surface.scrollback_key = Some(key);
        }
    }

    /// Command entry point: discard the focused pane's restored record.
    pub(super) fn discard_focused_pane_record(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        let Some(pane) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        if self.discard_pane_record(window_id, pane) {
            self.request_window_redraw(window_id);
        }
    }

    /// Drop the restored record from `pane`, leaving live output untouched.
    pub(super) fn discard_pane_record(&mut self, window_id: WindowId, pane: PaneId) -> bool {
        let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane))
        else {
            return false;
        };
        let Some(record) = surface.record_rows.take() else {
            surface.annotation_row = None;
            return false;
        };
        surface.annotation_row = None;
        {
            let mut terminal = surface.terminal.lock();
            if surface.record_generation != terminal.grid_coordinate_generation() {
                // The rows those indices named are gone; dropping that many rows
                // off the front now would take live history instead.
                return false;
            }
            terminal.discard_history_prefix(record.end.saturating_sub(record.start));
        }
        if let Some(key) = surface.scrollback_key.clone()
            && let Some(persister) = self.scrollback_persister.as_ref()
        {
            persister.discard(key);
        }
        true
    }

    /// Delete every record this session owns, now rather than at the next
    /// launch's collector.
    ///
    /// Someone who reacts to the privacy implication by turning the setting off
    /// means "stop keeping this", not "stop keeping it the next time you happen
    /// to start". Called on the `Tail -> Never` transition and when the budget
    /// is set to zero.
    pub(super) fn purge_scrollback_snapshots(&mut self) {
        let keys: Vec<String> = self
            .windows
            .values()
            .flat_map(|state| state.surfaces.values())
            .filter_map(|surface| surface.scrollback_key.clone())
            .collect();
        if let Some(persister) = self.scrollback_persister.as_ref() {
            for key in keys {
                persister.discard(key);
            }
        }
        for state in self.windows.values_mut() {
            for surface in state.surfaces.values_mut() {
                surface.scrollback_key = None;
                surface.scrollback_dirty = false;
            }
        }
        // Drop anything left from earlier runs too — nothing is referenced now.
        if let Some(dir) = noa_config::scrollback_dir()
            && dir.exists()
        {
            store::collect(&dir, &HashSet::new(), 0, 0);
        }
        self.persist_session();
    }

    /// Forget a record region once eviction has consumed it, so a pane that
    /// has scrolled past its restored history stops drawing a gutter for rows
    /// that are no longer there.
    pub(super) fn prune_record_regions(&mut self) {
        for state in self.windows.values_mut() {
            for surface in state.surfaces.values_mut() {
                if surface.record_rows.is_none() && surface.annotation_row.is_none() {
                    continue;
                }
                let (oldest, generation) = {
                    let terminal = surface.terminal.lock();
                    (
                        terminal.active_oldest_row(),
                        terminal.grid_coordinate_generation(),
                    )
                };
                if surface.record_generation != generation {
                    // A reflow or a scrollback clear renumbered every row: these
                    // indices no longer name anything.
                    surface.record_rows = None;
                    surface.annotation_row = None;
                    continue;
                }
                // The annotation is tracked even without a record (the Stage 0
                // notice has no record behind it), and a stale absolute index
                // would skip an unrelated live row from the next capture.
                if surface.annotation_row.is_some_and(|row| oldest > row) {
                    surface.annotation_row = None;
                }
                if let Some(record) = surface.record_rows.clone() {
                    if oldest >= record.end {
                        surface.record_rows = None;
                    } else if oldest > record.start {
                        surface.record_rows = Some(oldest..record.end);
                    }
                }
            }
        }
    }
}
