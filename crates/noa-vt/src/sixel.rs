//! SIXEL graphics DCS control-data parser.
//!
//! SIXEL rides on DCS as `ESC P Pa;Pb;Ph q <sixel-data> ST`. The byte-level
//! parser captures the whole DCS body; this module only recognizes the SIXEL
//! introducer and splits its three positional parameters from the image data.

/// Parsed SIXEL graphics command from a completed DCS payload.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SixelGraphicsCommand {
    /// `Pa` — pixel aspect ratio. Kept for future scaling parity; v1 ignores it.
    pub aspect_ratio: u16,
    /// `Pb` — background option. `2` requests an opaque background; other
    /// values leave zero bits transparent in the v1 rasterizer.
    pub background: u16,
    /// `Ph` — horizontal grid size, kept for parity but ignored by xterm too.
    pub horizontal_grid_size: u16,
    /// Raw bytes after the DCS final `q`.
    pub data: Vec<u8>,
}

/// Parse `DCS ... ST` payload as SIXEL, returning `None` for other DCS
/// protocols such as DECRQSS (`$q`) and XTGETTCAP (`+q`).
pub fn parse(data: &[u8]) -> Option<SixelGraphicsCommand> {
    let final_q = data.iter().position(|&b| b == b'q')?;
    let introducer = &data[..final_q];
    if !introducer.iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return None;
    }

    let mut params = [0u16; 3];
    for (idx, part) in introducer.split(|&b| b == b';').take(3).enumerate() {
        if part.is_empty() {
            continue;
        }
        params[idx] = parse_u16(part);
    }

    Some(SixelGraphicsCommand {
        aspect_ratio: params[0],
        background: params[1],
        horizontal_grid_size: params[2],
        data: data[final_q + 1..].to_vec(),
    })
}

fn parse_u16(bytes: &[u8]) -> u16 {
    let mut value = 0u16;
    for &b in bytes {
        value = value.saturating_mul(10).saturating_add(u16::from(b - b'0'));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_params() {
        let cmd = parse(b"q~~").unwrap();
        assert_eq!(cmd.aspect_ratio, 0);
        assert_eq!(cmd.background, 0);
        assert_eq!(cmd.horizontal_grid_size, 0);
        assert_eq!(cmd.data, b"~~");
    }

    #[test]
    fn parses_three_params_and_payload() {
        let cmd = parse(b"1;2;3q#1~~").unwrap();
        assert_eq!(cmd.aspect_ratio, 1);
        assert_eq!(cmd.background, 2);
        assert_eq!(cmd.horizontal_grid_size, 3);
        assert_eq!(cmd.data, b"#1~~");
    }

    #[test]
    fn rejects_other_q_dcs_protocols() {
        assert!(parse(b"$qm").is_none());
        assert!(parse(b"+q544e").is_none());
    }
}
