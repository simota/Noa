//! [`Stream`] — feeds bytes through a [`Parser`] and maps each [`Action`] onto
//! a [`Handler`]. This is the semantic layer: it knows what `CSI … H` *means*.

use crate::action::Action;
use crate::csi::{Csi, Esc};
use crate::handler::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler};
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
            self.parser.advance(b, &mut |action| dispatch(action, handler));
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
    }
}

fn dispatch_csi<H: Handler>(csi: &Csi, h: &mut H) {
    match csi.final_byte {
        b'A' => h.cursor_up(csi.param(0, 1)),
        b'B' | b'e' => h.cursor_down(csi.param(0, 1)),
        b'C' | b'a' => h.cursor_forward(csi.param(0, 1)),
        b'D' => h.cursor_backward(csi.param(0, 1)),
        b'E' => h.cursor_next_line(csi.param(0, 1)),
        b'F' => h.cursor_prev_line(csi.param(0, 1)),
        b'G' | b'`' => h.cursor_col_abs(csi.param(0, 1)),
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
        b's' if csi.private == 0 => h.save_cursor(),
        b'u' if csi.private == 0 => h.restore_cursor(),
        _ => {} // unknown / inc>=2
    }
}

fn dispatch_esc<H: Handler>(esc: &Esc, h: &mut H) {
    if !esc.intermediates.is_empty() {
        // Charset designation (`ESC ( B`, …) etc. — no-op in inc-1 (single ASCII charset).
        return;
    }
    match esc.final_byte {
        b'c' => h.full_reset(),      // RIS
        b'7' => h.save_cursor(),     // DECSC
        b'8' => h.restore_cursor(),  // DECRC
        b'M' => h.reverse_index(),   // RI
        b'D' => h.linefeed(),        // IND (index, no CR)
        b'E' => {
            // NEL
            h.carriage_return();
            h.linefeed();
        }
        b'H' => h.set_tab_stop(), // HTS
        _ => {}
    }
}
