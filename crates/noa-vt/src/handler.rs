//! The parse↔state seam: [`Handler`] is implemented by a terminal state model
//! (`noa-grid`'s `Terminal`). [`crate::Stream`] decodes [`crate::Action`]s and
//! calls these methods. Mirrors Ghostty's `stream.zig` → `StreamHandler`.

use crate::sgr::SgrAttr;

/// `ED` (erase-in-display) mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EraseDisplay {
    /// From the cursor to the end of the screen.
    Below,
    /// From the start of the screen to the cursor.
    Above,
    /// The whole screen.
    Complete,
    /// The scrollback buffer.
    Scrollback,
}

/// `EL` (erase-in-line) mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EraseLine {
    /// From the cursor to the end of the line.
    Right,
    /// From the start of the line to the cursor.
    Left,
    /// The whole line.
    Complete,
}

/// `DA` (device attributes) request kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DaKind {
    Primary,
    Secondary,
}

/// `DSR` (device status report) request kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DsrKind {
    /// `CSI 5 n` — operating status.
    Status,
    /// `CSI 6 n` — cursor position report.
    CursorPosition,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorStyle {
    /// DECSCUSR 0: reset to the configured default cursor style.
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

/// `SCS` (`ESC ( x` / `ESC ) x`) target slot — which of G0/G1 is designated.
/// G2/G3 and `SS2`/`SS3` are out of scope (Lite slice).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharsetSlot {
    G0,
    G1,
}

/// A designated character set. Lite scope: ASCII and DEC Special Graphics
/// (VT100 line-drawing) only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charset {
    Ascii,
    DecSpecialGraphics,
}

/// `DECRQM` (`CSI Ps $ p` / `CSI ? Ps $ p`) request target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ModeRequest {
    pub value: u16,
    pub ansi: bool,
}

/// One complete line inside a [`Handler::print_ascii_lines`] batch: the
/// (possibly empty) printable-ASCII text and whether its terminator was
/// `CR LF` rather than a bare `LF`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AsciiLine<'a> {
    pub text: &'a [u8],
    pub crlf: bool,
}

/// Iterator over the complete (LF-terminated) lines of a
/// [`Handler::print_ascii_lines`] batch. The batch contract guarantees every
/// line is LF-terminated, so [`AsciiLines::remainder`] is empty once the
/// iterator is exhausted; it is exposed so lenient consumers can still print
/// a violating unterminated tail rather than drop bytes.
pub struct AsciiLines<'a> {
    rest: &'a [u8],
}

impl<'a> AsciiLines<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { rest: data }
    }

    /// Bytes not yet consumed as complete lines.
    pub fn remainder(&self) -> &'a [u8] {
        self.rest
    }
}

impl<'a> Iterator for AsciiLines<'a> {
    type Item = AsciiLine<'a>;

    fn next(&mut self) -> Option<AsciiLine<'a>> {
        let nl = self.rest.iter().position(|&b| b == b'\n')?;
        let crlf = nl > 0 && self.rest[nl - 1] == b'\r';
        let text = &self.rest[..nl - usize::from(crlf)];
        self.rest = &self.rest[nl + 1..];
        Some(AsciiLine { text, crlf })
    }
}

/// One complete line inside a [`Handler::print_sgr_ascii_lines`] batch:
/// `lead` and `tail` are (possibly empty) contiguous runs of whole plain SGR
/// sequences (`ESC [ params m`, see [`crate::sgr::scan_plain_sgr`]) around
/// the (possibly empty) printable-ASCII `text`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SgrAsciiLine<'a> {
    pub lead: &'a [u8],
    pub text: &'a [u8],
    pub tail: &'a [u8],
    pub crlf: bool,
}

/// Iterator over the complete (LF-terminated) lines of a
/// [`Handler::print_sgr_ascii_lines`] batch, splitting each into its
/// lead-SGR / text / tail-SGR parts. Splitting trusts the batch contract
/// (`Stream`'s scanner validated the span): an SGR unit's params can never
/// contain `m`, so each `ESC`-led unit ends at the next `m` byte.
pub struct SgrAsciiLines<'a> {
    rest: &'a [u8],
}

impl<'a> SgrAsciiLines<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { rest: data }
    }

    /// Bytes not yet consumed as complete lines.
    pub fn remainder(&self) -> &'a [u8] {
        self.rest
    }
}

