//! Kitty graphics — Unicode placeholder (`U+10EEEE`) decoding.
//!
//! A *virtual placement* (`U=1`) is never drawn where it was created; instead a
//! client prints placeholder cells — each carrying the base scalar `U+10EEEE` —
//! and the terminal draws a piece of the image in each such cell. The row and
//! column of the image piece, plus the target image id, are encoded entirely in
//! the cell's style:
//!
//! * **foreground color** → the image id's low bits (`Palette(n)` → 8 bits,
//!   `Rgb` → 24 bits),
//! * **first combining diacritic** → the image row,
//! * **second combining diacritic** → the image column,
//! * **third combining diacritic** → the image id's most-significant byte,
//! * **underline color** → the placement id (0 when unset).
//!
//! Missing row/column/most-significant-byte are inferred from the previous cell
//! in the same screen row: the row and MSB repeat, the column advances by one.
//! This lets a client specify only the first cell of each image row and let the
//! rest auto-fill.
//!
//! [`scan_row`] is a pure decoder: it turns one screen row's cells into fused
//! [`PlaceholderRun`]s (maximal same-image, same-row, contiguous-column spans),
//! which [`crate::Terminal`] then resolves against the stored virtual placement
//! and image to produce the source sub-rectangle for the renderer.
//!
//! Ghostty analog: `terminal/kitty/graphics_unicode.zig`.

use noa_core::Color;

use crate::cell::Cell;

/// The Kitty Unicode placeholder base scalar. A cell whose `ch` is this draws a
/// piece of a virtual placement's image instead of a glyph.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The 297 combining diacritics Kitty uses to encode row/column/most-significant
/// byte values, in value order (index = encoded value). Derived from Kitty's
/// `gen/rowcolumn-diacritics.txt` (combining class 230, no decomposition
/// mapping, from Unicode 6.0.0). The list is sorted by code point, so a binary
/// search recovers the value from a diacritic.
static ROWCOLUMN_DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The value a row/column diacritic encodes (its index in Kitty's table), or
/// `None` if `c` is not one of the recognized diacritics.
pub fn diacritic_value(c: char) -> Option<u32> {
    ROWCOLUMN_DIACRITICS
        .binary_search(&(c as u32))
        .ok()
        .map(|i| i as u32)
}

/// The image/placement id a placeholder color encodes: `Rgb` packs the full
/// 24-bit id, `Palette(n)` the low 8 bits, and `Default` means "unset".
fn color_id(c: Color) -> Option<u32> {
    match c {
        Color::Rgb(rgb) => {
            Some((u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b))
        }
        Color::Palette(n) => Some(u32::from(n)),
        Color::Default => None,
    }
}

/// A maximal run of placeholder cells on one screen row that map to a
/// contiguous horizontal strip of one image row (same image id, placement id,
/// and image row; columns advancing by one). The renderer draws each run as a
/// single quad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceholderRun {
    pub image_id: u32,
    pub placement_id: u32,
    /// The image's row index (from the first diacritic).
    pub virt_row: u32,
    /// The image column index of the run's first cell.
    pub virt_col_start: u32,
    /// Viewport column of the run's first cell.
    pub screen_x: u16,
    /// Number of cells (image columns) in the run.
    pub cols: u16,
}

