//! IPC output push tap (noa-server spec FR-17 / NFR-2): lets a pane's io
//! thread hand its trailing viewport rows to a subscribed `noa-ipc` client
//! without ever blocking on it. `broadcast_output` is `try_send`-only and
//! never blocks (see `noa_ipc::push::Broadcaster`). Every pane's io thread
//! carries one of these unconditionally (R-3) — the zero-overhead-when-
//! nobody's-listening gate is `Broadcaster::has_output_subscriber_for(pane_id)`,
//! consulted per feed via `decide_ipc_output_push`'s `active` param, not the
//! tap's presence. Narrowing the gate per-pane (rather than server-wide)
//! matters once any client subscribes to output at all: without it, one
//! client watching a single pane would force every other producing pane to
//! pay the span-conversion + row-hash cost each throttle window too.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use noa_grid::Terminal;
use noa_ipc::protocol::{Row, Span, SpanColor};

/// Minimum interval between `noa.output` pushes for one pane — dirty rows
/// coalesce at this cadence (spec NFR-4 "dirty 合流 ≥16ms").
pub(crate) const OUTPUT_PUSH_MIN_INTERVAL: Duration = Duration::from_millis(16);

/// Per-pane handle an io thread pushes output through. Every pane carries
/// one (R-3) — whether it's ever actually used is gated per feed on
/// `broadcaster.has_output_subscriber_for(ipc_pane_id)`, not on whether this
/// tap exists.
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
/// now-as-param shape. `active` is
/// `broadcaster.has_output_subscriber_for(pane_id)` (R-3) — nobody
/// subscribed to `Output` for this pane specifically costs one bool check
/// and nothing else, same as a fully disabled server used to.
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

/// Per-pane cache of last-sent viewport row content hashes (F-6), keyed by
/// viewport slot (`visible_rows()` index), plus the absolute row `base`
/// (`rows_evicted + visible_row_base()`) those hashes were computed against.
///
/// The `base` is required, not just the hashes: `compute_ipc_row_diff`
/// diffs by *slot*, and when the viewport's base shifts (scrollback growth
/// pushes every visible row down by N), identical content can land in the
/// same slot it occupied before the shift. Without tracking the base, that
/// slot's hash would still match and the diff would wrongly suppress a row
/// whose absolute index (`Row.row`) the client needs updated (R-3).
#[derive(Default)]
pub(super) struct IpcRowCache {
    hashes: Vec<u64>,
    base: Option<u64>,
}

impl IpcRowCache {
    /// Drops every cached hash (R-3: per-pane subscriber gating). While a
    /// pane has no matching output subscriber the gate keeps this cache
    /// untouched, so a subscriber that appears later would otherwise diff
    /// against hashes computed the last time *some other* client was
    /// subscribed — potentially long stale, and wrongly suppressing rows the
    /// new subscriber has never actually seen. Called every feed the gate is
    /// closed, so the next feed it's open again starts from an empty cache
    /// and `compute_ipc_row_diff` resends the full viewport.
    pub(super) fn reset(&mut self) {
        self.hashes.clear();
        self.base = None;
    }
}

/// Diff the pane's current visible rows against `cache` (F-6), returning
/// only rows whose content hash changed since the last diff (or whose slot's
/// absolute row index moved, R-3) and updating the cache in place. Shared by
/// the normal per-feed push (`feed_terminal_batch`) and the trailing-flush
/// path (`flush_pending_ipc_output`) so both compute the diff identically
/// under a `Terminal` lock hold rather than one of them caching a
/// possibly-stale snapshot.
pub(super) fn compute_ipc_row_diff(term: &Terminal, cache: &mut IpcRowCache) -> Vec<Row> {
    let base = term
        .active_oldest_row()
        .saturating_add(term.active().visible_row_base()) as u64;
    let rows = term.active().visible_rows();
    if cache.hashes.len() != rows.len() || cache.base != Some(base) {
        // A viewport resize (length change) or a scroll (base change) both
        // invalidate every cached slot: a resize because slot indices no
        // longer mean the same thing, a scroll because a slot's absolute
        // row index (R-3) needs resending even if its content is unchanged.
        cache.hashes.clear();
        cache.hashes.resize(rows.len(), 0);
    }
    cache.base = Some(base);
    let mut diff = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let spans = crate::ipc_bridge::row_to_spans(row);
        let hash = hash_wire_row_spans(&spans);
        if cache.hashes[i] != hash {
            cache.hashes[i] = hash;
            diff.push(Row {
                row: base + i as u64,
                spans,
            });
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
    ipc_row_cache: &mut IpcRowCache,
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
