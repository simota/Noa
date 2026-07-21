#[test]
fn title_from_osc() {
    let t = run(b"\x1b]0;my title\x07");
    assert_eq!(t.title, "my title");
}

// tab-title REQ-TTL-5: a title set within a prompt cycle binds to the cwd
// reported at that cycle's end, regardless of whether the shell's title hook
// fires before or after its cwd hook. Both orders leave the fingerprint equal
// to the live cwd, so the resolver keeps the fresh title.
#[test]
fn osc_title_fingerprint_is_independent_of_title_cwd_hook_order() {
    // Title hook BEFORE cwd hook (zsh appends its cwd hook after user precmd):
    // OSC 2 for /b lands while cwd is still the previous /a, then OSC 7 reports
    // /b and re-binds the fingerprint to /b.
    let title_then_cwd = run(b"\x1b]7;file://localhost/a\x07\
          \x1b]2;build\x07\
          \x1b]7;file://localhost/b\x07");
    assert_eq!(title_then_cwd.title, "build");
    assert_eq!(title_then_cwd.cwd.as_deref(), Some("/b"));
    assert_eq!(title_then_cwd.title_cwd.as_deref(), Some("/b"));

    // Cwd hook BEFORE title hook: OSC 7 reports /b, then OSC 2 binds directly
    // to the fresh /b.
    let cwd_then_title = run(b"\x1b]7;file://localhost/b\x07\x1b]2;build\x07");
    assert_eq!(cwd_then_title.title, "build");
    assert_eq!(cwd_then_title.cwd.as_deref(), Some("/b"));
    assert_eq!(cwd_then_title.title_cwd.as_deref(), Some("/b"));
}

// tab-title REQ-TTL-5 (#34): a title set once at launch and never re-emitted
// stays pinned to its original cwd; a later `cd` (OSC 7 with no intervening
// OSC 2) diverges from it, so the resolver treats the startup title as stale.
#[test]
fn osc_title_fingerprint_goes_stale_when_cwd_moves_without_a_new_title() {
    let t = run(b"\x1b]2;/repo\x07\
          \x1b]7;file://localhost/repo\x07\
          \x1b]7;file://localhost/repo/sub\x07");
    assert_eq!(t.title, "/repo");
    assert_eq!(t.cwd.as_deref(), Some("/repo/sub"));
    // Fingerprint stayed at the launch cwd — diverged from the live cwd.
    assert_eq!(t.title_cwd.as_deref(), Some("/repo"));
}

// tab-title REQ-TTL-5 (P2): the title→cwd rebind window is bounded to a single
// prompt cycle by the OSC 133 prompt boundary. A startup title whose cwd hook
// ran *before* it (fingerprint provisionally bound, rebind pending) must not
// keep re-binding to later prompts' cwds — the 133 mark expires the pending
// rebind, so a subsequent `cd` leaves the startup title stale.
#[test]
fn osc133_prompt_boundary_expires_the_pending_title_cwd_rebind() {
    let t = run(b"\x1b]7;file://localhost/a\x07\
          \x1b]2;startup\x07\
          \x1b]133;A\x07\
          \x1b]7;file://localhost/b\x07");
    assert_eq!(t.title, "startup");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    // The 133 mark cleared the pending flag, so the /b report did not re-bind:
    // the fingerprint stays at /a and the title is stale at /b.
    assert_eq!(t.title_cwd.as_deref(), Some("/a"));
}

// tab-title REQ-TTL-5 (P1): the rebind window must survive OSC 133;D and only
// close at 133;A, matching our zsh integration's per-precmd emission order
// (`133;D → OSC 7 → 133;A`, shell-integration/zsh/.zshrc:23). A title set by a
// user precmd hook that runs *before* the cwd hook must still fingerprint the
// post-cd cwd, so the fresh title stays live after `cd`.
#[test]
fn osc133_rebind_window_survives_command_end_and_closes_at_prompt_start() {
    // Prior prompt at /a, then a cd: user title hook fires first (OSC 2 while
    // cwd is still /a), then the noa precmd emits D → OSC 7 /b → A → B.
    let t = run(b"\x1b]7;file://localhost/a\x07\x1b]133;A\x07\
          \x1b]2;cargo b\x07\
          \x1b]133;D;0\x07\
          \x1b]7;file://localhost/b\x07\
          \x1b]133;A\x07\x1b]133;B\x07");
    assert_eq!(t.title, "cargo b");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    // D did not expire the rebind; the /b report re-bound the fingerprint.
    assert_eq!(t.title_cwd.as_deref(), Some("/b"));
}

