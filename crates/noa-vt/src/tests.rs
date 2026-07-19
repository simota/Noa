//! Parser + SGR conformance tests (byte-sequence → action assertions),
//! ported from the semantics of Ghostty's `Parser.zig` unit tests.

use crate::action::Action;
use crate::csi::{Csi, Esc};
use crate::parser::Parser;
use crate::sgr::{SgrAttr, parse_sgr, parse_sgr_into};
use noa_core::{Color, Rgb};

/// Run the parser over `bytes` and collect every emitted action.
fn actions(bytes: &[u8]) -> Vec<Action> {
    let mut p = Parser::new();
    let mut out = Vec::new();
    for &b in bytes {
        p.advance(b, &mut |a| out.push(a));
    }
    out
}

/// Extract the single CSI in `bytes` (panics otherwise).
fn only_csi(bytes: &[u8]) -> Csi {
    match actions(bytes)
        .into_iter()
        .find(|a| matches!(a, Action::CsiDispatch(_)))
    {
        Some(Action::CsiDispatch(c)) => c,
        _ => panic!("no CSI dispatch in {bytes:?}"),
    }
}

#[test]
fn prints_ascii() {
    assert_eq!(actions(b"Ab"), vec![Action::Print('A'), Action::Print('b')]);
}

#[test]
fn executes_c0() {
    assert_eq!(
        actions(b"\r\n"),
        vec![Action::Execute(0x0d), Action::Execute(0x0a)]
    );
}

#[test]
fn del_ignored_in_ground() {
    assert_eq!(
        actions(b"a\x7fb"),
        vec![Action::Print('a'), Action::Print('b')]
    );
}

#[test]
fn csi_sgr_31m() {
    // ESC [ 3 1 m  →  CsiDispatch{ params:[31], final:'m' }
    let csi = only_csi(b"\x1b[31m");
    assert_eq!(csi.params(), &[31]);
    assert_eq!(csi.final_byte, b'm');
    assert!(csi.intermediates().is_empty());
    assert_eq!(csi.private, 0);
    assert_eq!(parse_sgr(&csi), vec![SgrAttr::Fg(Color::Palette(1))]);
}

#[test]
fn sgr_empty_is_reset() {
    let csi = only_csi(b"\x1b[m");
    assert_eq!(parse_sgr(&csi), vec![SgrAttr::Reset]);
}

#[test]
fn sgr_truecolor_semicolon_and_colon() {
    let semi = only_csi(b"\x1b[38;2;10;20;30m");
    assert_eq!(
        parse_sgr(&semi),
        vec![SgrAttr::Fg(Color::Rgb(Rgb::new(10, 20, 30)))]
    );
    // Colon form with empty colorspace field: 38:2::10:20:30
    let colon = only_csi(b"\x1b[38:2::10:20:30m");
    assert_eq!(
        parse_sgr(&colon),
        vec![SgrAttr::Fg(Color::Rgb(Rgb::new(10, 20, 30)))]
    );
}

#[test]
fn sgr_256_palette() {
    let csi = only_csi(b"\x1b[38;5;196m");
    assert_eq!(parse_sgr(&csi), vec![SgrAttr::Fg(Color::Palette(196))]);
}

#[test]
fn sgr_bright_fg_and_bg() {
    let csi = only_csi(b"\x1b[91;100m");
    assert_eq!(
        parse_sgr(&csi),
        vec![
            SgrAttr::Fg(Color::Palette(9)),
            SgrAttr::Bg(Color::Palette(8))
        ]
    );
}

#[test]
fn sgr_underline_styles_and_color() {
    let csi = only_csi(b"\x1b[21;4:3;58;2;10;20;30;59m");
    assert_eq!(
        parse_sgr(&csi),
        vec![
            SgrAttr::DoubleUnderline,
            SgrAttr::CurlyUnderline,
            SgrAttr::UnderlineColor(Color::Rgb(Rgb::new(10, 20, 30))),
            SgrAttr::DefaultUnderlineColor,
        ]
    );
}

#[test]
fn sgr_colon_underline_does_not_consume_semicolon_params() {
    let csi = only_csi(b"\x1b[4;3m");
    assert_eq!(parse_sgr(&csi), vec![SgrAttr::Underline, SgrAttr::Italic]);
}

#[test]
fn parse_sgr_into_reuses_buffer_with_identical_results() {
    // A caller-owned buffer left dirty (and over-capacity) from a prior,
    // longer SGR must end up identical to a fresh `parse_sgr` call — reuse
    // must not leak stale entries or depend on starting empty.
    let mut buf = vec![
        SgrAttr::Bold,
        SgrAttr::Italic,
        SgrAttr::Underline,
        SgrAttr::Reset,
    ];

    let reset = only_csi(b"\x1b[m");
    parse_sgr_into(&reset, &mut buf);
    assert_eq!(buf, parse_sgr(&reset));

    let multi = only_csi(b"\x1b[21;4:3;58;2;10;20;30;59m");
    parse_sgr_into(&multi, &mut buf);
    assert_eq!(buf, parse_sgr(&multi));

    let truecolor = only_csi(b"\x1b[38;2;10;20;30m");
    parse_sgr_into(&truecolor, &mut buf);
    assert_eq!(buf, parse_sgr(&truecolor));

    let palette = only_csi(b"\x1b[38;5;196m");
    parse_sgr_into(&palette, &mut buf);
    assert_eq!(buf, parse_sgr(&palette));
}

#[test]
fn cup_two_params() {
    let csi = only_csi(b"\x1b[5;10H");
    assert_eq!(csi.params(), &[5, 10]);
    assert_eq!(csi.final_byte, b'H');
    assert_eq!(csi.param(0, 1), 5);
    assert_eq!(csi.param(1, 1), 10);
}

#[test]
fn cup_leading_empty_param_defaults() {
    // ESC [ ; 5 H  →  row defaults to 1, col = 5
    let csi = only_csi(b"\x1b[;5H");
    assert_eq!(csi.param(0, 1), 1);
    assert_eq!(csi.param(1, 1), 5);
}

#[test]
fn private_mode_marker() {
    // ESC [ ? 2 5 h  (DECTCEM show cursor)
    let csi = only_csi(b"\x1b[?25h");
    assert_eq!(csi.private, b'?');
    assert_eq!(csi.params(), &[25]);
    assert_eq!(csi.final_byte, b'h');
}

#[test]
fn decscusr_has_space_intermediate() {
    let csi = only_csi(b"\x1b[4 q");

    assert_eq!(csi.params(), &[4]);
    assert_eq!(csi.intermediates(), b" ");
    assert_eq!(csi.final_byte, b'q');
}

#[test]
fn decslrm_uses_plain_s_final() {
    let csi = only_csi(b"\x1b[3;7s");

    assert_eq!(csi.params(), &[3, 7]);
    assert!(csi.intermediates().is_empty());
    assert_eq!(csi.private, 0);
    assert_eq!(csi.final_byte, b's');
}

#[test]
fn keypad_modes_parse_as_esc_dispatch() {
    assert_eq!(
        actions(b"\x1b=\x1b>"),
        vec![
            Action::EscDispatch(Esc::new(&[], b'=')),
            Action::EscDispatch(Esc::new(&[], b'>')),
        ]
    );
}

#[test]
fn missing_param_zero_default() {
    // ESC [ A  (CUU with no param) → default 1
    let csi = only_csi(b"\x1b[A");
    assert_eq!(csi.param(0, 1), 1);
}

#[test]
fn utf8_decoding() {
    // "é" = C3 A9 ; "→" = E2 86 92 ; "😀" = F0 9F 98 80
    assert_eq!(actions("é".as_bytes()), vec![Action::Print('é')]);
    assert_eq!(actions("→".as_bytes()), vec![Action::Print('→')]);
    assert_eq!(actions("😀".as_bytes()), vec![Action::Print('😀')]);
}

#[test]
fn utf8_invalid_yields_replacement() {
    // A lone continuation byte 0x80 is invalid.
    assert_eq!(actions(&[0x80]), vec![Action::Print('\u{FFFD}')]);
}

#[test]
fn utf8_overlong_and_out_of_range_yield_replacement() {
    assert_eq!(actions(&[0xc2, 0x80]), vec![Action::Print('\u{80}')]);
    assert_eq!(actions(&[0xe0, 0xa0, 0x80]), vec![Action::Print('\u{800}')]);
    assert_eq!(
        actions(&[0xf0, 0x90, 0x80, 0x80]),
        vec![Action::Print('\u{10000}')]
    );

    // Overlong encodings must not decode to their shortest-form scalar.
    assert_eq!(
        actions(&[0xc0, 0xaf]), // overlong '/'
        vec![Action::Print('\u{FFFD}'), Action::Print('\u{FFFD}')]
    );
    assert_eq!(
        actions(&[0xe0, 0x80, 0xaf]), // overlong '/'
        vec![Action::Print('\u{FFFD}')]
    );

    // F5..FF cannot start valid UTF-8, and F4 90 80 80 is above U+10FFFF.
    assert_eq!(actions(&[0xf5]), vec![Action::Print('\u{FFFD}')]);
    assert_eq!(
        actions(&[0xf4, 0x90, 0x80, 0x80]),
        vec![Action::Print('\u{FFFD}')]
    );
    assert_eq!(
        actions(&[0xed, 0xa0, 0x80]), // surrogate range
        vec![Action::Print('\u{FFFD}')]
    );
    assert_eq!(
        actions(&[0xe2, b'A']),
        vec![Action::Print('\u{FFFD}'), Action::Print('A')]
    );
}

