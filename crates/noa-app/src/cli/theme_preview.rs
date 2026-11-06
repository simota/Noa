//! `+list-themes`'s interactive preview, like Ghostty 1.4.0's: the theme list
//! on the left and the selected theme drawn on the right, with Ghostty's keys
//! (its `F1` help lists them). Noa's themes are compiled in, so there is no
//! theme file path to show or copy (Ghostty's `C`).
//!
//! [`Preview`] holds the state, [`Preview::handle`] applies a [`Key`], and
//! [`Preview::render`] draws a frame as one string of escape sequences, so
//! everything but the terminal I/O in [`run`] is testable without a tty.

use std::io::{self, Write};
use std::path::PathBuf;

use noa_core::Rgb;
use noa_theme::ThemeDef;
use unicode_width::UnicodeWidthChar;

/// Columns of the theme list; the preview takes the rest.
const LIST_WIDTH: usize = 32;
/// Themes moved by `PgUp` / `PgDown` / `^U` / `^D`.
const PAGE: usize = 20;
const SEARCH_WIDTH: usize = 60;

const LIST: Style = Style::plain(
    Some(Rgb::new(0x00, 0x00, 0x00)),
    Some(Rgb::new(0xff, 0xff, 0xff)),
);
const LIST_SELECTED: Style = Style::plain(
    Some(Rgb::new(0x00, 0xaa, 0x00)),
    Some(Rgb::new(0xaa, 0xaa, 0xaa)),
);
const DEFAULT: Style = Style::plain(None, None);

const HELP: &[(&str, &str)] = &[
    ("^C, q, ESC", "Quit."),
    ("F1, ?, ^H", "Toggle help window."),
    ("f", "Cycle through theme filters."),
    ("k, ↑", "Move up 1 theme."),
    ("ScrollUp", "Move up 1 theme."),
    ("PgUp, ^U", "Move up 20 themes."),
    ("j, ↓", "Move down 1 theme."),
    ("ScrollDown", "Move down 1 theme."),
    ("PgDown, ^D", "Move down 20 themes."),
    ("h, x", "Show palette numbers in hexadecimal."),
    ("d", "Show palette numbers in decimal."),
    ("c", "Copy theme name to the clipboard."),
    ("Home, g", "Go to the start of the list."),
    ("End, G", "Go to the end of the list."),
    ("/", "Start search."),
    ("^X, ^/", "Clear search."),
    ("⏎", "Save theme or close search window."),
    ("w", "Write theme to auto config file."),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    F1,
    Enter,
    Esc,
    Backspace,
    WheelUp,
    WheelDown,
}

/// What [`run`] has to do after a key, beyond redrawing.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Quit,
    /// Copy the text to the clipboard (OSC 52).
    Copy(String),
    /// Write `theme = <name>` to the auto theme file.
    WriteAutoTheme(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    None,
    Help,
    Search,
    Save,
}

pub(super) struct Preview {
    themes: &'static [(&'static str, ThemeDef)],
    /// Indices into `themes`, sorted by name ignoring case as Ghostty lists
    /// them (the catalog itself is in byte order).
    order: Vec<usize>,
    filter: Filter,
    query: String,
    /// Indices into `themes` that pass the filter and the search.
    visible: Vec<usize>,
    /// Index into `visible`.
    selected: usize,
    /// First `visible` index on screen.
    top: usize,
    overlay: Overlay,
    hex: bool,
    /// The theme `w` wrote and the outcome, shown in the save window while
    /// that theme is selected.
    status: Option<(String, String)>,
}