impl<'a> Iterator for SgrAsciiLines<'a> {
    type Item = SgrAsciiLine<'a>;

    fn next(&mut self) -> Option<SgrAsciiLine<'a>> {
        // Mirrors the walk `Stream`'s scanner validated: SGR units, one
        // SWAR text-run scan, SGR units, terminator — a single forward pass
        // (no separate LF search + ESC search over the same bytes).
        let b = self.rest;
        let mut p = 0;
        while b.get(p) == Some(&0x1b) {
            let Some(len) = crate::sgr::scan_plain_sgr(&b[p..]) else {
                debug_assert!(false, "print_sgr_ascii_lines lead unit is a plain SGR");
                return None;
            };
            p += len;
        }
        let lead = &b[..p];
        let (run, _ascii) = crate::stream::scan_run(&b[p..]);
        let text = &b[p..p + run];
        let mut q = p + run;
        while b.get(q) == Some(&0x1b) {
            let Some(len) = crate::sgr::scan_plain_sgr(&b[q..]) else {
                debug_assert!(false, "print_sgr_ascii_lines tail unit is a plain SGR");
                return None;
            };
            q += len;
        }
        let tail = &b[p + run..q];
        let crlf = match b.get(q) {
            Some(b'\n') => false,
            Some(b'\r') if b.get(q + 1) == Some(&b'\n') => true,
            _ => {
                debug_assert!(
                    b.get(q).is_none(),
                    "print_sgr_ascii_lines lines end in (CR)? LF"
                );
                return None;
            }
        };
        self.rest = &b[q + 1 + usize::from(crlf)..];
        Some(SgrAsciiLine {
            lead,
            text,
            tail,
            crlf,
        })
    }
}

/// Iterator over the whole plain SGR units of a [`SgrAsciiLine`] `lead` or
/// `tail` slice, yielding each unit's raw bytes (`ESC [ params m`).
pub struct PlainSgrUnits<'a> {
    rest: &'a [u8],
}

impl<'a> PlainSgrUnits<'a> {
    pub fn new(run: &'a [u8]) -> Self {
        Self { rest: run }
    }
}

impl<'a> Iterator for PlainSgrUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            return None;
        }
        let len =
            crate::sgr::scan_plain_sgr(self.rest).expect("SGR run holds whole plain SGR units");
        let (unit, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(unit)
    }
}

/// The terminal-state operations a parsed VT stream drives.
///
/// Methods with default no-op / composed bodies are the ones a minimal inc-1
/// model may leave unimplemented; the required methods form the inc-1 core.
pub trait Handler {
    // ── text ───────────────────────────────────────────────────────
    fn print(&mut self, c: char);
    /// A run of printable scalars (no C0/C1 controls), semantically identical
    /// to calling [`Handler::print`] once per scalar. `Stream` emits whole
    /// ground-state text runs through this so a state model can take a bulk
    /// fast path (Ghostty analog: `printString`); the default body preserves
    /// per-scalar behavior for implementations that don't.
    fn print_str(&mut self, s: &str) {
        for c in s.chars() {
            self.print(c);
        }
    }
    /// A run of complete printable-ASCII lines seen in plain ground state:
    /// `data` is a concatenation of one or more `text (CR)? LF` groups where
    /// `text` is (possibly empty) printable ASCII (`0x20..=0x7E`).
    /// Semantically identical to, per group: [`Handler::print_str`] on
    /// `text` (when non-empty), then [`Handler::execute_c0`] for the `CR`
    /// (when present) and the `LF`. `Stream` batches ground-state line
    /// floods through this so a state model can amortize per-line scroll
    /// costs across the whole batch; the default body preserves per-line
    /// behavior for implementations that don't.
    fn print_ascii_lines(&mut self, data: &[u8]) {
        let mut lines = AsciiLines::new(data);
        for line in &mut lines {
            if !line.text.is_empty() {
                let text = core::str::from_utf8(line.text)
                    .expect("print_ascii_lines text is printable ASCII");
                self.print_str(text);
            }
            if line.crlf {
                self.execute_c0(0x0d);
            }
            self.execute_c0(0x0a);
        }
        debug_assert!(
            lines.remainder().is_empty(),
            "print_ascii_lines data must be a run of complete LF-terminated lines"
        );
        if !lines.remainder().is_empty() {
            let text = core::str::from_utf8(lines.remainder())
                .expect("print_ascii_lines text is printable ASCII");
            self.print_str(text);
        }
    }

