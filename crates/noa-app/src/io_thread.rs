//! The single io thread: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize and input
//! requests come in from the main thread over crossbeam channels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use noa_core::GridSize;
use noa_grid::Terminal;
use noa_pty::{Pty, PtyWriter};
use noa_render::FrameSnapshot;
use winit::event_loop::EventLoopProxy;

use crate::events::UserEvent;
use crate::session_store::{
    self, PreviewLine, PreviewSpan, SessionCardId, SessionDelta, SessionWindowId, WallClock,
};
use crate::split_tree::PaneId;
use crate::tab_overview::OVERVIEW_TILE_MIN_RENDER_INTERVAL;

/// Which window/pane's `UserEvent`s this io thread posts back to the main
/// loop. Grouped into one struct (rather than two `spawn` arguments)
/// because they're always passed and used together, and to keep `spawn`
/// under clippy's argument-count lint now that `overview` adds an eighth.
pub(crate) struct IoThreadTarget {
    pub(crate) window_id: winit::window::WindowId,
    pub(crate) pane_id: PaneId,
}

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

/// Read-only gate for the session sidebar's publish path (FR-1/AC-19),
/// deliberately **parallel to — never aliased with — [`OverviewPublish`]**
/// (Omen T1). `visible` is app-wide (`App::sidebar_visible_gate`), flipped on
/// while any window shows its sidebar; when it's off the io thread skips only
/// the preview-row extraction — the expensive part of an upsert — for a single
/// atomic load. The lightweight card metadata (name/cwd/busy) still publishes
/// so the attention pipeline works with every sidebar hidden (FR-A3/FR-A4).
/// Unlike the overview there is no `FrameSnapshot` slot here: the
/// `SessionStore` itself is the lock-free published surface (ADR 0001 —
/// "SessionStore は overview_snapshot と同型の publish-slot 読取モデル"), fed by
/// the [`SessionDelta`]s this gate lets through.
#[derive(Clone)]
pub(crate) struct SidebarPublish {
    pub(crate) visible: Arc<AtomicBool>,
}

/// How many trailing terminal rows the sidebar card preview shows (FR-2).
const SIDEBAR_PREVIEW_LINES: usize = 5;

pub(crate) type PtyInput = Box<[u8]>;

pub(crate) const PTY_INPUT_QUEUE_CAPACITY: usize = 1024;
const PTY_DATA_DRAIN_BYTE_LIMIT: usize = 256 * 1024;

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
const SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION: Duration = Duration::from_millis(100);

/// Owned handle for stopping and joining a PTY io thread.
pub(crate) struct IoThreadHandle {
    shutdown_tx: Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl IoThreadHandle {
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

    pub(crate) fn shutdown_and_join(mut self) {
        let _ = self.shutdown_and_join_timeout(Self::JOIN_TIMEOUT);
    }

    fn shutdown_and_join_timeout(&mut self, timeout: Duration) -> bool {
        let _ = self.shutdown_tx.send(());
        let deadline = Instant::now() + timeout;
        while self.join.as_ref().is_some_and(|join| !join.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }

        let Some(join) = self.join.take() else {
            return true;
        };
        if !join.is_finished() {
            self.join = Some(join);
            log::warn!("pty io thread did not stop within {timeout:?}");
            return false;
        }
        if let Err(err) = join.join() {
            log::warn!("pty io thread panicked during shutdown: {err:?}");
            return false;
        }
        true
    }
}

pub(crate) fn input_channel() -> (Sender<PtyInput>, Receiver<PtyInput>) {
    crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY)
}

