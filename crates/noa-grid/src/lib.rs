//! `noa-grid` — the terminal state model: screen grid, cursor, modes, tab
//! stops, and the [`noa_vt::Handler`] implementation that mutates them.
//!
//! Ghostty analog: `terminal/Terminal.zig`, `Screen.zig`, `page.zig`,
//! `modes.zig`. The active area is a flat `Vec<Row>` of inlined `Cell`s;
//! scrollback is paged, style-interned, byte-bounded storage (`scrollback.rs`)
//! sized by the `scrollback-limit` config.

pub mod cell;
mod charset;
pub mod cursor;
pub mod kitty_keyboard;
pub mod modes;
mod osc;
pub mod screen;
mod scrollback;
pub mod search;
pub mod selection;
pub mod tabstops;
pub mod terminal;
pub mod url;

#[cfg(test)]
mod tests;

pub use cell::{Cell, Hyperlink, Row};
pub use cursor::{Cursor, CursorStyle, HorizontalMargins, ScrollRegion};
pub use kitty_keyboard::{
    KITTY_ALL_FLAGS, KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ALTERNATE_KEYS,
    KITTY_REPORT_ASSOCIATED_TEXT, KITTY_REPORT_EVENT_TYPES, KittyKeyboard, SetMode,
};
pub use modes::ModeState;
pub use osc::{Notification, Osc52Policy, TerminalColors};
pub use screen::Screen;
pub use search::{SearchMatch, SearchState};
pub use selection::{Selection, SelectionPoint};
pub use tabstops::Tabstops;
pub use terminal::{PromptJump, ShellIntegrationMark, ShellIntegrationMarkKind, Terminal};
pub use url::{UrlMatch, detect_url_at_column};
