//! Glyph Protocol (`ESC _ 25a1 ; … ST`) as Ghostty 1.4.0 runs it: a glossary
//! of registered Private Use Area codepoints, and the replies to `s`, `q`,
//! `r` and `c`.
//!
//! Ghostty 1.4.0 validates and stores registrations but doesn't draw them,
//! and its `q` reply reports only glossary coverage (never `system`). Noa
//! matches that, so the glossary keeps only the codepoints: a registration's
//! outline is validated and dropped.
//!
//! Ghostty analogs: `terminal/apc/glyph/{execute,Glossary,response}.zig` and
//! `font/opentype/glyf.zig`.

use noa_vt::glyph_protocol::{GlyphFormat, GlyphRegister, GlyphReply, GlyphRequest};

/// The spec's limit on registrations; registering past it evicts the oldest.
const MAX_ENTRIES: usize = 1024;

/// The spec's limit on the decoded payload.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Ghostty's limit on any one allocation while decoding a `glyf` record.
/// Only the point array can reach it.
const MAX_GLYF_ALLOCATION: usize = 64 * 1024;

/// The size of one decoded point in Ghostty (`x: i32, y: i32, on_curve:
/// bool`), which sets how many points fit in [`MAX_GLYF_ALLOCATION`]. 12 is
/// that struct's 4-byte-aligned size; it is inferred, not measured in Ghostty.
const GHOSTTY_POINT_BYTES: usize = 12;

/// The registered codepoints, oldest first.
#[derive(Debug, Default)]
pub(crate) struct Glossary {
    entries: Vec<u32>,
}

/// Why a registration failed, as its `reason=` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegisterError {
    OutOfNamespace,
    CompositeUnsupported,
    HintingUnsupported,
    MalformedPayload,
    PayloadTooLarge,
}

impl RegisterError {
    fn reason(self) -> &'static str {
        match self {
            Self::OutOfNamespace => "out_of_namespace",
            Self::CompositeUnsupported => "composite_unsupported",
            Self::HintingUnsupported => "hinting_unsupported",
            Self::MalformedPayload => "malformed_payload",
            Self::PayloadTooLarge => "payload_too_large",
        }
    }
}

impl Glossary {
    /// Run `req`, returning the reply to send, if any.
    pub(crate) fn execute(&mut self, req: &GlyphRequest) -> Option<String> {
        let body = match req {
            GlyphRequest::Support => "s;fmt=glyf".to_string(),
            GlyphRequest::Query(query) => {
                let cp = query.cp()?;
                let status = if self.entries.contains(&cp) {
                    "glossary"
                } else {
                    ""
                };
                format!("q;cp={cp:x};status={status}")
            }
            GlyphRequest::Register(reg) => {
                let result = self.register(reg);
                match (reg.reply(), result) {
                    (GlyphReply::None, _) | (GlyphReply::Failures, Ok(_)) => return None,
                    (GlyphReply::All, Ok(cp)) => format!("r;cp={cp:x};status=0"),
                    (_, Err(err)) => format!(
                        "r;cp={:x};status=1;reason={}",
                        reg.cp().unwrap_or(0),
                        err.reason()
                    ),
                }
            }
            GlyphRequest::Clear(clear) => match (clear.value(b"cp"), clear.cp()) {
                (None, _) => {
                    self.entries.clear();
                    "c;status=0".to_string()
                }
                (Some(_), None) => "c;status=1;reason=malformed_payload".to_string(),
                (Some(_), Some(cp)) if !is_private_use(cp) => {
                    "c;status=1;reason=out_of_namespace".to_string()
                }
                (Some(_), Some(cp)) => {
                    self.entries.retain(|&entry| entry != cp);
                    "c;status=0".to_string()
                }
            },
        };
        Some(format!("\x1b_25a1;{body}\x1b\\"))
    }

    /// Forget every registration (RIS).
    pub(crate) fn clear(&mut self) {
        self.entries = Vec::new();
    }

