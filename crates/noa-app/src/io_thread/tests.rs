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
        preview_lines: Arc::new(AtomicUsize::new(noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES)),
    }
}

fn feed_terminal_raw(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
    raw_attach: &RawAttachTap,
) -> TerminalOutput {
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    feed_terminal_batch(
        terminal,
        stream,
        bytes,
        std::iter::empty::<&[u8]>(),
        &test_overview_publish(),
        &mut None,
        &test_sidebar_publish(false),
        &mut None,
        &auto_approve,
        &mut crate::auto_approve::AutoApproveState::default(),
        false,
        &mut None,
        &mut IpcRowCache::default(),
        raw_attach,
    )
}

#[derive(Default)]
struct RecordingRawOutput {
    chunks: Mutex<Vec<Vec<u8>>>,
    closed: AtomicBool,
}

impl RawAttachOutput for RecordingRawOutput {
    fn send(&self, bytes: Vec<u8>) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.chunks.lock().push(bytes);
        true
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[test]
fn raw_attach_seed_and_live_output_have_an_atomic_terminal_lock_boundary() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    let raw_attach = RawAttachTap::default();
    let output = Arc::new(RecordingRawOutput::default());
    {
        let mut source = terminal.lock();
        noa_vt::Stream::new().feed(b"A", &mut *source);
    }

    // Hold the exact lock open_attach uses while a PTY feed attempts to land.
    // Registration + snapshot happen first; the blocked byte must therefore
    // appear only in the live stream, never in neither side of the boundary.
    let guard = terminal.lock();
    let ready = Arc::new(std::sync::Barrier::new(2));
    let feed_terminal = terminal.clone();
    let feed_tap = raw_attach.clone();
    let feed_ready = ready.clone();
    let feed = std::thread::spawn(move || {
        let mut stream = noa_vt::Stream::new();
        feed_ready.wait();
        feed_terminal_raw(&feed_terminal, &mut stream, b"B", &feed_tap);
    });
    ready.wait();
    let seed = raw_attach
        .register_test_and_seed(7, output.clone(), &guard)
        .unwrap();
    drop(guard);
    feed.join().unwrap();

    assert_eq!(output.chunks.lock().as_slice(), &[b"B".to_vec()]);
    let mut replica = Terminal::new(GridSize::new(8, 2));
    let mut replica_stream = noa_vt::Stream::new();
    replica_stream.feed(&seed, &mut replica);
    for chunk in output.chunks.lock().iter() {
        replica_stream.feed(chunk, &mut replica);
    }
    let source = terminal.lock();
    assert_eq!(replica.primary.grid[0].cells, source.primary.grid[0].cells);
    assert_eq!(replica.primary.cursor.x, source.primary.cursor.x);
}

#[test]
fn raw_attach_seed_carries_partial_csi_and_utf8_parser_prefixes() {
    for (prefix, suffix) in [
        (b"\x1b[".as_slice(), b"31mR".as_slice()),
        ([0xe4, 0xbd].as_slice(), [0xa0].as_slice()),
    ] {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
        let raw_attach = RawAttachTap::default();
        let output = Arc::new(RecordingRawOutput::default());
        let mut source_stream = noa_vt::Stream::with_shared_parser(raw_attach.parser());

        feed_terminal_raw(&terminal, &mut source_stream, prefix, &raw_attach);
        let seed = {
            let source = terminal.lock();
            raw_attach
                .register_test_and_seed(7, output.clone(), &source)
                .unwrap()
        };
        feed_terminal_raw(&terminal, &mut source_stream, suffix, &raw_attach);

        assert_eq!(output.chunks.lock().as_slice(), &[suffix.to_vec()]);
        let mut replica = Terminal::new(GridSize::new(8, 2));
        let mut replica_stream = noa_vt::Stream::new();
        replica_stream.feed(&seed, &mut replica);
        for chunk in output.chunks.lock().iter() {
            replica_stream.feed(chunk, &mut replica);
        }
        let source = terminal.lock();
        assert_eq!(replica.primary.grid[0].cells, source.primary.grid[0].cells);
        assert_eq!(replica.primary.cursor.x, source.primary.cursor.x);
    }
}

struct BlockingRawOutput {
    entered: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

impl RawAttachOutput for BlockingRawOutput {
    fn send(&self, _bytes: Vec<u8>) -> bool {
        self.entered.wait();
        self.release.wait();
        true
    }

    fn close(&self) {}
}

#[test]
fn raw_attach_backpressure_never_holds_the_terminal_lock() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    let raw_attach = RawAttachTap::default();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    raw_attach
        .register_test(
            3,
            Arc::new(BlockingRawOutput {
                entered: entered.clone(),
                release: release.clone(),
            }),
        )
        .unwrap();

    let feed_terminal = terminal.clone();
    let feed_tap = raw_attach.clone();
    let feed = std::thread::spawn(move || {
        feed_terminal_raw(
            &feed_terminal,
            &mut noa_vt::Stream::new(),
            b"blocked",
            &feed_tap,
        );
    });
    entered.wait();
    let terminal_was_unlocked = terminal.try_lock().is_some();
    release.wait();
    feed.join().unwrap();

    assert!(
        terminal_was_unlocked,
        "lossless send must block only after releasing Terminal"
    );
}

#[test]
fn raw_attach_stale_generation_cannot_write_or_detach_the_new_generation() {
    let raw_attach = RawAttachTap::default();
    let first_output = Arc::new(RecordingRawOutput::default());
    let second_output = Arc::new(RecordingRawOutput::default());
    let (input, rx) = input_channel();

    raw_attach.register_test(1, first_output.clone()).unwrap();
    assert_eq!(
        raw_attach.queue_input(1, &input, b"first"),
        Ok(QueueInputResult::Queued)
    );
    assert!(raw_attach.detach(1));
    raw_attach.register_test(2, second_output).unwrap();

    assert_eq!(raw_attach.queue_input(1, &input, b"stale"), Err(()));
    assert!(!raw_attach.detach(1), "stale detach cleared generation 2");
    let current_bytes = b"\0\xff\x1b[<0;2;3M";
    assert_eq!(
        raw_attach.queue_input(2, &input, current_bytes),
        Ok(QueueInputResult::Queued)
    );
    assert!(first_output.closed.load(Ordering::SeqCst));
    assert_eq!(rx.recv().unwrap().as_ref(), b"first");
    assert_eq!(rx.recv().unwrap().as_ref(), current_bytes);
    assert!(rx.try_recv().is_err(), "stale bytes reached the PTY queue");
}

#[test]
fn raw_attach_pane_shutdown_rejects_every_later_generation() {
    let raw_attach = RawAttachTap::default();
    let output = Arc::new(RecordingRawOutput::default());
    let (input, _rx) = input_channel();
    raw_attach.register_test(1, output.clone()).unwrap();

    raw_attach.shutdown();

    assert!(output.closed.load(Ordering::SeqCst));
    assert!(
        raw_attach
            .register_test(2, Arc::new(RecordingRawOutput::default()))
            .is_err()
    );
    assert_eq!(raw_attach.queue_input(1, &input, b"closed"), Err(()));
    assert_eq!(raw_attach.queue_input(2, &input, b"closed"), Err(()));
}

struct FailedRawOutput;

impl RawAttachOutput for FailedRawOutput {
    fn send(&self, _bytes: Vec<u8>) -> bool {
        false
    }

    fn close(&self) {}
}

#[test]
fn raw_attach_backpressure_failure_detaches_the_generation() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    let raw_attach = RawAttachTap::default();
    raw_attach
        .register_test(9, Arc::new(FailedRawOutput))
        .unwrap();

    feed_terminal_raw(
        &terminal,
        &mut noa_vt::Stream::new(),
        b"overflow",
        &raw_attach,
    );

    assert!(raw_attach.sink().is_none());
    let (input, _rx) = input_channel();
    assert_eq!(raw_attach.queue_input(9, &input, b"stale"), Err(()));
}

