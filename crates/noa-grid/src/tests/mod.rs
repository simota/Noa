//! Terminal state tests: deferred-wrap latch, cursor clamps, erase, DSR
//! replies, and captured-stream golden fixtures.

use crate::cursor::{CursorStyle, HorizontalMargins};
use crate::terminal::{PromptJump, ShellIntegrationMarkKind, Terminal};
use noa_core::{
    CellAttrs, Color, DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, GridSize, Point, Rgb, xterm_palette,
    xterm_palette_color,
};
use noa_vt::Stream;

/// Feed `bytes` through a fresh 80x24 terminal and return the final state.
fn run(bytes: &[u8]) -> Terminal {
    run_size(80, 24, bytes)
}

fn run_size(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

/// Like [`run`], but with `title-report` enabled so `CSI 21 t` replies.
fn run_title_report(bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.title_report = true;
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn run_with_base_colors(
    bytes: &[u8],
    default_fg: Rgb,
    default_bg: Rgb,
    cursor: Rgb,
    palette: [Rgb; 256],
) -> Terminal {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.set_base_colors(default_fg, default_bg, cursor, palette);
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn cell(t: &Terminal, x: usize, y: usize) -> crate::cell::Cell {
    t.primary.grid[y].cells[x].clone()
}

fn row_text(t: &Terminal, y: usize, width: usize) -> String {
    t.primary.grid[y].cells[..width]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn active_row_text(t: &Terminal, y: usize, width: usize) -> String {
    t.active().grid[y].cells[..width]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn rows_text(rows: &[crate::cell::Row], y: usize, width: usize) -> String {
    rows[y].cells[..width].iter().map(|c| c.ch).collect()
}

fn feed_full_rows(s: &mut Stream, t: &mut Terminal, cols: usize, n: usize) {
    for i in 0..n {
        let mut line = format!("R{i}");
        while line.len() < cols {
            line.push('.');
        }
        line.push_str("\r\n");
        s.feed(line.as_bytes(), t);
    }
}

fn terminal_full_history(cols: u16, rows: u16, n: usize) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    feed_full_rows(&mut s, &mut t, cols as usize, n);
    t
}

// Keep these files in one module so existing `tests::name` filters keep working.
include!("terminal_state.rs");
include!("selection_search.rs");
include!("osc.rs");
include!("text_resize.rs");
include!("vt_sequences.rs");
include!("kitty_graphics.rs");
include!("bulk_print.rs");
