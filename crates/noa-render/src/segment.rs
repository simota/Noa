//! Render-side text-run segmentation (WP2, REQ-SHAPE-6).
//!
//! Builds `Vec<ShapeCell>` runs from a row's worth of per-cell render
//! context, breaking at font-face / style / selection / search / cursor
//! boundaries (row boundaries are implicit: [`segment_row`] only ever sees
//! one row at a time). Segmentation lives here — in `noa-render` — rather
//! than in `noa-grid`, per CLAUDE.md's GUI-agnostic dependency rule: the
//! only per-cell context this needs (`ch`/`combining`/bold/italic) is
//! already exposed by `noa-grid`'s `Cell`/`CellAttrs` via `FrameSnapshot`;
//! no new grid-layer accessor was needed (WP2 failure condition 4,
//! resolved in the "extend the snapshot, or confirm it's already enough"
//! direction — it was already enough).

use noa_font::{FaceId, FontGrid, ShapeCell, StyleKey};

/// One source cell's input to segmentation: shapeable content/style plus
/// the frame-local highlight/cursor/color context the caller (the per-cell
/// background/decoration pass in `renderer.rs`) already computed.
///
/// Only `ch`/`combining`/`bold`/`italic` feed into the `ShapeCell`s handed
/// to `FontGrid::shape_run` — `selected`/`active_search`/`search_match`/
/// `cursor` are consumed ONLY as run-boundary keys by [`segment_row`] and
/// then carried in [`ShapeRun::cell_render`], never inside a `ShapeCell`
/// (FM-08: the shape cache key is built from `&[ShapeCell]` alone, so
/// per-frame highlight/cursor state has no field to leak into via this
/// path either).
#[derive(Clone)]
pub struct SegmentCell {
    pub ch: char,
    pub combining: Vec<char>,
    pub bold: bool,
    pub italic: bool,
    pub selected: bool,
    pub active_search: bool,
    pub search_match: bool,
    pub cursor: bool,
    pub color: [u8; 4],
}

/// Per-source-cell render context kept alongside a [`ShapeRun`]'s cells,
/// consumed only AFTER shaping (color/cursor for instance emission) — never
/// fed into `FontGrid::shape_run`'s cache key.
#[derive(Clone, Copy, Debug)]
pub struct CellRenderInfo {
    pub color: [u8; 4],
    pub cursor: bool,
}

/// One row-local shapeable run: a maximal span of cells sharing the same
/// resolved face + style + selection/search/cursor highlight state
/// (REQ-SHAPE-6). `start_col` plus a `ShapedGlyph::cluster` gives the
/// anchor cell's column (`start_col + cluster`).
pub struct ShapeRun {
    pub start_col: u16,
    pub cells: Vec<ShapeCell>,
    /// Parallel to `cells` (same length, same index order).
    pub cell_render: Vec<CellRenderInfo>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BoundaryKey {
    face: FaceId,
    style: StyleKey,
    selected: bool,
    active_search: bool,
    search_match: bool,
    cursor: bool,
}

fn boundary_key(font: &mut FontGrid, cell: &SegmentCell) -> (BoundaryKey, StyleKey) {
    let style = StyleKey {
        bold: cell.bold,
        italic: cell.italic,
    };
    let key = BoundaryKey {
        face: font.resolve_face_for_style(cell.ch, style),
        style,
        selected: cell.selected,
        active_search: cell.active_search,
        search_match: cell.search_match,
        cursor: cell.cursor,
    };
    (key, style)
}

/// Segment one row's cells into shapeable runs (REQ-SHAPE-6): breaks at
/// font-face, style (bold/italic), selection, active-search-match,
/// search-match, and cursor boundaries. A row never crosses into another
/// row (the caller passes one row's cells at a time), which keeps this
/// ready for WP4's per-row dirty patching.
///
/// Takes `&mut FontGrid` because resolving a codepoint the curated font stack
/// cannot map may lazily pull a system fallback face into the stack (macOS
/// CoreText cascade — see [`noa_font::FontGrid::resolve_face_for_style`]).
pub fn segment_row(font: &mut FontGrid, cells: &[SegmentCell]) -> Vec<ShapeRun> {
    let mut runs: Vec<ShapeRun> = Vec::new();
    let mut current_key: Option<BoundaryKey> = None;

    for (idx, cell) in cells.iter().enumerate() {
        let (key, style) = boundary_key(font, cell);
        let shape_cell = ShapeCell {
            ch: cell.ch,
            combining: cell.combining.clone(),
            style,
        };
        let render_info = CellRenderInfo {
            color: cell.color,
            cursor: cell.cursor,
        };

        if current_key == Some(key) {
            let run = runs.last_mut().expect("current_key implies an open run");
            run.cells.push(shape_cell);
            run.cell_render.push(render_info);
        } else {
            runs.push(ShapeRun {
                start_col: idx as u16,
                cells: vec![shape_cell],
                cell_render: vec![render_info],
            });
            current_key = Some(key);
        }
    }

    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_font::FontConfig;

    fn plain_cell(ch: char) -> SegmentCell {
        SegmentCell {
            ch,
            combining: Vec::new(),
            bold: false,
            italic: false,
            selected: false,
            active_search: false,
            search_match: false,
            cursor: false,
            color: [255, 255, 255, 255],
        }
    }

    fn skip_font() -> Option<FontGrid> {
        match FontGrid::new(24.0, FontConfig::default()) {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                None
            }
        }
    }

