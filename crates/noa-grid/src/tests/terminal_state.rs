#[test]
fn deferred_wrap_latch() {
    // 80 chars into an 80-col row: cursor parks at the last column, latched, no wrap.
    let t = run(&[b'x'; 80]);
    assert_eq!(t.primary.cursor.x, 79);
    assert_eq!(t.primary.cursor.y, 0);
    assert!(t.primary.cursor.pending_wrap);
    assert!(!t.primary.grid[0].wrapped);

    // The 81st char triggers the wrap: row 0 marked wrapped, char lands at (0,1).
    let t = run(&[b'x'; 81]);
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'x');
    assert_eq!(t.primary.cursor.y, 1);
    assert_eq!(t.primary.cursor.x, 1);
}

#[test]
fn cjk_utf8_split_across_feeds_decodes_without_replacement() {
    let mut t = Terminal::new(GridSize::new(10, 2));
    let mut s = Stream::new();

    for byte in "無効化".as_bytes() {
        s.feed(&[*byte], &mut t);
    }

    assert_eq!(cell(&t, 0, 0).ch, '無');
    assert_eq!(cell(&t, 2, 0).ch, '効');
    assert_eq!(cell(&t, 4, 0).ch, '化');
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 3, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(cell(&t, 4, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 5, 0).attrs.contains(CellAttrs::WIDE_SPACER));
}

#[test]
fn absolute_move_clears_latch_no_wrap() {
    // 80 x's (latched) then CHA to column 1 must NOT wrap.
    let mut bytes = vec![b'x'; 80];
    bytes.extend_from_slice(b"\x1b[G");
    let t = run(&bytes);
    assert!(!t.primary.cursor.pending_wrap);
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn cursor_position_clamped() {
    // CUP well past the screen clamps to (row 23, col 79) on 80×24.
    let t = run(b"\x1b[99;99H");
    assert_eq!(t.primary.cursor.y, 23);
    assert_eq!(t.primary.cursor.x, 79);
}

#[test]
fn decscusr_updates_cursor_style() {
    let t = run(b"\x1b[4 q");
    assert_eq!(t.primary.cursor.style, CursorStyle::SteadyUnderline);

    let t = run(b"\x1b[6 q");
    assert_eq!(t.primary.cursor.style, CursorStyle::SteadyBar);

    let t = run(b"\x1b[0 q");
    assert_eq!(t.primary.cursor.style, CursorStyle::BlinkingBlock);
}

#[test]
fn set_default_cursor_style_applies_and_decscusr_zero_resets_to_it() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.set_default_cursor_style(CursorStyle::SteadyBar);
    // Applied immediately as the active cursor style.
    assert_eq!(t.primary.cursor.style, CursorStyle::SteadyBar);

    let mut s = Stream::new();
    // A concrete DECSCUSR changes the style away from the default.
    s.feed(b"\x1b[2 q", &mut t);
    assert_eq!(t.primary.cursor.style, CursorStyle::SteadyBlock);
    // DECSCUSR 0 resets to the configured default, not a hardcoded block.
    s.feed(b"\x1b[0 q", &mut t);
    assert_eq!(t.primary.cursor.style, CursorStyle::SteadyBar);
}

#[test]
fn cup_is_one_based() {
    let t = run(b"\x1b[3;5H");
    assert_eq!(t.primary.cursor.y, 2);
    assert_eq!(t.primary.cursor.x, 4);
}