// A 133 boundary between the title and an unchanged cwd report is harmless:
// the pending rebind is expired but the fingerprint already matches the live
// cwd, so the fresh title stays live.
#[test]
fn osc133_boundary_leaves_a_same_cwd_title_live() {
    let t = run(b"\x1b]7;file://localhost/x\x07\
          \x1b]2;build\x07\
          \x1b]133;A\x07\
          \x1b]7;file://localhost/x\x07");
    assert_eq!(t.title, "build");
    assert_eq!(t.cwd.as_deref(), Some("/x"));
    assert_eq!(t.title_cwd.as_deref(), Some("/x"));
}

// tab-title REQ-TTL-5 (P2): bash emits `133;D → OSC 7 → 133;A → 133;B → user
// OSC 2` (shell-integration/bash/noa.bash:30-41 runs `_noa_prompt` first in
// PROMPT_COMMAND), so a one-shot user title lands AFTER A and the A-only expiry
// never closed its window. 133;C (command start) must also expire, so a later
// `cd` cannot re-bind the stale title's fingerprint.
#[test]
fn osc133_command_start_expires_a_post_prompt_title_rebind() {
    let t = run(b"\x1b]7;file://localhost/a\x07\
          \x1b]133;A\x07\x1b]133;B\x07\
          \x1b]2;startup\x07\
          \x1b]133;C\x07\
          \x1b]133;D;0\x07\
          \x1b]7;file://localhost/b\x07\
          \x1b]133;A\x07");
    assert_eq!(t.title, "startup");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    // C expired the window before the /b report, so the fingerprint stayed /a.
    assert_eq!(t.title_cwd.as_deref(), Some("/a"));
}

// A title set mid-command (after 133;C) still binds to the cwd its command
// reports on exit: C precedes the OSC 2, so the window it opens is consumed by
// the following OSC 7 rather than being expired by C.
#[test]
fn osc133_mid_command_title_rebinds_to_the_reported_cwd() {
    let t = run(b"\x1b]133;C\x07\
          \x1b]2;ssh box\x07\
          \x1b]133;D;0\x07\
          \x1b]7;file://localhost/b\x07");
    assert_eq!(t.title, "ssh box");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    assert_eq!(t.title_cwd.as_deref(), Some("/b"));
}

// tab-title REQ-TTL-5 (P2): a shell with no OSC 133 integration still bounds
// the rebind window by an executed LF/CR — the Enter echo (or command output)
// that always intervenes between one prompt's title/cwd burst and the next
// prompt's OSC 7. So a startup title does not re-bind across the boundary.
#[test]
fn markless_line_control_expires_the_title_cwd_rebind() {
    let t = run(b"\x1b]7;file://localhost/a\x07\
          \x1b]2;startup\x07\
          \r\n\
          \x1b]7;file://localhost/b\x07");
    assert_eq!(t.title, "startup");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    // The CR/LF closed the window before the /b report — fingerprint stays /a.
    assert_eq!(t.title_cwd.as_deref(), Some("/a"));
}

// tab-title REQ-TTL-5 (P2): within a single prompt's contiguous escape burst
// (no CR/LF), a markless shell's OSC 2 then OSC 7 still rebinds — the zsh
// cwd-hook-after-title order after a `cd`, with no 133 marks.
#[test]
fn markless_same_burst_title_then_cwd_rebinds() {
    let t = run(b"\x1b]2;build\x07\x1b]7;file://localhost/b\x07");
    assert_eq!(t.title, "build");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    assert_eq!(t.title_cwd.as_deref(), Some("/b"));
}

#[test]
fn osc8_hyperlink_state_is_stored_on_printed_cells() {
    let t = run(b"\x1b]8;id=docs;https://example.test/docs\x1b\\AB\
          \x1b]8;;\x1b\\C");

    let link_id = cell(&t, 0, 0).hyperlink.expect("A should carry link");
    assert_eq!(cell(&t, 1, 0).hyperlink, Some(link_id));
    assert_eq!(cell(&t, 2, 0).hyperlink, None);
    assert_eq!(t.hyperlinks[link_id.get()].uri, "https://example.test/docs");
    assert_eq!(t.hyperlinks[link_id.get()].id.as_deref(), Some("docs"));
}

