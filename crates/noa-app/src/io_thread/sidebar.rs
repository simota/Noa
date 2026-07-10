//! Session sidebar publish path (FR-1/AC-19): extracts per-feed card state
//! (name/cwd/busy/preview) under the terminal lock the io thread already
//! holds, throttled independently of the overview mirror.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;

use noa_grid::Terminal;

use crate::session_overview::OVERVIEW_TILE_MIN_RENDER_INTERVAL;
use crate::session_store::{PreviewLine, PreviewSpan};

/// Read-only gate for the session sidebar's publish path (FR-1/AC-19),
/// deliberately **parallel to — never aliased with — [`super::OverviewPublish`]**
/// (Omen T1). `visible` is app-wide (`App::sidebar_visible_gate`), flipped on
/// while any window shows its sidebar; when it's off the io thread skips only
/// the preview-row extraction — the expensive part of an upsert — for a single
/// atomic load. The lightweight card metadata (name/cwd/busy) still publishes
/// so the attention pipeline works with every sidebar hidden (FR-A3/FR-A4).
/// `preview_lines` is also shared because Theme & Settings can change the card
/// preview size while the pane's io thread is already running.
/// Unlike the overview there is no `FrameSnapshot` slot here: the
/// `SessionStore` itself is the lock-free published surface (ADR 0001 —
/// "SessionStore は overview_snapshot と同型の publish-slot 読取モデル"), fed by
/// the [`SessionDelta`]s this gate lets through.
#[derive(Clone)]
pub(crate) struct SidebarPublish {
    pub(crate) visible: Arc<AtomicBool>,
    pub(crate) preview_lines: Arc<AtomicUsize>,
}

/// Per-feed sidebar card state extracted under the terminal lock (FR-2). Time
/// and generation are added by the spawn loop after the lock is released.
/// `preview` is `None` when every sidebar is hidden (the extraction is the
/// expensive part of the upsert, so only it is gated on visibility — the store
/// keeps the card's previous preview).
pub(super) struct SidebarUpsert {
    pub(super) name: String,
    pub(super) cwd: String,
    pub(super) busy: bool,
    pub(super) preview: Option<Vec<PreviewLine>>,
}

/// The trailing non-blank rows of the active screen, for the card preview
/// (FR-2). Read under the terminal lock; returns at most
/// `limit` lines, oldest-first, trailing blanks dropped. Each
/// line coalesces adjacent cells sharing a foreground color into one
/// [`PreviewSpan`] so the sidebar can render it in its original ANSI colors.
/// Lock-held half of the preview extraction: clone only the trailing non-blank
/// rows the preview needs (at most `limit`), each truncated
/// at its last non-blank cell. Span/string building happens lock-free in
/// [`preview_spans`], keeping the pty-feed lock section short (NFR-1).
pub(super) fn preview_rows(terminal: &Terminal, limit: usize) -> Vec<Vec<noa_grid::Cell>> {
    let grid = &terminal.active().grid;
    let mut rows: Vec<Vec<noa_grid::Cell>> = grid
        .iter()
        .rev()
        .filter_map(|row| {
            let last = row.cells.iter().rposition(|cell| !cell.is_blank())?;
            Some(row.cells[..=last].to_vec())
        })
        .take(limit)
        .collect();
    rows.reverse();
    rows
}

/// Lock-free half of `extract_preview`: coalesce adjacent cells sharing a
/// foreground color into [`PreviewSpan`]s.
pub(super) fn preview_spans(rows: Vec<Vec<noa_grid::Cell>>) -> Vec<PreviewLine> {
    rows.into_iter()
        .map(|cells| {
            let mut spans: PreviewLine = Vec::new();
            for cell in &cells {
                match spans.last_mut() {
                    Some(span) if span.fg == cell.fg => cell.push_text_to(&mut span.text),
                    _ => {
                        let mut text = String::new();
                        cell.push_text_to(&mut text);
                        spans.push(PreviewSpan { text, fg: cell.fg });
                    }
                }
            }
            spans
        })
        .collect()
}

/// Pure throttle decision for a sidebar publish (AC-19), mirroring
/// [`super::overview::decide_overview_publish`]'s now-as-param shape so it is
/// testable without a wall-clock sleep. `true` means extract and post an
/// upsert this feed; the within-throttle case returns `false`. Not gated on
/// sidebar visibility (FR-A3/FR-A4): the store must know every pane's
/// name/cwd — and, via the cwd-driven metadata worker, its foreground
/// process — even with every sidebar hidden, or an agent bell could never
/// classify and escalate to an attention request, and an OSC 9/777
/// attention flag would land on a missing card. Only the preview extraction
/// is visibility-gated (in `feed_terminal_batch`). No trailing-flush
/// variant: a skipped upsert leaves slightly stale card state until the
/// next output, which the store tolerates (unlike the overview mirror there
/// is no frame to get visually stuck).
pub(super) fn decide_sidebar_publish(last_publish: Option<Instant>, now: Instant) -> bool {
    match last_publish {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= OVERVIEW_TILE_MIN_RENDER_INTERVAL,
    }
}