#[derive(Debug)]
pub(crate) enum QueuePtyInputError {
    Full(PtyInput),
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LosslessQueueResult {
    Queued,
    Deferred,
    Disconnected,
}

pub(crate) fn try_queue_input(
    tx: &Sender<PtyInput>,
    input: PtyInput,
) -> Result<(), QueuePtyInputError> {
    tx.try_send(input).map_err(|err| match err {
        TrySendError::Full(input) => QueuePtyInputError::Full(input),
        TrySendError::Disconnected(_) => QueuePtyInputError::Disconnected,
    })
}

pub(crate) fn queue_input_lossless(tx: Sender<PtyInput>, input: PtyInput) -> LosslessQueueResult {
    match try_queue_input(&tx, input) {
        Ok(()) => LosslessQueueResult::Queued,
        Err(QueuePtyInputError::Full(input)) => {
            match std::thread::Builder::new()
                .name("noa-pty-input-send".to_string())
                .spawn(move || {
                    let _ = tx.send(input);
                }) {
                Ok(_) => LosslessQueueResult::Deferred,
                Err(err) => {
                    log::warn!("failed to defer pty input onto a sender thread: {err}");
                    LosslessQueueResult::Disconnected
                }
            }
        }
        Err(QueuePtyInputError::Disconnected) => LosslessQueueResult::Disconnected,
    }
}

struct TerminalOutput {
    pending_writes: Vec<u8>,
    pending_clipboard_writes: Vec<String>,
    pending_clipboard_reads: Vec<String>,
    pending_notifications: Vec<noa_grid::Notification>,
    synchronized_output: bool,
    /// Trailing-flush deadline owed by this feed's throttled overview
    /// publish (Fix B defect 1: a burst's final feed can land inside the
    /// throttle window and get silently skipped, leaving the mirror stuck
    /// on a stale mid-burst frame — REQ-OV-4). Threaded back to `spawn`'s
    /// loop so it can wake the thread once it elapses even with no further
    /// pty output. `None` when nothing is owed (published now, or the
    /// overview isn't visible).
    overview_publish_pending: Option<Instant>,
    /// A sidebar card upsert extracted this feed (name/cwd/busy/preview), or
    /// `None` when the throttle window has not elapsed. The spawn loop stamps
    /// it with the current wall-clock + card generation and posts it as a
    /// [`SessionDelta::Upsert`].
    sidebar_upsert: Option<SidebarUpsert>,
    /// An unread bell was drained this feed (FR-11); the spawn loop posts a
    /// [`SessionDelta::Bell`]. Drained unconditionally (FR-A4): the main
    /// thread classifies it, so an agent session's bell can escalate to an
    /// attention request even with every sidebar hidden.
    sidebar_bell: bool,
}

/// Per-feed sidebar card state extracted under the terminal lock (FR-2). Time
/// and generation are added by the spawn loop after the lock is released.
/// `preview` is `None` when every sidebar is hidden (the extraction is the
/// expensive part of the upsert, so only it is gated on visibility — the store
/// keeps the card's previous preview).
struct SidebarUpsert {
    name: String,
    cwd: String,
    busy: bool,
    preview: Option<Vec<PreviewLine>>,
}

/// The trailing non-blank rows of the active screen, for the card preview
/// (FR-2). Read under the terminal lock; returns at most
/// [`SIDEBAR_PREVIEW_LINES`] lines, oldest-first, trailing blanks dropped. Each
/// line coalesces adjacent cells sharing a foreground color into one
/// [`PreviewSpan`] so the sidebar can render it in its original ANSI colors.
/// Lock-held half of the preview extraction: clone only the trailing non-blank
/// rows the preview needs (at most [`SIDEBAR_PREVIEW_LINES`]), each truncated
/// at its last non-blank cell. Span/string building happens lock-free in
/// [`preview_spans`], keeping the pty-feed lock section short (NFR-1).
fn preview_rows(terminal: &Terminal) -> Vec<Vec<noa_grid::Cell>> {
    let grid = &terminal.active().grid;
    let mut rows: Vec<Vec<noa_grid::Cell>> = grid
        .iter()
        .rev()
        .filter_map(|row| {
            let last = row.cells.iter().rposition(|cell| !cell.is_blank())?;
            Some(row.cells[..=last].to_vec())
        })
        .take(SIDEBAR_PREVIEW_LINES)
        .collect();
    rows.reverse();
    rows
}

/// Lock-free half of [`extract_preview`]: coalesce adjacent cells sharing a
/// foreground color into [`PreviewSpan`]s.
fn preview_spans(rows: Vec<Vec<noa_grid::Cell>>) -> Vec<PreviewLine> {
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
/// [`decide_overview_publish`]'s now-as-param shape so it is testable without a
/// wall-clock sleep. `true` means extract and post an upsert this feed; the
/// within-throttle case returns `false`. Not gated on sidebar visibility
/// (FR-A3/FR-A4): the store must know every pane's name/cwd — and, via the
/// cwd-driven metadata worker, its foreground process — even with every
/// sidebar hidden, or an agent bell could never classify and escalate to an
/// attention request, and an OSC 9/777 attention flag would land on a missing
/// card. Only the preview extraction is visibility-gated (in
/// [`feed_terminal_batch`]). No trailing-flush variant: a skipped upsert
/// leaves slightly stale card state until the next output, which the store
/// tolerates (unlike the overview mirror there is no frame to get visually
/// stuck).
fn decide_sidebar_publish(last_publish: Option<Instant>, now: Instant) -> bool {
    match last_publish {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= OVERVIEW_TILE_MIN_RENDER_INTERVAL,
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn feed_terminal(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
    sidebar: &SidebarPublish,
    last_sidebar_publish: &mut Option<Instant>,
) -> TerminalOutput {
    feed_terminal_batch(
        terminal,
        stream,
        bytes,
        std::iter::empty::<&[u8]>(),
        overview,
        last_overview_publish,
        sidebar,
        last_sidebar_publish,
    )
}

#[allow(clippy::too_many_arguments)]
fn feed_terminal_batch<'a>(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    first: &[u8],
    rest: impl IntoIterator<Item = &'a [u8]>,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
    sidebar: &SidebarPublish,
    last_sidebar_publish: &mut Option<Instant>,
) -> TerminalOutput {
    let mut term = terminal.lock();
    stream.feed(first, &mut *term);
    for bytes in rest {
        stream.feed(bytes, &mut *term);
    }
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
            sidebar_visible.then(|| preview_rows(&term)),
        )
    });

