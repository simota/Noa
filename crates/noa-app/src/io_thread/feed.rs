//! Terminal feed/drain: parses queued pty bytes into the shared `Terminal`,
//! then opportunistically publishes the overview and sidebar mirrors while
//! holding one final lock, plus the debug (`NOA_PTY_CAPTURE`) capture tap.
//!
//! The parse itself is chunked: each reader-thread chunk (or the eager
//! `first` read) takes its own lock hold with a *fair* unlock in between
//! (see [`feed_chunk_fair`]), so a big batch never blocks the main thread's
//! `FrameSnapshot` pass for longer than one chunk's parse time.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crossbeam_channel::{Receiver, TryRecvError};
use parking_lot::Mutex;

use noa_grid::Terminal;

#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[cfg(test)]
use crate::auto_approve::AutoApproveInputGuards;
use crate::auto_approve::AutoApproveState;
use crate::split_tree::PaneId;

use super::auto_approve::{
    AutoApproveCandidate, AutoApprovePublish, detect_auto_approve_candidate,
};
use super::ipc_tap::{
    IpcOutputPushDecision, IpcRowCache, IpcRowDiff, compute_ipc_row_diff, decide_ipc_output_push,
};
use super::overview::{OverviewPublish, publish_overview_snapshot};
use super::raw_attach::RawAttachTap;
use super::sidebar::{
    SidebarPublish, SidebarUpsert, decide_sidebar_publish, preview_rows, preview_spans,
};

/// Ceiling on pty bytes coalesced into one parse batch. Bigger batches drain
/// a sustained flood in proportionally fewer wake cycles, while the cap
/// bounds the batch's total parse cost (~1 MiB parses in a few ms even on
/// the heavier unicode path). This no longer bounds a single terminal-lock
/// hold — the lock is taken per chunk (see [`feed_chunk_fair`]), so the
/// worst-case hold is one `noa-pty` `READ_CHUNK` (64 KiB), not the whole
/// batch.
pub(super) const PTY_DATA_DRAIN_BYTE_LIMIT: usize = 1024 * 1024;

/// Eager report-reply sink: when `Some`, [`feed_chunk_fair`] drains
/// terminal-generated replies (DSR/DA/…) per chunk and hands them here so
/// they reach the pty without waiting for the rest of the batch to parse.
/// `None` (tests) keeps the batch-tail drain semantics.
pub(super) type ReplyFlush<'a> = Option<&'a mut dyn FnMut(&[u8])>;

