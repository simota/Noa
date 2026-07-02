//! Terminal mode state, keyed by `(value, ansi)` where `ansi=false` denotes a
//! DEC private mode (`CSI ? … h/l`).

use std::collections::HashSet;

#[derive(Clone, Debug, Default)]
pub struct ModeState {
    set: HashSet<(u16, bool)>,
}

impl ModeState {
    /// The power-on defaults: autowrap (DECAWM 7) and cursor-visible (DECTCEM 25) on.
    pub fn defaults() -> Self {
        let mut m = ModeState::default();
        m.set(7, false, true); // DECAWM
        m.set(25, false, true); // DECTCEM
        m
    }

    pub fn set(&mut self, value: u16, ansi: bool, on: bool) {
        if on {
            self.set.insert((value, ansi));
        } else {
            self.set.remove(&(value, ansi));
        }
    }

    pub fn get(&self, value: u16, ansi: bool) -> bool {
        self.set.contains(&(value, ansi))
    }

    // ── named accessors for the inc-1 modes that have behavior ─────
    /// DECAWM — automatic wraparound.
    pub fn autowrap(&self) -> bool {
        self.get(7, false)
    }
    /// DECTCEM — cursor visible.
    pub fn cursor_visible(&self) -> bool {
        self.get(25, false)
    }
    /// DECCKM — application cursor keys (arrow keys send SS3 not CSI).
    pub fn app_cursor_keys(&self) -> bool {
        self.get(1, false)
    }
    /// DECSET 2004 — bracketed paste mode.
    pub fn bracketed_paste(&self) -> bool {
        self.get(2004, false)
    }
    /// LNM — line-feed/new-line mode (LF also does CR).
    pub fn linefeed_newline(&self) -> bool {
        self.get(20, true)
    }
}