impl Preview {
    pub(super) fn new(themes: &'static [(&'static str, ThemeDef)]) -> Self {
        let mut order: Vec<usize> = (0..themes.len()).collect();
        order.sort_by_cached_key(|&index| themes[index].0.to_lowercase());
        let mut preview = Self {
            themes,
            order,
            filter: Filter::All,
            query: String::new(),
            visible: Vec::new(),
            selected: 0,
            top: 0,
            overlay: Overlay::None,
            hex: false,
            status: None,
        };
        preview.refilter();
        preview
    }

    fn selected_theme(&self) -> Option<&'static ThemeDef> {
        let themes = self.themes;
        self.visible
            .get(self.selected)
            .map(|&index| &themes[index].1)
    }

    /// Rebuild the list after the filter or the search changed. The selected
    /// theme stays selected when it is still listed; otherwise the selection
    /// keeps its row, clamped to the new list.
    fn refilter(&mut self) {
        let current = self.visible.get(self.selected).copied();
        let query: Vec<char> = self.query.to_lowercase().chars().collect();
        self.visible = self
            .order
            .iter()
            .copied()
            .filter(|&index| {
                let (name, theme) = &self.themes[index];
                let variant = super::theme_variant(theme);
                let shown = match self.filter {
                    Filter::All => true,
                    Filter::Dark => variant == "dark",
                    Filter::Light => variant == "light",
                };
                shown && is_subsequence(&query, name)
            })
            .collect();
        self.selected = current
            .and_then(|current| self.visible.iter().position(|&index| index == current))
            .unwrap_or_else(|| self.selected.min(self.visible.len().saturating_sub(1)));
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub(super) fn handle(&mut self, key: Key) -> Action {
        if self.overlay == Overlay::Search {
            match key {
                Key::Char(c) => {
                    self.query.push(c);
                    self.refilter();
                    return Action::None;
                }
                Key::Backspace | Key::Ctrl('h') => {
                    self.query.pop();
                    self.refilter();
                    return Action::None;
                }
                Key::Enter | Key::Esc => {
                    self.overlay = Overlay::None;
                    return Action::None;
                }
                _ => {}
            }
        }
        match key {
            Key::Ctrl('c') => return Action::Quit,
            Key::Esc if self.overlay != Overlay::None => self.set_overlay(Overlay::None),
            Key::Esc | Key::Char('q') => return Action::Quit,
            Key::F1 | Key::Char('?') | Key::Ctrl('h') => self.toggle_overlay(Overlay::Help),
            Key::Char('f') => {
                self.filter = match self.filter {
                    Filter::All => Filter::Dark,
                    Filter::Dark => Filter::Light,
                    Filter::Light => Filter::All,
                };
                self.refilter();
            }
            Key::Char('k') | Key::Up | Key::WheelUp => self.move_by(-1),
            Key::PageUp | Key::Ctrl('u') => self.move_by(-(PAGE as isize)),
            Key::Char('j') | Key::Down | Key::WheelDown => self.move_by(1),
            Key::PageDown | Key::Ctrl('d') => self.move_by(PAGE as isize),
            Key::Char('h' | 'x') => self.hex = true,
            Key::Char('d') => self.hex = false,
            Key::Char('c') => {
                if let Some(theme) = self.selected_theme() {
                    return Action::Copy(theme.name.to_string());
                }
            }
            Key::Home | Key::Char('g') => self.selected = 0,
            Key::End | Key::Char('G') => self.selected = self.visible.len().saturating_sub(1),
            Key::Char('/') => self.set_overlay(Overlay::Search),
            Key::Ctrl('x' | '/') => {
                self.query.clear();
                self.refilter();
            }
            Key::Enter if self.selected_theme().is_some() => self.toggle_overlay(Overlay::Save),
            Key::Char('w') if self.overlay == Overlay::Save => {
                if let Some(theme) = self.selected_theme() {
                    return Action::WriteAutoTheme(theme.name.to_string());
                }
            }
            _ => {}
        }
        Action::None
    }

    fn set_overlay(&mut self, overlay: Overlay) {
        self.overlay = overlay;
        self.status = None;
    }

    fn toggle_overlay(&mut self, overlay: Overlay) {
        self.set_overlay(if self.overlay == overlay {
            Overlay::None
        } else {
            overlay
        });
    }

    /// One frame for a `cols` × `rows` screen, wrapped in a synchronized
    /// update.
    pub(super) fn render(&mut self, cols: usize, rows: usize) -> String {
        if self.selected < self.top {
            self.top = self.selected;
        } else if rows > 0 && self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        let mut canvas = Canvas::new(cols, rows);
        let list_width = LIST_WIDTH.min(cols);
        for row in 0..rows {
            let index = self.top + row;
            let Some(&theme) = self.visible.get(index) else {
                canvas.fill(0, row, list_width, 1, LIST);
                continue;
            };
            let name = self.themes[theme].0;
            if index == self.selected {
                canvas.fill(0, row, list_width, 1, LIST_SELECTED);
                let marker = list_width.saturating_sub(1);
                canvas.put(0, row, &format!("❯ {name}"), LIST_SELECTED, marker);
                canvas.put(marker, row, "❮", LIST_SELECTED, list_width);
            } else {
                canvas.fill(0, row, list_width, 1, LIST);
                canvas.put(2, row, name, LIST, list_width);
            }
        }
        match self.selected_theme() {
            Some(theme) => draw_theme(&mut canvas, list_width, theme, self.hex),
            None => canvas.fill(list_width, 0, cols - list_width, rows, DEFAULT),
        }
        let mut cursor = None;
        match self.overlay {
            Overlay::None => {}
            Overlay::Help => {
                let lines: Vec<String> = std::iter::once(String::new())
                    .chain(
                        HELP.iter()
                            .map(|(keys, what)| format!("  {keys:<13}— {what}")),
                    )
                    .collect();
                draw_box(&mut canvas, &lines, 58);
            }
            Overlay::Save => {
                if let Some(theme) = self.selected_theme() {
                    let mut lines = save_lines(theme.name);
                    if let Some((_, status)) =
                        self.status.as_ref().filter(|(name, _)| name == theme.name)
                    {
                        lines.extend([String::new(), format!("  {status}")]);
                    }
                    let width = lines.iter().map(|line| text_width(line)).max().unwrap_or(0);
                    draw_box(&mut canvas, &lines, width);
                }
            }
            Overlay::Search => {
                let width = SEARCH_WIDTH.min(cols);
                let (x, y) = ((cols - width) / 2, rows.saturating_sub(5));
                canvas.frame(x, y, width, 3, LIST);
                let end = canvas.put(
                    x + 1,
                    y + 1,
                    &self.query,
                    LIST,
                    (x + width).saturating_sub(1),
                );
                cursor = Some((end, y + 1));
            }
        }
        let mut out = canvas.to_ansi();
        match cursor {
            Some((x, y)) if y < rows && x < cols => {
                out.push_str(&format!("\x1b[{};{}H\x1b[?25h", y + 1, x + 1));
            }
            _ => out.push_str("\x1b[?25l"),
        }
        out.push_str("\x1b[?2026l");
        out
    }

    pub(super) fn set_status(&mut self, name: String, status: String) {
        self.status = Some((name, status));
    }
}

