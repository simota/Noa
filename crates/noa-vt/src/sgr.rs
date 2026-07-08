//! SGR (Select Graphic Rendition) parameter decoding, including 256-color and
//! 24-bit truecolor in both semicolon (`38;2;r;g;b`) and colon
//! (`38:2::r:g:b`) forms.

use crate::csi::Csi;
use noa_core::{Color, Rgb};

/// A decoded SGR attribute change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SgrAttr {
    Reset,
    Bold,
    Faint,
    Italic,
    Underline,
    DoubleUnderline,
    CurlyUnderline,
    DottedUnderline,
    DashedUnderline,
    Blink,
    Inverse,
    Invisible,
    Strike,
    Overline,
    ResetBold,
    ResetItalic,
    ResetUnderline,
    ResetBlink,
    ResetInverse,
    ResetInvisible,
    ResetStrike,
    ResetOverline,
    Fg(Color),
    Bg(Color),
    UnderlineColor(Color),
    DefaultFg,
    DefaultBg,
    DefaultUnderlineColor,
}

/// Decode an SGR (`CSI … m`) sequence into a list of attribute changes.
/// An empty parameter list means `SGR 0` (reset).
pub fn parse_sgr(csi: &Csi) -> Vec<SgrAttr> {
    let p = csi.params();
    if p.is_empty() {
        return vec![SgrAttr::Reset];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            0 => out.push(SgrAttr::Reset),
            1 => out.push(SgrAttr::Bold),
            2 => out.push(SgrAttr::Faint),
            3 => out.push(SgrAttr::Italic),
            4 => {
                if csi.separator_is_colon(i) {
                    match p.get(i + 1).copied().unwrap_or(1) {
                        0 => out.push(SgrAttr::ResetUnderline),
                        1 => out.push(SgrAttr::Underline),
                        2 => out.push(SgrAttr::DoubleUnderline),
                        3 => out.push(SgrAttr::CurlyUnderline),
                        4 => out.push(SgrAttr::DottedUnderline),
                        5 => out.push(SgrAttr::DashedUnderline),
                        _ => {}
                    }
                    i += 2;
                    continue;
                }
                out.push(SgrAttr::Underline);
            }
            5 | 6 => out.push(SgrAttr::Blink),
            7 => out.push(SgrAttr::Inverse),
            8 => out.push(SgrAttr::Invisible),
            9 => out.push(SgrAttr::Strike),
            21 => out.push(SgrAttr::DoubleUnderline),
            22 => out.push(SgrAttr::ResetBold),
            23 => out.push(SgrAttr::ResetItalic),
            24 => out.push(SgrAttr::ResetUnderline),
            25 => out.push(SgrAttr::ResetBlink),
            27 => out.push(SgrAttr::ResetInverse),
            28 => out.push(SgrAttr::ResetInvisible),
            29 => out.push(SgrAttr::ResetStrike),
            30..=37 => out.push(SgrAttr::Fg(Color::Palette((p[i] - 30) as u8))),
            38 => {
                let (c, adv) = parse_ext_color(csi, i);
                if let Some(col) = c {
                    out.push(SgrAttr::Fg(col));
                }
                i += adv;
                continue;
            }
            39 => out.push(SgrAttr::DefaultFg),
            40..=47 => out.push(SgrAttr::Bg(Color::Palette((p[i] - 40) as u8))),
            48 => {
                let (c, adv) = parse_ext_color(csi, i);
                if let Some(col) = c {
                    out.push(SgrAttr::Bg(col));
                }
                i += adv;
                continue;
            }
            49 => out.push(SgrAttr::DefaultBg),
            53 => out.push(SgrAttr::Overline),
            55 => out.push(SgrAttr::ResetOverline),
            58 => {
                let (c, adv) = parse_ext_color(csi, i);
                if let Some(col) = c {
                    out.push(SgrAttr::UnderlineColor(col));
                }
                i += adv;
                continue;
            }
            59 => out.push(SgrAttr::DefaultUnderlineColor),
            90..=97 => out.push(SgrAttr::Fg(Color::Palette((p[i] - 90 + 8) as u8))),
            100..=107 => out.push(SgrAttr::Bg(Color::Palette((p[i] - 100 + 8) as u8))),
            _ => {}
        }
        i += 1;
    }
    out
}

/// Parse an extended (38/48/58) color operand starting at index `i` (the
/// `38`/`48`/`58` code). Returns the color and how many params to advance past.
fn parse_ext_color(csi: &Csi, i: usize) -> (Option<Color>, usize) {
    let p = csi.params();
    match p.get(i + 1).copied() {
        Some(5) => {
            let n = p.get(i + 2).copied().unwrap_or(0) as u8;
            (Some(Color::Palette(n)), 3)
        }
        Some(2) => {
            // Colon form with an (often empty) colorspace field is `38:2:cs:r:g:b`
            // (6 params); semicolon form is `38;2;r;g;b` (5 params).
            let colon = csi.separator_is_colon(i + 1);
            let rgb_start = if colon && p.len() >= i + 6 {
                i + 3
            } else {
                i + 2
            };
            let r = p.get(rgb_start).copied().unwrap_or(0) as u8;
            let g = p.get(rgb_start + 1).copied().unwrap_or(0) as u8;
            let b = p.get(rgb_start + 2).copied().unwrap_or(0) as u8;
            (Some(Color::Rgb(Rgb::new(r, g, b))), (rgb_start + 3) - i)
        }
        _ => (None, 1),
    }
}
