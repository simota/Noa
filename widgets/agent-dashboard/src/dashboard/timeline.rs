//! What the timeline tab (`gantt`) measures its lanes by.

use crate::fleet::{Worker, minutes_text};

/// What a full lane's grow factors add up to. Grow factors that add up to
/// less than 1 hand out only that fraction of the free space, so a lane is
/// scaled to this whatever its span (a span under a minute is under 1).
pub(super) const LANE_SHARES: f64 = 1000.0;

pub(super) fn span_text(span: f64) -> String {
    format!("← {}前　　いま →", minutes_text(span))
}

pub(super) fn timeline_span(workers: &[&Worker]) -> f64 {
    workers
        .iter()
        .map(|w| w.total_minutes())
        .fold(0.0, f64::max)
}
