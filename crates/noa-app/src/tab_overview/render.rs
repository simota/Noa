use std::time::{Duration, Instant};

/// Input row for pure thumbnail-regeneration selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRenderCandidate<Id> {
    pub id: Id,
    pub dirty: bool,
    pub last_render_at: Option<Instant>,
}

/// Rendering mode selected for an overview tile under resource pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewTileMode {
    LiveThumbnail,
    Placeholder,
}

/// Injected GPU lifecycle signal used by the resource-regeneration decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewResourceEvent {
    None,
    DeviceLost,
    SurfaceLost,
}

/// Decide whether a single tile is dirty and outside the compile-time
/// regeneration throttle.
pub fn should_render_tile(
    dirty: bool,
    last_render_at: Option<Instant>,
    now: Instant,
    min_interval: Duration,
) -> bool {
    if !dirty {
        return false;
    }
    let Some(last_render_at) = last_render_at else {
        return true;
    };
    now.saturating_duration_since(last_render_at) >= min_interval
}

/// Select the dirty-and-due tile ids for one overview frame.
///
/// Source-window occlusion must NOT gate this selection: tabs mirrored in the
/// overview are almost always occluded (they sit behind the overview window
/// itself and/or in a macOS native tab group), so filtering them out would
/// leave every live tile permanently blank and defeat REQ-OV-4's live mirror.
/// REQ-NF-7's occlusion-aware redraw suppression is honored at the tab-window
/// redraw layer (`TargetedRedrawDecision`) instead, which the overview tile
/// path does not bypass.
pub fn select_due_overview_tile_ids<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
    max_tiles: usize,
) -> Vec<Id> {
    candidates
        .iter()
        .filter(|candidate| {
            should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval)
        })
        .take(max_tiles)
        .map(|candidate| candidate.id)
        .collect()
}

/// Outcome of the post-frame dirty-backlog check `redraw_overview` runs
/// after each Session Overview frame (Fix A): either an immediate redraw is
/// warranted right now, or — if every remaining dirty tile is merely
/// throttle-blocked — the single instant at which the earliest one becomes
/// due, so the caller can schedule one delayed wake-up instead of spinning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewBacklogDecision {
    pub request_immediate_redraw: bool,
    pub wake_at: Option<Instant>,
}

/// Decide the post-frame backlog action from each source tile's dirty +
/// last-render state.
///
/// A tile only warrants `request_immediate_redraw` when it is dirty *and*
/// already due (i.e. [`should_render_tile`] would render it right now) —
/// that only happens when [`OVERVIEW_MAX_RENDER_TILES_PER_FRAME`] left it
/// un-rendered this frame. A tile that is merely dirty-but-throttled
/// contributes its throttle deadline (`last_render_at + min_interval`, or
/// `now` if it has never been rendered) to `wake_at`, and the earliest one
/// wins: one delayed wake-up covers every throttled tile, since a tile that
/// becomes due re-triggers this same check when it fires.
pub fn overview_backlog_decision<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
) -> OverviewBacklogDecision {
    let mut wake_at: Option<Instant> = None;
    for candidate in candidates {
        if !candidate.dirty {
            continue;
        }
        if should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval) {
            return OverviewBacklogDecision {
                request_immediate_redraw: true,
                wake_at: None,
            };
        }
        let due_at = candidate
            .last_render_at
            .map(|last_render_at| last_render_at + min_interval)
            .unwrap_or(now);
        wake_at = Some(wake_at.map_or(due_at, |current| current.min(due_at)));
    }
    OverviewBacklogDecision {
        request_immediate_redraw: false,
        wake_at,
    }
}

/// Decide the tile mode from an injected VRAM budget flag.
pub fn overview_tile_mode_for_budget(budget_exceeded: bool) -> OverviewTileMode {
    if budget_exceeded {
        OverviewTileMode::Placeholder
    } else {
        OverviewTileMode::LiveThumbnail
    }
}

/// Decide whether overview GPU resources must be regenerated.
pub fn overview_regen_required(event: OverviewResourceEvent) -> bool {
    matches!(
        event,
        OverviewResourceEvent::DeviceLost | OverviewResourceEvent::SurfaceLost
    )
}
