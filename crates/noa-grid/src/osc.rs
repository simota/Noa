//! OSC terminal state handled by the grid layer.

use noa_core::{DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, Rgb, xterm_palette_color};

const MAX_COLOR_OSC_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalColors {
    palette: [Option<Rgb>; 256],
    default_fg: Option<Rgb>,
    default_bg: Option<Rgb>,
    cursor: Option<Rgb>,
}

impl Default for TerminalColors {
    fn default() -> Self {
        Self {
            palette: [None; 256],
            default_fg: None,
            default_bg: None,
            cursor: None,
        }
    }
}

impl TerminalColors {
    pub fn palette(&self, index: u8) -> Option<Rgb> {
        self.palette[index as usize]
    }

    pub fn default_fg(&self) -> Option<Rgb> {
        self.default_fg
    }

    pub fn default_bg(&self) -> Option<Rgb> {
        self.default_bg
    }

    pub fn cursor(&self) -> Option<Rgb> {
        self.cursor
    }

    pub fn set_palette(&mut self, index: u8, rgb: Rgb) {
        self.palette[index as usize] = Some(rgb);
    }

    pub fn reset_palette(&mut self, index: u8) {
        self.palette[index as usize] = None;
    }

    pub fn reset_all_palette(&mut self) {
        self.palette.fill(None);
    }

    pub fn set_default_fg(&mut self, rgb: Rgb) {
        self.default_fg = Some(rgb);
    }

    pub fn set_default_bg(&mut self, rgb: Rgb) {
        self.default_bg = Some(rgb);
    }

    pub fn set_cursor(&mut self, rgb: Rgb) {
        self.cursor = Some(rgb);
    }

    pub fn reset_default_fg(&mut self) {
        self.default_fg = None;
    }

    pub fn reset_default_bg(&mut self) {
        self.default_bg = None;
    }

    pub fn reset_cursor(&mut self) {
        self.cursor = None;
    }

    fn query_palette(&self, index: u8) -> Rgb {
        self.palette(index)
            .unwrap_or_else(|| xterm_palette_color(index))
    }

    fn query_default_fg(&self) -> Rgb {
        self.default_fg.unwrap_or(DEFAULT_FG)
    }

    fn query_default_bg(&self) -> Rgb {
        self.default_bg.unwrap_or(DEFAULT_BG)
    }

    fn query_cursor(&self) -> Rgb {
        self.cursor.unwrap_or(DEFAULT_CURSOR)
    }
}

#[derive(Clone, Copy)]
enum ColorSlot {
    DefaultFg,
    DefaultBg,
    Cursor,
}

/// Handle OSC 4/10/11/12 color set/query and OSC 104/110/111/112 reset forms.
///
/// Returns true when `data` names a color OSC command, even if individual
/// arguments are rejected.
pub(crate) fn handle_color_osc(
    data: &[u8],
    colors: &mut TerminalColors,
    pending_writes: &mut Vec<u8>,
) -> bool {
    if data.len() > MAX_COLOR_OSC_BYTES || !data.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return false;
    }

    let parts: Vec<&[u8]> = data.split(|&b| b == b';').collect();
    let Some((&code, params)) = parts.split_first() else {
        return false;
    };

    match code {
        b"4" => {
            handle_palette(params, colors, pending_writes);
            true
        }
        b"10" => {
            handle_single_color(b"10", ColorSlot::DefaultFg, params, colors, pending_writes);
            true
        }
        b"11" => {
            handle_single_color(b"11", ColorSlot::DefaultBg, params, colors, pending_writes);
            true
        }
        b"12" => {
            handle_single_color(b"12", ColorSlot::Cursor, params, colors, pending_writes);
            true
        }
        b"104" => {
            handle_palette_reset(params, colors);
            true
        }
        b"110" => {
            if params.is_empty() {
                colors.reset_default_fg();
            }
            true
        }
        b"111" => {
            if params.is_empty() {
                colors.reset_default_bg();
            }
            true
        }
        b"112" => {
            if params.is_empty() {
                colors.reset_cursor();
            }
            true
        }
        _ => false,
    }
}

fn handle_palette(params: &[&[u8]], colors: &mut TerminalColors, pending_writes: &mut Vec<u8>) {
    let mut i = 0;
    while i + 1 < params.len() {
        let Some(index) = parse_palette_index(params[i]) else {
            i += 2;
            continue;
        };
        let spec = params[i + 1];
        if spec == b"?" {
            push_palette_reply(pending_writes, index, colors.query_palette(index));
        } else if let Some(rgb) = parse_color_spec(spec) {
            colors.set_palette(index, rgb);
        }
        i += 2;
    }
}

