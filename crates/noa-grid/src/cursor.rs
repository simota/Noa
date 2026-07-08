//! The cursor (position + pen state) and the scroll region.

use noa_core::{CellAttrs, Color};

use crate::cell::HyperlinkId;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorStyle {
    #[default]
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
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
    pub hyperlink: Option<HyperlinkId>,
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

/// What `DECSC` (`ESC 7` / `CSI s`) saves and `DECRC` (`ESC 8` / `CSI u`)
/// restores: position, the deferred-wrap latch, and pen state. Deliberately
/// narrower than [`Cursor`] — per xterm/ECMA-48 (and Ghostty's `SavedCursor`),
/// cursor visibility (DECTCEM) and shape (DECSCUSR) are *not* part of the
/// saved-cursor state, so DECRC must not roll them back.
#[derive(Clone, Copy, Debug)]
pub struct SavedCursor {
    pub x: u16,
    pub y: u16,
    pub pending_wrap: bool,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub hyperlink: Option<HyperlinkId>,
    pub attrs: CellAttrs,
}

impl From<Cursor> for SavedCursor {
    fn from(c: Cursor) -> Self {
        SavedCursor {
            x: c.x,
            y: c.y,
            pending_wrap: c.pending_wrap,
            fg: c.fg,
            bg: c.bg,
            underline_color: c.underline_color,
            hyperlink: c.hyperlink,
            attrs: c.attrs,
        }
    }
}

impl Cursor {
    /// Applies a restored [`SavedCursor`] onto this cursor, leaving `visible`
    /// and `style` untouched — DECRC does not restore them.
    pub fn restore_from(&mut self, saved: SavedCursor) {
        self.x = saved.x;
        self.y = saved.y;
        self.pending_wrap = saved.pending_wrap;
        self.fg = saved.fg;
        self.bg = saved.bg;
        self.underline_color = saved.underline_color;
        self.hyperlink = saved.hyperlink;
        self.attrs = saved.attrs;
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