#[test]
fn osc_title_captured() {
    // ESC ] 0 ; hi BEL
    let acts = actions(b"\x1b]0;hi\x07");
    assert_eq!(acts, vec![Action::OscDispatch(b"0;hi".to_vec())]);
}

#[test]
fn osc_terminated_by_st() {
    // ESC ] 2 ; t ESC \
    let acts = actions(b"\x1b]2;t\x1b\\");
    assert!(acts.contains(&Action::OscDispatch(b"2;t".to_vec())));
}

// Regression: UTF-8 payload bytes equal to 0x9c (continuation byte of e.g.
// 作 E4 BD 9C, 検 E6 A4 9C) must not be taken as 8-bit ST. Claude Code sets
// Japanese task summaries as the title every frame; the truncated remainder
// used to print into the grid at the cursor ("勝手に入力").
#[test]
fn osc_utf8_payload_with_9c_continuation_is_not_terminated() {
    // ESC ] 0 ; ⠐ 俳句を2つ作成する BEL — the exact shape Claude Code emits.
    let payload = "0;⠐ 俳句を2つ作成する".as_bytes().to_vec();
    let bytes = [b"\x1b]".as_slice(), &payload, b"\x07"].concat();
    assert_eq!(actions(&bytes), vec![Action::OscDispatch(payload)]);
}

#[test]
fn osc_8bit_st_still_terminates_between_utf8_scalars() {
    let mut bytes = b"\x1b]2;".to_vec();
    bytes.extend_from_slice("作".as_bytes()); // complete scalar
    bytes.push(0x9c); // real 8-bit ST at a scalar boundary
    let acts = actions(&bytes);
    assert!(acts.contains(&Action::OscDispatch("2;作".as_bytes().to_vec())));
}

#[test]
fn dcs_and_apc_utf8_payloads_with_9c_continuation_are_not_terminated() {
    let dcs = ["\x1bP".as_bytes(), "検".as_bytes(), b"\x1b\\"].concat();
    assert_eq!(
        actions(&dcs),
        vec![Action::DcsDispatch(crate::DcsPayload {
            data: "検".as_bytes().to_vec(),
        })]
    );
    let apc = ["\x1b_".as_bytes(), "作".as_bytes(), b"\x1b\\"].concat();
    assert!(actions(&apc).contains(&Action::ApcDispatch {
        data: "作".as_bytes().to_vec(),
        truncated: false,
    }));
}

#[test]
fn sos_pm_utf8_payload_with_9c_continuation_is_not_terminated() {
    // SOS payload is discarded, but a continuation 0x9c inside it must not
    // end the string early — the tail would print as text.
    let mut bytes = b"\x1bX".to_vec();
    bytes.extend_from_slice("作成".as_bytes());
    bytes.push(0x9c); // real ST at a scalar boundary ends the string
    bytes.extend_from_slice(b"ok");
    assert_eq!(
        actions(&bytes),
        vec![Action::Print('o'), Action::Print('k')]
    );
}

#[test]
fn osc8_hyperlink_payload_captured() {
    let acts = actions(b"\x1b]8;id=docs;https://example.test\x1b\\");

    assert!(acts.contains(&Action::OscDispatch(
        b"8;id=docs;https://example.test".to_vec()
    )));
}

#[test]
fn dcs_payload_dispatches_on_st() {
    assert_eq!(
        actions(b"\x1bP$qm\x1b\\"),
        vec![Action::DcsDispatch(crate::DcsPayload {
            data: b"$qm".to_vec(),
        })]
    );
}

#[test]
fn dcs_payload_dispatches_on_c1_st() {
    assert_eq!(
        actions(b"\x1bP+q544e\x9c"),
        vec![Action::DcsDispatch(crate::DcsPayload {
            data: b"+q544e".to_vec(),
        })]
    );
}

#[test]
fn dcs_overflow_is_discarded_without_dispatch() {
    let mut bytes = b"\x1bP".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', crate::parser::MAX_DCS_BYTES + 1));
    bytes.extend_from_slice(b"\x1b\\");

    assert!(
        actions(&bytes)
            .into_iter()
            .all(|action| !matches!(action, Action::DcsDispatch(_)))
    );
}

#[test]
fn osc133_prompt_mark_payload_captured() {
    let acts = actions(b"\x1b]133;D;0\x07");

    assert_eq!(acts, vec![Action::OscDispatch(b"133;D;0".to_vec())]);
}

#[test]
fn osc_payload_over_limit_is_dropped() {
    // One byte past the 12 MiB cap (sized for OSC 52 clipboard payloads).
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 12 * (1 << 20) + 1));
    bytes.push(0x07);

    let acts = actions(&bytes);

    assert!(
        !acts
            .iter()
            .any(|action| matches!(action, Action::OscDispatch(_)))
    );
}

#[test]
fn osc_overflow_releases_the_buffer_allocation() {
    // A runaway OSC that hits the cap must free the 12 MiB accumulation
    // buffer, not `clear()` it — clearing pins the capacity for the
    // parser's (i.e. the pane's) whole life.
    let mut parser = crate::parser::Parser::new();
    let mut sink = |_: Action| {};
    for &b in b"\x1b]0;".iter() {
        parser.advance(b, &mut sink);
    }
    for _ in 0..(12 * (1 << 20) + 1) {
        parser.advance(b'a', &mut sink);
    }
    parser.advance(0x07, &mut sink); // BEL terminates the OSC

    assert_eq!(parser.state(), crate::state::State::Ground);
    assert_eq!(parser.osc_buffer_capacity(), 0);
}

#[test]
fn esc_dispatch_ris_and_index() {
    assert_eq!(
        actions(b"\x1bc"),
        vec![Action::EscDispatch(Esc::new(&[], b'c'))]
    );
}

#[test]
fn charset_designation_has_intermediate() {
    // ESC ( B  → EscDispatch with intermediate '('
    let acts = actions(b"\x1b(B");
    assert_eq!(acts, vec![Action::EscDispatch(Esc::new(b"(", b'B'))]);
}

#[test]
fn print_after_csi_returns_to_ground() {
    let acts = actions(b"\x1b[0mX");
    assert_eq!(acts.last(), Some(&Action::Print('X')));
}

#[test]
fn c0_in_the_middle_of_csi_executes() {
    // A CR embedded in a CSI parameter run is executed immediately (xterm behavior).
    let acts = actions(b"\x1b[3\r1m");
    assert!(acts.contains(&Action::Execute(0x0d)));
    // The sequence still completes as SGR 31.
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::CsiDispatch(c) if c.final_byte == b'm'))
    );
}

#[test]
fn c1_csi_dispatches_like_escape_bracket() {
    let acts = actions(&[0x9b, b'3', b'1', b'm', b'X']);
    assert_eq!(
        acts,
        vec![
            Action::CsiDispatch(Csi::new(&[31], &[], &[], 0, b'm')),
            Action::Print('X'),
        ]
    );
}

#[test]
fn c1_string_controls_dispatch_and_st_terminates() {
    assert_eq!(
        actions(&[0x9d, b'2', b';', b't', 0x9c]),
        vec![Action::OscDispatch(b"2;t".to_vec())]
    );
    assert_eq!(
        actions(&[0x90, b'+', b'q', b'5', b'4', b'4', b'e', 0x9c]),
        vec![Action::DcsDispatch(crate::DcsPayload {
            data: b"+q544e".to_vec(),
        })]
    );

    let (data, truncated) = only_apc(&[0x9f, b'G', b'i', b'=', b'1', 0x9c]);
    assert_eq!(data, b"Gi=1");
    assert!(!truncated);
}

// ── APC bounded capture (Kitty graphics transport) ─────────────────

/// Extract the single APC dispatch in `bytes` (panics otherwise).
fn only_apc(bytes: &[u8]) -> (Vec<u8>, bool) {
    match actions(bytes)
        .into_iter()
        .find(|a| matches!(a, Action::ApcDispatch { .. }))
    {
        Some(Action::ApcDispatch { data, truncated }) => (data, truncated),
        _ => panic!("no APC dispatch in {bytes:?}"),
    }
}

#[test]
fn apc_payload_dispatches_on_st() {
    let (data, truncated) = only_apc(b"\x1b_Gi=1,a=q;AAAA\x1b\\");
    assert_eq!(data, b"Gi=1,a=q;AAAA");
    assert!(!truncated);
}

