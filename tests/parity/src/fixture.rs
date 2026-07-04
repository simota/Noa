//! The fixture file format: `## cols:`/`## rows:`/`## mode:` headers plus
//! `## input:`, `## expect:`, and `## why:` sections. See `README.md`.

use crate::dump::DumpMode;

/// One parsed fixture file.
pub struct Fixture {
    pub cols: u16,
    pub rows: u16,
    pub mode: DumpMode,
    /// The `## input:` section, unescaped to raw pty bytes.
    pub input: Vec<u8>,
    /// The `## expect:` section, verbatim (trailing blank lines stripped).
    /// Empty until the fixture is blessed.
    pub expect: String,
    /// The `## why:` section — what behavior the fixture pins, free text.
    pub why: String,
}

impl Fixture {
    /// Parse a fixture file. Errors carry a 1-based line number.
    pub fn parse(source: &str) -> Result<Fixture, String> {
        enum Section {
            Preamble,
            Input,
            Expect,
            Why,
        }
        let mut section = Section::Preamble;
        let mut cols: Option<u16> = None;
        let mut rows: Option<u16> = None;
        let mut mode: Option<DumpMode> = None;
        let mut input: Vec<u8> = Vec::new();
        let mut saw_input = false;
        let mut saw_expect = false;
        let mut expect_lines: Vec<&str> = Vec::new();
        let mut why_lines: Vec<&str> = Vec::new();

        for (idx, line) in source.lines().enumerate() {
            let lineno = idx + 1;
            if let Some(marker) = line.strip_prefix("## ") {
                if let Some(value) = marker.strip_prefix("cols:") {
                    cols = Some(parse_dim(value, lineno, "cols")?);
                } else if let Some(value) = marker.strip_prefix("rows:") {
                    rows = Some(parse_dim(value, lineno, "rows")?);
                } else if let Some(value) = marker.strip_prefix("mode:") {
                    mode = Some(match value.trim() {
                        "text" => DumpMode::Text,
                        "attrs" => DumpMode::Attrs,
                        other => {
                            return Err(format!("line {lineno}: unknown mode `{other}`"));
                        }
                    });
                } else if marker == "input:" {
                    section = Section::Input;
                    saw_input = true;
                } else if marker == "expect:" {
                    section = Section::Expect;
                    saw_expect = true;
                } else if marker == "why:" {
                    section = Section::Why;
                } else {
                    return Err(format!("line {lineno}: unknown marker `## {marker}`"));
                }
                continue;
            }
            match section {
                Section::Preamble => {
                    if !line.trim().is_empty() {
                        return Err(format!("line {lineno}: content outside any section"));
                    }
                }
                Section::Input => {
                    let bytes =
                        unescape(line).map_err(|err| format!("line {lineno}: {err}"))?;
                    input.extend(bytes);
                }
                Section::Expect => expect_lines.push(line),
                Section::Why => why_lines.push(line),
            }
        }

        // Both dump modes end with the `# cursor:` line, so trailing blank
        // lines in the expect section can only be cosmetic — drop them.
        while expect_lines.last().is_some_and(|l| l.trim().is_empty()) {
            expect_lines.pop();
        }

        let cols = cols.ok_or("missing `## cols:` header")?;
        let rows = rows.ok_or("missing `## rows:` header")?;
        let mode = mode.ok_or("missing `## mode:` header")?;
        if !saw_input {
            return Err("missing `## input:` section".into());
        }
        if !saw_expect {
            return Err("missing `## expect:` section".into());
        }
        if why_lines.iter().all(|l| l.trim().is_empty()) {
            return Err("missing or empty `## why:` section".into());
        }
        Ok(Fixture {
            cols,
            rows,
            mode,
            input,
            expect: expect_lines.join("\n"),
            why: why_lines.join("\n"),
        })
    }
}

fn parse_dim(value: &str, lineno: usize, key: &str) -> Result<u16, String> {
    let value: u16 = value
        .trim()
        .parse()
        .map_err(|_| format!("line {lineno}: bad {key} value `{}`", value.trim()))?;
    if value == 0 {
        return Err(format!("line {lineno}: {key} must be > 0"));
    }
    Ok(value)
}

/// Decode one `## input:` line to raw bytes.
///
/// Escapes: `\e` (ESC), `\r`, `\n`, `\t`, `\\`, `\xNN` (one raw byte from two
/// hex digits). Everything else is literal UTF-8. The line's trailing newline
/// is never part of the input — encode line breaks explicitly.
pub fn unescape(line: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars();
    let mut buf = [0u8; 4];
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hi = hex_digit(chars.next())?;
                let lo = hex_digit(chars.next())?;
                out.push(hi * 16 + lo);
            }
            Some(other) => return Err(format!("unknown escape `\\{other}`")),
            None => return Err("dangling `\\` at end of input line".into()),
        }
    }
    Ok(out)
}

fn hex_digit(c: Option<char>) -> Result<u8, String> {
    c.and_then(|c| c.to_digit(16))
        .map(|d| d as u8)
        .ok_or_else(|| "`\\x` needs two hex digits".into())
}

/// Rewrite the `## expect:` section of `source` with `actual`, leaving every
/// other byte of the file untouched (the bless workflow).
pub fn bless(source: &str, actual: &str) -> Result<String, String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|l| *l == "## expect:")
        .ok_or("no `## expect:` marker to bless")?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |offset| start + 1 + offset);

    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.extend_from_slice(&lines[..=start]);
    out.extend(actual.lines());
    out.extend_from_slice(&lines[end..]);
    let mut text = out.join("\n");
    text.push('\n');
    Ok(text)
}