#[test]
fn raw_attach_forwards_only_pty_bytes_not_terminal_generated_side_effects() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    terminal.lock().osc52_policy.allow_read = true;
    let raw_attach = RawAttachTap::default();
    let output = Arc::new(RecordingRawOutput::default());
    raw_attach.register_test(4, output.clone()).unwrap();
    let pty_bytes = b"\x1b[6n\x1b]52;c;?\x07";

    let terminal_output = feed_terminal_raw(
        &terminal,
        &mut noa_vt::Stream::new(),
        pty_bytes,
        &raw_attach,
    );

    assert_eq!(output.chunks.lock().as_slice(), &[pty_bytes.to_vec()]);
    assert_eq!(terminal_output.pending_writes, b"\x1b[1;1R");
    assert_eq!(terminal_output.pending_clipboard_reads, vec!["c"]);
}

/// A batch of pure report queries (a DSR probe / TUI capability poll) must
/// come back `display_dirty: false` — the spawn loop skips its redraw poke,
/// so a query round-trip burst never wakes the main thread to snapshot an
/// unchanged frame (the dominant term in the DSR p99 tail). Anything that
/// prints (or otherwise mutates visible state) must flip it back to `true`,
/// and the flag must reset between batches rather than stick.
#[test]
fn query_only_batches_are_not_display_dirty_but_printing_batches_are() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    let raw_attach = RawAttachTap::default();
    let mut stream = noa_vt::Stream::new();

    // DSR (CSI 6 n) + DA1 (CSI c): replies queued, nothing visible changed.
    let queries = feed_terminal_raw(&terminal, &mut stream, b"\x1b[6n\x1b[c", &raw_attach);
    assert!(!queries.display_dirty);
    assert!(
        !queries.pending_writes.is_empty(),
        "the replies themselves must still be queued"
    );

    // Printing dirties the display; the previous batch must not have latched
    // the flag off.
    let print = feed_terminal_raw(&terminal, &mut stream, b"hi", &raw_attach);
    assert!(print.display_dirty);

    // And a following pure-query batch resets to clean again.
    let queries_again = feed_terminal_raw(&terminal, &mut stream, b"\x1b[6n", &raw_attach);
    assert!(!queries_again.display_dirty);
}

/// A batch mixing a query with visible output (echoed keystroke + DSR, the
/// common interactive shape) must stay dirty — the conservative direction:
/// misclassification may only ever cost a spurious repaint, never a stale
/// frame.
#[test]
fn mixed_query_and_output_batches_stay_display_dirty() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
    let raw_attach = RawAttachTap::default();
    let mut stream = noa_vt::Stream::new();

    let mixed = feed_terminal_raw(&terminal, &mut stream, b"x\x1b[6n", &raw_attach);
    assert!(mixed.display_dirty);
    assert_eq!(mixed.pending_writes, b"\x1b[1;2R");
}

/// Drives `feed_terminal_batch` directly (not through the `feed_terminal`
/// test wrapper, which always passes `ipc_active: false`) — for F-6's row
/// diff behavior.
#[allow(clippy::too_many_arguments)]
fn feed_terminal_ipc(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
    overview: &OverviewPublish,
    last_overview_publish: &mut Option<Instant>,
    sidebar: &SidebarPublish,
    last_sidebar_publish: &mut Option<Instant>,
    last_ipc_push: &mut Option<Instant>,
    ipc_row_cache: &mut IpcRowCache,
) -> TerminalOutput {
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    let mut auto_approve_state = crate::auto_approve::AutoApproveState::default();
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
        true,
        last_ipc_push,
        ipc_row_cache,
        &RawAttachTap::default(),
    )
}

// ---- F-6: IPC output row diff ----

#[test]
fn ipc_output_first_feed_sends_the_full_viewport_as_a_diff() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    let output = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"hello",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );

    let rows = output.ipc_output.expect("first feed sends a diff").lines;
    assert_eq!(rows.len(), 4, "every viewport row is new on the first push");
}

#[test]
fn ipc_output_only_diffs_rows_whose_content_actually_changed() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    // First feed seeds the cache with all four rows.
    let first = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"one\r\ntwo\r\nthree",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    assert!(first.ipc_output.is_some());

    // Past the throttle window, only row 0 (cursor still at row 0) actually
    // changes.
    last_ipc_push = Some(Instant::now() - super::ipc_tap::OUTPUT_PUSH_MIN_INTERVAL);
    let second = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"\x1b[Hedited",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    let rows = second
        .ipc_output
        .expect("changed content produces a diff")
        .lines;
    assert_eq!(
        rows.len(),
        1,
        "only the row that actually changed is resent"
    );
    assert_eq!(rows[0].row, 0);
}

/// R-3: when the viewport's absolute base shifts (scrollback growth), a
/// slot whose *content* happens to be unchanged must still be resent — its
/// absolute row number moved, and a hash-only cache keyed purely by slot
/// would wrongly suppress it, leaving the client's row indices stale.
#[test]
fn ipc_output_resends_every_row_when_the_viewport_base_shifts_even_with_identical_content() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    // Fill all four viewport rows with identical content; base is 0.
    let first = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"same\r\nsame\r\nsame\r\nsame",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    let first_rows = first.ipc_output.expect("first feed sends a diff").lines;
    assert_eq!(first_rows.len(), 4);
    assert_eq!(
        first_rows.iter().map(|r| r.row).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );

    // One more identical line scrolls the viewport by exactly one row: every
    // visible slot still holds "same" content, but the absolute row base
    // moved from 0 to 1. Past the throttle window so the push isn't
    // suppressed for an unrelated reason.
    last_ipc_push = Some(Instant::now() - super::ipc_tap::OUTPUT_PUSH_MIN_INTERVAL);
    let second = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"\r\nsame",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    let second_rows = second
        .ipc_output
        .expect("a base shift must resend rows even though their content is unchanged")
        .lines;
    assert_eq!(
        second_rows.len(),
        4,
        "the whole viewport resends with fresh absolute indices"
    );
    assert_eq!(
        second_rows.iter().map(|r| r.row).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn ipc_output_row_ids_do_not_reuse_evicted_scrollback_coordinates() {
    let mut terminal = Terminal::new(GridSize::new(80, 4));
    let mut bytes = Vec::new();
    for i in 0..2_000 {
        bytes.extend_from_slice(format!("line-{i:04}-{}\r\n", "x".repeat(68)).as_bytes());
    }
    noa_vt::Stream::new().feed(&bytes, &mut terminal);
    terminal.set_scrollback_limit_bytes(1);

    let oldest = terminal.selection_rows_evicted() as u64;
    assert!(oldest > 0, "test setup must evict retained scrollback");

    let rows = compute_ipc_row_diff(&terminal, &mut IpcRowCache::default()).lines;
    assert_eq!(
        rows.first().map(|row| row.row),
        Some(oldest + terminal.active().visible_row_base() as u64),
        "push row ids must stay in the same session-absolute coordinate space as getGrid"
    );
}

#[test]
fn ipc_output_row_ids_advance_when_scrollback_is_disabled() {
    let mut terminal = Terminal::new(GridSize::new(80, 4));
    terminal.set_scrollback_limit_bytes(0);
    let mut stream = noa_vt::Stream::new();
    stream.feed(b"one\r\ntwo\r\nthree\r\nfour", &mut terminal);
    let mut cache = IpcRowCache::default();

    let before = compute_ipc_row_diff(&terminal, &mut cache);
    stream.feed(b"\r\nfive", &mut terminal);
    let after = compute_ipc_row_diff(&terminal, &mut cache);

    assert_eq!(
        after.coordinate_generation, before.coordinate_generation,
        "ordinary scrolling must stay in the same coordinate generation"
    );
    assert_eq!(
        after.lines.iter().map(|row| row.row).collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "discarded rows must still advance session-absolute row ids"
    );
}

#[test]
fn ipc_output_resends_full_viewport_with_a_new_generation_after_clear_scrollback() {
    let mut terminal = Terminal::new(GridSize::new(80, 4));
    noa_vt::Stream::new().feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive", &mut terminal);
    let mut cache = IpcRowCache::default();

    let before = compute_ipc_row_diff(&terminal, &mut cache);
    terminal.clear_scrollback();
    let after = compute_ipc_row_diff(&terminal, &mut cache);

    assert_ne!(after.coordinate_generation, before.coordinate_generation);
    assert_eq!(after.lines.len(), terminal.active().visible_rows().len());
}

#[test]
fn forced_ipc_output_refresh_notifies_idle_subscribers_after_generation_change() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    {
        let mut terminal = terminal.lock();
        noa_vt::Stream::new().feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive", &mut *terminal);
    }
    let mut cache = IpcRowCache::default();
    let generation_before =
        compute_ipc_row_diff(&terminal.lock(), &mut cache).coordinate_generation;
    terminal.lock().clear_scrollback();

    let broadcaster = noa_ipc::push::Broadcaster::new();
    let (conn_id, queue) = broadcaster.register_connection();
    broadcaster
        .add_subscription(conn_id, noa_ipc::push::EventMask::OUTPUT, None)
        .expect("connection was just registered");
    let tap = super::ipc_tap::IpcOutputTap {
        broadcaster,
        ipc_pane_id: 42,
    };
    let mut last_ipc_push = Some(Instant::now());

    super::ipc_tap::force_ipc_output_refresh(&terminal, &tap, &mut last_ipc_push, &mut cache);

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 1);
    match &notifications[0] {
        noa_ipc::push::QueuedNotification::Output {
            coordinate_generation,
            lines,
            ..
        } => {
            assert_ne!(*coordinate_generation, generation_before);
            assert_eq!(lines.len(), terminal.lock().active().visible_rows().len());
        }
        other => panic!("expected an Output notification, got {other:?}"),
    }
}