    let mut output = TerminalOutput {
        pending_writes: term.take_pending_writes(),
        pending_clipboard_writes: term.take_pending_clipboard_writes(),
        pending_clipboard_reads: term.take_pending_clipboard_reads(),
        pending_notifications: term.take_pending_notifications(),
        synchronized_output: term.modes.synchronized_output(),
        overview_publish_pending,
        sidebar_upsert: None,
        sidebar_bell,
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

/// Stamp a wall-clock timestamp for a sidebar upsert (FR-10): the current Unix
/// time shifted into the viewer's local zone, decomposed into calendar fields.
fn sidebar_wall_clock_now() -> WallClock {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    session_store::civil_from_unix_secs(unix_secs + crate::localtime::local_offset_seconds())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PtyDrainTerminalEvent {
    ExitOrError,
    Disconnected,
}

fn drain_queued_pty_data(
    rx: &Receiver<noa_pty::PtyEvent>,
    chunks: &mut Vec<Box<[u8]>>,
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

/// Pure decision for whether an overview publish should fire now, stay
/// silent, or fire later (Fix B defect 1). Not visible means nothing is
/// owed at all — reopening the overview re-peeks every tab unconditionally
/// (`App::seed_overview_snapshots`, Fix B defect 2), so a stale skip here
/// is never left stranded. Visible-but-throttled means the tab's current
/// state must still reach the mirror once the throttle window elapses,
/// even with no further pty output to re-trigger this decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverviewPublishDecision {
    Skip,
    Publish,
    ScheduleTrailingFlush { deadline: Instant },
}

fn decide_overview_publish(
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
fn publish_overview_snapshot(
    terminal: &Terminal,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
) -> Option<Instant> {
    let now = Instant::now();
    let visible = overview.visible.load(Ordering::Relaxed);
    match decide_overview_publish(visible, *last_overview_publish, now) {
        OverviewPublishDecision::Skip => None,
        OverviewPublishDecision::Publish => {
            let snapshot = Arc::new(FrameSnapshot::peek(terminal));
            *overview.slot.lock() = Some(snapshot);
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
fn flush_pending_overview_publish(
    terminal: &Arc<Mutex<Terminal>>,
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
) {
    let now = Instant::now();
    let snapshot = {
        let term = terminal.lock();
        Arc::new(FrameSnapshot::peek(&term))
    };
    *overview.slot.lock() = Some(snapshot);
    *last_overview_publish = Some(now);
}

fn write_pty_bytes(writer: &PtyWriter, bytes: &[u8]) {
    if let Err(err) = writer.write(bytes).and_then(|_| writer.flush()) {
        log::warn!("failed to write bytes to pty: {err}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedrawDecision {
    /// Ask the main thread to repaint now — synchronized output is not active,
    /// or the suppression cap has elapsed since the last paint.
    Now,
    /// Withhold this feed's redraw. Wake and force one at `deadline` unless
    /// intervening output clears synchronized output (or its own cap) first.
    Suppress { deadline: Instant },
}

/// Decide whether a just-fed batch should trigger a redraw. `synchronized` is
/// the terminal's DECSET 2026 state at end of batch; `last_redraw` is when the
/// io thread last actually asked the main thread to repaint. A batch left in
/// synchronized output withholds its redraw (that is the point of mode 2026) —
/// but never for longer than [`SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`] since the
/// last paint, so a stalled or batch-straddled frame can't freeze the screen
/// (bounds staleness to the cap even under a continuous burst of frames).
fn decide_redraw(synchronized: bool, last_redraw: Option<Instant>, now: Instant) -> RedrawDecision {
    if !synchronized {
        return RedrawDecision::Now;
    }
    match last_redraw {
        // Painted recently: hold this frame, but arm a deadline so the cap is
        // enforced even if no further output arrives to release 2026.
        Some(last) if now.saturating_duration_since(last) < SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION => {
            RedrawDecision::Suppress {
                deadline: last + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION,
            }
        }
        // Never painted, or the cap already elapsed: paint now to bound staleness.
        _ => RedrawDecision::Now,
    }
}

/// Spawn the io thread, which takes ownership of `pty`. Returns immediately;
/// the thread runs until the pty exits or errors, or the event loop is gone.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    pty: Pty,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
    target: IoThreadTarget,
    resize_rx: Receiver<GridSize>,
    input_rx: Receiver<PtyInput>,
    overview: OverviewPublish,
    sidebar: SidebarPublish,
) -> IoThreadHandle {
    let IoThreadTarget { window_id, pane_id } = target;
    // The GUI-agnostic card key for every sidebar delta this thread posts. The
    // store never sees a winit `WindowId` (NFR-6); the app boundary converts it
    // here via winit's stable `WindowId` ↔ `u64` mapping.
    let card_id = SessionCardId::new(SessionWindowId(u64::from(window_id)), pane_id);
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let join = std::thread::spawn(move || {
        let writer = pty.writer();
        let mut stream = noa_vt::Stream::new();
        let mut last_overview_publish: Option<Instant> = None;
        let mut last_sidebar_publish: Option<Instant> = None;
        // Per-card generation for [`SessionDelta::Upsert`], monotonic so the
        // store can drop a reordered/stale upsert (`SessionStore::apply`).
        let mut sidebar_seq: u64 = 0;
        // Trailing-flush deadline owed by a throttled overview publish
        // (Fix B defect 1), if any. `None` means nothing is owed, and the
        // select below blocks indefinitely exactly as before this fix — an
        // idle tab, or a tab whose last feed published immediately, costs
        // no extra wake-ups. This is why `crossbeam_channel::select!`'s
        // fixed-arm macro was swapped for the lower-level `Select` builder:
        // it lets the timeout arm be added only when something is owed,
        // instead of a constant poll interval.
        let mut publish_pending_at: Option<Instant> = None;
        // When the io thread last asked the main thread to repaint, and a
        // deadline owed by a redraw currently withheld under synchronized
        // output (DECSET 2026). Together they cap how long a mid-sync frame
        // can sit unpainted (see [`decide_redraw`]); `None` deadline means
        // nothing is owed and the select below blocks exactly as before.
        let mut last_redraw_at: Option<Instant> = None;
        let mut sync_redraw_deadline: Option<Instant> = None;
        loop {
            let mut sel = crossbeam_channel::Select::new();
            let shutdown_op = sel.recv(&shutdown_rx);
            let pty_op = sel.recv(pty.event_rx());
            let resize_op = sel.recv(&resize_rx);
            let input_op = sel.recv(&input_rx);

            // Wake at whichever owed deadline comes first: an overview trailing
            // flush (Fix B defect 1) or a withheld synchronized-output redraw.
            let next_deadline = [publish_pending_at, sync_redraw_deadline]
                .into_iter()
                .flatten()
                .min();
            let selected = match next_deadline {
                Some(deadline) => sel
                    .select_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok(),
                None => Some(sel.select()),
            };

            let Some(oper) = selected else {
                // A deadline elapsed with nothing else waking the thread first.
                let now = Instant::now();
                if publish_pending_at.is_some_and(|deadline| now >= deadline) {
                    // The throttle window elapsed — flush now (Fix B defect 1).
                    flush_pending_overview_publish(
                        &terminal,
                        &overview,
                        &mut last_overview_publish,
                    );
                    publish_pending_at = None;
                }
                if sync_redraw_deadline.is_some_and(|deadline| now >= deadline) {
                    // The synchronized-output suppression cap elapsed — force
                    // the withheld repaint so the stale frame can't persist.
                    sync_redraw_deadline = None;
                }
                // Either deadline means the frame the main thread holds is
                // stale; a single redraw covers both.
                last_redraw_at = Some(now);
                if proxy
                    .send_event(UserEvent::Redraw(window_id, pane_id))
                    .is_err()
                {
                    break; // event loop gone
                }
                continue;
            };

            match oper.index() {
                i if i == shutdown_op => {
                    let _ = oper.recv(&shutdown_rx);
                    break;
                }
                i if i == pty_op => match oper.recv(pty.event_rx()) {
                    Ok(noa_pty::PtyEvent::Data(bytes)) => {
                        let mut drained = Vec::new();
                        let terminal_event =
                            drain_queued_pty_data(pty.event_rx(), &mut drained, bytes.len());
                        let mut output = feed_terminal_batch(
                            &terminal,
                            &mut stream,
                            bytes.as_ref(),
                            drained.iter().map(|chunk| chunk.as_ref()),
                            &overview,
                            &mut last_overview_publish,
                            &sidebar,
                            &mut last_sidebar_publish,
                        );
                        publish_pending_at = output.overview_publish_pending;
                        let sidebar_bell = output.sidebar_bell;
                        let sidebar_upsert = output.sidebar_upsert.take();
                        if sidebar_bell
                            && proxy
                                .send_event(UserEvent::SessionDelta(SessionDelta::Bell {
                                    id: card_id,
                                }))
                                .is_err()
                        {
                            break; // event loop gone
                        }
                        if let Some(upsert) = sidebar_upsert {
                            sidebar_seq += 1;
                            if proxy
                                .send_event(UserEvent::SessionDelta(SessionDelta::Upsert {
                                    id: card_id,
                                    seq: sidebar_seq,
                                    name: upsert.name,
                                    cwd: upsert.cwd,
                                    busy: upsert.busy,
                                    updated_at: sidebar_wall_clock_now(),
                                    preview: upsert.preview,
                                }))
                                .is_err()
                            {
                                break; // event loop gone
                            }
                        }
                        if !output.pending_writes.is_empty() {
                            write_pty_bytes(&writer, &output.pending_writes);
                        }
                        let redraw = decide_redraw(
                            output.synchronized_output,
                            last_redraw_at,
                            Instant::now(),
                        );
                        for text in output.pending_clipboard_writes {
                            let _ = proxy.send_event(UserEvent::ClipboardWrite {
                                window_id,
                                pane_id,
                                text,
                            });
                        }
                        for target in output.pending_clipboard_reads {
                            let _ = proxy.send_event(UserEvent::ClipboardRead {
                                window_id,
                                pane_id,
                                target,
                            });
                        }
                        for notification in output.pending_notifications {
                            let _ = proxy.send_event(UserEvent::Notify {
                                window_id,
                                pane_id,
                                title: notification.title,
                                body: notification.body,
                            });
                        }
                        match redraw {
                            RedrawDecision::Now => {
                                sync_redraw_deadline = None;
                                last_redraw_at = Some(Instant::now());
                                if proxy
                                    .send_event(UserEvent::Redraw(window_id, pane_id))
                                    .is_err()
                                {
                                    break; // event loop gone
                                }
                            }
                            // Frame withheld under synchronized output: owe a
                            // redraw at the cap deadline so it can't get stuck.
                            RedrawDecision::Suppress { deadline } => {
                                sync_redraw_deadline = Some(deadline);
                            }
                        }
                        match terminal_event {
                            Some(PtyDrainTerminalEvent::ExitOrError) => {
                                let _ = proxy.send_event(UserEvent::PtyExit(window_id, pane_id));
                                break;
                            }
                            Some(PtyDrainTerminalEvent::Disconnected) => break,
                            None => {}
                        }
                    }
                    Ok(noa_pty::PtyEvent::Exit(_)) | Ok(noa_pty::PtyEvent::Error(_)) => {
                        let _ = proxy.send_event(UserEvent::PtyExit(window_id, pane_id));
                        break;
                    }
                    Err(_) => break, // channel closed
                },
                i if i == resize_op => match oper.recv(&resize_rx) {
                    Ok(size) => {
                        let _ = pty.resize(size);
                    }
                    Err(_) => break, // main thread / App dropped
                },
                i if i == input_op => match oper.recv(&input_rx) {
                    Ok(bytes) => write_pty_bytes(&writer, bytes.as_ref()),
                    Err(_) => break, // main thread / App dropped
                },
                _ => unreachable!("select only registers the four operations above"),
            }
        }
    });
    IoThreadHandle {
        shutdown_tx,
        join: Some(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_overview_publish() -> OverviewPublish {
        OverviewPublish {
            slot: Arc::new(Mutex::new(None)),
            visible: Arc::new(AtomicBool::new(false)),
        }
    }

    fn test_sidebar_publish(visible: bool) -> SidebarPublish {
        SidebarPublish {
            visible: Arc::new(AtomicBool::new(visible)),
        }
    }

    #[test]
    fn decide_sidebar_publish_throttles() {
        let now = Instant::now();
        // First feed publishes.
        assert!(decide_sidebar_publish(None, now));
        // Inside the throttle window: skip.
        assert!(!decide_sidebar_publish(
            Some(now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2),
            now
        ));
        // Past the throttle window: publish.
        assert!(decide_sidebar_publish(
            Some(now - OVERVIEW_TILE_MIN_RENDER_INTERVAL),
            now
        ));
    }

    // FR-A3/FR-A4: the upsert is not visibility-gated — with every sidebar
    // hidden the card metadata still publishes (so an agent bell can classify
    // and escalate), but the expensive preview extraction is skipped.
    #[test]
    fn feed_extracts_a_lightweight_upsert_while_hidden_and_a_full_one_while_visible() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        // Gate off: a lightweight upsert (no preview), no bell.
        let mut last_sidebar_publish = None;
        let off = feed_terminal(
            &terminal,
            &mut stream,
            b"hello",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut last_sidebar_publish,
        );
        let light = off.sidebar_upsert.expect("hidden first feed still publishes");
        assert!(light.preview.is_none());
        assert!(!off.sidebar_bell);
        assert!(last_sidebar_publish.is_some());

        // Gate on, past the throttle: an upsert carrying the trailing preview
        // line.
        let sidebar = test_sidebar_publish(true);
        let mut last_sidebar_publish = None;
        let on = feed_terminal(
            &terminal,
            &mut stream,
            b"\r\nsecond line",
            &overview,
            &mut last_overview_publish,
            &sidebar,
            &mut last_sidebar_publish,
        );
        let upsert = on.sidebar_upsert.expect("visible first feed publishes");
        assert!(
            upsert
                .preview
                .expect("visible feed extracts the preview")
                .iter()
                .any(|line| session_store::preview_line_text(line).contains("second line"))
        );
        assert!(last_sidebar_publish.is_some());

        // A second feed inside the throttle window yields no upsert.
        let throttled = feed_terminal(
            &terminal,
            &mut stream,
            b"more",
            &overview,
            &mut last_overview_publish,
            &sidebar,
            &mut last_sidebar_publish,
        );
        assert!(throttled.sidebar_upsert.is_none());
    }

    // FR-A4: the bell is drained regardless of sidebar visibility, so an agent
    // session's bell can escalate to an attention request even when the sidebar
    // is hidden (the main thread does the agent-vs-generic classification).
    #[test]
    fn feed_drains_the_bell_regardless_of_sidebar_visibility() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        // Bell rung while the sidebar is hidden is still drained and reported.
        let hidden = feed_terminal(
            &terminal,
            &mut stream,
            b"\x07",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );
        assert!(hidden.sidebar_bell);

        // With no further bell, a subsequent feed reports none.
        let quiet = feed_terminal(
            &terminal,
            &mut stream,
            b"x",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(true),
            &mut None,
        );
        assert!(!quiet.sidebar_bell);
    }

    #[test]
    fn feed_terminal_returns_pending_writes_after_releasing_lock() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"\x1b[6n",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );

        assert_eq!(output.pending_writes, b"\x1b[1;1R");
        assert!(output.pending_clipboard_writes.is_empty());
        assert!(!output.synchronized_output);
        assert!(
            terminal.try_lock().is_some(),
            "terminal lock must be released before PTY writes"
        );
    }

    #[test]
    fn synchronized_output_suppresses_redraw_until_release() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"\x1b[?2026hhidden",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );

        // A frame left mid-sync withholds its redraw while a recent paint means
        // the suppression cap hasn't elapsed yet — but it owes one at the cap.
        assert!(output.synchronized_output);
        let just_painted = Instant::now();
        assert!(matches!(
            decide_redraw(output.synchronized_output, Some(just_painted), just_painted),
            RedrawDecision::Suppress { .. }
        ));

        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"\x1b[?2026l",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );

        // Releasing 2026 paints immediately, regardless of how recent the last
        // paint was.
        assert!(!output.synchronized_output);
        assert_eq!(
            decide_redraw(output.synchronized_output, Some(Instant::now()), Instant::now()),
            RedrawDecision::Now
        );
    }

    #[test]
    fn synchronized_output_redraw_is_capped_so_a_held_frame_cannot_freeze() {
        // Regression: an app (e.g. a Claude Code selection menu navigated with a
        // held arrow key) whose pty output keeps ending a coalesced batch
        // mid-frame leaves 2026 set at every batch boundary. Without a cap the
        // redraw is suppressed forever and the screen freezes; with the cap it
        // must repaint once the suppression window elapses since the last paint.
        let now = Instant::now();
        let last_paint = now - SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION;
        assert_eq!(
            decide_redraw(true, Some(last_paint), now),
            RedrawDecision::Now,
            "a frame held past the cap must repaint"
        );

        // Never painted yet: paint now rather than start life frozen.
        assert_eq!(decide_redraw(true, None, now), RedrawDecision::Now);

        // Within the cap: hold, but arm the deadline at exactly cap-since-paint.
        let recent = now - SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION / 2;
        assert_eq!(
            decide_redraw(true, Some(recent), now),
            RedrawDecision::Suppress {
                deadline: recent + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION
            }
        );
    }

    #[test]
    fn feed_terminal_does_not_publish_an_overview_snapshot_while_the_gate_is_off() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"hello",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );

        assert!(
            overview.slot.lock().is_none(),
            "overview_visible=false must cost only the atomic load, no publish"
        );
        assert!(last_overview_publish.is_none());
        assert!(
            output.overview_publish_pending.is_none(),
            "not-visible must not owe a trailing flush either"
        );
    }

    #[test]
    fn drain_queued_pty_data_preserves_data_before_terminal_event() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(noa_pty::PtyEvent::Data(
            b"queued".to_vec().into_boxed_slice(),
        ))
        .unwrap();
        tx.send(noa_pty::PtyEvent::Exit(0)).unwrap();

        let mut chunks = Vec::new();
        let terminal_event = drain_queued_pty_data(&rx, &mut chunks, 0);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].as_ref(), b"queued");
        assert_eq!(terminal_event, Some(PtyDrainTerminalEvent::ExitOrError));
    }

    #[test]
    fn drain_queued_pty_data_stops_after_byte_cap() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(noa_pty::PtyEvent::Data(b"a".to_vec().into_boxed_slice()))
            .unwrap();
        tx.send(noa_pty::PtyEvent::Data(b"b".to_vec().into_boxed_slice()))
            .unwrap();

        let mut chunks = Vec::new();
        let terminal_event = drain_queued_pty_data(&rx, &mut chunks, PTY_DATA_DRAIN_BYTE_LIMIT - 1);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].as_ref(), b"a");
        assert_eq!(terminal_event, None);
        assert!(matches!(
            rx.try_recv(),
            Ok(noa_pty::PtyEvent::Data(bytes)) if bytes.as_ref() == b"b"
        ));
    }

    #[test]
    fn decide_overview_publish_skips_when_not_visible_regardless_of_timing() {
        let now = Instant::now();

        assert_eq!(
            decide_overview_publish(false, None, now),
            OverviewPublishDecision::Skip
        );
        assert_eq!(
            decide_overview_publish(
                false,
                Some(now - OVERVIEW_TILE_MIN_RENDER_INTERVAL * 10),
                now
            ),
            OverviewPublishDecision::Skip
        );
    }

    #[test]
    fn decide_overview_publish_publishes_on_first_feed_and_when_due() {
        let now = Instant::now();

        assert_eq!(
            decide_overview_publish(true, None, now),
            OverviewPublishDecision::Publish
        );
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;
        assert_eq!(
            decide_overview_publish(true, Some(due), now),
            OverviewPublishDecision::Publish
        );
    }

    #[test]
    fn decide_overview_publish_schedules_a_trailing_flush_when_throttled() {
        let now = Instant::now();
        let last = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;

        assert_eq!(
            decide_overview_publish(true, Some(last), now),
            OverviewPublishDecision::ScheduleTrailingFlush {
                deadline: last + OVERVIEW_TILE_MIN_RENDER_INTERVAL
            }
        );
    }

    #[test]
    fn flush_pending_overview_publish_publishes_the_terminals_current_state() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let overview = test_overview_publish();
        let mut last_overview_publish = None;

        flush_pending_overview_publish(&terminal, &overview, &mut last_overview_publish);

        assert!(
            overview.slot.lock().is_some(),
            "the trailing flush must publish unconditionally, regardless of the gate"
        );
        assert!(last_overview_publish.is_some());
    }

    #[test]
    fn feed_terminal_publishes_an_overview_snapshot_throttled_to_the_min_render_interval() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();
        let overview = test_overview_publish();
        overview.visible.store(true, Ordering::Relaxed);
        let mut last_overview_publish = None;

        feed_terminal(
            &terminal,
            &mut stream,
            b"first",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );
        let first_snapshot = overview
            .slot
            .lock()
            .clone()
            .expect("visible=true publishes on the first feed");
        assert!(last_overview_publish.is_some());

        // Still inside the throttle window: the slot must not be replaced,
        // but the feed must record a trailing-flush deadline (Fix B defect
        // 1) rather than dropping the burst's final state on the floor.
        let throttled_publish_at = last_overview_publish.expect("set by the first feed");
        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"second",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );
        let still_first = overview.slot.lock().clone().unwrap();
        assert!(
            Arc::ptr_eq(&first_snapshot, &still_first),
            "a feed inside the throttle window must not replace the published snapshot"
        );
        assert_eq!(
            output.overview_publish_pending,
            Some(throttled_publish_at + OVERVIEW_TILE_MIN_RENDER_INTERVAL),
            "a throttled feed must schedule a trailing flush at the throttle deadline"
        );

        // Force the throttle window to have elapsed, then feed again.
        last_overview_publish = Some(Instant::now() - OVERVIEW_TILE_MIN_RENDER_INTERVAL);
        let output = feed_terminal(
            &terminal,
            &mut stream,
            b"third",
            &overview,
            &mut last_overview_publish,
            &test_sidebar_publish(false),
            &mut None,
        );
        let third_snapshot = overview.slot.lock().clone().unwrap();
        assert!(
            !Arc::ptr_eq(&first_snapshot, &third_snapshot),
            "a feed past the throttle window must publish a fresh snapshot"
        );
        assert!(
            output.overview_publish_pending.is_none(),
            "a feed that publishes immediately owes no trailing flush"
        );
    }

    #[test]
    fn input_channel_is_bounded_and_nonblocking_for_ui_thread() {
        fn input(bytes: &[u8]) -> PtyInput {
            bytes.to_vec().into_boxed_slice()
        }

        let (tx, rx) = input_channel();
        for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
            tx.try_send(input(b"x")).expect("queue has capacity");
        }

        match tx.try_send(input(b"y")) {
            Err(crossbeam_channel::TrySendError::Full(bytes)) => {
                assert_eq!(bytes.as_ref(), b"y");
            }
            other => panic!("expected a full input queue, got {other:?}"),
        }
        assert_eq!(rx.len(), PTY_INPUT_QUEUE_CAPACITY);
    }

    #[test]
    fn lossless_input_defers_instead_of_dropping_when_queue_is_full() {
        fn input(bytes: &[u8]) -> PtyInput {
            bytes.to_vec().into_boxed_slice()
        }

        let (tx, rx) = input_channel();
        for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
            try_queue_input(&tx, input(b"x")).expect("queue has capacity");
        }

        assert_eq!(
            queue_input_lossless(tx, input(b"paste")),
            LosslessQueueResult::Deferred
        );
        for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
            assert_eq!(rx.recv().expect("queued input").as_ref(), b"x");
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1))
                .expect("deferred paste should be delivered")
                .as_ref(),
            b"paste"
        );
    }

    // AC-18 (NFR-2): git must never be spawned on the io read loop — it lives
    // only in the dedicated `branch_poll` worker. Assert this module's source
    // never spawns `git` (nor any `Command`). The needles are assembled at
    // runtime so this test file does not trip its own scan.
    #[test]
    fn io_read_loop_never_spawns_git() {
        let source = include_str!("io_thread.rs");
        for forbidden in [
            ["Command", "::new(\"git\")"].concat(),
            ["Command", "::new"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "io_thread.rs must not spawn a subprocess (`{forbidden}`) — git belongs in branch_poll"
            );
        }
    }

    #[test]
    fn io_thread_handle_shutdown_joins_within_timeout() {
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let join = std::thread::spawn(move || {
            let _ = shutdown_rx.recv();
        });
        let mut handle = IoThreadHandle {
            shutdown_tx,
            join: Some(join),
        };

        assert!(handle.shutdown_and_join_timeout(Duration::from_millis(500)));
        assert!(handle.join.is_none());
    }

    #[test]
    fn pane_io_thread_shutdown_joins_all_blocked_handles_within_timeout() {
        let mut handles = Vec::new();
        for _ in 0..3 {
            let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
            let join = std::thread::spawn(move || {
                let _ = shutdown_rx.recv();
            });
            handles.push(IoThreadHandle {
                shutdown_tx,
                join: Some(join),
            });
        }

        for handle in &mut handles {
            assert!(handle.shutdown_and_join_timeout(Duration::from_millis(500)));
            assert!(handle.join.is_none());
        }
    }
}