    /// [`Handler::print_ascii_lines`] extended with per-line styling: `data`
    /// is a concatenation of one or more `sgr* text sgr* (CR)? LF` groups,
    /// where `text` is (possibly empty) printable ASCII and each `sgr` is a
    /// whole plain SGR sequence ([`crate::sgr::scan_plain_sgr`]). SGRs never
    /// interrupt a line's text, so a state model can fill each batched row
    /// from a single per-line style template. Semantically identical to, in
    /// order per group: [`Handler::set_attributes`] once per lead unit,
    /// [`Handler::print_str`] on `text` (when non-empty), `set_attributes`
    /// once per tail unit, then [`Handler::execute_c0`] for the `CR` (when
    /// present) and the `LF`. The default body replays exactly that.
    fn print_sgr_ascii_lines(&mut self, data: &[u8]) {
        let mut attrs = Vec::new();
        let mut lines = SgrAsciiLines::new(data);
        for line in &mut lines {
            for unit in PlainSgrUnits::new(line.lead) {
                crate::sgr::parse_plain_sgr_unit(unit, &mut attrs);
                self.set_attributes(&attrs);
            }
            if !line.text.is_empty() {
                let text = core::str::from_utf8(line.text)
                    .expect("print_sgr_ascii_lines text is printable ASCII");
                self.print_str(text);
            }
            for unit in PlainSgrUnits::new(line.tail) {
                crate::sgr::parse_plain_sgr_unit(unit, &mut attrs);
                self.set_attributes(&attrs);
            }
            if line.crlf {
                self.execute_c0(0x0d);
            }
            self.execute_c0(0x0a);
        }
        debug_assert!(
            lines.remainder().is_empty(),
            "print_sgr_ascii_lines data must be a run of complete LF-terminated lines"
        );
        if !lines.remainder().is_empty() {
            let text = core::str::from_utf8(lines.remainder())
                .expect("print_sgr_ascii_lines text is printable ASCII");
            self.print_str(text);
        }
    }

    /// [`Handler::print_str`] for a caller that has already verified every
    /// byte of `s` is printable ASCII (`0x20..=0x7e`) — `Stream`'s
    /// ground-scan fast path knows this the moment its SWAR boundary scan
    /// finds no non-ASCII byte, but `print_str` re-derives it internally
    /// (a per-byte classification pass over text this call already proved
    /// ASCII) to stay correct for callers that can't make that guarantee.
    /// Default body forwards to `print_str`, so implementations that don't
    /// override this still behave correctly; `s` is guaranteed non-empty.
    fn print_ascii_str(&mut self, s: &str) {
        self.print_str(s);
    }

    /// A C0 control byte (`BEL`/`BS`/`HT`/`LF`/`VT`/`FF`/`CR`/…).
    fn execute_c0(&mut self, byte: u8);

    // ── cursor movement ────────────────────────────────────────────
    fn cursor_up(&mut self, n: u16);
    fn cursor_down(&mut self, n: u16);
    fn cursor_forward(&mut self, n: u16);
    fn cursor_backward(&mut self, n: u16);
    /// `CNL` — down `n` lines and to column 1.
    fn cursor_next_line(&mut self, n: u16) {
        self.cursor_down(n);
        self.carriage_return();
    }
    /// `CPL` — up `n` lines and to column 1.
    fn cursor_prev_line(&mut self, n: u16) {
        self.cursor_up(n);
        self.carriage_return();
    }
    /// `CUP`/`HVP` — 1-based row/col.
    fn cursor_position(&mut self, row: u16, col: u16);
    /// `CHA`/`HPA` — 1-based column.
    fn cursor_col_abs(&mut self, col: u16);
    /// `VPA` — 1-based row.
    fn cursor_row_abs(&mut self, row: u16);

    // ── erase ──────────────────────────────────────────────────────
    fn erase_display(&mut self, mode: EraseDisplay);
    fn erase_line(&mut self, mode: EraseLine);

