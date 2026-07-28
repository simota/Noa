//! Serialization of a scrollback tail to a self-contained byte buffer, and
//! back — the storage format behind `scrollback-persist`
//! (`docs/specs/scrollback-persistence.md`).
//!
//! Ghostty has no analog: Ghostty restores window topology but never terminal
//! contents, so this whole module is a noa extension gated behind an opt-in
//! config key.
//!
//! ## Why this does not serialize [`crate::scrollback::PagedScrollback`]
//!
//! The paged representation looks like the obvious thing to dump: it is already
//! compact and style-interned. It is not portable, because two of its ids are
//! *process*-scoped rather than page-scoped:
//!
//! - `GraphemeId` indexes a global `LazyLock` interner (`grapheme.rs`) whose
//!   numbering depends on the order a given run happened to see clusters in.
//! - `HyperlinkId` indexes `Terminal::hyperlinks`, which is per-`Terminal`.
//!
//! Writing those ids to disk would produce a file that decodes to different
//! text in the next process. So the wire format resolves both to their content
//! (the cluster's bytes, the link's URI) and rebuilds ids on load, and works at
//! the materialized [`Row`]/[`Cell`] level. Style interning is redone
//! per-snapshot, which recovers most of what the paged form would have saved;
//! the rest is recovered by deflate, which is very effective on grid data.
//!
//! ## Format (little-endian throughout)
//!
//! ```text
//! magic     6  b"NOASB\0"
//! version   2  u16
//! flags     2  u16   bit0 = body is deflate-compressed
//! cols      2  u16   grid width the rows were captured at
//! saved_at  8  u64   unix seconds, for the record-view label
//! rows      4  u32   row count
//! body      …  see encode_body
//! ```

use std::io::{Read, Write};

use noa_core::{CellAttrs, Color, Rgb};

use crate::cell::{Cell, Hyperlink, HyperlinkId, Row};

const MAGIC: &[u8; 6] = b"NOASB\0";
const VERSION: u16 = 1;
const FLAG_DEFLATE: u16 = 1 << 0;
const HEADER_LEN: usize = 24;

/// Fallback ceiling on the inflated body for callers that do not know the
/// configured budget (tests, the `dump-snapshot` example).
///
/// [`decode_within`] takes the real `scrollback-persist-limit` instead: a
/// snapshot is written by noa, but it is a file on disk that anything can
/// rewrite, and inflating 256 MiB from a few hundred compressed KiB at launch
/// is a freeze even though it is bounded.
const DEFAULT_MAX_DECODED_BODY: u64 = 256 << 20;

/// Encoded size of one cell in the body: `ch` + style index + grapheme index.
const CELL_ENCODED_BYTES: usize = 12;
/// Encoded per-row overhead: `wrapped` + cell count.
const ROW_ENCODED_BYTES: usize = 5;

/// Ceiling on a single persisted hyperlink target.
///
/// OSC 8 payloads are bounded only by the parser's 12 MiB `MAX_OSC_BYTES`, and
/// a link lives in the side table rather than in any row, so one of them can
/// carry a snapshot past its whole byte budget while the row walk sees nothing.
/// A target longer than this is not something anyone is going to click; the
/// cell's text is persisted either way, only the link is dropped.
const MAX_PERSISTED_LINK_BYTES: usize = 4096;

/// Color tags. `Option<Color>::None` (no underline color) needs a value
/// distinct from every `Some`, hence the fourth tag.
const TAG_DEFAULT: u32 = 0;
const TAG_PALETTE: u32 = 1;
const TAG_RGB: u32 = 2;
const TAG_NONE: u32 = 3;

/// A decoded scrollback tail: the rows themselves plus the hyperlink registry
/// their cells refer to. `Cell::hyperlink` ids are 1-based indices into
/// [`Self::hyperlinks`] and must be remapped into the target terminal's
/// registry before the rows are inserted — [`crate::Terminal::restore_scrollback_snapshot`]
/// does that.
/// The raw material for a snapshot: rows lifted out of a terminal, plus the
/// registry their cells index. Produced under the terminal lock, encoded
/// outside it.
#[derive(Clone, Debug)]
pub struct ScrollbackSnapshotInput {
    pub rows: Vec<Row>,
    pub cols: u16,
    pub hyperlinks: Vec<Hyperlink>,
}

