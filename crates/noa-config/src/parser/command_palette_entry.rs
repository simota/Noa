//! `command-palette-entry = title:...,description:...,action:...`, parsed
//! the way Ghostty 1.4.0's `cli.args.parseAutoStruct` parses
//! `input.Command`: comma-separated `field:value` pairs, where a value in
//! double quotes is decoded as a Zig string literal.

use std::path::Path;

use crate::CommandPaletteEntryConfig;

use super::diagnostics::invalid_value_diagnostic;
use super::{Diagnostic, Directive};

pub(super) fn parse_command_palette_entry(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<CommandPaletteEntryConfig> {
    // Ghostty: an empty value brings the default entries back, and `clear`
    // drops every entry so far, the defaults included.
    let Some(value) = directive.value.as_deref() else {
        return Some(CommandPaletteEntryConfig::Reset);
    };
    if value == "clear" {
        return Some(CommandPaletteEntryConfig::Clear);
    }
    let parsed = parse_fields(value);
    if parsed.is_none() {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
    }
    parsed
}

fn parse_fields(value: &str) -> Option<CommandPaletteEntryConfig> {
    let (mut title, mut description, mut action) = (None, None, None);
    for field in split_fields(value)? {
        let (key, value) = field.split_once(':')?;
        let value = decode_value(value.trim())?;
        match key.trim() {
            "title" => title = Some(value),
            "description" => description = Some(value),
            "action" => action = Some(value),
            _ => return None,
        }
    }
    Some(CommandPaletteEntryConfig::Entry {
        title: title?,
        description: description.unwrap_or_default(),
        action: action?,
    })
}

/// Split on the commas outside double quotes, like Ghostty's
/// `CommaSplitter`: a backslash escapes the next character, inside quotes
/// or out, and a quote left open is an error. Quotes and escapes are kept.
fn split_fields(value: &str) -> Option<Vec<&str>> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut chars = value.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next()?;
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    fields.push(&value[start..]);
    Some(fields)
}

/// A value in double quotes is a Zig string literal; any other value is
/// taken as written.
fn decode_value(value: &str) -> Option<String> {
    let Some(literal) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return Some(value.to_string());
    };
    let mut bytes = Vec::with_capacity(literal.len());
    let mut chars = literal.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return None;
        }
        if c != '\\' {
            let mut buf = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next()? {
            'n' => bytes.push(b'\n'),
            'r' => bytes.push(b'\r'),
            't' => bytes.push(b'\t'),
            '\\' => bytes.push(b'\\'),
            '\'' => bytes.push(b'\''),
            '"' => bytes.push(b'"'),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() != 2 {
                    return None;
                }
                bytes.push(u8::from_str_radix(&hex, 16).ok()?);
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let hex: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let scalar = char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?;
                let mut buf = [0; 4];
                bytes.extend_from_slice(scalar.encode_utf8(&mut buf).as_bytes());
            }
            _ => return None,
        }
    }
    String::from_utf8(bytes).ok()
}
