//! Cell rendition attributes (SGR state), stored as a compact bitset.

use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct CellAttrs: u16 {
        const BOLD          = 1 << 0;
        const FAINT         = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3;
        const BLINK         = 1 << 4;
        const INVERSE       = 1 << 5;
        const INVISIBLE     = 1 << 6;
        const STRIKETHROUGH = 1 << 7;
        const OVERLINE      = 1 << 8;
        const WIDE          = 1 << 9;  // lead of a wide (CJK) cell
        const WIDE_SPACER   = 1 << 10; // trailing spacer of a wide cell
        const DOUBLE_UNDERLINE = 1 << 11;
        const CURLY_UNDERLINE  = 1 << 12;
        const DOTTED_UNDERLINE = 1 << 13;
        const DASHED_UNDERLINE = 1 << 14;
        /// Filler in the last column of a soft-wrapped row where a wide glyph
        /// did not fit and moved to the next row. Not content: reflow, copy
        /// and search skip it. Anywhere else the flag is ignored.
        const WIDE_PAD      = 1 << 15;
    }
}

impl CellAttrs {
    pub fn underline_styles() -> Self {
        Self::UNDERLINE
            | Self::DOUBLE_UNDERLINE
            | Self::CURLY_UNDERLINE
            | Self::DOTTED_UNDERLINE
            | Self::DASHED_UNDERLINE
    }
}
