//! The screen-dump runner: feed bytes through the real `Stream` → `Terminal`
//! pipeline and render the resulting screen in a stable, diffable form.

use noa_core::{CellAttrs, Color, GridSize};
use noa_grid::{Cell, Row, Screen, Terminal};
use noa_vt::Stream;

/// How the final screen is rendered (the fixture `## mode:` header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpMode {
    /// Visible text: one line per grid row (trailing blanks trimmed, wide
    /// cells printed once), plus a final `# cursor: <row>,<col>` line.
    Text,
    /// Style annotations: one line per run of identically-styled cells,
    /// plus the same cursor line. Format documented in `README.md`.
    Attrs,
}

/// Run `input` through a fresh `cols`×`rows` terminal and dump the screen
/// in [`DumpMode::Text`].
pub fn run_fixture(input: &[u8], cols: u16, rows: u16) -> String {
    run_fixture_with_mode(input, cols, rows, DumpMode::Text)
}

/// Like [`run_fixture`], with an explicit [`DumpMode`].
pub fn run_fixture_with_mode(input: &[u8], cols: u16, rows: u16, mode: DumpMode) -> String {
    let mut term = Terminal::new(GridSize::new(cols, rows));
    let mut stream = Stream::new();
    stream.feed(input, &mut term);
    match mode {
        DumpMode::Text => dump_text(&term),
        DumpMode::Attrs => dump_attrs(&term),
    }
}

fn dump_text(term: &Terminal) -> String {
    let screen = term.active();
    let mut lines: Vec<String> = screen.grid.iter().map(row_text).collect();
    lines.push(cursor_line(screen));
    lines.join("\n")
}

fn row_text(row: &Row) -> String {
    let mut line = String::new();
    for cell in &row.cells {
        // A wide (CJK) cell occupies two grid cells; print the scalar once
        // and skip its trailing spacer.
        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            continue;
        }
        cell.push_text_to(&mut line);
    }
    line.truncate(line.trim_end().len());
    line
}

/// `# cursor: <row>,<col>` (0-based), with the deferred-wrap latch (xenl)
/// made explicit so wrap fixtures can pin it.
fn cursor_line(screen: &Screen) -> String {
    let cursor = &screen.cursor;
    let latch = if cursor.pending_wrap {
        " (pending-wrap)"
    } else {
        ""
    };
    format!("# cursor: {},{}{latch}", cursor.y, cursor.x)
}

/// The style facets a run groups by. `WIDE`/`WIDE_SPACER` are stripped so a
/// wide lead and its spacer fold into one run (the x-range spans both cells,
/// which is how the dump encodes wideness: range width > text width).
#[derive(PartialEq, Eq)]
struct RunStyle {
    fg: Color,
    bg: Color,
    underline_color: Option<Color>,
    attrs: CellAttrs,
}

impl RunStyle {
    fn of(cell: &Cell) -> Self {
        RunStyle {
            fg: cell.fg,
            bg: cell.bg,
            underline_color: cell.underline_color,
            attrs: cell
                .attrs
                .difference(CellAttrs::WIDE | CellAttrs::WIDE_SPACER),
        }
    }
}

/// One in-progress run of identically-styled, non-default cells.
struct Run {
    x0: usize,
    x1: usize,
    text: String,
    style: RunStyle,
}

fn dump_attrs(term: &Terminal) -> String {
    let screen = term.active();
    let mut lines = Vec::new();
    for (y, row) in screen.grid.iter().enumerate() {
        push_row_runs(&mut lines, y, row);
    }
    lines.push(cursor_line(screen));
    lines.join("\n")
}

fn push_row_runs(lines: &mut Vec<String>, y: usize, row: &Row) {
    let default = Cell::default();
    let mut run: Option<Run> = None;
    for (x, cell) in row.cells.iter().enumerate() {
        // Fully default cells (blank, unstyled) never appear in the dump;
        // they also break any open run.
        if *cell == default {
            flush_run(lines, y, &mut run);
            continue;
        }
        let style = RunStyle::of(cell);
        let is_spacer = cell.attrs.contains(CellAttrs::WIDE_SPACER);
        match &mut run {
            Some(open) if open.style == style => {
                open.x1 = x;
                if !is_spacer {
                    cell.push_text_to(&mut open.text);
                }
            }
            _ => {
                flush_run(lines, y, &mut run);
                let mut text = String::new();
                if !is_spacer {
                    cell.push_text_to(&mut text);
                }
                run = Some(Run {
                    x0: x,
                    x1: x,
                    text,
                    style,
                });
            }
        }
    }
    flush_run(lines, y, &mut run);
}

fn flush_run(lines: &mut Vec<String>, y: usize, run: &mut Option<Run>) {
    let Some(run) = run.take() else {
        return;
    };
    let mut line = format!(
        "{y}: [{}-{}] \"{}\"",
        run.x0,
        run.x1,
        escape_text(&run.text)
    );
    if run.style.fg != Color::Default {
        line.push_str(&format!(" fg={}", color_token(run.style.fg)));
    }
    if run.style.bg != Color::Default {
        line.push_str(&format!(" bg={}", color_token(run.style.bg)));
    }
    if let Some(ul) = run.style.underline_color {
        line.push_str(&format!(" ul={}", color_token(ul)));
    }
    let flags = attr_tokens(run.style.attrs);
    if !flags.is_empty() {
        line.push_str(&format!(" attrs={}", flags.join("+")));
    }
    lines.push(line);
}

fn color_token(color: Color) -> String {
    match color {
        Color::Default => "default".into(),
        Color::Palette(n) => n.to_string(),
        Color::Rgb(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
    }
}

fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

const ATTR_TOKENS: &[(CellAttrs, &str)] = &[
    (CellAttrs::BOLD, "bold"),
    (CellAttrs::FAINT, "faint"),
    (CellAttrs::ITALIC, "italic"),
    (CellAttrs::UNDERLINE, "underline"),
    (CellAttrs::BLINK, "blink"),
    (CellAttrs::INVERSE, "inverse"),
    (CellAttrs::INVISIBLE, "invisible"),
    (CellAttrs::STRIKETHROUGH, "strike"),
    (CellAttrs::OVERLINE, "overline"),
    (CellAttrs::DOUBLE_UNDERLINE, "double-underline"),
    (CellAttrs::CURLY_UNDERLINE, "curly-underline"),
    (CellAttrs::DOTTED_UNDERLINE, "dotted-underline"),
    (CellAttrs::DASHED_UNDERLINE, "dashed-underline"),
];

fn attr_tokens(attrs: CellAttrs) -> Vec<&'static str> {
    ATTR_TOKENS
        .iter()
        .filter(|(flag, _)| attrs.contains(*flag))
        .map(|(_, name)| *name)
        .collect()
}