/// Case-insensitive subsequence match: every query character appears in
/// `name`, in order.
fn is_subsequence(query: &[char], name: &str) -> bool {
    let mut name = name.chars().flat_map(char::to_lowercase);
    query.iter().all(|wanted| name.any(|c| c == *wanted))
}

fn save_lines(name: &str) -> Vec<String> {
    [
        "",
        "  To apply this theme, add the following line to your Noa configuration:",
        "",
        &format!("  theme = {name}"),
        "",
        "  Save the configuration file and then reload it to apply the new theme.",
        "",
        "  Or press 'w' to write an auto theme file to Noa's config directory.",
        "  Then add the following line to your Noa configuration and reload:",
        "",
        "  config-file = ?auto/theme",
        "",
        "  For more details on the theme option, run:",
        "",
        "  noa +explain-config theme",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect()
}

/// A bordered window of `lines`, `width` columns inside, centered.
fn draw_box(canvas: &mut Canvas, lines: &[String], width: usize) {
    let width = (width + 2).min(canvas.cols);
    let height = (lines.len() + 2).min(canvas.rows);
    let x = (canvas.cols - width) / 2;
    let y = (canvas.rows - height) / 2;
    canvas.frame(x, y, width, height, LIST);
    for (row, line) in lines.iter().take(height.saturating_sub(2)).enumerate() {
        canvas.put(
            x + 1,
            y + 1 + row,
            line,
            LIST,
            (x + width).saturating_sub(1),
        );
    }
}

/// Noa's own sample: a shell session and a short program, colored with the
/// theme's palette.
fn draw_theme(canvas: &mut Canvas, x0: usize, theme: &ThemeDef, hex: bool) {
    let bg = Some(theme.default_bg);
    let base = Style::plain(Some(theme.default_fg), bg);
    let color = |index: usize| Style::plain(Some(theme.palette[index]), bg);
    let bold = |index: usize| Style {
        bold: true,
        ..color(index)
    };
    let width = canvas.cols - x0;
    canvas.fill(x0, 0, width, canvas.rows, base);

    let name_width = text_width(theme.name);
    let name_x = x0 + width.saturating_sub(name_width).div_ceil(2);
    let title = Style {
        bold: true,
        italic: true,
        ..base
    };
    canvas.put(name_x, 1, theme.name, title, canvas.cols);

    for (row, first) in [(4, 0), (7, 8)] {
        for column in 0..8 {
            let index = first + column;
            let x = x0 + column * 8;
            let label = if hex {
                format!("{:>3} ", format!("{index:02x}"))
            } else {
                format!("{index:>3} ")
            };
            canvas.put(x, row, &label, base, canvas.cols);
            for swatch_row in [row, row + 1] {
                canvas.put(x + 4, swatch_row, "████", color(index), canvas.cols);
            }
        }
    }

    let selection = Style::plain(Some(theme.selection_fg), Some(theme.selection_bg));
    let gutter = color(8);
    let code: [&[(&str, Style)]; 9] = [
        &[("/// Counts the words in `text`.", color(8))],
        &[
            ("fn ", color(5)),
            ("count_words", color(4)),
            ("(text: ", base),
            ("&str", color(6)),
            (") -> ", base),
            ("usize", color(6)),
            (" {", base),
        ],
        &[
            ("    let ", color(5)),
            ("limit = ", base),
            ("80", color(3)),
            (";", base),
        ],
        &[("    text.split_whitespace().take(limit).count()", base)],
        &[("}", base)],
        &[],
        &[("fn ", color(5)), ("main", color(4)), ("() {", base)],
        &[
            ("    println!", color(6)),
            ("(", base),
            ("\"{} words\"", color(2)),
            (", count_words(", base),
            ("\"hello from noa\"", color(2)),
            ("));", base),
        ],
        &[("}", base)],
    ];
    let mut lines: Vec<Vec<(String, Style)>> = vec![
        vec![
            ("  ".into(), base),
            ("~/src/noa".into(), color(4)),
            (" on ".into(), base),
            ("main".into(), bold(5)),
            (" ❯ ".into(), color(2)),
            ("cargo test -p noa-vt".into(), base),
        ],
        vec![],
        vec![
            ("     ".into(), base),
            ("Compiling".into(), bold(2)),
            (" noa-vt v0.1.0".into(), base),
        ],
        vec![
            ("      ".into(), base),
            ("Finished".into(), bold(2)),
            (" test profile in 2.41s".into(), base),
        ],
        vec![
            ("  ".into(), base),
            ("warning".into(), bold(3)),
            (": unused variable: ".into(), base),
            ("`cols`".into(), Style { bold: true, ..base }),
        ],
        vec![
            ("  ".into(), base),
            ("error".into(), bold(1)),
            (": test failed, to rerun pass ".into(), base),
            ("`--lib`".into(), Style { bold: true, ..base }),
        ],
        vec![],
    ];
    for (number, spans) in code.iter().enumerate() {
        let mut line = vec![(format!("  {:>3} │ ", number + 1), gutter)];
        line.extend(spans.iter().map(|(text, style)| (text.to_string(), *style)));
        lines.push(line);
    }
    lines.extend([
        vec![],
        vec![
            ("  ".into(), base),
            ("selected text".into(), selection),
            ("  cursor ".into(), base),
            ("█".into(), Style::plain(Some(theme.cursor), bg)),
        ],
    ]);
    for (row, spans) in lines.iter().enumerate() {
        let mut x = x0;
        for (text, style) in spans {
            x = canvas.put(x, 10 + row, text, *style, canvas.cols);
        }
    }
}

fn text_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    /// `None` is the terminal's default color.
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    bold: bool,
    italic: bool,
}

