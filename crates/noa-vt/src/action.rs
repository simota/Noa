//! The low-level actions emitted by [`crate::Parser::advance`].

use crate::csi::{Csi, DcsPayload, Esc};

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
    /// A completed OSC string (raw bytes between `ESC ]` and `ST`/`BEL`)
    /// and how it ended.
    OscDispatch(Vec<u8>, OscTerminator),
    /// A completed DCS string (raw bytes between `ESC P` and `ST`).
    DcsDispatch(DcsPayload),
    /// A completed APC string (raw bytes between `ESC _` and `ST`/`BEL`).
    /// One over the capture limit is dropped silently, like OSC/DCS.
    ApcDispatch { data: Vec<u8> },
}

/// How an OSC string ended. Replies to some OSCs (Kitty's OSC 5522) echo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OscTerminator {
    Bel,
    /// `ST` — and anything else that ends the string (Ghostty 1.4.0 treats
    /// every non-BEL end as ST).
    St,
}
