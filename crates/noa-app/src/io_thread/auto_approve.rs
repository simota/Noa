//! Auto-approve candidate detection: runs the detection matrix against the
//! terminal state the io thread already has locked, and schedules the
//! stability rescan that catches a prompt going static after its final pty
//! frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use noa_core::Point;
use noa_grid::Terminal;

use crate::auto_approve::{
    self, AutoApproveInputGuards, AutoApproveSignature, AutoApproveState, Decision, DetectContext,
};

#[derive(Clone)]
pub(crate) struct AutoApprovePublish {
    /// P2-1: a swappable holder rather than a bare `Arc<AtomicBool>` — a
    /// cross-tab pane move (`App::move_pane_to_tab_at`) re-points this at the
    /// destination tab's flag, and every read below goes through the lock so
    /// a later toggle of either tab's flag is always observed, not just the
    /// value at spawn/move time.
    pub(crate) enabled: Arc<Mutex<Arc<AtomicBool>>>,
    pub(crate) guards: Arc<Mutex<AutoApproveInputGuards>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutoApproveFeedback {
    pub(crate) signature: AutoApproveSignature,
    pub(crate) region_hash: u64,
    pub(crate) accepted: bool,
}

/// Delay for the second stability scan when a prompt becomes static after its
/// final pty frame. The scan stays event-driven: it is armed only after a first
/// matching prompt frame and cancelled when the prompt changes or is consumed.
const AUTO_APPROVE_STABILITY_RESCAN_DELAY: Duration = Duration::from_millis(350);

pub(super) struct AutoApproveCandidate {
    pub(super) signature: AutoApproveSignature,
    pub(super) region_hash: u64,
    pub(super) disable_after: bool,
}

pub(super) fn detect_auto_approve_candidate(
    term: &Terminal,
    publish: &AutoApprovePublish,
    state: &mut AutoApproveState,
) -> Option<AutoApproveCandidate> {
    if !publish.enabled.lock().load(Ordering::Relaxed) {
        state.reset_for_mode_off();
        return None;
    }
    let ctx = DetectContext {
        now: Instant::now(),
        alt_screen: term.active_is_alt,
        scrollback_offset: term.viewport_offset(),
        guards: *publish.guards.lock(),
    };
    let rows = auto_approve::viewport_rows_from_terminal(term);
    let cursor = term.active().cursor;
    let decision = auto_approve::detect_and_update_any_agent(
        &rows,
        Point {
            x: cursor.x,
            y: cursor.y,
        },
        ctx,
        state,
    );
    match decision {
        Decision::Fire {
            signature,
            region_hash,
            disable_after,
            ..
        } => Some(AutoApproveCandidate {
            signature,
            region_hash,
            disable_after,
        }),
        Decision::Hold | Decision::Suppressed(_) => None,
    }
}

pub(super) fn auto_approve_rescan_deadline(
    state: &AutoApproveState,
    now: Instant,
) -> Option<Instant> {
    state
        .needs_static_rescan()
        .then_some(now + AUTO_APPROVE_STABILITY_RESCAN_DELAY)
}