pub(super) struct TerminalOutput {
    pub(super) pending_writes: Vec<u8>,
    pub(super) pending_clipboard_writes: Vec<String>,
    pub(super) pending_clipboard_reads: Vec<String>,
    pub(super) pending_notifications: Vec<noa_grid::Notification>,
    pub(super) synchronized_output: bool,
    /// Trailing-flush deadline owed by this feed's throttled overview
    /// publish (Fix B defect 1: a burst's final feed can land inside the
    /// throttle window and get silently skipped, leaving the mirror stuck
    /// on a stale mid-burst frame — REQ-OV-4). Threaded back to `spawn`'s
    /// loop so it can wake the thread once it elapses even with no further
    /// pty output. `None` when nothing is owed (published now, or the
    /// overview isn't visible).
    pub(super) overview_publish_pending: Option<Instant>,
    /// A sidebar card upsert extracted this feed (name/cwd/busy/preview), or
    /// `None` when the throttle window has not elapsed. The spawn loop stamps
    /// it with the current wall-clock + card generation and posts it as a
    /// [`crate::session_store::SessionDelta::Upsert`].
    pub(super) sidebar_upsert: Option<SidebarUpsert>,
    /// A prompt matched the auto-approve matrix and passed debounce.
    pub(super) auto_approve: Option<AutoApproveCandidate>,
    /// An unread bell was drained this feed (FR-11); the spawn loop posts a
    /// [`crate::session_store::SessionDelta::Bell`]. Drained unconditionally
    /// (FR-A4): the main thread classifies it, so an agent session's bell
    /// can escalate to an attention request even with every sidebar hidden.
    pub(super) sidebar_bell: bool,
    /// `noa.output` row diff (noa-server spec FR-17 / F-6): only the
    /// viewport rows whose content hash changed since the last push,
    /// carrying their absolute row indices. Extracted under this same
    /// `Terminal` lock hold — no second lock in the spawn loop. `None` when
    /// nobody currently subscribes to `Output` for this pane specifically
    /// (R-3: `Broadcaster::has_output_subscriber_for(pane_id)`) or the push
    /// is still within its throttle window; `Some(vec![])` never happens (an
    /// empty diff just stays `None`).
    pub(super) ipc_output: Option<IpcRowDiff>,
    /// Trailing-flush deadline owed by this feed's throttled `noa.output`
    /// push (R-1: mirrors `overview_publish_pending` — a burst's final feed
    /// can land inside the 16ms throttle window and get silently skipped,
    /// stranding subscribers on a stale mid-burst frame). Threaded back to
    /// `spawn`'s loop the same way, so it wakes the thread within the
    /// throttle window even with no further pty output. `None` when nothing
    /// is owed (pushed now, or the tap is inactive).
    pub(super) ipc_output_publish_pending: Option<Instant>,
    /// Whether this batch could have changed visible terminal state
    /// ([`noa_vt::Stream::take_display_dirty`]). `false` — every completed
    /// action was a pure report query (DSR/DA/DECRQM/XTVERSION/Kitty kbd
    /// query) — means the spawn loop skips the redraw poke entirely: there
    /// is nothing new to paint, and waking the main thread mid-query-burst
    /// only adds a snapshot pass that contends the terminal lock against
    /// the very next query's parse (measured as the dominant term in the
    /// DSR round-trip p99).
    pub(super) display_dirty: bool,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn feed_terminal(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
    sidebar: &SidebarPublish,
    last_sidebar_publish: &mut Option<Instant>,
) -> TerminalOutput {
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))),
        guards: Arc::new(Mutex::new(AutoApproveInputGuards::default())),
    };
    let mut auto_approve_state = AutoApproveState::default();
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();
    feed_terminal_batch(
        terminal,
        stream,
        bytes,
        std::iter::empty::<&[u8]>(),
        overview,
        last_overview_publish,
        sidebar,
        last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        false,
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
        None,
    )
}

