// ── WP1: G0/G1 charset designation + DEC Special Graphics (AC-CS) ──────

#[test]
fn ac_cs_001_dec_special_graphics_g0() {
    // ESC ( 0 designates G0 as DEC Special Graphics; `q` draws a horizontal line.
    let t = run(b"\x1b(0q");
    assert_eq!(cell(&t, 0, 0).ch, '\u{2500}');
}

#[test]
fn ac_cs_002_redesignate_g0_back_to_ascii() {
    let t = run(b"\x1b(0\x1b(Bq");
    assert_eq!(cell(&t, 0, 0).ch, 'q');
}

#[test]
fn ac_cs_003_so_si_switches_between_g1_and_g0() {
    // ESC ) 0 designates G1 as DEC Special Graphics; SO shifts to G1, SI back to G0.
    let t = run(b"\x1b)0\x0eq\x0fq");
    assert_eq!(cell(&t, 0, 0).ch, '\u{2500}');
    assert_eq!(cell(&t, 1, 0).ch, 'q');
}

#[test]
fn ac_cs_004_dec_special_graphics_box_drawing_glyphs() {
    let t = run(b"\x1b(0jklmx");
    assert_eq!(
        row_text(&t, 0, 5),
        "\u{2518}\u{2510}\u{250c}\u{2514}\u{2502}"
    );
}

#[test]
fn ac_cs_005_ris_resets_charset_state() {
    let t = run(b"\x1b(0\x1bcq");
    assert_eq!(cell(&t, 0, 0).ch, 'q');
}

// ── FM-1 regression: existing ESC final-byte arms (dispatch_esc rebuilt to
// match on `esc.intermediates` for SCS) must still dispatch unchanged ──────

#[test]
fn esc_decsc_decrc_save_and_restore_cursor() {
    let t = run(b"\x1b[5;10H\x1b7\x1b[1;1H\x1b8");
    assert_eq!(t.primary.cursor.y, 4);
    assert_eq!(t.primary.cursor.x, 9);
}

#[test]
fn decsc_decrc_does_not_save_or_restore_cursor_visibility() {
    // DECTCEM is terminal-wide mode state, not part of what DECSC/DECRC
    // capture (xterm/ECMA-48; Ghostty's SavedCursor omits it too) — hiding
    // the cursor after DECSC must survive a DECRC.
    let t = run(b"\x1b7\x1b[?25l\x1b8");
    assert!(!t.primary.cursor.visible);
}

#[test]
fn decsc_decrc_does_not_save_or_restore_cursor_shape() {
    // Same reasoning for DECSCUSR: the shape set after DECSC must survive
    // a later DECRC.
    let t = run(b"\x1b7\x1b[3 q\x1b8");
    assert_eq!(t.primary.cursor.style, CursorStyle::BlinkingUnderline);
}

#[test]
fn esc_ri_reverse_index_moves_cursor_up() {
    let t = run(b"\x1b[5;1H\x1bM");
    assert_eq!(t.primary.cursor.y, 3);
}

#[test]
fn esc_ind_linefeed_moves_cursor_down_without_cr() {
    let t = run(b"\x1b[5;10H\x1bD");
    assert_eq!(t.primary.cursor.y, 5);
    assert_eq!(t.primary.cursor.x, 9);
}

#[test]
fn esc_nel_moves_cursor_down_and_to_column_one() {
    let t = run(b"\x1b[5;10H\x1bE");
    assert_eq!(t.primary.cursor.y, 5);
    assert_eq!(t.primary.cursor.x, 0);
}

// ── WP4: DECALN screen alignment test (AC-ALN) ──────────────────────────

#[test]
fn ac_aln_001_fills_screen_with_e() {
    let t = run_size(5, 3, b"\x1b#8");
    for y in 0..3 {
        assert_eq!(row_text(&t, y, 5), "EEEEE");
    }
}

#[test]
fn ac_aln_002_homes_cursor() {
    let t = run_size(5, 3, b"\x1b[3;4H\x1b#8");
    assert_eq!(t.primary.cursor.y, 0);
    assert_eq!(t.primary.cursor.x, 0);
}

#[test]
fn ac_aln_003_leaves_scroll_region_unchanged() {
    let t = run_size(5, 8, b"\x1b[2;5r\x1b#8");
    assert_eq!(t.primary.region.top, 1);
    assert_eq!(t.primary.region.bottom, 4);
}

