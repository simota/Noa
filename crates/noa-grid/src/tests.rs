//! Terminal state tests: deferred-wrap latch, cursor clamps, erase, DSR
//! replies, and captured-stream golden fixtures.

use crate::cursor::{CursorStyle, HorizontalMargins};
use crate::terminal::{PromptJump, ShellIntegrationMarkKind, Terminal};
use noa_core::{
    CellAttrs, Color, DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, GridSize, Point, Rgb, xterm_palette,
    xterm_palette_color,
};
use noa_vt::Stream;

/// Feed `bytes` through a fresh 80×24 terminal and return the final state.
fn run(bytes: &[u8]) -> Terminal {
    run_size(80, 24, bytes)
}

fn run_size(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn run_with_base_colors(
    bytes: &[u8],
    default_fg: Rgb,
    default_bg: Rgb,
    cursor: Rgb,
    palette: [Rgb; 256],
) -> Terminal {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.set_base_colors(default_fg, default_bg, cursor, palette);
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn cell(t: &Terminal, x: usize, y: usize) -> crate::cell::Cell {
    t.primary.grid[y].cells[x].clone()
}

fn row_text(t: &Terminal, y: usize, width: usize) -> String {
    t.primary.grid[y].cells[..width]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn active_row_text(t: &Terminal, y: usize, width: usize) -> String {
    t.active().grid[y].cells[..width]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn rows_text(rows: &[crate::cell::Row], y: usize, width: usize) -> String {
    rows[y].cells[..width].iter().map(|c| c.ch).collect()
}

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
    assert_eq!(t.pending_writes, b"\x1b[?62;22c");
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
fn xtgettcap_and_xtversion_report_selected_capabilities() {
    let t = run(b"\x1bP+q544e;524742;7878\x1b\\\
          \x1bP>q\x1b\\");

    assert_eq!(
        t.pending_writes,
        concat!(
            "\x1bP1+r544e=6e6f61\x1b\\",
            "\x1bP1+r524742=383a383a38\x1b\\",
            "\x1bP0+r7878\x1b\\",
            "\x1bP>|noa ",
            env!("CARGO_PKG_VERSION"),
            "\x1b\\"
        )
        .as_bytes()
    );
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
fn partial_scroll_region_does_not_record_scrollback() {
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
fn output_returns_viewport_to_live_bottom() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);

    let mut s = Stream::new();
    s.feed(b"E", &mut t);

    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 2, 2), "DE");
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

#[test]
fn select_all_primary_selects_scrollback_and_live_grid() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.select_all();

    let selection = t.active().selection.expect("select all should select text");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(4, 3));
    assert_eq!(t.selected_text().as_deref(), Some("A\nB\nC\nD"));
}

#[test]
fn select_all_alternate_selects_visible_grid_only() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD\x1b[?1049hX\r\nY\r\nZ");
    let primary_scrollback_len = t.primary.scrollback_len();

    t.select_all();

    let selection = t.active().selection.expect("select all should select text");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(4, 2));
    assert_eq!(t.selected_text().as_deref(), Some("X\nY\nZ"));
    assert_eq!(t.primary.scrollback_len(), primary_scrollback_len);
}

#[test]
fn select_all_empty_terminal_keeps_copy_payload_empty() {
    let mut t = Terminal::new(GridSize::new(5, 3));

    t.select_all();

    assert!(t.active().selection.is_none());
    assert_eq!(t.selected_text(), None);
}

#[test]
fn alternate_screen_does_not_record_scrollback() {
    let t = run_size(5, 3, b"\x1b[?1049hA\r\nB\r\nC\r\nD");

    assert!(t.active_is_alt);
    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.primary.scrollback_len(), 0);
}

