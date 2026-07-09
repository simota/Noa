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
        let mut i = 0;
        // Cached exclusive end of the printable run containing `i` (bytes in
        // `i..run_end` are all `is_run_byte`). Caching it across DFA detours
        // for invalid UTF-8 keeps the run scan linear even on hostile input.
        let mut run_end = 0;
        while i < bytes.len() {
            // Fast path: in plain ground state every byte until the next C0
            // control (ESC included) is print data, so the dominant
            // bulk-output case hands whole decoded runs to
            // `Handler::print_str` and skips the per-byte DFA dispatch
            // entirely (Ghostty analog: `stream.zig`'s ground scan).
            if is_run_byte(bytes[i]) && self.parser.in_ground_plain() {
                if run_end <= i {
                    run_end = bytes[i..]
                        .iter()
                        .position(|&b| !is_run_byte(b))
                        .map_or(bytes.len(), |off| i + off);
                }
                match core::str::from_utf8(&bytes[i..run_end]) {
                    Ok(text) => {
                        handler.print_str(text);
                        i = run_end;
                        continue;
                    }
                    Err(err) => {
                        // Bulk-print the valid prefix, then let the DFA own
                        // the invalid/incomplete sequence byte-by-byte below
                        // (it carries the replacement + cross-chunk resume
                        // semantics), re-entering this fast path once it
                        // returns to plain ground.
                        let valid = err.valid_up_to();
                        if valid > 0 {
                            let text = core::str::from_utf8(&bytes[i..i + valid])
                                .expect("valid_up_to marks a valid UTF-8 prefix");
                            handler.print_str(text);
                            i += valid;
                        }
                    }
                }
            }
            self.parser
                .advance(bytes[i], &mut |action| dispatch(action, handler));
            i += 1;
        }
    }
}

/// A byte that stays on the ground-state print path: anything but a C0
/// control or DEL. `0x80..=0xff` are UTF-8 sequence bytes (the parser never
/// treats raw C1 bytes as controls in ground).
#[inline]
fn is_run_byte(b: u8) -> bool {
    b >= 0x20 && b != 0x7f
}

fn dispatch<H: Handler>(action: Action, h: &mut H) {
    match action {
        Action::Print(c) => h.print(c),
        Action::Execute(b) => h.execute_c0(b),
        Action::CsiDispatch(csi) => dispatch_csi(&csi, h),
        Action::EscDispatch(esc) => dispatch_esc(&esc, h),
        Action::OscDispatch(data) => h.osc_dispatch(&data),
        Action::DcsDispatch(payload) => {
            if let Some(cmd) = crate::sixel::parse(&payload.data) {
                h.sixel_graphics(cmd);
            } else {
                h.dcs_dispatch(&payload.data);
            }
        }
        Action::ApcDispatch { data, truncated } => {
            // Only Kitty graphics (`G`) is captured; other APC strings are dropped.
            if let [b'G', rest @ ..] = data.as_slice() {
                h.kitty_graphics(crate::kitty_graphics::parse(rest, truncated));
            }
        }
    }
}

fn dispatch_csi<H: Handler>(csi: &Csi, h: &mut H) {
    let plain = csi.private == 0 && csi.intermediates().is_empty();
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
        b'm' if plain => h.set_attributes(&parse_sgr(csi)),
        // XTMODKEYS `CSI > Pp ; Pv m` sets an xterm key-modifier resource;
        // `CSI > Pp m` (and bare `CSI > m`) resets it. Only modifyOtherKeys
        // (Pp=4) is tracked. Must not fall through to SGR: `CSI > 4;2 m`
        // read as SGR is underline-on + faint, sticking underline on every
        // cell printed afterwards.
        b'm' if csi.private == b'>' => {
            if csi.params().is_empty() || csi.param(0, 0) == 4 {
                h.set_modify_other_keys(csi.param(1, 0));
            }
        }
        b'p' if csi.intermediates() == [b'$'] => {
            h.request_mode(ModeRequest {
                value: csi.param(0, 0),
                ansi: csi.private != b'?',
            });
        }
        b'p' if csi.intermediates() == [b'!'] => h.soft_reset(), // DECSTR
        b'q' if csi.private == 0 && csi.intermediates() == [b' '] => {
            // `param` collapses an explicit `0` to its default, but DECSCUSR 0
            // must reset to the configured default, so match the raw param:
            // an explicit `0` is `Default`, an absent param is blinking block.
            let style = match csi.params().first().copied() {
                Some(0) => CursorStyle::Default,
                Some(3) => CursorStyle::BlinkingUnderline,
                Some(4) => CursorStyle::SteadyUnderline,
                Some(5) => CursorStyle::BlinkingBar,
                Some(6) => CursorStyle::SteadyBar,
                Some(2) => CursorStyle::SteadyBlock,
                _ => CursorStyle::BlinkingBlock,
            };
            h.set_cursor_style(style);
        }
        b'q' if csi.private == b'>' => h.xtversion_query(),
        b'h' | b'l' => {
            let on = csi.final_byte == b'h';
            let ansi = csi.private != b'?';
            for &value in csi.params() {
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
        b's' if csi.private == 0 && csi.params().is_empty() => h.save_cursor(),
        b's' if csi.private == 0 => h.set_horizontal_margins(csi.param(0, 1), csi.param(1, 0)),
        // Plain `CSI u` is SCORC (restore cursor). The private markers select
        // the Kitty keyboard protocol progressive-enhancement operations.
        b'u' if csi.private == 0 && csi.intermediates().is_empty() => h.restore_cursor(),
        b'u' if csi.private == b'?' => h.kitty_keyboard_query(),
        b'u' if csi.private == b'>' => h.kitty_keyboard_push(csi.param(0, 0) as u8),
        b'u' if csi.private == b'<' => h.kitty_keyboard_pop(csi.param(0, 1)),
        b'u' if csi.private == b'=' => {
            h.kitty_keyboard_set(csi.param(0, 0) as u8, csi.param(1, 1));
        }
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
    match esc.intermediates() {
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
