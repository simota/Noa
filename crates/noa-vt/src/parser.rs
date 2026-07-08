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
use crate::csi::{Csi, DcsPayload, Esc, Intermediates, Params, Separators, MAX_PARAMS};
use crate::state::State;

/// OSC payloads include OSC 52 clipboard writes, whose base64 must carry the
/// grid's 8 MiB decoded clipboard cap (~10.7 MiB encoded) plus target/params.
/// The buffer grows on demand; the cap only bounds a runaway unterminated OSC.
const MAX_OSC_BYTES: usize = 12 * (1 << 20); // 12 MiB
const MAX_DCS_BYTES: usize = 4096;
/// APC payloads carry whole Kitty-graphics transfers, so the cap is generous
/// (spec-compliant clients chunk at ≤4096B, but non-conforming tools send in
/// one shot). Overflow truncates rather than discards — see [`Action::ApcDispatch`].
const MAX_APC_BYTES: usize = 1 << 20; // 1 MiB

/// The from-scratch VT parser. Cheap to construct; holds only small buffers.
pub struct Parser {
    state: State,
    params: Params,
    sep_colon: Separators,
    intermediates: Intermediates,
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
    /// UTF-8 continuation bytes still expected inside a string payload
    /// (OSC/DCS/APC/SOS/PM). While nonzero, byte `0x9c` is payload data —
    /// a continuation byte (e.g. the third byte of `作`/`検`) — not 8-bit ST.
    /// Without this, a Japanese OSC title terminates mid-scalar and its tail
    /// prints into the grid at the cursor.
    string_utf8_rem: u8,
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
            params: Params::default(),
            sep_colon: Separators::default(),
            intermediates: Intermediates::default(),
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
            string_utf8_rem: 0,
        }
    }

    /// The current DFA state (primarily for tests / introspection).
    pub fn state(&self) -> State {
        self.state
    }

    /// Test-only: heap capacity currently held by the OSC accumulation buffer.
    #[cfg(test)]
    pub(crate) fn osc_buffer_capacity(&self) -> usize {
        self.osc.capacity()
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

        // (2) Recognized C1 controls. Some programs still emit 8-bit
        // CSI/OSC/DCS/APC forms; treating those bytes as malformed UTF-8
        // leaves the rest of the control sequence printed as garbage.
        if self.c1_control(b, sink) {
            return;
        }

        // (3) "Anywhere" transitions.
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

        // (4) Per-state handling.
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
            // SOS / PM string payloads are ignored by the state model, but
            // UTF-8 tracking must still run so a payload continuation 0x9c
            // is not mistaken for the terminating 8-bit ST.
            State::SosPmApcString => self.track_string_utf8(b),
        }
    }

    /// Track UTF-8 sequence progress within a string payload (OSC/DCS/APC/
    /// SOS/PM): while `string_utf8_rem > 0` the next 0x80..=0xbf bytes are
    /// continuations of an in-flight scalar, so 0x9c among them is data, not
    /// 8-bit ST. Payloads are accumulated as raw bytes; this only counts.
    fn track_string_utf8(&mut self, b: u8) {
        if self.string_utf8_rem > 0 && (0x80..=0xbf).contains(&b) {
            self.string_utf8_rem -= 1;
            return;
        }
        self.string_utf8_rem = match b {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        };
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
            0x58 | 0x5e => {
                // 'X' SOS / '^' PM (discarded)
                self.state = State::SosPmApcString;
                self.string_utf8_rem = 0;
            }
            0x5f => self.goto(State::ApcString, sink), // '_' APC (captured)
            0x5b => self.goto(State::CsiEntry, sink),  // '[' CSI
            0x5d => self.goto(State::OscString, sink), // ']' OSC
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
            // 8-bit ST — only outside a payload UTF-8 sequence, where 0x9c
            // is a continuation byte.
            0x9c if self.string_utf8_rem == 0 => self.goto(State::Ground, sink),
            0x20..=0x7e | 0x80..=0xff => {
                self.track_string_utf8(b);
                if self.osc_overflow {
                    return;
                }
                if self.osc.len() < MAX_OSC_BYTES {
                    self.osc.push(b);
                } else {
                    // Free, don't clear: the buffer is at its 12 MiB cap and
                    // nothing accumulates until the runaway OSC terminates, so
                    // clearing would pin the capacity for the parser's life.
                    self.osc = Vec::new();
                    self.osc_overflow = true;
                }
            }
            _ => {} // other C0 + DEL ignored
        }
    }

    fn st_dcs<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) {
        match b {
            0x9c if self.string_utf8_rem == 0 => self.finish_dcs(sink),
            0x20..=0x7e | 0x80..=0xff => {
                self.track_string_utf8(b);
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
            0x9c if self.string_utf8_rem == 0 => self.finish_apc(sink), // 8-bit ST
            0x20..=0x7e | 0x80..=0xff => {
                self.track_string_utf8(b);
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

    fn c1_control<F: FnMut(Action)>(&mut self, b: u8, sink: &mut F) -> bool {
        if !(0x80..=0x9f).contains(&b) {
            return false;
        }
        match self.state {
            State::DcsPassthrough | State::ApcString => return false,
            // 0x9c inside a payload UTF-8 sequence is a continuation byte,
            // not 8-bit ST — fall through to the string accumulator.
            State::OscString if b != 0x9c || self.string_utf8_rem > 0 => return false,
            State::SosPmApcString => {
                if self.string_utf8_rem > 0 {
                    return false; // payload UTF-8 continuation, not a C1
                }
                if b == 0x9c {
                    self.state = State::Ground;
                }
                return true;
            }
            _ => {}
        }

        match b {
            0x90 => self.goto(State::DcsPassthrough, sink), // DCS
            0x98 | 0x9e => {
                // SOS / PM
                self.state = State::SosPmApcString;
                self.string_utf8_rem = 0;
            }
            0x9b => self.goto(State::CsiEntry, sink), // CSI
            0x9c => {
                if self.state == State::OscString {
                    self.goto(State::Ground, sink);
                } else {
                    self.state = State::Ground; // ST outside a string: ignore.
                }
            }
            0x9d => self.goto(State::OscString, sink), // OSC
            0x9f => self.goto(State::ApcString, sink), // APC
            _ => return false,
        }
        true
    }

    fn collect(&mut self, b: u8) {
        self.intermediates.push(b);
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
        sink(Action::CsiDispatch(Csi::from_parts(
            self.params,
            self.sep_colon,
            self.intermediates,
            self.private,
            final_byte,
        )));
    }

    fn esc_dispatch<F: FnMut(Action)>(&mut self, final_byte: u8, sink: &mut F) {
        sink(Action::EscDispatch(Esc::from_parts(
            self.intermediates,
            final_byte,
        )));
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
        // Payload UTF-8 tracking never survives a state change.
        self.string_utf8_rem = 0;
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
