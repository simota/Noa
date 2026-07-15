//! The io-thread entry point: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize, input, and
//! explicit IPC viewport-refresh requests come in from the main thread over
//! crossbeam channels.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use noa_core::GridSize;
use noa_grid::Terminal;
use noa_pty::{Pty, PtyWriter};
use winit::event_loop::EventLoopProxy;

use crate::auto_approve::AutoApproveState;
use crate::events::UserEvent;
use crate::session_store::{SessionCardId, SessionDelta, SessionWindowId};
use crate::split_tree::PaneId;

use super::auto_approve::{
    AutoApproveFeedback, AutoApprovePublish, auto_approve_rescan_deadline,
    detect_auto_approve_candidate,
};
use super::feed::{
    PtyDrainTerminalEvent, capture_pty_bytes, drain_queued_pty_data, feed_terminal_batch,
    open_pty_capture,
};
use super::input_queue::QueuedPtyInput;
use super::ipc_tap::{IpcOutputTap, flush_pending_ipc_output, force_ipc_output_refresh};
use super::overview::{OverviewPublish, flush_pending_overview_publish};
use super::raw_attach::RawAttachTap;
use super::redraw::{RedrawDecision, RedrawFloor};
use super::sidebar::SidebarPublish;

/// How recently the last *small* pty output batch must have arrived for the
/// io thread to spin-poll before parking (see the pre-`Select` spin below).
/// Serialized query/reply traffic (DSR probes, TUI status queries, echo
/// during bursts) arrives well inside this window; human typing does not,
/// so an interactive-but-quiet pane never spins.
const HOT_SPIN_WINDOW: Duration = Duration::from_millis(2);
/// Upper bound on one pre-park spin. The next request of a serialized
/// round-trip loop lands 10–30µs after this thread goes idle, so this
/// budget catches it with margin while capping the wasted spin at the
/// trailing edge of a burst.
const HOT_SPIN_BUDGET: Duration = Duration::from_micros(150);
/// A fed batch at or under this size counts as interactive traffic and arms
/// the hot spin; anything bigger is bulk output (a flood reads full 64 KiB
/// `READ_CHUNK`s), where spinning would only steal cache/CPU from the
/// reader thread's sends — measured as a ~3% hit on the 150 MB consume
/// benchmark when the spin armed unconditionally.
const HOT_SPIN_MAX_BATCH: usize = 4096;

/// Which window/pane's `UserEvent`s this io thread posts back to the main
/// loop. Grouped into one struct (rather than two `spawn` arguments)
/// because they're always passed and used together, and to keep `spawn`
/// under clippy's argument-count lint now that `overview` adds an eighth.
pub(crate) struct IoThreadTarget {
    pub(crate) window_id: winit::window::WindowId,
    pub(crate) pane_id: PaneId,
}

/// Owned handle for stopping and joining a PTY io thread.
pub(crate) struct IoThreadHandle {
    pub(super) shutdown_tx: Sender<()>,
    pub(super) ipc_output_refresh_tx: Sender<()>,
    pub(super) join: Option<std::thread::JoinHandle<()>>,
}

impl IoThreadHandle {
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

    /// Ask the io thread to resend this pane's full viewport to matching IPC
    /// output subscribers. The bounded channel coalesces repeated main-thread
    /// mutations and never blocks the event loop.
    pub(crate) fn request_ipc_output_refresh(&self) {
        let _ = self.ipc_output_refresh_tx.try_send(());
    }

    /// Signal shutdown and reap the io thread off the caller (Item 6): a pty
    /// write stuck mid-syscall could otherwise freeze the caller — the main
    /// thread on every pane close — for up to `JOIN_TIMEOUT`, and a
    /// window/app close that tears down N panes in one sweep multiplies
    /// that. The wait + join is handed to a detached reaper thread instead;
    /// `Pty::drop` already kills the child process as soon as the io
    /// thread's closure returns and drops its owned `Pty`, independent of
    /// whether this join ever completes, so detaching changes nothing about
    /// shutdown correctness — including at final process exit (`Drop for
    /// App`), where the OS reclaims any still-running reaper along with
    /// everything else.
    ///
    /// Accepted risk: a reaping thread outlives the pane/window it belonged
    /// to by construction. If the OS ever reused this pane's `WindowId` for
    /// a brand-new window before the old io thread noticed `shutdown_rx` and
    /// exited, that stale thread's `PtyExit`/`Redraw` would carry the reused
    /// id. `contains_pane` guards on every late `UserEvent` make this
    /// harmless in practice (the new window's pane 1 has a different
    /// `PaneId`, which is per-window monotonic with no reuse until a u64
    /// wrap), so no generation counter is added here.
    pub(crate) fn shutdown_and_join(mut self) {
        std::thread::spawn(move || {
            let _ = self.shutdown_and_join_timeout(Self::JOIN_TIMEOUT);
        });
    }