#[test]
fn selection_normalizes_reversed_range() {
    let mut t = Terminal::new(GridSize::new(5, 3));
    t.set_viewport_selection(Point { x: 4, y: 2 }, Point { x: 1, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();

    assert_eq!(start.x, 1);
    assert_eq!(start.y, 1);
    assert_eq!(end.x, 4);
    assert_eq!(end.y, 2);
}

#[test]
fn selection_uses_visible_scrollback_row_base() {
    let mut t = Terminal::new(GridSize::new(2, 2));
    t.primary.grid[0].cells[0].ch = 'A';
    t.primary.grid[1].cells[0].ch = 'B';
    t.primary.scroll_up_region(1);
    t.primary.grid[1].cells[0].ch = 'C';
    t.scroll_viewport_up(1);

    t.set_viewport_selection(Point { x: 1, y: 0 }, Point { x: 0, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    assert!(!selection.contains(crate::SelectionPoint::new(0, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(1, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(0, 1)));
    assert!(!selection.contains(crate::SelectionPoint::new(1, 1)));
}

#[test]
fn selection_viewport_points_are_clamped_to_screen() {
    let mut t = Terminal::new(GridSize::new(3, 2));
    t.set_viewport_selection(Point { x: 9, y: 9 }, Point { x: 0, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (_start, end) = selection.normalized();

    assert_eq!(end.x, 2);
    assert_eq!(end.y, 1);
}

#[test]
fn word_selection_expands_non_whitespace_run() {
    let mut t = run_size(12, 1, b"foo bar");
    t.select_word_at_viewport_point(Point { x: 5, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    assert!(!selection.contains(crate::SelectionPoint::new(3, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(4, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(5, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(6, 0)));
    assert!(!selection.contains(crate::SelectionPoint::new(7, 0)));
}

#[test]
fn word_selection_on_blank_cell_selects_single_cell() {
    let mut t = run_size(8, 1, b"foo bar");
    t.select_word_at_viewport_point(Point { x: 3, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(3, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 0));
}

#[test]
fn word_selection_from_wide_spacer_selects_visual_cell() {
    let mut t = run_size(5, 1, " 界 ".as_bytes());
    t.select_word_at_viewport_point(Point { x: 2, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(1, 0));
    assert_eq!(end, crate::SelectionPoint::new(2, 0));
}

#[test]
fn line_selection_selects_entire_viewport_row() {
    let mut t = Terminal::new(GridSize::new(4, 2));
    t.select_line_at_viewport_point(Point { x: 99, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 1));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
}

#[test]
fn selected_text_copies_single_row_range() {
    let mut t = run_size(12, 1, b"hello world");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 4, y: 0 });

    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn selected_text_copies_multiline_range_with_newline() {
    let mut t = run_size(5, 2, b"ab\r\ncd");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("ab\ncd"));
}

#[test]
fn selected_text_skips_wide_spacer_cells() {
    let mut t = run_size(6, 1, "A界B".as_bytes());
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 3, y: 0 });

    assert_eq!(t.selected_text().as_deref(), Some("A界B"));
}

#[test]
fn selected_text_joins_soft_wrapped_rows() {
    let mut t = run_size(4, 2, b"abcdZ");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 0, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("abcdZ"));
}

#[test]
fn selected_text_keeps_trailing_spaces_on_wrapped_rows() {
    // "ab  " fills the 4-col row and wraps into "cd": the two spaces are real
    // content at the wrap boundary and must survive the join.
    let mut t = run_size(4, 2, b"ab  cd");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("ab  cd"));
}

#[test]
fn word_selection_stops_at_boundary_punctuation() {
    let mut t = run_size(12, 1, b"foo(bar)");
    t.select_word_at_viewport_point(Point { x: 5, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(4, 0));
    assert_eq!(end, crate::SelectionPoint::new(6, 0));
}

#[test]
fn line_selection_spans_soft_wrapped_logical_line() {
    // "abcdZ" wraps across rows 0-1; row 2 is a separate line. Triple-click
    // on either wrapped row selects the whole logical line.
    let mut t = run_size(4, 3, b"abcdZ\r\nnext");
    t.select_line_at_viewport_point(Point { x: 0, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
    assert_eq!(t.selected_text().as_deref(), Some("abcdZ"));

    t.select_line_at_viewport_point(Point { x: 2, y: 1 });
    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
}

#[test]
fn erase_display_clears_selection() {
    let mut t = run_size(8, 2, b"hello");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 4, y: 0 });
    assert!(t.active().selection.is_some());

    let mut s = Stream::new();
    s.feed(b"\x1b[2J", &mut t);
    assert!(t.active().selection.is_none());
}

#[test]
fn viewport_point_converts_to_storage_coordinate() {
    // Two rows scrolled into scrollback: viewport row 0 maps to storage row 2
    // at the bottom, and back to storage row 1 after scrolling up one row.
    let mut t = run_size(4, 2, b"a\r\nb\r\nc\r\nd");
    assert_eq!(
        t.viewport_point_to_selection_point(Point { x: 1, y: 0 }),
        crate::SelectionPoint::new(1, 2)
    );

    t.scroll_viewport_up(1);
    assert_eq!(
        t.viewport_point_to_selection_point(Point { x: 1, y: 0 }),
        crate::SelectionPoint::new(1, 1)
    );
}

#[test]
fn selection_clears_on_full_reset_and_screen_switch() {
    let mut t = run(b"\x1b[?1049h");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 0 });
    assert!(t.active().selection.is_some());

    let mut s = Stream::new();
    s.feed(b"\x1b[?1049l", &mut t);
    assert!(t.active().selection.is_none());

    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 0 });
    s.feed(b"\x1bc", &mut t);
    assert!(t.active().selection.is_none());
}

#[test]
fn search_matches_live_rows_and_navigates() {
    let mut t = run_size(12, 2, b"foo bar foo");

    t.set_search_query("foo");

    assert_eq!(t.active().search.matches().len(), 2);
    assert!(t.active().search.contains(crate::SelectionPoint::new(0, 0)));
    assert!(t.active().search.contains(crate::SelectionPoint::new(8, 0)));

    let next = t.search_next().expect("second match should be active");
    assert_eq!(next.start, crate::SelectionPoint::new(8, 0));

    let previous = t
        .search_previous()
        .expect("first match should be active again");
    assert_eq!(previous.start, crate::SelectionPoint::new(0, 0));
}

#[test]
fn search_navigation_and_clear_without_query_are_noops() {
    let mut t = run_size(8, 2, b"foo bar");
    let row_before = row_text(&t, 0, 8);
    let cursor_before = t.active().cursor;

    assert_eq!(t.search_next(), None);
    assert_eq!(t.search_previous(), None);
    t.clear_search();

    assert!(t.active().search.query().is_empty());
    assert!(t.active().search.matches().is_empty());
    assert_eq!(t.active().search.active_match(), None);
    assert_eq!(row_text(&t, 0, 8), row_before);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(t.active().cursor.x, cursor_before.x);
    assert_eq!(t.active().cursor.y, cursor_before.y);
    assert!(t.pending_writes.is_empty());
    assert!(t.pending_clipboard_writes.is_empty());
}

#[test]
fn search_reveals_scrollback_match() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.set_search_query("A");

    assert_eq!(t.viewport_offset(), 1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
}

#[test]
fn search_skips_wide_spacer_cells() {
    let mut t = run_size(5, 1, "A界B".as_bytes());

    t.set_search_query("界B");

    let found = t.active().search.active_match().expect("wide match");
    assert_eq!(found.start, crate::SelectionPoint::new(1, 0));
    assert_eq!(found.end, crate::SelectionPoint::new(3, 0));
}

#[test]
fn search_clears_on_full_reset_and_screen_switch() {
    let mut t = run(b"foo\x1b[?1049h");
    t.set_search_query("foo");
    assert!(!t.active().search.query().is_empty());

    let mut s = Stream::new();
    s.feed(b"\x1b[?1049l", &mut t);
    assert!(t.active().search.query().is_empty());

    t.set_search_query("foo");
    s.feed(b"\x1bc", &mut t);
    assert!(t.active().search.query().is_empty());
}

#[test]
fn resize_shrink_rows_moves_top_drained_primary_rows_to_scrollback() {
    let mut t = run_size(5, 5, b"\x1b[5;1HE");
    t.primary.grid[0].cells[0].ch = 'A';
    t.primary.grid[1].cells[0].ch = 'B';
    t.primary.grid[2].cells[0].ch = 'C';

    t.resize(GridSize::new(5, 3));

    assert_eq!(t.scrollback_len(), 2);
    t.scroll_viewport_up(2);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");
}

#[test]
fn title_from_osc() {
    let t = run(b"\x1b]0;my title\x07");
    assert_eq!(t.title, "my title");
}

#[test]
fn osc8_hyperlink_state_is_stored_on_printed_cells() {
    let t = run(b"\x1b]8;id=docs;https://example.test/docs\x1b\\AB\
          \x1b]8;;\x1b\\C");

    let link_id = cell(&t, 0, 0).hyperlink.expect("A should carry link");
    assert_eq!(cell(&t, 1, 0).hyperlink, Some(link_id));
    assert_eq!(cell(&t, 2, 0).hyperlink, None);
    assert_eq!(t.hyperlinks[link_id].uri, "https://example.test/docs");
    assert_eq!(t.hyperlinks[link_id].id.as_deref(), Some("docs"));
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

#[test]
fn utf8_scalar_stored_in_cell() {
    let t = run("étä".as_bytes());
    assert_eq!(cell(&t, 0, 0).ch, 'é');
    assert_eq!(cell(&t, 1, 0).ch, 't');
    assert_eq!(cell(&t, 2, 0).ch, 'ä');
}

#[test]
fn wide_char_marks_lead_and_spacer() {
    let t = run_size(6, 2, "A界B".as_bytes());

    assert_eq!(row_text(&t, 0, 5), "A界 B ");
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(
        !cell(&t, 3, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert_eq!(t.primary.cursor.x, 4);
}

#[test]
fn wide_char_wraps_before_last_column() {
    let t = run_size(4, 2, "abc界Z".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "abc ");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, '界');
    assert!(cell(&t, 0, 1).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 1).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(cell(&t, 2, 1).ch, 'Z');
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn wide_char_ending_at_row_end_defers_wrap_until_next_spacing_char() {
    let t = run_size(4, 2, "ab界Z".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "ab界 ");
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 3, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'Z');
    assert_eq!(t.primary.cursor.x, 1);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn narrow_overwrite_on_wide_spacer_clears_whole_wide_cell() {
    let t = run_size(4, 1, "界\x1b[1;2HX".as_bytes());

    assert_eq!(row_text(&t, 0, 4), " X  ");
    assert!(
        !cell(&t, 0, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn narrow_overwrite_on_wide_lead_clears_whole_wide_cell() {
    let t = run_size(4, 1, "界\x1b[1;1HX".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "X   ");
    assert!(
        !cell(&t, 0, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn combining_mark_does_not_advance_cell_cursor() {
    let t = run_size(5, 1, "e\u{301}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "e\u{301}");
    assert_eq!(row_text(&t, 0, 4), "eX  ");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn combining_attach_keeps_mark_in_cell_text() {
    let t = run_size(5, 1, "a\u{301}".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "a\u{301}");
    assert_eq!(t.primary.visible_rows()[0].cells[0].text(), "a\u{301}");
    assert_eq!(t.primary.cursor.x, 1);
}

#[test]
fn combining_mark_after_pending_wrap_does_not_trigger_wrap() {
    let t = run_size(2, 2, "ab\u{301}C".as_bytes());

    assert_eq!(cell(&t, 1, 0).text(), "b\u{301}");
    assert_eq!(row_text(&t, 0, 2), "ab");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'C');
    assert_eq!(t.primary.cursor.x, 1);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn combining_attach_after_wide_cell_uses_lead_cell() {
    let t = run_size(5, 1, "界\u{301}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "界\u{301}");
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn erase_chars_sanitizes_split_wide_cell() {
    let t = run_size(5, 1, "A界B\x1b[1;3H\x1b[X".as_bytes());

    assert_eq!(row_text(&t, 0, 5), "A  B ");
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 2, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn resize_grow_preserves_content() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"hello", &mut t);
    t.resize(GridSize::new(100, 30));
    assert_eq!(t.primary.cols, 100);
    assert_eq!(t.primary.rows, 30);
    assert_eq!(t.primary.grid.len(), 30);
    assert_eq!(t.primary.grid[0].cells.len(), 100);
    assert_eq!(cell(&t, 0, 0).ch, 'h');
    assert_eq!(cell(&t, 4, 0).ch, 'o');
    assert_eq!(t.size, GridSize::new(100, 30));
}

#[test]
fn resize_shrink_cols_truncates_row_width() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.resize(GridSize::new(40, 24));
    assert_eq!(t.primary.cols, 40);
    assert_eq!(t.primary.grid[0].cells.len(), 40);
    assert!(t.primary.cursor.x < 40);
}

#[test]
fn resize_reflows_soft_wrapped_rows_when_shrinking_cols() {
    let mut t = run_size(4, 4, b"abcdef");

    t.resize(GridSize::new(3, 4));

    assert_eq!(row_text(&t, 0, 3), "abc");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 3), "def");
    assert!(!t.primary.grid[1].wrapped);
    assert_eq!(t.primary.cursor.x, 2);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn resize_reflows_soft_wrapped_rows_when_growing_cols() {
    let mut t = run_size(3, 4, b"abcdef");

    t.resize(GridSize::new(6, 4));

    assert_eq!(row_text(&t, 0, 6), "abcdef");
    assert!(!t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 6), "      ");
    assert_eq!(t.primary.cursor.x, 5);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn resize_reflow_preserves_hard_newline_boundaries() {
    let mut t = run_size(4, 4, b"ab\r\ncd");

    t.resize(GridSize::new(6, 4));

    assert_eq!(row_text(&t, 0, 6), "ab    ");
    assert!(!t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 6), "cd    ");
    assert!(!t.primary.grid[1].wrapped);
}

#[test]
fn resize_reflow_keeps_wide_cell_intact_when_shrinking_cols() {
    let mut t = run_size(4, 1, "A界".as_bytes());

    t.resize(GridSize::new(2, 1));

    assert_eq!(t.scrollback_len(), 1);
    t.scroll_viewport_up(1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 2), "A ");
    assert!(
        !rows[0].cells[1]
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );

    t.scroll_viewport_to_bottom();
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 2), "界 ");
    assert!(
        rows[0].cells[0].attrs.contains(CellAttrs::WIDE)
            && rows[0].cells[1].attrs.contains(CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn resize_reflow_clears_selection_and_search_coordinates() {
    let mut t = run_size(4, 4, b"abcdef");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });
    t.set_search_query("cd");

    t.resize(GridSize::new(3, 4));

    assert!(t.active().selection.is_none());
    assert!(t.active().search.query().is_empty());
}

#[test]
fn resize_shrink_rows_keeps_cursor_in_bounds() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[24;1H", &mut t); // cursor to the last row
    assert_eq!(t.primary.cursor.y, 23);
    t.resize(GridSize::new(80, 10));
    assert_eq!(t.primary.rows, 10);
    assert_eq!(t.primary.grid.len(), 10);
    assert!(t.primary.cursor.y < 10);
}

#[test]
fn take_visible_rows_with_damage_isolates_a_single_row_and_clears_on_consume() {
    // AC-WP4-01: a single cell mutation in one row sets ONLY that row's
    // dirty flag; all other rows stay clean. `take_visible_rows_with_damage`
    // then clears the flag it just reported.
    let mut t = run_size(4, 3, b""); // fresh terminal: every row starts dirty.
    let screen = &mut t.primary;

    // Drain the initial all-dirty state so the next assertion is meaningful.
    let (_, initial_dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(initial_dirty, vec![true, true, true]);
    let (_, drained_dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(drained_dirty, vec![false, false, false]);

    // Mutate row 1 only (a real cell-mutating path, not a direct field poke).
    // `cursor_position` is 1-based: (row=2, col=1) -> (y=1, x=0).
    screen.cursor_position(2, 1);
    screen.print('x', true, false);

    let (rows, dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(dirty, vec![false, true, false]);
    assert_eq!(rows[1].cells[0].ch, 'x');

    // Consuming clears the flag; an unchanged next frame reports all clean.
    let (_, dirty_after) = screen.take_visible_rows_with_damage();
    assert_eq!(dirty_after, vec![false, false, false]);
}

#[test]
fn resize_shrink_rows_keeps_cursor_on_its_content() {
    // Cursor mid-screen (row 11 → y=10) with a marker on its line; shrinking
    // to 10 rows must keep the cursor on that same content, not drop it.
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[11;1HZ", &mut t);
    assert_eq!(cell(&t, 0, 10).ch, 'Z');
    t.resize(GridSize::new(80, 10));
    let cy = t.primary.cursor.y as usize;
    assert!(cy < 10);
    assert_eq!(cell(&t, 0, cy).ch, 'Z');
}

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
    assert!(cell(&t, 0, 0).combining.len() <= crate::cell::Cell::MAX_COMBINING_BYTES);
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
    let t = run(b"\x1b]2;foo\x1b\\\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfoo\x1b\\");
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
    let t = run(b"\x1b]2;first\x1b\\\x1b[22;2t\x1b]2;second\x1b\\\x1b[23;2t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

#[test]
fn ac_win_007_ps1_zero_and_two_are_equivalent() {
    let t = run(b"\x1b]2;first\x1b\\\x1b[22;0t\x1b]2;second\x1b\\\x1b[23;0t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

#[test]
fn ac_win_008_ps1_one_icon_only_is_a_noop() {
    let t = run(b"\x1b]2;first\x1b\\\x1b[22;1t\x1b]2;second\x1b\\\x1b[23;1t\x1b[21t");
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
    let t = run(b"\x1b]2;first\x1b\\\x1b[22;2;5t\x1b]2;second\x1b\\\x1b[23;2;5t\x1b[21t");
    assert_eq!(t.pending_writes, b"\x1b]lfirst\x1b\\");
}

// FM-7 regression: cap eviction must check length *before* pushing (cap=64,
// not 65). Push 65 distinct titles (evicting the oldest), then pop all 64
// surviving entries and confirm the stack is empty — a 65th surviving entry
// would make one more pop restore a pushed title instead of no-op'ing.
#[test]
fn title_stack_evicts_oldest_entry_past_cap_of_64() {
    let mut t = run(b"");
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

// ── paged scrollback: byte-limited storage across page boundaries ──────────
//
// These feed *full-width* rows (`R{i}` padded with dots, no trailing blanks so
// nothing is trimmed) so history reliably spans more than one 64 KiB page,
// exercising the page-granular eviction and cross-page read paths.

fn feed_full_rows(s: &mut Stream, t: &mut Terminal, cols: usize, n: usize) {
    for i in 0..n {
        let mut line = format!("R{i}");
        while line.len() < cols {
            line.push('.');
        }
        line.push_str("\r\n");
        s.feed(line.as_bytes(), t);
    }
}

fn terminal_full_history(cols: u16, rows: u16, n: usize) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    feed_full_rows(&mut s, &mut t, cols as usize, n);
    t
}

#[test]
fn scrollback_eviction_advances_rows_evicted_and_shifts_selection() {
    let mut t = terminal_full_history(80, 3, 400);
    let before_len = t.scrollback_len();
    assert!(before_len > 200, "history spans multiple pages");

    // A selection on the top live row survives eviction and must shift up by
    // exactly the evicted row count (session-absolute coordinates).
    t.scroll_viewport_to_bottom();
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 3, y: 0 });
    let before = t.active().selection.expect("selection set");
    let before_evicted = t.primary.rows_evicted();

    t.set_scrollback_limit_bytes(1);

    let evicted = t.primary.rows_evicted() - before_evicted;
    assert!(evicted > 0, "shrinking the limit evicts whole pages");
    assert!(t.scrollback_len() < before_len);
    let after = t
        .active()
        .selection
        .expect("live selection survives eviction");
    assert_eq!(after.anchor.y, before.anchor.y - evicted);
    assert_eq!(after.focus.y, before.focus.y - evicted);
}

#[test]
fn search_matches_span_scrollback_page_boundaries() {
    let mut t = terminal_full_history(80, 3, 300);

    t.set_search_query("R5.");
    let top = t.active().search.matches().to_vec();
    assert_eq!(
        top.len(),
        1,
        "the `R5.` marker occurs once, in an early page"
    );
    let top_y = top[0].start.y;

    t.set_search_query("R190.");
    let bot = t.active().search.matches().to_vec();
    assert_eq!(
        bot.len(),
        1,
        "the `R190.` marker occurs once, in a later page"
    );
    let bot_y = bot[0].start.y;

    assert!(
        bot_y > top_y + 100,
        "matches were found in rows far apart, i.e. across a page boundary"
    );
}

#[test]
fn selected_text_joins_wrapped_rows_across_page_boundary() {
    // One giant soft-wrapped logical line: 200 rows of 'a', spanning >1 page.
    let mut t = Terminal::new(GridSize::new(80, 3));
    let mut s = Stream::new();
    s.feed(&vec![b'a'; 80 * 200], &mut t);

    // Select a range that crosses a page boundary (rows ~102/103); every row is
    // soft-wrapped, so no newline is inserted between them.
    t.set_selection(
        crate::SelectionPoint::new(0, 50),
        crate::SelectionPoint::new(79, 160),
    );
    let text = t.selected_text().expect("selection has text");

    assert!(
        !text.contains('\n'),
        "soft-wrapped history rows join without a newline across pages"
    );
    assert!(text.chars().all(|c| c == 'a'));
}

#[test]
fn history_rows_report_clean_across_page_boundary() {
    let mut t = terminal_full_history(80, 3, 200);
    // Scroll fully into history; all visible rows are immutable scrollback rows.
    t.scroll_viewport_to_top();

    let (rows, dirty) = t.primary.take_visible_rows_with_damage();
    assert_eq!(rows.len(), 3);
    assert!(
        dirty.iter().all(|&d| !d),
        "immutable history rows always report clean"
    );
    assert_eq!(rows_text(&rows, 0, 2), "R0");

    // A second consume still reports clean (nothing re-dirtied history).
    let (_, dirty_again) = t.primary.take_visible_rows_with_damage();
    assert!(dirty_again.iter().all(|&d| !d));
}

#[test]
fn resize_preserves_cursor_anchor_with_multi_page_scrollback() {
    let mut t = terminal_full_history(80, 3, 200);
    // A known short prompt line under the cursor, after >1 page of history.
    let mut s = Stream::new();
    s.feed(b"PROMPT", &mut t);
    assert_eq!(t.primary.cursor.x, 6);

    // Column-count resize reflows the whole (multi-page) history and re-anchors
    // the cursor onto the same character.
    t.resize(GridSize::new(40, 3));

    assert_eq!(t.primary.cursor.x, 6, "cursor stays just past PROMPT");
    let y = t.primary.cursor.y as usize;
    assert_eq!(row_text(&t, y, 6), "PROMPT");
}

#[test]
fn prompt_jump_coordinates_survive_page_eviction() {
    let mut t = Terminal::new(GridSize::new(80, 3));
    let mut s = Stream::new();

    let feed_prompt = |s: &mut Stream, t: &mut Terminal, label: &str| {
        let mut line = label.to_string();
        while line.len() < 80 {
            line.push('.');
        }
        line.push_str("\r\n");
        let mut bytes = b"\x1b]133;A\x07".to_vec();
        bytes.extend_from_slice(line.as_bytes());
        s.feed(&bytes, t);
    };

    feed_prompt(&mut s, &mut t, "PA");
    feed_full_rows(&mut s, &mut t, 80, 120);
    feed_prompt(&mut s, &mut t, "PB");
    feed_full_rows(&mut s, &mut t, 80, 120);
    feed_prompt(&mut s, &mut t, "PC");
    feed_full_rows(&mut s, &mut t, 80, 10);

    // Evict old pages, then recompute which prompts remain in session-absolute
    // coordinates (shell marks are not rewritten by eviction).
    t.set_scrollback_limit_bytes(1);
    let evicted = t.primary.rows_evicted();
    assert!(evicted > 0, "shrinking the limit evicts whole pages");

    let mut survivors: Vec<usize> = t
        .shell_marks
        .iter()
        .filter(|mark| mark.kind == ShellIntegrationMarkKind::PromptStart)
        .map(|mark| mark.point.y)
        .filter(|&y| y >= evicted)
        .collect();
    assert!(!survivors.is_empty(), "at least the newest prompt survives");
    survivors.sort_unstable();
    survivors.reverse();

    // Prev from the bottom walks the retained prompts top-ward; each lands the
    // viewport top on that prompt's evicted-remapped row.
    t.scroll_viewport_to_bottom();
    let mut landed = Vec::new();
    while t.scroll_to_prompt(PromptJump::Prev) {
        landed.push(t.primary.rows_evicted() + t.primary.visible_row_base());
    }
    assert_eq!(landed, survivors, "no evicted prompt is ever jumped into");
}

#[test]
fn set_scrollback_limit_bytes_shrinks_and_disables_history() {
    let mut t = terminal_full_history(80, 3, 300);
    let before = t.scrollback_len();
    assert!(before > 100, "history spans multiple pages");

    t.set_scrollback_limit_bytes(1);
    let after = t.scrollback_len();
    assert!(after < before, "runtime shrink trims history");
    assert!(after > 0, "the newest page is always retained");

    t.set_scrollback_limit_bytes(0);
    assert_eq!(t.scrollback_len(), 0, "limit 0 disables scrollback");
}

// ── Kitty graphics (APC → image store → replies) ────────────────────

/// Build an APC Kitty graphics sequence `ESC _ G <ctrl> ; <base64(data)> ESC \`.
fn kitty_apc(ctrl: &str, data: &[u8]) -> Vec<u8> {
    let mut b64 = Vec::new();
    crate::osc::encode_base64(data, &mut b64);
    let mut out = b"\x1b_G".to_vec();
    out.extend_from_slice(ctrl.as_bytes());
    out.push(b';');
    out.extend_from_slice(&b64);
    out.extend_from_slice(b"\x1b\\");
    out
}

#[test]
fn kitty_transmit_stores_and_replies_ok() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    assert_eq!(t.pending_writes, b"\x1b_Gi=1;OK\x1b\\");
}

#[test]
fn kitty_quiet_one_suppresses_ok() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1,q=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    assert!(t.pending_writes.is_empty(), "q=1 suppresses the OK reply");
}

#[test]
fn kitty_quiet_two_suppresses_errors() {
    // Bad dimensions → ENODATA, but q=2 suppresses even errors.
    let t = run(&kitty_apc("a=t,f=32,s=4,v=4,i=1,q=2", &[0; 8]));
    assert!(t.pending_writes.is_empty(), "q=2 suppresses error replies");
}

#[test]
fn kitty_no_reply_when_neither_i_nor_i_number_given() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1", &[1, 2, 3, 4]));
    assert!(t.pending_writes.is_empty(), "i=0 and I=0 → no reply at all");
}

#[test]
fn kitty_auto_id_reply_echoes_assigned_id_and_number() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,I=7", &[1, 2, 3, 4]));
    // Auto-assigned id 1, number echoed.
    assert_eq!(t.pending_writes, b"\x1b_Gi=1,I=7;OK\x1b\\");
}

#[test]
fn kitty_error_reply_carries_code() {
    let t = run(&kitty_apc("a=t,f=32,s=4,v=4,i=1", &[0; 8]));
    assert_eq!(
        t.pending_writes,
        b"\x1b_Gi=1;ENODATA:data size mismatch\x1b\\"
    );
}

#[test]
fn kitty_query_validates_without_storing() {
    let t = run(&kitty_apc("a=q,f=32,s=1,v=1,i=9", &[0; 4]));
    assert!(t.kitty_images.get(9).is_none(), "query must not store");
    assert_eq!(t.pending_writes, b"\x1b_Gi=9;OK\x1b\\");
}

#[test]
fn kitty_full_reset_clears_store() {
    let mut t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    let mut s = Stream::new();
    s.feed(b"\x1bc", &mut t); // RIS
    assert!(
        t.kitty_images.get(1).is_none(),
        "RIS clears the image store"
    );
}

// ── Kitty graphics placements ───────────────────────────────────────

/// A 20×24 terminal with 10×20 px cells (metrics that `a=T`/`a=p` need).
fn kitty_terminal() -> Terminal {
    let mut t = Terminal::new(GridSize::new(20, 24));
    t.set_pixel_metrics(10, 20, 200, 480);
    t
}

fn feed(t: &mut Terminal, bytes: &[u8]) {
    let mut s = Stream::new();
    s.feed(bytes, t);
}

#[test]
fn kitty_transmit_and_display_creates_placement_and_moves_cursor() {
    let mut t = kitty_terminal();
    // 25x40 px image → ceil(25/10)=3 cols, ceil(40/20)=2 rows.
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=25,v=40,i=1", &vec![0u8; 25 * 40 * 4]),
    );
    let placements = &t.primary.kitty_placements;
    assert_eq!(placements.len(), 1);
    assert_eq!((placements[0].cols, placements[0].rows), (3, 2));
    // Cursor: last row of the image (down 1), one column past the right edge (0+3).
    assert_eq!((t.primary.cursor.x, t.primary.cursor.y), (3, 1));
}

#[test]
fn kitty_cursor_no_move_keeps_cursor() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!((t.primary.cursor.x, t.primary.cursor.y), (0, 0));
    assert_eq!(t.primary.kitty_placements.len(), 1);
}

#[test]
fn kitty_explicit_columns_rows_override_natural_size() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,c=5,r=4,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    let p = &t.primary.kitty_placements[0];
    assert_eq!((p.cols, p.rows), (5, 4));
}

#[test]
fn kitty_place_without_cell_metrics_is_einval() {
    let mut t = Terminal::new(GridSize::new(20, 24)); // no set_pixel_metrics
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert!(t.primary.kitty_placements.is_empty());
    assert_eq!(t.pending_writes, b"\x1b_Gi=1;EINVAL:invalid request\x1b\\");
}

#[test]
fn kitty_put_displays_transmitted_image() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=t,f=32,s=10,v=20,i=5", &vec![0u8; 10 * 20 * 4]),
    );
    assert!(
        t.primary.kitty_placements.is_empty(),
        "a=t alone doesn't place"
    );
    feed(&mut t, b"\x1b_Ga=p,i=5\x1b\\");
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert_eq!(t.primary.kitty_placements[0].image_id, 5);
}

#[test]
fn kitty_put_missing_image_is_enoent() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b_Ga=p,i=99\x1b\\");
    assert_eq!(t.pending_writes, b"\x1b_Gi=99;ENOENT:file not found\x1b\\");
}

#[test]
fn kitty_unnamed_placement_overwrites() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=t,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=p,i=1\x1b\\");
    feed(&mut t, b"\x1b_Ga=p,i=1\x1b\\");
    assert_eq!(
        t.primary.kitty_placements.len(),
        1,
        "second unnamed placement overwrites the first"
    );
}

#[test]
fn kitty_delete_all_placements() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=2", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.primary.kitty_placements.len(), 2);
    feed(&mut t, b"\x1b_Ga=d,d=a\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
    // Lowercase d=a keeps image data.
    assert!(t.kitty_images.get(1).is_some());
    assert!(t.kitty_images.get(2).is_some());
}

#[test]
fn kitty_delete_by_id_uppercase_frees_data() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=d,d=I,i=1\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
    assert!(
        t.kitty_images.get(1).is_none(),
        "uppercase d frees the image"
    );
}

#[test]
fn kitty_delete_at_cursor() {
    let mut t = kitty_terminal();
    // Place at (0,0) spanning 1x1, then move cursor onto it and delete d=c.
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[1;1H"); // cursor home, over the placement
    feed(&mut t, b"\x1b_Ga=d,d=c\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
}

#[test]
fn kitty_delete_by_z() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,z=5,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=2,z=9,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=d,d=z,z=5\x1b\\");
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert_eq!(t.primary.kitty_placements[0].image_id, 2);
}

#[test]
fn kitty_ed2_removes_intersecting_placements() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[2J"); // ED 2
    assert!(t.primary.kitty_placements.is_empty());
}

#[test]
fn kitty_visible_placement_projects_into_viewport() {
    let mut t = kitty_terminal();
    // Place at row 5.
    feed(&mut t, b"\x1b[6;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=40,i=1,C=1", &vec![0u8; 10 * 40 * 4]),
    );
    let vis = t.kitty_visible_placements();
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].grid_y, 5);
    assert_eq!((vis[0].cols, vis[0].rows), (1, 2));
    assert!(t.kitty_image(1).is_some());
}

#[test]
fn kitty_scroll_pushes_placement_up_via_absolute_anchor() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b[6;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.kitty_visible_placements()[0].grid_y, 5);
    // Scroll the whole screen up by 3 lines' worth of newlines from the bottom.
    feed(&mut t, b"\x1b[24;1H"); // last row
    feed(&mut t, b"\n\n\n");
    let vis = t.kitty_visible_placements();
    assert_eq!(vis.len(), 1, "placement follows content into scrollback");
    assert_eq!(vis[0].grid_y, 2, "moved up by 3 rows");
}

#[test]
fn kitty_alt_screen_placement_is_separated() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b[?1049h"); // enter alt screen
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.active().kitty_placements.len(), 1);
    feed(&mut t, b"\x1b[?1049l"); // leave alt screen
    assert!(
        t.active().kitty_placements.is_empty(),
        "alt-screen placement vanishes on return to primary"
    );
    // Image data survives (only placements are per-screen).
    assert!(t.kitty_images.get(1).is_some());
}

#[test]
fn kitty_reflow_reanchors_placement_to_same_content_row() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    // A 30-char logical line at the top wraps into two rows at 20 cols.
    feed(&mut t, b"\x1b[H");
    feed(&mut t, &[b'A'; 30]);
    feed(&mut t, b"\r\nIMGROW\r");
    // Place a 1×1 image over the IMGROW row (C=1 keeps the cursor put).
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    // Scroll once from the bottom so the wrapped line moves into scrollback.
    feed(&mut t, b"\x1b[4;1H\r\nTAIL\n");

    let find_imgrow = |t: &Terminal| -> i32 {
        (0..t.primary.rows as usize)
            .find(|&y| row_text(t, y, 6) == "IMGROW")
            .map(|y| y as i32)
            .expect("IMGROW still on screen")
    };
    let before = t.kitty_visible_placements();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].grid_y, find_imgrow(&t), "anchored on IMGROW row");

    // Widen so the 30-char line un-wraps to one row: the scrollback shrinks by a
    // row and every row below shifts up. The placement must track IMGROW.
    t.resize(GridSize::new(40, 4));

    let after = t.kitty_visible_placements();
    assert_eq!(after.len(), 1, "placement survives the reflow");
    assert_eq!(
        after[0].grid_y,
        find_imgrow(&t),
        "placement follows IMGROW to its new row"
    );
}

