//! Session Overview publish path (Fix B, REQ-NF-6): the io thread
//! opportunistically drops a peek snapshot into a shared slot while it
//! already holds the terminal lock, throttled and with a trailing flush so a
//! burst's final frame is never stranded on a stale mirror.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use noa_grid::Terminal;
use noa_render::FrameSnapshot;

use crate::session_overview::OVERVIEW_TILE_MIN_RENDER_INTERVAL;

/// Read-only publish channel from `feed_terminal` to the Session Overview's
/// main-thread render path (Fix B, REQ-NF-6): the overview must never lock
/// a tab's `Arc<Mutex<Terminal>>` itself, so the io thread — which already
/// holds that lock on every pty feed — opportunistically drops a
/// `FrameSnapshot::peek` into `slot` here instead, at most once per
/// `OVERVIEW_TILE_MIN_RENDER_INTERVAL`. `visible` is shared app-wide
/// (`App::overview_visible_gate`) so an idle tab or a closed overview costs
/// this thread only one atomic load per pty feed.
#[derive(Clone)]
pub(crate) struct OverviewPublish {
    pub(crate) slot: Arc<Mutex<Option<Arc<FrameSnapshot>>>>,
    pub(crate) visible: Arc<AtomicBool>,
}

/// Pure decision for whether an overview publish should fire now, stay
/// silent, or fire later (Fix B defect 1). Not visible means nothing is
/// owed at all — reopening the overview re-peeks every tab unconditionally
/// (`App::seed_overview_snapshots`, Fix B defect 2), so a stale skip here
/// is never left stranded. Visible-but-throttled means the tab's current
/// state must still reach the mirror once the throttle window elapses,
/// even with no further pty output to re-trigger this decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OverviewPublishDecision {
    Skip,
    Publish,
    ScheduleTrailingFlush { deadline: Instant },
}

pub(super) fn decide_overview_publish(
    visible: bool,
    last_overview_publish: Option<Instant>,
    now: Instant,
) -> OverviewPublishDecision {
    if !visible {
        return OverviewPublishDecision::Skip;
    }
    let Some(last) = last_overview_publish else {
        return OverviewPublishDecision::Publish;
    };
    if now.saturating_duration_since(last) >= OVERVIEW_TILE_MIN_RENDER_INTERVAL {
        OverviewPublishDecision::Publish
    } else {
        OverviewPublishDecision::ScheduleTrailingFlush {
            deadline: last + OVERVIEW_TILE_MIN_RENDER_INTERVAL,
        }
    }
}

/// Opportunistically publish a read-only overview mirror snapshot while
/// `feed_terminal` already holds the `Terminal` lock (Fix B, REQ-NF-6).
/// Returns the trailing-flush deadline when [`decide_overview_publish`]
/// schedules one (see `spawn`'s dynamic-timeout `Select`), `None` otherwise.
pub(crate) fn publish_overview_snapshot(
    terminal: &Terminal,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
) -> Option<Instant> {
    let now = Instant::now();
    let visible = overview.visible.load(Ordering::Relaxed);
    match decide_overview_publish(visible, *last_overview_publish, now) {
        OverviewPublishDecision::Skip => None,
        OverviewPublishDecision::Publish => {
            let mut slot = overview.slot.lock();
            FrameSnapshot::refresh_peek_slot(&mut slot, terminal);
            *last_overview_publish = Some(now);
            None
        }
        OverviewPublishDecision::ScheduleTrailingFlush { deadline } => Some(deadline),
    }
}

/// Effectful trailing-edge flush (Fix B defect 1 — see
/// [`OverviewPublishDecision::ScheduleTrailingFlush`]): the throttle window
/// for the last skipped publish elapsed with no further pty output to
/// re-trigger `publish_overview_snapshot`, so publish the tab's *current*
/// terminal state now instead of leaving the overview mirror stuck on a
/// stale mid-burst frame (REQ-OV-4) until the tab's next output.
pub(super) fn flush_pending_overview_publish(
    terminal: &Arc<Mutex<Terminal>>,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
) {
    let now = Instant::now();
    let term = terminal.lock();
    let mut slot = overview.slot.lock();
    FrameSnapshot::refresh_peek_slot(&mut slot, &term);
    *last_overview_publish = Some(now);
}
