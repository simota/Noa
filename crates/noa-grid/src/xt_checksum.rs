//! DECRQCRA (`CSI Pi ; Pg ; Pt ; Pl ; Pb ; Pr * y`, request the checksum of a
//! rectangle) and XTCHECKSUM (`CSI Ps # y`, choose how it is computed), as
//! Ghostty 1.4.0 does them. Ghostty follows xterm's `xtermCheckRect`.
//!
//! One gap: Noa cells don't record whether they were ever written, so a plain
//! space with no attributes counts as never written. Ghostty differs only
//! when such a space was printed explicitly and the DEC checksum would have
//! counted it as the first cell of the area.

use noa_core::CellAttrs;

use crate::cell::Cell;
use crate::screen::Screen;

/// The XTCHECKSUM bits (xterm's `CSBITS`). The default is the DEC checksum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XtChecksumFlags {
    /// Don't negate the result.
    pub positive: bool,
    /// Don't add the VT100 video attributes to each cell.
    pub no_attributes: bool,
    /// Don't omit blanks. The DEC checksum only counts a plain space if it
    /// is the first cell of the area.
    pub no_trim: bool,
    /// Count cells that were never written to as spaces instead of skipping
    /// them.
    pub undrawn: bool,
    /// Use the full codepoint rather than the DEC 8-bit value.
    pub full: bool,
}

impl XtChecksumFlags {
    /// The `Ps` of XTCHECKSUM. Bits beyond the five known ones are ignored,
    /// as in xterm.
    pub fn from_bits(bits: u16) -> Self {
        Self {
            positive: bits & 1 != 0,
            no_attributes: bits & 2 != 0,
            no_trim: bits & 4 != 0,
            undrawn: bits & 8 != 0,
            full: bits & 16 != 0,
        }
    }
}

/// A DECRQCRA rectangle: 1-based, zero meaning the parameter was omitted.
/// The page number is ignored since there is only one page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) top: u16,
    pub(crate) left: u16,
    pub(crate) bottom: u16,
    pub(crate) right: u16,
}

impl Rect {
    /// 0-based inclusive `(top, left, bottom, right)` in the active area, or
    /// `None` when empty. Under DECOM the coordinates are relative to the
    /// scroll region's top and the left margin, as in xterm; either way they
    /// are clamped to the screen.
    fn resolve(self, rows: u16, cols: u16, origin: Option<(u16, u16)>) -> Option<[u16; 4]> {
        if rows == 0 || cols == 0 {
            return None;
        }
        let (top_margin, left_margin) = origin.unwrap_or((0, 0));
        let limit = |v: u16, default: u16, margin: u16, max: u16| {
            let n = if v == 0 { default } else { v };
            n.saturating_add(margin).clamp(1, max) - 1
        };
        let top = limit(self.top, 1, top_margin, rows);
        let left = limit(self.left, 1, left_margin, cols);
        let bottom = limit(self.bottom, rows, top_margin, rows);
        let right = limit(self.right, cols, left_margin, cols);
        (top <= bottom && left <= right).then_some([top, left, bottom, right])
    }
}

/// The checksum of `rect` in `screen`'s active area, modulo 2^16.
pub(crate) fn compute(
    screen: &Screen,
    rect: Rect,
    origin: Option<(u16, u16)>,
    flags: XtChecksumFlags,
) -> u16 {
    // An empty rectangle still gets a reply, with a sum of zero.
    let Some([top, left, bottom, right]) = rect.resolve(screen.rows, screen.cols, origin) else {
        return 0;
    };
    let mut sum: u16 = 0;
    // The DEC checksum omits plain spaces except for the very first cell
    // counted in the area.
    let mut first = true;
    for y in top..=bottom {
        let Some(row) = screen.grid.get(usize::from(y)) else {
            continue;
        };
        let end = (usize::from(right) + 1).min(row.cells.len());
        let start = usize::from(left).min(end);
        for cell in &row.cells[start..end] {
            let mut ch = match value(cell, flags) {
                Value::Skip => continue,
                Value::Undrawn if flags.no_trim || flags.undrawn => u16::from(b' '),
                Value::Undrawn => continue,
                Value::Char(v) => v,
            };
            if !flags.no_attributes {
                ch = ch.wrapping_add(attributes(cell.attrs));
            }
            if flags.no_trim {
                sum = sum.wrapping_add(ch);
                // xterm adds combining marks only in the DEC mode, and only
                // to the untrimmed sum.
                if !flags.full {
                    for mark in cell.combining().chars() {
                        sum = sum.wrapping_add(u32::from(mark) as u16);
                    }
                }
            } else if first || ch != u16::from(b' ') || extended(cell.attrs) {
                sum = sum.wrapping_add(ch);
            }
            first = flags.no_trim;
        }
        if !flags.no_trim {
            first = false;
        }
    }
    if flags.positive {
        sum
    } else {
        0u16.wrapping_sub(sum)
    }
}

