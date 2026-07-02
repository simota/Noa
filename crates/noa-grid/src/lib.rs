//! `noa-grid` — the terminal state model: screen grid, cursor, modes, tab
//! stops, and the [`noa_vt::Handler`] implementation that mutates them.
//!
//! Ghostty analog: `terminal/Terminal.zig`, `Screen.zig`, `page.zig`,
//! `modes.zig`. The active area is still a flat `Vec<Row>`; scrollback is stored
//! as cloned rows until paged storage and `StyleId` interning land.

pub mod cell;
pub mod cursor;
pub mod modes;
mod osc;
pub mod screen;
pub mod search;
pub mod selection;
pub mod tabstops;
pub mod terminal;

#[cfg(test)]
mod tests;

pub use cell::{Cell, Row};
pub use cursor::{Cursor, ScrollRegion};
pub use modes::ModeState;
pub use osc::{Osc52Policy, TerminalColors};
pub use screen::Screen;
pub use search::{SearchMatch, SearchState};
pub use selection::{Selection, SelectionPoint};
pub use tabstops::Tabstops;
pub use terminal::Terminal;
