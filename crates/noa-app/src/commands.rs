//! App-level commands that must not be encoded as terminal input.

mod command;
mod key_token;
mod keybind;

#[cfg(test)]
mod tests;

pub use command::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};
#[cfg(test)]
pub(crate) use key_token::KeybindParseError;
#[cfg(test)]
pub(crate) use keybind::KeyBinding;
pub(crate) use keybind::KeybindEngine;