#[test]
fn kitty_reflow_drops_placement_whose_anchor_is_discarded() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    // Six short lines: L0/L1 spill into scrollback, L2..L5 fill the grid.
    for i in 0..6 {
        feed(&mut t, format!("L{i}\r\n").as_bytes());
    }
    // Place a 1×1 image on the last content row, then move the cursor to the top.
    feed(&mut t, b"\x1b[4;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[1;1H");
    assert_eq!(t.primary.kitty_placements.len(), 1);

    // Reflow with the cursor near the top drops the rows below the grid window,
    // including the placement's anchor line — the content is gone, so is it.
    t.resize(GridSize::new(40, 4));
    assert!(
        t.primary.kitty_placements.is_empty(),
        "a placement whose anchor content the reflow discards is removed"
    );
}

#[test]
fn kitty_placement_pruned_when_its_row_is_evicted() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    t.set_scrollback_limit_bytes(1); // keep essentially no history
    // Place a 1×1 image at the top, then scroll far past it. Eviction is
    // page-granular, so it takes more than a page of full-width rows to strand
    // the anchor.
    feed(&mut t, b"\x1b[1;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.primary.kitty_placements.len(), 1);
    let mut s = Stream::new();
    feed_full_rows(&mut s, &mut t, 20, 1000);

    assert!(t.primary.rows_evicted() > 1, "the anchor row scrolled off");
    assert!(
        t.primary.kitty_placements.is_empty(),
        "eviction prunes the stranded placement"
    );
    // The image data lingers but nothing references it now, so a quota sweep is
    // free to reclaim it (it no longer appears in the referenced-id set).
    assert!(t.kitty_images.get(1).is_some());
}

