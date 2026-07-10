//! Redraw pacing: floors how often the io thread pokes the winit event loop
//! to repaint, and bounds how long a redraw can be withheld under
//! synchronized output (DECSET 2026).

use std::time::{Duration, Instant};

/// The longest the io thread withholds a redraw while an application holds
/// synchronized output (DECSET 2026) open. Mode 2026 has no standardized
/// timeout — the spec leaves it to the terminal, tmux uses 1s — and Ghostty
/// dodges the question by presenting on a vsync timer, so a frame left mid-sync
/// simply isn't shown until the next vsync after release. noa's renderer is
/// event-driven off pty output instead, so *suppressing the redraw request with
/// no fallback* strands the display on a stale frame whenever an app leaves
/// 2026 set across an input-wait, or (more often) when pty batching keeps
/// ending a coalesced read mid-frame during rapid repaints — e.g. holding an
/// arrow key to move through a Claude Code selection menu. Capping suppression
/// at the render cadence keeps the screen live: a well-behaved single frame
/// still paints atomically at its ESU (which arrives well within the cap, so
/// the cap never fires for it), while sustained back-to-back frames refresh at
/// ~10fps instead of freezing until output happens to stop on a frame boundary.
pub(super) const SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION: Duration = Duration::from_millis(100);

/// Floor between consecutive redraw requests outside synchronized output.
/// Each `UserEvent::Redraw` is a real OS wake-up of the winit event loop and
/// the display can't present faster than its refresh, so a flood of parse
/// batches requesting one repaint each is pure overhead. A withheld redraw
/// arms the same trailing deadline as synchronized output, so a burst's
/// final frame always paints. One 120Hz frame.
pub(super) const REDRAW_MIN_INTERVAL: Duration = Duration::from_millis(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RedrawDecision {
    /// Ask the main thread to repaint now — the redraw floor (or, under
    /// synchronized output, the suppression cap) has elapsed since the last
    /// paint.
    Now,
    /// Withhold this feed's redraw. Wake and force one at `deadline` unless
    /// intervening output earns an immediate repaint first.
    Suppress { deadline: Instant },
}

/// Decide whether a just-fed batch should trigger a redraw. `synchronized` is
/// the terminal's DECSET 2026 state at end of batch; `last_redraw` is when the
/// io thread last actually asked the main thread to repaint. Two suppression
/// windows apply: outside synchronized output redraws are floored to
/// [`REDRAW_MIN_INTERVAL`], so a flood of parse batches can't wake the event
/// loop faster than the display presents; a batch left in synchronized output
/// withholds its redraw (that is the point of mode 2026) — but never for
/// longer than [`SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`] since the last paint,
/// so a stalled or batch-straddled frame can't freeze the screen. Either way
/// a suppressed batch owes a redraw at `deadline`, so a burst's final frame
/// always paints.
pub(super) fn decide_redraw(
    synchronized: bool,
    last_redraw: Option<Instant>,
    now: Instant,
) -> RedrawDecision {
    let window = if synchronized {
        SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION
    } else {
        REDRAW_MIN_INTERVAL
    };
    match last_redraw {
        // Painted recently: hold this frame, but arm a deadline so the window
        // is enforced even if no further output arrives.
        Some(last) if now.saturating_duration_since(last) < window => RedrawDecision::Suppress {
            deadline: last + window,
        },
        // Never painted, or the window already elapsed: paint now.
        _ => RedrawDecision::Now,
    }
}