#[test]
fn ipc_output_is_none_when_nothing_changed_or_the_tap_is_inactive() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;

    // Tap inactive: `feed_terminal` always passes `ipc_active: false`.
    let inactive = feed_terminal(
        &terminal,
        &mut stream,
        b"hello",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
    );
    assert!(inactive.ipc_output.is_none());

    // Tap active but still inside the throttle window: no push at all, not
    // even an empty one.
    let mut last_ipc_push = Some(Instant::now());
    let mut ipc_row_cache = IpcRowCache::default();
    let throttled = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"more",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    assert!(throttled.ipc_output.is_none());
}

/// R-3: every spawned pane now carries an `IpcOutputTap` unconditionally
/// (see `App::ipc_output_tap`) — the zero-work gate moved from "does a tap
/// exist" to `Broadcaster::has_output_subscriber_for(pane_id)`. A tap wired
/// to a real `Broadcaster` with zero output subscriptions must still drive
/// `feed_terminal_batch`'s `ipc_active` to `false`, so `ipc_output` stays
/// `None` and no per-feed row diff is computed.
#[test]
fn tap_present_but_no_output_subscriber_keeps_ipc_output_none() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    let broadcaster = noa_ipc::push::Broadcaster::new();
    let tap = IpcOutputTap {
        broadcaster: broadcaster.clone(),
        ipc_pane_id: 7,
    };
    assert!(
        !tap.broadcaster.has_output_subscriber_for(tap.ipc_pane_id),
        "no connection has subscribed yet"
    );

    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    let mut auto_approve_state = crate::auto_approve::AutoApproveState::default();
    let output = feed_terminal_batch(
        &terminal,
        &mut stream,
        b"hello".as_slice(),
        std::iter::empty::<&[u8]>(),
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        tap.broadcaster.has_output_subscriber_for(tap.ipc_pane_id),
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
    );
    assert!(
        output.ipc_output.is_none(),
        "a tap with zero output subscribers must do zero row-diff work"
    );

    // Once a connection subscribes, the same tap's broadcaster reports it —
    // proving the gate really does track subscriptions, not the tap.
    let (conn_id, _queue) = broadcaster.register_connection();
    let _ = broadcaster.add_subscription(conn_id, noa_ipc::push::EventMask::OUTPUT, None);
    assert!(tap.broadcaster.has_output_subscriber_for(tap.ipc_pane_id));
}

/// R-3: narrowing the gate to `has_output_subscriber_for(pane_id)` (rather
/// than the server-wide `has_output_subscribers()`) means a client that only
/// subscribed to pane A must not force pane B's io thread to do row-diff
/// work too — one client watching one pane must not tax every other
/// producing pane in the process.
#[test]
fn output_subscriber_for_one_pane_does_not_gate_open_for_another_pane() {
    let broadcaster = noa_ipc::push::Broadcaster::new();
    let (conn_id, _queue) = broadcaster.register_connection();
    let mut only_pane_a = std::collections::HashSet::new();
    only_pane_a.insert(1u64);
    let _ =
        broadcaster.add_subscription(conn_id, noa_ipc::push::EventMask::OUTPUT, Some(only_pane_a));

    assert!(
        broadcaster.has_output_subscriber_for(1),
        "pane 1 has a matching subscription"
    );
    assert!(
        !broadcaster.has_output_subscriber_for(2),
        "pane 2 has no matching subscription"
    );

    // Pane 2's feed does zero row-diff work: `ipc_output` stays `None` even
    // though pane 1's subscriber exists and `has_output_subscribers()` (the
    // old, server-wide gate) would have reported `true`.
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    let mut auto_approve_state = crate::auto_approve::AutoApproveState::default();
    let output = feed_terminal_batch(
        &terminal,
        &mut stream,
        b"hello".as_slice(),
        std::iter::empty::<&[u8]>(),
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        broadcaster.has_output_subscriber_for(2),
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
    );
    assert!(
        output.ipc_output.is_none(),
        "pane 2 has no subscriber, so no diff is computed"
    );
}

/// R-3: while a pane's gate is closed (no matching subscriber), the row-hash
/// cache is reset every feed (`decide_ipc_output_push`'s `Skip` arm) so it
/// can't hold stale hashes from before the pane went quiet. If a subscriber
/// appears later, the first push after the gate reopens must be a full
/// resend of the viewport, not a diff against an ancient cache.
#[test]
fn ipc_output_full_resends_after_a_subscriber_appears_following_a_period_with_none() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    let mut auto_approve_state = crate::auto_approve::AutoApproveState::default();

    // Gate open: first feed sends the full viewport and seeds the cache.
    let first = feed_terminal_batch(
        &terminal,
        &mut stream,
        b"one\r\ntwo\r\nthree".as_slice(),
        std::iter::empty::<&[u8]>(),
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        true,
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
    );
    assert_eq!(
        first
            .ipc_output
            .expect("first feed sends a diff")
            .lines
            .len(),
        4
    );

    // Gate closes (subscriber went away) — content still changes underneath,
    // but nothing is pushed and the cache is reset, not left stale.
    let closed = feed_terminal_batch(
        &terminal,
        &mut stream,
        b"\r\nfour".as_slice(),
        std::iter::empty::<&[u8]>(),
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        false,
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
    );
    assert!(
        closed.ipc_output.is_none(),
        "gate is closed, nothing is pushed"
    );

    // Gate reopens: a new subscriber wants the pane's current state, which
    // it has never seen — this must be a full resend, not a diff against the
    // stale pre-close cache.
    last_ipc_push = Some(Instant::now() - super::ipc_tap::OUTPUT_PUSH_MIN_INTERVAL);
    let reopened = feed_terminal_batch(
        &terminal,
        &mut stream,
        b"".as_slice(),
        std::iter::empty::<&[u8]>(),
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &auto_approve,
        &mut auto_approve_state,
        true,
        &mut last_ipc_push,
        &mut ipc_row_cache,
        &RawAttachTap::default(),
    );
    let rows = reopened
        .ipc_output
        .expect("gate reopening must produce a push")
        .lines;
    assert_eq!(
        rows.len(),
        4,
        "the full viewport resends, not just rows changed since the gate closed"
    );
}

