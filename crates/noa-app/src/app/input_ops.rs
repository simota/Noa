//! Input-driven `App` operations — terminal/font/search actions,
//! search prompt & command palette keys, clipboard, confirm dialog,
//! PTY writes, split-drag, and hover-link handling.

mod clipboard_confirm;
mod ime;
mod layout;
mod overlays;
mod pointer;
mod search;
mod terminal;
mod theme_settings;

pub(in crate::app) use overlays::ActiveOverlay;