#[test]
fn apc_payload_dispatches_on_c1_st() {
    // 8-bit ST (0x9c) terminates the APC just like 7-bit `ESC \`.
    let (data, truncated) = only_apc(b"\x1b_Gf=24;payload\x9c");
    assert_eq!(data, b"Gf=24;payload");
    assert!(!truncated);
}

#[test]
fn apc_sos_pm_still_discarded() {
    // ESC X (SOS) and ESC ^ (PM) keep the old discard behavior — no dispatch.
    for lead in [b"\x1bX".as_slice(), b"\x1b^".as_slice()] {
        let mut bytes = lead.to_vec();
        bytes.extend_from_slice(b"whatever\x1b\\");
        assert!(
            actions(&bytes)
                .into_iter()
                .all(|a| !matches!(a, Action::ApcDispatch { .. })),
            "lead {lead:?} should not dispatch"
        );
    }

    for lead in [0x98, 0x9e] {
        let mut bytes = vec![lead];
        bytes.extend_from_slice(&[b'w', b'h', b'a', b't', 0x9b, b'3', b'1', b'm', 0x9c]);
        assert!(
            actions(&bytes).is_empty(),
            "C1 SOS/PM payload must be discarded through 8-bit ST"
        );
    }
}

#[test]
fn apc_can_aborts_without_dispatch() {
    // CAN (0x18) mid-payload abandons the APC entirely.
    let acts = actions(b"\x1b_Gi=1;AAAA\x18more");
    assert!(
        acts.iter()
            .all(|a| !matches!(a, Action::ApcDispatch { .. })),
        "CAN should abort the APC"
    );
    // ...and the trailing bytes print normally in ground.
    assert_eq!(acts.last(), Some(&Action::Print('e')));
}

#[test]
fn apc_overflow_dispatches_truncated() {
    let mut bytes = b"\x1b_G".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', (1 << 20) + 10));
    bytes.extend_from_slice(b"\x1b\\");

    let (data, truncated) = only_apc(&bytes);
    assert!(truncated, "over-limit APC must be flagged truncated");
    assert_eq!(data.len(), 1 << 20, "capture caps at MAX_APC_BYTES");
    assert!(data.starts_with(b"Ga"));
}

#[test]
fn apc_survives_byte_at_a_time_feed() {
    // Feeding one byte per advance() call must not change capture (split resistance).
    let mut p = Parser::new();
    let mut out = Vec::new();
    for &b in b"\x1b_Gi=2,f=100;Zm9v\x1b\\" {
        p.advance(b, &mut |a| out.push(a));
    }
    let dispatched: Vec<_> = out
        .into_iter()
        .filter_map(|a| match a {
            Action::ApcDispatch { data, truncated } => Some((data, truncated)),
            _ => None,
        })
        .collect();
    assert_eq!(dispatched, vec![(b"Gi=2,f=100;Zm9v".to_vec(), false)]);
}

#[test]
fn apc_esc_non_backslash_aborts_and_reprocesses() {
    // ESC inside APC followed by a non-`\` byte abandons the APC and the ESC
    // sequence is reparsed from Escape (here ESC c = RIS).
    let acts = actions(b"\x1b_Gi=1;AA\x1bc");
    assert!(
        acts.iter()
            .all(|a| !matches!(a, Action::ApcDispatch { .. }))
    );
    assert!(acts.iter().any(|a| matches!(
        a,
        Action::EscDispatch(e) if e.final_byte == b'c'
    )));
}

// ── APC → Kitty graphics dispatch (Stream integration) ─────────────

/// Feed `bytes` through a full [`crate::Stream`] and return the Kitty graphics
/// commands the handler received.
fn kitty_commands(bytes: &[u8]) -> Vec<crate::KittyGraphicsCommand> {
    use crate::handler::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler};
    use crate::sgr::SgrAttr;

    #[derive(Default)]
    struct Capture {
        cmds: Vec<crate::KittyGraphicsCommand>,
    }
    // Only `kitty_graphics` is meaningful here; every required method no-ops.
    impl Handler for Capture {
        fn print(&mut self, _c: char) {}
        fn execute_c0(&mut self, _byte: u8) {}
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: EraseDisplay) {}
        fn erase_line(&mut self, _mode: EraseLine) {}
        fn set_attributes(&mut self, _attrs: &[SgrAttr]) {}
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: DaKind) {}
        fn device_status_report(&mut self, _kind: DsrKind) {}
        fn kitty_graphics(&mut self, cmd: crate::KittyGraphicsCommand) {
            self.cmds.push(cmd);
        }
    }

    let mut stream = crate::Stream::new();
    let mut cap = Capture::default();
    stream.feed(bytes, &mut cap);
    cap.cmds
}

/// Feed `bytes` through [`crate::Stream`] and return the SIXEL graphics commands
/// the handler received plus raw non-SIXEL DCS payloads.
fn sixel_and_dcs(bytes: &[u8]) -> (Vec<crate::SixelGraphicsCommand>, Vec<Vec<u8>>) {
    use crate::handler::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler};
    use crate::sgr::SgrAttr;

    #[derive(Default)]
    struct Capture {
        sixel: Vec<crate::SixelGraphicsCommand>,
        dcs: Vec<Vec<u8>>,
    }
    impl Handler for Capture {
        fn print(&mut self, _c: char) {}
        fn execute_c0(&mut self, _byte: u8) {}
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: EraseDisplay) {}
        fn erase_line(&mut self, _mode: EraseLine) {}
        fn set_attributes(&mut self, _attrs: &[SgrAttr]) {}
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: DaKind) {}
        fn device_status_report(&mut self, _kind: DsrKind) {}
        fn dcs_dispatch(&mut self, data: &[u8]) {
            self.dcs.push(data.to_vec());
        }
        fn sixel_graphics(&mut self, cmd: crate::SixelGraphicsCommand) {
            self.sixel.push(cmd);
        }
    }

    let mut stream = crate::Stream::new();
    let mut cap = Capture::default();
    stream.feed(bytes, &mut cap);
    (cap.sixel, cap.dcs)
}

#[test]
fn dcs_sixel_dispatch_parses_params_and_payload() {
    let (sixel, dcs) = sixel_and_dcs(b"\x1bP1;2;3q#1~~\x1b\\");

    assert!(dcs.is_empty());
    assert_eq!(sixel.len(), 1);
    assert_eq!(sixel[0].aspect_ratio, 1);
    assert_eq!(sixel[0].background, 2);
    assert_eq!(sixel[0].horizontal_grid_size, 3);
    assert_eq!(sixel[0].data, b"#1~~");
}

#[test]
fn non_sixel_dcs_still_dispatches_as_raw_dcs() {
    let (sixel, dcs) = sixel_and_dcs(b"\x1bP$qm\x1b\\");

    assert!(sixel.is_empty());
    assert_eq!(dcs, vec![b"$qm".to_vec()]);
}

#[test]
fn apc_kitty_dispatch_parses_control_and_payload() {
    let cmds = kitty_commands(b"\x1b_Gi=5,a=T,f=100;iVBORw0K\x1b\\");
    assert_eq!(cmds.len(), 1);
    let c = &cmds[0];
    assert_eq!(c.image_id, 5);
    assert_eq!(c.action, crate::KittyAction::TransmitAndDisplay);
    assert_eq!(c.format, crate::KittyFormat::Png);
    assert_eq!(c.payload, b"iVBORw0K");
    assert!(!c.parse_error);
}

#[test]
fn apc_non_g_is_ignored() {
    // APC not starting with 'G' is dropped (no Kitty dispatch).
    assert!(kitty_commands(b"\x1b_Xother\x1b\\").is_empty());
}

#[test]
fn apc_truncated_still_dispatches() {
    let mut bytes = b"\x1b_Gi=1;".to_vec();
    bytes.extend(std::iter::repeat_n(b'A', (1 << 20) + 4));
    bytes.extend_from_slice(b"\x1b\\");
    let cmds = kitty_commands(&bytes);
    assert_eq!(cmds.len(), 1);
    assert!(cmds[0].truncated);
}

// ── Stream ground-state text-run fast path ──────────────────────────

/// What a `Stream`-driven handler received, in order: bulk runs
/// (`print_str`), per-scalar prints (the DFA path), and C0 executes.
#[derive(Debug, PartialEq)]
enum TextEvent {
    Run(String),
    Scalar(char),
    C0(u8),
    Csi(u8),
}