impl Style {
    const fn plain(fg: Option<Rgb>, bg: Option<Rgb>) -> Self {
        Self {
            fg,
            bg,
            bold: false,
            italic: false,
        }
    }

    fn sgr(self) -> String {
        let mut sgr = String::from("\x1b[0");
        match self.fg {
            Some(c) => sgr.push_str(&format!(";38;2;{};{};{}", c.r, c.g, c.b)),
            None => sgr.push_str(";39"),
        }
        match self.bg {
            Some(c) => sgr.push_str(&format!(";48;2;{};{};{}", c.r, c.g, c.b)),
            None => sgr.push_str(";49"),
        }
        if self.bold {
            sgr.push_str(";1");
        }
        if self.italic {
            sgr.push_str(";3");
        }
        sgr.push('m');
        sgr
    }
}

#[derive(Clone, Copy)]
struct Cell {
    /// `None` is the second column of a wide character.
    ch: Option<char>,
    style: Style,
}

struct Canvas {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
}

impl Canvas {
    fn new(cols: usize, rows: usize) -> Self {
        let blank = Cell {
            ch: Some(' '),
            style: DEFAULT,
        };
        Self {
            cols,
            rows,
            cells: vec![blank; cols * rows],
        }
    }

    fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, style: Style) {
        for row in y..(y + height).min(self.rows) {
            for column in x..(x + width).min(self.cols) {
                self.cells[row * self.cols + column] = Cell {
                    ch: Some(' '),
                    style,
                };
            }
        }
    }

    /// Write `text` from column `x`, stopping before column `end` (and the
    /// screen's edge). Returns the column after the last character written.
    fn put(&mut self, mut x: usize, y: usize, text: &str, style: Style, end: usize) -> usize {
        let end = end.min(self.cols);
        if y >= self.rows {
            return x;
        }
        for c in text.chars() {
            let width = c.width().unwrap_or(0);
            if width == 0 {
                continue;
            }
            if x + width > end {
                break;
            }
            let at = y * self.cols + x;
            self.cells[at] = Cell { ch: Some(c), style };
            if width == 2 {
                self.cells[at + 1] = Cell { ch: None, style };
            }
            x += width;
        }
        x
    }

    /// A rounded border around a `width` × `height` area, blank inside.
    fn frame(&mut self, x: usize, y: usize, width: usize, height: usize, style: Style) {
        if width < 2 || height < 2 {
            return;
        }
        self.fill(x, y, width, height, style);
        let inner = "─".repeat(width - 2);
        self.put(x, y, &format!("╭{inner}╮"), style, x + width);
        self.put(x, y + height - 1, &format!("╰{inner}╯"), style, x + width);
        for row in y + 1..y + height - 1 {
            self.put(x, row, "│", style, x + 1);
            self.put(x + width - 1, row, "│", style, x + width);
        }
    }

    fn to_ansi(&self) -> String {
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        for row in 0..self.rows {
            out.push_str(&format!("\x1b[{};1H", row + 1));
            let mut current = None;
            for cell in &self.cells[row * self.cols..(row + 1) * self.cols] {
                let Some(ch) = cell.ch else {
                    continue;
                };
                if current != Some(cell.style) {
                    out.push_str(&cell.style.sgr());
                    current = Some(cell.style);
                }
                out.push(ch);
            }
        }
        out.push_str("\x1b[0m");
        out
    }
}

/// Decodes keys from terminal reads. A read can end inside a key's escape
/// sequence or UTF-8 encoding, so an unfinished tail waits for the next read;
/// a lone ESC is the Esc key only once no more input follows it ([`flush`]).
///
/// [`flush`]: KeyReader::flush
#[derive(Default)]
pub(super) struct KeyReader {
    pending: Vec<u8>,
    /// Pauses `pending` has waited through.
    pauses: usize,
}

/// Pauses (of [`run`]'s 100 ms poll) an unfinished sequence other than a
/// lone ESC may wait for its rest before it is dropped: long enough for
/// bytes that arrive late, short enough that it does not eat later keys.
const STALE_PAUSES: usize = 10;

impl KeyReader {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut i = 0;
        while let Some((key, len)) = next_key(&self.pending[i..]) {
            keys.extend(key);
            i += len;
        }
        self.pending.drain(..i);
        self.pauses = 0;
        keys
    }

    /// No input followed: a held lone ESC was the Esc key. Any other
    /// unfinished sequence waits a little longer, since its rest can arrive
    /// late (over ssh, say), then is dropped.
    pub(super) fn flush(&mut self) -> Option<Key> {
        if self.pending == [0x1b] {
            self.pending.clear();
            return Some(Key::Esc);
        }
        self.pauses += 1;
        if self.pauses >= STALE_PAUSES {
            self.pending.clear();
        }
        None
    }
}