#[test]
fn ac_aln_004_alt_screen_only_active_screen_is_filled() {
    let t = run_size(5, 3, b"main\x1b[?1049h\x1b#8");
    assert_eq!(active_row_text(&t, 0, 5), "EEEEE");
    // The fill went to the alt screen only; primary still holds "main".
    assert_eq!(row_text(&t, 0, 4), "main");
}

// ── WP5: DECSTR soft reset (AC-STR) ─────────────────────────────────────

#[test]
fn ac_str_001_soft_reset_clears_decom_reported_via_decrqm() {
    let t = run(b"\x1b[?6h\x1b[!p\x1b[?6$p");
    assert_eq!(t.pending_writes, b"\x1b[?6;2$y");
}

#[test]
fn ac_str_002_soft_reset_restores_full_screen_scroll_region() {
    let t = run_size(10, 8, b"\x1b[2;5r\x1b[!p");
    assert_eq!(t.primary.region.top, 0);
    assert_eq!(t.primary.region.bottom, 7);
}

#[test]
fn ac_str_003_soft_reset_resets_charset() {
    let t = run(b"\x1b(0\x1b[!pq");
    assert_eq!(cell(&t, 0, 0).ch, 'q');
}

#[test]
fn ac_str_004_soft_reset_leaves_screen_content_untouched() {
    let t = run(b"hello\x1b[!p");
    assert_eq!(row_text(&t, 0, 5), "hello");
}

#[test]
fn ac_str_005_soft_reset_clears_saved_cursor_to_default() {
    // Move + color, DECSC, DECSTR (which re-arms the saved cursor to
    // default), then DECRC must land on the default, not the DECSC'd state.
    let t = run(b"\x1b[5;10H\x1b[31m\x1b7\x1b[!p\x1b8");
    assert_eq!(t.primary.cursor.y, 0);
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.fg, Color::Default);
}

// ── WP2: mode 2027 grapheme clustering negotiation (AC-2027-005) ───────

#[test]
fn ac_2027_005_decrqm_reports_grapheme_clustering_state() {
    let t = run(b"\x1b[?2027h\x1b[?2027$p");
    assert_eq!(t.pending_writes, b"\x1b[?2027;1$y");

    let t = run(b"\x1b[?2027h\x1b[?2027l\x1b[?2027$p");
    assert_eq!(t.pending_writes, b"\x1b[?2027;2$y");
}

// ── WP3: cluster attachment under mode 2027 (AC-2027-001..004) ─────────