#[test]
fn osc8_repeated_link_dedupes_and_registry_growth_is_capped() {
    // The same target sent twice reuses one registry slot.
    let t = run(b"\x1b]8;;https://example.test\x07A\x1b]8;;\x07\
          \x1b]8;;https://example.test\x07B");
    assert_eq!(t.hyperlinks.len(), 1);
    assert_eq!(cell(&t, 0, 0).hyperlink, cell(&t, 1, 0).hyperlink);

    // Streaming unique URIs stops growing the registry at the cap; cells
    // printed past it carry no link instead of a bogus index.
    let mut t = Terminal::new(GridSize::new(20, 4));
    let mut s = Stream::new();
    for i in 0..(crate::terminal::HYPERLINK_REGISTRY_CAP + 10) {
        s.feed(format!("\x1b]8;;https://u{i}.test\x07x").as_bytes(), &mut t);
    }
    assert_eq!(t.hyperlinks.len(), crate::terminal::HYPERLINK_REGISTRY_CAP);
    assert_eq!(t.active().cursor.hyperlink, None);
}

#[test]
fn shell_mark_recording_is_capped() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    let mut s = Stream::new();
    for _ in 0..(crate::terminal::SHELL_MARK_CAP + 50) {
        s.feed(b"\x1b]133;A\x07", &mut t);
    }
    assert_eq!(t.shell_marks.len(), crate::terminal::SHELL_MARK_CAP);
}

#[test]
fn osc8_malformed_payload_is_ignored_without_mutating_active_link() {
    let t = run(b"\x1b]8;;https://example.test\x07A\x1b]8;missing-separator\x07B");

    assert_eq!(cell(&t, 0, 0).hyperlink, cell(&t, 1, 0).hyperlink);
    assert_eq!(t.hyperlinks.len(), 1);
    assert_eq!(t.hyperlinks[0].uri, "https://example.test");
}

#[test]
fn osc7_cwd_updates_from_file_uri_and_rejects_malformed_payloads() {
    let t = run(b"\x1b]7;file://localhost/Users/noa%20dev/project\x07\
          \x1b]7;file://localhost/%zz\x07");

    assert_eq!(t.cwd.as_deref(), Some("/Users/noa dev/project"));
}

#[test]
fn osc7_empty_value_resets_cwd_rather_than_ignoring_it() {
    // AC-OSC-1: `OSC 7 ; ST` (empty value) is a pwd reset, not `Malformed` —
    // it must clear an already-set cwd rather than leave it untouched.
    let t = run(b"\x1b]7;file://localhost/tmp\x07\x1b]7;\x07");

    assert!(t.cwd.is_none());
}

#[test]
fn osc7_non_local_host_is_ignored_leaving_prior_cwd_unchanged() {
    // AC-OSC-2: a hostname that can't possibly match this test-running
    // machine is rejected; the earlier accepted cwd survives untouched.
    let t = run(b"\x1b]7;file://localhost/first\x07\
          \x1b]7;file://evil-remote-host/tmp\x07");

    assert_eq!(t.cwd.as_deref(), Some("/first"));
}

#[test]
fn osc7_empty_and_localhost_hosts_are_always_accepted() {
    // AC-OSC-3: empty host and `localhost` both bypass hostname resolution
    // entirely, so this holds regardless of the real machine's name.
    let empty_host = run(b"\x1b]7;file:///Users/x\x07");
    let localhost = run(b"\x1b]7;file://localhost/Users/x\x07");

    assert_eq!(empty_host.cwd.as_deref(), Some("/Users/x"));
    assert_eq!(localhost.cwd.as_deref(), Some("/Users/x"));
}

#[test]
fn osc7_kitty_shell_cwd_scheme_is_accepted_as_a_raw_path() {
    // AC-OSC-4: the empty-host form.
    let t = run(b"\x1b]7;kitty-shell-cwd:///Users/x\x07");

    assert_eq!(t.cwd.as_deref(), Some("/Users/x"));
}

#[test]
fn osc7_kitty_shell_cwd_with_localhost_host_is_accepted() {
    let t = run(b"\x1b]7;kitty-shell-cwd://localhost/Users/x\x07");

    assert_eq!(t.cwd.as_deref(), Some("/Users/x"));
}

#[test]
fn osc7_kitty_shell_cwd_non_local_host_is_ignored_leaving_prior_cwd_unchanged() {
    // The host is validated through the same gate as `file://` (REQ-OSC-2).
    let t = run(b"\x1b]7;kitty-shell-cwd://localhost/first\x07\
          \x1b]7;kitty-shell-cwd://evil-remote-host/tmp\x07");

    assert_eq!(t.cwd.as_deref(), Some("/first"));
}

