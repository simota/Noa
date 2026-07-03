//! [`Stream`] — feeds bytes through a [`Parser`] and maps each [`Action`] onto
//! a [`Handler`]. This is the semantic layer: it knows what `CSI … H` *means*.

use crate::action::Action;
use crate::csi::{Csi, Esc};
use crate::handler::{
    Charset, CharsetSlot, CursorStyle, DaKind, DsrKind, EraseDisplay, EraseLine, Handler,
    ModeRequest,
};
use crate::parser::Parser;
use crate::sgr::parse_sgr;

/// Owns a [`Parser`] and drives a [`Handler`] from a byte stream.
#[derive(Default)]
pub struct Stream {
    parser: Parser,
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes, dispatching all resulting operations to `handler`.
    pub fn feed<H: Handler>(&mut self, bytes: &[u8], handler: &mut H) {
        for &b in bytes {
            self.parser
                .advance(b, &mut |action| dispatch(action, handler));
        }
    }
}

fn dispatch<H: Handler>(action: Action, h: &mut H) {
    match action {
        Action::Print(c) => h.print(c),
        Action::Execute(b) => h.execute_c0(b),
        Action::CsiDispatch(csi) => dispatch_csi(&csi, h),
        Action::EscDispatch(esc) => dispatch_esc(&esc, h),
        Action::OscDispatch(data) => h.osc_dispatch(&data),
        Action::DcsDispatch(payload) => h.dcs_dispatch(&payload.data),
    }
}

fn dispatch_csi<H: Handler>(csi: &Csi, h: &mut H) {
    let plain = csi.private == 0 && csi.intermediates.is_empty();
    match csi.final_byte {
        b'@' if plain => h.insert_blank_chars(csi.param(0, 1)),
        b'A' => h.cursor_up(csi.param(0, 1)),
        b'B' | b'e' => h.cursor_down(csi.param(0, 1)),
        b'C' | b'a' => h.cursor_forward(csi.param(0, 1)),
        b'D' => h.cursor_backward(csi.param(0, 1)),
        b'E' => h.cursor_next_line(csi.param(0, 1)),
        b'F' => h.cursor_prev_line(csi.param(0, 1)),
        b'G' | b'`' => h.cursor_col_abs(csi.param(0, 1)),
        b'I' if plain => h.tab(csi.param(0, 1)),
        b'L' if plain => h.insert_lines(csi.param(0, 1)),
        b'M' if plain => h.delete_lines(csi.param(0, 1)),
        b'P' if plain => h.delete_chars(csi.param(0, 1)),
        b'S' if plain => h.scroll_up(csi.param(0, 1)),
        b'T' if plain => h.scroll_down(csi.param(0, 1)),
        b'X' if plain => h.erase_chars(csi.param(0, 1)),
        b'Z' if plain => h.tab_back(csi.param(0, 1)),
        b'b' if plain => h.repeat_preceding_char(csi.param(0, 1)),
        b'd' => h.cursor_row_abs(csi.param(0, 1)),
        b'H' | b'f' => h.cursor_position(csi.param(0, 1), csi.param(1, 1)),
        b'J' => h.erase_display(match csi.param(0, 0) {
            1 => EraseDisplay::Above,
            2 => EraseDisplay::Complete,
            3 => EraseDisplay::Scrollback,
            _ => EraseDisplay::Below,
        }),
        b'K' => h.erase_line(match csi.param(0, 0) {
            1 => EraseLine::Left,
            2 => EraseLine::Complete,
            _ => EraseLine::Right,
        }),
        b'm' => h.set_attributes(&parse_sgr(csi)),
        b'p' if csi.intermediates.as_slice() == [b'$'] => {
            h.request_mode(ModeRequest {
                value: csi.param(0, 0),
                ansi: csi.private != b'?',
            });
        }
        b'p' if csi.intermediates.as_slice() == [b'!'] => h.soft_reset(), // DECSTR
        b'q' if csi.private == 0 && csi.intermediates.as_slice() == [b' '] => {
            let style = match csi.param(0, 1) {
                3 => CursorStyle::BlinkingUnderline,
                4 => CursorStyle::SteadyUnderline,
                5 => CursorStyle::BlinkingBar,
                6 => CursorStyle::SteadyBar,
                2 => CursorStyle::SteadyBlock,
                _ => CursorStyle::BlinkingBlock,
            };
            h.set_cursor_style(style);
        }
        b'h' | b'l' => {
            let on = csi.final_byte == b'h';
            let ansi = csi.private != b'?';
            for &value in &csi.params {
                h.set_mode(value, ansi, on);
            }
        }
        b'c' => match csi.private {
            0 => h.device_attributes(DaKind::Primary),
            b'>' => h.device_attributes(DaKind::Secondary),
            _ => {}
        },
        b'n' => match csi.param(0, 0) {
            5 => h.device_status_report(DsrKind::Status),
            6 => h.device_status_report(DsrKind::CursorPosition),
            _ => {}
        },
        b'r' if csi.private == 0 => h.set_scroll_region(csi.param(0, 1), csi.param(1, 0)),
        b's' if csi.private == 0 && csi.params.is_empty() => h.save_cursor(),
        b's' if csi.private == 0 => h.set_horizontal_margins(csi.param(0, 1), csi.param(1, 0)),
        b'u' if csi.private == 0 => h.restore_cursor(),
        b'g' if plain => match csi.param(0, 0) {
            0 => h.clear_tab_stop(),
            3 => h.clear_all_tab_stops(),
            _ => {}
        },
        b't' if plain => h.window_op(csi.param(0, 0), csi.param(1, 0), csi.param(2, 0)), // XTWINOPS
        _ => {} // unknown / inc>=2
    }
}

fn dispatch_esc<H: Handler>(esc: &Esc, h: &mut H) {
    match esc.intermediates.as_slice() {
        [] => match esc.final_byte {
            b'c' => h.full_reset(),                  // RIS
            b'7' => h.save_cursor(),                 // DECSC
            b'8' => h.restore_cursor(),              // DECRC
            b'=' => h.set_application_keypad(true),  // DECPAM
            b'>' => h.set_application_keypad(false), // DECPNM
            b'M' => h.reverse_index(),               // RI
            b'D' => h.linefeed(),                    // IND (index, no CR)
            b'E' => {
                // NEL
                h.carriage_return();
                h.linefeed();
            }
            b'H' => h.set_tab_stop(), // HTS
            _ => {}
        },
        // SCS: `ESC ( x` designates G0, `ESC ) x` designates G1.
        [b'('] => h.designate_charset(CharsetSlot::G0, charset_from(esc.final_byte)),
        [b')'] => h.designate_charset(CharsetSlot::G1, charset_from(esc.final_byte)),
        [b'#'] if esc.final_byte == b'8' => h.screen_alignment_test(), // DECALN
        _ => {} // DECDHL/DECSWL etc. — no-op (out of scope)
    }
}

/// Map an `SCS` final byte to the [`Charset`] it designates. Lite scope only
/// distinguishes ASCII vs DEC Special Graphics; every other final byte (UK,
/// …) falls back to ASCII.
fn charset_from(final_byte: u8) -> Charset {
    match final_byte {
        b'0' => Charset::DecSpecialGraphics,
        _ => Charset::Ascii,
    }
}