/// R-1: a feed suppressed by the 16ms throttle gate must not be silently
/// dropped — it owes a trailing flush, and once that deadline is reached
/// (simulated here by calling `flush_pending_ipc_output` directly, exactly
/// as `spawn`'s loop does when its owed deadline elapses) subscribers must
/// still receive the burst's final rows.
#[test]
fn ipc_output_throttled_feed_eventually_flushes_the_final_rows() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 4))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(false);
    let mut last_sidebar_publish = None;
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    // First feed pushes immediately and seeds the cache.
    let first = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"one\r\ntwo\r\nthree",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    assert!(first.ipc_output.is_some());
    assert!(first.ipc_output_publish_pending.is_none());

    // A burst's tail lands inside the throttle window: no push now, but a
    // trailing-flush deadline is owed instead of the diff being dropped.
    let second = feed_terminal_ipc(
        &terminal,
        &mut stream,
        b"\x1b[Hedited",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );
    assert!(
        second.ipc_output.is_none(),
        "still inside the throttle window"
    );
    let deadline = second
        .ipc_output_publish_pending
        .expect("a suppressed push must owe a trailing flush");
    assert!(deadline > Instant::now() - super::ipc_tap::OUTPUT_PUSH_MIN_INTERVAL);

    // The owed deadline elapses with no further pty output — `spawn`'s loop
    // would call `flush_pending_ipc_output` here; drive it directly.
    let broadcaster = noa_ipc::push::Broadcaster::new();
    let (conn_id, queue) = broadcaster.register_connection();
    broadcaster
        .add_subscription(conn_id, noa_ipc::push::EventMask::OUTPUT, None)
        .expect("connection was just registered");
    let tap = super::ipc_tap::IpcOutputTap {
        broadcaster,
        ipc_pane_id: 42,
    };

    super::ipc_tap::flush_pending_ipc_output(
        &terminal,
        &tap,
        &mut last_ipc_push,
        &mut ipc_row_cache,
    );

    let notifications = queue.drain();
    assert_eq!(
        notifications.len(),
        1,
        "the trailing flush must push exactly one notification"
    );
    match &notifications[0] {
        noa_ipc::push::QueuedNotification::Output { pane_id, lines, .. } => {
            assert_eq!(*pane_id, 42);
            assert_eq!(
                lines.len(),
                1,
                "only the row edited during the throttle window"
            );
            assert_eq!(lines[0].row, 0);
        }
        other => panic!("expected an Output notification, got {other:?}"),
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
    let light = off
        .sidebar_upsert
        .expect("hidden first feed still publishes");
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
            .any(|line| { crate::session_store::preview_line_text(line).contains("second line") })
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

#[test]
fn visible_sidebar_preview_respects_configured_line_count() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 8))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let mut last_sidebar_publish = None;
    let sidebar = SidebarPublish {
        visible: Arc::new(AtomicBool::new(true)),
        preview_lines: Arc::new(AtomicUsize::new(3)),
    };

    let output = feed_terminal(
        &terminal,
        &mut stream,
        b"one\r\ntwo\r\nthree\r\nfour\r\nfive",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
    );

    let preview = output
        .sidebar_upsert
        .expect("visible feed publishes")
        .preview
        .expect("visible feed extracts preview");
    let lines: Vec<_> = preview
        .iter()
        .map(|line| crate::session_store::preview_line_text(line))
        .collect();
    assert_eq!(lines, vec!["three", "four", "five"]);
}

// Agent "intermediate output" — a spinner/status line rewritten in place
// (cursor-up + EL, no newline) — must flow into successive upserts'
// previews: this is what makes a busy agent's card read as live.
#[test]
fn preview_tracks_in_place_intermediate_output_rewrites() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 8))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(true);

    let preview_texts = |output: &TerminalOutput| -> Vec<String> {
        output
            .sidebar_upsert
            .as_ref()
            .expect("visible feed publishes")
            .preview
            .as_ref()
            .expect("visible feed extracts preview")
            .iter()
            .map(|line| crate::session_store::preview_line_text(line))
            .collect()
    };

    // Codex-style bottom UI: spinner, prompt, footer.
    let mut last_sidebar_publish = None;
    let output = feed_terminal(
        &terminal,
        &mut stream,
        b"Reviewing changes (1)\r\n> prompt\r\nfooter",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
    );
    assert!(
        preview_texts(&output)
            .iter()
            .any(|l| l.contains("Reviewing changes (1)"))
    );

    // The spinner ticks by rewriting its own row: cursor up ×2, clear the
    // line, print the new frame. A fresh throttle window (as after 100ms)
    // must publish the rewritten text.
    let mut last_sidebar_publish = None;
    let output = feed_terminal(
        &terminal,
        &mut stream,
        b"\x1b[2A\r\x1b[2KReviewing changes (2)\x1b[2B\r",
        &overview,
        &mut last_overview_publish,
        &sidebar,
        &mut last_sidebar_publish,
    );
    let lines = preview_texts(&output);
    assert!(
        lines.iter().any(|l| l.contains("Reviewing changes (2)")),
        "rewritten spinner must reach the preview: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("(1)")),
        "the stale frame must be gone: {lines:?}"
    );
}

#[test]
fn feed_terminal_preserves_utf8_split_across_pty_reads() {
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(4, 1))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let mut last_sidebar_publish = None;
    let bytes = "日".as_bytes();

    feed_terminal(
        &terminal,
        &mut stream,
        &bytes[..1],
        &overview,
        &mut last_overview_publish,
        &test_sidebar_publish(false),
        &mut last_sidebar_publish,
    );
    assert_eq!(
        terminal.lock().primary.grid[0].cells[0].ch,
        ' ',
        "an incomplete UTF-8 scalar must not print a replacement cell"
    );

    feed_terminal(
        &terminal,
        &mut stream,
        &bytes[1..],
        &overview,
        &mut last_overview_publish,
        &test_sidebar_publish(false),
        &mut last_sidebar_publish,
    );
    let term = terminal.lock();
    assert_eq!(term.primary.grid[0].cells[0].ch, '日');
    assert!(
        term.primary.grid[0].cells[0]
            .attrs
            .contains(noa_core::CellAttrs::WIDE)
    );
    assert!(
        term.primary.grid[0].cells[1]
            .attrs
            .contains(noa_core::CellAttrs::WIDE_SPACER)
    );
}

