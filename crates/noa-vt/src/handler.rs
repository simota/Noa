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

    // ── control ────────────────────────────────────────────────────
    fn carriage_return(&mut self);
    /// Index (line feed without carriage return): down one, scroll at bottom.
    fn linefeed(&mut self);
    fn tab(&mut self, n: u16);
    /// `RI` (`ESC M`) — reverse index.
    fn reverse_index(&mut self);
    /// `DECSC` (`ESC 7`) / `CSI s`.
    fn save_cursor(&mut self);
    /// `DECRC` (`ESC 8`) / `CSI u`.
    fn restore_cursor(&mut self);
    /// `HTS` (`ESC H`) — set a tab stop at the cursor column.
    fn set_tab_stop(&mut self) {}
    /// `RIS` (`ESC c`) — full reset.
    fn full_reset(&mut self);

    // ── reports (terminal writes back to the pty) ──────────────────
    fn device_attributes(&mut self, kind: DaKind);
    fn device_status_report(&mut self, kind: DsrKind);

    // ── parsed but inc-1 no-ops ────────────────────────────────────
    /// OSC payload (`ESC ] … ST`). Inc-1: parse title (`0`/`2`), drop the rest.
    fn osc_dispatch(&mut self, _data: &[u8]) {}
    /// `DECSTBM` — set the vertical scroll region (1-based; `bottom = 0` = last row).
    fn set_scroll_region(&mut self, _top: u16, _bottom: u16) {}
}