fn text_events(chunks: &[&[u8]]) -> Vec<TextEvent> {
    use crate::handler::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler};
    use crate::sgr::SgrAttr;

    #[derive(Default)]
    struct Capture {
        events: Vec<TextEvent>,
    }
    impl Handler for Capture {
        fn print(&mut self, c: char) {
            self.events.push(TextEvent::Scalar(c));
        }
        fn print_str(&mut self, s: &str) {
            self.events.push(TextEvent::Run(s.to_string()));
        }
        fn execute_c0(&mut self, byte: u8) {
            self.events.push(TextEvent::C0(byte));
        }
        fn set_attributes(&mut self, _attrs: &[SgrAttr]) {
            self.events.push(TextEvent::Csi(b'm'));
        }
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: EraseDisplay) {}
        fn erase_line(&mut self, _mode: EraseLine) {}
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: DaKind) {}
        fn device_status_report(&mut self, _kind: DsrKind) {}
    }

    let mut stream = crate::Stream::new();
    let mut cap = Capture::default();
    for chunk in chunks {
        stream.feed(chunk, &mut cap);
    }
    cap.events
}

#[test]
fn stream_bulk_prints_ground_runs_including_utf8() {
    assert_eq!(
        text_events(&["hello 日本\x1b[31mworld".as_bytes()]),
        vec![
            TextEvent::Run("hello 日本".to_string()),
            TextEvent::Csi(b'm'),
            TextEvent::Run("world".to_string()),
        ]
    );
}

#[test]
fn stream_bulk_run_splits_at_c0_and_del() {
    assert_eq!(
        text_events(&[b"ab\rcd\x7fef"]),
        vec![
            TextEvent::Run("ab".to_string()),
            TextEvent::C0(0x0d),
            TextEvent::Run("cd".to_string()),
            // DEL is ignored in ground, but must still split the run.
            TextEvent::Run("ef".to_string()),
        ]
    );
}

#[test]
fn stream_utf8_split_across_feeds_falls_back_to_the_dfa() {
    let bytes = "日".as_bytes();
    assert_eq!(
        text_events(&[&bytes[..1], &bytes[1..]]),
        vec![TextEvent::Scalar('日')]
    );
    // A multibyte scalar split *within* one chunk's run tail behaves the same.
    let mixed = "ab日".as_bytes();
    assert_eq!(
        text_events(&[&mixed[..3], &mixed[3..]]),
        vec![TextEvent::Run("ab".to_string()), TextEvent::Scalar('日'),]
    );
}

#[test]
fn stream_invalid_utf8_yields_replacement_between_bulk_runs() {
    assert_eq!(
        text_events(&[b"ab\xffcd"]),
        vec![
            TextEvent::Run("ab".to_string()),
            TextEvent::Scalar('\u{FFFD}'),
            TextEvent::Run("cd".to_string()),
        ]
    );
    // A truncated sequence interrupted by a control: replacement, then C0.
    assert_eq!(
        text_events(&[b"\xe6\x97\rx"]),
        vec![
            TextEvent::Scalar('\u{FFFD}'),
            TextEvent::C0(0x0d),
            TextEvent::Run("x".to_string()),
        ]
    );
}

// ── scan_run boundary-scan + UTF-8-fast-path regression coverage ────
//
// `scan_run` (the merged word-at-a-time boundary scan + ASCII fast-path
// used by `Stream::feed`'s ground-run fast path) processes 8 bytes at a
// time. Off-by-one bugs in that chunking characteristically show up right
// at chunk edges, so the tests below sweep run lengths and byte offsets
// through several 8-byte-word boundaries (0, 8, 16) rather than relying on
// a couple of hand-picked examples.

/// Reference oracle for `scan_run`'s boundary rule, independent of its SWAR
/// implementation: the first byte that is a C0 control or DEL.
fn naive_run_end(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&b| b < 0x20 || b == 0x7f)
        .unwrap_or(bytes.len())
}

#[test]
fn scan_run_matches_naive_boundary_and_ascii_flag_across_word_boundaries() {
    for len in 0..=20usize {
        let base = vec![b'a'; len];
        let (end, ascii) = crate::stream::scan_run(&base);
        assert_eq!(end, naive_run_end(&base), "len={len} all-ascii run_end");
        assert_eq!(end, len, "len={len} all-ascii run_end should reach the end");
        assert!(ascii, "len={len} all-ascii run should report ascii=true");

        for pos in 0..len {
            // Bytes that must split the run (C0 controls, DEL) and bytes
            // that must not (ordinary ASCII edges, C1/continuation bytes,
            // stray UTF-8 lead bytes) — the latter only flip the ascii flag.
            for &special in &[0x00u8, 0x01, 0x1f, 0x7f, 0x20, 0x7e, 0x80, 0x9f, 0xc0, 0xff] {
                let mut buf = base.clone();
                buf[pos] = special;
                let (end, ascii) = crate::stream::scan_run(&buf);
                let expected_end = naive_run_end(&buf);
                assert_eq!(
                    end, expected_end,
                    "len={len} pos={pos} special={special:#04x} run_end mismatch"
                );
                let expected_ascii = buf[..expected_end].iter().all(|&b| b < 0x80);
                assert_eq!(
                    ascii, expected_ascii,
                    "len={len} pos={pos} special={special:#04x} ascii flag mismatch"
                );
            }
        }
    }
}

/// Build the expected `TextEvent`s for an all-`a` run of `len` bytes with a
/// single byte at `pos` replaced by one event (a C0 execute or a decode
/// error's replacement scalar); `None` reproduces a plain unbroken run.
fn split_run_expectation(len: usize, pos: usize, mid: Option<TextEvent>) -> Vec<TextEvent> {
    let mut expected = Vec::new();
    if pos > 0 {
        expected.push(TextEvent::Run("a".repeat(pos)));
    }
    if let Some(e) = mid {
        expected.push(e);
    }
    let suffix_len = len - pos - 1;
    if suffix_len > 0 {
        expected.push(TextEvent::Run("a".repeat(suffix_len)));
    }
    expected
}

#[test]
fn stream_control_bytes_split_ascii_runs_across_word_boundaries() {
    for len in 1..=20usize {
        for pos in 0..len {
            for &byte in &[0x00u8, 0x08, 0x1f, 0x0d] {
                let mut buf = vec![b'a'; len];
                buf[pos] = byte;
                assert_eq!(
                    text_events(&[&buf]),
                    split_run_expectation(len, pos, Some(TextEvent::C0(byte))),
                    "control byte len={len} pos={pos} byte={byte:#04x}"
                );
            }
        }
    }
}

#[test]
fn stream_lone_c1_and_stray_lead_bytes_are_invalid_utf8_across_word_boundaries() {
    // A single C1 byte outside the recognized 8-bit CSI/OSC/DCS/APC/SOS/PM
    // introducers (0x90/0x98/0x9b/0x9c/0x9d/0x9e/0x9f — see `c1_control`),
    // or a stray multi-byte lead/continuation byte, is a run byte
    // (is_run_byte doesn't special-case C1) but on its own is never valid
    // UTF-8, so the slow from_utf8 path must still catch it and emit
    // exactly one replacement scalar — regardless of which SWAR chunk it
    // lands in.
    for len in 1..=20usize {
        for pos in 0..len {
            for &byte in &[0x80u8, 0x81, 0x8a, 0x99, 0xa0, 0xc0, 0xff] {
                let mut buf = vec![b'a'; len];
                buf[pos] = byte;
                assert_eq!(
                    text_events(&[&buf]),
                    split_run_expectation(len, pos, Some(TextEvent::Scalar('\u{FFFD}'))),
                    "invalid utf8 byte len={len} pos={pos} byte={byte:#04x}"
                );
            }
        }
    }
}

#[test]
fn stream_cjk_and_emoji_runs_stay_bulk_across_word_boundaries() {
    // A multi-byte scalar straddling the SWAR 8-byte chunk boundary must
    // still decode as part of one uninterrupted bulk run, not get split or
    // misdetected by the boundary scan.
    for pos in 0..=12usize {
        for ch in ["日", "\u{1f4a9}"] {
            let text = format!("{}{}{}", "a".repeat(pos), ch, "a".repeat(9));
            assert_eq!(
                text_events(&[text.as_bytes()]),
                vec![TextEvent::Run(text.clone())],
                "pos={pos} ch={ch:?}"
            );
        }
    }
}

#[test]
fn stream_overlong_sequence_across_word_boundaries_yields_replacements() {
    // `0xc0 0xaf` is an overlong (invalid) 2-byte encoding of '/': each byte
    // is individually invalid, so it must yield two replacement scalars no
    // matter where the DFA detour starts relative to the SWAR chunking.
    for pos in 0..=14usize {
        let mut buf = vec![b'a'; pos];
        buf.extend_from_slice(&[0xc0, 0xaf]);
        buf.extend(std::iter::repeat_n(b'a', 9));

        let mut expected = Vec::new();
        if pos > 0 {
            expected.push(TextEvent::Run("a".repeat(pos)));
        }
        expected.push(TextEvent::Scalar('\u{FFFD}'));
        expected.push(TextEvent::Scalar('\u{FFFD}'));
        expected.push(TextEvent::Run("a".repeat(9)));

        assert_eq!(text_events(&[&buf]), expected, "pos={pos}");
    }
}