/// The key at the start of `bytes` (`None` for a sequence Noa ignores) and
/// its length, or `None` when `bytes` is empty or ends before the key does.
fn next_key(bytes: &[u8]) -> Option<(Option<Key>, usize)> {
    let byte = *bytes.first()?;
    let key = match byte {
        0x1b => match *bytes.get(1)? {
            // An X10 mouse report (`CSI M Cb Cx Cy`), from a terminal that
            // ignores SGR mouse mode: its three bytes are not keys.
            b'[' if bytes.get(2) == Some(&b'M') => {
                let report = bytes.get(3..6)?;
                let key = match report[0] {
                    96 => Some(Key::WheelUp),
                    97 => Some(Key::WheelDown),
                    _ => None,
                };
                return Some((key, 6));
            }
            b'[' => {
                let len = bytes[2..].iter().position(|b| !(0x20..=0x3f).contains(b))?;
                let fin = bytes[2 + len];
                if !(0x40..=0x7e).contains(&fin) {
                    // Not a CSI after all (an ESC, say): drop what came
                    // before that byte and decode it afresh.
                    return Some((None, len + 2));
                }
                return Some((csi_key(&bytes[2..2 + len], fin), len + 3));
            }
            b'O' => {
                let key = match *bytes.get(2)? {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'P' => Some(Key::F1),
                    _ => None,
                };
                return Some((key, 3));
            }
            0x1b => Some(Key::Esc),
            // Alt (meta) + a key: not one of the preview's keys, and not Esc
            // followed by that key.
            _ => return Some((None, 2)),
        },
        b'\r' | b'\n' => Some(Key::Enter),
        0x7f => Some(Key::Backspace),
        0x1f => Some(Key::Ctrl('/')),
        0x01..=0x1a => Some(Key::Ctrl(char::from(b'a' + byte - 1))),
        0x00..=0x1f => None,
        _ => {
            let len = match byte {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            // A lead byte without its continuation bytes is dropped alone,
            // so the key after it survives.
            let continued = bytes[1..]
                .iter()
                .take(len - 1)
                .take_while(|b| (0x80..=0xbf).contains(*b))
                .count();
            if continued < len - 1 {
                if continued + 1 == bytes.len() {
                    return None;
                }
                return Some((None, 1));
            }
            let key = std::str::from_utf8(&bytes[..len])
                .ok()
                .and_then(|text| text.chars().next())
                .map(Key::Char);
            return Some((key, len));
        }
    };
    Some((key, 1))
}

fn csi_key(params: &[u8], fin: u8) -> Option<Key> {
    if let Some(mouse) = params.strip_prefix(b"<") {
        // SGR mouse report: only the wheel moves the selection.
        let button = mouse.split(|b| *b == b';').next()?;
        return match (button, fin) {
            (b"64", b'M') => Some(Key::WheelUp),
            (b"65", b'M') => Some(Key::WheelDown),
            _ => None,
        };
    }
    match (params, fin) {
        (_, b'A') => Some(Key::Up),
        (_, b'B') => Some(Key::Down),
        (_, b'H') | (b"1" | b"7", b'~') => Some(Key::Home),
        (_, b'F') | (b"4" | b"8", b'~') => Some(Key::End),
        (b"5", b'~') => Some(Key::PageUp),
        (b"6", b'~') => Some(Key::PageDown),
        (b"11", b'~') | (b"1;1", b'P') | (b"", b'P') => Some(Key::F1),
        _ => None,
    }
}

/// `~/.config/noa/auto/theme`, the file `w` writes and
/// `config-file = ?auto/theme` includes from the main config.
fn auto_theme_path() -> Option<PathBuf> {
    Some(
        noa_config::default_config_path()?
            .parent()?
            .join("auto")
            .join("theme"),
    )
}

fn write_auto_theme(name: &str) -> String {
    let Some(path) = auto_theme_path() else {
        return "Could not find Noa's config directory.".to_string();
    };
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, format!("theme = {name}\n")));
    match written {
        Ok(()) => format!("Wrote {}", path.display()),
        Err(err) => format!("Could not write {}: {err}", path.display()),
    }
}

fn osc52(text: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let bits = chunk
            .iter()
            .enumerate()
            .fold(0u32, |bits, (i, b)| bits | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                encoded.push(char::from(
                    ALPHABET[((bits >> (18 - 6 * i)) & 0x3f) as usize],
                ));
            } else {
                encoded.push('=');
            }
        }
    }
    format!("\x1b]52;c;{encoded}\x1b\\")
}

/// Puts the terminal in raw mode on the alternate screen with wheel
/// reporting, and restores it when dropped (an error or panic included).
struct RawTerminal {
    saved: libc::termios,
}

impl RawTerminal {
    fn enter() -> io::Result<Self> {
        // SAFETY: tcgetattr/tcsetattr only write the termios we pass.
        let saved = unsafe {
            let mut saved = std::mem::zeroed::<libc::termios>();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            saved
        };
        // Built before the writes, so a failed write still restores the
        // terminal when the guard drops.
        let terminal = Self { saved };
        let mut stdout = io::stdout();
        stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h")?;
        stdout.flush()?;
        Ok(terminal)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
        // SAFETY: restores the termios saved in `enter`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
    }
}

fn terminal_size() -> (usize, usize) {
    // SAFETY: TIOCGWINSZ only writes the winsize we pass.
    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } != 0 {
        return (80, 24);
    }
    (usize::from(size.ws_col), usize::from(size.ws_row))
}