#[test]
fn osc7_kitty_shell_cwd_path_is_taken_raw_without_percent_decoding() {
    // kitty semantics (REQ-OSC-3): unlike `file://`, `%20` in the path stays
    // literal rather than being decoded to a space.
    let t = run(b"\x1b]7;kitty-shell-cwd:///Users/noa%20dev\x07");

    assert_eq!(t.cwd.as_deref(), Some("/Users/noa%20dev"));
}

#[test]
fn hostname_matches_local_accepts_case_insensitive_full_or_short_label_shapes() {
    // Pure-function coverage (REQ-OSC-2's match rule) that does not depend
    // on the real machine's hostname: a machine named `SG-H-0001` must match
    // itself, an FQDN, a differently-cased short form, and vice versa.
    use crate::osc::hostname_matches_local;

    assert!(hostname_matches_local("sg-h-0001", "SG-H-0001"));
    assert!(hostname_matches_local("SG-H-0001.local", "sg-h-0001"));
    assert!(hostname_matches_local("sg-h-0001", "SG-H-0001.local"));
    assert!(hostname_matches_local("", "SG-H-0001"));
    assert!(hostname_matches_local("localhost", "SG-H-0001"));
    assert!(!hostname_matches_local("evil-remote-host", "SG-H-0001"));
    assert!(!hostname_matches_local(
        "sg-h-0002.local",
        "SG-H-0001.local"
    ));
}

#[test]
fn osc133_prompt_marks_record_cursor_positions_and_exit_status() {
    let t = run(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\x1b]133;D;7\x07");

    assert_eq!(t.shell_marks.len(), 4);
    assert_eq!(t.shell_marks[0].kind, ShellIntegrationMarkKind::PromptStart);
    assert_eq!(t.shell_marks[0].point, crate::SelectionPoint::new(0, 0));
    assert_eq!(t.shell_marks[1].kind, ShellIntegrationMarkKind::InputStart);
    assert_eq!(t.shell_marks[1].point, crate::SelectionPoint::new(2, 0));
    assert_eq!(
        t.shell_marks[2].kind,
        ShellIntegrationMarkKind::CommandStart
    );
    assert_eq!(t.shell_marks[2].point, crate::SelectionPoint::new(5, 0));
    assert_eq!(t.shell_marks[3].kind, ShellIntegrationMarkKind::CommandEnd);
    assert_eq!(t.shell_marks[3].exit_status, Some(7));
}

#[test]
fn osc133_latest_command_start_marks_running_program() {
    assert!(!run(b"plain shell output").has_running_program());
    assert!(!run(b"\x1b]133;A\x07$ \x1b]133;B\x07").has_running_program());
    assert!(run(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07").has_running_program());
    assert!(
        !run(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\x1b]133;D;0\x07")
            .has_running_program()
    );
    assert!(
        !run(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\x1b]133;A\x07")
            .has_running_program()
    );
}

#[test]
fn scroll_to_prompt_jumps_between_prompt_marks() {
    // A 3-row screen with three prompts (OSC 133;A) separated by output, so
    // history scrolls and the prompts land at known absolute rows:
    // history = [p0, a, b, p1, c, d, p2] (indices 0..=6), prompts at 0/3/6.
    let mut t = Terminal::new(GridSize::new(20, 3));
    let mut s = Stream::new();
    s.feed(b"\x1b]133;A\x07p0\r\na\r\nb\r\n", &mut t);
    s.feed(b"\x1b]133;A\x07p1\r\nc\r\nd\r\n", &mut t);
    s.feed(b"\x1b]133;A\x07p2", &mut t);

    let prompt_rows: Vec<usize> = t
        .shell_marks
        .iter()
        .filter(|mark| mark.kind == ShellIntegrationMarkKind::PromptStart)
        .map(|mark| mark.point.y)
        .collect();
    assert_eq!(prompt_rows, vec![0, 3, 6]);

    // First cell pair of the top visible row, to identify which prompt line
    // is at the viewport top after a jump.
    let top_line = |t: &Terminal| -> String {
        let rows = t.primary.visible_rows();
        let row = &rows[0];
        row.cells[0]
            .text_chars()
            .chain(row.cells[1].text_chars())
            .collect()
    };

    t.scroll_viewport_to_bottom();
    assert_eq!(t.viewport_offset(), 0);

    // Prev from the bottom lands on the prompt just above the viewport top (p1).
    assert!(t.scroll_to_prompt(PromptJump::Prev));
    assert_eq!(t.viewport_offset(), 1);
    assert_eq!(top_line(&t), "p1");

    // Another Prev climbs to the oldest prompt (p0), clamped to the top.
    assert!(t.scroll_to_prompt(PromptJump::Prev));
    assert_eq!(t.viewport_offset(), 4);
    assert_eq!(top_line(&t), "p0");

    // No prompt above the top: no-op, viewport unchanged.
    assert!(!t.scroll_to_prompt(PromptJump::Prev));
    assert_eq!(t.viewport_offset(), 4);

    // Next walks back down through the prompts.
    assert!(t.scroll_to_prompt(PromptJump::Next));
    assert_eq!(t.viewport_offset(), 1);
    assert_eq!(top_line(&t), "p1");
}

#[test]
fn scroll_to_prompt_without_marks_is_a_noop() {
    let mut t = run_size(20, 3, b"hello\r\nworld\r\nfoo\r\nbar\r\n");
    let before = t.viewport_offset();
    assert!(!t.scroll_to_prompt(PromptJump::Prev));
    assert!(!t.scroll_to_prompt(PromptJump::Next));
    assert_eq!(t.viewport_offset(), before);
}

#[test]
fn osc_protocol_state_clears_on_full_reset() {
    let t = run(b"\x1b]7;file://localhost/tmp\x07\
          \x1b]8;;https://example.test\x07A\
          \x1b]133;A\x07\
          \x1bc");

    assert!(t.cwd.is_none());
    assert!(t.hyperlinks.is_empty());
    assert!(t.shell_marks.is_empty());
    assert_eq!(cell(&t, 0, 0).hyperlink, None);
}

#[test]
fn osc9_queues_a_notification_with_no_title() {
    let mut t = run(b"\x1b]9;build finished\x07");

    let notifications = t.take_pending_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].title, None);
    assert_eq!(notifications[0].body, "build finished");
}