#[test]
fn stream_bulk_run_huge_ascii_is_a_single_run() {
    let text = "x".repeat(10_000);
    assert_eq!(text_events(&[text.as_bytes()]), vec![TextEvent::Run(text)]);
}

// ── scan_run adversarial verification: SWAR unsafe-soundness audit ──
//
// `Stream::feed`'s fast path trusts `scan_run`'s `ascii` flag to skip UTF-8
// validation via `from_utf8_unchecked` (see the `SAFETY` comment on its call
// site in `stream.rs`). If `scan_run` ever reported `ascii = true` for a
// range containing a byte `>= 0x80`, that would be a memory-safety bug, not
// just a logic bug. The SWAR `haslessthan`/`haszero` tricks it uses compute
// a real 64-bit subtraction per chunk, and subtraction borrows can
// propagate from one byte lane into the next, so the tests below fuzz
// `scan_run` against a byte-by-byte reference oracle from three angles:
// (1) every one of the 256 byte values at every position, swept across
// several 8-byte SWAR-chunk boundaries, (2) every 256x256 adjacent-byte
// pair at every intra-chunk lane transition, to rule out a borrow from lane
// `k` corrupting the result at lane `k+1`, and (3) large-scale seeded
// random fuzzing over arbitrary buffers.

/// Reference oracle: `scan_run`'s documented contract, computed the slow
/// but obviously-correct way (equivalent to the pre-SWAR two-pass
/// implementation this function replaced).
fn naive_scan_run(bytes: &[u8]) -> (usize, bool) {
    let end = naive_run_end(bytes);
    (end, bytes[..end].iter().all(|&b| b < 0x80))
}

#[test]
fn scan_run_exhaustive_all_256_byte_values_at_every_position() {
    // Sweep run lengths through the 1st, 2nd, and into the 3rd 8-byte SWAR
    // chunk, every insertion position, every possible byte value (0-255),
    // not just the hand-picked "special" bytes the sweep test above uses.
    for len in 1..=17usize {
        for pos in 0..len {
            for byte in 0u16..=255 {
                let byte = byte as u8;
                let mut buf = vec![b'a'; len];
                buf[pos] = byte;
                let expected = naive_scan_run(&buf);
                let actual = crate::stream::scan_run(&buf);
                assert_eq!(actual, expected, "len={len} pos={pos} byte={byte:#04x}");
            }
        }
    }
}

#[test]
fn scan_run_exhaustive_adjacent_byte_pairs_no_cross_lane_contamination() {
    // A SWAR subtraction borrow can only ever propagate from one lane into
    // the immediately next lane (the borrow state is a single bit), so
    // exhaustively covering every (b0, b1) pair at every adjacent
    // intra-chunk lane transition (0,1)..(6,7), for every possible
    // incoming-borrow-inducing predecessor value, fully rules out
    // cross-lane corruption — including arbitrarily long propagation
    // chains, since the recurrence is memoryless beyond that one bit.
    for pos in 0..7usize {
        for b0 in 0u16..=255 {
            for b1 in 0u16..=255 {
                let mut buf = [b'A'; 16];
                buf[pos] = b0 as u8;
                buf[pos + 1] = b1 as u8;
                let expected = naive_scan_run(&buf);
                let actual = crate::stream::scan_run(&buf);
                assert_eq!(actual, expected, "pos={pos} b0={b0:#04x} b1={b1:#04x}");
            }
        }
    }
}

/// Minimal splitmix64: deterministic, dependency-free PRNG for the fuzz
/// sweep below (`noa-vt` has no `rand` dev-dependency).
struct SplitMix64(u64);
impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[test]
fn scan_run_random_fuzz_matches_naive_oracle() {
    // Fixed seed => fully deterministic/reproducible; 2M random-length,
    // random-byte buffers cross-checked against the naive oracle.
    let mut rng = SplitMix64(0xC0FF_EE15_5EED_1234);
    for _ in 0..2_000_000u32 {
        let len = (rng.next_u64() % 48) as usize;
        let buf: Vec<u8> = (0..len).map(|_| (rng.next_u64() % 256) as u8).collect();
        let expected = naive_scan_run(&buf);
        let actual = crate::stream::scan_run(&buf);
        assert_eq!(actual, expected, "buf={buf:?}");
    }
}

// ── Stream::take_display_dirty (query-only batch detection) ────────

/// Feed `bytes` through a fresh no-op-handler [`crate::Stream`] and return
/// `take_display_dirty()`.
fn display_dirty_after(bytes: &[u8]) -> bool {
    use crate::handler::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler};
    use crate::sgr::SgrAttr;

    struct NoOp;
    impl Handler for NoOp {
        fn print(&mut self, _c: char) {}
        fn execute_c0(&mut self, _byte: u8) {}
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: EraseDisplay) {}
        fn erase_line(&mut self, _mode: EraseLine) {}
        fn set_attributes(&mut self, _attrs: &[SgrAttr]) {}
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: DaKind) {}
        fn device_status_report(&mut self, _kind: DsrKind) {}
    }

    let mut stream = crate::Stream::new();
    stream.feed(bytes, &mut NoOp);
    stream.take_display_dirty()
}

/// The pure report queries — DSR, DA1/DA2, DECRQM, XTVERSION, Kitty keyboard
/// query — must not mark the display dirty: they only queue replies, and the
/// io thread uses a clean batch to skip its redraw poke.
#[test]
fn pure_report_queries_do_not_dirty_the_display() {
    assert!(!display_dirty_after(b"\x1b[6n"), "DSR CPR");
    assert!(!display_dirty_after(b"\x1b[5n"), "DSR status");
    assert!(!display_dirty_after(b"\x1b[c"), "DA1");
    assert!(!display_dirty_after(b"\x1b[>c"), "DA2");
    assert!(!display_dirty_after(b"\x1b[?2026$p"), "DECRQM (DEC)");
    assert!(!display_dirty_after(b"\x1b[4$p"), "DECRQM (ANSI)");
    assert!(!display_dirty_after(b"\x1b[>q"), "XTVERSION");
    assert!(!display_dirty_after(b"\x1b[?u"), "Kitty keyboard query");
    assert!(
        !display_dirty_after(b"\x1b[6n\x1b[c\x1b[?2026$p\x1b[>q\x1b[?u"),
        "a whole capability-poll burst stays clean"
    );
}

/// Everything else is conservative-dirty: prints (both the ground fast path
/// and DFA-resumed UTF-8), C0 controls, visually meaningful CSI/ESC/OSC, and
/// even sequences noa ignores — a misclassification may only ever cause a
/// spurious repaint, never a stale frame.
#[test]
fn non_query_actions_dirty_the_display() {
    assert!(display_dirty_after(b"x"), "ASCII print (fast path)");
    assert!(display_dirty_after("é".as_bytes()), "UTF-8 print");
    assert!(display_dirty_after(b"\n"), "C0 control");
    assert!(display_dirty_after(b"\x1b[2J"), "erase display");
    assert!(display_dirty_after(b"\x1b[1m"), "SGR");
    assert!(display_dirty_after(b"\x1b[?25l"), "DECSET");
    assert!(display_dirty_after(b"\x1b[!p"), "DECSTR (soft reset)");
    assert!(display_dirty_after(b"\x1b[ q"), "DECSCUSR (cursor style)");
    assert!(display_dirty_after(b"\x1b[>1u"), "Kitty keyboard push");
    assert!(display_dirty_after(b"\x1bc"), "RIS");
    assert!(display_dirty_after(b"\x1b]0;t\x07"), "OSC title");
    assert!(
        display_dirty_after(b"\x1b[9999z"),
        "unknown CSI stays dirty"
    );
    assert!(
        display_dirty_after(b"x\x1b[6n"),
        "mixed print+query is dirty"
    );
}

/// A sequence still mid-parse dispatches nothing and so must stay clean; the
/// flag belongs to the batch whose completed actions dirtied the frame.
#[test]
fn take_display_dirty_resets_per_take_and_ignores_partial_sequences() {
    use crate::handler::Handler;
    struct NoOp2;
    impl Handler for NoOp2 {
        fn print(&mut self, _c: char) {}
        fn execute_c0(&mut self, _byte: u8) {}
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: crate::handler::EraseDisplay) {}
        fn erase_line(&mut self, _mode: crate::handler::EraseLine) {}
        fn set_attributes(&mut self, _attrs: &[crate::sgr::SgrAttr]) {}
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: crate::handler::DaKind) {}
        fn device_status_report(&mut self, _kind: crate::handler::DsrKind) {}
    }

    let mut stream = crate::Stream::new();
    let mut h = NoOp2;

    stream.feed(b"x", &mut h);
    assert!(stream.take_display_dirty());
    assert!(!stream.take_display_dirty(), "take resets the flag");

    // Partial CSI: no action completed yet.
    stream.feed(b"\x1b[2", &mut h);
    assert!(!stream.take_display_dirty());
    // Completing it as erase-display dirties.
    stream.feed(b"J", &mut h);
    assert!(stream.take_display_dirty());

    // Partial query completing as a query stays clean.
    stream.feed(b"\x1b[6", &mut h);
    assert!(!stream.take_display_dirty());
    stream.feed(b"n", &mut h);
    assert!(!stream.take_display_dirty());
}