    // ── rendition / modes ──────────────────────────────────────────
    fn set_attributes(&mut self, attrs: &[SgrAttr]);
    fn set_mode(&mut self, value: u16, ansi: bool, on: bool);
    /// `DECSCUSR` (`CSI Ps SP q`) — cursor style.
    fn set_cursor_style(&mut self, _style: CursorStyle) {}
    /// `DECSLRM` (`CSI Pl;Pr s`) — set horizontal margins.
    fn set_horizontal_margins(&mut self, _left: u16, _right: u16) {}
    /// `DECPAM`/`DECPNM` (`ESC =`/`ESC >`) — application/numeric keypad.
    fn set_application_keypad(&mut self, _on: bool) {}
    /// `DECRQM` — request mode state; replies with `DECRPM`.
    fn request_mode(&mut self, _request: ModeRequest) {}

    // ── charsets ───────────────────────────────────────────────────
    /// `SCS` (`ESC ( x` / `ESC ) x`) — designate `set` into `slot` (G0/G1).
    fn designate_charset(&mut self, _slot: CharsetSlot, _set: Charset) {}
    /// `SO`/`SI` (`0x0E`/`0x0F`) — shift the active (GL) charset slot.
    fn locking_shift(&mut self, _slot: CharsetSlot) {}

    // ── control ────────────────────────────────────────────────────
    /// `BEL` (`0x07`) — ring the terminal bell. No grid-state side effect.
    fn bell(&mut self) {}
    fn carriage_return(&mut self);
    /// Index (line feed without carriage return): down one, scroll at bottom.
    fn linefeed(&mut self);
    fn tab(&mut self, n: u16);
    /// `CBT` — move backward to the previous tab stop(s).
    fn tab_back(&mut self, _n: u16) {}
    /// `RI` (`ESC M`) — reverse index.
    fn reverse_index(&mut self);
    /// `DECSC` (`ESC 7`) / `CSI s`.
    fn save_cursor(&mut self);
    /// `DECRC` (`ESC 8`) / `CSI u`.
    fn restore_cursor(&mut self);
    /// `HTS` (`ESC H`) — set a tab stop at the cursor column.
    fn set_tab_stop(&mut self) {}
    /// `TBC 0` — clear the tab stop at the cursor column.
    fn clear_tab_stop(&mut self) {}
    /// `TBC 3` — clear all horizontal tab stops.
    fn clear_all_tab_stops(&mut self) {}
    /// `RIS` (`ESC c`) — full reset.
    fn full_reset(&mut self);
    /// `DECALN` (`ESC # 8`) — screen alignment test: fill the active screen
    /// with `'E'`, home the cursor, leave margins/mode untouched.
    fn screen_alignment_test(&mut self) {}
    /// `DECSTR` (`CSI ! p`) — soft reset. Unlike `RIS`, screen content and
    /// scrollback are left untouched.
    fn soft_reset(&mut self) {}

    // ── edit ───────────────────────────────────────────────────────
    /// `ICH` — insert blank cells at the cursor.
    fn insert_blank_chars(&mut self, _n: u16) {}
    /// `IL` — insert blank lines in the scroll region.
    fn insert_lines(&mut self, _n: u16) {}
    /// `DL` — delete lines in the scroll region.
    fn delete_lines(&mut self, _n: u16) {}
    /// `DCH` — delete cells at the cursor.
    fn delete_chars(&mut self, _n: u16) {}
    /// `SU` — scroll the scroll region up.
    fn scroll_up(&mut self, _n: u16) {}
    /// `SD` — scroll the scroll region down.
    fn scroll_down(&mut self, _n: u16) {}
    /// `ECH` — erase cells at the cursor without moving it.
    fn erase_chars(&mut self, _n: u16) {}
    /// `REP` — repeat the preceding printable character.
    fn repeat_preceding_char(&mut self, _n: u16) {}
    /// Client-mode seed-only: `CSI > Ph ; Pl $ s` sets the active screen's
    /// `REP` state (`last_printed`) to the Unicode scalar
    /// `(Ph << 16) | Pl`, without touching grid content. Standard `REP`
    /// ties `last_printed` to whatever was most recently painted, so a
    /// synthetic repaint that visits cells in a different order than the
    /// source's live prints would otherwise leave `last_printed` pointing
    /// at the wrong character. Emitted only by
    /// `noa_grid::terminal::Terminal::synthetic_seed`; real programs never
    /// send this.
    fn seed_set_last_printed(&mut self, _ch: char) {}
    /// Client-mode seed-only: `CSI > $ t` promotes the cursor style
    /// set by the immediately preceding `DECSCUSR` from a plain block to
    /// its hollow variant. Standard `DECSCUSR` cannot express
    /// `block_hollow`, so the seed follows a block `DECSCUSR` with this to
    /// recover it exactly. Emitted only by `Terminal::synthetic_seed`; real
    /// programs never send this.
    fn seed_set_cursor_hollow(&mut self) {}
    /// Client-mode seed-only: `CSI > Ps ; Ph $ q` restores the DECSCUSR-0
    /// default cursor style (the shape a bare `CSI 0 q` resets to), kept
    /// independent from whatever `DECSCUSR` the seed used to paint the
    /// *current* cursor above it. `Ps` matches `DECSCUSR`'s own
    /// blink/style numbering (1=blinking block … 6=steady bar); `Ph` is `1`
    /// when the default is the hollow-block variant standard `DECSCUSR`
    /// cannot express (mirrors [`Self::seed_set_cursor_hollow`] for the
    /// default rather than the live cursor). Emitted only by
    /// `noa_grid::terminal::Terminal::synthetic_seed`; real programs never
    /// send this.
    fn seed_set_default_cursor_style(&mut self, _ps: u16, _hollow: bool) {}