#[test]
fn decslrm_requires_left_right_margin_mode() {
    let t = run_size(10, 5, b"\x1b[3;7s");

    assert_eq!(t.primary.horizontal_margins, None);
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn decslrm_sets_horizontal_margins_and_homes_to_left_margin() {
    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7s");

    assert_eq!(
        t.primary.horizontal_margins,
        Some(HorizontalMargins { left: 2, right: 6 })
    );
    assert_eq!(t.primary.cursor.x, 2);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn horizontal_margins_clamp_cursor_motion_and_carriage_return() {
    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7s\x1b[99C");
    assert_eq!(t.primary.cursor.x, 6);

    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7s\x1b[99C\x1b[99D");
    assert_eq!(t.primary.cursor.x, 2);

    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7s\x1b[99C\r");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn horizontal_margins_wrap_printing_to_left_margin() {
    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7sabcdeZ");

    assert_eq!(row_text(&t, 0, 8), "  abcde ");
    assert_eq!(cell(&t, 2, 1).ch, 'Z');
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn left_right_margin_reset_restores_full_width_motion() {
    let t = run_size(10, 5, b"\x1b[?69h\x1b[3;7s\x1b[?69l\x1b[1G");

    assert_eq!(t.primary.horizontal_margins, None);
    assert_eq!(t.primary.cursor.x, 0);
}

#[test]
fn keypad_mode_tracks_esc_and_dec_private_mode() {
    let t = run(b"\x1b=");
    assert!(t.modes.app_keypad());

    let t = run(b"\x1b=\x1b>");
    assert!(!t.modes.app_keypad());

    let t = run(b"\x1b[?66h");
    assert!(t.modes.app_keypad());
}

#[test]
fn cursor_down_and_forward_no_overflow_on_large_csi_params() {
    let t = run_size(10, 5, b"\x1b[2;3H\x1b[65535B");
    assert_eq!(t.primary.cursor.y, 4);
    assert_eq!(t.primary.cursor.x, 2);

    let t = run_size(10, 5, b"\x1b[2;3H\x1b[65535C");
    assert_eq!(t.primary.cursor.y, 1);
    assert_eq!(t.primary.cursor.x, 9);
}

#[test]
fn cursor_up_down_only_clamp_to_scroll_region_when_inside_it() {
    let t = run_size(10, 8, b"\x1b[3;6r\x1b[2;1H\x1b[A");
    assert_eq!(t.primary.cursor.y, 0);
    assert_eq!(t.primary.cursor.x, 0);

    let t = run_size(10, 8, b"\x1b[3;6r\x1b[7;1H\x1b[B");
    assert_eq!(t.primary.cursor.y, 7);
    assert_eq!(t.primary.cursor.x, 0);

    let t = run_size(10, 8, b"\x1b[3;6r\x1b[3;1H\x1b[A");
    assert_eq!(t.primary.cursor.y, 2);

    let t = run_size(10, 8, b"\x1b[3;6r\x1b[6;1H\x1b[B");
    assert_eq!(t.primary.cursor.y, 5);
}

#[test]
fn invalid_scroll_region_does_not_home_or_change_existing_region() {
    let t = run_size(10, 8, b"\x1b[2;5r\x1b[4;4H\x1b[6;3r");

    assert_eq!(t.primary.region.top, 1);
    assert_eq!(t.primary.region.bottom, 4);
    assert_eq!(t.primary.cursor.y, 3);
    assert_eq!(t.primary.cursor.x, 3);
}

#[test]
fn dsr_cursor_position_reply() {
    // Move to row 3, col 5, then request DSR-6.
    let t = run(b"\x1b[3;5H\x1b[6n");
    assert_eq!(t.pending_writes, b"\x1b[3;5R");
}

#[test]
fn take_pending_writes_drains_queue() {
    let mut t = run(b"\x1b[3;5H\x1b[6n");

    assert_eq!(t.take_pending_writes(), b"\x1b[3;5R");
    assert!(t.pending_writes.is_empty());
    assert!(t.take_pending_writes().is_empty());
}

#[test]
fn da1_reply() {
    let t = run(b"\x1b[c");
    assert_eq!(t.pending_writes, b"\x1b[?62;4;22c");
}

#[test]
fn decrqss_reports_sgr_cursor_and_margins() {
    let t = run_size(
        10,
        5,
        b"\x1b[31;4:3m\
          \x1b[6 q\
          \x1b[2;4r\
          \x1b[?69h\x1b[3;7s\
          \x1bP$qm\x1b\\\
          \x1bP$q q\x1b\\\
          \x1bP$qr\x1b\\\
          \x1bP$qs\x1b\\\
          \x1bP$qBAD\x1b\\",
    );

    assert_eq!(
        t.pending_writes,
        b"\x1bP1$r0;4:3;31m\x1b\\\
          \x1bP1$r6 q\x1b\\\
          \x1bP1$r2;4r\x1b\\\
          \x1bP1$r3;7s\x1b\\\
          \x1bP0$rBAD\x1b\\"
    );
}

#[test]
fn xtgettcap_reports_selected_capabilities() {
    let t = run(b"\x1bP+q544e;524742;7878\x1b\\");

    assert_eq!(
        t.pending_writes,
        concat!(
            "\x1bP1+r544e=6e6f61\x1b\\",
            "\x1bP1+r524742=383a383a38\x1b\\",
            "\x1bP0+r7878\x1b\\",
        )
        .as_bytes()
    );
}

#[test]
fn xtversion_query_reports_name_and_version() {
    // `CSI > 0 q` and the bare `CSI > q` are both valid XTVERSION queries.
    let t = run(b"\x1b[>0q\x1b[>q");

    let expected = format!(
        "\x1bP>|noa {v}\x1b\\\x1bP>|noa {v}\x1b\\",
        v = env!("CARGO_PKG_VERSION")
    );
    assert_eq!(t.pending_writes, expected.as_bytes());
}

#[test]
fn decrqm_reports_known_mode_state() {
    let t = run(b"\x1b[?25$p\x1b[?25l\x1b[?25$p\x1b[20$p\x1b[?9999$p");

    assert_eq!(
        t.pending_writes,
        b"\x1b[?25;1$y\x1b[?25;2$y\x1b[20;2$y\x1b[?9999;0$y"
    );
}

#[test]
fn decrqm_reports_alternate_scroll_mode_1007_state() {
    // 1007 defaults on (matches Ghostty), so an un-set query reports 1
    // (set); after `CSI ?1007l` it reports 2 (reset).
    let t = run(b"\x1b[?1007$p\x1b[?1007l\x1b[?1007$p");
    assert_eq!(t.pending_writes, b"\x1b[?1007;1$y\x1b[?1007;2$y");
}

#[test]
fn erase_display_and_home() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"hello", &mut t);
    s.feed(b"\x1b[2J\x1b[H", &mut t); // clear all + home
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.y, 0);
    assert_eq!(cell(&t, 0, 0).ch, ' ');
}

#[test]
fn sgr_palette_fg_written_to_cell() {
    let t = run(b"\x1b[31mR\x1b[0m");
    let c = cell(&t, 0, 0);
    assert_eq!(c.ch, 'R');
    assert_eq!(c.fg, Color::Palette(1));
}

#[test]
fn sgr_truecolor_written_to_cell() {
    let t = run(b"\x1b[38;2;10;20;30mX");
    let c = cell(&t, 0, 0);
    assert_eq!(c.ch, 'X');
    assert_eq!(c.fg, Color::Rgb(Rgb::new(10, 20, 30)));
}

#[test]
fn bold_attribute_set_then_reset() {
    let t = run(b"\x1b[1mB\x1b[22mn");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::BOLD));
    assert!(!cell(&t, 1, 0).attrs.contains(CellAttrs::BOLD));
}

#[test]
fn sgr_underline_styles_and_color_written_to_cells() {
    let t = run(b"\x1b[4:2;58;5;123mD\x1b[4:5;59mE\x1b[24mF");

    let double = cell(&t, 0, 0);
    assert!(double.attrs.contains(CellAttrs::DOUBLE_UNDERLINE));
    assert_eq!(double.underline_color, Some(Color::Palette(123)));

    let dashed = cell(&t, 1, 0);
    assert!(dashed.attrs.contains(CellAttrs::DASHED_UNDERLINE));
    assert!(!dashed.attrs.contains(CellAttrs::DOUBLE_UNDERLINE));
    assert_eq!(dashed.underline_color, None);

    let reset = cell(&t, 2, 0);
    assert!(!reset.attrs.intersects(CellAttrs::underline_styles()));
}

#[test]
fn sgr_reset_clears_underline_color() {
    let t = run(b"\x1b[4:3;58;2;1;2;3mA\x1b[0mB");

    let styled = cell(&t, 0, 0);
    assert!(styled.attrs.contains(CellAttrs::CURLY_UNDERLINE));
    assert_eq!(styled.underline_color, Some(Color::Rgb(Rgb::new(1, 2, 3))));

    let reset = cell(&t, 1, 0);
    assert_eq!(reset.underline_color, None);
    assert!(!reset.attrs.intersects(CellAttrs::underline_styles()));
}

// XTMODKEYS `CSI > 4 ; 2 m` is not SGR — misreading it as `4;2m` sticks
// underline+faint on every cell printed afterwards (seen with Claude Code
// enabling modifyOtherKeys when it detects a capable terminal).
#[test]
fn xtmodkeys_is_not_sgr_and_tracks_modify_other_keys() {
    let mut t = run(b"\x1b[>4;2mA");

    let printed = cell(&t, 0, 0);
    assert!(printed.attrs.is_empty(), "attrs: {:?}", printed.attrs);
    assert!(t.modify_other_keys_2);

    let mut s = Stream::new();
    s.feed(b"\x1b[>4m", &mut t);
    assert!(!t.modify_other_keys_2);

    s.feed(b"\x1b[>4;1m\x1b[>4;2m\x1b[>m", &mut t);
    assert!(!t.modify_other_keys_2, "bare CSI > m resets");
}

#[test]
fn newline_and_carriage_return() {
    // "ab\r\ncd" → row0 "ab", row1 "cd"
    let t = run(b"ab\r\ncd");
    assert_eq!(cell(&t, 0, 0).ch, 'a');
    assert_eq!(cell(&t, 1, 0).ch, 'b');
    assert_eq!(cell(&t, 0, 1).ch, 'c');
    assert_eq!(cell(&t, 1, 1).ch, 'd');
}

#[test]
fn scroll_at_bottom_when_indexing_off_screen() {
    // Put the cursor on the last row, print, then LF: content scrolls up.
    let mut bytes = b"\x1b[24;1H".to_vec(); // row 24 (last)
    bytes.extend_from_slice(b"bottom\r\n"); // print + LF (should scroll)
    let t = run(&bytes);
    // After the scroll, the cursor stays on the last row.
    assert_eq!(t.primary.cursor.y, 23);
    // "bottom" scrolled up to row 22.
    assert_eq!(cell(&t, 0, 22).ch, 'b');
}

#[test]
fn tab_advances_to_next_stop() {
    let t = run(b"\tX");
    // First tab stop is column 8; 'X' lands there.
    assert_eq!(cell(&t, 8, 0).ch, 'X');
}

#[test]
fn csi_tab_forward_and_backward() {
    let t = run(b"\x1b[2IZ\x1b[20G\x1b[2ZX");
    assert_eq!(cell(&t, 16, 0).ch, 'Z');
    assert_eq!(cell(&t, 8, 0).ch, 'X');
}

#[test]
fn tab_clear_current_stop() {
    let t = run(b"\x1b[5G\x1bH\x1b[g\x1b[G\tY");
    assert_eq!(cell(&t, 4, 0).ch, ' ');
    assert_eq!(cell(&t, 8, 0).ch, 'Y');
}

#[test]
fn tab_clear_all_stops() {
    let t = run(b"\x1b[3g\tZ");
    assert_eq!(cell(&t, 79, 0).ch, 'Z');
}

#[test]
fn insert_blank_chars_shifts_right() {
    let t = run(b"abcdef\x1b[3G\x1b[2@");
    assert_eq!(row_text(&t, 0, 8), "ab  cdef");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn delete_chars_shifts_left() {
    let t = run(b"abcdef\x1b[3G\x1b[2P");
    assert_eq!(row_text(&t, 0, 6), "abef  ");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn erase_chars_keeps_cursor_position() {
    let t = run(b"abcdef\x1b[3G\x1b[2X");
    assert_eq!(row_text(&t, 0, 6), "ab  ef");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn insert_lines_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[3;1H\x1b[L");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'B');
    assert_eq!(cell(&t, 0, 2).ch, ' ');
    assert_eq!(cell(&t, 0, 3).ch, 'C');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn delete_lines_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[3;1H\x1b[M");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'B');
    assert_eq!(cell(&t, 0, 2).ch, 'D');
    assert_eq!(cell(&t, 0, 3).ch, ' ');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn scroll_up_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[S");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'C');
    assert_eq!(cell(&t, 0, 2).ch, 'D');
    assert_eq!(cell(&t, 0, 3).ch, ' ');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn scroll_down_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[T");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, ' ');
    assert_eq!(cell(&t, 0, 2).ch, 'B');
    assert_eq!(cell(&t, 0, 3).ch, 'C');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn repeat_preceding_char() {
    let t = run(b"ab\x1b[3b");
    assert_eq!(row_text(&t, 0, 5), "abbbb");
}

#[test]
fn alternate_screen_1049_isolated_and_restores_primary_cursor() {
    let t = run(b"main\x1b[?1049hALT\x1b[?1049lZ");

    assert!(!t.active_is_alt);
    assert_eq!(row_text(&t, 0, 5), "mainZ");
    let alt = t.alt.as_ref().expect("alternate screen should exist");
    assert_eq!(alt.grid[0].cells[0].ch, ' ');
    assert_eq!(alt.grid[0].cells[1].ch, ' ');
}

#[test]
fn active_screen_returns_alternate_while_enabled() {
    let t = run(b"P\x1b[?1049hA");

    assert!(t.active_is_alt);
    assert_eq!(t.primary.grid[0].cells[0].ch, 'P');
    assert_eq!(t.active().grid[0].cells[0].ch, 'A');
}

#[test]
fn alternate_screen_1048_saves_and_restores_cursor_without_switching() {
    let t = run(b"\x1b[10;5H\x1b[?1048h\x1b[1;1H\x1b[?1048lX");

    assert!(!t.active_is_alt);
    assert_eq!(cell(&t, 4, 9).ch, 'X');
}

#[test]
fn alternate_screen_47_preserves_without_clear() {
    let t = run(b"\x1b[?47hOLD\x1b[?47l\x1b[?47h");

    assert!(t.active_is_alt);
    assert_eq!(t.active().grid[0].cells[0].ch, 'O');
    assert_eq!(t.active().grid[0].cells[1].ch, 'L');
    assert_eq!(t.active().grid[0].cells[2].ch, 'D');
}

#[test]
fn alternate_screen_1047_clears_on_reset() {
    let t = run(b"\x1b[?47hOLD\x1b[?47l\x1b[?1047h\x1b[?1047l\x1b[?47hN");

    assert!(t.active_is_alt);
    assert_eq!(t.active().grid[0].cells[0].ch, 'N');
    assert_eq!(t.active().grid[0].cells[1].ch, ' ');
}

#[test]
fn dsr_cursor_position_uses_active_screen() {
    let t = run(b"\x1b[?1049h\x1b[3;5H\x1b[6n");

    assert_eq!(t.pending_writes, b"\x1b[3;5R");
}

#[test]
fn resize_updates_primary_and_alternate_screens() {
    let mut t = run(b"\x1b[?1049h");
    t.resize(GridSize::new(100, 30));

    assert_eq!(t.primary.cols, 100);
    assert_eq!(t.primary.rows, 30);
    let alt = t.alt.as_ref().expect("alternate screen should exist");
    assert_eq!(alt.cols, 100);
    assert_eq!(alt.rows, 30);
    assert_eq!(t.active().cols, 100);
    assert_eq!(t.active().rows, 30);
}

#[test]
fn full_reset_leaves_alternate_screen_and_clears_state() {
    let t = run(b"main\x1b[?1049hALT\x1bcZ");

    assert!(!t.active_is_alt);
    assert!(t.alt.is_none());
    assert_eq!(cell(&t, 0, 0).ch, 'Z');
    assert_eq!(cell(&t, 1, 0).ch, ' ');
}

#[test]
fn bracketed_paste_mode_toggles_with_dec_private_2004() {
    let t = run(b"\x1b[?2004h");
    assert!(t.modes.bracketed_paste());

    let t = run(b"\x1b[?2004h\x1b[?2004l");
    assert!(!t.modes.bracketed_paste());
}

#[test]
fn full_reset_clears_bracketed_paste_mode() {
    let t = run(b"\x1b[?2004h\x1bc");

    assert!(!t.modes.bracketed_paste());
}

#[test]
fn sgr_mouse_modes_toggle_and_reset() {
    let t = run(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(t.modes.mouse_tracking(), crate::modes::MouseTracking::Press);
    assert_eq!(t.modes.mouse_format(), crate::modes::MouseFormat::Sgr);
    assert!(t.modes.mouse_reporting());

    let t = run(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        t.modes.mouse_tracking(),
        crate::modes::MouseTracking::ButtonMotion
    );

    let t = run(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h");
    assert_eq!(
        t.modes.mouse_tracking(),
        crate::modes::MouseTracking::AnyMotion
    );

    let t = run(b"\x1b[?1000h\x1b[?1006h\x1bc");
    assert!(!t.modes.mouse_reporting());
    assert_eq!(t.modes.mouse_format(), crate::modes::MouseFormat::Legacy);
}

#[test]
fn x10_mouse_tracking_mode_toggles_and_yields_to_vt200_modes() {
    let t = run(b"\x1b[?9h");
    assert_eq!(t.modes.mouse_tracking(), crate::modes::MouseTracking::X10);
    assert!(t.modes.mouse_reporting());
    assert_eq!(t.modes.mouse_format(), crate::modes::MouseFormat::Legacy);

    // Any VT200-style tracking mode outranks X10 while both are set.
    let t = run(b"\x1b[?9h\x1b[?1000h");
    assert_eq!(t.modes.mouse_tracking(), crate::modes::MouseTracking::Press);
    let t = run(b"\x1b[?9h\x1b[?1000h\x1b[?1000l");
    assert_eq!(t.modes.mouse_tracking(), crate::modes::MouseTracking::X10);

    let t = run(b"\x1b[?9h\x1b[?9l");
    assert_eq!(t.modes.mouse_tracking(), crate::modes::MouseTracking::Off);
    assert!(!t.modes.mouse_reporting());
}

#[test]
fn mouse_format_modes_are_exclusive_and_last_set_wins() {
    use crate::modes::MouseFormat;

    assert_eq!(run(b"").modes.mouse_format(), MouseFormat::Legacy);
    assert_eq!(run(b"\x1b[?1005h").modes.mouse_format(), MouseFormat::Utf8);
    assert_eq!(run(b"\x1b[?1015h").modes.mouse_format(), MouseFormat::Urxvt);

    // The last format set displaces the previous one…
    let t = run(b"\x1b[?1005h\x1b[?1015h\x1b[?1006h");
    assert_eq!(t.modes.mouse_format(), MouseFormat::Sgr);
    let t = run(b"\x1b[?1006h\x1b[?1005h");
    assert_eq!(t.modes.mouse_format(), MouseFormat::Utf8);

    // …so resetting a displaced format leaves the active one untouched,
    // while resetting the active format falls back to Legacy.
    let t = run(b"\x1b[?1005h\x1b[?1006h\x1b[?1005l");
    assert_eq!(t.modes.mouse_format(), MouseFormat::Sgr);
    let t = run(b"\x1b[?1005h\x1b[?1006h\x1b[?1006l");
    assert_eq!(t.modes.mouse_format(), MouseFormat::Legacy);
}

#[test]
fn alternate_scroll_mode_defaults_on_and_toggles_and_resets() {
    // DECSET 1007 defaults on (matching Ghostty) so alt-screen TUIs scroll
    // via wheel→arrow conversion without opting in.
    let t = run(b"");
    assert!(t.modes.alternate_scroll());

    let t = run(b"\x1b[?1007l");
    assert!(!t.modes.alternate_scroll());
    let t = run(b"\x1b[?1007l\x1b[?1007h");
    assert!(t.modes.alternate_scroll());

    // RIS restores the power-on default (on), unlike plain-default modes.
    let t = run(b"\x1b[?1007l\x1bc");
    assert!(t.modes.alternate_scroll());
}

#[test]
fn decrqm_reports_mouse_tracking_and_format_modes() {
    let t = run(b"\x1b[?9h\x1b[?9$p\x1b[?1005$p\x1b[?1015$p");
    assert_eq!(t.pending_writes, b"\x1b[?9;1$y\x1b[?1005;2$y\x1b[?1015;2$y");

    // Only the active (last-set) format mode reports as set.
    let t = run(b"\x1b[?1005h\x1b[?1015h\x1b[?1005$p\x1b[?1015$p\x1b[?1006$p");
    assert_eq!(
        t.pending_writes,
        b"\x1b[?1005;2$y\x1b[?1015;1$y\x1b[?1006;2$y"
    );
}

#[test]
fn focus_reporting_and_synchronized_output_modes_toggle_and_reset() {
    let t = run(b"\x1b[?1004h\x1b[?2026h\x1b[?1004$p\x1b[?2026$p");

    assert!(t.modes.focus_reporting());
    assert!(t.modes.synchronized_output());
    assert_eq!(t.pending_writes, b"\x1b[?1004;1$y\x1b[?2026;1$y");

    let t = run(b"\x1b[?1004h\x1b[?2026h\x1b[?1004l\x1b[?2026l");

    assert!(!t.modes.focus_reporting());
    assert!(!t.modes.synchronized_output());

    let t = run(b"\x1b[?1004h\x1b[?2026h\x1bc");

    assert!(!t.modes.focus_reporting());
    assert!(!t.modes.synchronized_output());
}

#[test]
fn scrollback_records_full_screen_scrolls() {
    let t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
}

#[test]
fn top_anchored_scroll_region_records_scrollback() {
    let mut t = run_size(
        5,
        5,
        b"AA\x1b[2;1HBB\x1b[3;1HCC\x1b[4;1HDD\x1b[5;1HEE\x1b[1;3r\x1b[3;1H\r\n",
    );

    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(t.primary.take_scroll_shift(), 0);
    assert_eq!(row_text(&t, 0, 2), "BB");
    assert_eq!(row_text(&t, 1, 2), "CC");
    assert_eq!(row_text(&t, 2, 2), "  ");
    assert_eq!(row_text(&t, 3, 2), "DD");
    assert_eq!(row_text(&t, 4, 2), "EE");

    t.scroll_viewport_up(1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 2), "AA");
    assert_eq!(rows_text(&rows, 1, 2), "BB");
    assert_eq!(rows_text(&rows, 2, 2), "CC");
}

#[test]
fn non_top_scroll_region_does_not_record_scrollback() {
    let t = run_size(5, 4, b"\x1b[2;4r\x1b[4;1HA\r\n");

    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn viewport_can_show_scrollback_and_return_to_live() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.scroll_viewport_up(1);
    assert_eq!(t.viewport_offset(), 1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");

    t.scroll_viewport_down(1);
    assert_eq!(t.viewport_offset(), 0);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "B");
    assert_eq!(rows_text(&rows, 1, 1), "C");
    assert_eq!(rows_text(&rows, 2, 1), "D");
}

#[test]
fn viewport_can_jump_to_scrollback_top() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD\r\nE\r\nF");

    t.scroll_viewport_to_top();
    assert_eq!(t.viewport_offset(), t.scrollback_len());
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");
}

#[test]
fn output_keeps_scrolled_viewport_pinned_to_content() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);

    let mut s = Stream::new();
    // In-place output must not move a scrolled-back viewport.
    s.feed(b"E", &mut t);
    assert_eq!(t.viewport_offset(), 1);

    // Output that scrolls rows into scrollback grows the offset so the
    // same content stays on screen (a repainting TUI must not yank the
    // viewport back to the live bottom).
    s.feed(b"\r\nF", &mut t);
    assert_eq!(t.viewport_offset(), 2);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");
}

#[test]
fn output_at_live_bottom_keeps_following() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    let mut s = Stream::new();
    s.feed(b"\r\nE\r\nF", &mut t);

    assert_eq!(t.viewport_offset(), 0);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 2, 1), "F");
}

#[test]
fn pinned_viewport_clamps_when_scrollback_evicts() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_to_top();
    let pinned = t.viewport_offset();
    assert!(pinned > 0);

    // Shrinking the limit below the retained history evicts rows; the
    // pinned offset must clamp to the remaining scrollback.
    t.set_scrollback_limit_bytes(0);
    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.viewport_offset(), 0);
}

#[test]
fn erase_display_scrollback_clears_history_only() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);

    let mut s = Stream::new();
    s.feed(b"\x1b[3J", &mut t);

    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
}

#[test]
fn clear_active_display_and_scrollback_clears_primary_state() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 0, y: 1 });
    t.set_search_query("A");
    t.pending_writes.extend_from_slice(b"reply");
    t.pending_clipboard_writes.push("clip".to_string());

    t.clear_active_display_and_scrollback();

    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 0, 5), "     ");
    assert_eq!(row_text(&t, 1, 5), "     ");
    assert_eq!(row_text(&t, 2, 5), "     ");
    assert!(t.active().selection.is_none());
    assert!(t.active().search.query().is_empty());
    assert_eq!(t.pending_writes, b"reply");
    assert_eq!(t.pending_clipboard_writes, vec!["clip"]);
}