/// Guards the chunked-lock feed path (`feed_chunk_fair`, used internally by
/// `feed_terminal_batch`): splitting a batch across many separate
/// lock/unlock cycles — one per simulated `noa-pty` `READ_CHUNK` (64 KiB),
/// well past the old single-lock-covers-the-whole-batch model — must still
/// parse to the exact same `Terminal` state as feeding the identical bytes
/// as one unsplit chunk. The parser state (`Stream`) lives outside the
/// mutex, so relocking mid-parse must be invisible to the parse result; this
/// pins that invariant down with a batch comfortably past
/// `PTY_DATA_DRAIN_BYTE_LIMIT` (1 MiB), with a UTF-8 scalar and a CSI escape
/// sequence deliberately straddling two of the chunk boundaries so the
/// carried-over parser state at a lock boundary is actually exercised.
#[test]
fn feed_terminal_batch_chunked_locking_matches_single_lock_result() {
    const CHUNK: usize = 64 * 1024;

    let mut bytes = Vec::new();
    while bytes.len() < CHUNK - 1 {
        bytes.push(b'a' + (bytes.len() % 26) as u8);
    }
    // This 3-byte UTF-8 scalar straddles the chunk 0/1 boundary.
    bytes.extend_from_slice("日".as_bytes());
    while bytes.len() < 2 * CHUNK - 2 {
        bytes.push(b'a' + (bytes.len() % 26) as u8);
    }
    // This 5-byte CSI sequence straddles the chunk 1/2 boundary.
    bytes.extend_from_slice(b"\x1b[35m");
    bytes.extend_from_slice(b"styled\x1b[0m");
    // Pad well past `PTY_DATA_DRAIN_BYTE_LIMIT` (1 MiB) with CRLF-terminated
    // lines so cursor/scrollback state is nontrivial, not just one long row.
    while bytes.len() < 17 * CHUNK {
        bytes.extend_from_slice(b"the quick brown fox jumps\r\n");
    }
    assert!(bytes.len() > PTY_DATA_DRAIN_BYTE_LIMIT);

    let chunks: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
    let (&first, rest) = chunks.split_first().expect("at least one chunk");

    // "Unsplit": the whole batch fed as a single chunk (`rest` empty) — one
    // lock hold end to end, byte-for-byte the pre-chunking model.
    let single_terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut single_stream = noa_vt::Stream::new();
    feed_terminal(
        &single_terminal,
        &mut single_stream,
        &bytes,
        &test_overview_publish(),
        &mut None,
        &test_sidebar_publish(false),
        &mut None,
    );

    // "Chunked": the real drain path — one `feed_chunk_fair` lock/unlock per
    // 64 KiB chunk, same as a real sustained-flood batch.
    let chunked_terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut chunked_stream = noa_vt::Stream::new();
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    feed_terminal_batch(
        &chunked_terminal,
        &mut chunked_stream,
        first,
        rest.iter().copied(),
        &test_overview_publish(),
        &mut None,
        &test_sidebar_publish(false),
        &mut None,
        &auto_approve,
        &mut crate::auto_approve::AutoApproveState::default(),
        false,
        &mut None,
        &mut IpcRowCache::default(),
        &RawAttachTap::default(),
    );

    let single = single_terminal.lock();
    let chunked = chunked_terminal.lock();
    let single_grid: Vec<Vec<noa_grid::Cell>> = single
        .primary
        .grid
        .iter()
        .map(|row| row.cells.clone())
        .collect();
    let chunked_grid: Vec<Vec<noa_grid::Cell>> = chunked
        .primary
        .grid
        .iter()
        .map(|row| row.cells.clone())
        .collect();
    assert_eq!(
        single_grid, chunked_grid,
        "chunked-lock feed must parse to the identical grid content as a single-lock feed"
    );
    assert_eq!(single.primary.cursor.x, chunked.primary.cursor.x);
    assert_eq!(single.primary.cursor.y, chunked.primary.cursor.y);
    assert_eq!(
        single.primary.cursor.pending_wrap,
        chunked.primary.cursor.pending_wrap
    );
}

/// Same invariant as
/// `feed_terminal_batch_chunked_locking_matches_single_lock_result`, but for
/// the two dispatch kinds that test doesn't touch: OSC (title) and DCS
/// (DECRQSS). Both accumulate into a side buffer distinct from the grid
/// (`Terminal::title`, `Terminal::pending_writes`) up to their string
/// terminator, so a relock that clobbered or restarted that accumulator
/// between chunks — rather than just corrupting a grid cell — could easily
/// go unnoticed by a grid-only equivalence check.
#[test]
fn feed_terminal_batch_chunked_locking_matches_single_lock_for_osc_and_dcs() {
    const CHUNK: usize = 64 * 1024;

    let mut bytes = Vec::new();
    while bytes.len() < CHUNK - 4 {
        bytes.push(b'x');
    }
    // This OSC 2 (title) sequence straddles the chunk 0/1 boundary.
    bytes.extend_from_slice(b"\x1b]2;chunked-title\x07");
    while bytes.len() < 2 * CHUNK - 3 {
        bytes.push(b'y');
    }
    // This DECRQSS (DCS) query straddles the chunk 1/2 boundary.
    bytes.extend_from_slice(b"\x1bP$qm\x1b\\");
    while bytes.len() < 3 * CHUNK {
        bytes.push(b'z');
    }

    let single_terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut single_stream = noa_vt::Stream::new();
    feed_terminal(
        &single_terminal,
        &mut single_stream,
        &bytes,
        &test_overview_publish(),
        &mut None,
        &test_sidebar_publish(false),
        &mut None,
    );

    let chunked_terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut chunked_stream = noa_vt::Stream::new();
    let chunks: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
    let (&first, rest) = chunks.split_first().expect("at least one chunk");
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    feed_terminal_batch(
        &chunked_terminal,
        &mut chunked_stream,
        first,
        rest.iter().copied(),
        &test_overview_publish(),
        &mut None,
        &test_sidebar_publish(false),
        &mut None,
        &auto_approve,
        &mut crate::auto_approve::AutoApproveState::default(),
        false,
        &mut None,
        &mut IpcRowCache::default(),
        &RawAttachTap::default(),
    );

    let mut single = single_terminal.lock();
    let mut chunked = chunked_terminal.lock();
    assert_eq!(
        single.title, "chunked-title",
        "sanity: the OSC straddling the boundary must still be recognized"
    );
    assert_eq!(
        single.title, chunked.title,
        "an OSC straddling a chunk boundary must set the same title on both paths"
    );
    assert_eq!(
        single.take_pending_writes(),
        chunked.take_pending_writes(),
        "a DECRQSS DCS straddling a chunk boundary must queue the same reply on both paths"
    );
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

    // Releasing 2026 drops the suppression window from the sync cap to
    // the ordinary redraw floor.
    assert!(!output.synchronized_output);
    assert_eq!(
        decide_redraw(
            output.synchronized_output,
            Some(Instant::now() - REDRAW_MIN_INTERVAL),
            Instant::now()
        ),
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

// A pty flood parsed in many batches must not request one repaint per
// batch: outside synchronized output, redraws are floored to
// REDRAW_MIN_INTERVAL, with the trailing deadline guaranteeing the
// burst's final frame still paints.
#[test]
fn unsynchronized_redraws_are_floored_to_the_min_interval() {
    let now = Instant::now();
    // First-ever paint, or a paint older than the floor: draw now.
    assert_eq!(decide_redraw(false, None, now), RedrawDecision::Now);
    assert_eq!(
        decide_redraw(false, Some(now - REDRAW_MIN_INTERVAL), now),
        RedrawDecision::Now
    );
    // Inside the floor: hold, arming the trailing deadline.
    let recent = now - REDRAW_MIN_INTERVAL / 2;
    assert_eq!(
        decide_redraw(false, Some(recent), now),
        RedrawDecision::Suppress {
            deadline: recent + REDRAW_MIN_INTERVAL
        }
    );
}

// FIX 1: the redraw floor should track a window's actual monitor refresh
// rate instead of a hardcoded 120Hz assumption, so 60Hz displays don't earn
// ~2x more redraw wakes than frames they can show.
#[test]
fn redraw_floor_from_refresh_millihertz_derives_the_period_and_clamps() {
    // 120Hz → ~8.33ms, not the old flat 8ms constant.
    assert_eq!(
        redraw_floor_from_refresh_millihertz(Some(120_000)),
        Duration::from_nanos(1_000_000_000_000 / 120_000)
    );
    // 60Hz → ~16.67ms: the case this fix targets.
    assert_eq!(
        redraw_floor_from_refresh_millihertz(Some(60_000)),
        Duration::from_nanos(1_000_000_000_000 / 60_000)
    );
    // Unknown or nonsensical (0Hz) rates fall back to the pre-fix constant.
    assert_eq!(
        redraw_floor_from_refresh_millihertz(None),
        REDRAW_MIN_INTERVAL
    );
    assert_eq!(
        redraw_floor_from_refresh_millihertz(Some(0)),
        REDRAW_MIN_INTERVAL
    );
    // Implausibly high/low reported rates clamp instead of producing a floor
    // that busy-loops or visibly stalls the io thread.
    assert_eq!(
        redraw_floor_from_refresh_millihertz(Some(10_000_000)),
        Duration::from_millis(4)
    );
    assert_eq!(
        redraw_floor_from_refresh_millihertz(Some(1)),
        Duration::from_millis(33)
    );
}

// FIX 2: an N-pane split must not earn N floored redraw wakes per floor
// window — every pane in a window shares one `RedrawFloor` clock.
#[test]
fn redraw_floor_decide_is_shared_across_clones_of_the_same_window() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let pane_a = floor.clone();
    let pane_b = floor.clone();

    let t0 = Instant::now();
    // Pane A's batch is the window's first-ever paint: draws now.
    assert_eq!(pane_a.decide(false, t0), RedrawDecision::Now);
    // Pane B, in the same window, asks moments later — inside the floor
    // window A just recorded, so it must suppress rather than wake again.
    let t1 = t0 + Duration::from_millis(2);
    assert_eq!(
        pane_b.decide(false, t1),
        RedrawDecision::Suppress {
            deadline: t0 + Duration::from_millis(10)
        }
    );
}

// A pane's `set_min_interval` (main thread, FIX 1) must be visible to every
// clone sharing the window (FIX 2's whole point).
#[test]
fn redraw_floor_set_min_interval_is_visible_to_every_clone() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let other_pane = floor.clone();
    floor.set_min_interval(Duration::from_millis(20));

    let t0 = Instant::now();
    assert_eq!(other_pane.decide(false, t0), RedrawDecision::Now);
    let t1 = t0 + Duration::from_millis(15);
    assert_eq!(
        other_pane.decide(false, t1),
        RedrawDecision::Suppress {
            deadline: t0 + Duration::from_millis(20)
        },
        "the widened interval set on one clone must apply to another"
    );
}