#[test]
fn osc9_body_keeps_embedded_semicolons() {
    let mut t = run(b"\x1b]9;a;b;c\x1b\\");

    let notifications = t.take_pending_notifications();
    assert_eq!(notifications[0].body, "a;b;c");
}

#[test]
fn osc9_empty_body_queues_nothing() {
    let mut t = run(b"\x1b]9;\x07");
    assert!(t.take_pending_notifications().is_empty());
}

#[test]
fn osc9_4_progress_report_is_not_a_notification() {
    // ConEmu/Windows Terminal progress: `OSC 9;4;<state>;<pct>`. noa has no
    // progress UI, so it is silently ignored rather than notified.
    let mut t = run(b"\x1b]9;4;1;50\x07");
    assert!(t.take_pending_notifications().is_empty());
}

#[test]
fn osc9_4_progress_clear_is_not_a_notification() {
    let mut t = run(b"\x1b]9;4;0\x07");
    assert!(t.take_pending_notifications().is_empty());
}

#[test]
fn osc9_4x_body_is_still_a_notification() {
    // Starts with `4` but is not the `9;4;` progress form, so it notifies.
    let mut t = run(b"\x1b]9;4x\x07");
    let notifications = t.take_pending_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].body, "4x");
}

#[test]
fn osc777_notify_queues_title_and_body() {
    let mut t = run(b"\x1b]777;notify;Title;the body\x1b\\");

    let notifications = t.take_pending_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].title.as_deref(), Some("Title"));
    assert_eq!(notifications[0].body, "the body");
}

#[test]
fn osc777_notify_body_keeps_embedded_semicolons() {
    let mut t = run(b"\x1b]777;notify;T;a;b\x07");

    let notifications = t.take_pending_notifications();
    assert_eq!(notifications[0].title.as_deref(), Some("T"));
    assert_eq!(notifications[0].body, "a;b");
}

#[test]
fn osc777_ignores_non_notify_subcommands() {
    let mut t = run(b"\x1b]777;precmd;foo\x07");
    assert!(t.take_pending_notifications().is_empty());
}

#[test]
fn osc777_without_a_body_queues_nothing() {
    let mut t = run(b"\x1b]777;notify;just a title\x07");
    assert!(t.take_pending_notifications().is_empty());
}

#[test]
fn notification_queue_drops_the_oldest_past_the_cap() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    // 40 notifications into a queue capped at 32: the first 8 are evicted, so
    // the survivors are bodies 8..=39, oldest first.
    for i in 0..40 {
        s.feed(format!("\x1b]9;n{i}\x07").as_bytes(), &mut t);
    }

    let notifications = t.take_pending_notifications();
    assert_eq!(notifications.len(), 32);
    assert_eq!(notifications.first().unwrap().body, "n8");
    assert_eq!(notifications.last().unwrap().body, "n39");
}

