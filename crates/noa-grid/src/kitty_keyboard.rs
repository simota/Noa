//! Kitty keyboard protocol progressive-enhancement state.
//!
//! Tracks the per-screen flag stacks driven by the `CSI ... u` private-marker
//! sequences (`CSI ? u` query, `CSI > flags u` push, `CSI < n u` pop,
//! `CSI = flags ; mode u` set). The active flags are the top of the stack for
//! the currently active screen (main vs alternate), or `0` when empty.
//!
//! Ghostty analog: `terminal/kitty/key.zig` (`KeyFlagStack`).

/// Disambiguate escape codes (progressive-enhancement bit 1).
pub const KITTY_DISAMBIGUATE: u8 = 0b0_0001;
/// Report event types — press/repeat/release (bit 2).
pub const KITTY_REPORT_EVENT_TYPES: u8 = 0b0_0010;
/// Report alternate keys — shifted / base-layout key codes (bit 4).
pub const KITTY_REPORT_ALTERNATE_KEYS: u8 = 0b0_0100;
/// Report all keys as escape codes (bit 8).
pub const KITTY_REPORT_ALL_KEYS: u8 = 0b0_1000;
/// Report associated text (bit 16).
pub const KITTY_REPORT_ASSOCIATED_TEXT: u8 = 0b1_0000;

/// Every defined progressive-enhancement bit; higher bits are ignored on set.
pub const KITTY_ALL_FLAGS: u8 = KITTY_DISAMBIGUATE
    | KITTY_REPORT_EVENT_TYPES
    | KITTY_REPORT_ALTERNATE_KEYS
    | KITTY_REPORT_ALL_KEYS
    | KITTY_REPORT_ASSOCIATED_TEXT;

/// Maximum stack depth per kitty spec. A push beyond this evicts the oldest
/// entry so the newest flags always take effect.
const STACK_DEPTH: usize = 8;

/// `mode` field of `CSI = flags ; mode u` — how the given flags combine with
/// the current ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetMode {
    /// mode 1 (or omitted): replace the current flags with the given value.
    Replace,
    /// mode 2: set (OR in) the given bits.
    Or,
    /// mode 3: clear (AND-NOT) the given bits.
    Clear,
}

impl SetMode {
    /// Decode the raw `mode` param; anything other than 2/3 is [`SetMode::Replace`].
    pub fn from_param(mode: u16) -> Self {
        match mode {
            2 => SetMode::Or,
            3 => SetMode::Clear,
            _ => SetMode::Replace,
        }
    }
}

/// Two independent flag stacks — one per screen (main / alternate). The active
/// screen selects which stack the `CSI ... u` sequences and the current-flags
/// query operate on.
#[derive(Clone, Debug, Default)]
pub struct KittyKeyboard {
    main: Vec<u8>,
    alt: Vec<u8>,
}

impl KittyKeyboard {
    fn stack(&self, is_alt: bool) -> &Vec<u8> {
        if is_alt { &self.alt } else { &self.main }
    }

    fn stack_mut(&mut self, is_alt: bool) -> &mut Vec<u8> {
        if is_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    /// Current active flags for `is_alt`'s screen — the top of the stack, or
    /// `0` when the stack is empty.
    pub fn flags(&self, is_alt: bool) -> u8 {
        self.stack(is_alt).last().copied().unwrap_or(0)
    }

    /// `CSI > flags u` — push new flags. Beyond [`STACK_DEPTH`] the oldest
    /// entry is evicted first.
    pub fn push(&mut self, is_alt: bool, flags: u8) {
        let stack = self.stack_mut(is_alt);
        if stack.len() >= STACK_DEPTH {
            stack.remove(0);
        }
        stack.push(flags & KITTY_ALL_FLAGS);
    }

    /// `CSI < n u` — pop `n` entries (saturating at empty = flags 0).
    pub fn pop(&mut self, is_alt: bool, n: u16) {
        let stack = self.stack_mut(is_alt);
        for _ in 0..n {
            if stack.pop().is_none() {
                break;
            }
        }
    }

    /// `CSI = flags ; mode u` — modify the current (top) flags in place. When
    /// the stack is empty the current flags are treated as `0` and a new entry
    /// is created so the change takes effect.
    pub fn set(&mut self, is_alt: bool, flags: u8, mode: SetMode) {
        let flags = flags & KITTY_ALL_FLAGS;
        let stack = self.stack_mut(is_alt);
        let current = stack.last().copied().unwrap_or(0);
        let next = match mode {
            SetMode::Replace => flags,
            SetMode::Or => current | flags,
            SetMode::Clear => current & !flags,
        };
        match stack.last_mut() {
            Some(top) => *top = next,
            None => stack.push(next),
        }
    }

    /// `RIS` — clear both stacks.
    pub fn reset(&mut self) {
        self.main.clear();
        self.alt.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stack_reports_zero_flags() {
        let kb = KittyKeyboard::default();
        assert_eq!(kb.flags(false), 0);
        assert_eq!(kb.flags(true), 0);
    }

    #[test]
    fn push_pop_tracks_top() {
        let mut kb = KittyKeyboard::default();
        kb.push(false, KITTY_DISAMBIGUATE);
        assert_eq!(kb.flags(false), 1);
        kb.push(false, KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES);
        assert_eq!(kb.flags(false), 3);
        kb.pop(false, 1);
        assert_eq!(kb.flags(false), 1);
        kb.pop(false, 1);
        assert_eq!(kb.flags(false), 0);
        // Popping an empty stack is a no-op that leaves flags at 0.
        kb.pop(false, 5);
        assert_eq!(kb.flags(false), 0);
    }

    #[test]
    fn push_beyond_depth_evicts_oldest() {
        let mut kb = KittyKeyboard::default();
        for i in 0..8 {
            kb.push(false, i as u8 & KITTY_ALL_FLAGS);
        }
        // Stack now holds 0..=7 (flags 0..7). Pushing a 9th evicts the oldest.
        kb.push(false, KITTY_ALL_FLAGS);
        assert_eq!(kb.flags(false), KITTY_ALL_FLAGS);
        // Pop back down to the bottom: 8 entries remain (oldest `0` gone), so
        // seven pops reach the entry that was pushed second (flags 1).
        kb.pop(false, 7);
        assert_eq!(kb.flags(false), 1);
    }

    #[test]
    fn set_modes_replace_or_clear() {
        let mut kb = KittyKeyboard::default();
        // Set on an empty stack creates an entry.
        kb.set(false, KITTY_DISAMBIGUATE, SetMode::Replace);
        assert_eq!(kb.flags(false), 1);
        kb.set(false, KITTY_REPORT_EVENT_TYPES, SetMode::Or);
        assert_eq!(kb.flags(false), 3);
        kb.set(false, KITTY_DISAMBIGUATE, SetMode::Clear);
        assert_eq!(kb.flags(false), 2);
        kb.set(false, KITTY_REPORT_ALL_KEYS, SetMode::Replace);
        assert_eq!(kb.flags(false), 8);
    }

    #[test]
    fn main_and_alt_stacks_are_independent() {
        let mut kb = KittyKeyboard::default();
        kb.push(false, KITTY_DISAMBIGUATE);
        kb.push(true, KITTY_REPORT_ALL_KEYS);
        assert_eq!(kb.flags(false), 1);
        assert_eq!(kb.flags(true), 8);
        kb.reset();
        assert_eq!(kb.flags(false), 0);
        assert_eq!(kb.flags(true), 0);
    }
}
