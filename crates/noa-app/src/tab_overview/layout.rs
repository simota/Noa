use super::metrics::OverviewMetrics;
use super::{Point, TileRect};

/// Pure layout result for the Session Overview grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewLayout {
    pub cols: usize,
    pub rows: usize,
    pub placeholder_rows: usize,
    pub tiles: Vec<TileRect>,
    pub placeholders: Vec<TileRect>,
    pub overflow: bool,
}

/// The close (✕) button hit-rect for `tile`: a square at the title bar's
/// top-right corner (REQ-OV-13).
pub fn overview_close_button_rect(tile: TileRect, metrics: OverviewMetrics) -> TileRect {
    let w = metrics.close_button_w.min(tile.w);
    let h = metrics.title_bar_h.min(tile.h);
    TileRect::new(tile.right().saturating_sub(w), tile.y, w, h)
}

/// Return the target id whose close button contains `point`, or `None` for a
/// point in the rest of the tile (or outside every tile). Deliberately a
/// separate hit-test surface from [`hit_test_overview_grid`] (REQ-OV-13):
/// callers check this one first and only fall back to the tile-body
/// hit-test on a miss, so a close-button click is never mistaken for a
/// tile-focus click even though both rects overlap at that corner.
pub fn overview_close_hit_test<T: Copy>(
    tiles: &[(T, TileRect)],
    point: Point,
    metrics: OverviewMetrics,
) -> Option<T> {
    tiles
        .iter()
        .find(|(_, rect)| overview_close_button_rect(*rect, metrics).contains(point))
        .map(|(id, _)| *id)
}

/// The enlarged quick-look rect for a zoomed tile (Tab toggle): the tile's
/// size scaled up, clamped to `grid_bounds`, centered within it.
pub fn overview_zoom_rect(grid_bounds: TileRect, tile: TileRect) -> TileRect {
    const ZOOM_SCALE: f32 = 1.6;
    if grid_bounds.w == 0 || grid_bounds.h == 0 {
        return grid_bounds;
    }
    let w = ((tile.w as f32 * ZOOM_SCALE).round() as u32)
        .min(grid_bounds.w)
        .max(1);
    let h = ((tile.h as f32 * ZOOM_SCALE).round() as u32)
        .min(grid_bounds.h)
        .max(1);
    TileRect::new(
        grid_bounds.x + (grid_bounds.w - w) / 2,
        grid_bounds.y + (grid_bounds.h - h) / 2,
        w,
        h,
    )
}

/// Compute equal-size row-major tile rectangles for the Session Overview.
///
/// `cap` is part of the pure seam so tests can exercise the degradation
/// boundary directly; production uses [`OVERVIEW_GRID_CAP`]. `gutter` is the
/// fixed gap between adjacent tiles and `margin` the gap between the grid and
/// `bounds`' edges (REQ-OV-11, mockup parity v2); production uses
/// [`OVERVIEW_TILE_GUTTER`]/[`OVERVIEW_OUTER_MARGIN`]. `gutter=0, margin=0`
/// reproduces v1's edge-to-edge tiling bit-for-bit (AC-OV-11).
pub fn compute_overview_grid(
    tab_count: usize,
    bounds: TileRect,
    cap: usize,
    gutter: u32,
    margin: u32,
) -> OverviewLayout {
    let live_cap = cap.min(tab_count);
    let overflow_count = tab_count.saturating_sub(live_cap);
    let overflow = overflow_count > 0;

    if live_cap == 0 {
        return OverviewLayout {
            cols: 0,
            rows: 0,
            placeholder_rows: 0,
            tiles: Vec::new(),
            placeholders: Vec::new(),
            overflow,
        };
    }

    let cols = ceil_sqrt(live_cap);
    let rows = live_cap.div_ceil(cols);
    let placeholder_rows = if overflow {
        overflow_count.div_ceil(cols)
    } else {
        0
    };
    let total_rows = rows + placeholder_rows;

    // Inner content area after subtracting the outer margin on both sides;
    // with margin=0 this is `bounds` itself.
    let inner_w = bounds.w.saturating_sub(2 * margin);
    let inner_h = bounds.h.saturating_sub(2 * margin);
    let col_gutters = gutter.saturating_mul(cols as u32 - 1);
    let row_gutters = gutter.saturating_mul(total_rows as u32 - 1);
    let tile_w = inner_w.saturating_sub(col_gutters) / cols as u32;
    let tile_h = inner_h.saturating_sub(row_gutters) / total_rows as u32;
    let origin_x = bounds.x + margin;
    let origin_y = bounds.y + margin;

    let tiles = (0..live_cap)
        .map(|index| rect_at(origin_x, origin_y, tile_w, tile_h, cols, index, gutter))
        .collect();
    let placeholders = (0..overflow_count)
        .map(|index| {
            rect_at(
                origin_x,
                origin_y,
                tile_w,
                tile_h,
                cols,
                live_cap + index,
                gutter,
            )
        })
        .collect();

    OverviewLayout {
        cols,
        rows,
        placeholder_rows,
        tiles,
        placeholders,
        overflow,
    }
}