#[test]
fn osc52_write_is_decoded_and_queued() {
    let mut t = run(b"\x1b]52;c;aGVsbG8=\x07");

    assert_eq!(t.take_pending_clipboard_writes(), vec!["hello".to_string()]);
    assert!(t.pending_writes.is_empty());
}

#[test]
fn osc52_rejects_query_by_default() {
    let mut t = run(b"\x1b]52;c;?\x07");

    assert!(t.take_pending_clipboard_writes().is_empty());
    assert!(t.take_pending_clipboard_reads().is_empty());
    assert!(t.pending_writes.is_empty());
}

#[test]
fn osc52_query_queues_a_read_request_when_allowed() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.osc52_policy.allow_read = true;
    let mut s = Stream::new();
    s.feed(b"\x1b]52;c;?\x07", &mut t);

    // The grid queues a read request rather than replying inline (it can't
    // read the system clipboard); no bytes go to the pty yet.
    assert_eq!(t.take_pending_clipboard_reads(), vec!["c".to_string()]);
    assert!(t.pending_writes.is_empty());
}

#[test]
fn osc52_read_reply_base64_encodes_the_clipboard_text() {
    // "hi" -> "aGk=", full ST-terminated OSC 52 reply.
    assert_eq!(
        Terminal::osc52_read_reply("c", "hi"),
        b"\x1b]52;c;aGk=\x1b\\".to_vec()
    );
    // Round-trips the write test's payload ("hello" -> "aGVsbG8=").
    assert_eq!(
        Terminal::osc52_read_reply("c", "hello"),
        b"\x1b]52;c;aGVsbG8=\x1b\\".to_vec()
    );
}

#[test]
fn osc52_primary_and_secondary_targets_map_to_the_clipboard() {
    // macOS has one system clipboard; `p`/`s` writes land there instead of
    // being silently dropped (Ghostty's fallback behavior).
    let mut t = run(b"\x1b]52;p;aGVsbG8=\x07");
    assert_eq!(t.take_pending_clipboard_writes(), vec!["hello".to_string()]);

    // A `p` query queues a read echoing the requested target.
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.osc52_policy.allow_read = true;
    let mut s = Stream::new();
    s.feed(b"\x1b]52;p;?\x07", &mut t);
    assert_eq!(t.take_pending_clipboard_reads(), vec!["p".to_string()]);

    // A target without any known selection char is still ignored.
    let mut t = run(b"\x1b]52;q;aGVsbG8=\x07");
    assert!(t.take_pending_clipboard_writes().is_empty());
}

#[test]
fn osc52_write_accepts_unpadded_base64() {
    // "hi" -> "aGk" without the trailing `=`.
    let mut t = run(b"\x1b]52;c;aGk\x07");
    assert_eq!(t.take_pending_clipboard_writes(), vec!["hi".to_string()]);

    // "hello" -> "aGVsbG8" without padding.
    let mut t = run(b"\x1b]52;c;aGVsbG8\x07");
    assert_eq!(t.take_pending_clipboard_writes(), vec!["hello".to_string()]);

    // A single leftover symbol can never encode a byte: still rejected.
    let mut t = run(b"\x1b]52;c;aGkA1\x07");
    assert!(t.take_pending_clipboard_writes().is_empty());
}

#[test]
fn osc52_default_limit_accepts_multi_kilobyte_payloads() {
    // A 64 KiB payload (well past the old 3 KiB cap) decodes and queues.
    let raw = vec![b'x'; 64 * 1024];
    let mut encoded = Vec::new();
    crate::osc::encode_base64(&raw, &mut encoded);
    let mut seq = b"\x1b]52;c;".to_vec();
    seq.extend_from_slice(&encoded);
    seq.push(0x07);

    let mut t = run(&seq);
    let writes = t.take_pending_clipboard_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].len(), 64 * 1024);
}

#[test]
fn osc52_flood_within_one_feed_keeps_only_the_last_write() {
    // Many OSC 52 writes delivered in a single feed (mirrors a large pty
    // batch coalescing tens of thousands of minimal writes): only the final
    // one should survive the queue, so the app layer never issues more than
    // one pasteboard syscall per drained batch.
    let mut bytes = Vec::new();
    for i in 0..500 {
        bytes.extend_from_slice(format!("\x1b]52;c;{}\x07", encode(&format!("n{i}"))).as_bytes());
    }
    let mut t = run(&bytes);
    assert_eq!(t.take_pending_clipboard_writes(), vec!["n499".to_string()]);

    fn encode(s: &str) -> String {
        let mut out = Vec::new();
        crate::osc::encode_base64(s.as_bytes(), &mut out);
        String::from_utf8(out).unwrap()
    }
}