// Every pane suppressed within the same floor window computes the identical
// shared deadline; without a winner-take-all guard they'd all fire their
// owed redraw in the same tick. `claim_deadline` must let exactly one pane
// through per deadline.
#[test]
fn redraw_floor_claim_deadline_lets_only_one_pane_through() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let pane_a = floor.clone();
    let pane_b = floor.clone();
    let pane_c = floor.clone();

    let deadline = Instant::now();
    assert!(pane_a.claim_deadline(deadline), "first claim wins");
    assert!(
        !pane_b.claim_deadline(deadline),
        "same instant already claimed"
    );
    assert!(
        !pane_c.claim_deadline(deadline),
        "same instant already claimed"
    );
    // A genuinely later redraw can still be claimed afterward.
    assert!(pane_b.claim_deadline(deadline + Duration::from_millis(1)));
}

// A user-input echo bypasses the redraw floor: even when the window painted
// moments ago (which would suppress an ordinary output batch), the echo's
// batch repaints now — a keystroke must never wait out the floor behind
// another pane's recent paint.
#[test]
fn redraw_floor_input_echo_bypasses_the_floor() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let t0 = Instant::now();
    assert_eq!(floor.decide(false, t0), RedrawDecision::Now);

    let t1 = t0 + Duration::from_millis(2);
    // Ordinary output inside the floor window: suppressed…
    assert!(matches!(
        floor.decide(false, t1),
        RedrawDecision::Suppress { .. }
    ));
    // …but an input echo is not.
    assert_eq!(floor.decide_input_echo(false, t1), RedrawDecision::Now);
}

// The bypassed paint must still land on the window's shared clock, so a
// sibling pane's next ordinary batch sees it and suppresses against it.
#[test]
fn redraw_floor_input_echo_records_on_the_shared_clock() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let sibling = floor.clone();

    let t0 = Instant::now();
    assert_eq!(floor.decide_input_echo(false, t0), RedrawDecision::Now);
    let t1 = t0 + Duration::from_millis(2);
    assert_eq!(
        sibling.decide(false, t1),
        RedrawDecision::Suppress {
            deadline: t0 + Duration::from_millis(10)
        }
    );
}

// Synchronized output (DECSET 2026) is an application-requested atomicity
// contract, not a pacing heuristic — an input echo mid-sync must keep
// deferring like any other batch (bounded by the suppression cap).
#[test]
fn redraw_floor_input_echo_does_not_bypass_synchronized_output() {
    let floor = RedrawFloor::new(Duration::from_millis(10));
    let t0 = Instant::now();
    assert_eq!(floor.decide(false, t0), RedrawDecision::Now);

    let t1 = t0 + Duration::from_millis(2);
    assert_eq!(
        floor.decide_input_echo(true, t1),
        RedrawDecision::Suppress {
            deadline: t0 + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION
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
fn capture_pty_bytes_appends_batches_verbatim() {
    let dir = std::env::temp_dir().join(format!("noa-capture-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cap.bin");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();

    assert!(capture_pty_bytes(
        &mut file,
        b"first",
        [b"a".as_ref(), b"b".as_ref()]
    ));
    assert!(capture_pty_bytes(
        &mut file,
        b"|second",
        std::iter::empty::<&[u8]>()
    ));

    assert_eq!(std::fs::read(&path).unwrap(), b"firstab|second");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drain_queued_pty_data_preserves_data_before_terminal_event() {
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(noa_pty::PtyEvent::Data(b"queued".to_vec().into()))
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
    tx.send(noa_pty::PtyEvent::Data(b"a".to_vec().into()))
        .unwrap();
    tx.send(noa_pty::PtyEvent::Data(b"b".to_vec().into()))
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
fn overview_publish_reuses_unique_snapshot_slot_when_due() {
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
    let first_ptr = {
        let slot = overview.slot.lock();
        Arc::as_ptr(slot.as_ref().expect("first feed publishes"))
    };

    last_overview_publish = Some(Instant::now() - OVERVIEW_TILE_MIN_RENDER_INTERVAL);
    feed_terminal(
        &terminal,
        &mut stream,
        b"second",
        &overview,
        &mut last_overview_publish,
        &test_sidebar_publish(false),
        &mut None,
    );

    let slot = overview.slot.lock();
    let snap = slot.as_ref().expect("due feed publishes");
    assert_eq!(Arc::as_ptr(snap), first_ptr);
    assert_eq!(snap.rows[0].cells[5].ch, 's');
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
fn input_queue_is_bounded_and_nonblocking_for_ui_thread() {
    fn input(bytes: &[u8]) -> PtyInput {
        bytes.to_vec().into_boxed_slice()
    }

    let (queue, rx) = input_channel();
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        assert_eq!(queue.queue(input(b"x")), QueueInputResult::Queued);
    }

    // The overflowing write must return immediately (deferred to the
    // spillover thread), never block the ui thread.
    assert_eq!(queue.queue(input(b"y")), QueueInputResult::Deferred);
    assert_eq!(rx.len(), PTY_INPUT_QUEUE_CAPACITY);
}

#[test]
fn overflowing_input_defers_and_preserves_order_across_later_writes() {
    fn input(bytes: &[u8]) -> PtyInput {
        bytes.to_vec().into_boxed_slice()
    }

    let (queue, rx) = input_channel();
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        assert_eq!(queue.queue(input(b"x")), QueueInputResult::Queued);
    }

    // The overflowing paste defers — and a key typed right after it must
    // park *behind* it, not race the spillover thread for the next free
    // slot (the regression this queue exists to prevent).
    assert_eq!(queue.queue(input(b"paste")), QueueInputResult::Deferred);
    assert_eq!(queue.queue(input(b"key")), QueueInputResult::Deferred);

    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        assert_eq!(rx.recv().expect("queued input").as_ref(), b"x");
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1))
            .expect("deferred paste should be delivered")
            .as_ref(),
        b"paste"
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1))
            .expect("deferred key should follow the paste")
            .as_ref(),
        b"key"
    );
}