#[derive(Clone, Debug)]
pub struct ScrollbackSnapshot {
    /// Grid width the rows were captured at. Rows are rewrapped when the
    /// restoring screen is a different width.
    pub cols: u16,
    /// Unix seconds at capture time, surfaced by the record-view separator.
    pub saved_at: u64,
    pub rows: Vec<Row>,
    pub hyperlinks: Vec<Hyperlink>,
}

fn encode_color(color: Color) -> u32 {
    match color {
        Color::Default => TAG_DEFAULT << 24,
        Color::Palette(index) => (TAG_PALETTE << 24) | u32::from(index),
        Color::Rgb(rgb) => {
            (TAG_RGB << 24) | (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
        }
    }
}

fn decode_color(raw: u32) -> Option<Color> {
    match raw >> 24 {
        TAG_DEFAULT => Some(Color::Default),
        TAG_PALETTE => Some(Color::Palette((raw & 0xff) as u8)),
        TAG_RGB => Some(Color::Rgb(Rgb::new(
            ((raw >> 16) & 0xff) as u8,
            ((raw >> 8) & 0xff) as u8,
            (raw & 0xff) as u8,
        ))),
        _ => None,
    }
}

fn encode_optional_color(color: Option<Color>) -> u32 {
    match color {
        None => TAG_NONE << 24,
        Some(color) => encode_color(color),
    }
}

fn decode_optional_color(raw: u32) -> Option<Option<Color>> {
    if raw >> 24 == TAG_NONE {
        Some(None)
    } else {
        decode_color(raw).map(Some)
    }
}

/// The style half of a cell — everything except the character itself. Interned
/// per snapshot so a screenful of same-pen text costs one table entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PackedStyle {
    fg: u32,
    bg: u32,
    underline: u32,
    attrs: u16,
    /// 1-based index into the snapshot's hyperlink table; `0` = no link.
    link: u32,
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

struct BodyWriter {
    out: Vec<u8>,
    styles: Vec<PackedStyle>,
    style_lookup: std::collections::HashMap<PackedStyle, u32>,
    graphemes: Vec<String>,
    grapheme_lookup: std::collections::HashMap<String, u32>,
    links: Vec<Hyperlink>,
    link_lookup: std::collections::HashMap<HyperlinkId, u32>,
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_str(out: &mut Vec<u8>, value: &str) {
    push_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

impl BodyWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            styles: Vec::new(),
            style_lookup: std::collections::HashMap::new(),
            graphemes: Vec::new(),
            grapheme_lookup: std::collections::HashMap::new(),
            links: Vec::new(),
            link_lookup: std::collections::HashMap::new(),
        }
    }

    /// Intern `id` against the source registry, returning a 1-based index into
    /// the snapshot-local table. A cell pointing at an id the terminal no
    /// longer knows about loses its link rather than failing the whole capture.
    fn intern_link(&mut self, id: HyperlinkId, registry: &[Hyperlink]) -> u32 {
        if let Some(&index) = self.link_lookup.get(&id) {
            return index;
        }
        let Some(link) = registry.get(id.get()) else {
            return 0;
        };
        if link.uri.len() + link.id.as_deref().map_or(0, str::len) > MAX_PERSISTED_LINK_BYTES {
            self.link_lookup.insert(id, 0);
            return 0;
        }
        self.links.push(link.clone());
        let index = self.links.len() as u32;
        self.link_lookup.insert(id, index);
        index
    }

    fn intern_grapheme(&mut self, tail: &str) -> u32 {
        if let Some(&index) = self.grapheme_lookup.get(tail) {
            return index;
        }
        self.graphemes.push(tail.to_owned());
        let index = self.graphemes.len() as u32;
        self.grapheme_lookup.insert(tail.to_owned(), index);
        index
    }

    fn intern_style(&mut self, cell: &Cell, registry: &[Hyperlink]) -> u32 {
        let link = cell
            .hyperlink
            .map(|id| self.intern_link(id, registry))
            .unwrap_or(0);
        let style = PackedStyle {
            fg: encode_color(cell.fg),
            bg: encode_color(cell.bg),
            underline: encode_optional_color(cell.underline_color),
            attrs: cell.attrs.bits(),
            link,
        };
        if let Some(&index) = self.style_lookup.get(&style) {
            return index;
        }
        self.styles.push(style);
        let index = (self.styles.len() - 1) as u32;
        self.style_lookup.insert(style, index);
        index
    }
}

