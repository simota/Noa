//! Input-driven `App` operations — terminal/font/search actions,
//! search prompt & command palette keys, clipboard, confirm dialog,
//! PTY writes, split-drag, and hover-link handling.

mod clipboard_confirm;
mod copy_mode;
mod ime;
mod layout;
mod overlays;
mod pointer;
mod process_monitor;
mod search;
mod tab_title;
mod terminal;
mod theme_settings;

pub(in crate::app) use copy_mode::{
    copy_mode_should_exit_for_pty_bytes, copy_mode_should_swallow_super_key,
};
pub(in crate::app) use overlays::ActiveOverlay;