// ── Kitty Unicode placeholders (U+10EEEE) ───────────────────────────

/// Row/column/most-significant-byte diacritics for values 0, 1, 2 (the first
/// three entries of Kitty's table).
const DIA: [char; 3] = ['\u{0305}', '\u{030D}', '\u{030E}'];

/// Write a placeholder cell (`U+10EEEE`) at grid `(x, y)`, encoding image id in
/// the fg, placement id in the underline color, and the given diacritics.
fn put_placeholder(t: &mut Terminal, x: usize, y: usize, id: u32, diacritics: &[char]) {
    let cell = &mut t.primary.grid[y].cells[x];
    cell.ch = crate::PLACEHOLDER;
    cell.fg = Color::Rgb(Rgb::new(
        ((id >> 16) & 0xff) as u8,
        ((id >> 8) & 0xff) as u8,
        (id & 0xff) as u8,
    ));
    cell.combining.clear();
    for &d in diacritics {
        cell.combining.push(d);
    }
}

/// A 20×24 terminal holding image id 1 (30×40 px) placed as a virtual 3×2 cell
/// grid, so each virtual cell maps to a clean 10×20 px image tile.
fn kitty_virtual_terminal() -> Terminal {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc(
            "a=T,f=32,s=30,v=40,i=1,U=1,c=3,r=2,C=1",
            &vec![0u8; 30 * 40 * 4],
        ),
    );
    // The virtual placement is stored but excluded from direct rendering.
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert!(t.primary.kitty_placements[0].is_virtual);
    assert!(t.kitty_visible_placements().is_empty());
    t
}

