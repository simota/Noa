//! Terminal mode state, keyed by `(value, ansi)` where `ansi=false` denotes a
//! DEC private mode (`CSI ? … h/l`).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseTracking {
    Off,
    /// DECSET 9 — X10 compatibility mode: presses only, no modifiers.
    X10,
    Press,
    ButtonMotion,
    AnyMotion,
}

/// Coordinate encoding for mouse reports, selected by DECSET 1005/1015/1006.
///
/// Orthogonal to [`MouseTracking`]: the tracking mode decides *which* events
/// are reported, the format decides *how* they are written to the pty.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseFormat {
    /// X10/normal encoding: `ESC [ M` followed by three raw bytes.
    #[default]
    Legacy,
    /// DECSET 1005 — legacy values encoded as UTF-8 code points.
    Utf8,
    /// DECSET 1015 — urxvt decimal encoding `CSI Cb ; Cx ; Cy M`.
    Urxvt,
    /// DECSET 1006 — SGR encoding `CSI < Cb ; Cx ; Cy M/m`.
    Sgr,
}

/// The DEC private mode numbers that select a [`MouseFormat`]. They are
/// mutually exclusive: the last one set wins (xterm keeps a single
/// "extended coordinates" slot, not one flag per mode).
const MOUSE_FORMAT_MODES: [u16; 3] = [1005, 1015, 1006];

#[derive(Clone, Debug, Default)]
pub struct ModeState {
    /// A real session holds at most a handful of modes at once (the power-on
    /// defaults plus whatever a full-screen app toggles), so a linear-scanned
    /// `Vec` beats a `HashSet` here: [`Self::autowrap`], [`Self::linefeed_newline`]
    /// and friends are queried on nearly every `print_str`/`execute_c0` call —
    /// once per bulk-print run and once per C0 control — and `SipHash`'s
    /// per-call keying/finalization dwarfed the cost of comparing a dozen
    /// `(u16, bool)` pairs directly (measured: mode-lookup hashing was ~8% of
    /// `bench_throughput`'s self-time).
    set: Vec<(u16, bool)>,
}

impl ModeState {
    /// The power-on defaults: autowrap (DECAWM 7), cursor-visible (DECTCEM 25)
    /// and alternate scroll (1007) on — 1007 defaults on to match Ghostty.
    pub fn defaults() -> Self {
        let mut m = ModeState::default();
        m.set(7, false, true); // DECAWM
        m.set(25, false, true); // DECTCEM
        m.set(1007, false, true); // alternate scroll
        m
    }

    pub fn set(&mut self, value: u16, ansi: bool, on: bool) {
        // Mouse-format modes displace each other: setting one clears the
        // others, and resetting a non-active one leaves the active format
        // untouched (matching xterm's single extend_coords slot).
        if on && !ansi && MOUSE_FORMAT_MODES.contains(&value) {
            self.set
                .retain(|&(v, a)| a || !MOUSE_FORMAT_MODES.contains(&v));
        }
        if on {
            if !self.set.contains(&(value, ansi)) {
                self.set.push((value, ansi));
            }
        } else {
            self.set.retain(|&e| e != (value, ansi));
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
    /// DECNKM / DECPAM — application keypad mode.
    pub fn app_keypad(&self) -> bool {
        self.get(66, false)
    }
    /// DECLRMM — left/right margin mode.
    pub fn left_right_margin(&self) -> bool {
        self.get(69, false)
    }
    /// DECSET 2004 — bracketed paste mode.
    pub fn bracketed_paste(&self) -> bool {
        self.get(2004, false)
    }
    /// DECSET 1005/1015/1006 — the active mouse report encoding. At most one
    /// of the format modes is set at a time (see [`ModeState::set`]), so the
    /// probe order here is irrelevant.
    pub fn mouse_format(&self) -> MouseFormat {
        if self.get(1006, false) {
            MouseFormat::Sgr
        } else if self.get(1015, false) {
            MouseFormat::Urxvt
        } else if self.get(1005, false) {
            MouseFormat::Utf8
        } else {
            MouseFormat::Legacy
        }
    }
    /// DECSET 1007 — alternate scroll: on the alternate screen with mouse
    /// tracking off, wheel events become cursor up/down key presses.
    pub fn alternate_scroll(&self) -> bool {
        self.get(1007, false)
    }
    /// DECSET 1004 — focus event reporting.
    pub fn focus_reporting(&self) -> bool {
        self.get(1004, false)
    }
    /// DECSET 2026 — synchronized output mode.
    pub fn synchronized_output(&self) -> bool {
        self.get(2026, false)
    }
    /// DECSET 2027 — grapheme clustering (candidate-1 scope: ZWJ / Fitzpatrick
    /// modifier / regional-indicator pairing; full UAX#29 is out of scope).
    pub fn grapheme_clustering(&self) -> bool {
        self.get(2027, false)
    }
    /// DECSET 9/1000/1002/1003 mouse tracking mode.
    pub fn mouse_tracking(&self) -> MouseTracking {
        if self.get(1003, false) {
            MouseTracking::AnyMotion
        } else if self.get(1002, false) {
            MouseTracking::ButtonMotion
        } else if self.get(1000, false) {
            MouseTracking::Press
        } else if self.get(9, false) {
            MouseTracking::X10
        } else {
            MouseTracking::Off
        }
    }
    /// True when the app should encode mouse events for the pty. The format
    /// is always defined (Legacy by default), so only tracking gates this.
    pub fn mouse_reporting(&self) -> bool {
        self.mouse_tracking() != MouseTracking::Off
    }
    /// LNM — line-feed/new-line mode (LF also does CR).
    pub fn linefeed_newline(&self) -> bool {
        self.get(20, true)
    }
}
