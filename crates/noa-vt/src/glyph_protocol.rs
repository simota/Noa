//! Glyph Protocol — request parser.
//!
//! The protocol rides on APC strings: `ESC _ 25a1 ; <verb> [; key=value]* ST`.
//! [`crate::Stream`] hands the bytes after `25a1;` to [`parse`], which
//! classifies the verb and keeps the options raw; the typed getters decode
//! one option on demand, applying the protocol defaults. Validating the glyph
//! payload, the glossary, and the replies are the grid layer's job.
//!
//! Ghostty analog: `terminal/apc/glyph/request.zig` (Ghostty 1.4.0).

/// The APC prefix that selects the Glyph Protocol, including the `;` that
/// ends the identifier.
pub const APC_PREFIX: &[u8] = b"25a1;";

/// A parsed Glyph Protocol request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlyphRequest {
    /// `s` — does the terminal implement the protocol, and which formats.
    Support,
    /// `q` — is a codepoint covered.
    Query(GlyphOptions),
    /// `r` — register a glyph outline at a codepoint.
    Register(GlyphRegister),
    /// `c` — clear one registration, or all of them.
    Clear(GlyphOptions),
}

/// The `key=value` options of a request, kept raw. A repeated key takes its
/// last value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphOptions {
    raw: Vec<u8>,
}

/// An `r` request: its options and the base64 payload after the last `;`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphRegister {
    pub options: GlyphOptions,
    pub payload: Vec<u8>,
}

/// The `fmt=` payload format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphFormat {
    Glyf,
    Colrv0,
    Colrv1,
}

/// The `reply=` verbosity of a registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphReply {
    /// `0` — never reply.
    None,
    /// `1` — reply on success and failure.
    All,
    /// `2` — reply on failure only.
    Failures,
}

/// The `size=` scale policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphSize {
    Height,
    Advance,
    Contain,
    Cover,
    Stretch,
}

/// The horizontal half of `align=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphAlignH {
    Start,
    Center,
    End,
}

/// The vertical half of `align=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphAlignV {
    Start,
    Center,
    End,
    Baseline,
}

/// The `pad=` insets, as fractions of the render span.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlyphPad {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// Parse the bytes after `25a1;`. `None` is a request Ghostty rejects
/// without a reply: an unknown verb, a verb not followed by `;`, or an `r`
/// with no `;` after its options.
pub fn parse(data: &[u8]) -> Option<GlyphRequest> {
    // A bare verb (`s`) is read as `s;`.
    let (&verb, rest) = data.split_first()?;
    let options = match rest {
        [] => &[][..],
        [b';', options @ ..] => options,
        _ => return None,
    };
    let options_of = |raw: &[u8]| GlyphOptions { raw: raw.to_vec() };
    Some(match verb {
        b's' => GlyphRequest::Support,
        b'q' => GlyphRequest::Query(options_of(options)),
        b'c' => GlyphRequest::Clear(options_of(options)),
        b'r' => {
            // The payload is everything after the last `;`, so the options
            // section must end in one.
            let split = options.iter().rposition(|&b| b == b';')?;
            GlyphRequest::Register(GlyphRegister {
                options: options_of(&options[..split]),
                payload: options[split + 1..].to_vec(),
            })
        }
        _ => return None,
    })
}

impl GlyphOptions {
    /// The value of the last `key=` option, if any (possibly empty).
    pub fn value(&self, key: &[u8]) -> Option<&[u8]> {
        self.raw.rsplit(|&b| b == b';').find_map(|option| {
            let eq = option.iter().position(|&b| b == b'=')?;
            (&option[..eq] == key).then(|| &option[eq + 1..])
        })
    }

    /// `cp=`: a hex codepoint up to Zig's `u21` range. `None` when absent or
    /// malformed.
    pub fn cp(&self) -> Option<u32> {
        parse_uint(self.value(b"cp")?, 16, 0x1F_FFFF)
    }
}