#[test]
fn input_pending_past_byte_cap_is_dropped_and_released_after_consumption() {
    fn input(bytes: &[u8]) -> PtyInput {
        bytes.to_vec().into_boxed_slice()
    }

    let (queue, rx) = input_channel();
    let raw_message_size = 1024 * 1024;
    for _ in 0..(PTY_INPUT_PENDING_BYTE_CAP / raw_message_size) {
        assert_eq!(
            queue.queue(vec![0u8; raw_message_size].into_boxed_slice()),
            QueueInputResult::Queued
        );
    }

    // The byte budget covers the channel itself, not just overflow. Another
    // message is rejected even though most message slots remain available.
    assert_eq!(queue.queue(input(b"x")), QueueInputResult::Dropped);

    for _ in 0..(PTY_INPUT_PENDING_BYTE_CAP / raw_message_size) {
        let received = rx.recv().expect("queued input");
        assert_eq!(received.as_ref().len(), raw_message_size);
    }

    // Consuming the queued bytes releases their reservation.
    assert_eq!(queue.queue(input(b"y")), QueueInputResult::Queued);
}

#[test]
fn input_pending_budget_charges_small_message_overhead() {
    let (queue, rx) = input_channel();
    let message_cap = PTY_INPUT_PENDING_BYTE_CAP / PTY_INPUT_PENDING_MIN_CHARGE;
    for index in 0..message_cap {
        let result = queue.queue(Vec::new().into_boxed_slice());
        assert_eq!(
            result,
            if index < PTY_INPUT_QUEUE_CAPACITY {
                QueueInputResult::Queued
            } else {
                QueueInputResult::Deferred
            }
        );
    }
    assert_eq!(
        queue.queue(Vec::new().into_boxed_slice()),
        QueueInputResult::Dropped,
        "empty frames must not grow container overhead without bound"
    );
    drop(rx);
}

// Regression guard for the `write_pty_bytes`/`write_pane_pty_bytes`/
// `queue_pane_pty_bytes` signature change from `&[u8]` to
// `impl Into<Box<[u8]>>` (double-copy elimination): every real caller shape
// — an owned `Vec<u8>` from key/paste encoding, a `Box<[u8]>`, and the two
// `&'static [u8]` literal callers (`focus_report_bytes`, `Signature::bytes`)
// — must still enqueue byte-identical output through the same `.into()`
// conversion those methods apply before calling `PtyInputQueue::queue`.
#[test]
fn queue_input_is_byte_identical_regardless_of_owned_source_type() {
    fn convert(bytes: impl Into<Box<[u8]>>) -> Box<[u8]> {
        bytes.into()
    }

    let (queue, rx) = input_channel();

    let from_vec: Vec<u8> = b"vec-owned".to_vec();
    let from_box: Box<[u8]> = b"box-owned".to_vec().into_boxed_slice();
    let from_static: &'static [u8] = b"static-literal";

    assert_eq!(
        queue.queue(convert(from_vec.clone())),
        QueueInputResult::Queued
    );
    assert_eq!(
        queue.queue(convert(from_box.clone())),
        QueueInputResult::Queued
    );
    assert_eq!(queue.queue(convert(from_static)), QueueInputResult::Queued);

    assert_eq!(
        rx.recv().expect("vec-sourced input").as_ref(),
        &from_vec[..]
    );
    assert_eq!(
        rx.recv().expect("box-sourced input").as_ref(),
        &from_box[..]
    );
    assert_eq!(
        rx.recv().expect("static-sourced input").as_ref(),
        from_static
    );
}

// AC-18 (NFR-2): git must never be spawned on the io read loop — it lives
// only in the dedicated `branch_poll` worker. Assert this module's source
// never spawns `git` (nor any `Command`). The needles are assembled at
// runtime so this test file does not trip its own scan.
#[test]
fn io_read_loop_never_spawns_git() {
    let source = include_str!("spawn.rs");
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
        ipc_output_refresh_tx: crossbeam_channel::bounded::<()>(1).0,
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
            ipc_output_refresh_tx: crossbeam_channel::bounded::<()>(1).0,
            join: Some(join),
        });
    }

    for handle in &mut handles {
        assert!(handle.shutdown_and_join_timeout(Duration::from_millis(500)));
        assert!(handle.join.is_none());
    }
}

// Item 6: the caller (the main thread on every pane close) must never block
// on the join. The io thread here only honors shutdown after outlasting a
// generous "caller must already have returned" budget, proving
// `shutdown_and_join` handed the wait off to a reaper instead of blocking.
#[test]
fn shutdown_and_join_does_not_block_the_caller() {
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let join = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = shutdown_rx.recv();
    });
    let handle = IoThreadHandle {
        shutdown_tx,
        ipc_output_refresh_tx: crossbeam_channel::bounded::<()>(1).0,
        join: Some(join),
    };

    let start = Instant::now();
    handle.shutdown_and_join();
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "shutdown_and_join must not block the caller on the io thread's join"
    );
}

/// Bolt perf harness (text-input hot path): enqueue+drain cost through
/// [`PtyInputQueue`] for keystroke-sized (1-4 byte) writes — the shape the
/// main thread pushes per key. `#[ignore]`d so `cargo test` stays fast; run
/// explicitly with:
/// `cargo test -p noa-app --offline io_thread::tests::bench_pty_input_queue_enqueue_drain -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_pty_input_queue_enqueue_drain() {
    const ITERS: u32 = 200_000;
    let (queue, rx) = input_channel();

    let start = Instant::now();
    for _ in 0..ITERS {
        // Mirrors `queue_pane_pty_bytes`'s current `bytes.to_vec().into_boxed_slice()`
        // pattern for an already-owned single-keystroke buffer.
        let owned: Vec<u8> = b"a".to_vec();
        let boxed: PtyInput = std::hint::black_box(owned.as_slice())
            .to_vec()
            .into_boxed_slice();
        assert_eq!(queue.queue(boxed), QueueInputResult::Queued);
        let _ = std::hint::black_box(rx.recv().unwrap());
    }
    let elapsed = start.elapsed();
    eprintln!(
        "bench_pty_input_queue_enqueue_drain: {:.1} ns/op ({ITERS} iters, {elapsed:?} total)",
        elapsed.as_nanos() as f64 / f64::from(ITERS)
    );
}