fn handle_palette_reset(params: &[&[u8]], colors: &mut TerminalColors) {
    if params.is_empty() || params.iter().all(|p| p.is_empty()) {
        colors.reset_all_palette();
        return;
    }

    for param in params {
        if let Some(index) = parse_palette_index(param) {
            colors.reset_palette(index);
        }
    }
}

fn handle_single_color(
    code: &'static [u8],
    slot: ColorSlot,
    params: &[&[u8]],
    colors: &mut TerminalColors,
    pending_writes: &mut Vec<u8>,
) {
    if params.len() != 1 {
        return;
    }

    let value = params[0];
    if value == b"?" {
        let rgb = match slot {
            ColorSlot::DefaultFg => colors.query_default_fg(),
            ColorSlot::DefaultBg => colors.query_default_bg(),
            ColorSlot::Cursor => colors.query_cursor(),
        };
        push_single_color_reply(pending_writes, code, rgb);
        return;
    }

    let Some(rgb) = parse_color_spec(value) else {
        return;
    };
    match slot {
        ColorSlot::DefaultFg => colors.set_default_fg(rgb),
        ColorSlot::DefaultBg => colors.set_default_bg(rgb),
        ColorSlot::Cursor => colors.set_cursor(rgb),
    }
}

fn parse_palette_index(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || bytes.len() > 3 {
        return None;
    }
    let mut value: u16 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + u16::from(b - b'0');
        if value > u16::from(u8::MAX) {
            return None;
        }
    }
    Some(value as u8)
}

fn parse_color_spec(bytes: &[u8]) -> Option<Rgb> {
    if let Some(hex) = bytes.strip_prefix(b"#") {
        return parse_hash_color(hex);
    }
    if let Some(rest) = bytes.strip_prefix(b"rgb:") {
        return parse_rgb_color(rest);
    }
    None
}

fn parse_hash_color(hex: &[u8]) -> Option<Rgb> {
    if hex.len() != 6 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(Rgb::new(
        parse_hex_byte(&hex[0..2])?,
        parse_hex_byte(&hex[2..4])?,
        parse_hex_byte(&hex[4..6])?,
    ))
}

fn parse_rgb_color(rest: &[u8]) -> Option<Rgb> {
    let parts: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
    let [r, g, b] = parts.as_slice() else {
        return None;
    };
    Some(Rgb::new(
        parse_scaled_hex_component(r)?,
        parse_scaled_hex_component(g)?,
        parse_scaled_hex_component(b)?,
    ))
}

fn parse_hex_byte(bytes: &[u8]) -> Option<u8> {
    let [hi, lo] = bytes else {
        return None;
    };
    Some((hex_value(*hi)? << 4) | hex_value(*lo)?)
}

fn parse_scaled_hex_component(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || bytes.len() > 4 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }

    let mut value: u32 = 0;
    for &b in bytes {
        value = (value << 4) | u32::from(hex_value(b)?);
    }

    let max = (1u32 << (bytes.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_single_color_reply(pending_writes: &mut Vec<u8>, code: &[u8], rgb: Rgb) {
    pending_writes.extend_from_slice(b"\x1b]");
    pending_writes.extend_from_slice(code);
    pending_writes.push(b';');
    push_rgb_report(pending_writes, rgb);
    pending_writes.extend_from_slice(b"\x1b\\");
}

fn push_palette_reply(pending_writes: &mut Vec<u8>, index: u8, rgb: Rgb) {
    pending_writes.extend_from_slice(b"\x1b]4;");
    pending_writes.extend_from_slice(index.to_string().as_bytes());
    pending_writes.push(b';');
    push_rgb_report(pending_writes, rgb);
    pending_writes.extend_from_slice(b"\x1b\\");
}

fn push_rgb_report(pending_writes: &mut Vec<u8>, rgb: Rgb) {
    pending_writes.extend_from_slice(
        format!(
            "rgb:{:04x}/{:04x}/{:04x}",
            u16::from(rgb.r) * 0x0101,
            u16::from(rgb.g) * 0x0101,
            u16::from(rgb.b) * 0x0101
        )
        .as_bytes(),
    );
}
