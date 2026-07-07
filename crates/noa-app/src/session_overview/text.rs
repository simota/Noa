use unicode_width::UnicodeWidthChar;

/// Title label associated with a live or placeholder overview tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewTileLabel<Id> {
    pub id: Id,
    pub label: String,
}

/// Placeholder shown in the "Search sessions" field while the query is empty
/// (REQ-OV-16). Compile-time constant (⚠G precedent: no config knob).
pub const OVERVIEW_SEARCH_PLACEHOLDER: &str = "Search sessions";

/// The text to render in the top search field (REQ-OV-16): the live query, or
/// the [`OVERVIEW_SEARCH_PLACEHOLDER`] when it is empty. Kept pure so the
/// empty-vs-typed switch is unit-testable without a GPU.
pub fn overview_search_field_text(query: &str) -> String {
    if query.is_empty() {
        OVERVIEW_SEARCH_PLACEHOLDER.to_string()
    } else {
        query.to_string()
    }
}

/// Compose the single terminal row rendered into the rounded search field.
/// The leading search glyph is a visual affordance only; if the font cannot
/// render it, the row still degrades to readable placeholder/query text.
pub fn overview_search_field_row(query: &str, cols: u16) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    let text = overview_search_field_text(query);
    let row = format!("  ⌕  {text}");
    // Cell-width clip (not char count): a wide char in the typed query must
    // not push the row past `cols`, or the single-row `Terminal` wraps.
    clip_to_cols(&row, cols)
}

/// The close glyph pinned to a title bar's final column (REQ-OV-13).
/// `'✕'` (U+2715) for mockup parity; falls back to font-fallback rendering —
/// if it tofus on some setup, swap back to ASCII `'x'` at this one site
/// (manual-verify caveat, same as [`overview_hint_bar_text`]).
pub const TITLE_BAR_CLOSE_GLYPH: char = '✕';

/// Compose one title-bar row: the centered tab `label` with the close glyph
/// pinned to the final column (REQ-OV-13). The label is centered within the
/// columns left of the glyph and clipped if it would overrun them, so the
/// close glyph is always visible.
pub fn title_bar_row_with_close(label: &str, cols: u16) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    if cols < 2 {
        // Too narrow for both a label and the glyph — show the glyph alone.
        return TITLE_BAR_CLOSE_GLYPH.to_string();
    }
    // Reserve the last column for the close glyph; center the label in the rest.
    let label_field = cols - 1;
    let centered = center_label(label, label_field as u16);
    let mut row: Vec<char> = centered.chars().take(label_field).collect();
    while row.len() < label_field {
        row.push(' ');
    }
    row.push(TITLE_BAR_CLOSE_GLYPH);
    row.into_iter().collect()
}

/// A truecolor SGR foreground prefix for the ANSI title-bar composer.
fn ansi_fg(color: noa_core::Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", color.r, color.g, color.b)
}

/// Compose one title-bar row with inline SGR styling, visually identical in
/// layout to [`title_bar_row_with_close`] but adding: an optional dim `⌘n`
/// switch badge before the label (REQ-OV-15c affordance), a colored status dot
/// when the label carries the `● ` needs-user prefix (red attention / yellow
/// bell / blue busy — the caller picks the color from card state), and an
/// accent-bold highlight of the first case-insensitive `query` match inside
/// the label (REQ-OV-16). The escapes occupy no cells, so the visible layout
/// (centering, clipping, trailing close glyph) matches the plain composer.
pub fn title_bar_row_ansi(
    label: &str,
    cols: u16,
    badge: Option<usize>,
    dot_color: Option<noa_core::Rgb>,
    query: &str,
) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    if cols < 2 {
        return TITLE_BAR_CLOSE_GLYPH.to_string();
    }
    let field = cols - 1;

    let badge_text = badge.map(|n| format!("{n} ")).unwrap_or_default();
    let badge_len = badge_text.chars().count();
    // Clip the label to the space left of the badge so the glyph never moves.
    let label: String = label
        .chars()
        .take(field.saturating_sub(badge_len))
        .collect();
    let vis_len = badge_len + label.chars().count();
    let pad = (field.saturating_sub(vis_len)) / 2;

    const RESET_FG: &str = "\x1b[39m";
    let dim = ansi_fg(crate::chrome::palette().dim_fg);
    let accent = ansi_fg(crate::chrome::palette().accent);

    let mut out = String::new();
    out.extend(std::iter::repeat_n(' ', pad));
    if !badge_text.is_empty() {
        out.push_str(&dim);
        out.push_str(&badge_text);
        out.push_str(RESET_FG);
    }

    // Split the label into an optional colored dot prefix and the rest.
    let (dot_seg, rest) = match dot_color {
        Some(_) if label.starts_with("● ") => label.split_at("● ".len()),
        _ => ("", label.as_str()),
    };
    if !dot_seg.is_empty() {
        // `dot_color` is Some by construction of `dot_seg`.
        out.push_str(&ansi_fg(
            dot_color.unwrap_or(crate::chrome::palette().dot_red),
        ));
        out.push_str(dot_seg);
        out.push_str(RESET_FG);
    }

    // First case-insensitive match of `query` within the rest of the label.
    // `to_ascii_lowercase` preserves byte offsets, so the byte range found in
    // the lowered copy slices the original safely.
    let match_range = if query.is_empty() {
        None
    } else {
        rest.to_ascii_lowercase()
            .find(&query.to_ascii_lowercase())
            .map(|start| (start, start + query.len()))
    };
    match match_range {
        Some((start, end))
            if end <= rest.len() && rest.is_char_boundary(start) && rest.is_char_boundary(end) =>
        {
            out.push_str(&rest[..start]);
            out.push_str("\x1b[1m");
            out.push_str(&accent);
            out.push_str(&rest[start..end]);
            out.push_str(RESET_FG);
            out.push_str("\x1b[22m");
            out.push_str(&rest[end..]);
        }
        _ => out.push_str(rest),
    }

    // Right-pad the label field, then pin the dim close glyph to the last col.
    let visible = pad + vis_len;
    out.extend(std::iter::repeat_n(' ', field.saturating_sub(visible)));
    out.push_str(&dim);
    out.push(TITLE_BAR_CLOSE_GLYPH);
    out.push_str(RESET_FG);
    out
}

