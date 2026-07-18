//! SGR (Select Graphic Rendition) parameter decoding, including 256-color and
//! 24-bit truecolor in both semicolon (`38;2;r;g;b`) and colon
//! (`38:2::r:g:b`) forms.

use crate::csi::{Csi, MAX_PARAMS};
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
    let mut out = Vec::new();
    parse_sgr_into(csi, &mut out);
    out
}

/// Same decoding as [`parse_sgr`], but fills a caller-owned buffer instead of
/// allocating one. `out` is cleared first; reusing the same `Vec` across
/// calls (as [`crate::Stream`] does) keeps the hot SGR path allocation-free
/// after warm-up.
pub fn parse_sgr_into(csi: &Csi, out: &mut Vec<SgrAttr>) {
    out.clear();
    let p = csi.params();
    // Fast path: a lone truecolor pen (`38;2;r;g;b` / `48;2;r;g;b`) — the
    // per-cell shape SGR-dense floods emit. Output is identical to the
    // general loop below (for exactly 5 params, `parse_ext_color` reads
    // r/g/b from the same slots in both the semicolon and colon forms).
    if let [code @ (38 | 48), 2, r, g, b] = *p {
        let color = Color::Rgb(Rgb::new(r as u8, g as u8, b as u8));
        out.push(if code == 38 {
            SgrAttr::Fg(color)
        } else {
            SgrAttr::Bg(color)
        });
        return;
    }
    if p.is_empty() {
        out.push(SgrAttr::Reset);
        return;
    }
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
}

/// Lex the *plain SGR* sequence at the head of `bytes` — `ESC [ params m`
/// with params consisting only of digits/`;`/`:` (no private marker, no
/// intermediates) and a parameter count within the DFA's cap — returning its
/// total byte length. This is exactly the style unit
/// [`crate::Handler::print_sgr_ascii_lines`] admits; anything else (including
/// sequences the per-byte DFA would still accept, like `CSI > … m`) returns
/// `None` so the caller falls back to the regular dispatch paths.
///
/// Acceptance must stay aligned with `Stream`'s whole-CSI lexer: a byte
/// string this function accepts must produce, through
/// [`parse_plain_sgr_unit`], the same attribute list the DFA path dispatches
/// for it.
#[inline]
pub fn scan_plain_sgr(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 3 || bytes[0] != 0x1b || bytes[1] != b'[' {
        return None;
    }
    let mut i = 2;
    // Parameter count mirrors the DFA: one param per separator plus the
    // trailing one; the DFA's param-overflow quirk (fold into the last
    // param) is deferred to the per-byte path, like `try_scan_csi`.
    let mut nparams = 1usize;
    loop {
        match *bytes.get(i)? {
            0x30..=0x39 => {}
            0x3a | 0x3b => {
                nparams += 1;
                if nparams > MAX_PARAMS {
                    return None;
                }
            }
            b'm' => return Some(i + 1),
            _ => return None,
        }
        i += 1;
    }
}

/// Decode one [`scan_plain_sgr`]-shaped unit (`ESC [ params m`) into
/// attribute changes, clearing `out` first (same contract as
/// [`parse_sgr_into`]). The parameter accumulation matches the per-byte
/// DFA's exactly (saturating values, one param per separator, empty list
/// for `ESC [ m`), so the resulting attrs are bit-identical to what the
/// regular dispatch path hands [`crate::Handler::set_attributes`].
pub fn parse_plain_sgr_unit(unit: &[u8], out: &mut Vec<SgrAttr>) {
    debug_assert_eq!(scan_plain_sgr(unit), Some(unit.len()), "not a plain SGR unit");
    let mut params = crate::csi::Params::default();
    let mut sep_colon = crate::csi::Separators::default();
    let mut cur: u16 = 0;
    let mut any_params = false;
    for &b in &unit[2..unit.len() - 1] {
        match b {
            0x30..=0x39 => {
                cur = cur.saturating_mul(10).saturating_add(u16::from(b - 0x30));
                any_params = true;
            }
            _ => {
                // `scan_plain_sgr` admits only `:`/`;` here.
                params.push(cur);
                sep_colon.push(b == 0x3a);
                cur = 0;
                any_params = true;
            }
        }
    }
    if any_params {
        params.push(cur);
    }
    let csi = Csi::from_parts(
        params,
        sep_colon,
        crate::csi::Intermediates::default(),
        0,
        b'm',
    );
    parse_sgr_into(&csi, out);
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