#[test]
fn placeholder_run_resolves_source_tile() {
    let mut t = kitty_virtual_terminal();
    // Row 0 of the image across all three columns: first cell fully specified,
    // the next two infer column +1.
    put_placeholder(&mut t, 0, 0, 1, &[DIA[0], DIA[0]]);
    put_placeholder(&mut t, 1, 0, 1, &[]);
    put_placeholder(&mut t, 2, 0, 1, &[]);

    let placements = t.kitty_placeholder_placements();
    assert_eq!(placements.len(), 1, "three cells fuse into one run");
    let p = &placements[0];
    assert_eq!((p.grid_x, p.grid_y), (0, 0));
    assert_eq!((p.cols, p.rows), (3, 1));
    assert_eq!(p.image_id, 1);
    // Whole first image row: x=0, y=0, w=30 (3×10), h=20 (40/2).
    assert_eq!(p.src, Some([0, 0, 30, 20]));
    assert_eq!(p.z, 0);
}

#[test]
fn placeholder_second_row_offsets_source_y() {
    let mut t = kitty_virtual_terminal();
    // Image row 1, single column 0 → lower tile of the image.
    put_placeholder(&mut t, 4, 2, 1, &[DIA[1], DIA[0]]);
    let placements = t.kitty_placeholder_placements();
    assert_eq!(placements.len(), 1);
    let p = &placements[0];
    assert_eq!((p.grid_x, p.grid_y), (4, 2));
    assert_eq!((p.cols, p.rows), (1, 1));
    assert_eq!(p.src, Some([0, 20, 10, 20]), "image row 1 starts at y=20");
}