/// Map source tabs to display labels using already-known tab titles.
pub fn overview_tile_labels<Id: Copy>(
    source_ids: &[Id],
    mut title_for_id: impl FnMut(Id) -> Option<String>,
) -> Vec<OverviewTileLabel<Id>> {
    source_ids
        .iter()
        .copied()
        .map(|id| OverviewTileLabel {
            id,
            label: title_for_id(id).unwrap_or_else(|| "Noa".to_string()),
        })
        .collect()
}

/// Overflow window ids relegated to title-only placeholder rows (REQ-OV-10):
/// the tail of `source_ids` beyond the live tile cap. Index-parallel with
/// `OverviewLayout::placeholders` (both walk the same overflow ids in order).
pub fn overview_placeholder_source_ids<Id: Copy>(
    source_ids: &[Id],
    live_tile_count: usize,
) -> &[Id] {
    source_ids.get(live_tile_count..).unwrap_or(&[])
}

/// Sanitize a tab title for display in a single-row placeholder tile: tab
/// titles arrive via OSC 0/2 with no control-character filtering, and a
/// placeholder tile has no live mirror to clip an overlong string visually,
/// so this strips control characters and clamps to `max_cols` characters.
pub fn sanitize_placeholder_label(label: &str, max_cols: u16) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(max_cols as usize)
        .collect()
}

/// Build the bottom hint-bar text (REQ-OV-17). `live_tile_count` is the number
/// of live thumbnail tiles (`min(tab_count, cap)`); the `⌘1-N` range tracks it
/// dynamically rather than hard-coding the mockup's "1-6".
///
/// NOTE (manual-verify): the `⌘`, arrow, and `・` glyphs depend on font
/// fallback. If they render as tofu, swap to the ASCII form returned by
/// [`overview_hint_bar_text_ascii`] (a compile-time swap at the one call site)
/// and record the deviation.
pub fn overview_hint_bar_text(live_tile_count: usize) -> String {
    let n = live_tile_count.max(1);
    format!("⌘1-{n} to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close")
}

/// ASCII fallback for [`overview_hint_bar_text`] when the Unicode glyphs tofu.
pub fn overview_hint_bar_text_ascii(live_tile_count: usize) -> String {
    let n = live_tile_count.max(1);
    format!(
        "cmd+1-{n} to switch / arrows to navigate / return to open / tab to zoom / esc to close"
    )
}

/// Compact fallback for [`overview_hint_bar_text`] when the hint bar is too
/// narrow for the full sentence: same key hints, connective words dropped.
pub fn overview_hint_bar_text_compact(live_tile_count: usize) -> String {
    let n = live_tile_count.max(1);
    format!("⌘1-{n} switch・↑↓←→ navigate・Return open・Tab zoom・esc close")
}

/// Grid-cell width of `text` (the same `unicode-width` semantics `noa-grid`
/// lays cells out with — `・` is 2 cells, the arrows and `⌘` are 1).
pub(super) fn text_cell_width(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Clip `text` to at most `cols` grid cells (never splitting a wide char).
fn clip_to_cols(text: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > cols {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

/// Compose the single terminal row rendered into the hint bar: the full hint
/// if it fits `cols`, otherwise the compact variant, centered and
/// hard-clipped. The clip matters — the row is fed to a synthetic single-row
/// `Terminal`, where overflow wraps-and-scrolls and would leave only the tail
/// of the sentence visible.
pub fn overview_hint_bar_row(live_tile_count: usize, cols: u16) -> String {
    let cols = cols as usize;
    let full = overview_hint_bar_text(live_tile_count);
    let text = if text_cell_width(&full) <= cols {
        full
    } else {
        overview_hint_bar_text_compact(live_tile_count)
    };
    clip_to_cols(&center_label(&text, cols as u16), cols)
}

/// Horizontally center `text` within `cols` columns by left-padding with
/// spaces (used for title-bar and hint-bar labels rendered through a synthetic
/// single-row `Terminal`). Longer-than-`cols` text is returned unpadded; the
/// renderer clips it to the tile.
pub fn center_label(text: &str, cols: u16) -> String {
    let width = text.chars().count();
    let cols = cols as usize;
    if width >= cols {
        return text.to_string();
    }
    let pad = (cols - width) / 2;
    let mut out = String::with_capacity(cols);
    out.extend(std::iter::repeat_n(' ', pad));
    out.push_str(text);
    out
}