/// The DECRQCRA reply, `DCS Pi ! ~ XXXX ST`.
pub(crate) fn reply(id: u16, sum: u16) -> String {
    format!("\x1bP{id}!~{sum:04X}\x1b\\")
}

enum Value {
    /// Not counted at all.
    Skip,
    /// Never written to.
    Undrawn,
    /// The value of the character, before attributes.
    Char(u16),
}

/// Attributes that the checksum reads: a space carrying any of them was
/// written, not left blank.
fn written_attrs() -> CellAttrs {
    CellAttrs::BOLD
        | CellAttrs::FAINT
        | CellAttrs::ITALIC
        | CellAttrs::BLINK
        | CellAttrs::INVERSE
        | CellAttrs::INVISIBLE
        | CellAttrs::STRIKETHROUGH
        | CellAttrs::underline_styles()
}

fn value(cell: &Cell, flags: XtChecksumFlags) -> Value {
    // The right half of a wide character. xterm stores a placeholder outside
    // of 8 bits there, so the DEC checksum counts it as ESC.
    if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
        return if flags.full {
            Value::Skip
        } else {
            Value::Char(0x1B)
        };
    }
    if cell.ch == ' ' && cell.grapheme.is_none() && !cell.attrs.intersects(written_attrs()) {
        return Value::Undrawn;
    }
    let cp = u32::from(cell.ch);
    if flags.full {
        return Value::Char(cp as u16);
    }
    // The DEC 8-bit value, as xterm's `xtermCharSetDec` produces it for the
    // ASCII character set. Cells hold the translated character, so DEC line
    // drawing counts as outside of 8 bits (as in Ghostty).
    Value::Char(match cp {
        0x7F | 0xFF => 0,
        0x20..=0x7E | 0x80..=0x9F | 0xA1..=0xFE => (cp & 0x7F) as u16,
        _ => 0x1B,
    })
}

/// The VT100 video attributes xterm adds to each cell. Noa has no DECSCA
/// protection, so xterm's 0x04 never applies.
fn attributes(attrs: CellAttrs) -> u16 {
    let mut result = 0;
    if attrs.contains(CellAttrs::INVISIBLE) {
        result += 0x08;
    }
    if attrs.intersects(CellAttrs::underline_styles()) {
        result += 0x10;
    }
    if attrs.contains(CellAttrs::INVERSE) {
        result += 0x20;
    }
    if attrs.contains(CellAttrs::BLINK) {
        result += 0x40;
    }
    if attrs.contains(CellAttrs::BOLD) {
        result += 0x80;
    }
    result
}

/// Attributes xterm doesn't add to the sum but that still keep a space from
/// being trimmed.
fn extended(attrs: CellAttrs) -> bool {
    attrs.intersects(
        CellAttrs::FAINT
            | CellAttrs::ITALIC
            | CellAttrs::STRIKETHROUGH
            | CellAttrs::DOUBLE_UNDERLINE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_defaults_and_clamping() {
        assert_eq!(Rect::default().resolve(5, 10, None), Some([0, 0, 4, 9]));
        let rect = Rect {
            top: 2,
            left: 3,
            bottom: 99,
            right: 99,
        };
        assert_eq!(rect.resolve(5, 10, None), Some([1, 2, 4, 9]));
        let inverted = Rect {
            top: 3,
            bottom: 2,
            ..Rect::default()
        };
        assert_eq!(inverted.resolve(5, 10, None), None);
    }

    #[test]
    fn rect_is_relative_to_the_origin_under_decom() {
        assert_eq!(
            Rect::default().resolve(5, 10, Some((1, 2))),
            Some([1, 2, 4, 9])
        );
        let one = Rect {
            top: 1,
            left: 1,
            bottom: 1,
            right: 1,
        };
        assert_eq!(one.resolve(5, 10, Some((1, 2))), Some([1, 2, 1, 2]));
    }

    #[test]
    fn from_bits_ignores_unknown_bits() {
        assert_eq!(
            XtChecksumFlags::from_bits(0xFFE0 | 0b10101),
            XtChecksumFlags {
                positive: true,
                no_trim: true,
                full: true,
                ..XtChecksumFlags::default()
            }
        );
    }

    #[test]
    fn reply_is_hex_padded_to_four_digits() {
        assert_eq!(reply(65535, 0xFFFF), "\x1bP65535!~FFFF\x1b\\");
        assert_eq!(reply(1, 0x1A), "\x1bP1!~001A\x1b\\");
    }
}
