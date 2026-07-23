use super::*;
use crate::auto_approve::{self, AutoApproveSignature, DetectContext};
use crate::io_thread::{AutoApproveFeedback, QueueInputResult};
use crate::session_store::{AutoApproveAuditEntry, SessionDelta};
use crate::sidebar::{agent_display_name, classify_agent};

const AUTO_APPROVE_FLASH_DURATION: Duration = Duration::from_millis(700);

impl App {
    pub(super) fn handle_auto_approve(
        &mut self,
        id: SessionCardId,
        signature: AutoApproveSignature,
        region_hash: u64,
        disable_after: bool,
    ) {
        let window_id = WindowId::from(id.window_id.0);
        let pane_id = id.pane_id;
        if self.is_quick_terminal_window(window_id) || self.is_scratch_terminal_window(window_id) {
            return;
        }

        let Some((feedback_tx, pane_live, auto_enabled)) =
            self.windows.get(&window_id).and_then(|state| {
                state.surfaces.get(&pane_id).and_then(|surface| {
                    let SurfaceTransport::Local(local) = &surface.transport else {
                        return None;
                    };
                    Some((
                        local.auto_approve_feedback_tx.clone(),
                        state.contains_pane(pane_id),
                        state.auto_approve_enabled.load(Ordering::Relaxed),
                    ))
                })
            })
        else {
            return;
        };
        let reject = || {
            let _ = feedback_tx.send(AutoApproveFeedback {
                signature,
                region_hash,
                accepted: false,
            });
        };
        if !pane_live || !auto_enabled {
            reject();
            return;
        }

        let Some(process) = self
            .session_store
            .get(&id)
            .and_then(|card| card.process.clone())
        else {
            reject();
            return;
        };
        if classify_agent(&process) != signature.agent() {
            reject();
            return;
        }

        let live_match = {
            let Some(surface) = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
            else {
                reject();
                return;
            };
            let terminal = surface.terminal.lock();
            let ctx = DetectContext {
                now: Instant::now(),
                alt_screen: terminal.active_is_alt,
                scrollback_offset: terminal.viewport_offset(),
                guards: *surface.auto_approve_guards.lock(),
            };
            let rows = auto_approve::viewport_rows_from_terminal(&terminal);
            let cursor = terminal.active().cursor;
            auto_approve::rescan_signature(
                &rows,
                signature,
                Point {
                    x: cursor.x,
                    y: cursor.y,
                },
                ctx,
            )
        };
        if live_match.is_none_or(|matched| matched.region_hash != region_hash) {
            reject();
            return;
        }

        match self.queue_pane_pty_bytes(window_id, pane_id, signature.bytes()) {
            QueueInputResult::Queued | QueueInputResult::Deferred => {}
            QueueInputResult::Dropped | QueueInputResult::Disconnected => {
                reject();
                return;
            }
        }
        let _ = feedback_tx.send(AutoApproveFeedback {
            signature,
            region_hash,
            accepted: true,
        });

        self.session_store.record_auto_approve(
            id,
            AutoApproveAuditEntry {
                at: crate::localtime::wall_clock_now(),
                agent: agent_display_name(signature.agent(), &process).to_string(),
                prompt: signature.label().to_string(),
            },
        );
        self.auto_approve_flash_until
            .insert(id, Instant::now() + AUTO_APPROVE_FLASH_DURATION);

        if disable_after {
            if let Some(state) = self.windows.get(&window_id) {
                state.auto_approve_enabled.store(false, Ordering::Relaxed);
            }
            self.session_store
                .set_auto_approve_for_window(id.window_id, false);
            self.sync_macos_auto_approve_menu_state(window_id);
            self.apply_session_delta(SessionDelta::Attention { id });
        }

        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        self.request_sidebar_redraw();
        self.request_window_redraw(window_id);
    }

    pub(super) fn toggle_auto_approve(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        if self.is_quick_terminal_window(window_id) || self.is_scratch_terminal_window(window_id) {
            return;
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let enabled = !state.auto_approve_enabled.load(Ordering::Relaxed);
        state.auto_approve_enabled.store(enabled, Ordering::Relaxed);
        self.session_store
            .set_auto_approve_for_window(SessionWindowId(u64::from(window_id)), enabled);
        self.sync_macos_auto_approve_menu_state(window_id);
        self.request_sidebar_redraw();
        self.request_window_redraw(window_id);
    }

    #[cfg(target_os = "macos")]
    pub(in crate::app) fn sync_macos_auto_approve_menu_state(&self, window_id: WindowId) {
        let Some(menu) = self.macos_menu.as_ref() else {
            return;
        };
        let enabled = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.auto_approve_enabled.load(Ordering::Relaxed));
        menu.set_auto_approve_checked(enabled);
    }

    #[cfg(not(target_os = "macos"))]
    pub(in crate::app) fn sync_macos_auto_approve_menu_state(&self, _window_id: WindowId) {}

    pub(in crate::app) fn mark_focused_pane_user_input(&mut self, window_id: WindowId) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.mark_pane_user_input(window_id, pane_id);
    }

    pub(in crate::app) fn mark_pane_user_input(&mut self, window_id: WindowId, pane_id: PaneId) {
        self.mark_pane_auto_approve_input(window_id, pane_id, false);
    }

    pub(in crate::app) fn mark_pane_paste_input(&mut self, window_id: WindowId, pane_id: PaneId) {
        self.mark_pane_auto_approve_input(window_id, pane_id, true);
    }

    fn mark_pane_auto_approve_input(&mut self, window_id: WindowId, pane_id: PaneId, paste: bool) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        let now = Instant::now();
        let mut guards = surface.auto_approve_guards.lock();
        if paste {
            guards.mark_paste(now);
        } else {
            guards.mark_user_input(now);
        }
    }
}