impl GlyphRegister {
    pub fn cp(&self) -> Option<u32> {
        self.options.cp()
    }

    /// `fmt=`, default `glyf`. `None` when present but unknown.
    pub fn format(&self) -> Option<GlyphFormat> {
        match self.options.value(b"fmt") {
            None | Some(b"glyf") => Some(GlyphFormat::Glyf),
            Some(b"colrv0") => Some(GlyphFormat::Colrv0),
            Some(b"colrv1") => Some(GlyphFormat::Colrv1),
            Some(_) => None,
        }
    }

    /// `reply=`, default `1`. An invalid value also means `1`.
    pub fn reply(&self) -> GlyphReply {
        match self.options.value(b"reply") {
            Some(b"0") => GlyphReply::None,
            Some(b"2") => GlyphReply::Failures,
            _ => GlyphReply::All,
        }
    }

    /// `upm=`, default 1000.
    pub fn units_per_em(&self) -> Option<u32> {
        self.decimal(b"upm").unwrap_or(Some(1000))
    }

    /// `aw=`, default `upm`.
    pub fn advance_width(&self) -> Option<u32> {
        self.decimal(b"aw").unwrap_or_else(|| self.units_per_em())
    }

    /// `lh=`, default `upm`.
    pub fn line_height(&self) -> Option<u32> {
        self.decimal(b"lh").unwrap_or_else(|| self.units_per_em())
    }

    /// `width=` (`1` or `2`), default 1.
    pub fn width(&self) -> Option<u8> {
        match self.options.value(b"width") {
            None | Some(b"1") => Some(1),
            Some(b"2") => Some(2),
            Some(_) => None,
        }
    }

    /// `size=`, default `height`.
    pub fn size(&self) -> Option<GlyphSize> {
        Some(match self.options.value(b"size") {
            None | Some(b"height") => GlyphSize::Height,
            Some(b"advance") => GlyphSize::Advance,
            Some(b"contain") => GlyphSize::Contain,
            Some(b"cover") => GlyphSize::Cover,
            Some(b"stretch") => GlyphSize::Stretch,
            Some(_) => return None,
        })
    }

    /// `align=<horizontal>,<vertical>`, default `center,center`.
    pub fn align(&self) -> Option<(GlyphAlignH, GlyphAlignV)> {
        let Some(value) = self.options.value(b"align") else {
            return Some((GlyphAlignH::Center, GlyphAlignV::Center));
        };
        let mut parts = value.split(|&b| b == b',');
        let horizontal = match parts.next()? {
            b"start" => GlyphAlignH::Start,
            b"center" => GlyphAlignH::Center,
            b"end" => GlyphAlignH::End,
            _ => return None,
        };
        let vertical = match parts.next()? {
            b"start" => GlyphAlignV::Start,
            b"center" => GlyphAlignV::Center,
            b"end" => GlyphAlignV::End,
            b"baseline" => GlyphAlignV::Baseline,
            _ => return None,
        };
        parts.next().is_none().then_some((horizontal, vertical))
    }

    /// `pad=<top>,<right>,<bottom>,<left>`, default all zero. Insets that
    /// leave no room (`left + right >= 1` or `top + bottom >= 1`) also mean
    /// zero, as the spec requires.
    pub fn pad(&self) -> Option<GlyphPad> {
        let Some(value) = self.options.value(b"pad") else {
            return Some(GlyphPad::default());
        };
        let mut parts = value.split(|&b| b == b',');
        let mut next = || parse_fraction(parts.next()?);
        let pad = GlyphPad {
            top: next()?,
            right: next()?,
            bottom: next()?,
            left: next()?,
        };
        if parts.next().is_some() {
            return None;
        }
        if pad.left + pad.right >= 1.0 || pad.top + pad.bottom >= 1.0 {
            return Some(GlyphPad::default());
        }
        Some(pad)
    }

