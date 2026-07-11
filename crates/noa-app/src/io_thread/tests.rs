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
        guards: Arc::new(Mutex::new(crate::auto_approve::AutoApproveInputGuards::default())),
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

    let rows = output.ipc_output.expect("first feed sends a diff");
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
    let rows = second.ipc_output.expect("changed content produces a diff");
    assert_eq!(rows.len(), 1, "only the row that actually changed is resent");
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
    let first_rows = first.ipc_output.expect("first feed sends a diff");
    assert_eq!(first_rows.len(), 4);
    assert_eq!(first_rows.iter().map(|r| r.row).collect::<Vec<_>>(), vec![0, 1, 2, 3]);

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
        .expect("a base shift must resend rows even though their content is unchanged");
    assert_eq!(second_rows.len(), 4, "the whole viewport resends with fresh absolute indices");
    assert_eq!(second_rows.iter().map(|r| r.row).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
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
    assert!(second.ipc_output.is_none(), "still inside the throttle window");
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
    let tap = super::ipc_tap::IpcOutputTap { broadcaster, ipc_pane_id: 42 };

    super::ipc_tap::flush_pending_ipc_output(&terminal, &tap, &mut last_ipc_push, &mut ipc_row_cache);

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 1, "the trailing flush must push exactly one notification");
    match &notifications[0] {
        noa_ipc::push::QueuedNotification::Output { pane_id, lines, .. } => {
            assert_eq!(*pane_id, 42);
            assert_eq!(lines.len(), 1, "only the row edited during the throttle window");
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
fn input_overflow_past_byte_cap_is_dropped() {
    fn input(bytes: &[u8]) -> PtyInput {
        bytes.to_vec().into_boxed_slice()
    }

    let (queue, rx) = input_channel();
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        assert_eq!(queue.queue(input(b"x")), QueueInputResult::Queued);
    }

    // A parked write that alone exceeds the byte cap is refused outright
    // instead of growing the overflow buffer without bound.
    let huge = vec![0u8; PTY_INPUT_OVERFLOW_BYTE_CAP + 1].into_boxed_slice();
    assert_eq!(queue.queue(huge), QueueInputResult::Dropped);

    // The drop leaves the queue fully usable.
    assert_eq!(rx.recv().expect("queued input").as_ref(), b"x");
    assert_eq!(queue.queue(input(b"y")), QueueInputResult::Queued);
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