    pub(super) fn shutdown_and_join_timeout(&mut self, timeout: Duration) -> bool {
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

fn write_pty_bytes(writer: &PtyWriter, bytes: &[u8]) {
    if std::env::var_os("NOA_IME_TRACE").is_some() {
        eprintln!("[ime-trace] io write: {:?}", String::from_utf8_lossy(bytes));
    }
    if let Err(err) = writer.write(bytes).and_then(|_| writer.flush()) {
        log::warn!("failed to write bytes to pty: {err}");
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
    input_rx: Receiver<QueuedPtyInput>,
    auto_approve_feedback_rx: Receiver<AutoApproveFeedback>,
    overview: OverviewPublish,
    sidebar: SidebarPublish,
    auto_approve: AutoApprovePublish,
    redraw_floor: RedrawFloor,
    ipc: IpcOutputTap,
    raw_attach: RawAttachTap,
) -> IoThreadHandle {
    let IoThreadTarget { window_id, pane_id } = target;
    // The GUI-agnostic card key for every sidebar delta this thread posts. The
    // store never sees a winit `WindowId` (NFR-6); the app boundary converts it
    // here via winit's stable `WindowId` ↔ `u64` mapping.
    let card_id = SessionCardId::new(SessionWindowId(u64::from(window_id)), pane_id);
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let (ipc_output_refresh_tx, ipc_output_refresh_rx) = crossbeam_channel::bounded(1);
    let join = std::thread::spawn(move || {
        let writer = pty.writer();
        let mut stream = noa_vt::Stream::with_shared_parser(raw_attach.parser());
        let mut pty_capture = open_pty_capture(window_id, pane_id);
        let mut last_overview_publish: Option<Instant> = None;
        let mut last_sidebar_publish: Option<Instant> = None;
        let mut auto_approve_state = AutoApproveState::default();
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
        // Last `noa.output` push instant for this pane (FR-17), `None` when
        // nobody currently subscribes to this pane's output specifically
        // (R-3: `ipc.broadcaster.has_output_subscriber_for(ipc.ipc_pane_id)`)
        // — the throttle gate is then never consulted, so a pane no
        // subscriber wants costs nothing per feed regardless of whether the
        // server is running, or other panes are being watched, at all.
        let mut last_ipc_push: Option<Instant> = None;
        // Per-pane, lock-free-after-extraction cache of last-sent viewport
        // row content hashes (F-6), keyed by viewport slot (`visible_rows()`
        // index) — `feed_terminal_batch` diffs against this under the same
        // lock hold it already extracts rows in, so `noa.output` only ever
        // carries rows whose content actually changed.
        let mut ipc_row_cache = super::ipc_tap::IpcRowCache::default();
        // Trailing-flush deadline owed by a throttled `noa.output` push
        // (R-1), mirroring `publish_pending_at` above. `None` means nothing
        // is owed and the select below blocks exactly as before this fix —
        // an inactive tap, or a push that went out immediately, costs no
        // extra wake-ups.
        let mut ipc_publish_pending_at: Option<Instant> = None;
        // A deadline owed by a redraw currently withheld — by the window's
        // shared [`RedrawFloor`] or the synchronized-output (DECSET 2026)
        // cap. Bounds how long a fed-but-unpainted frame can sit (see
        // [`RedrawFloor::decide`]); "when this pane last repainted" itself
        // now lives on `redraw_floor`, shared with every other pane in the
        // window, rather than as a local here. `None` deadline means nothing
        // is owed and the select below blocks exactly as before.
        let mut redraw_deadline: Option<Instant> = None;
        let mut auto_approve_rescan_at: Option<Instant> = None;
        // True while this pane owes a repaint for a user-input echo: set when
        // input bytes are forwarded to the pty, cleared by the next redraw
        // that actually fires. The next pty-output batch (the echo, when the
        // program echoes at all) then bypasses the redraw floor via
        // [`RedrawFloor::decide_input_echo`] instead of being withheld up to
        // one floor interval behind another pane's recent paint. At most one
        // bypass per input event, so a non-echoing program costs one extra
        // repaint per keystroke at worst — bounded by typing speed.
        let mut input_echo_pending = false;
        // When the last *small* pty output batch arrived, for the
        // hot-traffic spin gate: `None`/stale means this pane is idle (or
        // mid-flood), and every park below stays a plain block, exactly as
        // before the spin existed.
        let mut last_small_data_at: Option<Instant> = None;
        loop {
            // Fast path: poll every channel with `try_recv` before falling
            // back to a blocking `Select`. During sustained output the pty
            // channel is almost always ready, so rebuilding the six-op
            // `Select` per batch is pure overhead — while the control
            // channels are still polled every iteration, so a flood can't
            // starve shutdown, resize, or user input (^C must reach the
            // shell mid-flood).
            match shutdown_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let mut did_work = false;
            match ipc_output_refresh_rx.try_recv() {
                Ok(()) => {
                    force_ipc_output_refresh(
                        &terminal,
                        &ipc,
                        &mut last_ipc_push,
                        &mut ipc_row_cache,
                    );
                    ipc_publish_pending_at = None;
                    did_work = true;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            match resize_rx.try_recv() {
                Ok(size) => {
                    let _ = pty.resize(size);
                    did_work = true;
                }
                Err(TryRecvError::Disconnected) => break, // main thread / App dropped
                Err(TryRecvError::Empty) => {}
            }
            match input_rx.try_recv() {
                Ok(bytes) => {
                    if std::env::var_os("NOA_IME_TRACE").is_some() {
                        eprintln!(
                            "[ime-trace] io write: {:?}",
                            String::from_utf8_lossy(bytes.as_ref())
                        );
                    }
                    if let Err(err) = writer.write_owned(bytes) {
                        log::warn!("failed to queue bytes to pty: {err}");
                    }
                    input_echo_pending = true;
                    did_work = true;
                }
                Err(TryRecvError::Disconnected) => break, // main thread / App dropped
                Err(TryRecvError::Empty) => {}
            }
            match auto_approve_feedback_rx.try_recv() {
                Ok(feedback) => {
                    auto_approve_state.apply_feedback(
                        feedback.signature,
                        feedback.region_hash,
                        feedback.accepted,
                        Instant::now(),
                    );
                    auto_approve_rescan_at =
                        auto_approve_rescan_deadline(&auto_approve_state, Instant::now());
                    did_work = true;
                }
                Err(TryRecvError::Disconnected) => break, // main thread / App dropped
                Err(TryRecvError::Empty) => {}
            }
            match pty.event_rx().try_recv() {
                Ok(noa_pty::PtyEvent::Data(bytes)) => {
                    did_work = true;
                    let mut drained = Vec::new();
                    let terminal_event =
                        drain_queued_pty_data(pty.event_rx(), &mut drained, bytes.len());
                    let batch_bytes = bytes.len()
                        + drained.iter().map(|chunk| chunk.len()).sum::<usize>();
                    last_small_data_at =
                        (batch_bytes <= HOT_SPIN_MAX_BATCH).then(Instant::now);
                    if let Some(file) = pty_capture.as_mut()
                        && !capture_pty_bytes(
                            file,
                            bytes.as_ref(),
                            drained.iter().map(|chunk| chunk.as_ref()),
                        )
                    {
                        pty_capture = None;
                    }
                    let mut output = feed_terminal_batch(
                        &terminal,
                        &mut stream,
                        bytes.as_ref(),
                        drained.iter().map(|chunk| chunk.as_ref()),
                        &overview,
                        &mut last_overview_publish,
                        &sidebar,
                        &mut last_sidebar_publish,
                        &auto_approve,
                        &mut auto_approve_state,
                        ipc.broadcaster.has_output_subscriber_for(ipc.ipc_pane_id),
                        &mut last_ipc_push,
                        &mut ipc_row_cache,
                        &raw_attach,
                    );
                    // NOA_LATENCY_TRACE t1: the batch (a pending keypress's
                    // echo, when one is pending) is now parsed into the
                    // shared Terminal — the next snapshot contains it.
                    crate::latency_trace::on_pty_feed();
                    auto_approve_rescan_at =
                        auto_approve_rescan_deadline(&auto_approve_state, Instant::now());
                    publish_pending_at = output.overview_publish_pending;
                    ipc_publish_pending_at = output.ipc_output_publish_pending;
                    let sidebar_bell = output.sidebar_bell;
                    let sidebar_upsert = output.sidebar_upsert.take();
                    let auto_approve_candidate = output.auto_approve.take();
                    if sidebar_bell
                        && proxy
                            .send_event(UserEvent::SessionDelta(SessionDelta::Bell { id: card_id }))
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
                                updated_at: crate::localtime::wall_clock_now(),
                                preview: upsert.preview,
                            }))
                            .is_err()
                        {
                            break; // event loop gone
                        }
                    }
                    if let Some(candidate) = auto_approve_candidate
                        && proxy
                            .send_event(UserEvent::AutoApprove {
                                id: card_id,
                                signature: candidate.signature,
                                region_hash: candidate.region_hash,
                                disable_after: candidate.disable_after,
                            })
                            .is_err()
                    {
                        break; // event loop gone
                    }
                    // Row diff already extracted inside `feed_terminal_batch`
                    // under its one `Terminal` lock hold (F-6) — no second
                    // lock here, just handing the diff to the broadcaster.
                    // `feed_terminal_batch` only ever produces `Some` here
                    // when `has_output_subscriber_for(ipc.ipc_pane_id)` was
                    // true, so this `broadcast_output` never fires into an
                    // empty room.
                    if let Some(diff) = output.ipc_output.take() {
                        ipc.broadcaster.broadcast_output(
                            ipc.ipc_pane_id,
                            diff.coordinate_generation,
                            diff.lines,
                        );
                    }
                    if !output.pending_writes.is_empty() {
                        write_pty_bytes(&writer, &output.pending_writes);
                    }
                    let redraw = if input_echo_pending {
                        redraw_floor.decide_input_echo(output.synchronized_output, Instant::now())
                    } else {
                        redraw_floor.decide(output.synchronized_output, Instant::now())
                    };
                    if matches!(redraw, RedrawDecision::Now) {
                        input_echo_pending = false;
                    }
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
                            redraw_deadline = None;
                            if proxy
                                .send_event(UserEvent::Redraw(window_id, pane_id))
                                .is_err()
                            {
                                break; // event loop gone
                            }
                        }
                        // Frame withheld (redraw floor or synchronized
                        // output): owe a redraw at the window deadline so it
                        // can't get stuck.
                        RedrawDecision::Suppress { deadline } => {
                            redraw_deadline = Some(deadline);
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
                Err(TryRecvError::Disconnected) => break, // channel closed
                Err(TryRecvError::Empty) => {}
            }
            if did_work {
                continue;
            }

            // Nothing ready: settle any elapsed deadline, then block.
            let now = Instant::now();
            let mut deadline_elapsed = false;
            let mut needs_redraw = false;
            if publish_pending_at.is_some_and(|deadline| now >= deadline) {
                // The throttle window elapsed — flush now (Fix B defect 1).
                flush_pending_overview_publish(&terminal, &overview, &mut last_overview_publish);
                publish_pending_at = None;
                deadline_elapsed = true;
                needs_redraw = true;
            }
            if ipc_publish_pending_at.is_some_and(|deadline| now >= deadline) {
                // The throttle window elapsed with no further pty output to
                // re-trigger the per-feed push — flush now (R-1). Not a
                // redraw-worthy event: `noa.output` is a side channel to IPC
                // subscribers, not the on-screen frame, so this never sets
                // `needs_redraw`.
                flush_pending_ipc_output(&terminal, &ipc, &mut last_ipc_push, &mut ipc_row_cache);
                ipc_publish_pending_at = None;
                deadline_elapsed = true;
            }
            let mut redraw_claimed = false;
            if redraw_deadline.is_some_and(|deadline| now >= deadline) {
                // A withheld redraw (floor or synchronized-output cap) came
                // due — force the repaint so the stale frame can't persist.
                // Every pane suppressed within the same floor window shares
                // this exact deadline (it's derived from the same window
                // clock), so `claim_deadline` picks a single winner instead
                // of every pane firing its own wake in the same tick.
                redraw_deadline = None;
                deadline_elapsed = true;
                if redraw_floor.claim_deadline(now) {
                    needs_redraw = true;
                    redraw_claimed = true;
                }
            }
            if auto_approve_rescan_at.is_some_and(|deadline| now >= deadline) {
                deadline_elapsed = true;
                let candidate = {
                    let term = terminal.lock();
                    detect_auto_approve_candidate(&term, &auto_approve, &mut auto_approve_state)
                };
                auto_approve_rescan_at =
                    auto_approve_rescan_deadline(&auto_approve_state, Instant::now());
                if let Some(candidate) = candidate
                    && proxy
                        .send_event(UserEvent::AutoApprove {
                            id: card_id,
                            signature: candidate.signature,
                            region_hash: candidate.region_hash,
                            disable_after: candidate.disable_after,
                        })
                        .is_err()
                {
                    break; // event loop gone
                }
            }
            if deadline_elapsed {
                if needs_redraw {
                    if !redraw_claimed {
                        // Owed by the publish-pending flush, not the shared
                        // floor deadline — still a real paint, so keep the
                        // window's clock in sync for other panes.
                        redraw_floor.record(now);
                    }
                    if proxy
                        .send_event(UserEvent::Redraw(window_id, pane_id))
                        .is_err()
                    {
                        break; // event loop gone
                    }
                }
                continue;
            }

            // Hot-traffic spin (input-latency tail): a parked wake on this
            // thread costs 20–80µs at the scheduler's mercy, which is the
            // dominant term in a query round-trip's p99 (the parse itself is
            // ~1µs). While *interactive-sized* output is actively streaming
            // (see `HOT_SPIN_MAX_BATCH` — bulk floods never arm this), poll
            // every channel for a short budget before parking — the next
            // event of a serialized round-trip loop (DSR probe, TUI query,
            // echoed keystroke burst) arrives within a few tens of µs,
            // turning that scheduler wake into a hit in this loop. An idle
            // or human-typing pane parks exactly as before, burning
            // nothing.
            if last_small_data_at
                .is_some_and(|at| now.saturating_duration_since(at) < HOT_SPIN_WINDOW)
            {
                let spin_deadline = Instant::now() + HOT_SPIN_BUDGET;
                let mut ready = false;
                while Instant::now() < spin_deadline {
                    if !pty.event_rx().is_empty()
                        || !input_rx.is_empty()
                        || !resize_rx.is_empty()
                        || !shutdown_rx.is_empty()
                        || !ipc_output_refresh_rx.is_empty()
                        || !auto_approve_feedback_rx.is_empty()
                    {
                        ready = true;
                        break;
                    }
                    std::hint::spin_loop();
                }
                if ready {
                    continue;
                }
            }

            // Wake at whichever owed deadline comes first: an overview
            // trailing flush (Fix B defect 1), an IPC output trailing flush
            // (R-1), a withheld redraw, or an
            // auto-approve stability rescan. `ready`/`ready_timeout` report
            // readiness without receiving; the fast-path drains above
            // complete the operation on the next iteration (a spurious
            // wake-up just loops back here).
            let next_deadline = [
                publish_pending_at,
                redraw_deadline,
                auto_approve_rescan_at,
                ipc_publish_pending_at,
            ]
            .into_iter()
            .flatten()
            .min();
            let mut sel = crossbeam_channel::Select::new();
            sel.recv(&shutdown_rx);
            sel.recv(&ipc_output_refresh_rx);
            sel.recv(pty.event_rx());
            sel.recv(&resize_rx);
            sel.recv(&input_rx);
            sel.recv(&auto_approve_feedback_rx);
            match next_deadline {
                Some(deadline) => {
                    let _ = sel.ready_timeout(deadline.saturating_duration_since(Instant::now()));
                }
                None => {
                    let _ = sel.ready();
                }
            }
        }
    });
    IoThreadHandle {
        shutdown_tx,
        ipc_output_refresh_tx,
        join: Some(join),
    }
}
