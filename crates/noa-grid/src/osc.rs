//! OSC terminal state handled by the grid layer.

use crate::cell::Hyperlink;
use noa_core::{DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, Rgb, xterm_palette};

const MAX_COLOR_OSC_BYTES: usize = 4096;
const DEFAULT_OSC52_MAX_DECODED_BYTES: usize = 3 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Osc52Policy {
    pub allow_write: bool,
    pub allow_read: bool,
    pub max_decoded_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HyperlinkOsc {
    Start(Hyperlink),
    End,
    Malformed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CwdOsc {
    Set(String),
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationOscKind {
    PromptStart,
    InputStart,
    CommandStart,
    CommandEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationOsc {
    Mark {
        kind: ShellIntegrationOscKind,
        exit_status: Option<i32>,
    },
    Malformed,
}

impl Default for Osc52Policy {
    fn default() -> Self {
        Self {
            allow_write: true,
            allow_read: false,
            max_decoded_bytes: DEFAULT_OSC52_MAX_DECODED_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalColors {
    base_fg: Rgb,
    base_bg: Rgb,
    base_cursor: Rgb,
    base_palette: [Rgb; 256],
    palette: [Option<Rgb>; 256],
    default_fg: Option<Rgb>,
    default_bg: Option<Rgb>,
    cursor: Option<Rgb>,
}

impl Default for TerminalColors {
    fn default() -> Self {
        Self {
            base_fg: DEFAULT_FG,
            base_bg: DEFAULT_BG,
            base_cursor: DEFAULT_CURSOR,
            base_palette: xterm_palette(),
            palette: [None; 256],
            default_fg: None,
            default_bg: None,
            cursor: None,
        }
    }
}

impl TerminalColors {
    pub fn base_default_fg(&self) -> Rgb {
        self.base_fg
    }

    pub fn base_default_bg(&self) -> Rgb {
        self.base_bg
    }

    pub fn base_cursor(&self) -> Rgb {
        self.base_cursor
    }

    pub fn base_palette(&self, index: u8) -> Rgb {
        self.base_palette[index as usize]
    }

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

    pub fn set_base_colors(
        &mut self,
        default_fg: Rgb,
        default_bg: Rgb,
        cursor: Rgb,
        palette: [Rgb; 256],
    ) {
        self.base_fg = default_fg;
        self.base_bg = default_bg;
        self.base_cursor = cursor;
        self.base_palette = palette;
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

    pub fn reset_dynamic_overrides(&mut self) {
        self.palette.fill(None);
        self.default_fg = None;
        self.default_bg = None;
        self.cursor = None;
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
            .unwrap_or_else(|| self.base_palette(index))
    }

    fn query_default_fg(&self) -> Rgb {
        self.default_fg.unwrap_or(self.base_fg)
    }

    fn query_default_bg(&self) -> Rgb {
        self.default_bg.unwrap_or(self.base_bg)
    }

    fn query_cursor(&self) -> Rgb {
        self.cursor.unwrap_or(self.base_cursor)
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

/// Handle OSC 52 clipboard read/write. Returns true for any OSC 52 payload,
/// even when the policy rejects the request.
pub(crate) fn handle_clipboard_osc(
    data: &[u8],
    policy: &Osc52Policy,
    pending_clipboard_writes: &mut Vec<String>,
    pending_clipboard_reads: &mut Vec<String>,
) -> bool {
    let Some(rest) = data.strip_prefix(b"52;") else {
        return false;
    };
    if !rest.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return true;
    }

    let Some(separator) = rest.iter().position(|&b| b == b';') else {
        return true;
    };
    let target = &rest[..separator];
    let payload = &rest[separator + 1..];
    if !osc52_targets_clipboard(target) {
        return true;
    }

    if payload == b"?" {
        // The grid can't read the system clipboard synchronously; queue a
        // request for the app layer to fulfill (and possibly prompt for).
        if policy.allow_read {
            pending_clipboard_reads.push("c".to_string());
        }
        return true;
    }

    if !policy.allow_write {
        return true;
    }
    let Some(decoded) = decode_base64_limited(payload, policy.max_decoded_bytes) else {
        return true;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return true;
    };
    pending_clipboard_writes.push(text);
    true
}

pub(crate) fn parse_hyperlink_osc(data: &[u8]) -> Option<HyperlinkOsc> {
    let rest = data.strip_prefix(b"8;")?;
    let Some(separator) = rest.iter().position(|&b| b == b';') else {
        return Some(HyperlinkOsc::Malformed);
    };
    let params = &rest[..separator];
    let uri = &rest[separator + 1..];

    if uri.is_empty() {
        return Some(HyperlinkOsc::End);
    }

    let Some(uri) = utf8_no_controls(uri) else {
        return Some(HyperlinkOsc::Malformed);
    };
    let Some(id) = hyperlink_id(params) else {
        return Some(HyperlinkOsc::Malformed);
    };

    Some(HyperlinkOsc::Start(Hyperlink { uri, id }))
}

/// A desktop notification requested via OSC 9 or OSC 777. `title` is `None`
/// for OSC 9 (body only); OSC 777's `notify` subcommand carries both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: Option<String>,
    pub body: String,
}

/// Parse OSC 9 (`9;<body>`) and OSC 777 (`777;notify;<title>;<body>`) desktop
/// notification requests. Returns `None` when `data` is neither, when the body
/// is empty, or when OSC 777's subcommand isn't `notify`. OSC 9's body is taken
/// whole (any `;` it contains is part of the body, iTerm2/kitty compatible);
/// OSC 777 splits only on the first `;` after the title, so its body may too.
pub(crate) fn parse_notification_osc(data: &[u8]) -> Option<Notification> {
    if let Some(body) = data.strip_prefix(b"9;") {
        if body.is_empty() {
            return None;
        }
        return Some(Notification {
            title: None,
            body: String::from_utf8_lossy(body).into_owned(),
        });
    }

    let rest = data.strip_prefix(b"777;")?;
    // Only the `notify` subcommand is supported; every other one is ignored.
    let after = rest.strip_prefix(b"notify;")?;
    let sep = after.iter().position(|&b| b == b';')?;
    let title = &after[..sep];
    let body = &after[sep + 1..];
    if body.is_empty() {
        return None;
    }
    Some(Notification {
        title: Some(String::from_utf8_lossy(title).into_owned()),
        body: String::from_utf8_lossy(body).into_owned(),
    })
}

pub(crate) fn parse_cwd_osc(data: &[u8]) -> Option<CwdOsc> {
    let uri = data.strip_prefix(b"7;")?;
    let Some(uri) = utf8_no_controls(uri) else {
        return Some(CwdOsc::Malformed);
    };
    let Some(path) = parse_file_uri_path(&uri) else {
        return Some(CwdOsc::Malformed);
    };
    Some(CwdOsc::Set(path))
}

pub(crate) fn parse_shell_integration_osc(data: &[u8]) -> Option<ShellIntegrationOsc> {
    let rest = data.strip_prefix(b"133;")?;
    if !rest.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return Some(ShellIntegrationOsc::Malformed);
    }

    let mut parts = rest.split(|&b| b == b';');
    let Some(action) = parts.next() else {
        return Some(ShellIntegrationOsc::Malformed);
    };

    let kind = match action {
        b"A" => ShellIntegrationOscKind::PromptStart,
        b"B" => ShellIntegrationOscKind::InputStart,
        b"C" => ShellIntegrationOscKind::CommandStart,
        b"D" => ShellIntegrationOscKind::CommandEnd,
        _ => return Some(ShellIntegrationOsc::Malformed),
    };
    let status = parts.next();
    if parts.next().is_some() {
        return Some(ShellIntegrationOsc::Malformed);
    }
    let exit_status = match (kind, status) {
        (ShellIntegrationOscKind::CommandEnd, None | Some(b"")) => None,
        (ShellIntegrationOscKind::CommandEnd, Some(status)) => {
            let Some(status) = parse_i32_ascii(status) else {
                return Some(ShellIntegrationOsc::Malformed);
            };
            Some(status)
        }
        (_, None) => None,
        (_, Some(_)) => return Some(ShellIntegrationOsc::Malformed),
    };

    Some(ShellIntegrationOsc::Mark { kind, exit_status })
}

fn hyperlink_id(params: &[u8]) -> Option<Option<String>> {
    if !params.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return None;
    }
    for param in params.split(|&b| b == b':') {
        let Some(id) = param.strip_prefix(b"id=") else {
            continue;
        };
        if id.is_empty() {
            return None;
        }
        return utf8_no_controls(id).map(Some);
    }
    Some(None)
}

fn parse_file_uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path_start = rest.find('/')?;
    let path = &rest[path_start..];
    if !path.starts_with('/') {
        return None;
    }
    let decoded = percent_decode_utf8(path)?;
    if decoded.starts_with('/') && !decoded.chars().any(char::is_control) {
        Some(decoded)
    } else {
        None
    }
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = percent_hex_value(bytes[i + 1])?;
            let lo = percent_hex_value(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_i32_ascii(bytes: &[u8]) -> Option<i32> {
    let s = std::str::from_utf8(bytes).ok()?;
    s.parse().ok()
}

fn utf8_no_controls(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8(bytes.to_vec()).ok()?;
    if s.chars().any(char::is_control) {
        None
    } else {
        Some(s)
    }
}

fn osc52_targets_clipboard(target: &[u8]) -> bool {
    target.is_empty() || target.contains(&b'c')
}

/// Build a full `OSC 52` reply (`ESC ] 52 ; <target> ; <base64(raw)> ST`)
/// for an accepted clipboard read, base64-encoding `raw`.
pub(crate) fn osc52_reply_bytes(target: &[u8], raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + raw.len() * 4 / 3 + 4);
    out.extend_from_slice(b"\x1b]52;");
    out.extend_from_slice(target);
    out.push(b';');
    encode_base64(raw, &mut out);
    out.extend_from_slice(b"\x1b\\");
    out
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648, with `=` padding), appended to `out`.
pub(crate) fn encode_base64(input: &[u8], out: &mut Vec<u8>) {
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        let n = ((b0 as u32) << 16) | ((b1.unwrap_or(0) as u32) << 8) | b2.unwrap_or(0) as u32;
        out.push(BASE64_ALPHABET[(n >> 18) as usize & 0x3f]);
        out.push(BASE64_ALPHABET[(n >> 12) as usize & 0x3f]);
        out.push(if b1.is_some() {
            BASE64_ALPHABET[(n >> 6) as usize & 0x3f]
        } else {
            b'='
        });
        out.push(if b2.is_some() {
            BASE64_ALPHABET[n as usize & 0x3f]
        } else {
            b'='
        });
    }
}

pub(crate) fn decode_base64_limited(input: &[u8], max_decoded_bytes: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() / 4) * 3);
    let mut quartet = [0u8; 4];
    let mut quartet_len = 0;
    let mut saw_padding = false;

    for &b in input {
        if b.is_ascii_whitespace() {
            continue;
        }
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                saw_padding = true;
                64
            }
            _ => return None,
        };
        if saw_padding && value != 64 {
            return None;
        }
        quartet[quartet_len] = value;
        quartet_len += 1;
        if quartet_len == 4 {
            push_decoded_quartet(&quartet, &mut out)?;
            if out.len() > max_decoded_bytes {
                return None;
            }
            quartet_len = 0;
        }
    }

    if quartet_len != 0 {
        return None;
    }
    Some(out)
}

fn push_decoded_quartet(q: &[u8; 4], out: &mut Vec<u8>) -> Option<()> {
    if q[0] == 64 || q[1] == 64 {
        return None;
    }
    out.push((q[0] << 2) | (q[1] >> 4));
    match (q[2], q[3]) {
        (64, 64) => {}
        (64, _) => return None,
        (c, 64) => out.push((q[1] << 4) | (c >> 2)),
        (c, d) => {
            out.push((q[1] << 4) | (c >> 2));
            out.push((c << 6) | d);
        }
    }
    Some(())
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
