//! IPC output push tap (noa-server spec FR-17 / NFR-2): lets a pane's io
//! thread hand its trailing viewport rows to a subscribed `noa-ipc` client
//! without ever blocking on it. `broadcast_output` is `try_send`-only and
//! never blocks (see `noa_ipc::push::Broadcaster`); `None` here (server
//! disabled, or the pane hasn't been registered with an IPC id yet) costs
//! nothing beyond the `Option` check.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use noa_grid::Terminal;
use noa_ipc::protocol::{Row, Span, SpanColor};

/// Minimum interval between `noa.output` pushes for one pane — dirty rows
/// coalesce at this cadence (spec NFR-4 "dirty 合流 ≥16ms").
pub(crate) const OUTPUT_PUSH_MIN_INTERVAL: Duration = Duration::from_millis(16);

/// Per-pane handle an io thread pushes output through, if the `noa-ipc`
/// server is running and this pane has been minted an IPC id.
#[derive(Clone)]
pub(crate) struct IpcOutputTap {
    pub(crate) broadcaster: noa_ipc::Broadcaster,
    pub(crate) ipc_pane_id: u64,
}

/// Decision for whether a `noa.output` push should fire now, stay silent, or
/// fire later (R-1: without the trailing branch, a burst's final feed
/// landing inside the 16ms throttle window was silently skipped with
/// nothing to ever resend it — subscribers never saw the tail of a burst).
/// Mirrors `overview::OverviewPublishDecision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IpcOutputPushDecision {
    Skip,
    Push,
    ScheduleTrailingFlush { deadline: Instant },
}

/// Pure throttle decision, mirroring `overview::decide_overview_publish`'s
/// now-as-param shape. `active` folds in the `ipc_active` gate (server
/// disabled, or this pane has no IPC id yet) so a disabled tap costs one
/// bool check and nothing else — same as before this decision type existed.
pub(super) fn decide_ipc_output_push(
    active: bool,
    last_push: Option<Instant>,
    now: Instant,
) -> IpcOutputPushDecision {
    if !active {
        return IpcOutputPushDecision::Skip;
    }
    match last_push {
        None => IpcOutputPushDecision::Push,
        Some(last) if now.saturating_duration_since(last) >= OUTPUT_PUSH_MIN_INTERVAL => {
            IpcOutputPushDecision::Push
        }
        Some(last) => IpcOutputPushDecision::ScheduleTrailingFlush {
            deadline: last + OUTPUT_PUSH_MIN_INTERVAL,
        },
    }
}

/// Diff the pane's current visible rows against `ipc_row_cache` (F-6),
/// returning only rows whose content hash changed since the last diff and
/// updating the cache in place. Shared by the normal per-feed push
/// (`feed_terminal_batch`) and the trailing-flush path
/// (`flush_pending_ipc_output`) so both compute the diff identically under
/// a `Terminal` lock hold rather than one of them caching a possibly-stale
/// snapshot.
pub(super) fn compute_ipc_row_diff(term: &Terminal, ipc_row_cache: &mut Vec<u64>) -> Vec<Row> {
    let base = term.active().visible_row_base() as u64;
    let rows = term.active().visible_rows();
    if ipc_row_cache.len() != rows.len() {
        ipc_row_cache.clear();
        ipc_row_cache.resize(rows.len(), 0);
    }
    let mut diff = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let spans = crate::ipc_bridge::row_to_spans(row);
        let hash = hash_wire_row_spans(&spans);
        if ipc_row_cache[i] != hash {
            ipc_row_cache[i] = hash;
            diff.push(Row { row: base + i as u64, spans });
        }
    }
    diff
}

/// Effectful trailing-edge flush (R-1, mirrors
/// `overview::flush_pending_overview_publish`): the throttle window for the
/// last skipped push elapsed with no further pty output to re-trigger
/// `feed_terminal_batch`'s own push, so recompute the diff from the pane's
/// *current* terminal state now (never resend a cached mid-burst diff — the
/// cache may itself be stale by the time the deadline fires) and push it if
/// non-empty.
pub(super) fn flush_pending_ipc_output(
    terminal: &Arc<Mutex<Terminal>>,
    tap: &IpcOutputTap,
    last_ipc_push: &mut Option<Instant>,
    ipc_row_cache: &mut Vec<u64>,
) {
    let now = Instant::now();
    let diff = {
        let term = terminal.lock();
        compute_ipc_row_diff(&term, ipc_row_cache)
    };
    *last_ipc_push = Some(now);
    if !diff.is_empty() {
        tap.broadcaster.broadcast_output(tap.ipc_pane_id, diff);
    }
}

/// A content hash of one wire row's spans (F-6: row diffing). Two calls with
/// unchanged text/fg/bg/attrs must hash equal, so a per-pane cache of these
/// (keyed by viewport slot) can skip resending rows that haven't changed.
/// Not cryptographic — a `DefaultHasher` collision only ever costs a missed
/// diff (a stale row briefly not resent), never a correctness bug, since
/// `noa.output` is best-effort push, not a source of truth clients rely on
/// for anything beyond display.
pub(super) fn hash_wire_row_spans(spans: &[Span]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for span in spans {
        span.text.hash(&mut hasher);
        hash_span_color(span.fg, &mut hasher);
        hash_span_color(span.bg, &mut hasher);
        match &span.attrs {
            Some(attrs) => {
                1u8.hash(&mut hasher);
                for attr in attrs {
                    (*attr as u8).hash(&mut hasher);
                }
            }
            None => 0u8.hash(&mut hasher),
        }
    }
    hasher.finish()
}

fn hash_span_color(color: Option<SpanColor>, hasher: &mut impl Hasher) {
    match color {
        None => 0u8.hash(hasher),
        Some(SpanColor::Hex((r, g, b))) => {
            1u8.hash(hasher);
            r.hash(hasher);
            g.hash(hasher);
            b.hash(hasher);
        }
        Some(SpanColor::Palette(p)) => {
            2u8.hash(hasher);
            p.hash(hasher);
        }
    }
}
