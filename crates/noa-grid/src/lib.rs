//! `noa-grid` — the terminal state model: screen grid, cursor, modes, tab
//! stops, and the [`noa_vt::Handler`] implementation that mutates them.
//!
//! Ghostty analog: `terminal/Terminal.zig`, `Screen.zig`, `page.zig`,
//! `modes.zig`. Inc-1 uses a flat `Vec<Row>` active area (no paged scrollback
//! or `StyleId` interning — those land in inc≥3).

pub mod cell;
pub mod cursor;
pub mod modes;
pub mod screen;
pub mod tabstops;
pub mod terminal;

#[cfg(test)]
mod tests;

pub use cell::{Cell, Row};
pub use cursor::{Cursor, ScrollRegion};
pub use modes::ModeState;
pub use screen::Screen;
pub use tabstops::Tabstops;
pub use terminal::Terminal;
