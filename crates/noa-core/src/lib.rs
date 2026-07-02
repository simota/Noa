//! `noa-core` — shared primitive types every noa crate speaks in.
//!
//! A leaf type-crate: it has no internal dependencies, which prevents
//! `noa-vt` / `noa-grid` / `noa-render` from each redefining `Color` and the
//! geometry types (and avoids dependency cycles). Mirrors the primitive types
//! Ghostty shares across its `terminal` / `renderer` subsystems.

pub mod attrs;
pub mod color;
pub mod geometry;
pub mod palette;

pub use attrs::CellAttrs;
pub use color::{Color, Rgb};
pub use geometry::{CellSize, GridSize, PixelSize, Point};
pub use palette::{DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, xterm_palette, xterm_palette_color};
