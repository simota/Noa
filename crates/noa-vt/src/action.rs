//! The low-level actions emitted by [`crate::Parser::advance`].

use crate::csi::{Csi, Esc};

/// A single primitive action produced by the parser DFA. [`crate::Stream`]
/// maps these onto [`crate::Handler`] method calls.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Print a Unicode scalar into the active cell.
    Print(char),
    /// Execute a C0 control byte (`BEL`/`BS`/`HT`/`LF`/`CR`/…).
    Execute(u8),
    /// A completed CSI sequence.
    CsiDispatch(Csi),
    /// A completed `ESC` sequence.
    EscDispatch(Esc),
    /// A completed OSC string (raw bytes between `ESC ]` and `ST`/`BEL`).
    OscDispatch(Vec<u8>),
}