    // ── reports (terminal writes back to the pty) ──────────────────
    fn device_attributes(&mut self, kind: DaKind);
    fn device_status_report(&mut self, kind: DsrKind);
    /// `XTVERSION` (`CSI > 0 q` / `CSI > q`) — report the terminal name/version.
    fn xtversion_query(&mut self) {}
    /// `XTWINOPS` (`CSI Ps ; Ps1 ; Ps2 t`) — window operation / report
    /// request. Ghostty-parity subset only: `Ps` 14/16/18/21 report; 22/23
    /// push/pop the window-title stack; every other `Ps` (4/8/9/10/19/20/…)
    /// is ignored with no reply.
    fn window_op(&mut self, _ps: u16, _p1: u16, _p2: u16) {}

    // ── Kitty keyboard protocol (`CSI ... u` with a private marker) ────
    /// `CSI ? u` — query the active progressive-enhancement flags; replies
    /// with `CSI ? <flags> u`.
    fn kitty_keyboard_query(&mut self) {}
    /// `CSI > flags u` — push `flags` onto the active screen's flag stack.
    fn kitty_keyboard_push(&mut self, _flags: u8) {}
    /// `CSI < n u` — pop `n` entries from the active screen's flag stack.
    fn kitty_keyboard_pop(&mut self, _n: u16) {}
    /// `CSI = flags ; mode u` — set flags (`mode` 1 replace / 2 set / 3 clear).
    fn kitty_keyboard_set(&mut self, _flags: u8, _mode: u16) {}

    /// XTMODKEYS `CSI > 4 ; Pv m` — xterm modifyOtherKeys level (0/1/2);
    /// the reset forms (`CSI > 4 m` / `CSI > m`) arrive as level 0.
    fn set_modify_other_keys(&mut self, _level: u16) {}

    // ── parsed but inc-1 no-ops ────────────────────────────────────
    /// OSC payload (`ESC ] … ST`). Inc-1: parse title (`0`/`2`), drop the rest.
    fn osc_dispatch(&mut self, _data: &[u8]) {}
    /// DCS payload (`ESC P … ST`) for query protocols such as DECRQSS and XTGETTCAP.
    fn dcs_dispatch(&mut self, _data: &[u8]) {}
    /// SIXEL graphics (`DCS Pa;Pb;Ph q Ps..Ps ST`). Parsed by `noa-vt`; the
    /// grid layer rasterizes it and reuses the existing image placement path.
    fn sixel_graphics(&mut self, _cmd: crate::sixel::SixelGraphicsCommand) {}
    /// Kitty graphics command (`ESC _ G … ST`). Parsed by `noa-vt`; the grid
    /// layer decodes the payload, stores the image, and queues any reply.
    fn kitty_graphics(&mut self, _cmd: crate::kitty_graphics::KittyGraphicsCommand) {}
    /// `DECSTBM` — set the vertical scroll region (1-based; `bottom = 0` = last row).
    fn set_scroll_region(&mut self, _top: u16, _bottom: u16) {}
}
