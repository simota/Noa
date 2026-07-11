//! The io-thread entry point: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize and input
//! requests come in from the main thread over crossbeam channels.

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
use super::input_queue::PtyInput;
use super::ipc_tap::IpcOutputTap;
use super::overview::{OverviewPublish, flush_pending_overview_publish};
use super::redraw::{RedrawDecision, RedrawFloor};
use super::sidebar::SidebarPublish;

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
    pub(super) join: Option<std::thread::JoinHandle<()>>,
}

impl IoThreadHandle {
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

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
    input_rx: Receiver<PtyInput>,
    auto_approve_feedback_rx: Receiver<AutoApproveFeedback>,
    overview: OverviewPublish,
    sidebar: SidebarPublish,
    auto_approve: AutoApprovePublish,
    redraw_floor: RedrawFloor,
    ipc: Option<IpcOutputTap>,
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
        // no server is running (`ipc` is `None`) — the throttle gate is then
        // never consulted, so a disabled server costs nothing per feed.
        let mut last_ipc_push: Option<Instant> = None;
        // Per-pane, lock-free-after-extraction cache of last-sent viewport
        // row content hashes (F-6), keyed by viewport slot (`visible_rows()`
        // index) — `feed_terminal_batch` diffs against this under the same
        // lock hold it already extracts rows in, so `noa.output` only ever
        // carries rows whose content actually changed.
        let mut ipc_row_cache: Vec<u64> = Vec::new();
        // A deadline owed by a redraw currently withheld — by the window's
        // shared [`RedrawFloor`] or the synchronized-output (DECSET 2026)
        // cap. Bounds how long a fed-but-unpainted frame can sit (see
        // [`RedrawFloor::decide`]); "when this pane last repainted" itself
        // now lives on `redraw_floor`, shared with every other pane in the
        // window, rather than as a local here. `None` deadline means nothing
        // is owed and the select below blocks exactly as before.
        let mut redraw_deadline: Option<Instant> = None;
        let mut auto_approve_rescan_at: Option<Instant> = None;
        loop {
            // Fast path: poll every channel with `try_recv` before falling
            // back to a blocking `Select`. During sustained output the pty
            // channel is almost always ready, so rebuilding the five-op
            // `Select` per batch is pure overhead — while the control
            // channels are still polled every iteration, so a flood can't
            // starve shutdown, resize, or user input (^C must reach the
            // shell mid-flood).
            match shutdown_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let mut did_work = false;
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
                    write_pty_bytes(&writer, bytes.as_ref());
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
                        ipc.is_some(),
                        &mut last_ipc_push,
                        &mut ipc_row_cache,
                    );
                    auto_approve_rescan_at =
                        auto_approve_rescan_deadline(&auto_approve_state, Instant::now());
                    publish_pending_at = output.overview_publish_pending;
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
                    if let Some(rows) = output.ipc_output.take()
                        && let Some(tap) = ipc.as_ref()
                    {
                        tap.broadcaster.broadcast_output(tap.ipc_pane_id, rows);
                    }
                    if !output.pending_writes.is_empty() {
                        write_pty_bytes(&writer, &output.pending_writes);
                    }
                    let redraw = redraw_floor.decide(output.synchronized_output, Instant::now());
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

            // Wake at whichever owed deadline comes first: an overview
            // trailing flush (Fix B defect 1), a withheld redraw, or an
            // auto-approve stability rescan. `ready`/`ready_timeout` report
            // readiness without receiving; the fast-path drains above
            // complete the operation on the next iteration (a spurious
            // wake-up just loops back here).
            let next_deadline = [publish_pending_at, redraw_deadline, auto_approve_rescan_at]
                .into_iter()
                .flatten()
                .min();
            let mut sel = crossbeam_channel::Select::new();
            sel.recv(&shutdown_rx);
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
        join: Some(join),
    }
}
