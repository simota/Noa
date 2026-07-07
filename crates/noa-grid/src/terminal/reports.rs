use noa_core::{CellAttrs, Color};

use super::Terminal;

impl Terminal {
    pub(super) fn push_dcs_response(&mut self, body: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes.extend_from_slice(body);
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn push_decrqss_response(&mut self, valid: bool, request: &[u8], setting: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        if valid {
            self.pending_writes.extend_from_slice(b"1$r");
            self.pending_writes.extend_from_slice(setting);
        } else {
            self.pending_writes.extend_from_slice(b"0$r");
            self.pending_writes.extend_from_slice(request);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    pub(super) fn handle_decrqss(&mut self, request: &[u8]) {
        match request {
            b"m" => {
                let setting = self.current_sgr_report();
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b" q" => {
                let setting = format!("{} q", self.cursor_style_number());
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"r" => {
                let region = self.active().region;
                let setting = format!("{};{}r", region.top + 1, region.bottom + 1);
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"s" => {
                let (left, right) = self
                    .active()
                    .horizontal_margins
                    .map(|m| (m.left + 1, m.right + 1))
                    .unwrap_or((1, self.size.cols));
                let setting = format!("{left};{right}s");
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            _ => self.push_decrqss_response(false, request, &[]),
        }
    }

    fn cursor_style_number(&self) -> u8 {
        match self.active().cursor.style {
            crate::cursor::CursorStyle::BlinkingBlock => 1,
            crate::cursor::CursorStyle::SteadyBlock => 2,
            crate::cursor::CursorStyle::BlinkingUnderline => 3,
            crate::cursor::CursorStyle::SteadyUnderline => 4,
            crate::cursor::CursorStyle::BlinkingBar => 5,
            crate::cursor::CursorStyle::SteadyBar => 6,
        }
    }

    fn current_sgr_report(&self) -> String {
        let c = &self.active().cursor;
        let mut params = vec!["0".to_string()];
        if c.attrs.contains(CellAttrs::BOLD) {
            params.push("1".to_string());
        }
        if c.attrs.contains(CellAttrs::FAINT) {
            params.push("2".to_string());
        }
        if c.attrs.contains(CellAttrs::ITALIC) {
            params.push("3".to_string());
        }
        if c.attrs.contains(CellAttrs::UNDERLINE) {
            params.push("4".to_string());
        } else if c.attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
            params.push("21".to_string());
        } else if c.attrs.contains(CellAttrs::CURLY_UNDERLINE) {
            params.push("4:3".to_string());
        } else if c.attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
            params.push("4:4".to_string());
        } else if c.attrs.contains(CellAttrs::DASHED_UNDERLINE) {
            params.push("4:5".to_string());
        }
        if c.attrs.contains(CellAttrs::BLINK) {
            params.push("5".to_string());
        }
        if c.attrs.contains(CellAttrs::INVERSE) {
            params.push("7".to_string());
        }
        if c.attrs.contains(CellAttrs::INVISIBLE) {
            params.push("8".to_string());
        }
        if c.attrs.contains(CellAttrs::STRIKETHROUGH) {
            params.push("9".to_string());
        }
        if c.attrs.contains(CellAttrs::OVERLINE) {
            params.push("53".to_string());
        }
        push_color_params(&mut params, 30, 90, 38, c.fg);
        push_color_params(&mut params, 40, 100, 48, c.bg);
        if let Some(color) = c.underline_color {
            push_color_params(&mut params, 0, 0, 58, color);
        }
        format!("{}m", params.join(";"))
    }

    pub(super) fn handle_xtgettcap(&mut self, payload: &[u8]) {
        for encoded_name in payload
            .split(|&b| b == b';')
            .filter(|name| !name.is_empty())
        {
            let Some(name) = decode_xtgettcap_name(encoded_name) else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
                continue;
            };
            let value = match name.as_slice() {
                b"TN" => Some(b"noa".as_slice()),
                b"RGB" => Some(b"8:8:8".as_slice()),
                b"Co" => Some(b"256".as_slice()),
                _ => None,
            };
            if let Some(value) = value {
                self.push_xtgettcap_response(true, encoded_name, value);
            } else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
            }
        }
    }

    fn push_xtgettcap_response(&mut self, valid: bool, encoded_name: &[u8], value: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes
            .extend_from_slice(if valid { b"1+r" } else { b"0+r" });
        self.pending_writes.extend_from_slice(encoded_name);
        if valid {
            self.pending_writes.push(b'=');
            push_hex_bytes(&mut self.pending_writes, value);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }
}

fn push_color_params(
    params: &mut Vec<String>,
    base: u16,
    bright_base: u16,
    extended: u16,
    color: Color,
) {
    match color {
        Color::Default => {}
        Color::Palette(index) if index < 8 && base != 0 => {
            params.push((base + index as u16).to_string());
        }
        Color::Palette(index) if index < 16 && bright_base != 0 => {
            params.push((bright_base + index as u16 - 8).to_string());
        }
        Color::Palette(index) => params.push(format!("{extended};5;{index}")),
        Color::Rgb(rgb) => params.push(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

fn decode_xtgettcap_name(encoded: &[u8]) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.chunks_exact(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_hex_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
}