/// Return the target id for `point`, or `None` outside live tiles.
///
/// Callers pass only live thumbnail tile pairs. Placeholder rows and empty grid
/// cells are therefore naturally non-interactive.
pub fn hit_test_overview_grid<T: Copy>(tiles: &[(T, TileRect)], point: Point) -> Option<T> {
    tiles
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(id, _)| *id)
}

/// The three horizontal bands the Overview window is split into (REQ-OV-11/16/17):
/// a reserved top search band, the middle tile-grid area, and a bottom hint
/// band. `grid_bounds` is what feeds [`compute_overview_grid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewChrome {
    pub search_band: TileRect,
    pub grid_bounds: TileRect,
    pub hint_band: TileRect,
}

/// Carve `bounds` into the search / grid / hint bands (REQ-OV-11, v2 mockup
/// parity). Reserving both bands here — rather than only around the grid —
/// keeps the grid origin stable when P3 starts drawing the search field, and
/// routes hit-testing + selection nav through the same `grid_bounds` the tiles
/// are laid out in. Both band heights clamp so a very short window degrades to
/// an empty grid instead of underflowing.
pub fn overview_chrome_bands(bounds: TileRect, metrics: OverviewMetrics) -> OverviewChrome {
    let search_h = metrics.search_band_h.min(bounds.h);
    let after_search = bounds.h - search_h;
    let hint_h = metrics.hint_band_h.min(after_search);
    let grid_h = after_search - hint_h;

    OverviewChrome {
        search_band: TileRect::new(bounds.x, bounds.y, bounds.w, search_h),
        grid_bounds: TileRect::new(bounds.x, bounds.y + search_h, bounds.w, grid_h),
        hint_band: TileRect::new(bounds.x, bounds.y + search_h + grid_h, bounds.w, hint_h),
    }
}

/// Centered rounded search-field rect inside the top chrome band.
pub fn overview_search_field_rect(search_band: TileRect, metrics: OverviewMetrics) -> TileRect {
    centered_pill_rect(
        search_band,
        0.36,
        metrics.search_field_min_w,
        metrics.search_field_max_w,
        metrics.search_field_h,
    )
}

/// Centered rounded hint-bar rect inside the bottom chrome band.
pub fn overview_hint_bar_rect(hint_band: TileRect, metrics: OverviewMetrics) -> TileRect {
    centered_pill_rect(
        hint_band,
        0.48,
        metrics.hint_bar_min_w,
        metrics.hint_bar_max_w,
        metrics.hint_bar_h,
    )
}

fn ceil_sqrt(n: usize) -> usize {
    let mut cols = 1;
    while cols * cols < n {
        cols += 1;
    }
    cols
}

fn rect_at(
    origin_x: u32,
    origin_y: u32,
    tile_w: u32,
    tile_h: u32,
    cols: usize,
    index: usize,
    gutter: u32,
) -> TileRect {
    let col = index % cols;
    let row = index / cols;
    TileRect::new(
        origin_x + (tile_w + gutter).saturating_mul(col as u32),
        origin_y + (tile_h + gutter).saturating_mul(row as u32),
        tile_w,
        tile_h,
    )
}

fn centered_pill_rect(
    band: TileRect,
    width_fraction: f32,
    min_width: u32,
    max_width: u32,
    preferred_height: u32,
) -> TileRect {
    if band.w == 0 || band.h == 0 {
        return TileRect::new(band.x, band.y, 0, 0);
    }
    let desired_w = (band.w as f32 * width_fraction).round() as u32;
    let w = desired_w
        .clamp(min_width.min(max_width), max_width)
        .min(band.w);
    let h = preferred_height.min(band.h);
    TileRect::new(band.x + (band.w - w) / 2, band.y + (band.h - h) / 2, w, h)
}