// ── CSI fast-path equivalence: whole-chunk lookahead vs per-byte DFA ──
//
// `Stream::feed` lexes a complete in-chunk CSI sequence with `try_scan_csi`
// and dispatches it without touching the per-byte DFA; sequences split
// across feed boundaries (or containing any byte the DFA treats specially)
// still take the DFA path, with `Parser::scan_csi_params` batching digit
// runs on resume. All three paths must produce identical Handler call
// sequences. The tests below feed CSI-dense streams whole (fast path), one
// byte at a time (pure DFA + trivial resumes), and split at every possible
// position (every partial-sequence resume boundary), asserting the recorded
// call logs are equal.

/// Records every Handler call CSI dispatch can produce, with arguments.
/// Adjacent `print`/`print_str` text coalesces into one entry so run
/// granularity (a chunking artifact by design) doesn't affect equality.
#[derive(Default)]
struct CsiRecorder {
    events: Vec<String>,
    text: String,
}

impl CsiRecorder {
    fn ev(&mut self, event: String) {
        if !self.text.is_empty() {
            let text = std::mem::take(&mut self.text);
            self.events.push(format!("text {text:?}"));
        }
        self.events.push(event);
    }

    fn finish(mut self) -> Vec<String> {
        self.ev(String::from("end"));
        self.events
    }
}

impl crate::handler::Handler for CsiRecorder {
    fn print(&mut self, c: char) {
        self.text.push(c);
    }
    fn print_str(&mut self, s: &str) {
        self.text.push_str(s);
    }
    fn execute_c0(&mut self, byte: u8) {
        self.ev(format!("c0 {byte:#04x}"));
    }
    fn cursor_up(&mut self, n: u16) {
        self.ev(format!("cursor_up {n}"));
    }
    fn cursor_down(&mut self, n: u16) {
        self.ev(format!("cursor_down {n}"));
    }
    fn cursor_forward(&mut self, n: u16) {
        self.ev(format!("cursor_forward {n}"));
    }
    fn cursor_backward(&mut self, n: u16) {
        self.ev(format!("cursor_backward {n}"));
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.ev(format!("cursor_position {row} {col}"));
    }
    fn cursor_col_abs(&mut self, col: u16) {
        self.ev(format!("cursor_col_abs {col}"));
    }
    fn cursor_row_abs(&mut self, row: u16) {
        self.ev(format!("cursor_row_abs {row}"));
    }
    fn erase_display(&mut self, mode: crate::handler::EraseDisplay) {
        self.ev(format!("erase_display {mode:?}"));
    }
    fn erase_line(&mut self, mode: crate::handler::EraseLine) {
        self.ev(format!("erase_line {mode:?}"));
    }
    fn set_attributes(&mut self, attrs: &[SgrAttr]) {
        self.ev(format!("sgr {attrs:?}"));
    }
    fn set_mode(&mut self, value: u16, ansi: bool, on: bool) {
        self.ev(format!("set_mode {value} {ansi} {on}"));
    }
    fn set_cursor_style(&mut self, style: crate::handler::CursorStyle) {
        self.ev(format!("set_cursor_style {style:?}"));
    }
    fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.ev(format!("set_horizontal_margins {left} {right}"));
    }
    fn request_mode(&mut self, request: crate::handler::ModeRequest) {
        self.ev(format!("request_mode {request:?}"));
    }
    fn carriage_return(&mut self) {
        self.ev(String::from("carriage_return"));
    }
    fn linefeed(&mut self) {
        self.ev(String::from("linefeed"));
    }
    fn tab(&mut self, n: u16) {
        self.ev(format!("tab {n}"));
    }
    fn tab_back(&mut self, n: u16) {
        self.ev(format!("tab_back {n}"));
    }
    fn reverse_index(&mut self) {
        self.ev(String::from("reverse_index"));
    }
    fn save_cursor(&mut self) {
        self.ev(String::from("save_cursor"));
    }
    fn restore_cursor(&mut self) {
        self.ev(String::from("restore_cursor"));
    }
    fn full_reset(&mut self) {
        self.ev(String::from("full_reset"));
    }
    fn soft_reset(&mut self) {
        self.ev(String::from("soft_reset"));
    }
    fn clear_tab_stop(&mut self) {
        self.ev(String::from("clear_tab_stop"));
    }
    fn clear_all_tab_stops(&mut self) {
        self.ev(String::from("clear_all_tab_stops"));
    }
    fn insert_blank_chars(&mut self, n: u16) {
        self.ev(format!("insert_blank_chars {n}"));
    }
    fn insert_lines(&mut self, n: u16) {
        self.ev(format!("insert_lines {n}"));
    }
    fn delete_lines(&mut self, n: u16) {
        self.ev(format!("delete_lines {n}"));
    }
    fn delete_chars(&mut self, n: u16) {
        self.ev(format!("delete_chars {n}"));
    }
    fn scroll_up(&mut self, n: u16) {
        self.ev(format!("scroll_up {n}"));
    }
    fn scroll_down(&mut self, n: u16) {
        self.ev(format!("scroll_down {n}"));
    }
    fn erase_chars(&mut self, n: u16) {
        self.ev(format!("erase_chars {n}"));
    }
    fn repeat_preceding_char(&mut self, n: u16) {
        self.ev(format!("repeat_preceding_char {n}"));
    }
    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.ev(format!("set_scroll_region {top} {bottom}"));
    }
    fn device_attributes(&mut self, kind: crate::handler::DaKind) {
        self.ev(format!("device_attributes {kind:?}"));
    }
    fn device_status_report(&mut self, kind: crate::handler::DsrKind) {
        self.ev(format!("device_status_report {kind:?}"));
    }
    fn xtversion_query(&mut self) {
        self.ev(String::from("xtversion_query"));
    }
    fn window_op(&mut self, ps: u16, p1: u16, p2: u16) {
        self.ev(format!("window_op {ps} {p1} {p2}"));
    }
    fn kitty_keyboard_query(&mut self) {
        self.ev(String::from("kitty_keyboard_query"));
    }
    fn kitty_keyboard_push(&mut self, flags: u8) {
        self.ev(format!("kitty_keyboard_push {flags}"));
    }
    fn kitty_keyboard_pop(&mut self, n: u16) {
        self.ev(format!("kitty_keyboard_pop {n}"));
    }
    fn kitty_keyboard_set(&mut self, flags: u8, mode: u16) {
        self.ev(format!("kitty_keyboard_set {flags} {mode}"));
    }
    fn set_modify_other_keys(&mut self, level: u16) {
        self.ev(format!("set_modify_other_keys {level}"));
    }
}

/// Feed `chunks` through a fresh `Stream` and return the recorded call log
/// plus the final display-dirty flag.
fn csi_recorded(chunks: &[&[u8]]) -> (Vec<String>, bool) {
    let mut stream = crate::Stream::new();
    let mut recorder = CsiRecorder::default();
    for chunk in chunks {
        stream.feed(chunk, &mut recorder);
    }
    let dirty = stream.take_display_dirty();
    (recorder.finish(), dirty)
}

/// CSI-dense streams covering the fast path's accept shapes and every bail
/// condition (C0 mid-sequence, CAN abort, ESC restart, DEL, 8-bit ST,
/// misplaced private markers, param overflow, intermediates).
fn csi_equivalence_streams() -> Vec<Vec<u8>> {
    let mut streams: Vec<Vec<u8>> = vec![
        // Fire-style frame slice: CUP + truecolor bg/fg pens + half blocks.
        "\x1b[1;1H\x1b[48;2;7;7;7m\x1b[38;2;223;79;7m▄\x1b[48;2;255;255;255m\x1b[38;2;0;0;0m▄\x1b[0m"
            .as_bytes()
            .to_vec(),
        // Scroll-stress line shape: DECSTBM + home + 256-color + attrs.
        b"\x1b[3;24r\x1b[H\x1b[38;5;196;48;5;17;1mscroll line\x1b[0m\nnext\r\n".to_vec(),
        // Private modes, intermediates, DECSCUSR, XTMODKEYS, DECRQM.
        b"\x1b[?1049h\x1b[?25l\x1b[!p\x1b[4 q\x1b[>4;2m\x1b[?2004$p\x1b[?1049l\x1b[0 q".to_vec(),
        // Colon-form SGR: underline styles and both truecolor spellings.
        b"\x1b[4:3m\x1b[38:2::255:128:0m\x1b[38:2:255:128:0m\x1b[58:5:100m\x1b[4:0m".to_vec(),
        // DFA-special bytes inside a sequence: C0 executes mid-CSI, DEL is
        // ignored, CAN aborts, ESC restarts, 8-bit ST cancels.
        b"\x1b[3;\x087m\x1b[3\x7f1mA\x1b[38;2;1\x18XY\x1b[12\x1b[31mZ\x1b[3\x9cB".to_vec(),
        // Misplaced private marker -> CsiIgnore; empty and default params.
        b"\x1b[1;?5mC\x1b[m\x1b[;m\x1b[H\x1b[;5H".to_vec(),
        // Queries and the Kitty keyboard family.
        b"\x1b[?u\x1b[>1u\x1b[<1u\x1b[=5;2u\x1b[u\x1b[6n\x1b[5n\x1b[c\x1b[>c\x1b[>q".to_vec(),
        // Cursor/edit/tab family with defaults and explicit params.
        b"\x1b[5A\x1b[3B\x1b[2C\x1b[4D\x1b[2E\x1b[2F\x1b[7G\x1b[3d\x1b[2;5H\x1b[3@\x1b[2L\x1b[2M\x1b[2P\x1b[2S\x1b[2T\x1b[2X\x1b[2Z\x1b[3b\x1b[2J\x1b[1K\x1b[0g\x1b[3g\x1b[14t\x1b[22;0;0t\x1b[s\x1b[5;10s".to_vec(),
        // Saturating and overlong digit runs.
        b"\x1b[99999999999999999999m\x1b[65535;65535H".to_vec(),
    ];
    // 40 params: beyond MAX_PARAMS, exercising the DFA's overflow quirk
    // (the fast path must bail so the folded-digit behavior is preserved).
    let mut overflow = b"\x1b[".to_vec();
    for _ in 0..40 {
        overflow.extend_from_slice(b"1;");
    }
    overflow.extend_from_slice(b"5m");
    streams.push(overflow);
    streams
}