/// Feed one chunk (a `noa-pty` reader-thread `READ_CHUNK`, or the eager
/// `first` read) into the terminal under its own lock hold, then release
/// with a *fair* unlock.
///
/// `parking_lot::Mutex` is unfair by default: the thread that just dropped a
/// guard is free to re-lock immediately, even past another thread that has
/// been parked on it — it only falls back to a fair unlock automatically on
/// average every ~0.5 ms, or unconditionally once a critical section runs
/// past ~1 ms (see the `parking_lot::Mutex` docs on eventual fairness). A
/// single chunk parses in well under either threshold, so without an
/// explicit `unlock_fair` here this io thread would keep barging back in
/// ahead of the main thread's queued `FrameSnapshot` lock, and chunking the
/// batch would buy nothing. `unlock_fair` costs nothing extra when no thread
/// is waiting — parking_lot's raw mutex takes the same single
/// compare-exchange fast path a plain unlock does in that case; the slower
/// handoff path only runs once a waiter is actually parked.
fn feed_chunk_fair(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
    raw_attach: &RawAttachTap,
    reply_flush: ReplyFlush<'_>,
) {
    let mut term = terminal.lock();
    stream.feed(bytes, &mut *term);
    // With an eager reply sink, drain report replies (DSR/DA/…) produced by
    // *this chunk* so they reach the pty without waiting for the rest of the
    // batch to parse — under a sustained flood the remaining-batch parse time
    // is the dominant term in the loaded DSR round-trip. The `is_empty` check
    // keeps the common no-reply chunk at one branch; the flush itself runs
    // after the fair unlock so the pty writer is never called under the
    // terminal lock.
    let replies = if reply_flush.is_some() && !term.pending_writes.is_empty() {
        term.take_pending_writes()
    } else {
        Vec::new()
    };
    let sink = raw_attach.sink();
    parking_lot::MutexGuard::unlock_fair(term);
    if !replies.is_empty()
        && let Some(flush) = reply_flush
    {
        flush(&replies);
    }
    if let Some(sink) = sink {
        sink.send(raw_attach, bytes);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn feed_terminal_batch<T: AsRef<[u8]>>(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    first: T,
    rest: impl IntoIterator<Item = T>,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
    sidebar: &SidebarPublish,
    last_sidebar_publish: &mut Option<Instant>,
    auto_approve: &AutoApprovePublish,
    auto_approve_state: &mut AutoApproveState,
    ipc_active: bool,
    last_ipc_push: &mut Option<Instant>,
    ipc_row_cache: &mut IpcRowCache,
    raw_attach: &RawAttachTap,
    mut reply_flush: ReplyFlush<'_>,
) -> TerminalOutput {
    // Feed each chunk under its own lock hold (worst case one `READ_CHUNK`,
    // 64 KiB) instead of holding the lock across the whole (up to 1 MiB)
    // `PTY_DATA_DRAIN_BYTE_LIMIT` batch, so a main thread waiting on this
    // same lock for a `FrameSnapshot` gets a chance to run between chunks.
    // The bytes still reach `stream`/`term` in the exact same order as
    // before — only the lock granularity changes, not the parse.
    //
    // Chunks are taken by value and dropped one by one as they are parsed
    // (not held to the end of the batch): dropping a `noa_pty::PtyData`
    // credits the reader's in-flight byte gate and returns its buffer to the
    // read pool, so the reader thread refills while this batch is still
    // parsing — read and parse overlap instead of alternating.
    feed_chunk_fair(
        terminal,
        stream,
        first.as_ref(),
        raw_attach,
        reply_flush
            .as_mut()
            .map(|f| &mut **f as &mut dyn FnMut(&[u8])),
    );
    drop(first);
    for bytes in rest {
        feed_chunk_fair(
            terminal,
            stream,
            bytes.as_ref(),
            raw_attach,
            reply_flush
                .as_mut()
                .map(|f| &mut **f as &mut dyn FnMut(&[u8])),
        );
        drop(bytes);
    }

    // Batch-tail work (overview/sidebar/auto-approve/IPC extraction below)
    // stays a single lock hold, as before — none of it should be
    // interleaved with the main thread's snapshot pass.
    let mut term = terminal.lock();
    let overview_publish_pending =
        publish_overview_snapshot(&term, overview, last_overview_publish);

    // Sidebar publish (FR-1/FR-11) — extracted in this same lock section so the
    // main thread never locks a `Terminal` to build card state (NFR-1). The
    // bell is drained unconditionally (FR-A4): the main thread classifies it —
    // an agent session's bell escalates to an attention request even with the
    // sidebar hidden, while a generic bell only renders once the sidebar is
    // shown (its flag is otherwise invisible).
    let sidebar_visible = sidebar.visible.load(Ordering::Relaxed);
    let sidebar_bell = term.take_pending_bell();
    // Under the lock, clone only the raw preview rows plus the small card
    // scalars; the span/string building runs after the lock is released so
    // the main thread's snapshot pass is never blocked on string formatting.
    // The upsert itself is not visibility-gated (see `decide_sidebar_publish`);
    // only the preview-row clone — the expensive part — is.
    let sidebar_raw = decide_sidebar_publish(*last_sidebar_publish, Instant::now()).then(|| {
        *last_sidebar_publish = Some(Instant::now());
        (
            term.title.clone(),
            term.cwd.clone().unwrap_or_default(),
            term.has_running_program(),
            sidebar_visible
                .then(|| preview_rows(&term, sidebar.preview_lines.load(Ordering::Relaxed))),
        )
    });
    let auto_approve_candidate =
        detect_auto_approve_candidate(&term, auto_approve, auto_approve_state);

    // IPC output row diff (FR-17 / F-6): extracted under this same lock
    // hold — no second `Terminal` lock from the spawn loop. `ipc_active ==
    // false` (R-3: `Broadcaster::has_output_subscriber_for(pane_id)` —
    // server disabled, running with no output subscribers, or this pane's
    // rows specifically just aren't wanted by any of them) costs one bool
    // check and nothing else.
    let (ipc_output, ipc_output_publish_pending) =
        match decide_ipc_output_push(ipc_active, *last_ipc_push, Instant::now()) {
            IpcOutputPushDecision::Skip => {
                // R-3: the gate is closed this feed (no subscriber matches
                // this pane right now). Drop any cached hashes so that if a
                // subscriber appears on a later feed, the cache is empty and
                // the first push after (re)activation is a full resend
                // rather than a diff against a possibly ancient snapshot.
                ipc_row_cache.reset();
                (None, None)
            }
            IpcOutputPushDecision::Push => {
                *last_ipc_push = Some(Instant::now());
                let diff = compute_ipc_row_diff(&term, ipc_row_cache);
                (
                    if diff.lines.is_empty() {
                        None
                    } else {
                        Some(diff)
                    },
                    None,
                )
            }
            IpcOutputPushDecision::ScheduleTrailingFlush { deadline } => (None, Some(deadline)),
        };

    // With an eager `reply_flush` every reply already left in
    // `feed_chunk_fair`; this batch-tail drain is the catch-all for the
    // `None` path (tests) and for anything queued outside `stream.feed`.
    let mut output = TerminalOutput {
        pending_writes: term.take_pending_writes(),
        pending_clipboard_writes: term.take_pending_clipboard_writes(),
        pending_clipboard_reads: term.take_pending_clipboard_reads(),
        pending_notifications: term.take_pending_notifications(),
        synchronized_output: term.modes.synchronized_output(),
        overview_publish_pending,
        sidebar_upsert: None,
        auto_approve: auto_approve_candidate,
        sidebar_bell,
        ipc_output,
        ipc_output_publish_pending,
        display_dirty: stream.take_display_dirty(),
    };
    drop(term);
    output.sidebar_upsert = sidebar_raw.map(|(name, cwd, busy, rows)| SidebarUpsert {
        name,
        cwd,
        busy,
        preview: rows.map(preview_spans),
    });
    output
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PtyDrainTerminalEvent {
    ExitOrError,
    Disconnected,
}

/// Debug pty capture (`NOA_PTY_CAPTURE=<prefix>`): open this pane's capture
/// file, `<prefix>.<window>-<pane>.bin`. Every raw byte the io thread feeds
/// into the terminal is appended verbatim, so a rendering-fidelity report can
/// be replayed offline with `cargo run -p noa-grid --example replay -- <file>
/// <cols> <rows>` and diffed against the on-screen state. One file per pane —
/// panes must not interleave into a shared stream or the replay is garbage.
pub(super) fn open_pty_capture(
    window_id: winit::window::WindowId,
    pane_id: PaneId,
) -> Option<std::fs::File> {
    let prefix = std::env::var_os("NOA_PTY_CAPTURE")?;
    let path = std::path::PathBuf::from(format!(
        "{}.{}-{}.bin",
        prefix.to_string_lossy(),
        u64::from(window_id),
        pane_id.get()
    ));
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => Some(file),
        Err(err) => {
            log::warn!("NOA_PTY_CAPTURE: cannot open {}: {err}", path.display());
            None
        }
    }
}

/// Append one feed batch to the capture file. On a write error the capture is
/// dropped (returns `false`) after a warning — a broken debug tap must not
/// stall or kill the io thread.
pub(super) fn capture_pty_bytes<'a>(
    file: &mut std::fs::File,
    first: &[u8],
    rest: impl IntoIterator<Item = &'a [u8]>,
) -> bool {
    use std::io::Write as _;
    let result = file
        .write_all(first)
        .and_then(|()| rest.into_iter().try_for_each(|chunk| file.write_all(chunk)));
    if let Err(err) = result {
        log::warn!("NOA_PTY_CAPTURE: write failed, disabling capture: {err}");
        return false;
    }
    true
}

pub(super) fn drain_queued_pty_data(
    rx: &Receiver<noa_pty::PtyEvent>,
    chunks: &mut Vec<noa_pty::PtyData>,
    mut buffered_bytes: usize,
) -> Option<PtyDrainTerminalEvent> {
    while buffered_bytes < PTY_DATA_DRAIN_BYTE_LIMIT {
        match rx.try_recv() {
            Ok(noa_pty::PtyEvent::Data(bytes)) => {
                buffered_bytes = buffered_bytes.saturating_add(bytes.len());
                chunks.push(bytes);
            }
            Ok(noa_pty::PtyEvent::Exit(_)) | Ok(noa_pty::PtyEvent::Error(_)) => {
                return Some(PtyDrainTerminalEvent::ExitOrError);
            }
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => return Some(PtyDrainTerminalEvent::Disconnected),
        }
    }
    None
}