#[test]
fn ac_2027_001_zwj_family_emoji_clusters_into_one_cell() {
    // 👨‍👩‍👧‍👦 = man ZWJ woman ZWJ girl ZWJ boy, each base emoji width-2.
    let t = run_size(
        10,
        1,
        "\x1b[?2027h\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}X".as_bytes(),
    );

    assert_eq!(
        cell(&t, 0, 0).text(),
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"
    );
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(cell(&t, 1, 0).is_blank());
    // The cluster advanced the cursor by its one (wide) cell only; the
    // trailing 'X' lands right after it, not further down the row.
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn ac_2027_002_fitzpatrick_modifier_attaches_to_wide_lead_not_spacer() {
    // FM-2 fixture: 👍🏽 (thumbs-up + medium skin tone) lands right on the
    // wide lead's spacer boundary. The modifier must attach to the lead
    // cell, and the spacer cell must stay untouched (pure blank), or the
    // cursor desyncs from the rendered cluster.
    let t = run_size(10, 1, "\x1b[?2027h\u{1F44D}\u{1F3FD}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "\u{1F44D}\u{1F3FD}");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(cell(&t, 1, 0).is_blank());
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn ac_2027_003_regional_indicator_pair_clusters_into_one_flag_cell() {
    // 🇯🇵 = REGIONAL INDICATOR J + REGIONAL INDICATOR P, each width-1
    // standalone; the second attaches to the first instead of printing
    // into its own cell, and the completed flag renders two cells wide.
    let t = run_size(10, 1, "\x1b[?2027h\u{1F1EF}\u{1F1F5}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "\u{1F1EF}\u{1F1F5}");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn ac_2027_006_vs16_promotes_narrow_symbol_to_wide() {
    // ☀ (U+2600, width 1) + VS16 requests emoji presentation, which renders
    // two cells wide; the cell must widen and the cursor step past the
    // claimed spacer so following text does not collide with the glyph.
    let t = run_size(10, 1, "\x1b[?2027h\u{2600}\u{FE0F}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "\u{2600}\u{FE0F}");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn ac_2027_007_vs15_keeps_cluster_narrow() {
    // ☀ + VS15 (text presentation) stays one cell wide.
    let t = run_size(10, 1, "\x1b[?2027h\u{2600}\u{FE0E}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "\u{2600}\u{FE0E}");
    assert!(!cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert_eq!(cell(&t, 1, 0).ch, 'X');
}

#[test]
fn ac_2027_008_vs16_at_last_column_skips_promotion() {
    // The narrow base sits in the last column; there is no room for a
    // spacer, so the cluster keeps its narrow footprint rather than
    // spilling past the margin.
    let t = run_size(3, 2, "\x1b[?2027hAB\u{2600}\u{FE0F}".as_bytes());

    assert_eq!(cell(&t, 2, 0).text(), "\u{2600}\u{FE0F}");
    assert!(!cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE));
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn ac_2027_009_promotion_overwrites_following_wide_lead_cleanly() {
    // Layout before promotion: ☀ at 0, あ wide at 1..=2. Moving back to
    // column 2 and sending VS16 attaches to the ☀ at 0 and widens it; the
    // あ's lead/spacer pair must be destroyed with no orphaned spacer.
    let t = run_size(
        10,
        1,
        "\x1b[?2027h\u{2600}\u{3042}\x1b[1;2H\u{FE0F}".as_bytes(),
    );

    assert_eq!(cell(&t, 0, 0).text(), "\u{2600}\u{FE0F}");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(!cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(cell(&t, 2, 0).is_blank());
}

#[test]
fn combining_marks_are_capped_per_cell() {
    // A hostile stream repeating a combining mark must not grow one cell's
    // storage without bound; the tail past the cap is dropped.
    let mut input = String::from("a");
    for _ in 0..1000 {
        input.push('\u{0301}');
    }
    let t = run_size(10, 1, input.as_bytes());
    assert!(cell(&t, 0, 0).combining().len() <= crate::cell::Cell::MAX_COMBINING_BYTES);
}

#[test]
fn ac_2027_004_mode_off_falls_back_to_plain_combining_only() {
    // With 2027 off, the ZWJs still attach as zero-width combining marks
    // (pre-existing, ungated behavior), but each width-2 base emoji still
    // consumes its own cell — no cluster merge, per REQ-2027's documented
    // out-of-scope complex clustering.
    let t = run_size(
        10,
        1,
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}".as_bytes(),
    );

    assert_eq!(cell(&t, 0, 0).text(), "\u{1F468}\u{200D}");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert_eq!(cell(&t, 2, 0).text(), "\u{1F469}\u{200D}");
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE));
    assert_eq!(cell(&t, 4, 0).text(), "\u{1F467}\u{200D}");
    assert_eq!(cell(&t, 6, 0).ch, '\u{1F466}');
}

// ── WP6: BEL surfaced as a drainable terminal event (AC-BEL) ───────────

#[test]
fn ac_bel_001_take_pending_bell_drains_once() {
    let mut t = run(b"\x07");
    assert!(t.take_pending_bell());
    assert!(!t.take_pending_bell());
}

#[test]
fn ac_bel_002_bell_has_no_grid_side_effect() {
    let t = run(b"\x1b[5;10Hhi\x07");
    assert_eq!(t.primary.cursor.y, 4);
    assert_eq!(t.primary.cursor.x, 11);
    assert_eq!(row_text(&t, 4, 12), "         hi ");
}

#[test]
fn ac_bel_003_consecutive_bels_in_one_feed_drain_true() {
    let mut t = run(b"\x07\x07\x07");
    assert!(t.take_pending_bell());
    assert!(!t.take_pending_bell());
}

#[test]
fn ac_bel_004_full_reset_clears_pending_bell() {
    let mut t = run(b"\x07\x1bc");
    assert!(!t.take_pending_bell());
}

// ── WP7: XTWINOPS report subset + title stack (AC-WIN) ─────────────────

#[test]
fn ac_win_001_report_size_in_chars() {
    let t = run_size(80, 24, b"\x1b[18t");
    assert_eq!(t.pending_writes, b"\x1b[8;24;80t");
}

#[test]
fn ac_win_002_report_window_title() {
    let t = run_title_report(b"\x1b]2;foo\x1b\\\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfoo\x1b\\");
}

// `title-report` defaults off (Ghostty parity): the reply echoes
// program-settable title text back into the pty, so any displayed byte
// stream could inject input (e.g. a Claude Code task summary reappearing
// in its own prompt).
#[test]
fn ac_win_011_title_report_disabled_by_default() {
    let t = run(b"\x1b]2;foo\x1b\\\x1b[21t");
    assert!(t.pending_writes.is_empty());
}

#[test]
fn ac_win_003_report_text_area_px() {
    let mut t = run(b"");
    t.set_pixel_metrics(9, 18, 720, 432);
    let mut s = Stream::new();
    s.feed(b"\x1b[14t", &mut t);
    assert_eq!(t.pending_writes, b"\x1b[4;432;720t");
}

#[test]
fn ac_win_004_report_cell_size_px() {
    let mut t = run(b"");
    t.set_pixel_metrics(9, 18, 720, 432);
    let mut s = Stream::new();
    s.feed(b"\x1b[16t", &mut t);
    assert_eq!(t.pending_writes, b"\x1b[6;18;9t");
}

#[test]
fn ac_win_005_pixel_metrics_reflect_latest_set_no_stale_values() {
    let mut t = run(b"");
    t.set_pixel_metrics(9, 18, 720, 432);
    t.set_pixel_metrics(10, 20, 800, 480);
    let mut s = Stream::new();
    s.feed(b"\x1b[14t\x1b[16t", &mut t);
    assert_eq!(t.pending_writes, b"\x1b[4;480;800t\x1b[6;20;10t");
}

#[test]
fn ac_win_006_title_stack_push_pop_restores_pushed_title() {
    let t = run_title_report(b"\x1b]2;first\x1b\\\x1b[22;2t\x1b]2;second\x1b\\\x1b[23;2t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

#[test]
fn ac_win_007_ps1_zero_and_two_are_equivalent() {
    let t = run_title_report(b"\x1b]2;first\x1b\\\x1b[22;0t\x1b]2;second\x1b\\\x1b[23;0t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

#[test]
fn ac_win_008_ps1_one_icon_only_is_a_noop() {
    let t = run_title_report(b"\x1b]2;first\x1b\\\x1b[22;1t\x1b]2;second\x1b\\\x1b[23;1t\x1b[21t");
    // Push/pop with Ps[1]=1 never touched the stack, so title stays "second".
    assert_eq!(t.pending_writes, b"\x1b]lsecond\x1b\\");
}

#[test]
fn ac_win_009_unsupported_ps_values_produce_no_reply() {
    let t = run(b"\x1b[4t\x1b[8t\x1b[9t\x1b[10t\x1b[19t\x1b[20t");
    assert!(t.pending_writes.is_empty());
}

#[test]
fn ac_win_010_third_param_stack_index_round_trips() {
    let t =
        run_title_report(b"\x1b]2;first\x1b\\\x1b[22;2;5t\x1b]2;second\x1b\\\x1b[23;2;5t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

// FM-7 regression: cap eviction must check length *before* pushing (cap=64,
// not 65). Push 65 distinct titles (evicting the oldest), then pop all 64
// surviving entries and confirm the stack is empty — a 65th surviving entry
// would make one more pop restore a pushed title instead of no-op'ing.
#[test]
fn title_stack_evicts_oldest_entry_past_cap_of_64() {
    let mut t = run(b"");
    t.title_report = true;
    let mut s = Stream::new();
    for i in 0..65 {
        s.feed(
            format!("\x1b]2;title-{i}\x1b\\\x1b[22;2t").as_bytes(),
            &mut t,
        );
    }
    for _ in 0..64 {
        s.feed(b"\x1b[23;2t", &mut t);
    }
    s.feed(b"\x1b]2;sentinel\x1b\\\x1b[23;2t\x1b[21t", &mut t);
    // The stack was already empty (only 64 of the 65 pushes survived), so
    // this final pop is a no-op and the title stays "sentinel".
    assert_eq!(t.pending_writes, b"\x1b]lsentinel\x1b\\");
}

// tab-title REQ-TTL-5 (P2): the title stack carries each title's staleness
// fingerprint, so a popped title is judged against the cwd it was pushed at,
// not a later title's cwd. Push title A bound to /a, cd to /b, set title B,
// then pop: A is restored AND its fingerprint (/a) diverges from the live cwd
// (/b), so the resolver treats the restored title as stale.
#[test]
fn title_stack_pop_restores_the_pushed_titles_staleness_fingerprint() {
    let t = run(b"\x1b]2;A\x1b\\\x1b]7;file://localhost/a\x07\
          \x1b[22;2t\
          \x1b]7;file://localhost/b\x07\
          \x1b]2;B\x1b\\\
          \x1b[23;2t");
    assert_eq!(t.title, "A");
    assert_eq!(t.cwd.as_deref(), Some("/b"));
    assert_eq!(t.title_cwd.as_deref(), Some("/a"));
}

// ── Kitty keyboard protocol progressive enhancement ────────────────────

#[test]
fn kitty_keyboard_query_reports_zero_by_default() {
    let mut t = run(b"");
    let mut s = Stream::new();
    s.feed(b"\x1b[?u", &mut t);
    assert_eq!(t.pending_writes, b"\x1b[?0u");
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_keyboard_push_pop_query_roundtrip() {
    let mut t = run(b"");
    let mut s = Stream::new();
    // Push flags 5 (disambiguate | alternate keys), then query.
    s.feed(b"\x1b[>5u\x1b[?u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 5);
    assert_eq!(t.take_pending_writes(), b"\x1b[?5u");
    // Push again with 1, query, then pop back to 5.
    s.feed(b"\x1b[>1u\x1b[?u", &mut t);
    assert_eq!(t.take_pending_writes(), b"\x1b[?1u");
    s.feed(b"\x1b[<1u\x1b[?u", &mut t);
    assert_eq!(t.take_pending_writes(), b"\x1b[?5u");
}

#[test]
fn kitty_keyboard_push_without_param_is_zero() {
    let mut t = run(b"");
    let mut s = Stream::new();
    s.feed(b"\x1b[>3u\x1b[>u\x1b[?u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 0);
    assert_eq!(t.pending_writes, b"\x1b[?0u");
}

#[test]
fn kitty_keyboard_set_modes_replace_or_clear() {
    let mut t = run(b"");
    let mut s = Stream::new();
    // Replace (mode omitted) → 1, OR mode 2 → 3, clear mode 3 → 2.
    s.feed(b"\x1b[=1u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 1);
    s.feed(b"\x1b[=2;2u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 3);
    s.feed(b"\x1b[=1;3u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 2);
}

#[test]
fn kitty_keyboard_pop_past_bottom_clears_flags() {
    let mut t = run(b"");
    let mut s = Stream::new();
    s.feed(b"\x1b[>1u\x1b[<5u\x1b[?u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 0);
    assert_eq!(t.pending_writes, b"\x1b[?0u");
}

#[test]
fn kitty_keyboard_stack_evicts_oldest_past_depth_eight() {
    let mut t = run(b"");
    let mut s = Stream::new();
    // Push nine distinct flag values; the first (1) is evicted.
    for flags in [1u8, 2, 4, 8, 16, 3, 5, 9, 17] {
        s.feed(format!("\x1b[>{flags}u").as_bytes(), &mut t);
    }
    assert_eq!(t.kitty_keyboard_flags(), 17);
    // Eight entries remain (2,4,8,16,3,5,9,17); seven pops reach `2`.
    s.feed(b"\x1b[<7u\x1b[?u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 2);
}

#[test]
fn kitty_keyboard_main_and_alt_stacks_are_separate() {
    let mut t = run(b"");
    let mut s = Stream::new();
    // Set flags on the main screen, enter the alt screen (DECSET 1049), and
    // confirm the alt stack starts empty and is independent.
    s.feed(b"\x1b[>7u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 7);
    s.feed(b"\x1b[?1049h", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 0);
    s.feed(b"\x1b[>1u", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 1);
    // Leaving the alt screen restores the main flags.
    s.feed(b"\x1b[?1049l", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 7);
}

#[test]
fn kitty_keyboard_ris_clears_both_stacks() {
    let mut t = run(b"");
    let mut s = Stream::new();
    s.feed(b"\x1b[>7u\x1b[?1049h\x1b[>1u", &mut t);
    s.feed(b"\x1bc", &mut t); // RIS
    assert_eq!(t.kitty_keyboard_flags(), 0);
    s.feed(b"\x1b[?1049h", &mut t);
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn plain_csi_u_is_still_restore_cursor() {
    // CSI u with no private marker must remain SCORC (restore cursor), not be
    // captured by the Kitty keyboard dispatch. Save at (2,3), move, restore.
    let t = run(b"\x1b[2;3H\x1b[s\x1b[10;10H\x1b[u");
    assert_eq!(t.primary.cursor.y, 1);
    assert_eq!(t.primary.cursor.x, 2);
    // And the DECSC/DECRC `CSI u` form leaves no pending writes (not a query).
    assert!(t.pending_writes.is_empty());
}