#[test]
fn osc52_flood_does_not_affect_read_requests() {
    // Reads must never be coalesced away — each one owes its requester a pty
    // reply. Interleave writes and reads and confirm every read survives.
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.osc52_policy.allow_read = true;
    let mut s = Stream::new();
    s.feed(b"\x1b]52;c;aGVsbG8=\x07", &mut t);
    s.feed(b"\x1b]52;c;?\x07", &mut t);
    s.feed(b"\x1b]52;c;d29ybGQ=\x07", &mut t);
    s.feed(b"\x1b]52;p;?\x07", &mut t);

    assert_eq!(t.take_pending_clipboard_writes(), vec!["world".to_string()]);
    assert_eq!(
        t.take_pending_clipboard_reads(),
        vec!["c".to_string(), "p".to_string()]
    );
}

#[test]
fn osc52_policy_can_disable_writes_and_limit_payloads() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.osc52_policy.allow_write = false;
    let mut s = Stream::new();
    s.feed(b"\x1b]52;c;aGk=\x07", &mut t);
    assert!(t.take_pending_clipboard_writes().is_empty());

    t.osc52_policy.allow_write = true;
    t.osc52_policy.max_decoded_bytes = 1;
    s.feed(b"\x1b]52;c;aGk=\x07", &mut t);
    assert!(t.take_pending_clipboard_writes().is_empty());
}

#[test]
fn osc_palette_set_query_and_selected_reset() {
    let t = run(b"\x1b]4;1;#112233\x07\
          \x1b]4;1;?\x07\
          \x1b]104;1\x07\
          \x1b]4;1;?\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(
        t.pending_writes,
        b"\x1b]4;1;rgb:1111/2222/3333\x1b\\\
          \x1b]4;1;rgb:cdcd/0000/0000\x1b\\"
    );
}

#[test]
fn osc_palette_accepts_multiple_pairs_and_resets_all() {
    let t = run(b"\x1b]4;1;#010203;2;rgb:0404/0505/0606\x07\
          \x1b]104\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(t.colors.palette(2), None);
}

#[test]
fn osc_default_slots_set_query_and_reset() {
    let t = run(b"\x1b]10;#112233\x07\
          \x1b]11;rgb:4444/5555/6666\x07\
          \x1b]12;rgb:a/b/c\x07\
          \x1b]10;?\x07\
          \x1b]11;?\x07\
          \x1b]12;?\x07\
          \x1b]110\x07\
          \x1b]111\x07\
          \x1b]112\x07");

    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert_eq!(
        t.pending_writes,
        b"\x1b]10;rgb:1111/2222/3333\x1b\\\
          \x1b]11;rgb:4444/5555/6666\x1b\\\
          \x1b]12;rgb:aaaa/bbbb/cccc\x1b\\"
    );
}

#[test]
fn osc_default_queries_use_theme_defaults() {
    let t = run(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");

    assert_eq!(
        t.pending_writes,
        b"\x1b]10;rgb:e0e0/e0e0/e0e0\x1b\\\
          \x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\
          \x1b]12;rgb:e0e0/e0e0/e0e0\x1b\\"
    );
}

#[test]
fn terminal_colors_default_base_layer_matches_legacy_defaults() {
    let colors = crate::TerminalColors::default();

    assert_eq!(colors.base_default_fg(), DEFAULT_FG);
    assert_eq!(colors.base_default_bg(), DEFAULT_BG);
    assert_eq!(colors.base_cursor(), DEFAULT_CURSOR);
    assert_eq!(colors.base_palette(1), xterm_palette_color(1));
    assert_eq!(colors.default_fg(), None);
    assert_eq!(colors.default_bg(), None);
    assert_eq!(colors.cursor(), None);
    assert_eq!(colors.palette(1), None);
}

#[test]
fn terminal_set_base_colors_seeds_colors_without_clearing_dynamic_overrides() {
    let mut palette = xterm_palette();
    palette[1] = Rgb::new(0x10, 0x20, 0x30);
    let dynamic_fg = Rgb::new(0x01, 0x02, 0x03);
    let dynamic_palette = Rgb::new(0x04, 0x05, 0x06);
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.colors.set_default_fg(dynamic_fg);
    t.colors.set_palette(1, dynamic_palette);

    t.set_base_colors(
        Rgb::new(0xaa, 0xbb, 0xcc),
        Rgb::new(0x11, 0x22, 0x33),
        Rgb::new(0x44, 0x55, 0x66),
        palette,
    );

    assert_eq!(t.colors.base_default_fg(), Rgb::new(0xaa, 0xbb, 0xcc));
    assert_eq!(t.colors.base_default_bg(), Rgb::new(0x11, 0x22, 0x33));
    assert_eq!(t.colors.base_cursor(), Rgb::new(0x44, 0x55, 0x66));
    assert_eq!(t.colors.base_palette(1), Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(t.colors.default_fg(), Some(dynamic_fg));
    assert_eq!(t.colors.palette(1), Some(dynamic_palette));
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
}

#[test]
fn osc_11_and_color_queries_report_active_base_colors() {
    let mut palette = xterm_palette();
    palette[1] = Rgb::new(0x10, 0x20, 0x30);
    let mut t = run_with_base_colors(
        b"\x1b]4;1;?\x07\
          \x1b]10;?\x07\
          \x1b]11;?\x07\
          \x1b]12;?\x07",
        Rgb::new(0x0a, 0x0b, 0x0c),
        Rgb::new(0xaa, 0xbb, 0xcc),
        Rgb::new(0x44, 0x55, 0x66),
        palette,
    );

    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]4;1;rgb:1010/2020/3030\x1b\\\
          \x1b]10;rgb:0a0a/0b0b/0c0c\x1b\\\
          \x1b]11;rgb:aaaa/bbbb/cccc\x1b\\\
          \x1b]12;rgb:4444/5555/6666\x1b\\"
    );
}