    /// Validate `reg` in Ghostty's order — options, then the payload, then
    /// the codepoint's range — and store its codepoint as the newest entry.
    fn register(&mut self, reg: &GlyphRegister) -> Result<u32, RegisterError> {
        use RegisterError::*;
        let cp = reg.cp().ok_or(MalformedPayload)?;
        let format = reg.format().ok_or(MalformedPayload)?;
        let metrics = [reg.units_per_em(), reg.advance_width(), reg.line_height()];
        if metrics.iter().any(|m| matches!(m, None | Some(0))) {
            return Err(MalformedPayload);
        }
        reg.width().ok_or(MalformedPayload)?;
        reg.size().ok_or(MalformedPayload)?;
        reg.align().ok_or(MalformedPayload)?;
        reg.pad().ok_or(MalformedPayload)?;
        match format {
            GlyphFormat::Glyf => validate_glyf(&decode_payload(&reg.payload)?)?,
            GlyphFormat::Colrv0 | GlyphFormat::Colrv1 => return Err(MalformedPayload),
        }
        if !is_private_use(cp) {
            return Err(OutOfNamespace);
        }
        self.entries.retain(|&entry| entry != cp);
        self.entries.push(cp);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        Ok(cp)
    }
}

fn is_private_use(cp: u32) -> bool {
    matches!(cp, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
}

/// Strict padded base64, as Zig's `std.base64.standard.Decoder`: the length
/// is a multiple of four, `=` only ends the last group, and the bits it pads
/// are zero. The size is checked from the length before the content.
fn decode_payload(payload: &[u8]) -> Result<Vec<u8>, RegisterError> {
    use RegisterError::*;
    if !payload.len().is_multiple_of(4) {
        return Err(MalformedPayload);
    }
    // Zig counts each of the last two symbols that is `=`, even "AB=C".
    let padded = |i: usize| payload.len() >= i && payload[payload.len() - i] == b'=';
    let size = payload.len() / 4 * 3 - usize::from(padded(1)) - usize::from(padded(2));
    if size > MAX_PAYLOAD_BYTES {
        return Err(PayloadTooLarge);
    }
    let padding = payload
        .iter()
        .rev()
        .take(2)
        .take_while(|&&b| b == b'=')
        .count();
    let data = &payload[..payload.len() - padding];
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in data {
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(MalformedPayload),
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // The bits the padding covers must be zero.
    if acc != 0 {
        return Err(MalformedPayload);
    }
    Ok(out)
}

/// Check that `data` is a simple TrueType glyph without instructions, the
/// way Ghostty decodes it (`Glyf.Entry.decode`), errors in its order.
fn validate_glyf(data: &[u8]) -> Result<(), RegisterError> {
    use RegisterError::*;
    let mut reader = Reader { data, pos: 0 };
    let contours = reader.i16()?;
    reader.skip(8)?; // xMin, yMin, xMax, yMax
    let Ok(contours) = usize::try_from(contours) else {
        return Err(CompositeUnsupported);
    };
    // A glyph with no contours may be just the header.
    if contours == 0 && reader.remaining() < 2 {
        return Ok(());
    }
    let mut last_end: Option<u16> = None;
    for _ in 0..contours {
        let end = reader.u16()?;
        if last_end.is_some_and(|last| end <= last) {
            return Err(MalformedPayload);
        }
        last_end = Some(end);
    }
    if reader.u16()? > 0 {
        return Err(HintingUnsupported);
    }
    let Some(last_end) = last_end else {
        return Ok(());
    };
    let points = usize::from(last_end) + 1;
    if points * GHOSTTY_POINT_BYTES > MAX_GLYF_ALLOCATION {
        return Err(PayloadTooLarge);
    }
    // Flags, then the x and y deltas they describe.
    let mut flags = Vec::with_capacity(points);
    while flags.len() < points {
        let flag = reader.u8()?;
        flags.push(flag);
        if flag & 0x08 != 0 {
            let repeat = usize::from(reader.u8()?);
            if flags.len() + repeat > points {
                return Err(MalformedPayload);
            }
            flags.extend(std::iter::repeat_n(flag, repeat));
        }
    }
    for (short, same_or_positive) in [(0x02, 0x10), (0x04, 0x20)] {
        for &flag in &flags {
            if flag & short != 0 {
                reader.skip(1)?;
            } else if flag & same_or_positive == 0 {
                reader.skip(2)?;
            }
        }
    }
    Ok(())
}

/// A big-endian reader whose every short read is a malformed payload.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn skip(&mut self, n: usize) -> Result<&[u8], RegisterError> {
        if self.remaining() < n {
            return Err(RegisterError::MalformedPayload);
        }
        self.pos += n;
        Ok(&self.data[self.pos - n..self.pos])
    }

    fn u8(&mut self) -> Result<u8, RegisterError> {
        Ok(self.skip(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, RegisterError> {
        let bytes = self.skip(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i16(&mut self) -> Result<i16, RegisterError> {
        Ok(self.u16()? as i16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ghostty's test triangle: one contour, three on-curve points.
    const TRIANGLE: &str = "AAEAZABkA4QDhAACAAABAQEB9P5wAyADhPzgAAA=";

    fn run(glossary: &mut Glossary, data: &[u8]) -> Option<String> {
        let req = noa_vt::glyph_protocol::parse(data).expect("valid request");
        glossary.execute(&req)
    }

    fn reply(body: &str) -> Option<String> {
        Some(format!("\x1b_25a1;{body}\x1b\\"))
    }

    fn b64(bytes: &[u8]) -> String {
        let mut out = Vec::new();
        crate::osc::encode_base64(bytes, &mut out);
        String::from_utf8(out).unwrap()
    }

    /// A simple glyph with `points` on-curve points at the origin, built
    /// like Ghostty's `testRepeatedPointRegisterReq`.
    fn repeated_points(points: usize) -> Vec<u8> {
        let mut glyf = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        glyf.extend_from_slice(&((points - 1) as u16).to_be_bytes());
        glyf.extend_from_slice(&[0, 0]);
        let mut remaining = points;
        while remaining > 0 {
            let run = remaining.min(256);
            if run == 1 {
                glyf.push(0x31);
            } else {
                glyf.extend_from_slice(&[0x39, (run - 1) as u8]);
            }
            remaining -= run;
        }
        glyf
    }

    #[test]
    fn support_advertises_glyf() {
        assert_eq!(run(&mut Glossary::default(), b"s"), reply("s;fmt=glyf"));
    }

    #[test]
    fn register_query_and_clear() {
        let mut g = Glossary::default();
        assert_eq!(run(&mut g, b"q;cp=E0A0"), reply("q;cp=e0a0;status="));
        let register = format!("r;cp=E0A0;{TRIANGLE}");
        assert_eq!(
            run(&mut g, register.as_bytes()),
            reply("r;cp=e0a0;status=0")
        );
        assert_eq!(
            run(&mut g, b"q;cp=e0a0"),
            reply("q;cp=e0a0;status=glossary")
        );
        assert_eq!(run(&mut g, b"c;cp=e0a0"), reply("c;status=0"));
        assert_eq!(run(&mut g, b"q;cp=e0a0"), reply("q;cp=e0a0;status="));
        // Clearing an empty slot still succeeds.
        assert_eq!(run(&mut g, b"c;cp=e0a0"), reply("c;status=0"));
        // A query without a usable `cp` gets no reply.
        assert_eq!(run(&mut g, b"q"), None);
        assert_eq!(run(&mut g, b"q;cp=zz"), None);
    }

    #[test]
    fn clear_all_and_clear_errors() {
        let mut g = Glossary::default();
        run(&mut g, format!("r;cp=e000;reply=0;{TRIANGLE}").as_bytes());
        run(&mut g, format!("r;cp=e001;reply=0;{TRIANGLE}").as_bytes());
        assert_eq!(g.entries, [0xE000, 0xE001]);
        assert_eq!(run(&mut g, b"c"), reply("c;status=0"));
        assert!(g.entries.is_empty());
        assert_eq!(
            run(&mut g, b"c;cp=41"),
            reply("c;status=1;reason=out_of_namespace")
        );
        for malformed in [&b"c;cp=zz"[..], b"c;cp=", b"c;cp=200000"] {
            assert_eq!(
                run(&mut g, malformed),
                reply("c;status=1;reason=malformed_payload"),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn reply_modes() {
        let mut g = Glossary::default();
        assert_eq!(
            run(&mut g, format!("r;cp=e0a0;reply=2;{TRIANGLE}").as_bytes()),
            None
        );
        assert_eq!(
            run(&mut g, format!("r;cp=41;reply=0;{TRIANGLE}").as_bytes()),
            None
        );
        assert_eq!(
            run(&mut g, format!("r;cp=41;reply=2;{TRIANGLE}").as_bytes()),
            reply("r;cp=41;status=1;reason=out_of_namespace")
        );
    }

    #[test]
    fn register_errors() {
        let mut g = Glossary::default();
        let cases: &[(&str, &str)] = &[
            // No `cp`: the reply says 0.
            (
                "r;upm=1000;QQ==",
                "r;cp=0;status=1;reason=malformed_payload",
            ),
            (
                "r;cp=e0a0;%%%not-base64%%%",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            (
                "r;cp=e0a0;fmt=colrv0;QQ==",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            (
                "r;cp=e0a0;upm=0;QQ==",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            (
                "r;cp=e0a0;width=3;QQ==",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            // One byte isn't a glyph header.
            (
                "r;cp=e0a0;QQ==",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            // Nonzero padding bits.
            (
                "r;cp=e0a0;QR==",
                "r;cp=e0a0;status=1;reason=malformed_payload",
            ),
            // A composite glyph (numberOfContours = -1).
            (
                "r;cp=e0a0;//8AAAAAAAAAAA==",
                "r;cp=e0a0;status=1;reason=composite_unsupported",
            ),
            // Instructions on a zero-contour glyph.
            (
                "r;cp=e0a0;AAAAAAAAAAAAAAAB",
                "r;cp=e0a0;status=1;reason=hinting_unsupported",
            ),
            // The payload is checked before the codepoint's range.
            ("r;cp=41;QQ==", "r;cp=41;status=1;reason=malformed_payload"),
        ];
        for (request, expected) in cases {
            assert_eq!(
                run(&mut g, request.as_bytes()),
                reply(expected),
                "{request}"
            );
        }
        assert!(g.entries.is_empty());
    }

    #[test]
    fn empty_glyph_registers() {
        let mut g = Glossary::default();
        assert_eq!(
            run(&mut g, b"r;cp=e0a0;AAAAAAAAAAAAAA=="),
            reply("r;cp=e0a0;status=0")
        );
    }

    #[test]
    fn payload_limits() {
        let mut g = Glossary::default();
        // 5461 points fit in Ghostty's 64 KiB point array; 5462 don't.
        let fits = format!("r;cp=e000;{}", b64(&repeated_points(5461)));
        assert_eq!(run(&mut g, fits.as_bytes()), reply("r;cp=e000;status=0"));
        let too_many = format!("r;cp=e000;{}", b64(&repeated_points(5462)));
        assert_eq!(
            run(&mut g, too_many.as_bytes()),
            reply("r;cp=e000;status=1;reason=payload_too_large")
        );
        // More than 64 KiB once decoded, judged from the length alone.
        let big = format!("r;cp=e000;{}", "A".repeat((MAX_PAYLOAD_BYTES / 3 + 1) * 4));
        assert_eq!(
            run(&mut g, big.as_bytes()),
            reply("r;cp=e000;status=1;reason=payload_too_large")
        );
    }

    #[test]
    fn reregistering_moves_to_newest_and_overflow_evicts_oldest() {
        let mut g = Glossary::default();
        for i in 0..MAX_ENTRIES as u32 {
            let request = format!("r;cp={:x};reply=0;{TRIANGLE}", 0xE000 + i);
            run(&mut g, request.as_bytes());
        }
        run(&mut g, format!("r;cp=e000;reply=0;{TRIANGLE}").as_bytes());
        run(&mut g, format!("r;cp=f000;reply=0;{TRIANGLE}").as_bytes());
        assert_eq!(g.entries.len(), MAX_ENTRIES);
        assert!(g.entries.contains(&0xE000));
        assert!(!g.entries.contains(&0xE001));
        assert_eq!(g.entries.last(), Some(&0xF000));
    }
}
