//! Parser + SGR conformance tests (byte-sequence → action assertions),
//! ported from the semantics of Ghostty's `Parser.zig` unit tests.

use crate::action::Action;
use crate::csi::Csi;
use crate::parser::Parser;
use crate::sgr::{SgrAttr, parse_sgr};
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
    assert_eq!(csi.params, vec![31]);
    assert_eq!(csi.final_byte, b'm');
    assert!(csi.intermediates.is_empty());
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
fn cup_two_params() {
    let csi = only_csi(b"\x1b[5;10H");
    assert_eq!(csi.params, vec![5, 10]);
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
    assert_eq!(csi.params, vec![25]);
    assert_eq!(csi.final_byte, b'h');
}

#[test]
fn decscusr_has_space_intermediate() {
    let csi = only_csi(b"\x1b[4 q");

    assert_eq!(csi.params, vec![4]);
    assert_eq!(csi.intermediates, vec![b' ']);
    assert_eq!(csi.final_byte, b'q');
}

#[test]
fn decslrm_uses_plain_s_final() {
    let csi = only_csi(b"\x1b[3;7s");

    assert_eq!(csi.params, vec![3, 7]);
    assert!(csi.intermediates.is_empty());
    assert_eq!(csi.private, 0);
    assert_eq!(csi.final_byte, b's');
}

#[test]
fn keypad_modes_parse_as_esc_dispatch() {
    assert_eq!(
        actions(b"\x1b=\x1b>"),
        vec![
            Action::EscDispatch(crate::csi::Esc {
                intermediates: Vec::new(),
                final_byte: b'=',
            }),
            Action::EscDispatch(crate::csi::Esc {
                intermediates: Vec::new(),
                final_byte: b'>',
            }),
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
    bytes.extend(std::iter::repeat_n(b'a', 4097));
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
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 4097));
    bytes.push(0x07);

    let acts = actions(&bytes);

    assert!(
        !acts
            .iter()
            .any(|action| matches!(action, Action::OscDispatch(_)))
    );
}

#[test]
fn esc_dispatch_ris_and_index() {
    assert_eq!(
        actions(b"\x1bc"),
        vec![Action::EscDispatch(crate::csi::Esc {
            intermediates: vec![],
            final_byte: b'c'
        })]
    );
}

#[test]
fn charset_designation_has_intermediate() {
    // ESC ( B  → EscDispatch with intermediate '('
    let acts = actions(b"\x1b(B");
    assert_eq!(
        acts,
        vec![Action::EscDispatch(crate::csi::Esc {
            intermediates: vec![b'('],
            final_byte: b'B'
        })]
    );
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
