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
use crate::csi::{Csi, DcsPayload, Esc, MAX_INTERMEDIATES, MAX_PARAMS};
use crate::state::State;

const MAX_OSC_BYTES: usize = 4096;
const MAX_DCS_BYTES: usize = 4096;
/// APC payloads carry whole Kitty-graphics transfers, so the cap is generous
/// (spec-compliant clients chunk at ≤4096B, but non-conforming tools send in
/// one shot). Overflow truncates rather than discards — see [`Action::ApcDispatch`].
const MAX_APC_BYTES: usize = 1 << 20; // 1 MiB

/// The from-scratch VT parser. Cheap to construct; holds only small buffers.
pub struct Parser {
    state: State,
    params: Vec<u16>,
    sep_colon: Vec<bool>,
    intermediates: Vec<u8>,
    private: u8,
    osc: Vec<u8>,
    osc_overflow: bool,
    dcs: Vec<u8>,
    dcs_overflow: bool,
    apc: Vec<u8>,
    apc_overflow: bool,
    utf8_acc: u32,
    utf8_rem: u8,
    utf8_min: u32,
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
            osc_overflow: false,
            dcs: Vec::new(),
            dcs_overflow: false,
            apc: Vec::new(),
            apc_overflow: false,
            utf8_acc: 0,
            utf8_rem: 0,
            utf8_min: 0,
        }
    }

    /// The current DFA state (primarily for tests / introspection).
    pub fn state(&self) -> State {
        self.state
    }

    /// True when the next byte takes the plain ground path (no UTF-8
    /// continuation pending), i.e. a printable-ASCII byte maps 1:1 onto
    /// `Action::Print`. Lets `Stream::feed` batch runs of plain text past
    /// the per-byte DFA dispatch.
    pub fn in_ground_plain(&self) -> bool {
        self.state == State::Ground && self.utf8_rem == 0
    }

    /// Feed one byte, emitting any resulting actions through `sink`.
    pub fn advance<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        if self.state == State::DcsEscape {
            self.st_dcs_escape(b, sink);
            return;
        }
        if self.state == State::DcsPassthrough && b == 0x1b {
            self.state = State::DcsEscape;
            return;
        }
        if self.state == State::ApcEscape {
            self.st_apc_escape(b, sink);
            return;
        }
        if self.state == State::ApcString && b == 0x1b {
            self.state = State::ApcEscape;
            return;
        }

        // (1) A UTF-8 multibyte sequence in progress (ground path only).
        if self.utf8_rem > 0 {
            if (0x80..=0xbf).contains(&b) {
                self.utf8_acc = (self.utf8_acc << 6) | ((b & 0x3f) as u32);
                self.utf8_rem -= 1;
                if self.utf8_rem == 0 {
                    let ch = if self.utf8_acc >= self.utf8_min {
                        char::from_u32(self.utf8_acc).unwrap_or('\u{FFFD}')
                    } else {
                        '\u{FFFD}'
                    };
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
            State::DcsPassthrough => self.st_dcs(b, sink),
            State::DcsEscape => unreachable!("handled before anywhere transitions"),
            State::OscString => self.st_osc(b, sink),
            State::ApcString => self.st_apc(b, sink),
            State::ApcEscape => unreachable!("handled before anywhere transitions"),
            // SOS / PM string payloads are ignored by the state model.
            State::SosPmApcString => {}
        }
    }

    // ── state handlers ─────────────────────────────────────────────

    fn st_ground<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x7e => sink(Action::Print(b as char)),
            0x7f => {}                     // DEL ignored in ground
            _ => self.utf8_begin(b, sink), // 0x80..=0xff
        }
    }

    fn utf8_begin<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        let (len, init, min): (u8, u32, u32) = if (0xf0..=0xf4).contains(&b) {
            (4, (b & 0x07) as u32, 0x10000)
        } else if (0xe0..=0xef).contains(&b) {
            (3, (b & 0x0f) as u32, 0x800)
        } else if (0xc2..=0xdf).contains(&b) {
            (2, (b & 0x1f) as u32, 0x80)
        } else {
            // 0x80..=0xc1 (stray continuation / overlong lead), 0xf5..=0xff
            sink(Action::Print('\u{FFFD}'));
            return;
        };
        self.utf8_acc = init;
        self.utf8_rem = len - 1;
        self.utf8_min = min;
    }

    fn st_escape<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => sink(Action::Execute(b)),
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::EscapeIntermediate;
            }
            0x50 => self.goto(State::DcsPassthrough, sink), // 'P' DCS
            0x58 | 0x5e => self.state = State::SosPmApcString, // 'X' SOS / '^' PM (discarded)
            0x5f => self.goto(State::ApcString, sink),      // '_' APC (captured)
            0x5b => self.goto(State::CsiEntry, sink),       // '[' CSI
            0x5d => self.goto(State::OscString, sink),      // ']' OSC
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
            0x20..=0x7e | 0x80..=0xff => {
                if self.osc_overflow {
                    return;
                }
                if self.osc.len() < MAX_OSC_BYTES {
                    self.osc.push(b);
                } else {
                    self.osc.clear();
                    self.osc_overflow = true;
                }
            }
            _ => {} // other C0 + DEL ignored
        }
    }

    fn st_dcs<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x9c => self.finish_dcs(sink),
            0x20..=0x7e | 0x80..=0xff => {
                if self.dcs_overflow {
                    return;
                }
                if self.dcs.len() < MAX_DCS_BYTES {
                    self.dcs.push(b);
                } else {
                    self.dcs.clear();
                    self.dcs_overflow = true;
                }
            }
            _ => {}
        }
    }

    fn st_dcs_escape<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        if b == b'\\' {
            self.finish_dcs(sink);
        } else {
            self.dcs.clear();
            self.dcs_overflow = false;
            self.goto(State::Escape, sink);
            self.advance(b, sink);
        }
    }

    fn st_apc<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x9c => self.finish_apc(sink), // 8-bit ST
            0x20..=0x7e | 0x80..=0xff => {
                // Unlike OSC/DCS, overflow keeps the captured prefix and marks
                // it truncated so the dispatch still fires (Kitty must reply).
                if !self.apc_overflow {
                    if self.apc.len() < MAX_APC_BYTES {
                        self.apc.push(b);
                    } else {
                        self.apc_overflow = true;
                    }
                }
            }
            _ => {} // other C0 + DEL ignored
        }
    }

    fn st_apc_escape<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        if b == b'\\' {
            self.finish_apc(sink);
        } else {
            // Not an ST: abandon the APC and reprocess the byte from Escape.
            self.apc.clear();
            self.apc_overflow = false;
            self.goto(State::Escape, sink);
            self.advance(b, sink);
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

    fn finish_dcs<F: FnMut(Action)>(&mut self, sink: &mut F) {
        if !self.dcs_overflow {
            let data = std::mem::take(&mut self.dcs);
            sink(Action::DcsDispatch(DcsPayload { data }));
        } else {
            self.dcs.clear();
            self.dcs_overflow = false;
        }
        self.state = State::Ground;
    }

    fn finish_apc<F: FnMut(Action)>(&mut self, sink: &mut F) {
        let data = std::mem::take(&mut self.apc);
        let truncated = self.apc_overflow;
        self.apc_overflow = false;
        sink(Action::ApcDispatch { data, truncated });
        self.state = State::Ground;
    }

    /// Transition to `new`, running the source state's exit action and the
    /// target state's entry action (OSC end/start, `clear` on escape/CSI entry).
    fn goto<F: FnMut(Action)>(&mut self, new: State, sink: &mut F) {
        if self.state == State::OscString {
            if !self.osc_overflow {
                let data = std::mem::take(&mut self.osc);
                sink(Action::OscDispatch(data));
            } else {
                self.osc.clear();
                self.osc_overflow = false;
            }
        }
        self.state = new;
        match new {
            State::Escape | State::CsiEntry => self.clear(),
            State::OscString => {
                self.osc.clear();
                self.osc_overflow = false;
            }
            State::DcsPassthrough => {
                self.dcs.clear();
                self.dcs_overflow = false;
            }
            State::ApcString => {
                self.apc.clear();
                self.apc_overflow = false;
            }
            _ => {}
        }
    }
}