/// Decode one screen row's cells into fused placeholder runs. Pure: it reads
/// only the cells, resolving the image/placement id and image row/column from
/// each placeholder cell's color and diacritics, and fuses adjacent cells that
/// continue the same image strip.
pub fn scan_row(cells: &[Cell]) -> Vec<PlaceholderRun> {
    let mut runs = Vec::new();
    let mut cur: Option<PlaceholderRun> = None;
    // Inference state across the row: the previous placeholder cell's image row,
    // image column, and most-significant id byte. Reset by any gap.
    let mut prev_row: Option<u32> = None;
    let mut prev_col: Option<u32> = None;
    let mut prev_msb: u32 = 0;

    for (x, cell) in cells.iter().enumerate() {
        // A non-placeholder cell, or one whose fg carries no id, breaks the run
        // and clears the inference state.
        if cell.ch != PLACEHOLDER {
            runs.extend(cur.take());
            prev_row = None;
            prev_col = None;
            prev_msb = 0;
            continue;
        }
        let Some(fg_low) = color_id(cell.fg) else {
            runs.extend(cur.take());
            prev_row = None;
            prev_col = None;
            prev_msb = 0;
            continue;
        };

        let mut dia = cell.combining().chars();
        let d_row = dia.next().and_then(diacritic_value);
        let d_col = dia.next().and_then(diacritic_value);
        let d_msb = dia.next().and_then(diacritic_value);

        let virt_row = d_row.or(prev_row).unwrap_or(0);
        let virt_col = d_col.or_else(|| prev_col.map(|c| c + 1)).unwrap_or(0);
        let msb = d_msb.unwrap_or(prev_msb);
        let image_id = (msb << 24) | (fg_low & 0x00FF_FFFF);
        let placement_id = cell.underline_color.and_then(color_id).unwrap_or(0);

        let extends = cur.as_ref().is_some_and(|run| {
            run.image_id == image_id
                && run.placement_id == placement_id
                && run.virt_row == virt_row
                && virt_col == run.virt_col_start + u32::from(run.cols)
                && x == usize::from(run.screen_x) + usize::from(run.cols)
        });
        if extends {
            cur.as_mut().unwrap().cols += 1;
        } else {
            runs.extend(cur.take());
            cur = Some(PlaceholderRun {
                image_id,
                placement_id,
                virt_row,
                virt_col_start: virt_col,
                screen_x: x as u16,
                cols: 1,
            });
        }

        prev_row = Some(virt_row);
        prev_col = Some(virt_col);
        prev_msb = msb;
    }
    runs.extend(cur.take());
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;

    /// The table is sorted and the round trip value→char→value is stable.
    #[test]
    fn diacritic_table_is_sorted_and_reversible() {
        assert!(ROWCOLUMN_DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        for (value, &cp) in ROWCOLUMN_DIACRITICS.iter().enumerate() {
            let c = char::from_u32(cp).unwrap();
            assert_eq!(diacritic_value(c), Some(value as u32));
        }
        // First three published mappings (Kitty docs): value 0/1/2.
        assert_eq!(diacritic_value('\u{0305}'), Some(0));
        assert_eq!(diacritic_value('\u{030D}'), Some(1));
        assert_eq!(diacritic_value('\u{030E}'), Some(2));
        // A non-diacritic scalar is rejected.
        assert_eq!(diacritic_value('a'), None);
    }

    fn placeholder(fg: Color, underline: Option<Color>, diacritics: &[char]) -> Cell {
        let mut cell = Cell {
            ch: PLACEHOLDER,
            fg,
            underline_color: underline,
            ..Default::default()
        };
        for &d in diacritics {
            cell.push_combining(d);
        }
        cell
    }

    /// A cell with explicit row+column diacritics decodes exactly, and the id
    /// comes from the fg color's low 24 bits.
    #[test]
    fn explicit_row_and_column() {
        let cell = placeholder(
            Color::Rgb(Rgb::new(0, 0, 7)),
            None,
            &['\u{030D}', '\u{030E}'], // row 1, column 2
        );
        let runs = scan_row(&[cell]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].image_id, 7);
        assert_eq!(runs[0].virt_row, 1);
        assert_eq!(runs[0].virt_col_start, 2);
        assert_eq!(runs[0].cols, 1);
    }

    /// Palette fg supplies only the low 8 bits of the id.
    #[test]
    fn palette_fg_is_low_byte_id() {
        let cell = placeholder(Color::Palette(42), None, &['\u{0305}', '\u{0305}']);
        let runs = scan_row(&[cell]);
        assert_eq!(runs[0].image_id, 42);
    }

    /// The third diacritic contributes the most-significant byte of the id.
    #[test]
    fn third_diacritic_is_id_high_byte() {
        let cell = placeholder(
            Color::Rgb(Rgb::new(0, 0, 1)),
            None,
            &['\u{0305}', '\u{0305}', '\u{030D}'], // msb = value 1
        );
        let runs = scan_row(&[cell]);
        assert_eq!(runs[0].image_id, (1 << 24) | 1);
    }

    /// The underline color carries the placement id.
    #[test]
    fn underline_color_is_placement_id() {
        let cell = placeholder(
            Color::Rgb(Rgb::new(0, 0, 5)),
            Some(Color::Rgb(Rgb::new(0, 1, 0))), // 0x000100 = 256
            &['\u{0305}', '\u{0305}'],
        );
        let runs = scan_row(&[cell]);
        assert_eq!(runs[0].placement_id, 256);
    }

    /// Omitted column advances by one, omitted row repeats: a bare run of four
    /// placeholder cells (only the first fully specified) fuses into one run.
    #[test]
    fn omitted_row_column_infer_and_fuse() {
        let fg = Color::Rgb(Rgb::new(0, 0, 3));
        let first = placeholder(fg, None, &['\u{0305}', '\u{0305}']); // row 0, col 0
        let bare = placeholder(fg, None, &[]); // infer row 0, col +1
        let cells = vec![first, bare, bare, bare];
        let runs = scan_row(&cells);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].virt_row, 0);
        assert_eq!(runs[0].virt_col_start, 0);
        assert_eq!(runs[0].cols, 4);
        assert_eq!(runs[0].screen_x, 0);
    }

    /// A different image row starts a new run rather than extending.
    #[test]
    fn row_change_splits_run() {
        let fg = Color::Rgb(Rgb::new(0, 0, 3));
        let a = placeholder(fg, None, &['\u{0305}', '\u{0305}']); // row 0 col 0
        let b = placeholder(fg, None, &['\u{030D}', '\u{030D}']); // row 1 col 1
        let runs = scan_row(&[a, b]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].virt_row, 0);
        assert_eq!(runs[1].virt_row, 1);
    }

    /// A non-placeholder cell between two placeholders breaks the run and resets
    /// column inference (the second run restarts at column 0).
    #[test]
    fn gap_breaks_run_and_resets_inference() {
        let fg = Color::Rgb(Rgb::new(0, 0, 3));
        let ph = placeholder(fg, None, &[]);
        let plain = Cell {
            ch: 'x',
            ..Default::default()
        };
        let cells = vec![ph, plain, ph];
        let runs = scan_row(&cells);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].screen_x, 0);
        assert_eq!(runs[0].virt_col_start, 0);
        assert_eq!(runs[1].screen_x, 2);
        assert_eq!(
            runs[1].virt_col_start, 0,
            "column inference resets after a gap"
        );
    }

    /// A placeholder cell without a foreground id is ignored (no image to draw).
    #[test]
    fn default_fg_yields_no_run() {
        let cell = placeholder(Color::Default, None, &['\u{0305}', '\u{0305}']);
        assert!(scan_row(&[cell]).is_empty());
    }

    /// A non-diacritic combining mark is treated as absent, so the value is
    /// inferred rather than mis-decoded.
    #[test]
    fn non_diacritic_combining_is_ignored() {
        // U+0301 (acute) is not in the table; row/column fall back to inference.
        let cell = placeholder(Color::Rgb(Rgb::new(0, 0, 1)), None, &['\u{0301}']);
        let runs = scan_row(&[cell]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].virt_row, 0);
        assert_eq!(runs[0].virt_col_start, 0);
    }
}