/// Bolt perf harness (text-input hot path, echo side): the io thread's
/// per-feed cost for a typical single-character shell echo (`feed_terminal`
/// under the terminal lock: VT parse + overview/sidebar/auto-approve
/// bookkeeping). `#[ignore]`d so `cargo test` stays fast; run explicitly
/// with:
/// `cargo test -p noa-app --offline io_thread::tests::bench_feed_terminal_echo -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_feed_terminal_echo() {
    const ITERS: u32 = 50_000;
    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(true);
    let mut last_sidebar_publish = None;

    // A plain typed-character echo, as a shell in cooked/raw echo mode sends
    // it straight back (the common case on every keystroke).
    let start = Instant::now();
    for _ in 0..ITERS {
        let _ = std::hint::black_box(feed_terminal(
            &terminal,
            &mut stream,
            std::hint::black_box(b"a"),
            &overview,
            &mut last_overview_publish,
            &sidebar,
            &mut last_sidebar_publish,
        ));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "bench_feed_terminal_echo[plain char]: {:.1} ns/op ({ITERS} iters, {elapsed:?} total)",
        elapsed.as_nanos() as f64 / f64::from(ITERS)
    );

    // A prompt-line rewrite, as line-editing programs (readline, Claude Code)
    // send on many keystrokes: cursor reposition + SGR color + text.
    let prompt_echo: &[u8] = b"\x1b[2K\x1b[1G\x1b[32m$ \x1b[0mecho hello world";
    let start = Instant::now();
    for _ in 0..ITERS {
        let _ = std::hint::black_box(feed_terminal(
            &terminal,
            &mut stream,
            std::hint::black_box(prompt_echo),
            &overview,
            &mut last_overview_publish,
            &sidebar,
            &mut last_sidebar_publish,
        ));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "bench_feed_terminal_echo[styled prompt line]: {:.1} ns/op ({ITERS} iters, {elapsed:?} total)",
        elapsed.as_nanos() as f64 / f64::from(ITERS)
    );
}

/// Bolt perf harness (io-lock chunking): measures `feed_terminal_batch`'s
/// per-call cost for a large multi-chunk drain — 16 chunks of 64 KiB (1 MiB,
/// matching `PTY_DATA_DRAIN_BYTE_LIMIT`), the sustained-flood case the
/// chunked-lock change (`feed_chunk_fair`) targets. `feed_terminal_batch`'s
/// public signature is unchanged by that change, so this same bench runs
/// unmodified against either the chunked-lock or the pre-change
/// single-lock-per-batch implementation — diff the two with `git stash`
/// (same tree, same path) rather than a worktree compare. `#[ignore]`d so
/// `cargo test` stays fast; run explicitly with:
/// `cargo test -p noa-app --offline io_thread::tests::bench_feed_terminal_batch_large_flood -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_feed_terminal_batch_large_flood() {
    const ITERS: u32 = 200;
    const CHUNK: usize = 64 * 1024;
    const NUM_CHUNKS: usize = 16; // 1 MiB, matching PTY_DATA_DRAIN_BYTE_LIMIT

    let mut line = Vec::new();
    while line.len() < CHUNK {
        line.extend_from_slice(b"the quick brown fox jumps over the lazy dog\r\n");
    }
    line.truncate(CHUNK);
    let full: Vec<u8> = std::iter::repeat_n(line.iter().copied(), NUM_CHUNKS)
        .flatten()
        .collect();
    let chunks: Vec<&[u8]> = full.chunks(CHUNK).collect();
    let (&first, rest) = chunks.split_first().expect("at least one chunk");

    let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
    let mut stream = noa_vt::Stream::new();
    let overview = test_overview_publish();
    let mut last_overview_publish = None;
    let sidebar = test_sidebar_publish(true);
    let mut last_sidebar_publish = None;
    let auto_approve = AutoApprovePublish {
        enabled: Arc::new(AtomicBool::new(false)),
        guards: Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        )),
    };
    let mut auto_approve_state = crate::auto_approve::AutoApproveState::default();
    let mut last_ipc_push = None;
    let mut ipc_row_cache = IpcRowCache::default();

    let start = Instant::now();
    for _ in 0..ITERS {
        let _ = std::hint::black_box(feed_terminal_batch(
            &terminal,
            &mut stream,
            std::hint::black_box(first),
            rest.iter().copied(),
            &overview,
            &mut last_overview_publish,
            &sidebar,
            &mut last_sidebar_publish,
            &auto_approve,
            &mut auto_approve_state,
            false,
            &mut last_ipc_push,
            &mut ipc_row_cache,
            &RawAttachTap::default(),
        ));
    }
    let elapsed = start.elapsed();
    let total_bytes = u64::from(ITERS) * full.len() as u64;
    eprintln!(
        "bench_feed_terminal_batch_large_flood: {:.1} us/call, {:.1} MB/s ({ITERS} iters x {} bytes, {elapsed:?} total)",
        elapsed.as_micros() as f64 / f64::from(ITERS),
        (total_bytes as f64 / elapsed.as_secs_f64()) / 1e6,
        full.len(),
    );
}

// ---- SpinTraffic: the hot-spin gate's sliding traffic gauge ----

/// A single small batch (a DSR reply, an echoed keystroke) arms the spin
/// for the next park, and it stays armed only within `HOT_SPIN_WINDOW`.
#[test]
fn spin_traffic_arms_on_small_recent_traffic_and_expires() {
    let t0 = Instant::now();
    let mut traffic = SpinTraffic::default();
    assert!(!traffic.wants_spin(t0), "no traffic yet");
    traffic.record(t0, 32);
    assert!(traffic.wants_spin(t0 + HOT_SPIN_WINDOW / 2));
    assert!(!traffic.wants_spin(t0 + HOT_SPIN_WINDOW), "gone quiet");
}

/// A serialized query/reply loop — small frames arriving every few tens of
/// µs, indefinitely — keeps the spin armed: each bucket's byte total stays
/// interactive-sized no matter how long the loop runs.
#[test]
fn spin_traffic_stays_armed_through_a_sustained_query_reply_loop() {
    let t0 = Instant::now();
    let mut traffic = SpinTraffic::default();
    let step = Duration::from_micros(50);
    let mut now = t0;
    for _ in 0..2000 {
        traffic.record(now, 10);
        now += step;
    }
    assert!(traffic.wants_spin(now));
}

/// A flood delivered in *small* pty read chunks (the parser outpacing the
/// reader) must disarm the spin: cumulative window bytes cross
/// `HOT_SPIN_MAX_BATCH` within the first few chunks even though every
/// individual batch is tiny.
#[test]
fn spin_traffic_disarms_during_a_small_chunk_flood() {
    let t0 = Instant::now();
    let mut traffic = SpinTraffic::default();
    let step = Duration::from_micros(20);
    let mut now = t0;
    for _ in 0..8 {
        traffic.record(now, 1024);
        now += step;
    }
    assert!(!traffic.wants_spin(now));
}

/// Bulk traffic cannot slip under the gate at a bucket boundary: the
/// previous bucket's bytes still count against the budget right after a
/// rotation.
#[test]
fn spin_traffic_bucket_rotation_carries_the_previous_window() {
    let t0 = Instant::now();
    let mut traffic = SpinTraffic::default();
    traffic.record(t0, 64 * 1024);
    // Just past one window: rotate, current resets, previous carries.
    let t1 = t0 + HOT_SPIN_WINDOW + Duration::from_micros(1);
    traffic.record(t1, 16);
    assert!(!traffic.wants_spin(t1));
}

/// A flood followed by ≥ two quiet windows fully resets the gauge, so the
/// next interactive exchange arms the spin again.
#[test]
fn spin_traffic_resets_after_two_quiet_windows() {
    let t0 = Instant::now();
    let mut traffic = SpinTraffic::default();
    traffic.record(t0, 1024 * 1024);
    let t1 = t0 + HOT_SPIN_WINDOW * 2;
    traffic.record(t1, 16);
    assert!(traffic.wants_spin(t1));
}