/// The terminating signal [`run`] caught, or 0.
static SIGNALED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALED.store(signal, std::sync::atomic::Ordering::Relaxed);
}

const TERMINATING_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// Run the preview until the user quits. The caller has checked that stdin
/// and stdout are terminals. A terminating signal restores the terminal
/// first, then ends the process as that signal would.
pub(super) fn run() -> io::Result<()> {
    for signal in TERMINATING_SIGNALS {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                signal,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
    }
    let result = run_until_quit();
    let signal = SIGNALED.load(std::sync::atomic::Ordering::Relaxed);
    for signal in TERMINATING_SIGNALS {
        // SAFETY: back to the default disposition.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
    if signal != 0 {
        // SAFETY: re-raises the caught signal with its default action.
        unsafe { libc::raise(signal) };
    }
    result
}

fn run_until_quit() -> io::Result<()> {
    let _terminal = RawTerminal::enter()?;
    let mut preview = Preview::new(noa_theme::THEMES);
    let mut stdout = io::stdout();
    let mut size = terminal_size();
    let mut buf = [0u8; 1024];
    let mut reader = KeyReader::default();
    stdout.write_all(preview.render(size.0, size.1).as_bytes())?;
    stdout.flush()?;
    loop {
        let mut poll = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // A short timeout so a resize redraws without a SIGWINCH handler.
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if SIGNALED.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return Ok(());
        }
        let mut dirty = false;
        let keys = if ready > 0 {
            // SAFETY: reads into `buf`, at most its length.
            let read =
                unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if read <= 0 {
                return Ok(());
            }
            reader.feed(&buf[..read as usize])
        } else {
            reader.flush().into_iter().collect()
        };
        for key in keys {
            match preview.handle(key) {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Copy(text) => stdout.write_all(osc52(&text).as_bytes())?,
                Action::WriteAutoTheme(name) => {
                    let status = write_auto_theme(&name);
                    preview.set_status(name, status);
                }
            }
            dirty = true;
        }
        let now = terminal_size();
        if now != size {
            size = now;
            dirty = true;
        }
        if dirty {
            stdout.write_all(preview.render(size.0, size.1).as_bytes())?;
            stdout.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys from one read with nothing after it.
    fn parse_keys(bytes: &[u8]) -> Vec<Key> {
        let mut reader = KeyReader::default();
        let mut keys = reader.feed(bytes);
        keys.extend(reader.flush());
        keys
    }

    fn preview() -> Preview {
        Preview::new(noa_theme::THEMES)
    }

    fn selected_name(preview: &Preview) -> &'static str {
        preview.selected_theme().map_or("", |theme| theme.name)
    }

    #[test]
    fn parse_keys_decodes_the_keys_the_help_lists() {
        assert_eq!(parse_keys(b"\x1b"), [Key::Esc]);
        assert_eq!(parse_keys(b"\x1b[A\x1bOB"), [Key::Up, Key::Down]);
        assert_eq!(parse_keys(b"\x1b[5~\x1b[6~"), [Key::PageUp, Key::PageDown]);
        assert_eq!(
            parse_keys(b"\x1b[H\x1b[1~\x1bOH\x1b[F\x1b[4~\x1bOF"),
            [
                Key::Home,
                Key::Home,
                Key::Home,
                Key::End,
                Key::End,
                Key::End
            ]
        );
        assert_eq!(parse_keys(b"\x1bOP\x1b[11~"), [Key::F1, Key::F1]);
        assert_eq!(
            parse_keys(b"\x03\x04\x15\x08\x18\x1f"),
            [
                Key::Ctrl('c'),
                Key::Ctrl('d'),
                Key::Ctrl('u'),
                Key::Ctrl('h'),
                Key::Ctrl('x'),
                Key::Ctrl('/')
            ]
        );
        assert_eq!(parse_keys(b"\r\x7f"), [Key::Enter, Key::Backspace]);
        assert_eq!(
            parse_keys("gé".as_bytes()),
            [Key::Char('g'), Key::Char('é')]
        );
    }

    #[test]
    fn a_key_split_across_reads_waits_for_the_rest() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b"), []);
        assert_eq!(reader.feed(b"[B"), [Key::Down]);
        assert_eq!(reader.feed(b"j\x1b[<65;1"), [Key::Char('j')]);
        assert_eq!(reader.feed(b"0;5M"), [Key::WheelDown]);
        assert_eq!(reader.feed(b"\x1bO"), []);
        assert_eq!(reader.feed(b"P\xc3"), [Key::F1]);
        assert_eq!(reader.feed(b"\xa9"), [Key::Char('é')]);
        assert_eq!(reader.flush(), None);
    }

    #[test]
    fn a_held_esc_is_the_esc_key_once_input_stops() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b"), []);
        assert_eq!(reader.flush(), Some(Key::Esc));
        assert_eq!(reader.feed(b"j"), [Key::Char('j')]);
    }

    #[test]
    fn other_unfinished_input_outlasts_a_pause() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b["), []);
        assert_eq!(reader.flush(), None);
        assert_eq!(reader.feed(b"B"), [Key::Down]);
        assert_eq!(reader.feed(b"\xc3"), []);
        assert_eq!(reader.flush(), None);
        assert_eq!(reader.feed(b"\xa9"), [Key::Char('é')]);
    }

    #[test]
    fn an_unfinished_sequence_is_dropped_after_a_long_pause() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b[1"), []);
        for _ in 1..STALE_PAUSES {
            assert_eq!(reader.flush(), None);
        }
        assert_eq!(reader.feed(b";"), []);
        for _ in 0..STALE_PAUSES {
            assert_eq!(reader.flush(), None);
        }
        assert_eq!(reader.feed(b"q"), [Key::Char('q')]);
    }

    #[test]
    fn an_esc_inside_a_csi_ends_it() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b[1\x1b[A"), [Key::Up]);
        assert_eq!(reader.feed(b"\x1b[\rj"), [Key::Enter, Key::Char('j')]);
    }

    #[test]
    fn alt_and_a_key_is_neither_esc_nor_the_key() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1bj\x1bqk"), [Key::Char('k')]);
        assert_eq!(reader.feed(b"\x1b\x1b"), [Key::Esc]);
        assert_eq!(reader.flush(), Some(Key::Esc));
    }

    #[test]
    fn a_stray_utf8_lead_byte_does_not_eat_the_next_key() {
        let mut reader = KeyReader::default();
        assert_eq!(
            reader.feed(b"\xeaj\xe2\x86k"),
            [Key::Char('j'), Key::Char('k')]
        );
        assert_eq!(reader.feed(b"\xe2\x86"), []);
        assert_eq!(reader.feed(b"\x91"), [Key::Char('↑')]);
    }

    #[test]
    fn x10_mouse_reports_are_not_keys() {
        let mut reader = KeyReader::default();
        // Clicks at column 81 (Cx = 'q') and row 74 (Cy = 'j').
        assert_eq!(reader.feed(b"\x1b[M qj\x1b[M#qj"), []);
        assert_eq!(reader.feed(b"\x1b[M`!!\x1b[Ma!"), [Key::WheelUp]);
        assert_eq!(reader.feed(b"!"), [Key::WheelDown]);
    }

    #[test]
    fn parse_keys_takes_only_the_wheel_from_mouse_reports() {
        assert_eq!(
            parse_keys(b"\x1b[<64;10;5M\x1b[<65;10;5M\x1b[<0;10;5M\x1b[<0;10;5m"),
            [Key::WheelUp, Key::WheelDown]
        );
    }

    #[test]
    fn parse_keys_drops_unknown_and_truncated_sequences() {
        assert_eq!(parse_keys(b"\x1b[99~j"), [Key::Char('j')]);
        assert_eq!(parse_keys(b"j\x1b[1"), [Key::Char('j')]);
        assert_eq!(parse_keys(b"\x1bO"), []);
    }

    #[test]
    fn the_list_is_sorted_ignoring_case() {
        let preview = preview();
        let names: Vec<String> = preview
            .visible
            .iter()
            .map(|&index| preview.themes[index].0.to_lowercase())
            .collect();
        assert_eq!(names.len(), noa_theme::THEMES.len());
        assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn moves_stop_at_the_ends_of_the_list() {
        let mut preview = preview();
        let last = preview.visible.len() - 1;
        preview.handle(Key::Char('k'));
        assert_eq!(preview.selected, 0);
        preview.handle(Key::Char('j'));
        preview.handle(Key::Down);
        preview.handle(Key::WheelDown);
        assert_eq!(preview.selected, 3);
        preview.handle(Key::Ctrl('d'));
        assert_eq!(preview.selected, 23);
        preview.handle(Key::PageUp);
        preview.handle(Key::Ctrl('u'));
        assert_eq!(preview.selected, 0);
        preview.handle(Key::Char('G'));
        assert_eq!(preview.selected, last);
        preview.handle(Key::PageDown);
        assert_eq!(preview.selected, last);
        preview.handle(Key::Home);
        assert_eq!(preview.selected, 0);
        preview.handle(Key::End);
        preview.handle(Key::Char('g'));
        assert_eq!(preview.selected, 0);
    }

    #[test]
    fn filters_cycle_all_dark_light_and_keep_a_theme_still_listed() {
        let mut preview = preview();
        let all = preview.visible.len();
        preview.handle(Key::End);
        let dark_theme = preview
            .visible
            .iter()
            .rev()
            .map(|&index| &preview.themes[index].1)
            .find(|theme| super::super::theme_variant(theme) == "dark")
            .unwrap()
            .name;
        while selected_name(&preview) != dark_theme {
            preview.handle(Key::Up);
        }
        preview.handle(Key::Char('f'));
        assert!(preview.visible.len() < all);
        assert_eq!(selected_name(&preview), dark_theme);
        preview.handle(Key::Char('f'));
        let selected = preview.selected_theme().unwrap();
        assert_eq!(super::super::theme_variant(selected), "light");
        let light_theme = selected.name;
        preview.handle(Key::Char('f'));
        assert_eq!(preview.visible.len(), all);
        assert_eq!(selected_name(&preview), light_theme);
    }

    #[test]
    fn search_matches_a_case_insensitive_subsequence_in_list_order() {
        let mut preview = preview();
        preview.handle(Key::Char('/'));
        for c in "ZEN".chars() {
            preview.handle(Key::Char(c));
        }
        let names: Vec<&str> = preview
            .visible
            .iter()
            .map(|&index| preview.themes[index].0)
            .collect();
        assert!(!names.is_empty());
        assert!(
            names
                .windows(2)
                .all(|pair| pair[0].to_lowercase() <= pair[1].to_lowercase())
        );
        assert!(
            names
                .iter()
                .all(|name| is_subsequence(&['z', 'e', 'n'], name))
        );
        assert!(names.contains(&"Zenburn"));
        // `q` is typed while searching; Enter closes the search window and
        // keeps the query.
        preview.handle(Key::Char('q'));
        assert!(preview.visible.is_empty());
        preview.handle(Key::Backspace);
        preview.handle(Key::Enter);
        assert_eq!(preview.overlay, Overlay::None);
        assert_eq!(preview.query, "ZEN");
        preview.handle(Key::Ctrl('x'));
        assert_eq!(preview.query, "");
        assert_eq!(preview.visible.len(), noa_theme::THEMES.len());
    }

    #[test]
    fn esc_closes_a_window_before_it_quits() {
        let mut preview = preview();
        preview.handle(Key::F1);
        assert_eq!(preview.overlay, Overlay::Help);
        assert_eq!(preview.handle(Key::Esc), Action::None);
        assert_eq!(preview.overlay, Overlay::None);
        assert_eq!(preview.handle(Key::Esc), Action::Quit);
        assert_eq!(preview.handle(Key::Char('q')), Action::Quit);
        assert_eq!(preview.handle(Key::Ctrl('c')), Action::Quit);
        preview.handle(Key::Char('?'));
        preview.handle(Key::Ctrl('h'));
        assert_eq!(preview.overlay, Overlay::None);
    }

    #[test]
    fn c_copies_and_w_writes_only_from_the_save_window() {
        let mut preview = preview();
        let name = selected_name(&preview).to_string();
        assert_eq!(preview.handle(Key::Char('c')), Action::Copy(name.clone()));
        assert_eq!(preview.handle(Key::Char('w')), Action::None);
        preview.handle(Key::Enter);
        assert_eq!(preview.overlay, Overlay::Save);
        assert_eq!(preview.handle(Key::Char('w')), Action::WriteAutoTheme(name));
    }

    #[test]
    fn enter_does_nothing_without_a_theme() {
        let mut preview = preview();
        preview.handle(Key::Char('/'));
        preview.handle(Key::Char('#'));
        preview.handle(Key::Esc);
        assert!(preview.visible.is_empty());
        preview.handle(Key::Enter);
        assert_eq!(preview.overlay, Overlay::None);
        assert_eq!(preview.handle(Key::Char('c')), Action::None);
        assert!(!preview.render(100, 30).is_empty());
    }

    #[test]
    fn render_marks_the_selection_and_scrolls_to_keep_it_visible() {
        let mut preview = preview();
        let frame = preview.render(100, 10);
        let first = selected_name(&preview);
        assert!(frame.contains(&format!("❯ {first}")));
        assert!(frame.contains("❮"));
        for _ in 0..12 {
            preview.handle(Key::Down);
        }
        preview.render(100, 10);
        assert_eq!(preview.top, 3);
        preview.handle(Key::Home);
        preview.render(100, 10);
        assert_eq!(preview.top, 0);
    }

    #[test]
    fn render_labels_the_palette_in_decimal_or_hex() {
        let mut preview = preview();
        let frame = preview.render(120, 30);
        assert!(frame.contains(" 15 "));
        preview.handle(Key::Char('x'));
        let frame = preview.render(120, 30);
        assert!(frame.contains(" 0f "));
        assert!(!frame.contains(" 15 "));
        preview.handle(Key::Char('d'));
        assert!(preview.render(120, 30).contains(" 15 "));
    }

    #[test]
    fn render_draws_the_help_and_save_windows() {
        let mut preview = preview();
        preview.handle(Key::F1);
        let frame = preview.render(100, 30);
        assert!(frame.contains("Toggle help window."));
        assert!(frame.contains("Write theme to auto config file."));
        preview.handle(Key::F1);
        preview.handle(Key::Enter);
        let name = selected_name(&preview);
        preview.set_status(name.to_string(), "Wrote /tmp/auto/theme".to_string());
        let frame = preview.render(100, 30);
        assert!(frame.contains(&format!("theme = {name}")));
        assert!(frame.contains("config-file = ?auto/theme"));
        assert!(frame.contains("Wrote /tmp/auto/theme"));
        // Another theme in the save window: the status was not about it.
        preview.handle(Key::Down);
        assert!(!preview.render(100, 30).contains("Wrote /tmp/auto/theme"));
        preview.handle(Key::Up);
        assert!(preview.render(100, 30).contains("Wrote /tmp/auto/theme"));
    }

    #[test]
    fn render_survives_tiny_screens() {
        let mut preview = preview();
        for overlay in [Key::F1, Key::Enter, Key::Char('/')] {
            preview.handle(Key::Esc);
            preview.handle(overlay);
            for (cols, rows) in [(0, 0), (0, 3), (1, 1), (10, 3), (31, 2), (32, 1), (40, 4)] {
                preview.render(cols, rows);
            }
        }
    }

    #[test]
    fn osc52_base64_encodes_the_text() {
        assert_eq!(osc52("Zenburn"), "\x1b]52;c;WmVuYnVybg==\x1b\\");
        assert_eq!(osc52("ab"), "\x1b]52;c;YWI=\x1b\\");
        assert_eq!(osc52("abc"), "\x1b]52;c;YWJj\x1b\\");
    }
}
