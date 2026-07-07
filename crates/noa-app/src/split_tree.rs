//! Pure split-pane tree and layout math.
//!
//! This module intentionally stays independent of `winit`, `wgpu`, terminals,
//! and ptys so split behavior can be unit-tested without constructing a
//! window or GPU context.

mod tree;

pub use tree::*;
