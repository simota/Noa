//! IPC output push tap (noa-server spec FR-17 / NFR-2): lets a pane's io
//! thread hand its trailing viewport rows to a subscribed `noa-ipc` client
//! without ever blocking on it. `broadcast_output` is `try_send`-only and
//! never blocks (see `noa_ipc::push::Broadcaster`); `None` here (server
//! disabled, or the pane hasn't been registered with an IPC id yet) costs
//! nothing beyond the `Option` check.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use noa_ipc::protocol::{Span, SpanColor};

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

/// Pure throttle decision, mirroring `sidebar::decide_sidebar_publish`'s
/// now-as-param shape.
pub(super) fn decide_ipc_output_push(last_push: Option<Instant>, now: Instant) -> bool {
    match last_push {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= OUTPUT_PUSH_MIN_INTERVAL,
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