#[test]
fn placeholder_without_virtual_placement_draws_nothing() {
    let mut t = kitty_terminal();
    // A placeholder referencing image 7, which has no virtual placement.
    put_placeholder(&mut t, 0, 0, 7, &[DIA[0], DIA[0]]);
    assert!(
        t.kitty_placeholder_placements().is_empty(),
        "no virtual placement ⇒ nothing to resolve"
    );
}

#[test]
fn placeholder_id_mismatch_is_skipped() {
    let mut t = kitty_virtual_terminal();
    // Virtual placement is for image 1; a placeholder for image 2 resolves to
    // nothing even though a virtual placement exists.
    put_placeholder(&mut t, 0, 0, 2, &[DIA[0], DIA[0]]);
    assert!(t.kitty_placeholder_placements().is_empty());
}

#[test]
fn placeholder_column_jump_splits_run() {
    let mut t = kitty_virtual_terminal();
    // Column 0 then an explicit jump to column 2 (skipping 1) ⇒ two runs.
    put_placeholder(&mut t, 0, 0, 1, &[DIA[0], DIA[0]]);
    put_placeholder(&mut t, 1, 0, 1, &[DIA[0], DIA[2]]);
    let placements = t.kitty_placeholder_placements();
    assert_eq!(
        placements.len(),
        2,
        "non-contiguous image columns don't fuse"
    );
    assert_eq!(placements[0].src, Some([0, 0, 10, 20]));
    assert_eq!(placements[1].src, Some([20, 0, 10, 20]), "column 2 tile");
}