#[test]
fn csi_fast_path_equals_dfa_for_whole_and_byte_at_a_time_feeds() {
    for stream in csi_equivalence_streams() {
        let whole = csi_recorded(&[&stream]);
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(
            csi_recorded(&bytes),
            whole,
            "byte-at-a-time diverged for {stream:?}"
        );
    }
}

#[test]
fn csi_fast_path_equals_dfa_at_every_split_position() {
    for stream in csi_equivalence_streams() {
        let whole = csi_recorded(&[&stream]);
        for cut in 1..stream.len() {
            assert_eq!(
                csi_recorded(&[&stream[..cut], &stream[cut..]]),
                whole,
                "split at {cut} diverged for {stream:?}"
            );
        }
    }
}

#[test]
fn sgr_lone_truecolor_decodes_identically_for_all_separator_patterns() {
    // A lone 5-param truecolor pen (`38;2;r;g;b` and every colon/semicolon
    // separator mix) decodes to the same attribute: with exactly 5 params
    // `parse_ext_color` reads r/g/b from the same slots in both forms, and
    // the dedicated fast path in `parse_sgr_into` must match.
    for mask in 0u8..16 {
        let seps: Vec<bool> = (0..4).map(|k| mask & (1 << k) != 0).collect();
        for code in [38u16, 48] {
            let csi = Csi::new(&[code, 2, 10, 20, 30], &seps, &[], 0, b'm');
            let expected = if code == 38 {
                SgrAttr::Fg(Color::Rgb(Rgb::new(10, 20, 30)))
            } else {
                SgrAttr::Bg(Color::Rgb(Rgb::new(10, 20, 30)))
            };
            assert_eq!(parse_sgr(&csi), vec![expected], "mask={mask} code={code}");
        }
    }
}

// ── ground-state line batching (`Handler::print_ascii_lines`) ──────

/// The operation trace a line-batch capture handler records.
#[derive(Debug, PartialEq, Eq, Clone)]
enum LineOp {
    Print(String),
    Exec(u8),
    Batch(Vec<u8>),
    SgrBatch(Vec<u8>),
    Attrs(Vec<SgrAttr>),
    Csi(u8),
}

macro_rules! line_capture_noop_methods {
    () => {
        fn print(&mut self, c: char) {
            self.ops.push(LineOp::Print(c.to_string()));
        }
        fn print_str(&mut self, s: &str) {
            self.ops.push(LineOp::Print(s.to_owned()));
        }
        fn execute_c0(&mut self, byte: u8) {
            self.ops.push(LineOp::Exec(byte));
        }
        fn cursor_up(&mut self, _n: u16) {}
        fn cursor_down(&mut self, _n: u16) {}
        fn cursor_forward(&mut self, _n: u16) {}
        fn cursor_backward(&mut self, _n: u16) {}
        fn cursor_position(&mut self, _row: u16, _col: u16) {}
        fn cursor_col_abs(&mut self, _col: u16) {}
        fn cursor_row_abs(&mut self, _row: u16) {}
        fn erase_display(&mut self, _mode: crate::handler::EraseDisplay) {}
        fn erase_line(&mut self, _mode: crate::handler::EraseLine) {
            self.ops.push(LineOp::Csi(b'K'));
        }
        fn set_attributes(&mut self, attrs: &[SgrAttr]) {
            self.ops.push(LineOp::Attrs(attrs.to_vec()));
        }
        fn set_mode(&mut self, _value: u16, _ansi: bool, _on: bool) {}
        fn carriage_return(&mut self) {}
        fn linefeed(&mut self) {}
        fn tab(&mut self, _n: u16) {}
        fn reverse_index(&mut self) {}
        fn save_cursor(&mut self) {}
        fn restore_cursor(&mut self) {}
        fn full_reset(&mut self) {}
        fn device_attributes(&mut self, _kind: crate::handler::DaKind) {}
        fn device_status_report(&mut self, _kind: crate::handler::DsrKind) {}
    };
}

/// Captures the batch dispatches themselves (overrides `print_ascii_lines`
/// and `print_sgr_ascii_lines`).
#[derive(Default)]
struct BatchCapture {
    ops: Vec<LineOp>,
}

impl crate::Handler for BatchCapture {
    line_capture_noop_methods!();
    fn print_ascii_lines(&mut self, data: &[u8]) {
        self.ops.push(LineOp::Batch(data.to_vec()));
    }
    fn print_sgr_ascii_lines(&mut self, data: &[u8]) {
        self.ops.push(LineOp::SgrBatch(data.to_vec()));
    }
}

/// Relies on the default `print_ascii_lines` body (per-line replay).
#[derive(Default)]
struct DefaultBatchCapture {
    ops: Vec<LineOp>,
}

impl crate::Handler for DefaultBatchCapture {
    line_capture_noop_methods!();
}

fn batch_ops(bytes: &[u8]) -> Vec<LineOp> {
    let mut stream = crate::Stream::new();
    let mut cap = BatchCapture::default();
    stream.feed(bytes, &mut cap);
    cap.ops
}

#[test]
fn line_batch_dispatches_whole_ground_spans() {
    // The LF ending a text run plus at least one whole following line hands
    // the complete-line span to `print_ascii_lines` in one call.
    assert_eq!(
        batch_ops(b"abc\r\ndef\nghi\r\nx"),
        vec![
            LineOp::Print("abc".into()),
            LineOp::Batch(b"\r\ndef\nghi\r\n".to_vec()),
            LineOp::Print("x".into()),
        ]
    );
}

#[test]
fn line_batch_requires_two_complete_lines() {
    // A single terminator (interactive echo) stays on the per-action path.
    assert_eq!(
        batch_ops(b"abc\r\n"),
        vec![
            LineOp::Print("abc".into()),
            LineOp::Exec(0x0d),
            LineOp::Exec(0x0a),
        ]
    );
    assert_eq!(
        batch_ops(b"abc\ndef"),
        vec![
            LineOp::Print("abc".into()),
            LineOp::Exec(0x0a),
            LineOp::Print("def".into()),
        ]
    );
}

#[test]
fn line_batch_stops_before_bytes_the_regular_paths_own() {
    // A non-SGR escape ends the batch span before the line containing it.
    assert_eq!(
        batch_ops(b"\na\n\x1b[2Kb\r\nc\n"),
        vec![
            LineOp::Batch(b"\na\n".to_vec()),
            LineOp::Csi(b'K'),
            LineOp::Print("b".into()),
            LineOp::Batch(b"\r\nc\n".to_vec()),
        ]
    );
    // A CR not followed by LF is not a line terminator.
    assert_eq!(
        batch_ops(b"\na\nb\rc\n"),
        vec![
            LineOp::Batch(b"\na\n".to_vec()),
            LineOp::Print("b".into()),
            LineOp::Exec(0x0d),
            LineOp::Print("c".into()),
            LineOp::Exec(0x0a),
        ]
    );
    // Non-ASCII text stays on the UTF-8 print path.
    assert_eq!(
        batch_ops("\nab\nこ\n".as_bytes()),
        vec![
            LineOp::Batch(b"\nab\n".to_vec()),
            LineOp::Print("こ".into()),
            LineOp::Exec(0x0a),
        ]
    );
}

