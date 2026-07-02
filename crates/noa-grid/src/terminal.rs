//! [`Terminal`] — the top-level state model. Implements [`noa_vt::Handler`],
//! dispatching parsed operations onto the active [`Screen`] and queuing report
//! replies (DA/DSR) for the pty writer.

use crate::cursor::ScrollRegion;
use crate::modes::ModeState;
use crate::screen::Screen;
use noa_core::{CellAttrs, Color, GridSize};
use noa_vt::{DaKind, DsrKind, EraseDisplay, EraseLine, Handler, SgrAttr};

pub struct Terminal {
    pub primary: Screen,
    /// Alternate screen — populated in inc≥2.
    pub alt: Option<Screen>,
    pub active_is_alt: bool,
    pub modes: ModeState,
    /// Window title from OSC 0/2 (stored; unused by the inc-1 renderer).
    pub title: String,
    pub size: GridSize,
    /// Bytes the terminal must write back to the pty (query replies).
    pub pending_writes: Vec<u8>,
}

impl Terminal {
    pub fn new(size: GridSize) -> Self {
        Terminal {
            primary: Screen::new(size.cols, size.rows),
            alt: None,
            active_is_alt: false,
            modes: ModeState::defaults(),
            title: String::new(),
            size,
            pending_writes: Vec::new(),
        }
    }

    /// The active screen (inc-1: always primary).
    pub fn active(&self) -> &Screen {
        &self.primary
    }

    /// Resize the terminal to a new cell grid (from a window resize). Resizes
    /// every screen and updates the recorded size; soft-wrap reflow is inc≥3.
    pub fn resize(&mut self, size: GridSize) {
        self.primary.resize(size.cols, size.rows);
        if let Some(alt) = &mut self.alt {
            alt.resize(size.cols, size.rows);
        }
        self.size = size;
    }

    /// Take the queued report-reply bytes (for the io thread → pty writer).
    pub fn take_pending_writes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_writes)
    }

    fn apply_sgr(&mut self, attrs: &[SgrAttr]) {
        let c = &mut self.primary.cursor;
        for a in attrs {
            match *a {
                SgrAttr::Reset => {
                    c.fg = Color::Default;
                    c.bg = Color::Default;
                    c.attrs = CellAttrs::empty();
                }
                SgrAttr::Bold => c.attrs.insert(CellAttrs::BOLD),
                SgrAttr::Faint => c.attrs.insert(CellAttrs::FAINT),
                SgrAttr::Italic => c.attrs.insert(CellAttrs::ITALIC),
                SgrAttr::Underline => c.attrs.insert(CellAttrs::UNDERLINE),
                SgrAttr::Blink => c.attrs.insert(CellAttrs::BLINK),
                SgrAttr::Inverse => c.attrs.insert(CellAttrs::INVERSE),
                SgrAttr::Invisible => c.attrs.insert(CellAttrs::INVISIBLE),
                SgrAttr::Strike => c.attrs.insert(CellAttrs::STRIKETHROUGH),
                SgrAttr::Overline => c.attrs.insert(CellAttrs::OVERLINE),
                SgrAttr::ResetBold => c.attrs.remove(CellAttrs::BOLD | CellAttrs::FAINT),
                SgrAttr::ResetItalic => c.attrs.remove(CellAttrs::ITALIC),
                SgrAttr::ResetUnderline => c.attrs.remove(CellAttrs::UNDERLINE),
                SgrAttr::ResetBlink => c.attrs.remove(CellAttrs::BLINK),
                SgrAttr::ResetInverse => c.attrs.remove(CellAttrs::INVERSE),
                SgrAttr::ResetInvisible => c.attrs.remove(CellAttrs::INVISIBLE),
                SgrAttr::ResetStrike => c.attrs.remove(CellAttrs::STRIKETHROUGH),
                SgrAttr::ResetOverline => c.attrs.remove(CellAttrs::OVERLINE),
                SgrAttr::Fg(col) => c.fg = col,
                SgrAttr::Bg(col) => c.bg = col,
                SgrAttr::DefaultFg => c.fg = Color::Default,
                SgrAttr::DefaultBg => c.bg = Color::Default,
            }
        }
    }
}

impl Handler for Terminal {
    fn print(&mut self, c: char) {
        let autowrap = self.modes.autowrap();
        self.primary.print(c, autowrap);
    }

