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
    /// `block_hollow`: a hollow rectangle outline, focused or not (unlike
    /// the render layer's separate unfocused-pane hollow, this is the
    /// user-requested DECSCUSR-adjacent shape and still honors blink).
    BlinkingBlockHollow,
    SteadyBlockHollow,
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

/// The pen half of a [`Cursor`] — exactly the fields SGR mutates. Snapshot /
/// restore vocabulary for the line-batch's per-line style speculation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub attrs: CellAttrs,
}

impl Cursor {
    pub(crate) fn pen(&self) -> Pen {
        Pen {
            fg: self.fg,
            bg: self.bg,
            underline_color: self.underline_color,
            attrs: self.attrs,
        }
    }

    pub(crate) fn set_pen(&mut self, pen: Pen) {
        self.fg = pen.fg;
        self.bg = pen.bg;
        self.underline_color = pen.underline_color;
        self.attrs = pen.attrs;
    }

    /// Apply decoded SGR attribute changes to the pen, in order. The single
    /// authority for SGR→pen semantics: `Terminal::set_attributes` and the
    /// line-batch's per-line style templating both funnel here.
    pub(crate) fn apply_sgr(&mut self, attrs: &[noa_vt::SgrAttr]) {
        use noa_vt::SgrAttr;
        for a in attrs {
            match *a {
                SgrAttr::Reset => {
                    self.fg = Color::Default;
                    self.bg = Color::Default;
                    self.underline_color = None;
                    self.attrs = CellAttrs::empty();
                }
                SgrAttr::Bold => self.attrs.insert(CellAttrs::BOLD),
                SgrAttr::Faint => self.attrs.insert(CellAttrs::FAINT),
                SgrAttr::Italic => self.attrs.insert(CellAttrs::ITALIC),
                SgrAttr::Underline => {
                    self.attrs.remove(CellAttrs::underline_styles());
                    self.attrs.insert(CellAttrs::UNDERLINE);
                }
                SgrAttr::DoubleUnderline => {
                    self.attrs.remove(CellAttrs::underline_styles());
                    self.attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                SgrAttr::CurlyUnderline => {
                    self.attrs.remove(CellAttrs::underline_styles());
                    self.attrs.insert(CellAttrs::CURLY_UNDERLINE);
                }
                SgrAttr::DottedUnderline => {
                    self.attrs.remove(CellAttrs::underline_styles());
                    self.attrs.insert(CellAttrs::DOTTED_UNDERLINE);
                }
                SgrAttr::DashedUnderline => {
                    self.attrs.remove(CellAttrs::underline_styles());
                    self.attrs.insert(CellAttrs::DASHED_UNDERLINE);
                }
                SgrAttr::Blink => self.attrs.insert(CellAttrs::BLINK),
                SgrAttr::Inverse => self.attrs.insert(CellAttrs::INVERSE),
                SgrAttr::Invisible => self.attrs.insert(CellAttrs::INVISIBLE),
                SgrAttr::Strike => self.attrs.insert(CellAttrs::STRIKETHROUGH),
                SgrAttr::Overline => self.attrs.insert(CellAttrs::OVERLINE),
                SgrAttr::ResetBold => self.attrs.remove(CellAttrs::BOLD | CellAttrs::FAINT),
                SgrAttr::ResetItalic => self.attrs.remove(CellAttrs::ITALIC),
                SgrAttr::ResetUnderline => self.attrs.remove(CellAttrs::underline_styles()),
                SgrAttr::ResetBlink => self.attrs.remove(CellAttrs::BLINK),
                SgrAttr::ResetInverse => self.attrs.remove(CellAttrs::INVERSE),
                SgrAttr::ResetInvisible => self.attrs.remove(CellAttrs::INVISIBLE),
                SgrAttr::ResetStrike => self.attrs.remove(CellAttrs::STRIKETHROUGH),
                SgrAttr::ResetOverline => self.attrs.remove(CellAttrs::OVERLINE),
                SgrAttr::Fg(col) => self.fg = col,
                SgrAttr::Bg(col) => self.bg = col,
                SgrAttr::UnderlineColor(col) => self.underline_color = Some(col),
                SgrAttr::DefaultFg => self.fg = Color::Default,
                SgrAttr::DefaultBg => self.bg = Color::Default,
                SgrAttr::DefaultUnderlineColor => self.underline_color = None,
            }
        }
    }
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