#[test]
fn sgr_line_batch_dispatches_edge_styled_spans() {
    // The tbench ansi shape: every line wrapped `ESC[32m … ESC[0m`. The
    // whole complete-line span (from the first line's terminator on) goes
    // to `print_sgr_ascii_lines` in one call.
    let body = b"\r\n\x1b[32mthe quick fox\x1b[0m\r\n\x1b[32mjumps over\x1b[0m\r\n";
    let mut fed = b"lead".to_vec();
    fed.extend_from_slice(b"\x1b[32mdog\x1b[0m");
    fed.extend_from_slice(body);
    let ops = batch_ops(&fed);
    assert_eq!(
        ops,
        vec![
            LineOp::Print("lead".into()),
            LineOp::Attrs(vec![SgrAttr::Fg(Color::Palette(2))]),
            LineOp::Print("dog".into()),
            LineOp::Attrs(vec![SgrAttr::Reset]),
            LineOp::SgrBatch(body.to_vec()),
        ]
    );
    // The staircase palette shape: multi-param lead SGR, `ESC[0m` tail —
    // still one styled span even as the palette rotates per line.
    let stair = b"\r\n\x1b[38;5;196;48;5;17;1mAAAA\x1b[0m\n\x1b[38;5;46;48;5;52;3mBBBB\x1b[0m\n";
    assert_eq!(
        batch_ops(&[b"x".as_slice(), stair.as_slice()].concat()),
        vec![LineOp::Print("x".into()), LineOp::SgrBatch(stair.to_vec()),]
    );
    // A plain-only span still dispatches through `print_ascii_lines`.
    assert_eq!(
        batch_ops(b"\na\nb\n"),
        vec![LineOp::Batch(b"\na\nb\n".to_vec())]
    );
}

#[test]
fn sgr_line_batch_rejects_non_edge_and_non_plain_sgr_lines() {
    // An SGR splitting a line's text ends the span before that line.
    assert_eq!(
        batch_ops(b"\na\nb\x1b[31mc\r\nd\ne\n"),
        vec![
            LineOp::Batch(b"\na\n".to_vec()),
            LineOp::Print("b".into()),
            LineOp::Attrs(vec![SgrAttr::Fg(Color::Palette(1))]),
            LineOp::Print("c".into()),
            LineOp::Batch(b"\r\nd\ne\n".to_vec()),
        ]
    );
    // XTMODKEYS (`CSI > … m`) is not a plain SGR: never batched.
    let ops = batch_ops(b"\na\n\x1b[>4;2mb\r\nc\nd\n");
    assert!(
        ops.contains(&LineOp::Batch(b"\na\n".to_vec()))
            && ops.contains(&LineOp::Batch(b"\r\nc\nd\n".to_vec())),
        "XTMODKEYS line must fall out of the batch: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, LineOp::SgrBatch(_))),
        "XTMODKEYS must not mark the span styled: {ops:?}"
    );
}

#[test]
fn sgr_line_batch_default_body_replays_units_in_order() {
    use crate::Handler as _;
    let mut cap = DefaultBatchCapture::default();
    cap.print_sgr_ascii_lines(b"\x1b[1m\x1b[31ma\x1b[0m\r\n\x1b[42mb\n");
    assert_eq!(
        cap.ops,
        vec![
            LineOp::Attrs(vec![SgrAttr::Bold]),
            LineOp::Attrs(vec![SgrAttr::Fg(Color::Palette(1))]),
            LineOp::Print("a".into()),
            LineOp::Attrs(vec![SgrAttr::Reset]),
            LineOp::Exec(0x0d),
            LineOp::Exec(0x0a),
            LineOp::Attrs(vec![SgrAttr::Bg(Color::Palette(2))]),
            LineOp::Print("b".into()),
            LineOp::Exec(0x0a),
        ]
    );
}

#[test]
fn sgr_ascii_lines_split_lead_text_tail() {
    use crate::handler::{SgrAsciiLine, SgrAsciiLines};
    let mut lines = SgrAsciiLines::new(b"\x1b[1m\x1b[31mab\x1b[0m\r\n\ncd\ntail");
    assert_eq!(
        lines.next(),
        Some(SgrAsciiLine {
            lead: b"\x1b[1m\x1b[31m",
            text: b"ab",
            tail: b"\x1b[0m",
            crlf: true
        })
    );
    assert_eq!(
        lines.next(),
        Some(SgrAsciiLine {
            lead: b"",
            text: b"",
            tail: b"",
            crlf: false
        })
    );
    assert_eq!(
        lines.next(),
        Some(SgrAsciiLine {
            lead: b"",
            text: b"cd",
            tail: b"",
            crlf: false
        })
    );
    assert_eq!(lines.next(), None);
    assert_eq!(lines.remainder(), b"tail");
}

#[test]
fn scan_plain_sgr_accepts_exactly_the_plain_sgr_shape() {
    use crate::sgr::scan_plain_sgr;
    assert_eq!(scan_plain_sgr(b"\x1b[m"), Some(3));
    assert_eq!(scan_plain_sgr(b"\x1b[0m"), Some(4));
    assert_eq!(scan_plain_sgr(b"\x1b[38;5;196;48;5;17;1mrest"), Some(21));
    assert_eq!(scan_plain_sgr(b"\x1b[4:3m"), Some(6));
    assert_eq!(scan_plain_sgr(b"\x1b[>4;2m"), None); // private marker
    assert_eq!(scan_plain_sgr(b"\x1b[?25h"), None); // private + wrong final
    assert_eq!(scan_plain_sgr(b"\x1b[2K"), None); // non-SGR final
    assert_eq!(scan_plain_sgr(b"\x1b[1 m"), None); // intermediate byte
    assert_eq!(scan_plain_sgr(b"\x1b[31"), None); // incomplete
    assert_eq!(scan_plain_sgr(b"\x1bM"), None); // not a CSI
    // Param-count cap matches the whole-CSI lexer: 32 params fit, 33 defer.
    let max = format!("\x1b[{}m", vec!["1"; 32].join(";"));
    assert_eq!(scan_plain_sgr(max.as_bytes()), Some(max.len()));
    let over = format!("\x1b[{}m", vec!["1"; 33].join(";"));
    assert_eq!(scan_plain_sgr(over.as_bytes()), None);
}

#[test]
fn parse_plain_sgr_unit_matches_the_csi_parse() {
    use crate::sgr::parse_plain_sgr_unit;
    let mut out = Vec::new();
    parse_plain_sgr_unit(b"\x1b[m", &mut out);
    assert_eq!(out, vec![SgrAttr::Reset]);
    parse_plain_sgr_unit(b"\x1b[1;38;5;196m", &mut out);
    assert_eq!(out, vec![SgrAttr::Bold, SgrAttr::Fg(Color::Palette(196))]);
    parse_plain_sgr_unit(b"\x1b[38;2;10;20;30m", &mut out);
    assert_eq!(out, vec![SgrAttr::Fg(Color::Rgb(Rgb::new(10, 20, 30)))]);
    parse_plain_sgr_unit(b"\x1b[4:3m", &mut out);
    assert_eq!(out, vec![SgrAttr::CurlyUnderline]);
    // Value saturation matches the DFA's accumulator.
    parse_plain_sgr_unit(b"\x1b[99999m", &mut out);
    assert_eq!(out, Vec::new());
}

#[test]
fn line_batch_default_body_replays_per_line() {
    use crate::Handler as _;
    let mut cap = DefaultBatchCapture::default();
    cap.print_ascii_lines(b"a\r\nb\n\n");
    assert_eq!(
        cap.ops,
        vec![
            LineOp::Print("a".into()),
            LineOp::Exec(0x0d),
            LineOp::Exec(0x0a),
            LineOp::Print("b".into()),
            LineOp::Exec(0x0a),
            LineOp::Exec(0x0a),
        ]
    );
}

#[test]
fn line_batch_marks_display_dirty() {
    let mut stream = crate::Stream::new();
    let mut cap = BatchCapture::default();
    stream.feed(b"\x1b[5n", &mut cap); // pure query: not dirty
    assert!(!stream.take_display_dirty());
    stream.feed(b"\nx\ny\n", &mut cap);
    assert!(stream.take_display_dirty());
    assert!(cap.ops.contains(&LineOp::Batch(b"\nx\ny\n".to_vec())));
}

#[test]
fn ascii_lines_iterates_complete_lines_and_exposes_the_remainder() {
    use crate::handler::{AsciiLine, AsciiLines};
    let mut lines = AsciiLines::new(b"a\r\n\nbc\nrest");
    assert_eq!(
        lines.next(),
        Some(AsciiLine {
            text: b"a",
            crlf: true
        })
    );
    assert_eq!(
        lines.next(),
        Some(AsciiLine {
            text: b"",
            crlf: false
        })
    );
    assert_eq!(
        lines.next(),
        Some(AsciiLine {
            text: b"bc",
            crlf: false
        })
    );
    assert_eq!(lines.next(), None);
    assert_eq!(lines.remainder(), b"rest");
}
