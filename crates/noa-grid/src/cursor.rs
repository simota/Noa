//! The cursor (position + pen state) and the scroll region.

use noa_core::{CellAttrs, Color};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorStyle {
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self::BlinkingBlock
    }
}

/// The terminal cursor: position, the deferred-wrap latch, and the active pen.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    /// Column, 0-based.
    pub x: u16,
    /// Row, 0-based (viewport-relative in inc-1).
    pub y: u16,
    /// Deferred-wrap latch (xenl): set after printing into the last column;
    /// the next printable char wraps first (when autowrap is on).
    pub pending_wrap: bool,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub hyperlink: Option<usize>,
    pub attrs: CellAttrs,
    /// DECTCEM (mode 25).
    pub visible: bool,
    pub style: CursorStyle,
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor {
            x: 0,
            y: 0,
            pending_wrap: false,
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
            visible: true,
            style: CursorStyle::default(),
        }
    }
}

/// The vertical scroll region (0-based, inclusive). Inc-1: full-width only.
#[derive(Clone, Copy, Debug)]
pub struct ScrollRegion {
    pub top: u16,
    pub bottom: u16,
}

/// The horizontal left/right margins (0-based, inclusive).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HorizontalMargins {
    pub left: u16,
    pub right: u16,
}
