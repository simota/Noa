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

/// The terminal-state operations a parsed VT stream drives.
///
/// Methods with default no-op / composed bodies are the ones a minimal inc-1
/// model may leave unimplemented; the required methods form the inc-1 core.
pub trait Handler {
    // ── text ───────────────────────────────────────────────────────
    fn print(&mut self, c: char);
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

    // ── reports (terminal writes back to the pty) ──────────────────
    fn device_attributes(&mut self, kind: DaKind);
    fn device_status_report(&mut self, kind: DsrKind);

    // ── parsed but inc-1 no-ops ────────────────────────────────────
    /// OSC payload (`ESC ] … ST`). Inc-1: parse title (`0`/`2`), drop the rest.
    fn osc_dispatch(&mut self, _data: &[u8]) {}
    /// DCS payload (`ESC P … ST`) for query protocols such as DECRQSS and XTGETTCAP.
    fn dcs_dispatch(&mut self, _data: &[u8]) {}
    /// `DECSTBM` — set the vertical scroll region (1-based; `bottom = 0` = last row).
    fn set_scroll_region(&mut self, _top: u16, _bottom: u16) {}
}
