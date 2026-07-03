//! G0/G1 character-set state and the DEC Special Graphics (VT100
//! line-drawing) translation table. Ghostty analog: `terminal/charsets.zig`.

use noa_vt::{Charset, CharsetSlot};

/// The two designated slots (G0/G1) plus which is active (GL). `RIS` and
/// `DECSTR` both reset this to the default (`G0 = Ascii`, `G1 = Ascii`,
/// active `G0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharsetState {
    g0: Charset,
    g1: Charset,
    active: CharsetSlot,
}

impl Default for CharsetState {
    fn default() -> Self {
        CharsetState {
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            active: CharsetSlot::G0,
        }
    }
}

impl CharsetState {
    /// `SCS` — designate `set` into `slot`.
    pub fn designate(&mut self, slot: CharsetSlot, set: Charset) {
        match slot {
            CharsetSlot::G0 => self.g0 = set,
            CharsetSlot::G1 => self.g1 = set,
        }
    }

    /// `SO`/`SI` — shift the active (GL) slot.
    pub fn shift(&mut self, slot: CharsetSlot) {
        self.active = slot;
    }

    /// Translate a printed scalar through the active (GL) charset.
    pub fn translate(&self, c: char) -> char {
        let active = match self.active {
            CharsetSlot::G0 => self.g0,
            CharsetSlot::G1 => self.g1,
        };
        match active {
            Charset::Ascii => c,
            Charset::DecSpecialGraphics => dec_special_graphics(c),
        }
    }
}

/// The VT100 DEC Special Graphics (line-drawing) table, active for
/// `` ` ``..=`~` (`0x60..=0x7e`); every other scalar passes through unchanged.
fn dec_special_graphics(c: char) -> char {
    match c {
        '`' => '\u{25c6}', // ◆
        'a' => '\u{2592}', // ▒
        'b' => '\u{2409}', // ␉ HT
        'c' => '\u{240c}', // ␌ FF
        'd' => '\u{240d}', // ␍ CR
        'e' => '\u{240a}', // ␊ LF
        'f' => '\u{00b0}', // °
        'g' => '\u{00b1}', // ±
        'h' => '\u{2424}', // ␤ NL
        'i' => '\u{240b}', // ␋ VT
        'j' => '\u{2518}', // ┘
        'k' => '\u{2510}', // ┐
        'l' => '\u{250c}', // ┌
        'm' => '\u{2514}', // └
        'n' => '\u{253c}', // ┼
        'o' => '\u{23ba}', // ⎺
        'p' => '\u{23bb}', // ⎻
        'q' => '\u{2500}', // ─
        'r' => '\u{23bc}', // ⎼
        's' => '\u{23bd}', // ⎽
        't' => '\u{251c}', // ├
        'u' => '\u{2524}', // ┤
        'v' => '\u{2534}', // ┴
        'w' => '\u{252c}', // ┬
        'x' => '\u{2502}', // │
        'y' => '\u{2264}', // ≤
        'z' => '\u{2265}', // ≥
        '{' => '\u{03c0}', // π
        '|' => '\u{2260}', // ≠
        '}' => '\u{00a3}', // £
        '~' => '\u{00b7}', // ·
        other => other,
    }
}
