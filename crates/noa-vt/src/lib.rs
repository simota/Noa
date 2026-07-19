//! `noa-vt` — a from-scratch DEC ANSI (Paul Williams) VT parser and semantic
//! stream dispatcher.
//!
//! This is the fidelity core of noa: it reproduces the observable behavior of
//! Ghostty's `terminal/Parser.zig` + `parse_table.zig` + `stream.zig`, but is
//! written from scratch in Rust (no `vte` / `alacritty_terminal`).
//!
//! Layers:
//! * [`Parser`] — the byte-driven DFA. `advance` emits low-level [`Action`]s.
//! * [`Stream`] — maps [`Action`]s onto a [`Handler`] (the parse↔state seam).
//! * [`Handler`] — the trait a terminal state model implements (see `noa-grid`).

pub mod action;
pub mod csi;
pub mod handler;
pub mod kitty_graphics;
pub mod parser;
pub mod sgr;
pub mod sixel;
pub mod state;
pub mod stream;

#[cfg(test)]
mod tests;

pub use action::Action;
pub use csi::{Csi, DcsPayload, Esc};
pub use handler::{
    AsciiLine, AsciiLines, Charset, CharsetSlot, CursorStyle, DaKind, DsrKind, EraseDisplay,
    EraseLine, Handler, ModeRequest, PlainSgrUnits, SgrAsciiLine, SgrAsciiLines,
};
pub use kitty_graphics::{
    KittyAction, KittyCompression, KittyDelete, KittyFormat, KittyGraphicsCommand, KittyMedium,
};
pub use parser::Parser;
pub use sgr::{SgrAttr, parse_plain_sgr_unit, parse_sgr, scan_plain_sgr};
pub use sixel::SixelGraphicsCommand;
pub use state::State;
pub use stream::{SharedParser, Stream};
