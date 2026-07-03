//! [`Terminal`] — the top-level state model. Implements [`noa_vt::Handler`],
//! dispatching parsed operations onto the active [`Screen`] and queuing report
//! replies (DA/DSR) for the pty writer.

use crate::cell::Hyperlink;
use crate::charset::CharsetState;
use crate::cursor::{CursorStyle, ScrollRegion};
use crate::modes::ModeState;
use crate::osc::{
    CwdOsc, HyperlinkOsc, Osc52Policy, ShellIntegrationOsc, ShellIntegrationOscKind,
    TerminalColors, handle_clipboard_osc, handle_color_osc, parse_cwd_osc, parse_hyperlink_osc,
    parse_shell_integration_osc,
};
use crate::screen::Screen;
use crate::search::SearchMatch;
use crate::selection::SelectionPoint;
use noa_core::{CellAttrs, Color, GridSize, Point};
use noa_vt::{
    Charset, CharsetSlot, CursorStyle as VtCursorStyle, DaKind, DsrKind, EraseDisplay, EraseLine,
    Handler, ModeRequest, SgrAttr,
};

pub struct Terminal {
    pub primary: Screen,
    /// Alternate screen — populated in inc≥2.
    pub alt: Option<Screen>,
    pub active_is_alt: bool,
    pub modes: ModeState,
    /// G0/G1 designation + active (GL) slot for `SCS`/`SO`/`SI`.
    charset: CharsetState,
    /// Window title from OSC 0/2 (stored; unused by the inc-1 renderer).
    pub title: String,
    /// Current working directory reported by OSC 7 as a decoded absolute path.
    pub cwd: Option<String>,
    /// OSC 8 hyperlink registry. Cells store indices into this table.
    pub hyperlinks: Vec<Hyperlink>,
    /// OSC 133 shell integration marks recorded at cursor positions.
    pub shell_marks: Vec<ShellIntegrationMark>,
    /// Dynamic colors set through safe OSC 4/10/11/12 sequences.
    pub colors: TerminalColors,
    /// Policy for OSC 52 clipboard writes/queries.
    pub osc52_policy: Osc52Policy,
    pub size: GridSize,
    /// Bytes the terminal must write back to the pty (query replies).
    pub pending_writes: Vec<u8>,
    /// Text payloads accepted by OSC 52 and ready for the app clipboard layer.
    pub pending_clipboard_writes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellIntegrationMarkKind {
    PromptStart,
    InputStart,
    CommandStart,
    CommandEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellIntegrationMark {
    pub kind: ShellIntegrationMarkKind,
    pub point: SelectionPoint,
    pub exit_status: Option<i32>,
}

impl Terminal {
    pub fn new(size: GridSize) -> Self {
        Terminal {
            primary: Screen::new(size.cols, size.rows),
            alt: None,
            active_is_alt: false,
            modes: ModeState::defaults(),
            charset: CharsetState::default(),
            title: String::new(),
            cwd: None,
            hyperlinks: Vec::new(),
            shell_marks: Vec::new(),
            colors: TerminalColors::default(),
            osc52_policy: Osc52Policy::default(),
            size,
            pending_writes: Vec::new(),
            pending_clipboard_writes: Vec::new(),
        }
    }

    /// The active screen.
    pub fn active(&self) -> &Screen {
        if self.active_is_alt {
            self.alt.as_ref().unwrap_or(&self.primary)
        } else {
            &self.primary
        }
    }

    pub fn scrollback_len(&self) -> usize {
        self.active().scrollback_len()
    }

    pub fn viewport_offset(&self) -> usize {
        self.active().viewport_offset()
    }

    pub fn scroll_viewport_up(&mut self, rows: usize) {
        self.active_mut().scroll_viewport_up(rows);
    }

    pub fn scroll_viewport_down(&mut self, rows: usize) {
        self.active_mut().scroll_viewport_down(rows);
    }

    pub fn scroll_viewport_to_top(&mut self) {
        self.active_mut().scroll_viewport_to_top();
    }

    pub fn scroll_viewport_to_bottom(&mut self) {
        self.active_mut().scroll_viewport_to_bottom();
    }

    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        self.active_mut().set_selection(anchor, focus);
    }

    pub fn set_viewport_selection(&mut self, anchor: Point, focus: Point) {
        self.active_mut().set_viewport_selection(anchor, focus);
    }

    pub fn select_word_at_viewport_point(&mut self, point: Point) {
        self.active_mut().select_word_at_viewport_point(point);
    }

    pub fn select_line_at_viewport_point(&mut self, point: Point) {
        self.active_mut().select_line_at_viewport_point(point);
    }

    pub fn clear_selection(&mut self) {
        self.active_mut().clear_selection();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.active().selected_text()
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.active_mut().set_search_query(query);
    }

    pub fn clear_search(&mut self) {
        self.active_mut().clear_search();
    }

    pub fn search_next(&mut self) -> Option<SearchMatch> {
        self.active_mut().search_next()
    }

    pub fn search_previous(&mut self) -> Option<SearchMatch> {
        self.active_mut().search_previous()
    }

    pub fn clear_active_display_and_scrollback(&mut self) {
        if self.active_is_alt {
            let active = self.active_mut();
            active.clear_display();
            active.clear_selection();
            active.clear_search();
        } else {
            self.primary.clear_display();
            self.primary.clear_scrollback();
        }
    }

    pub fn clear_scrollback(&mut self) {
        self.primary.clear_scrollback();
    }

    pub fn select_all(&mut self) {
        self.active_mut().select_all();
    }

    /// Resize the terminal to a new cell grid (from a window resize). Resizes
    /// every screen, reflows soft-wrapped lines, and updates the recorded size.
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

    pub fn take_pending_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_clipboard_writes)
    }

    pub fn set_base_colors(
        &mut self,
        default_fg: noa_core::Rgb,
        default_bg: noa_core::Rgb,
        cursor: noa_core::Rgb,
        palette: [noa_core::Rgb; 256],
    ) {
        self.colors
            .set_base_colors(default_fg, default_bg, cursor, palette);
    }

    fn apply_sgr(&mut self, attrs: &[SgrAttr]) {
        let c = &mut self.active_mut().cursor;
        for a in attrs {
            match *a {
                SgrAttr::Reset => {
                    c.fg = Color::Default;
                    c.bg = Color::Default;
                    c.underline_color = None;
                    c.attrs = CellAttrs::empty();
                }
                SgrAttr::Bold => c.attrs.insert(CellAttrs::BOLD),
                SgrAttr::Faint => c.attrs.insert(CellAttrs::FAINT),
                SgrAttr::Italic => c.attrs.insert(CellAttrs::ITALIC),
                SgrAttr::Underline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::UNDERLINE);
                }
                SgrAttr::DoubleUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                SgrAttr::CurlyUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::CURLY_UNDERLINE);
                }
                SgrAttr::DottedUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DOTTED_UNDERLINE);
                }
                SgrAttr::DashedUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DASHED_UNDERLINE);
                }
                SgrAttr::Blink => c.attrs.insert(CellAttrs::BLINK),
                SgrAttr::Inverse => c.attrs.insert(CellAttrs::INVERSE),
                SgrAttr::Invisible => c.attrs.insert(CellAttrs::INVISIBLE),
                SgrAttr::Strike => c.attrs.insert(CellAttrs::STRIKETHROUGH),
                SgrAttr::Overline => c.attrs.insert(CellAttrs::OVERLINE),
                SgrAttr::ResetBold => c.attrs.remove(CellAttrs::BOLD | CellAttrs::FAINT),
                SgrAttr::ResetItalic => c.attrs.remove(CellAttrs::ITALIC),
                SgrAttr::ResetUnderline => c.attrs.remove(CellAttrs::underline_styles()),
                SgrAttr::ResetBlink => c.attrs.remove(CellAttrs::BLINK),
                SgrAttr::ResetInverse => c.attrs.remove(CellAttrs::INVERSE),
                SgrAttr::ResetInvisible => c.attrs.remove(CellAttrs::INVISIBLE),
                SgrAttr::ResetStrike => c.attrs.remove(CellAttrs::STRIKETHROUGH),
                SgrAttr::ResetOverline => c.attrs.remove(CellAttrs::OVERLINE),
                SgrAttr::Fg(col) => c.fg = col,
                SgrAttr::Bg(col) => c.bg = col,
                SgrAttr::UnderlineColor(col) => c.underline_color = Some(col),
                SgrAttr::DefaultFg => c.fg = Color::Default,
                SgrAttr::DefaultBg => c.bg = Color::Default,
                SgrAttr::DefaultUnderlineColor => c.underline_color = None,
            }
        }
    }

    fn active_mut(&mut self) -> &mut Screen {
        if self.active_is_alt {
            let cols = self.size.cols;
            let rows = self.size.rows;
            self.alt
                .get_or_insert_with(|| Screen::alternate(cols, rows))
        } else {
            &mut self.primary
        }
    }

    fn set_current_hyperlink(&mut self, hyperlink: Hyperlink) {
        let id = self
            .hyperlinks
            .iter()
            .position(|existing| existing == &hyperlink)
            .unwrap_or_else(|| {
                self.hyperlinks.push(hyperlink);
                self.hyperlinks.len() - 1
            });
        self.active_mut().cursor.hyperlink = Some(id);
    }

    fn clear_current_hyperlink(&mut self) {
        self.active_mut().cursor.hyperlink = None;
    }

    fn record_shell_mark(&mut self, kind: ShellIntegrationMarkKind, exit_status: Option<i32>) {
        let screen = self.active();
        let point = SelectionPoint::new(
            screen.cursor.x,
            screen.scrollback_len() + screen.cursor.y as usize,
        );
        self.shell_marks.push(ShellIntegrationMark {
            kind,
            point,
            exit_status,
        });
    }

    fn push_dcs_response(&mut self, body: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes.extend_from_slice(body);
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn push_decrqss_response(&mut self, valid: bool, request: &[u8], setting: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        if valid {
            self.pending_writes.extend_from_slice(b"1$r");
            self.pending_writes.extend_from_slice(setting);
        } else {
            self.pending_writes.extend_from_slice(b"0$r");
            self.pending_writes.extend_from_slice(request);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn handle_decrqss(&mut self, request: &[u8]) {
        match request {
            b"m" => {
                let setting = self.current_sgr_report();
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b" q" => {
                let setting = format!("{} q", self.cursor_style_number());
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"r" => {
                let region = self.active().region;
                let setting = format!("{};{}r", region.top + 1, region.bottom + 1);
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"s" => {
                let (left, right) = self
                    .active()
                    .horizontal_margins
                    .map(|m| (m.left + 1, m.right + 1))
                    .unwrap_or((1, self.size.cols));
                let setting = format!("{left};{right}s");
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            _ => self.push_decrqss_response(false, request, &[]),
        }
    }

    fn cursor_style_number(&self) -> u8 {
        match self.active().cursor.style {
            CursorStyle::BlinkingBlock => 1,
            CursorStyle::SteadyBlock => 2,
            CursorStyle::BlinkingUnderline => 3,
            CursorStyle::SteadyUnderline => 4,
            CursorStyle::BlinkingBar => 5,
            CursorStyle::SteadyBar => 6,
        }
    }

    fn current_sgr_report(&self) -> String {
        let c = &self.active().cursor;
        let mut params = vec!["0".to_string()];
        if c.attrs.contains(CellAttrs::BOLD) {
            params.push("1".to_string());
        }
        if c.attrs.contains(CellAttrs::FAINT) {
            params.push("2".to_string());
        }
        if c.attrs.contains(CellAttrs::ITALIC) {
            params.push("3".to_string());
        }
        if c.attrs.contains(CellAttrs::UNDERLINE) {
            params.push("4".to_string());
        } else if c.attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
            params.push("21".to_string());
        } else if c.attrs.contains(CellAttrs::CURLY_UNDERLINE) {
            params.push("4:3".to_string());
        } else if c.attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
            params.push("4:4".to_string());
        } else if c.attrs.contains(CellAttrs::DASHED_UNDERLINE) {
            params.push("4:5".to_string());
        }
        if c.attrs.contains(CellAttrs::BLINK) {
            params.push("5".to_string());
        }
        if c.attrs.contains(CellAttrs::INVERSE) {
            params.push("7".to_string());
        }
        if c.attrs.contains(CellAttrs::INVISIBLE) {
            params.push("8".to_string());
        }
        if c.attrs.contains(CellAttrs::STRIKETHROUGH) {
            params.push("9".to_string());
        }
        if c.attrs.contains(CellAttrs::OVERLINE) {
            params.push("53".to_string());
        }
        push_color_params(&mut params, 30, 90, 38, c.fg);
        push_color_params(&mut params, 40, 100, 48, c.bg);
        if let Some(color) = c.underline_color {
            push_color_params(&mut params, 0, 0, 58, color);
        }
        format!("{}m", params.join(";"))
    }

    fn handle_xtgettcap(&mut self, payload: &[u8]) {
        for encoded_name in payload
            .split(|&b| b == b';')
            .filter(|name| !name.is_empty())
        {
            let Some(name) = decode_xtgettcap_name(encoded_name) else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
                continue;
            };
            let value = match name.as_slice() {
                b"TN" => Some(b"noa".as_slice()),
                b"RGB" => Some(b"8:8:8".as_slice()),
                b"Co" => Some(b"256".as_slice()),
                _ => None,
            };
            if let Some(value) = value {
                self.push_xtgettcap_response(true, encoded_name, value);
            } else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
            }
        }
    }

    fn push_xtgettcap_response(&mut self, valid: bool, encoded_name: &[u8], value: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes
            .extend_from_slice(if valid { b"1+r" } else { b"0+r" });
        self.pending_writes.extend_from_slice(encoded_name);
        if valid {
            self.pending_writes.push(b'=');
            push_hex_bytes(&mut self.pending_writes, value);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn enter_alt_screen(&mut self, clear: bool) {
        if clear || self.alt.is_none() {
            let mut alt = Screen::alternate(self.size.cols, self.size.rows);
            alt.cursor.visible = self.modes.cursor_visible();
            self.alt = Some(alt);
        } else if let Some(alt) = &mut self.alt {
            alt.cursor.visible = self.modes.cursor_visible();
        }
        self.active_is_alt = true;
        self.primary.clear_selection();
        self.primary.clear_search();
        if let Some(alt) = &mut self.alt {
            alt.clear_selection();
            alt.clear_search();
        }
    }

    fn leave_alt_screen(&mut self, restore_cursor: bool, clear_alt: bool) {
        let was_alt = self.active_is_alt;
        self.active_is_alt = false;
        self.primary.scroll_viewport_to_bottom();
        self.primary.cursor.visible = self.modes.cursor_visible();
        if restore_cursor {
            self.primary.restore_cursor();
        }
        if clear_alt && was_alt {
            let mut alt = Screen::alternate(self.size.cols, self.size.rows);
            alt.cursor.visible = self.modes.cursor_visible();
            self.alt = Some(alt);
        }
        if was_alt {
            self.primary.clear_selection();
            self.primary.clear_search();
            if let Some(alt) = &mut self.alt {
                alt.clear_selection();
                alt.clear_search();
            }
        }
    }
}

fn push_color_params(
    params: &mut Vec<String>,
    base: u16,
    bright_base: u16,
    extended: u16,
    color: Color,
) {
    match color {
        Color::Default => {}
        Color::Palette(index) if index < 8 && base != 0 => {
            params.push((base + index as u16).to_string());
        }
        Color::Palette(index) if index < 16 && bright_base != 0 => {
            params.push((bright_base + index as u16 - 8).to_string());
        }
        Color::Palette(index) => params.push(format!("{extended};5;{index}")),
        Color::Rgb(rgb) => params.push(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

fn decode_xtgettcap_name(encoded: &[u8]) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.chunks_exact(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_hex_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
}

impl Handler for Terminal {
    fn print(&mut self, c: char) {
        let autowrap = self.modes.autowrap();
        let c = self.charset.translate(c);
        self.active_mut().print(c, autowrap);
    }

    fn execute_c0(&mut self, byte: u8) {
        match byte {
            0x0e => return self.locking_shift(CharsetSlot::G1), // SO
            0x0f => return self.locking_shift(CharsetSlot::G0), // SI
            _ => {}
        }
        let linefeed_newline = self.modes.linefeed_newline();
        let screen = self.active_mut();
        match byte {
            0x07 => {} // BEL has no grid-state side effect.
            0x08 => screen.backspace(),
            0x09 => screen.tab(1),
            0x0a..=0x0c => {
                if linefeed_newline {
                    screen.carriage_return();
                }
                screen.index();
            }
            0x0d => screen.carriage_return(),
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        self.active_mut().cursor_up(n);
    }
    fn cursor_down(&mut self, n: u16) {
        self.active_mut().cursor_down(n);
    }
    fn cursor_forward(&mut self, n: u16) {
        self.active_mut().cursor_forward(n);
    }
    fn cursor_backward(&mut self, n: u16) {
        self.active_mut().cursor_backward(n);
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.active_mut().cursor_position(row, col);
    }
    fn cursor_col_abs(&mut self, col: u16) {
        self.active_mut().cursor_col_abs(col);
    }
    fn cursor_row_abs(&mut self, row: u16) {
        self.active_mut().cursor_row_abs(row);
    }

    fn erase_display(&mut self, mode: EraseDisplay) {
        self.active_mut().erase_display(mode);
    }
    fn erase_line(&mut self, mode: EraseLine) {
        self.active_mut().erase_line(mode);
    }

    fn set_attributes(&mut self, attrs: &[SgrAttr]) {
        self.apply_sgr(attrs);
    }

    fn set_cursor_style(&mut self, style: VtCursorStyle) {
        self.active_mut().cursor.style = match style {
            VtCursorStyle::BlinkingBlock => CursorStyle::BlinkingBlock,
            VtCursorStyle::SteadyBlock => CursorStyle::SteadyBlock,
            VtCursorStyle::BlinkingUnderline => CursorStyle::BlinkingUnderline,
            VtCursorStyle::SteadyUnderline => CursorStyle::SteadyUnderline,
            VtCursorStyle::BlinkingBar => CursorStyle::BlinkingBar,
            VtCursorStyle::SteadyBar => CursorStyle::SteadyBar,
        };
    }

    fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        if self.modes.left_right_margin() {
            self.active_mut().set_horizontal_margins(left, right);
        }
    }

    fn set_application_keypad(&mut self, on: bool) {
        self.modes.set(66, false, on);
    }

    fn request_mode(&mut self, request: ModeRequest) {
        let state = match (request.value, request.ansi) {
            (20, true)
            | (1, false)
            | (7, false)
            | (25, false)
            | (47, false)
            | (66, false)
            | (69, false)
            | (1004, false)
            | (1000, false)
            | (1002, false)
            | (1003, false)
            | (1006, false)
            | (1047, false)
            | (1048, false)
            | (1049, false)
            | (2026, false)
            | (2004, false) => {
                if self.modes.get(request.value, request.ansi) {
                    1
                } else {
                    2
                }
            }
            _ => 0,
        };
        if request.ansi {
            self.pending_writes
                .extend_from_slice(format!("\x1b[{};{}$y", request.value, state).as_bytes());
        } else {
            self.pending_writes
                .extend_from_slice(format!("\x1b[?{};{}$y", request.value, state).as_bytes());
        }
    }

    fn designate_charset(&mut self, slot: CharsetSlot, set: Charset) {
        self.charset.designate(slot, set);
    }

    fn locking_shift(&mut self, slot: CharsetSlot) {
        self.charset.shift(slot);
    }

    fn set_mode(&mut self, value: u16, ansi: bool, on: bool) {
        self.modes.set(value, ansi, on);
        if !ansi {
            match value {
                25 => self.active_mut().cursor.visible = on, // DECTCEM
                69 => {
                    if on {
                        self.active_mut().enable_horizontal_margins();
                    } else {
                        self.active_mut().disable_horizontal_margins();
                    }
                }
                47 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, false);
                    }
                }
                1047 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, true);
                    }
                }
                1048 => {
                    if on {
                        self.active_mut().save_cursor();
                    } else {
                        self.active_mut().restore_cursor();
                    }
                }
                1049 => {
                    if on {
                        self.primary.save_cursor();
                        self.enter_alt_screen(true);
                    } else {
                        self.leave_alt_screen(true, true);
                    }
                }
                _ => {}
            }
        }
    }

    fn carriage_return(&mut self) {
        self.active_mut().carriage_return();
    }
    fn linefeed(&mut self) {
        self.active_mut().index();
    }
    fn tab(&mut self, n: u16) {
        self.active_mut().tab(n);
    }
    fn tab_back(&mut self, n: u16) {
        self.active_mut().tab_back(n);
    }
    fn reverse_index(&mut self) {
        self.active_mut().reverse_index();
    }
    fn save_cursor(&mut self) {
        self.active_mut().save_cursor();
    }
    fn restore_cursor(&mut self) {
        self.active_mut().restore_cursor();
    }
    fn set_tab_stop(&mut self) {
        self.active_mut().set_tab_stop();
    }
    fn clear_tab_stop(&mut self) {
        self.active_mut().clear_tab_stop();
    }
    fn clear_all_tab_stops(&mut self) {
        self.active_mut().clear_all_tab_stops();
    }

    fn full_reset(&mut self) {
        self.primary = Screen::new(self.size.cols, self.size.rows);
        self.alt = None;
        self.active_is_alt = false;
        self.modes = ModeState::defaults();
        self.charset = CharsetState::default();
        self.title.clear();
        self.cwd = None;
        self.hyperlinks.clear();
        self.shell_marks.clear();
        self.colors.reset_dynamic_overrides();
        self.pending_clipboard_writes.clear();
        self.clear_selection();
        self.clear_search();
    }

    fn insert_blank_chars(&mut self, n: u16) {
        self.active_mut().insert_blank_chars(n);
    }
    fn insert_lines(&mut self, n: u16) {
        self.active_mut().insert_lines(n);
    }
    fn delete_lines(&mut self, n: u16) {
        self.active_mut().delete_lines(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.active_mut().delete_chars(n);
    }
    fn scroll_up(&mut self, n: u16) {
        self.active_mut().scroll_up_region(n);
    }
    fn scroll_down(&mut self, n: u16) {
        self.active_mut().scroll_down_region(n);
    }
    fn erase_chars(&mut self, n: u16) {
        self.active_mut().erase_chars(n);
    }
    fn repeat_preceding_char(&mut self, n: u16) {
        let autowrap = self.modes.autowrap();
        self.active_mut().repeat_preceding_char(n, autowrap);
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
                let row = self.active().cursor.y + 1;
                let col = self.active().cursor.x + 1;
                self.pending_writes
                    .extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
            }
        }
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        if handle_color_osc(data, &mut self.colors, &mut self.pending_writes) {
            return;
        }
        if handle_clipboard_osc(
            data,
            &self.osc52_policy,
            &mut self.pending_clipboard_writes,
            &mut self.pending_writes,
        ) {
            return;
        }
        if let Some(action) = parse_hyperlink_osc(data) {
            match action {
                HyperlinkOsc::Start(hyperlink) => self.set_current_hyperlink(hyperlink),
                HyperlinkOsc::End => self.clear_current_hyperlink(),
                HyperlinkOsc::Malformed => {}
            }
            return;
        }
        if let Some(action) = parse_cwd_osc(data) {
            if let CwdOsc::Set(cwd) = action {
                self.cwd = Some(cwd);
            }
            return;
        }
        if let Some(action) = parse_shell_integration_osc(data) {
            if let ShellIntegrationOsc::Mark { kind, exit_status } = action {
                let kind = match kind {
                    ShellIntegrationOscKind::PromptStart => ShellIntegrationMarkKind::PromptStart,
                    ShellIntegrationOscKind::InputStart => ShellIntegrationMarkKind::InputStart,
                    ShellIntegrationOscKind::CommandStart => ShellIntegrationMarkKind::CommandStart,
                    ShellIntegrationOscKind::CommandEnd => ShellIntegrationMarkKind::CommandEnd,
                };
                self.record_shell_mark(kind, exit_status);
            }
            return;
        }

        // OSC 0 (icon+title) / 2 (title): "<code>;<text>".
        let sep = data.iter().position(|&b| b == b';');
        if let Some(i) = sep {
            let code = &data[..i];
            if code == b"0" || code == b"2" {
                self.title = String::from_utf8_lossy(&data[i + 1..]).into_owned();
            }
        }
    }

    fn dcs_dispatch(&mut self, data: &[u8]) {
        if let Some(request) = data.strip_prefix(b"$q") {
            self.handle_decrqss(request);
        } else if let Some(payload) = data.strip_prefix(b"+q") {
            self.handle_xtgettcap(payload);
        } else if data == b">q" {
            self.push_dcs_response(format!(">|noa {}", env!("CARGO_PKG_VERSION")).as_bytes());
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let screen = self.active_mut();
        let last = screen.rows.saturating_sub(1);
        let t = top.saturating_sub(1).min(last);
        let b = if bottom == 0 {
            last
        } else {
            bottom.saturating_sub(1).min(last)
        };
        if t < b {
            screen.region = ScrollRegion { top: t, bottom: b };
            screen.cursor_position(1, 1); // DECSTBM homes the cursor after a valid region.
        }
    }
}
