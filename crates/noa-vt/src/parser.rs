//! The byte-driven DEC ANSI parser DFA.
//!
//! Ported from the semantics of Ghostty's `terminal/Parser.zig` +
//! `parse_table.zig` (itself the vt100.net / Paul Williams state machine).
//! `advance` is fed one byte at a time and emits zero or more [`Action`]s via
//! a sink closure (a byte can produce an exit action + a transition action).
//!
//! UTF-8 scalars for the `Print` path are decoded inline in the ground state
//! (Ghostty keeps a dedicated UTF-8 fast path in `stream.zig`); escape/CSI/OSC
//! control bytes are 7-bit ASCII.

use crate::action::Action;
use crate::csi::{Csi, Esc, MAX_INTERMEDIATES, MAX_PARAMS};
use crate::state::State;

/// The from-scratch VT parser. Cheap to construct; holds only small buffers.
pub struct Parser {
    state: State,
    params: Vec<u16>,
    sep_colon: Vec<bool>,
    intermediates: Vec<u8>,
    private: u8,
    osc: Vec<u8>,
    utf8_acc: u32,
    utf8_rem: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Vec::new(),
            sep_colon: Vec::new(),
            intermediates: Vec::new(),
            private: 0,
            osc: Vec::new(),
            utf8_acc: 0,
            utf8_rem: 0,
        }
    }

    /// The current DFA state (primarily for tests / introspection).
    pub fn state(&self) -> State {
        self.state
    }

    /// Feed one byte, emitting any resulting actions through `sink`.
    pub fn advance<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        // (1) A UTF-8 multibyte sequence in progress (ground path only).
        if self.utf8_rem > 0 {
            if (0x80..=0xbf).contains(&b) {
                self.utf8_acc = (self.utf8_acc << 6) | ((b & 0x3f) as u32);
                self.utf8_rem -= 1;
                if self.utf8_rem == 0 {
                    let ch = char::from_u32(self.utf8_acc).unwrap_or('\u{FFFD}');
                    sink(Action::Print(ch));
                }
                return;
            }
            // Invalid continuation: emit U+FFFD, then reprocess `b` normally.
            self.utf8_rem = 0;
            sink(Action::Print('\u{FFFD}'));
        }

        // (2) "Anywhere" transitions.
        match b {
            0x18 | 0x1a => {
                sink(Action::Execute(b));
                self.state = State::Ground;
                return;
            }
            0x1b => {
                self.goto(State::Escape, sink);
                return;
            }
            _ => {}
        }

        // (3) Per-state handling.
        match self.state {
            State::Ground => self.st_ground(b, sink),
            State::Escape => self.st_escape(b, sink),
            State::EscapeIntermediate => self.st_escape_intermediate(b, sink),
            State::CsiEntry => self.st_csi_entry(b, sink),
            State::CsiParam => self.st_csi_param(b, sink),
            State::CsiIntermediate => self.st_csi_intermediate(b, sink),
            State::CsiIgnore => self.st_csi_ignore(b, sink),
            // Inc-1: DCS carries no side effects — consume until ST/ESC.
            // TODO(agent): DCS passthrough (XTGETTCAP / DECRQSS) — inc-5.
            State::DcsPassthrough => {}
            State::OscString => self.st_osc(b, sink),
            // APC / SOS / PM: consume until ST/ESC.
            State::SosPmApcString => {}
        }
    }

    // ── state handlers ─────────────────────────────────────────────

    fn st_ground<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x7e => sink(Action::Print(b as char)),
            0x7f => {} // DEL ignored in ground
            _ => self.utf8_begin(b, sink), // 0x80..=0xff
        }
    }

    fn utf8_begin<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        let (len, init): (u8, u32) = if (0xf0..=0xf7).contains(&b) {
            (4, (b & 0x07) as u32)
        } else if (0xe0..=0xef).contains(&b) {
            (3, (b & 0x0f) as u32)
        } else if (0xc2..=0xdf).contains(&b) {
            (2, (b & 0x1f) as u32)
        } else {
            // 0x80..=0xc1 (stray continuation / overlong lead), 0xf8..=0xff
            sink(Action::Print('\u{FFFD}'));
            return;
        };
        self.utf8_acc = init;
        self.utf8_rem = len - 1;
    }

    fn st_escape<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::EscapeIntermediate;
            }
            0x50 => self.state = State::DcsPassthrough, // 'P' DCS
            0x58 | 0x5e | 0x5f => self.state = State::SosPmApcString, // X ^ _
            0x5b => self.goto(State::CsiEntry, sink),   // '[' CSI
            0x5d => self.goto(State::OscString, sink),  // ']' OSC
            0x30..=0x4f | 0x51..=0x57 | 0x59 | 0x5a | 0x5c | 0x60..=0x7e => {
                self.esc_dispatch(b, sink);
                self.state = State::Ground;
            }
            _ => {} // 0x7f
        }
    }

    fn st_escape_intermediate<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x2f => self.collect(b),
            0x30..=0x7e => {
                self.esc_dispatch(b, sink);
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    fn st_csi_entry<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => {
                self.param_digit(b);
                self.state = State::CsiParam;
            }
            0x3a => {
                self.param_sep(true);
                self.state = State::CsiParam;
            }
            0x3b => {
                self.param_sep(false);
                self.state = State::CsiParam;
            }
            0x3c..=0x3f => {
                self.private = b;
                self.state = State::CsiParam;
            }
            0x40..=0x7e => {
                self.csi_dispatch(b, sink);
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    fn st_csi_param<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x30..=0x39 => self.param_digit(b),
            0x3a => self.param_sep(true),
            0x3b => self.param_sep(false),
            0x3c..=0x3f => self.state = State::CsiIgnore,
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => {
                self.csi_dispatch(b, sink);
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    fn st_csi_intermediate<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x2f => self.collect(b),
            0x30..=0x3f => self.state = State::CsiIgnore,
            0x40..=0x7e => {
                self.csi_dispatch(b, sink);
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    fn st_csi_ignore<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x40..=0x7e => self.state = State::Ground,
            _ => {}
        }
    }

    fn st_osc<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x07 => self.goto(State::Ground, sink), // BEL terminates OSC
            0x20..=0x7e | 0x80..=0xff => self.osc.push(b),
            _ => {} // other C0 + DEL ignored
        }
    }

    // ── primitive actions ──────────────────────────────────────────

    fn collect(&mut self, b: u8) {
        if self.intermediates.len() < MAX_INTERMEDIATES {
            self.intermediates.push(b);
        }
    }

    fn param_digit(&mut self, b: u8) {
        let d = (b - 0x30) as u16;
        if self.params.is_empty() {
            self.params.push(0);
        }
        if let Some(last) = self.params.last_mut() {
            *last = last.saturating_mul(10).saturating_add(d);
        }
    }

    fn param_sep(&mut self, colon: bool) {
        if self.params.is_empty() {
            self.params.push(0);
        }
        if self.params.len() < MAX_PARAMS {
            self.params.push(0);
            self.sep_colon.push(colon);
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.sep_colon.clear();
        self.intermediates.clear();
        self.private = 0;
    }

    fn csi_dispatch<F: FnMut(Action)>(&mut self, final_byte: u8, sink: &mut F) {
        sink(Action::CsiDispatch(Csi {
            params: self.params.clone(),
            sep_colon: self.sep_colon.clone(),
            intermediates: self.intermediates.clone(),
            private: self.private,
            final_byte,
        }));
    }

    fn esc_dispatch<F: FnMut(Action)>(&mut self, final_byte: u8, sink: &mut F) {
        sink(Action::EscDispatch(Esc {
            intermediates: self.intermediates.clone(),
            final_byte,
        }));
    }

    /// Transition to `new`, running the source state's exit action and the
    /// target state's entry action (OSC end/start, `clear` on escape/CSI entry).
    fn goto<F: FnMut(Action)>(&mut self, new: State, sink: &mut F) {
        if self.state == State::OscString {
            let data = std::mem::take(&mut self.osc);
            sink(Action::OscDispatch(data));
        }
        self.state = new;
        match new {
            State::Escape | State::CsiEntry => self.clear(),
            State::OscString => self.osc.clear(),
            _ => {}
        }
    }
}