    fn execute_c0(&mut self, byte: u8) {
        match byte {
            0x07 => {} // BEL — TODO(agent): visual/audible bell (inc≥2)
            0x08 => self.primary.backspace(),
            0x09 => self.primary.tab(1),
            0x0a..=0x0c => {
                if self.modes.linefeed_newline() {
                    self.primary.carriage_return();
                }
                self.primary.index();
            }
            0x0d => self.primary.carriage_return(),
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        self.primary.cursor_up(n);
    }
    fn cursor_down(&mut self, n: u16) {
        self.primary.cursor_down(n);
    }
    fn cursor_forward(&mut self, n: u16) {
        self.primary.cursor_forward(n);
    }
    fn cursor_backward(&mut self, n: u16) {
        self.primary.cursor_backward(n);
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.primary.cursor_position(row, col);
    }
    fn cursor_col_abs(&mut self, col: u16) {
        self.primary.cursor_col_abs(col);
    }
    fn cursor_row_abs(&mut self, row: u16) {
        self.primary.cursor_row_abs(row);
    }

    fn erase_display(&mut self, mode: EraseDisplay) {
        self.primary.erase_display(mode);
    }
    fn erase_line(&mut self, mode: EraseLine) {
        self.primary.erase_line(mode);
    }

    fn set_attributes(&mut self, attrs: &[SgrAttr]) {
        self.apply_sgr(attrs);
    }

    fn set_mode(&mut self, value: u16, ansi: bool, on: bool) {
        self.modes.set(value, ansi, on);
        if value == 25 && !ansi {
            self.primary.cursor.visible = on; // DECTCEM
        }
    }

    fn carriage_return(&mut self) {
        self.primary.carriage_return();
    }
    fn linefeed(&mut self) {
        self.primary.index();
    }
    fn tab(&mut self, n: u16) {
        self.primary.tab(n);
    }
    fn tab_back(&mut self, n: u16) {
        self.primary.tab_back(n);
    }
    fn reverse_index(&mut self) {
        self.primary.reverse_index();
    }
    fn save_cursor(&mut self) {
        self.primary.save_cursor();
    }
    fn restore_cursor(&mut self) {
        self.primary.restore_cursor();
    }
    fn set_tab_stop(&mut self) {
        self.primary.set_tab_stop();
    }
    fn clear_tab_stop(&mut self) {
        self.primary.clear_tab_stop();
    }
    fn clear_all_tab_stops(&mut self) {
        self.primary.clear_all_tab_stops();
    }

    fn full_reset(&mut self) {
        self.primary = Screen::new(self.size.cols, self.size.rows);
        self.modes = ModeState::defaults();
        self.title.clear();
    }

    fn insert_blank_chars(&mut self, n: u16) {
        self.primary.insert_blank_chars(n);
    }
    fn insert_lines(&mut self, n: u16) {
        self.primary.insert_lines(n);
    }
    fn delete_lines(&mut self, n: u16) {
        self.primary.delete_lines(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.primary.delete_chars(n);
    }
    fn scroll_up(&mut self, n: u16) {
        self.primary.scroll_up_region(n);
    }
    fn scroll_down(&mut self, n: u16) {
        self.primary.scroll_down_region(n);
    }
    fn erase_chars(&mut self, n: u16) {
        self.primary.erase_chars(n);
    }
    fn repeat_preceding_char(&mut self, n: u16) {
        let autowrap = self.modes.autowrap();
        self.primary.repeat_preceding_char(n, autowrap);
    }

    fn device_attributes(&mut self, kind: DaKind) {
        match kind {
            // DA1: "I am a VT220 with these features" (matches Ghostty's reply shape).
            DaKind::Primary => self.pending_writes.extend_from_slice(b"\x1b[?62;22c"),
            DaKind::Secondary => self.pending_writes.extend_from_slice(b"\x1b[>1;0;0c"),
        }
    }

    fn device_status_report(&mut self, kind: DsrKind) {
        match kind {
            DsrKind::Status => self.pending_writes.extend_from_slice(b"\x1b[0n"),
            DsrKind::CursorPosition => {
                let row = self.primary.cursor.y + 1;
                let col = self.primary.cursor.x + 1;
                self.pending_writes
                    .extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
            }
        }
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        // OSC 0 (icon+title) / 2 (title): "<code>;<text>".
        let sep = data.iter().position(|&b| b == b';');
        if let Some(i) = sep {
            let code = &data[..i];
            if code == b"0" || code == b"2" {
                self.title = String::from_utf8_lossy(&data[i + 1..]).into_owned();
            }
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let last = self.primary.rows.saturating_sub(1);
        let t = top.saturating_sub(1).min(last);
        let b = if bottom == 0 {
            last
        } else {
            bottom.saturating_sub(1).min(last)
        };
        if t < b {
            self.primary.region = ScrollRegion { top: t, bottom: b };
        }
        self.primary.cursor_position(1, 1); // DECSTBM homes the cursor
    }
}
