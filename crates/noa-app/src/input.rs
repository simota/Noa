//! Keyboard-event -> pty-byte encoding.
//!
//! `winit::event::KeyEvent` has a private platform-specific field, so it
//! can't be constructed in tests; [`encode_key_with_modes`] takes the pieces
//! we need (`logical_key`, `text`, modifiers, and mode flags) directly so the
//! encoding logic stays unit-testable without a live `KeyEvent`.

mod ime;
mod key;
mod kitty;
mod paste;
mod text;

pub(crate) use ime::ImeState;
#[cfg(test)]
use key::encode_key;
pub(crate) use key::encode_key_with_modes;
pub use paste::encode_paste;
pub(crate) use paste::paste_is_unsafe;
#[cfg(test)]
use text::encode_text;

#[cfg(test)]
mod tests;