#[test]
fn clear_scrollback_preserves_primary_live_display() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 0, y: 1 });
    t.set_search_query("A");

    t.clear_scrollback();

    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
    assert!(t.active().selection.is_none());
    assert!(t.active().search.query().is_empty());
}

#[test]
fn alternate_clear_preserves_primary_scrollback_and_terminal_state() {
    let mut t = run_size(
        5,
        3,
        b"A\r\nB\r\nC\r\nD\x1b[?2004h\x1b]0;alt title\x07\x1b[?1049hALT",
    );
    t.colors.set_default_fg(Rgb::new(1, 2, 3));
    t.pending_writes.extend_from_slice(b"reply");
    t.pending_clipboard_writes.push("clip".to_string());
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 2, y: 0 });
    t.set_search_query("ALT");
    let primary_scrollback_len = t.primary.scrollback_len();

    t.clear_active_display_and_scrollback();

    assert!(t.active_is_alt);
    assert_eq!(t.primary.scrollback_len(), primary_scrollback_len);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
    assert_eq!(active_row_text(&t, 0, 5), "     ");
    assert_eq!(active_row_text(&t, 1, 5), "     ");
    assert_eq!(active_row_text(&t, 2, 5), "     ");
    assert!(t.active().selection.is_none());
    assert!(t.active().search.query().is_empty());
    assert!(t.modes.bracketed_paste());
    assert_eq!(t.title, "alt title");
    assert_eq!(t.colors.default_fg(), Some(Rgb::new(1, 2, 3)));
    assert_eq!(t.pending_writes, b"reply");
    assert_eq!(t.pending_clipboard_writes, vec!["clip"]);
}