#[test]
fn osc_resets_restore_active_base_colors() {
    let mut palette = xterm_palette();
    palette[1] = Rgb::new(0x12, 0x34, 0x56);
    let mut t = run_with_base_colors(
        b"\x1b]4;1;#010203\x07\
          \x1b]10;#040506\x07\
          \x1b]11;#070809\x07\
          \x1b]12;#0a0b0c\x07\
          \x1b]104;1\x07\
          \x1b]110\x07\
          \x1b]111\x07\
          \x1b]112\x07\
          \x1b]4;1;?\x07\
          \x1b]10;?\x07\
          \x1b]11;?\x07\
          \x1b]12;?\x07",
        Rgb::new(0xde, 0xad, 0xbe),
        Rgb::new(0x13, 0x57, 0x9b),
        Rgb::new(0x24, 0x68, 0xac),
        palette,
    );

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]4;1;rgb:1212/3434/5656\x1b\\\
          \x1b]10;rgb:dede/adad/bebe\x1b\\\
          \x1b]11;rgb:1313/5757/9b9b\x1b\\\
          \x1b]12;rgb:2424/6868/acac\x1b\\"
    );
}

#[test]
fn full_reset_preserves_active_base_colors() {
    let mut palette = xterm_palette();
    palette[2] = Rgb::new(0x21, 0x43, 0x65);
    let mut t = run_with_base_colors(
        b"\x1b]4;2;#010203\x07\
          \x1b]10;#040506\x07\
          \x1b]11;#070809\x07\
          \x1b]12;#0a0b0c\x07\
          \x1bc\
          \x1b]4;2;?\x07\
          \x1b]10;?\x07\
          \x1b]11;?\x07\
          \x1b]12;?\x07",
        Rgb::new(0x90, 0x91, 0x92),
        Rgb::new(0x30, 0x31, 0x32),
        Rgb::new(0x70, 0x71, 0x72),
        palette,
    );

    assert_eq!(t.colors.palette(2), None);
    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]4;2;rgb:2121/4343/6565\x1b\\\
          \x1b]10;rgb:9090/9191/9292\x1b\\\
          \x1b]11;rgb:3030/3131/3232\x1b\\\
          \x1b]12;rgb:7070/7171/7272\x1b\\"
    );
}

#[test]
fn osc_color_rejects_malformed_without_mutation_or_reply() {
    let t = run(b"\x1b]4;256;#112233\x07\
          \x1b]4;1;#bad\x07\
          \x1b]10;#010203;#040506\x07\
          \x1b]11;rgb:12//34\x07\
          \x1b]12;not-a-color\x07\
          \x1b]110;unexpected\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert!(t.pending_writes.is_empty());
}
