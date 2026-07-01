//! Cell rendition attributes (SGR state), stored as a compact bitset.

use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct CellAttrs: u16 {
        const BOLD          = 1 << 0;
        const FAINT         = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3; // single; richer underline styles land in inc>=2
        const BLINK         = 1 << 4;
        const INVERSE       = 1 << 5;
        const INVISIBLE     = 1 << 6;
        const STRIKETHROUGH = 1 << 7;
        const OVERLINE      = 1 << 8;
        const WIDE          = 1 << 9;  // lead of a wide (CJK) cell
        const WIDE_SPACER   = 1 << 10; // trailing spacer of a wide cell
    }
}