    /// `Some(value)` when the option is present (`None` inside if it's
    /// malformed); `None` when absent.
    fn decimal(&self, key: &[u8]) -> Option<Option<u32>> {
        let value = self.options.value(key)?;
        Some(parse_uint(value, 10, u32::MAX))
    }
}

/// Zig's `std.fmt.parseInt` for an unsigned type with at most `max`: an
/// optional sign (`-` only for zero), and `_` allowed between digits.
fn parse_uint(value: &[u8], radix: u32, max: u32) -> Option<u32> {
    let (negative, digits) = match value {
        [b'+', rest @ ..] => (false, rest),
        [b'-', rest @ ..] => (true, rest),
        _ => (false, value),
    };
    if digits.is_empty() || digits.first() == Some(&b'_') || digits.last() == Some(&b'_') {
        return None;
    }
    let mut result: u32 = 0;
    for &b in digits.iter().filter(|&&b| b != b'_') {
        let digit = char::from(b).to_digit(radix)?;
        result = result.checked_mul(radix)?.checked_add(digit)?;
        if result > max {
            return None;
        }
    }
    (!negative || result == 0).then_some(result)
}

/// Ghostty's `fraction.parse`: a decimal in `[0, 1]` with an optional sign
/// and point (`0.5`, `.5`, `1.`, `1`), no exponent.
fn parse_fraction(value: &[u8]) -> Option<f64> {
    let (negative, digits) = match value {
        [b'+', rest @ ..] => (false, rest),
        [b'-', rest @ ..] => (true, rest),
        _ => (false, value),
    };
    let (int, frac) = match digits.iter().position(|&b| b == b'.') {
        Some(dot) => (&digits[..dot], &digits[dot + 1..]),
        None => (digits, &[][..]),
    };
    if int.len() + frac.len() == 0 || !int.iter().chain(frac).all(u8::is_ascii_digit) {
        return None;
    }
    let int_part = int
        .iter()
        .fold(0.0, |acc, &d| acc * 10.0 + f64::from(d - b'0'));
    // Like Ghostty, only the first 15 fractional digits count, so the
    // division rounds once.
    let (numerator, scale) = frac.iter().take(15).fold((0u64, 1u64), |(n, s), &d| {
        (n * 10 + u64::from(d - b'0'), s * 10)
    });
    let magnitude = int_part + numerator as f64 / scale as f64;
    let result = if negative { -magnitude } else { magnitude };
    (0.0..=1.0).contains(&result).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(data: &[u8]) -> GlyphRegister {
        match parse(data) {
            Some(GlyphRequest::Register(reg)) => reg,
            other => panic!("not a register request: {other:?}"),
        }
    }

    #[test]
    fn verbs() {
        assert_eq!(parse(b"s"), Some(GlyphRequest::Support));
        assert_eq!(parse(b"s;fmt=x"), Some(GlyphRequest::Support));
        let Some(GlyphRequest::Query(query)) = parse(b"q;cp=E0A0") else {
            panic!("not a query");
        };
        assert_eq!(query.cp(), Some(0xE0A0));
        let Some(GlyphRequest::Clear(clear)) = parse(b"c") else {
            panic!("not a clear");
        };
        assert_eq!(clear.value(b"cp"), None);
        for invalid in [&b""[..], b"x", b"x;", b"sx", b"r", b"r;cp=e0a0", b"r;foo"] {
            assert_eq!(parse(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn register_options_and_payload() {
        let reg = register(
            b"r;cp=e0a0;upm=2048;aw=1024;lh=1536;width=2;size=contain;\
              align=end,baseline;pad=0.1,0.2,0.3,0.4;reply=2;QQ==",
        );
        assert_eq!(reg.cp(), Some(0xE0A0));
        assert_eq!(reg.units_per_em(), Some(2048));
        assert_eq!(reg.advance_width(), Some(1024));
        assert_eq!(reg.line_height(), Some(1536));
        assert_eq!(reg.width(), Some(2));
        assert_eq!(reg.size(), Some(GlyphSize::Contain));
        assert_eq!(reg.align(), Some((GlyphAlignH::End, GlyphAlignV::Baseline)));
        assert_eq!(
            reg.pad(),
            Some(GlyphPad {
                top: 0.1,
                right: 0.2,
                bottom: 0.3,
                left: 0.4
            })
        );
        assert_eq!(reg.reply(), GlyphReply::Failures);
        assert_eq!(reg.payload, b"QQ==");
    }

    #[test]
    fn register_defaults() {
        let reg = register(b"r;cp=e0a0;QQ==");
        assert_eq!(reg.format(), Some(GlyphFormat::Glyf));
        assert_eq!(reg.reply(), GlyphReply::All);
        assert_eq!(reg.units_per_em(), Some(1000));
        assert_eq!(reg.advance_width(), Some(1000));
        assert_eq!(reg.line_height(), Some(1000));
        assert_eq!(reg.width(), Some(1));
        assert_eq!(reg.size(), Some(GlyphSize::Height));
        assert_eq!(
            reg.align(),
            Some((GlyphAlignH::Center, GlyphAlignV::Center))
        );
        assert_eq!(reg.pad(), Some(GlyphPad::default()));
        // `aw` and `lh` follow `upm`.
        let reg = register(b"r;cp=e0a0;upm=2048;QQ==");
        assert_eq!(reg.advance_width(), Some(2048));
        assert_eq!(reg.line_height(), Some(2048));
    }

    #[test]
    fn register_invalid_options() {
        let reg = register(
            b"r;cp=e0a0;fmt=svg;width=3;size=invalid;align=center,middle;pad=0,1.2,0,0;QQ==",
        );
        assert_eq!(reg.format(), None);
        assert_eq!(reg.width(), None);
        assert_eq!(reg.size(), None);
        assert_eq!(reg.align(), None);
        assert_eq!(reg.pad(), None);
        // An invalid reply falls back to 1; the last duplicate wins.
        assert_eq!(register(b"r;cp=e0a0;reply=9;QQ==").reply(), GlyphReply::All);
        assert_eq!(
            register(b"r;cp=e0a0;reply=1;reply=2;QQ==").reply(),
            GlyphReply::Failures
        );
        // Degenerate padding is no padding.
        assert_eq!(
            register(b"r;cp=e0a0;pad=0.4,0.2,0.6,0.1;QQ==").pad(),
            Some(GlyphPad::default())
        );
    }

    #[test]
    fn register_payload_is_after_the_last_semicolon() {
        let reg = register(b"r;cp=E0A0;status=4;reason=out_of_namespace");
        assert_eq!(reg.cp(), Some(0xE0A0));
        assert_eq!(reg.payload, b"reason=out_of_namespace");
    }

    #[test]
    fn integers_follow_zig_parse_int() {
        assert_eq!(parse_uint(b"e0a0", 16, 0x1F_FFFF), Some(0xE0A0));
        assert_eq!(parse_uint(b"+E0_A0", 16, 0x1F_FFFF), Some(0xE0A0));
        assert_eq!(parse_uint(b"-0", 16, 0x1F_FFFF), Some(0));
        assert_eq!(parse_uint(b"200000", 16, 0x1F_FFFF), None);
        for invalid in [&b""[..], b"+", b"-1", b"_1", b"1_", b"zz", b"0x10"] {
            assert_eq!(parse_uint(invalid, 16, 0x1F_FFFF), None, "{invalid:?}");
        }
    }

    #[test]
    fn fractions_follow_ghostty() {
        assert_eq!(parse_fraction(b"0.5"), Some(0.5));
        assert_eq!(parse_fraction(b".5"), Some(0.5));
        assert_eq!(parse_fraction(b"1."), Some(1.0));
        assert_eq!(parse_fraction(b"-0"), Some(-0.0));
        for invalid in [&b""[..], b".", b"-0.1", b"1.5", b"1e-1", b"0,5"] {
            assert_eq!(parse_fraction(invalid), None, "{invalid:?}");
        }
    }
}
