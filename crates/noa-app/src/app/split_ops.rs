//! Split-pane operations: creating panes, moving focus, resizing, equalizing,
//! and toggling split zoom.

use super::*;

/// Pane-dnd P3 (review round 6): move a single entry from `old` to `new` in
/// a `SessionCardId`-keyed side table, alongside the session-store's own
/// card rekey (`SessionStore::rekey`) — shared by `move_pane_to_tab_at` for
/// `App::attention_flash_until` so its transient visual entry follows the card
/// instead of being silently dropped by `reconcile_session_store`'s liveness
/// GC on the next tick. A no-op (map untouched) when `old` isn't present, e.g.
/// a card that never received attention.
fn rekey_card_entry<V>(
    map: &mut HashMap<SessionCardId, V>,
    old: SessionCardId,
    new: SessionCardId,
) {
    if let Some(value) = map.remove(&old) {
        map.insert(new, value);
    }
}

/// Pane-dnd P2-2 (review round 5): whether `action` is one of the four
/// per-pane [`ConfirmAction`] variants and carries exactly `pane_id`. Used
/// by [`App::move_pane_to_tab_at`] to decide whether an open confirm dialog
/// must be dismissed ahead of a pane move — the other four variants
/// (`AttachRemote`, `CloseTab`, `CloseWindow`, `Quit`) carry no pane id at
/// all, so they can never reference a specific pane and always return
/// `false`.
fn confirm_action_references_pane(action: &ConfirmAction, pane_id: PaneId) -> bool {
    match action {
        ConfirmAction::RetryDetachedRemote { pane_id: p, .. }
        | ConfirmAction::Paste { pane_id: p, .. }
        | ConfirmAction::ClipboardRead { pane_id: p, .. }
        | ConfirmAction::ClosePane { pane_id: p, .. } => *p == pane_id,
        ConfirmAction::AttachRemote { .. }
        | ConfirmAction::CloseTab { .. }
        | ConfirmAction::CloseWindow { .. }
        | ConfirmAction::Quit => false,
    }
}

/// Pane-dnd P2-2 (review round 8): whether an open send-selection picker
/// still references `(window_id, pane_id)` — either as the source it was
/// opened from, or as one of its `SendSelectionTarget` candidates. Used by
/// [`App::move_pane_to_tab_at`] to decide whether the picker must be cancelled
/// ahead of a cross-tab pane move: after the move the pane no longer lives
/// in `window_id`, so a later Enter would look it up under the wrong window
/// and silently drop the selected text.
fn send_selection_picker_references_pane(
    session: &SendSelectionPickerSession,
    window_id: WindowId,
    pane_id: PaneId,
) -> bool {
    (session.window_id == window_id && session.source_pane == pane_id)
        || session
            .targets
            .iter()
            .any(|target| target.window_id == window_id && target.pane_id == pane_id)
}

/// The pure result of a successful [`cross_tab_move`]: the two transformed
/// trees plus the source-side removal decision (focus/tab-close), ready for
/// the caller to commit.
struct CrossTabMove {
    source_tree: SplitTree,
    dest_tree: SplitTree,
    remove_outcome: split_tree::RemoveOutcome,
}

/// Pure cross-tree half of a cross-tab pane move (pane-dnd FR-8, L2(e)): the
/// single-tree [`move_pane`] (`reposition.rs`) can't do this directly since
/// `moved` and `target` live in two different `SplitTree`s here, so this
/// composes the same two primitives (`extract_pane` + `split_pane_in_direction`)
/// across both trees instead.
///
/// `dest_target`/`dest_direction` (Overview U3) name *where* in `dest` the pane
/// lands: it is split-inserted at `dest_target`'s `dest_direction`-side edge.
/// The sidebar-card drop and the no-target Overview drop both pass
/// `(dest_focused, Direction::Right)` — the original "right of the focused
/// pane" behavior — while an Overview drop onto a specific pane in another
/// tab passes that pane and the drop zone's direction (edge zones) or
/// `Direction::Right` (a center-zone drop, which inserts to the target's
/// right; cross-tab swap is out — the engine is one-way).
///
/// **Atomicity**, mirroring `move_pane`'s own clone-then-commit contract
/// (Omen A3): every check happens before either input tree is touched, and
/// both transforms run against private clones — a rejection (either cap)
/// returns `None` with `source`/`dest` byte-for-byte unchanged (AC-28).
///
/// **Cap contract**, mirroring `move_pane`'s (`reposition.rs`): `tab_cap_ok`
/// must be computed by the caller against `dest_target`'s current rect
/// (`app::helpers::geometry::can_create_split`), since this pure tree layer
/// has no pixel geometry of its own; the axis cap
/// (`MAX_PANES_PER_AXIS`) is checked here via `can_add_pane_in_direction`.
///
/// What this function does **not** decide (the caller's job, since it isn't
/// a tree property): the window-group filter (AC-29).
fn cross_tab_move(
    source: &SplitTree,
    pane: PaneId,
    dest: &SplitTree,
    dest_target: PaneId,
    dest_direction: Direction,
    tab_cap_ok: bool,
) -> Option<CrossTabMove> {
    if !tab_cap_ok || !can_add_pane_in_direction(dest, dest_target, dest_direction) {
        return None;
    }
    let mut source_tree = source.clone();
    let remove_outcome = split_tree::extract_pane(&mut source_tree, pane);

    let mut dest_tree = dest.clone();
    if !split_pane_in_direction(&mut dest_tree, dest_target, pane, dest_direction) {
        return None;
    }

    Some(CrossTabMove {
        source_tree,
        dest_tree,
        remove_outcome,
    })
}

/// P1 remediation (pane-dnd review round 4): decide the *source* tab's
/// `focused_pane` after `move_pane_to_tab_at` detaches `moved` from it. The
/// moved pane's own focus (if it had any) passes to its sibling per
/// `remove_outcome.next_focus` — but a user looking at some *other* pane in
/// the same tab must keep looking at it: reassigning focus unconditionally
/// yanked keystrokes to the moved pane's neighbor even when the user's own
/// focused pane never moved. `focused_before == moved` is exactly the
/// "the moved pane held focus" case the reassignment exists for; anything
/// else is a no-op returning `focused_before` untouched.
fn source_focus_after_cross_tab_move(
    focused_before: PaneId,
    moved: PaneId,
    next_focus: Option<PaneId>,
    still_present: impl Fn(PaneId) -> bool,
    any_remaining: Option<PaneId>,
) -> PaneId {
    if focused_before != moved {
        return focused_before;
    }
    next_focus
        .filter(|candidate| still_present(*candidate))
        .or(any_remaining)
        .unwrap_or(focused_before)
}