    /// AC-WP2-06: a row with a same-style/highlight run of plain ASCII
    /// stays one run (no spurious boundary).
    #[test]
    fn same_face_and_style_run_stays_one_run() {
        let Some(mut font) = skip_font() else { return };
        let cells = vec![plain_cell('a'), plain_cell('b'), plain_cell('c')];
        let runs = segment_row(&mut font, &cells);
        assert_eq!(runs.len(), 1, "uniform ASCII text must stay a single run");
        assert_eq!(runs[0].start_col, 0);
        assert_eq!(runs[0].cells.len(), 3);
    }

    /// AC-WP2-06: a style change (bold) breaks the run.
    #[test]
    fn style_change_breaks_the_run() {
        let Some(mut font) = skip_font() else { return };
        let mut cells = vec![plain_cell('a'), plain_cell('b')];
        cells[1].bold = true;
        let runs = segment_row(&mut font, &cells);
        assert_eq!(
            runs.len(),
            2,
            "a bold-attribute change must start a new run"
        );
        assert_eq!(runs[0].start_col, 0);
        assert_eq!(runs[1].start_col, 1);
    }

    /// AC-WP2-06: selection/cursor highlight boundaries also break the run,
    /// even with identical style/face — the spec's safe default so a
    /// shaped ligature never straddles a highlight edge.
    #[test]
    fn selection_and_cursor_boundaries_break_the_run() {
        let Some(mut font) = skip_font() else { return };
        let mut cells = vec![plain_cell('a'), plain_cell('b'), plain_cell('c')];
        cells[1].selected = true;
        let runs = segment_row(&mut font, &cells);
        assert_eq!(
            runs.len(),
            3,
            "a selected cell surrounded by unselected cells must be its own run"
        );

        let mut cells = vec![plain_cell('a'), plain_cell('b')];
        cells[1].cursor = true;
        let runs = segment_row(&mut font, &cells);
        assert_eq!(runs.len(), 2, "the cursor cell must start a new run");
    }

    /// AC-WP2-06: a Latin+CJK mixed row segments into >=2 runs at the face
    /// boundary, each shaped with its own resolved face.
    #[test]
    fn latin_and_cjk_mixed_row_segments_at_face_boundary() {
        let Some(mut font) = skip_font() else { return };
        if font.resolve_face('A') == font.resolve_face('日') {
            eprintln!(
                "skipping: installed font stack resolves Latin and CJK to the same face \
                 (no distinct CJK fallback in this environment)"
            );
            return;
        }

        let cells = vec![
            plain_cell('a'),
            plain_cell('b'),
            plain_cell('日'),
            plain_cell('本'),
        ];
        let runs = segment_row(&mut font, &cells);
        assert!(
            runs.len() >= 2,
            "a Latin+CJK mixed row must segment into >=2 runs at the face boundary, got {}",
            runs.len()
        );

        let latin_run = &runs[0];
        let cjk_run = runs.last().unwrap();
        let latin_face = font.resolve_face(latin_run.cells[0].ch);
        let cjk_face = font.resolve_face(cjk_run.cells[0].ch);
        assert_ne!(
            latin_face, cjk_face,
            "each run must resolve to its own distinct face"
        );
    }

    /// `cell_render` stays parallel to `cells` and carries color/cursor
    /// context without polluting the `ShapeCell`s themselves.
    #[test]
    fn cell_render_context_is_parallel_and_excluded_from_shape_cells() {
        let Some(mut font) = skip_font() else { return };
        let mut cells = vec![plain_cell('x'), plain_cell('y')];
        cells[1].color = [1, 2, 3, 255];
        cells[1].cursor = true;
        // Same style/face/highlight-except-cursor -> cursor still breaks it.
        let runs = segment_row(&mut font, &cells);
        let last = runs.last().unwrap();
        assert_eq!(last.cells.len(), last.cell_render.len());
        assert_eq!(last.cell_render[0].color, [1, 2, 3, 255]);
        assert!(last.cell_render[0].cursor);
    }
}
