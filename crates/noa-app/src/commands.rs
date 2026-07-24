//! App-level commands that must not be encoded as terminal input.

mod command;
mod key_token;
mod keybind;

#[cfg(test)]
mod tests;

pub use command::{
    AppCommand, CopyModeAction, FontSizeAction, SearchAction, TerminalAction, ViewportScroll,
};
#[cfg(test)]
pub(crate) use key_token::KeybindParseError;
#[cfg(test)]
pub(crate) use keybind::KeyBinding;
pub(crate) use keybind::KeybindEngine;
pub(crate) use keybind::command_from_applescript_action;
pub(crate) use keybind::is_valid_keybind_chord;