/// Serialize `rows` into the NOASB body: tables first, then the rows that
/// reference them.
fn encode_body(rows: &[Row], registry: &[Hyperlink]) -> Vec<u8> {
    let mut writer = BodyWriter::new();

    // Rows are encoded into a scratch buffer first: interning them is what
    // populates the tables that must be written ahead of them.
    let mut row_bytes: Vec<u8> = Vec::new();
    for row in rows {
        let cells = trimmed_cells(row);
        row_bytes.push(u8::from(row.wrapped));
        push_u32(&mut row_bytes, cells.len() as u32);
        for cell in cells {
            let style = writer.intern_style(cell, registry);
            let combining = cell.combining();
            let grapheme = if combining.is_empty() {
                0
            } else {
                writer.intern_grapheme(combining)
            };
            push_u32(&mut row_bytes, cell.ch as u32);
            push_u32(&mut row_bytes, style);
            push_u32(&mut row_bytes, grapheme);
        }
    }

    let out = &mut writer.out;
    push_u32(out, writer.styles.len() as u32);
    for style in &writer.styles {
        push_u32(out, style.fg);
        push_u32(out, style.bg);
        push_u32(out, style.underline);
        push_u16(out, style.attrs);
        push_u32(out, style.link);
    }
    push_u32(out, writer.links.len() as u32);
    for link in &writer.links {
        push_str(out, &link.uri);
        match link.id.as_deref() {
            Some(id) => {
                push_u32(out, 1);
                push_str(out, id);
            }
            None => push_u32(out, 0),
        }
    }
    push_u32(out, writer.graphemes.len() as u32);
    for grapheme in &writer.graphemes {
        push_str(out, grapheme);
    }
    out.extend_from_slice(&row_bytes);
    writer.out
}

/// A row's cells with the trailing run of untouched blanks removed. A live
/// grid is mostly empty to the right of the cursor, and those cells carry no
/// information a restored record needs.
fn trimmed_cells(row: &Row) -> &[Cell] {
    let blank = Cell::default();
    let end = row
        .cells
        .iter()
        .rposition(|cell| *cell != blank)
        .map_or(0, |index| index + 1);
    &row.cells[..end]
}

/// Whether `row` holds nothing a restored record would show. The capture drops
/// a trailing run of these (the live grid is blank below the cursor); a leading
/// run inside the budget is kept, since it is history the program printed.
pub(crate) fn is_blank_row(row: &Row) -> bool {
    trimmed_cells(row).is_empty()
}

/// Encoded size of `row`, used to spend the caller's byte budget without
/// building the buffer twice.
pub(crate) fn encoded_row_size(row: &Row) -> usize {
    ROW_ENCODED_BYTES + trimmed_cells(row).len() * CELL_ENCODED_BYTES
}

/// Wrap a finished body in the NOASB header, deflating it.
fn frame(body: &[u8], cols: u16, saved_at: u64, rows: u32) -> Option<Vec<u8>> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).ok()?;
    let compressed = encoder.finish().ok()?;

    let mut out = Vec::with_capacity(HEADER_LEN + compressed.len());
    out.extend_from_slice(MAGIC);
    push_u16(&mut out, VERSION);
    push_u16(&mut out, FLAG_DEFLATE);
    push_u16(&mut out, cols);
    push_u64(&mut out, saved_at);
    push_u32(&mut out, rows);
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(&compressed);
    Some(out)
}

