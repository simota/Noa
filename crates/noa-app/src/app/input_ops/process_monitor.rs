//! Process-monitor overlay (`docs/specs/panel-metrics-view.md`) — the `App`
//! wiring half. Mirrors `theme_settings.rs`/`overlays.rs`'s command-palette
//! handling: a single app-wide session, a keyboard handler that consumes
//! every key while it owns the modal tier, and one close choke point
//! (`App::close_process_monitor`) so metrics collection always stops and
//! stale numbers never survive a reopen.

use super::super::*;
use super::ActiveOverlay;
use crate::process_monitor::{MonitorRow, ProcessMonitor};

impl App {
    /// FR-1: open/close the process-monitor overlay from the command
    /// palette. Opening binds it to the focused window with a fresh row
    /// snapshot and turns the branch-poll metrics tick on; re-firing while
    /// open closes it (mirrors `toggle_command_palette`). A no-op when
    /// another overlay already owns the focused window's keyboard (R-3).
    pub(in crate::app) fn toggle_process_monitor(&mut self) {
        if self.process_monitor.is_some() {
            self.close_process_monitor();
            return;
        }
        let Some(window_id) = self.focused else {
            return;
        };
        if self.active_overlay(window_id) != ActiveOverlay::None {
            return;
        }
        // Symmetric with the close path's clear: a `SessionDelta::Metrics`
        // already queued in the event loop at close time can land AFTER
        // `close_process_monitor`'s `clear_all_metrics()` and repopulate a
        // card — clearing again here guarantees a reopen never flashes the
        // previous session's ~1-tick-old values.
        self.session_store.clear_all_metrics();
        let rows = self.process_monitor_rows();
        self.process_monitor = Some(ProcessMonitorSession {
            window_id,
            state: ProcessMonitor::open(rows),
            opened_at: Instant::now(),
        });
        if let Some(branch_poll) = self.branch_poll.as_ref() {
            branch_poll.set_metrics_active(true);
        }
        self.request_window_redraw(window_id);
    }

    /// FR-2: build one row per live pane across every window/tab (the store's
    /// unfiltered order — the overlay is not scoped to one window's sidebar).
    fn process_monitor_rows(&self) -> Vec<MonitorRow> {
        crate::process_monitor::build_rows(&self.session_store.ordered_cards())
    }

    /// Re-sort/replace the open overlay's rows from the store (called on
    /// every `SessionDelta::Metrics` apply while open, `sidebar/state.rs`). A
    /// no-op when the overlay is closed.
    pub(in crate::app) fn refresh_process_monitor(&mut self) {
        let Some(window_id) = self
            .process_monitor
            .as_ref()
            .map(|session| session.window_id)
        else {
            return;
        };
        let rows = self.process_monitor_rows();
        if let Some(session) = self.process_monitor.as_mut() {
            session.state.refresh(rows);
        }
        self.request_window_redraw(window_id);
    }

    /// The single close choke point (open/close side effects, panel-metrics-view
    /// L2): turns the metrics tick back off and clears every card's metrics
    /// (so a reopen never flashes the previous session's stale numbers)
    /// before dropping the session. Called from Esc, the Enter-jump tail, and
    /// window teardown (`lifecycle.rs`) alike — a no-op when nothing is open.
    pub(in crate::app) fn close_process_monitor(&mut self) {
        let Some(session) = self.process_monitor.take() else {
            return;
        };
        if let Some(branch_poll) = self.branch_poll.as_ref() {
            branch_poll.set_metrics_active(false);
        }
        self.session_store.clear_all_metrics();
        self.request_window_redraw(session.window_id);
    }

    /// Drive the open overlay from a keypress (mirrors
    /// `handle_theme_settings_key`): Esc closes; ↑↓ move the selection; `s`
    /// cycles the sort key (FR-5); Enter jumps to the selected pane's
    /// window/tab/pane and closes (FR-6). Every other key is swallowed while
    /// this modal owns the keyboard (R-3 direction 2) — it is read-only, so
    /// no text ever needs to fall through. Only called when
    /// `self.process_monitor` targets `window_id` (checked by the caller,
    /// mirroring every other modal branch in `event_loop.rs`).
    pub(in crate::app) fn handle_process_monitor_key(
        &mut self,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_process_monitor();
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.process_monitor.as_mut() {
                    session.state.move_selection(-1);
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.process_monitor.as_mut() {
                    session.state.move_selection(1);
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::Enter) => self.jump_to_process_monitor_selection(),
            Key::Character(c)
                if c.eq_ignore_ascii_case("s")
                    && !self.modifiers.super_key()
                    && !self.modifiers.control_key()
                    && !self.modifiers.alt_key() =>
            {
                if let Some(session) = self.process_monitor.as_mut() {
                    session.state.cycle_sort();
                }
                self.request_window_redraw(window_id);
            }
            // Every other key (including any resolved keybind) is swallowed:
            // this is a read-only, modal overlay with no text input.
            _ => {}
        }
    }

    /// Enter (FR-6): jump to the selected row's window/tab/pane, focus it,
    /// and close the overlay — reusing the sidebar's own
    /// `SessionCardId` → `WindowId` conversion and focus sequence
    /// (`sidebar/interaction.rs`'s `focus_session_card`). Tolerates a
    /// selected id whose window has since vanished (closed mid-overlay): the
    /// `windows.get` guard below just closes without jumping instead of
    /// panicking.
    fn jump_to_process_monitor_selection(&mut self) {
        let selected = self
            .process_monitor
            .as_ref()
            .and_then(|session| session.state.selected_id());
        let Some(card_id) = selected else {
            self.close_process_monitor();
            return;
        };
        let target_window_id = WindowId::from(card_id.window_id.0);
        if let Some(window) = self
            .windows
            .get(&target_window_id)
            .map(|s| s.window.clone())
        {
            self.focus_pane(target_window_id, card_id.pane_id);
            self.focused = Some(target_window_id);
            window.focus_window();
        }
        self.close_process_monitor();
    }
}