impl App {
    pub(super) fn new_split(&mut self, window_id: WindowId, direction: Direction) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some((focused_pane, new_pane, focused_rect, auto_approve_enabled, redraw_floor)) =
            self.windows.get_mut(&window_id).and_then(|state| {
                let focused_rect = state.focused_surface()?.rect;
                if !can_create_split_in_direction(state.pane_count(), focused_rect, direction)
                    || !can_add_pane_in_direction(&state.split_tree, state.focused_pane, direction)
                {
                    return None;
                }
                let new_pane = PaneId::alloc();
                Some((
                    state.focused_pane,
                    new_pane,
                    focused_rect,
                    state.auto_approve_enabled.clone(),
                    state.redraw_floor.clone(),
                ))
            })
        else {
            return;
        };

        let grid_size = grid_size_for_pane_rect(focused_rect, gpu.font.metrics(), self.padding);
        let inherited_cwd = self.pane_cwd(window_id, focused_pane);
        let new_surface = match self.spawn_pane_surface(
            window_id,
            new_pane,
            grid_size,
            focused_rect,
            inherited_cwd,
            auto_approve_enabled,
            redraw_floor,
        ) {
            Ok(surface) => surface,
            Err(err) => {
                log::warn!("failed to spawn split pty: {err}");
                return;
            }
        };

        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            };
            if !split_pane_in_direction(&mut state.split_tree, focused_pane, new_pane, direction) {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            }
            state.surfaces.insert(new_pane, new_surface);
            state.focused_pane = new_pane;
            state.zoomed = None;
            state.last_mouse_pane = Some(new_pane);
            state.window.clone()
        };

        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.persist_session();
    }

    pub(super) fn focus_split_direction(&mut self, window_id: WindowId, direction: Direction) {
        let Some(next) = self.windows.get(&window_id).and_then(|state| {
            focus_in_direction(&state.split_tree, state.focused_pane, direction)
                .filter(|pane| state.contains_pane(*pane))
        }) else {
            return;
        };
        self.focus_pane(window_id, next);
    }

    /// Cross-tab pane move with an explicit destination insertion target
    /// (Overview U3). `dest_target` names *where* in `dest_window` the pane
    /// lands: `None` keeps the sidebar-drop behavior (to the right of
    /// `dest_window`'s currently-focused pane), while `Some((target_pane,
    /// direction))` split-inserts the pane at `target_pane`'s `direction`-side
    /// edge — an Overview edge-zone drop passes the zone's direction, a
    /// center-zone drop passes `Direction::Right` (insert to the target's
    /// right; cross-tab swap is out, the engine is one-way).
    ///
    /// Returns `false` (no-op, both trees/surface maps/the session store left
    /// byte-for-byte unchanged) when: `source_window == dest_window`; `pane`
    /// isn't live in `source_window`; `dest_window` doesn't exist or belongs
    /// to a different window group (AC-29); the named `dest_target` pane isn't
    /// live in `dest_window`; or either pane-count cap would be exceeded
    /// (AC-28, via [`cross_tab_move`]).
    ///
    /// `PaneId` is a process-global, never-reused id ([`PaneId::alloc`],
    /// `split_tree/tree/types.rs`) — every tab's first pane used to be
    /// literally `PaneId(1)` before this Track (`WindowState::next_pane_id`
    /// restarted at 2 per tab), which made `dest` already holding a pane
    /// numerically equal to `pane` the common case, not an edge case. Now
    /// that allocation is global, `dest` holding a pane with the same id as
    /// `pane` (still live in `source`) is structurally impossible, so the
    /// insert below is asserted rather than checked.
    pub(super) fn move_pane_to_tab_at(
        &mut self,
        event_loop: &ActiveEventLoop,
        source_window: WindowId,
        pane: PaneId,
        dest_window: WindowId,
        dest_target: Option<(PaneId, Direction)>,
    ) -> bool {
        if source_window == dest_window {
            return false;
        }
        let Some(source_group) = self.windows.get(&source_window).map(|state| state.group) else {
            return false;
        };
        if !self
            .windows
            .get(&source_window)
            .is_some_and(|state| state.contains_pane(pane))
        {
            return false;
        }
        let Some(dest_state) = self.windows.get(&dest_window) else {
            return false;
        };
        // AC-29: cross-tab move is confined to the drag source's own window
        // group — a different group is never a valid drop target.
        if dest_state.group != source_group {
            return false;
        }
        // See this method's doc comment: with `PaneId` allocation now global,
        // `dest` cannot already hold a pane with `pane`'s id while `pane` is
        // still live in `source`.
        debug_assert!(
            !dest_state.contains_pane(pane),
            "PaneId is process-global; dest must never already contain the moved pane's id"
        );
        // Resolve the insertion target: an explicit Overview target (U3), or
        // the sidebar-drop default of "right of the destination's focused
        // pane". A named target that no longer lives in `dest` rejects the
        // whole move rather than silently falling back.
        let (dest_target_pane, dest_direction) =
            dest_target.unwrap_or((dest_state.focused_pane, Direction::Right));
        if dest_target.is_some() && !dest_state.contains_pane(dest_target_pane) {
            return false;
        }
        let Some(dest_rect) = dest_state
            .surfaces
            .get(&dest_target_pane)
            .map(|surface| surface.rect)
        else {
            return false;
        };
        let tab_cap_ok = can_create_split(
            dest_state.pane_count(),
            dest_rect,
            dest_direction.split_orientation(),
        );
        // P2-1/P2-4: captured now, while `dest_state` is still borrowed here,
        // for the re-attach below — the moved pane's io thread must gate
        // auto-approve production and coalesce redraws against the
        // *destination* tab's flag/clock, not the source tab's (which the
        // pane's `Surface` is about to be detached from).
        let dest_auto_approve_enabled = dest_state.auto_approve_enabled.clone();
        let dest_redraw_floor = dest_state.redraw_floor.clone();

        let Some(source_tree) = self
            .windows
            .get(&source_window)
            .map(|state| &state.split_tree)
        else {
            return false;
        };
        let Some(dest_tree) = self
            .windows
            .get(&dest_window)
            .map(|state| &state.split_tree)
        else {
            return false;
        };
        let Some(transform) = cross_tab_move(
            source_tree,
            pane,
            dest_tree,
            dest_target_pane,
            dest_direction,
            tab_cap_ok,
        ) else {
            return false;
        };

        // P2-2 (pane-dnd review round 4): the move is now guaranteed to
        // commit, so tear down the moved pane's own per-pane transient state
        // before its `Surface` detaches — mirrors `close_pane`'s identical
        // teardown of the same two states (`end_copy_mode_for_pane` +
        // `search_prompt`). Both are keyed by `(window_id, pane_id)` but
        // gated for keyboard routing on `window_id` alone (`event_loop.rs`'s
        // `KeyboardInput` handler), so leaving either bound to the moved pane
        // would keep it swallowing every keystroke in the *source* window
        // (which survives this move) for a target that's now gone — the same
        // dead-Surface soft-lock `close_pane` already guards against.
        //
        // Audited the rest of the close path's per-pane-adjacent state for
        // the same shape: `tab_title_prompt` and `modal_preedit` key only on
        // `window_id` (no `pane_id` at all), so a same-window pane move can
        // never strand them — no action needed.
        // #TODO(agent): `close_pane` still doesn't tear down `sidebar_rename`/
        // `confirm_dialog` at single-pane granularity — that gap predates
        // this move code and isn't introduced by it, and is a separate task.
        // The move side (below) is now handled.
        self.end_copy_mode_for_pane(source_window, pane);
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == source_window && session.pane_id == pane)
        {
            self.search_prompt = None;
        }
        // P2-2 (pane-dnd review round 5): a confirm dialog or inline sidebar
        // rename bound to the moved pane must not survive the move. If the
        // source tab survives, its later Enter/click would resolve the
        // dialog's `ConfirmAction`/the rename's `SessionCardId` against
        // `(source_window, pane)` — a pane that's now either living in
        // `dest_window` (a silent no-op action, or a rename applied to the
        // wrong pane) or, if the source tab closed, already discarded
        // mid-input by `close_tab`. Both are dismissed/cancelled, never
        // executed: a paste/clipboard-read/close firing against a pane
        // that just relocated is surprising regardless of which tab it
        // lands in, and a rename mid-flight silently following its card to
        // another tab would be just as surprising as losing it — cancel
        // matches the confirm-dialog treatment on both counts, so this
        // deliberately does not attempt to re-key either one to
        // `dest_window`.
        if self.confirm_dialog.as_ref().is_some_and(|session| {
            session.window_id == source_window
                && confirm_action_references_pane(&session.action, pane)
        }) {
            self.confirm_dialog = None;
            self.request_window_redraw(source_window);
        }
        if self
            .sidebar_rename
            .as_ref()
            .is_some_and(|session| session.card == Self::session_card_id(source_window, pane))
        {
            self.cancel_sidebar_rename();
        }
        // P2-2 (pane-dnd review round 8): a send-selection picker that still
        // references the moved pane — as its own source, or among its target
        // candidates — must not survive the move. `commit_send_selection_
        // picker_target` resolves the chosen `SendSelectionTarget` by looking
        // the pane up under its captured `window_id`; once the pane relocates
        // to `dest_window`, that lookup misses in `source_window` and the
        // selected text is silently dropped. Re-keying the candidate to
        // `dest_window` in place would leave its display label — built at open
        // time from the pane's tab/pane *indices* via the full
        // `inter_pane_targets_in_group` traversal — stale, and that traversal
        // can't even run here yet (the dest tree isn't committed until below),
        // so rebuilding the label would duplicate significant open-time logic
        // rather than the "cheap and self-contained" recompute an in-place
        // update would need. Cancel outright instead, matching the
        // confirm-dialog and rename treatment above.
        if self.send_selection_picker.as_ref().is_some_and(|session| {
            send_selection_picker_references_pane(session, source_window, pane)
        }) {
            self.send_selection_picker = None;
            self.request_window_redraw(source_window);
        }
        // P2-3 (pane-dnd review round 9): an `Attach Remote` retry overlay
        // pinned to the moved pane must not survive the move. Its Enter
        // re-resolves the pane under `source_window`, which after the move
        // either misses (source tab survives → "Remote pane is no longer
        // available") or dangles on a window `close_tab` already tore down
        // (source tab closed). Dismissed via the same `remote_ui = None`
        // path Esc uses, matching the confirm-dialog/rename/send-picker
        // teardown above; only the pane-pinned `Retry` phase is affected.
        self.dismiss_remote_ui_for_moved_pane(source_window, pane);
        // P2-1 (pane-dnd review round 7): a Cmd+hover link tracked on the
        // moved pane must not survive the move either. `sync_hover_link`
        // only ever revisits `App.hovered_link`'s previous `(window, pane)`
        // on a *later* `CursorMoved`/`ModifiersChanged` in `source_window`,
        // but after this move `pane` no longer lives there at all — no such
        // event can ever land on it again to trigger that clear. Left alone,
        // `App.hovered_link` would keep pointing at a dead `(source_window,
        // pane)` pair, and — the actually-visible half — the moved Surface's
        // own `hover_link` (its underline + the pointer cursor it drives)
        // would carry over into `dest_window` with nothing there to ever
        // invalidate it, since nothing in `dest_window` currently points
        // back at this pane as "the" hovered one. Clear both explicitly,
        // mirroring the same-pane-disappears clear `sync_hover_link` already
        // performs when its tracked target vanishes.
        if self.hovered_link == Some((source_window, pane)) {
            self.hovered_link = None;
        }
        if let Some(state) = self.windows.get_mut(&source_window)
            && let Some(surface) = state.surfaces.get_mut(&pane)
        {
            surface.hover_link = None;
        }
        self.update_cursor_icon(source_window);

        // P2 (pane-dnd review round 12): the moved pane is leaving an
        // OS-focused source window while still being that window's
        // focus-reported pane, and it lands in a hidden destination tab.
        // `report_focus_event` only fires on `WindowEvent::Focused(false)`,
        // which never happens here — the source window keeps OS focus
        // throughout the move — so a focus-reporting TUI in the moved pane
        // would keep believing it's focused while invisible. Emit the FocusOut
        // now, before the Surface detaches below, reusing the exact
        // mode-gate + encoding the `Focused(false)` path uses
        // (`pane_owns_keyboard_focus` + `focus_report_bytes`); a pane with
        // focus reporting off gets nothing. The symmetric FocusIn for when the
        // destination window later gains OS focus is delivered by that window's
        // own `Focused(true)` handler (the moved pane is dest's focused pane by
        // then), so nothing is sent to it here.
        let os_focused = self.os_focused;
        if let Some(source_state) = self.windows.get(&source_window)
            && pane_owns_keyboard_focus(source_window, pane, os_focused, source_state.focused_pane)
        {
            let focus_reporting = source_state
                .surfaces
                .get(&pane)
                .is_some_and(|surface| surface.terminal.lock().modes.focus_reporting());
            if let Some(bytes) = focus_report_bytes(false, focus_reporting) {
                self.write_pane_pty_bytes(source_window, pane, bytes);
            }
        }

        // Commit: trees, zoom, Surface, focus, then the io-thread window-id
        // cell and the session-store re-key (L2(e)/FR-12).
        let Some(source_state) = self.windows.get_mut(&source_window) else {
            return false;
        };
        // P1 (pane-dnd review round 7): if the moved pane held the source
        // tab's focus with a live IME composition, end it here — *before*
        // the Surface detaches below. Left alone, the moved Surface carries
        // its stale `preedit` into `dest_window` (able to swallow its very
        // next keystroke there, `keyboard_preedit_should_swallow_key`), and
        // the source window's OS-level composition session survives the
        // focus flip to a sibling pane a few lines down — so the next
        // `Ime::Commit` the OS delivers to `source_window` would land on
        // that sibling instead of the (correctly) discarded moved pane.
        // Mirrors `focus_pane`'s identical local-clear + OS toggle, which
        // exists for the same reason on an ordinary in-window focus switch.
        if source_state.focused_pane == pane
            && let Some(surface) = source_state.surfaces.get_mut(&pane)
            && surface.ime_state.preedit_active()
        {
            surface.ime_state.commit_preedit();
            surface.auto_approve_guards.lock().ime_preedit_active = false;
            source_state.window.set_ime_allowed(false);
            source_state.window.set_ime_allowed(true);
        }
        // Atomicity (mirrors `move_pane`'s own clone-then-commit contract,
        // L2(e)): verify/remove the Surface *before* committing the
        // transformed tree, so a (currently unreachable, since `pane`'s
        // presence was already checked above) removal failure can never
        // leave `source_state` half-committed.
        let Some(mut surface) = source_state.surfaces.remove(&pane) else {
            return false;
        };
        source_state.split_tree = transform.source_tree;
        if source_state.zoomed == Some(pane) {
            source_state.zoomed = None;
        }
        if !transform.remove_outcome.tab_should_close {
            // P1: only steal focus when the moved pane itself held it —
            // otherwise the pane the user is actually looking at must keep
            // it, untouched (see `source_focus_after_cross_tab_move`).
            let resolved_focus = source_focus_after_cross_tab_move(
                source_state.focused_pane,
                pane,
                transform.remove_outcome.next_focus,
                |candidate| source_state.contains_pane(candidate),
                source_state.surfaces.keys().copied().next(),
            );
            if resolved_focus != source_state.focused_pane {
                source_state.focused_pane = resolved_focus;
                source_state.last_mouse_pane = Some(source_state.focused_pane);
            }
        }
        let source_window_handle = source_state.window.clone();

        // L2(e): the destination window id lands in the shared cell before
        // the Surface becomes visible under `dest`'s key, so no event from
        // this pane's io thread racing the insert can still target the old
        // window — `Ordering::Release` pairs with every send site's
        // `Ordering::Acquire` load (`io_thread::spawn::current_card_target`).
        //
        // P1-2 (only `Local` has a cell to store into): a `Remote` pane's
        // `WinitConnectionNotifier` (`remote_attach.rs`) bakes its `window_id`
        // in once at attach time and is never re-pointed here — converting it
        // to a shared cell too would need a second seam (the notifier is
        // cloned into a background connection-manager thread, not owned by
        // this `Surface`). That gap is deliberately left unclosed at the
        // source: `App::resolve_pane_window` (`event_loop.rs`) re-resolves
        // every pane-scoped `UserEvent` — including a `Remote` pane's
        // `Redraw`/`Bell`, the only two kinds `WinitConnectionNotifier` ever
        // sends — to whichever window currently holds the pane at *receive*
        // time, so a stale baked-in id here never produces a stale-routed
        // event; it's a no-op for `Remote`, not a rejected move.
        if let SurfaceTransport::Local(local) = &surface.transport {
            local
                .io_window_id
                .store(u64::from(dest_window), Ordering::Release);
            // P2-1: re-point the moved pane's auto-approve producer at the
            // destination tab's flag. A one-time value copy would not be
            // enough (a later toggle of either tab's checkbox would then be
            // invisible to this pane), so this swaps the *holder*'s contents
            // — the io thread re-reads through `AutoApprovePublish::enabled`
            // on every batch, so it starts following the destination flag,
            // including its future toggles, from the very next feed.
            *local.auto_approve_flag.lock() = dest_auto_approve_enabled.clone();
            // P2-4: same re-attach, for the pane's redraw-pacing clock — see
            // `RedrawFloorHandle`. Without this the moved pane keeps
            // coalescing redraws against the source tab's (now orphaned)
            // clock: a second independent floor ticking inside one tab
            // (extra frames), and one that stops following monitor-refresh
            // changes forever once the source tab closes.
            *local.redraw_floor.lock() = dest_redraw_floor.clone();
        }

        let Some(dest_state) = self.windows.get_mut(&dest_window) else {
            // Unreachable: nothing between the checks above and here can
            // remove `dest_window` — this whole method runs synchronously on
            // the main thread. Still don't strand a live Pty on the floor.
            surface.shutdown();
            return false;
        };
        dest_state.split_tree = transform.dest_tree;
        // FR-6: force-unzoom whenever the destination *tab* has any pane
        // zoomed, not only when it's exactly `dest_focused` — a zoomed pane
        // fills the whole tab regardless of which pane is zoomed, so any
        // zoom state there must be cleared before the insert becomes visible.
        dest_state.zoomed = None;
        // P2-2 (review round 9): the moved Surface still carries the
        // `last_mouse_cell` it captured at the *source* window's drag-start
        // pointer position. The drop gesture that triggered this move happens
        // over the source window's sidebar, so the pointer is not over
        // `dest_window` at all — clear the stale cell and leave dest's
        // `last_mouse_pane` unset rather than pinning it to the moved pane.
        // Without this, a click or wheel in `dest_window` before any
        // `CursorMoved` there (which is what repopulates both) would route
        // through the moved pane and report against a cell resolved from a
        // different window's geometry. The mouse-input fallback already
        // resolves `last_mouse_pane.or(focused_pane)`, so `None` here simply
        // defers to the (correctly) focused moved pane with no stale cell.
        surface.last_mouse_cell = None;
        dest_state.surfaces.insert(pane, surface);
        // P1 (review round 13): force `dest`'s renderer to fully rebuild this
        // pane's rows on its next frame. `PaneId` is process-global and never
        // reused ([`PaneId::alloc`]), so `dest`'s per-window `pane_render_cache`
        // can only hold an entry for this exact id if this same pane lived here
        // before and was moved away (a return trip) — that entry is never
        // evicted when a pane leaves a window. While the pane was in the other
        // tab, output drawn there read (and cleared) the terminal's row damage,
        // so with unchanged grid size/theme the next `rebuild_panes` here would
        // trust its stale `row_dirty` bits, reuse the stale cached rows, and
        // never show the output produced while away. `invalidate_pane` bypasses
        // `row_dirty` entirely for one rebuild, reading every row straight from
        // the fresh snapshot. The renderer is a plain (non-`Option`) field on
        // `WindowState`, so no None guard is needed. Only the INSERT side needs
        // this: the source renderer's now-orphaned entry for `pane` is never
        // rebuilt (the pane is gone from its `surfaces`), and the only path that
        // can resurface that id is this same pane arriving somewhere — which is
        // exactly this insert-side invalidation (a return to `source` inserts
        // here with `source` as `dest`). No other path can put a pre-existing
        // cache id back into a window: split creates fresh (never-cached) ids.
        dest_state.renderer.invalidate_pane(render_pane_id(pane));
        dest_state.focused_pane = pane;
        dest_state.last_mouse_pane = None;
        let dest_window_handle = dest_state.window.clone();

        // FR-12: the moved pane's sidebar card follows to its new window.
        self.session_store.rekey(
            Self::session_card_id(source_window, pane),
            Self::session_card_id(dest_window, pane),
        );
        // P2 (review round 13): an open process-monitor overlay still holds the
        // moved pane's row under its old `(source_window, pane)` card id after
        // the store rekey above — Enter would raise the old window and focus a
        // pane no longer there. Rekey the overlay's held rows/selection now so
        // the selection follows the moved pane instead of waiting for the next
        // metrics tick. A no-op when the overlay is closed.
        self.rekey_process_monitor_card(
            Self::session_card_id(source_window, pane),
            Self::session_card_id(dest_window, pane),
        );
        // The moved pane's one-shot attention emphasis follows it too. This
        // table lives on `App`, outside the rekeyed session store, so preserve
        // its original deadline instead of ending or restarting the effect.
        rekey_card_entry(
            &mut self.attention_flash_until,
            Self::session_card_id(source_window, pane),
            Self::session_card_id(dest_window, pane),
        );
        rekey_card_entry(
            &mut self.progress_flashes,
            Self::session_card_id(source_window, pane),
            Self::session_card_id(dest_window, pane),
        );
        // P3 (review round 9): the moved pane's auto-approve flash follows it
        // too, for the same reason as `attention_flash_until` above — it's keyed by
        // the same `SessionCardId` the store rekey just changed, but lives on
        // `App`. Without this rekey the sidebar model looks the flash up under
        // the card's *new* id (`sidebar/model.rs`), misses it, and the flash
        // dies early at the destination while a stale entry lingers at the old
        // id until it times out. Preserves the original deadline `Instant` so
        // the flash's remaining duration carries over unchanged.
        rekey_card_entry(
            &mut self.auto_approve_flash_until,
            Self::session_card_id(source_window, pane),
            Self::session_card_id(dest_window, pane),
        );
        // P2-1: the rekeyed card kept its own fields across the rekey above
        // — including `auto_approve_enabled`, still mirroring the *source*
        // tab's flag — so sync it to the destination tab's current value
        // right away rather than leaving it stale until the next sidebar
        // `Upsert` tick. Mirrors every other per-window toggle site
        // (`set_auto_approve_for_window`); harmlessly re-syncs the
        // destination's other cards to the value they already have.
        self.session_store.set_auto_approve_for_window(
            SessionWindowId(u64::from(dest_window)),
            dest_auto_approve_enabled.load(Ordering::Relaxed),
        );

        // P2-1: move the pane's IPC registrations (minted registry id + raw
        // attach registration) to the new window key alongside the
        // session-store rekey above — otherwise the next
        // `sync_ipc_snapshot` tick sees no live pane at the old key, prunes
        // it, and mints a *new* ipc id at the new key, severing any live
        // raw-attach client and changing the pane's wire-visible identity.
        self.ipc_shared.lock().rekey_pane(
            (u64::from(source_window), pane.get()),
            (u64::from(dest_window), pane.get()),
        );

        // P2-5: move the foreground-process probe registration too — it's
        // keyed by the same `SessionCardId` the session-store rekey above
        // just changed, so without this the probe silently drops out of the
        // next `retain_process_probes` GC (the old id is no longer live) and
        // nothing re-registers it under the new one.
        if let Some(branch_poll) = self.branch_poll.as_ref() {
            branch_poll.rekey_process_probe(
                Self::session_card_id(source_window, pane),
                Self::session_card_id(dest_window, pane),
            );
        }

        if transform.remove_outcome.tab_should_close {
            // FR-2/FR-8/ASSUME-9's window-group filter means a same-group
            // destination tab always exists whenever this move was valid, so
            // `source_window` can never be its group's last tab here — the
            // `TabCloseOutcome::Quit` app-exit cascade is unreachable
            // (L2(e)), and `close_tab` only ever tears down this one window.
            self.close_tab(event_loop, source_window);
        } else {
            self.relayout_and_resize_window(source_window);
            // P2-2: the source tab's focus moved to `next_focus`/a fallback
            // pane above — its IME candidate window must follow, exactly
            // like an in-tab reposition. Skipped when the tab closed: there
            // is no `WindowState` left for it to anchor to.
            self.update_focused_ime_cursor_area(source_window);
            source_window_handle.request_redraw();
        }
        self.relayout_and_resize_window(dest_window);
        // P2-2: the moved pane now lives (and is focused) in `dest_window` —
        // its IME candidate window must anchor there, not linger at the
        // source tab's pre-move position.
        self.update_focused_ime_cursor_area(dest_window);
        dest_window_handle.request_redraw();
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.reconcile_session_store();
        self.persist_session();
        true
    }

    pub(super) fn focus_pane(&mut self, window_id: WindowId, pane_id: PaneId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if !state.contains_pane(pane_id) || state.focused_pane == pane_id {
            return;
        }
        self.end_copy_mode_for_window(window_id);
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let losing = state.focused_pane;
        let losing_preedit = state
            .surfaces
            .get(&losing)
            .is_some_and(|surface| surface.ime_state.preedit_active());
        let plan = focus_switch_plan(losing, pane_id);

        if let Some(state) = self.windows.get_mut(&window_id) {
            for op in plan {
                match op {
                    ImeOp::CommitPreedit(pane) => {
                        if let Some(surface) = state.surfaces.get_mut(&pane) {
                            surface.ime_state.commit_preedit();
                        }
                    }
                    ImeOp::RetargetIme(pane) => {
                        if state.contains_pane(pane) {
                            state.focused_pane = pane;
                            state.last_mouse_pane = Some(pane);
                        }
                    }
                }
            }
            // The OS-level composition session survives our local clear —
            // without this, the IME keeps composing and its next Preedit
            // lands on the newly focused pane. Toggling IME off/on discards
            // the marked text so the new pane starts clean.
            if losing_preedit {
                state.window.set_ime_allowed(false);
                state.window.set_ime_allowed(true);
            }
        }
        self.update_focused_ime_cursor_area(window_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn resize_focused_split(&mut self, window_id: WindowId, direction: Direction) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            resize_split(
                &mut state.split_tree,
                state.focused_pane,
                direction,
                SPLIT_RESIZE_STEP_PX,
            );
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    pub(super) fn equalize_splits(&mut self, window_id: WindowId) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            equalize(&mut state.split_tree);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    pub(super) fn toggle_split_zoom(&mut self, window_id: WindowId) {
        let bounds = self.window_pane_bounds(window_id);
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            let decision = zoom_toggle(&state.split_tree, state.zoomed, state.focused_pane, bounds);
            state.zoomed = decision.zoomed;
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-2 (pane-dnd review round 8): the send-selection picker is treated
    /// as referencing the moved pane when it is the picker's own source, when
    /// it is one of the target candidates, and never when neither matches (a
    /// same-id pane in a *different* window must not trigger a cancel).
    #[test]
    fn send_selection_picker_references_pane_matches_source_and_targets() {
        let source_window = WindowId::from(1u64);
        let other_window = WindowId::from(2u64);
        let source_pane = PaneId::new(10);
        let target_pane = PaneId::new(11);
        let unrelated_pane = PaneId::new(12);

        let session = |targets: Vec<SendSelectionTarget>| SendSelectionPickerSession {
            window_id: source_window,
            source_pane,
            selected_text: "hi".to_string(),
            targets,
            selected: 0,
            opened_at: std::time::Instant::now(),
        };
        let target = |window_id, pane_id| SendSelectionTarget {
            window_id,
            pane_id,
            label: String::new(),
        };

        // Matches its own source pane.
        assert!(send_selection_picker_references_pane(
            &session(vec![]),
            source_window,
            source_pane
        ));
        // Matches a target candidate.
        assert!(send_selection_picker_references_pane(
            &session(vec![target(source_window, target_pane)]),
            source_window,
            target_pane
        ));
        // No match for an unrelated pane.
        assert!(!send_selection_picker_references_pane(
            &session(vec![target(source_window, target_pane)]),
            source_window,
            unrelated_pane
        ));
        // Same pane id in a different window is not a match (window is part
        // of the key).
        assert!(!send_selection_picker_references_pane(
            &session(vec![target(source_window, target_pane)]),
            other_window,
            source_pane
        ));
    }

    /// P2-2 (pane-dnd review round 5): each of the four per-pane
    /// `ConfirmAction` variants matches only its own `pane_id`, never a
    /// different one.
    #[test]
    fn confirm_action_references_pane_matches_only_own_pane_id_for_per_pane_variants() {
        let pane = PaneId::new(1);
        let other = PaneId::new(2);
        let window_id = WindowId::from(1u64);

        let retry_detached_remote = ConfirmAction::RetryDetachedRemote {
            window_id,
            pane_id: pane,
            endpoint: crate::remote_attach::RemoteEndpoint::parse("127.0.0.1:1234")
                .expect("valid loopback endpoint"),
        };
        assert!(confirm_action_references_pane(&retry_detached_remote, pane));
        assert!(!confirm_action_references_pane(
            &retry_detached_remote,
            other
        ));

        let paste = ConfirmAction::Paste {
            window_id,
            pane_id: pane,
            text: "hi".to_string(),
            then_enter: false,
        };
        assert!(confirm_action_references_pane(&paste, pane));
        assert!(!confirm_action_references_pane(&paste, other));

        let clipboard_read = ConfirmAction::ClipboardRead {
            window_id,
            pane_id: pane,
            target: "clipboard".to_string(),
        };
        assert!(confirm_action_references_pane(&clipboard_read, pane));
        assert!(!confirm_action_references_pane(&clipboard_read, other));

        let close_pane = ConfirmAction::ClosePane {
            window_id,
            pane_id: pane,
        };
        assert!(confirm_action_references_pane(&close_pane, pane));
        assert!(!confirm_action_references_pane(&close_pane, other));
    }

    /// P2-2: the pane-less variants never reference any pane, regardless of
    /// which id is queried.
    #[test]
    fn confirm_action_references_pane_is_always_false_for_pane_less_variants() {
        let pane = PaneId::new(1);
        let window_id = WindowId::from(1u64);

        assert!(!confirm_action_references_pane(
            &ConfirmAction::AttachRemote {
                window_id,
                endpoint: crate::remote_attach::RemoteEndpoint::parse("127.0.0.1:1234")
                    .expect("valid loopback endpoint"),
            },
            pane
        ));
        assert!(!confirm_action_references_pane(
            &ConfirmAction::CloseTab { window_id },
            pane
        ));
        assert!(!confirm_action_references_pane(
            &ConfirmAction::CloseWindow {
                group: WindowGroupId(1)
            },
            pane
        ));
        assert!(!confirm_action_references_pane(&ConfirmAction::Quit, pane));
    }

    // Pane-dnd cross-tab move (`docs/specs/pane-dnd.md` FR-8) — `cross_tab_move`
    // (tree half) plus the Surface-transfer identity proof. `App::move_pane_to_tab_at`
    // itself needs a real `WindowState` (a real winit `Window` + `wgpu::Surface`),
    // which this sandboxed offline test environment cannot construct, so these
    // tests exercise the pure/GPU-free seams the App method is built from.

    /// AC-11 (tree half): the moved pane detaches from `source` (a single-pane
    /// tree — `tab_should_close` fires) and split-inserts to the right of
    /// `dest`'s focused pane.
    #[test]
    fn cross_tab_move_detaches_source_and_inserts_right_of_dest_focused() {
        let moved = PaneId::new(1);
        let dest_focused = PaneId::new(1); // pure tree layer: reusing `1` here is fine, it doesn't allocate
        let dest_other = PaneId::new(2);
        let source = SplitTree::leaf(moved);
        let dest = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(dest_focused),
            SplitTree::leaf(dest_other),
        );

        let transform = cross_tab_move(&source, moved, &dest, dest_focused, Direction::Right, true)
            .expect("axis and tab caps both pass");

        assert!(transform.remove_outcome.tab_should_close);
        let mut expected_dest = dest.clone();
        assert!(split_pane_in_direction(
            &mut expected_dest,
            dest_focused,
            moved,
            Direction::Right
        ));
        assert_eq!(transform.dest_tree, expected_dest);
        // The source input is untouched — `cross_tab_move` only ever produces
        // a new tree, it never mutates its `&SplitTree` inputs.
        assert_eq!(source, SplitTree::leaf(moved));
    }

    /// P1 (pane-dnd review round 4): when the moved pane was NOT the source
    /// tab's focused pane, `App::move_pane_to_tab_at` must not steal focus —
    /// `source_focus_after_cross_tab_move` returns `focused_before`
    /// untouched, ignoring `next_focus` entirely, even though a valid
    /// successor exists.
    #[test]
    fn source_focus_after_cross_tab_move_leaves_a_differently_focused_pane_alone() {
        let moved = PaneId::new(1);
        let focused = PaneId::new(2);
        let sibling = PaneId::new(3);

        let resolved = source_focus_after_cross_tab_move(
            focused,
            moved,
            Some(sibling),
            |candidate| candidate == focused || candidate == sibling,
            Some(focused),
        );

        assert_eq!(
            resolved, focused,
            "focus must stay on the pane the user was actually looking at"
        );
    }

    /// P1 companion: when the moved pane WAS focused, focus must still pass
    /// to `next_focus` exactly as before (the reassignment this guard
    /// protects, not a case it disables).
    #[test]
    fn source_focus_after_cross_tab_move_follows_next_focus_when_the_moved_pane_was_focused() {
        let moved = PaneId::new(1);
        let sibling = PaneId::new(2);

        let resolved = source_focus_after_cross_tab_move(
            moved,
            moved,
            Some(sibling),
            |candidate| candidate == sibling,
            Some(sibling),
        );

        assert_eq!(resolved, sibling);
    }

    /// P1 edge case: the moved pane was focused, but `next_focus` no longer
    /// exists (already re-filtered elsewhere) — falls back to
    /// `any_remaining`, mirroring the call site's `.or_else(surfaces.keys()
    /// .next())` chain.
    #[test]
    fn source_focus_after_cross_tab_move_falls_back_when_next_focus_is_stale() {
        let moved = PaneId::new(1);
        let stale_next = PaneId::new(2);
        let fallback = PaneId::new(3);

        let resolved = source_focus_after_cross_tab_move(
            moved,
            moved,
            Some(stale_next),
            |_candidate| false,
            Some(fallback),
        );

        assert_eq!(resolved, fallback);
    }

    /// AC-28 (per-tab cap half): `dest` already at `MAX_PANES_PER_TAB` rejects
    /// the whole move (`tab_cap_ok=false`, computed via the real
    /// `can_create_split` geometry helper) — neither tree is touched.
    #[test]
    fn cross_tab_move_rejects_when_dest_at_per_tab_cap() {
        let moved = PaneId::new(9);
        let dest_focused = PaneId::new(1);
        let source = SplitTree::leaf(moved);
        let dest = SplitTree::leaf(dest_focused);

        let rect = PaneRectApp::new(0, 0, 400, 400);
        let tab_cap_ok = can_create_split(
            MAX_PANES_PER_TAB,
            rect,
            Direction::Right.split_orientation(),
        );
        assert!(
            !tab_cap_ok,
            "a tab already at MAX_PANES_PER_TAB must reject further inserts"
        );

        assert!(
            cross_tab_move(
                &source,
                moved,
                &dest,
                dest_focused,
                Direction::Right,
                tab_cap_ok
            )
            .is_none()
        );
    }

    /// AC-28 (axis cap half): `dest_focused` already sits in a 3-pane
    /// horizontal row (`MAX_PANES_PER_AXIS`) rejects the move even when
    /// `tab_cap_ok=true` — mirrors `split_tree::tests::
    /// move_pane_axis_cap_exceeded_is_rejected_and_tree_unchanged`'s tree
    /// shape for the single-tree case.
    #[test]
    fn cross_tab_move_rejects_when_axis_cap_exceeded() {
        let moved = PaneId::new(9);
        let first = PaneId::new(1);
        let second = PaneId::new(2);
        let third = PaneId::new(3);
        let source = SplitTree::leaf(moved);
        let dest = SplitTree::split(
            SplitOrientation::Horizontal,
            1.0 / 3.0,
            SplitTree::leaf(first),
            SplitTree::split_even(
                SplitOrientation::Horizontal,
                SplitTree::leaf(second),
                SplitTree::leaf(third),
            ),
        );

        assert!(cross_tab_move(&source, moved, &dest, third, Direction::Right, true).is_none());
    }

    /// Overview U3 (position targeting): a cross-tab move onto a *specific*
    /// destination pane inserts the moved pane at that pane's edge, not at the
    /// destination tab's focused pane. Here the target is `dest_other` with a
    /// `Down` edge zone, so the result must match a direct `Down`-split at
    /// `dest_other` — proving `cross_tab_move` honors the passed
    /// target/direction rather than the old hardcoded "right of focused".
    #[test]
    fn cross_tab_move_inserts_at_the_named_target_pane_and_direction() {
        let moved = PaneId::new(1);
        let dest_focused = PaneId::new(1); // pure tree layer: reusing `1` is fine, no allocation
        let dest_other = PaneId::new(2);
        let source = SplitTree::leaf(moved);
        let dest = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(dest_focused),
            SplitTree::leaf(dest_other),
        );

        let transform = cross_tab_move(&source, moved, &dest, dest_other, Direction::Down, true)
            .expect("axis and tab caps both pass");

        let mut expected_dest = dest.clone();
        assert!(split_pane_in_direction(
            &mut expected_dest,
            dest_other,
            moved,
            Direction::Down
        ));
        assert_eq!(transform.dest_tree, expected_dest);
        // A center-zone drop resolves to the same target with `Right`, which
        // must differ from the `Down` edge result above (distinct insertion).
        let center = cross_tab_move(&source, moved, &dest, dest_other, Direction::Right, true)
            .expect("axis and tab caps both pass");
        assert_ne!(center.dest_tree, transform.dest_tree);
    }

    /// AC-12 (shape): the exact one-line transfer `App::move_pane_to_tab_at`
    /// performs (`dest.surfaces.insert(pane, source.surfaces.remove(&pane))`)
    /// moves the key across the two maps and preserves the moved `Surface`'s
    /// `Terminal` identity — proof it's the same live pane, not a respawned
    /// one. Built via `SurfaceTransport::Remote` (no pty/GPU needed) since a
    /// `Local` transport's `PtyWriter`/`IoThreadHandle` require a real pty,
    /// which this sandboxed test environment cannot spawn; the transfer
    /// mechanics under test here don't depend on which transport variant is
    /// carried.
    #[test]
    fn surface_transfer_moves_key_and_preserves_terminal_identity() {
        let pane = PaneId::new(1);
        let grid_size = GridSize::new(80, 24);
        let rect = PaneRectApp::new(0, 0, 800, 480);
        let terminal = Arc::new(Mutex::new(Terminal::new(grid_size)));
        let surface = Surface::new(
            terminal.clone(),
            SurfaceTransport::Remote(RemoteSurfaceTransport {
                identity: crate::remote_attach::RemotePaneIdentity {
                    endpoint: "test:1".to_string(),
                    pane_id: 1,
                    cached_title: None,
                },
                state: Arc::new(Mutex::new(
                    crate::remote_attach::RemoteAttachState::connected(),
                )),
                connection: None,
                card_seq: 1,
            }),
            grid_size,
            rect,
            Arc::new(Mutex::new(
                crate::auto_approve::AutoApproveInputGuards::default(),
            )),
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
        );

        let mut source: HashMap<PaneId, Surface> = HashMap::new();
        source.insert(pane, surface);
        let mut dest: HashMap<PaneId, Surface> = HashMap::new();

        dest.insert(pane, source.remove(&pane).expect("present in source"));

        assert!(
            !source.contains_key(&pane),
            "no ghost Surface left under the source key"
        );
        let moved = dest.get(&pane).expect("present in dest under the same key");
        assert!(
            Arc::ptr_eq(&moved.terminal, &terminal),
            "same Terminal allocation, not a respawned one"
        );
    }

    // P3 (pane-dnd review round 6): the value under the old key moves to the
    // new key, unchanged — proving the attention onset's `Instant` timestamp
    // (not just presence) carries over, so the blink's remaining duration is
    // preserved rather than restarted.
    #[test]
    fn rekey_card_entry_moves_value_to_new_key_when_old_key_present() {
        let old = SessionCardId::new(SessionWindowId(1), PaneId::new(1));
        let new = SessionCardId::new(SessionWindowId(2), PaneId::new(1));
        let onset = Instant::now();
        let mut map: HashMap<SessionCardId, Instant> = HashMap::new();
        map.insert(old, onset);

        rekey_card_entry(&mut map, old, new);

        assert!(
            !map.contains_key(&old),
            "old key must not remain (no stale entry)"
        );
        assert_eq!(
            map.get(&new).copied(),
            Some(onset),
            "the exact same Instant must move to the new key, not a fresh one"
        );
    }

    // P3: a card with no onset entry (never received attention) must not
    // spuriously create one at the new key — `reconcile_session_store`'s GC
    // already handles "no entry" correctly; this rekey must not disturb that.
    #[test]
    fn rekey_card_entry_is_noop_when_old_key_absent() {
        let old = SessionCardId::new(SessionWindowId(1), PaneId::new(1));
        let new = SessionCardId::new(SessionWindowId(2), PaneId::new(1));
        let mut map: HashMap<SessionCardId, Instant> = HashMap::new();

        rekey_card_entry(&mut map, old, new);

        assert!(map.is_empty(), "must not fabricate an entry at the new key");
    }
}