/// Serialize the newest rows of `rows` that fit in `max_bytes` of *encoded*
/// payload. The budget is deliberately measured before deflate: it is the
/// quantity the capture side can bound without compressing twice, and
/// compression only ever makes the file smaller than the promise.
///
/// Returns `None` when there is nothing worth saving — an empty tail, a zero
/// budget, or a tail that is entirely blank rows.
pub fn encode_tail(
    rows: &[Row],
    cols: u16,
    saved_at: u64,
    registry: &[Hyperlink],
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if max_bytes == 0 {
        return None;
    }
    // Walk backwards from the newest row, spending the budget, then keep that
    // suffix. A single row wider than the whole budget is still kept, so a
    // tiny limit degrades to "one row" rather than to "nothing".
    let mut spent = 0usize;
    let mut start = rows.len();
    for (index, row) in rows.iter().enumerate().rev() {
        let size = encoded_row_size(row);
        if spent + size > max_bytes && start < rows.len() {
            break;
        }
        spent += size;
        start = index;
    }
    if rows[start..]
        .iter()
        .all(|row| trimmed_cells(row).is_empty())
    {
        return None;
    }

    // The row walk above only accounts for row bodies. The style, hyperlink and
    // grapheme tables are written alongside them and are *not* bounded by cell
    // count: one OSC 8 URI can reach the parser's 12 MiB ceiling on its own, so
    // a link-heavy tail can blow a 1 MiB budget with a handful of rows. Encode
    // for real and drop the oldest rows until it fits, so the configured limit
    // is a limit rather than an estimate.
    loop {
        let tail = &rows[start..];
        let body = encode_body(tail, registry);
        if body.len() <= max_bytes || tail.len() <= 1 {
            return frame(&body, cols, saved_at, tail.len() as u32);
        }
        // Halve rather than step: the overshoot is usually a table entry the
        // per-row estimate cannot see, so a linear walk would re-encode the
        // whole tail once per row.
        start += (tail.len() / 2).max(1);
        if rows[start..].iter().all(|row| trimmed_cells(row).is_empty()) {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

struct BodyReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> BodyReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(len)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

/// Parse a NOASB buffer. Every malformed, truncated, wrong-version, or
/// wrong-magic input returns `None` — a snapshot is a convenience, and the
/// caller's contract is that a bad one degrades to "no record", never to a
/// failed launch.
pub fn decode(bytes: &[u8]) -> Option<ScrollbackSnapshot> {
    decode_within(bytes, DEFAULT_MAX_DECODED_BODY)
}

/// [`decode`], bounded by the caller's configured budget.
///
/// `max_body` is a hard reject rather than a truncation: a body that does not
/// fit was not written by a noa honoring the same limit, and half a record is
/// worse than none.
pub fn decode_within(bytes: &[u8], max_body: u64) -> Option<ScrollbackSnapshot> {
    let header = bytes.get(..HEADER_LEN)?;
    if &header[..6] != MAGIC {
        return None;
    }
    let version = u16::from_le_bytes(header[6..8].try_into().ok()?);
    if version != VERSION {
        return None;
    }
    let flags = u16::from_le_bytes(header[8..10].try_into().ok()?);
    let cols = u16::from_le_bytes(header[10..12].try_into().ok()?);
    let saved_at = u64::from_le_bytes(header[12..20].try_into().ok()?);
    let row_count = u32::from_le_bytes(header[20..24].try_into().ok()?) as usize;

    let raw = &bytes[HEADER_LEN..];
    let body = if flags & FLAG_DEFLATE != 0 {
        let mut decoded = Vec::new();
        // `take(n + 1)`: reading exactly the ceiling cannot distinguish "fits"
        // from "truncated here", and a silently truncated body decodes to a
        // plausible-looking short record.
        flate2::read::DeflateDecoder::new(raw)
            .take(max_body.saturating_add(1))
            .read_to_end(&mut decoded)
            .ok()?;
        if decoded.len() as u64 > max_body {
            return None;
        }
        decoded
    } else {
        if raw.len() as u64 > max_body {
            return None;
        }
        raw.to_vec()
    };

    let mut reader = BodyReader::new(&body);

    let style_count = reader.u32()? as usize;
    let mut styles = Vec::with_capacity(style_count.min(4096));
    for _ in 0..style_count {
        let fg = decode_color(reader.u32()?)?;
        let bg = decode_color(reader.u32()?)?;
        let underline = decode_optional_color(reader.u32()?)?;
        let attrs = CellAttrs::from_bits_truncate(reader.u16()?);
        let link = reader.u32()?;
        styles.push((fg, bg, underline, attrs, link));
    }

    let link_count = reader.u32()? as usize;
    let mut hyperlinks = Vec::with_capacity(link_count.min(4096));
    for _ in 0..link_count {
        let uri = reader.string()?;
        let id = match reader.u32()? {
            0 => None,
            _ => Some(reader.string()?),
        };
        hyperlinks.push(Hyperlink { uri, id });
    }

    let grapheme_count = reader.u32()? as usize;
    let mut graphemes = Vec::with_capacity(grapheme_count.min(4096));
    for _ in 0..grapheme_count {
        graphemes.push(reader.string()?);
    }

    let mut rows = Vec::with_capacity(row_count.min(4096));
    for _ in 0..row_count {
        let wrapped = reader.take(1)?[0] != 0;
        let cell_count = reader.u32()? as usize;
        let mut cells = Vec::with_capacity(cell_count.min(4096));
        for _ in 0..cell_count {
            let ch = char::from_u32(reader.u32()?)?;
            let style_index = reader.u32()? as usize;
            let grapheme_index = reader.u32()? as usize;
            let &(fg, bg, underline, attrs, link) = styles.get(style_index)?;
            let mut cell = Cell {
                ch,
                fg,
                bg,
                underline_color: underline,
                // Snapshot-local index into `hyperlinks` (the wire value is
                // 1-based so `0` can mean "no link"); remapped into the target
                // terminal's registry by
                // `Terminal::restore_scrollback_snapshot`.
                // Bounded against the table decoded above: an id past its end
                // would otherwise be handed to callers as a live registry index
                // and adopt an unrelated URI.
                hyperlink: link
                    .checked_sub(1)
                    .filter(|index| (*index as usize) < hyperlinks.len())
                    .and_then(|index| HyperlinkId::new(index as usize)),
                attrs,
                grapheme: None,
            };
            if grapheme_index != 0 {
                cell.set_combining(graphemes.get(grapheme_index - 1)?);
            }
            cells.push(cell);
        }
        rows.push(Row::from_cells(cells, wrapped, false));
    }

    Some(ScrollbackSnapshot {
        cols,
        saved_at,
        rows,
        hyperlinks,
    })
}

// ---------------------------------------------------------------------------
// Rewrapping
// ---------------------------------------------------------------------------

/// Re-lay `rows` for a screen `cols` wide, preserving styles.
///
/// A snapshot captured in an 80-column window and restored into a 200-column
/// one would otherwise show its soft-wraps frozen at the old width. Rows are
/// joined back into logical lines along their `wrapped` flags and re-split, so
/// restored history wraps like live history does.
///
/// A wide (CJK) glyph and its spacer are never separated: when a split would
/// land between them, the lead moves to the next row and the vacated column is
/// left blank — the same choice the live reflow makes.
pub fn rewrap(rows: Vec<Row>, cols: u16) -> Vec<Row> {
    if cols == 0 {
        return Vec::new();
    }
    let width = usize::from(cols);
    if rows.iter().all(|row| row.cells.len() == width) && rows.iter().all(|row| !row.wrapped) {
        return rows;
    }

    let mut out = Vec::with_capacity(rows.len());
    let mut logical: Vec<Cell> = Vec::new();
    for row in rows {
        let continues = row.wrapped;
        let trimmed = trimmed_cells(&row).len();
        logical.extend_from_slice(&row.cells[..trimmed]);
        if continues {
            continue;
        }
        emit_logical_line(&logical, width, &mut out);
        logical.clear();
    }
    if !logical.is_empty() {
        emit_logical_line(&logical, width, &mut out);
    }
    out
}

fn emit_logical_line(line: &[Cell], width: usize, out: &mut Vec<Row>) {
    if line.is_empty() {
        out.push(Row::from_cells(vec![Cell::default(); width], false, false));
        return;
    }
    let mut start = 0usize;
    while start < line.len() {
        let mut end = (start + width).min(line.len());
        // Never strand a wide lead from its spacer across the split — but never
        // back off past `start` either: at width 1 a wide glyph would otherwise
        // make `end == start`, and the loop would emit blank rows forever
        // without consuming a single cell.
        if end < line.len()
            && end > start + 1
            && line[end].attrs.contains(CellAttrs::WIDE_SPACER)
            && line[end - 1].attrs.contains(CellAttrs::WIDE)
        {
            end -= 1;
        }
        debug_assert!(end > start, "every iteration must consume at least one cell");
        let mut cells = line[start..end].to_vec();
        cells.resize(width, Cell::default());
        let wrapped = end < line.len();
        out.push(Row::from_cells(cells, wrapped, false));
        start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(ch: char, fg: Color, attrs: CellAttrs) -> Cell {
        Cell {
            ch,
            fg,
            attrs,
            ..Cell::default()
        }
    }

    fn row_of(text: &str, cols: usize) -> Row {
        let mut cells: Vec<Cell> = text
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        cells.resize(cols, Cell::default());
        Row::from_cells(cells, false, false)
    }

    #[test]
    fn roundtrip_preserves_every_cell_field() {
        let mut cell = styled('A', Color::Rgb(Rgb::new(1, 2, 3)), CellAttrs::BOLD);
        cell.bg = Color::Palette(42);
        cell.underline_color = Some(Color::Palette(7));
        cell.hyperlink = HyperlinkId::new(0);
        let registry = vec![Hyperlink {
            uri: "https://example.com".into(),
            id: Some("anchor".into()),
        }];
        let row = Row::from_cells(vec![cell, Cell::default()], true, false);

        let bytes = encode_tail(&[row], 2, 1234, &registry, 4096).expect("non-empty tail encodes");
        let decoded = decode(&bytes).expect("roundtrip decodes");

        assert_eq!(decoded.cols, 2);
        assert_eq!(decoded.saved_at, 1234);
        assert_eq!(decoded.hyperlinks, registry);
        assert_eq!(decoded.rows.len(), 1);
        let restored = &decoded.rows[0];
        assert!(restored.wrapped);
        assert_eq!(restored.cells[0].ch, 'A');
        assert_eq!(restored.cells[0].fg, Color::Rgb(Rgb::new(1, 2, 3)));
        assert_eq!(restored.cells[0].bg, Color::Palette(42));
        assert_eq!(restored.cells[0].underline_color, Some(Color::Palette(7)));
        assert_eq!(restored.cells[0].attrs, CellAttrs::BOLD);
        assert_eq!(restored.cells[0].hyperlink, HyperlinkId::new(0));
    }

    #[test]
    fn roundtrip_preserves_a_combining_cluster() {
        let mut cell = Cell {
            ch: 'e',
            ..Cell::default()
        };
        cell.set_combining("\u{301}");
        let row = Row::from_cells(vec![cell], false, false);

        let bytes = encode_tail(&[row], 1, 0, &[], 4096).expect("encodes");
        let decoded = decode(&bytes).expect("decodes");
        assert_eq!(decoded.rows[0].cells[0].ch, 'e');
        assert_eq!(decoded.rows[0].cells[0].combining(), "\u{301}");
    }

    #[test]
    fn trailing_blanks_are_dropped_but_interior_ones_survive() {
        let row = row_of("a b", 40);
        let bytes = encode_tail(&[row], 40, 0, &[], 4096).expect("encodes");
        let decoded = decode(&bytes).expect("decodes");
        // "a b" — the blank between the words is interior and kept; the 37
        // untouched columns after it are not.
        assert_eq!(decoded.rows[0].cells.len(), 3);
        assert_eq!(decoded.rows[0].cells[1].ch, ' ');
    }

    #[test]
    fn the_budget_keeps_the_newest_rows() {
        let rows: Vec<Row> = (0..10).map(|i| row_of(&format!("row{i}"), 8)).collect();
        // Room for roughly two rows.
        let budget = 2 * (ROW_ENCODED_BYTES + 4 * CELL_ENCODED_BYTES);
        let bytes = encode_tail(&rows, 8, 0, &[], budget).expect("encodes");
        let decoded = decode(&bytes).expect("decodes");
        assert!(decoded.rows.len() < 10, "budget must drop older rows");
        let last: String = decoded.rows.last().unwrap().cells.iter().map(|c| c.ch).collect();
        assert!(last.starts_with("row9"), "newest row must survive: {last:?}");
    }

    #[test]
    fn a_row_larger_than_the_whole_budget_is_still_kept() {
        let rows = vec![row_of("hello", 80)];
        let bytes = encode_tail(&rows, 80, 0, &[], 1).expect("one oversized row still encodes");
        assert_eq!(decode(&bytes).expect("decodes").rows.len(), 1);
    }

    #[test]
    fn an_all_blank_tail_encodes_to_nothing() {
        let rows = vec![row_of("", 80), row_of("", 80)];
        assert!(encode_tail(&rows, 80, 0, &[], 4096).is_none());
        assert!(encode_tail(&[row_of("x", 80)], 80, 0, &[], 0).is_none());
    }

    #[test]
    fn corrupt_input_decodes_to_none_rather_than_panicking() {
        assert!(decode(b"").is_none());
        assert!(decode(b"not a snapshot at all, really").is_none());

        let good = encode_tail(&[row_of("hi", 8)], 8, 0, &[], 4096).expect("encodes");
        // Wrong magic.
        let mut wrong_magic = good.clone();
        wrong_magic[0] = b'X';
        assert!(decode(&wrong_magic).is_none());
        // Wrong version.
        let mut wrong_version = good.clone();
        wrong_version[6] = 9;
        assert!(decode(&wrong_version).is_none());
        // Truncated body.
        assert!(decode(&good[..good.len() - 3]).is_none());
        // Header claiming more rows than the body holds.
        let mut lying_header = good.clone();
        lying_header[20] = 200;
        assert!(decode(&lying_header).is_none());
    }

    #[test]
    fn identical_styles_share_one_table_entry() {
        let cells: Vec<Cell> = (0..200)
            .map(|_| styled('x', Color::Palette(3), CellAttrs::ITALIC))
            .collect();
        let row = Row::from_cells(cells, false, false);
        let bytes = encode_tail(&[row], 200, 0, &[], 1 << 20).expect("encodes");
        let decoded = decode(&bytes).expect("decodes");
        assert!(
            decoded.rows[0]
                .cells
                .iter()
                .all(|c| c.attrs == CellAttrs::ITALIC && c.fg == Color::Palette(3))
        );
        // 200 identically-styled cells must not cost 200 style entries; the
        // whole file should stay far under the raw cell footprint.
        assert!(bytes.len() < 200 * CELL_ENCODED_BYTES, "{}", bytes.len());
    }

    #[test]
    fn rewrap_rejoins_soft_wrapped_rows_at_the_new_width() {
        // "abcdef" captured at width 3 → two rows, the first soft-wrapped.
        let mut first = row_of("abc", 3);
        first.wrapped = true;
        let second = row_of("def", 3);

        let widened = rewrap(vec![first, second], 6);
        assert_eq!(widened.len(), 1);
        let text: String = widened[0].cells.iter().map(|c| c.ch).collect();
        assert_eq!(text, "abcdef");
        assert!(!widened[0].wrapped);
    }

    #[test]
    fn rewrap_splits_a_long_line_and_marks_the_continuations() {
        let narrowed = rewrap(vec![row_of("abcdef", 6)], 4);
        assert_eq!(narrowed.len(), 2);
        assert!(narrowed[0].wrapped, "the first row continues");
        assert!(!narrowed[1].wrapped, "the last row ends the line");
        let first: String = narrowed[0].cells.iter().map(|c| c.ch).collect();
        let second: String = narrowed[1].cells.iter().map(|c| c.ch).collect();
        assert_eq!(first, "abcd");
        assert_eq!(second, "ef  ", "the tail is padded with blanks");
    }

    #[test]
    fn rewrap_never_separates_a_wide_glyph_from_its_spacer() {
        let wide = styled('あ', Color::Default, CellAttrs::WIDE);
        let spacer = styled(' ', Color::Default, CellAttrs::WIDE_SPACER);
        let row = Row::from_cells(
            vec![
                Cell {
                    ch: 'x',
                    ..Cell::default()
                },
                wide,
                spacer,
            ],
            false,
            false,
        );

        // Width 2 would otherwise split between the lead and its spacer.
        let wrapped = rewrap(vec![row], 2);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].cells[0].ch, 'x');
        assert_eq!(
            wrapped[0].cells[1],
            Cell::default(),
            "the lead moved down, leaving the column blank"
        );
        assert_eq!(wrapped[1].cells[0].ch, 'あ');
        assert!(wrapped[1].cells[1].attrs.contains(CellAttrs::WIDE_SPACER));
    }

    #[test]
    fn rewrap_terminates_when_a_wide_glyph_cannot_fit_the_width() {
        // Backing off to keep a wide lead with its spacer must never leave the
        // split at the row start: that consumes nothing and loops forever.
        let wide = styled('あ', Color::Default, CellAttrs::WIDE);
        let spacer = styled(' ', Color::Default, CellAttrs::WIDE_SPACER);
        let row = Row::from_cells(vec![wide, spacer], false, false);

        let out = rewrap(vec![row], 1);

        assert_eq!(out.len(), 2, "one cell per row, no more");
        assert_eq!(out[0].cells[0].ch, 'あ');
        assert!(out[1].cells[0].attrs.contains(CellAttrs::WIDE_SPACER));
    }

    #[test]
    fn the_budget_counts_the_side_tables_not_just_the_rows() {
        // A single huge URI lives in the link table, which the per-row size
        // estimate cannot see. The encoded file must still respect the budget.
        let uri = "https://example.com/".to_string() + &"a".repeat(200_000);
        let registry = vec![Hyperlink {
            uri: uri.clone(),
            id: None,
        }];
        let mut linked = Cell {
            ch: 'x',
            ..Cell::default()
        };
        linked.hyperlink = HyperlinkId::new(0);
        let rows = vec![
            row_of("plain older line", 40),
            Row::from_cells(vec![linked], false, false),
        ];

        let bytes = encode_tail(&rows, 40, 0, &registry, 4096).expect("something encodes");
        assert!(
            bytes.len() <= 4096,
            "encoded {} bytes against a 4096 budget",
            bytes.len()
        );
        let decoded = decode(&bytes).expect("decodes");
        assert!(
            decoded.hyperlinks.iter().all(|link| link.uri != uri),
            "an oversized link must not reach the file at all"
        );
        let text: String = decoded
            .rows
            .last()
            .expect("the linked row survives")
            .cells
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert!(
            text.starts_with('x'),
            "dropping the link must not drop the text: {text:?}"
        );
    }

    #[test]
    fn rewrap_preserves_blank_lines_between_paragraphs() {
        let rows = vec![row_of("a", 4), row_of("", 4), row_of("b", 4)];
        let out = rewrap(rows, 4);
        assert_eq!(out.len(), 3);
        assert!(
            out[1].cells.iter().all(|c| *c == Cell::default()),
            "the blank line survives as a blank row"
        );
    }

    #[test]
    fn a_link_id_the_registry_no_longer_knows_degrades_to_no_link() {
        let mut cell = Cell {
            ch: 'z',
            ..Cell::default()
        };
        cell.hyperlink = HyperlinkId::new(99);
        let row = Row::from_cells(vec![cell], false, false);
        let bytes = encode_tail(&[row], 1, 0, &[], 4096).expect("encodes");
        let decoded = decode(&bytes).expect("decodes");
        assert_eq!(decoded.rows[0].cells[0].hyperlink, None);
        assert!(decoded.hyperlinks.is_empty());
    }
}

