//! Split out of the former monolithic `renderer.rs` — cursor visuals and cell decorations (underline/strikethrough/patterns).
//! Shares the parent module namespace via `use super::*`.

use super::*;

/// How the cursor renders at its current cell, resolved once per row from
/// pane-wide [`FrameSnapshot`] state (position, DECSCUSR style, focus, blink
/// phase) — never per-cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CursorVisual {
    /// DECTCEM off, viewport scrolled away from the live cursor, or a
    /// focused `Blinking*` style in its off phase.
    None,
    /// Solid block fill + inverted glyph (steady or blinking-and-visible).
    Block,
    /// Thin vertical bar at the cell's left edge; glyph keeps its own colors.
    Bar,
    /// Thin horizontal strip at the cell's bottom; glyph keeps its own colors.
    Underline,
    /// Unfocused pane: a hollow rectangle outline regardless of DECSCUSR
    /// style. Never blinks.
    Hollow,
}

pub(super) fn cursor_visual_for(snap: &FrameSnapshot) -> CursorVisual {
    if !snap.cursor.visible {
        return CursorVisual::None;
    }
    if !snap.focused {
        return CursorVisual::Hollow;
    }
    let is_blinking_style = matches!(
        snap.cursor.style,
        CursorStyle::BlinkingBlock
            | CursorStyle::BlinkingUnderline
            | CursorStyle::BlinkingBar
            | CursorStyle::BlinkingBlockHollow
    );
    if is_blinking_style && !snap.cursor_blink_visible {
        return CursorVisual::None;
    }
    match snap.cursor.style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => CursorVisual::Block,
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => CursorVisual::Underline,
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => CursorVisual::Bar,
        CursorStyle::BlinkingBlockHollow | CursorStyle::SteadyBlockHollow => CursorVisual::Hollow,
    }
}

/// The cursor's own color: an explicit OSC 12 override if set, else the
/// effective cell foreground (so an unstyled cursor tracks the text it sits on).
/// The returned color is contrast-adjusted against the effective cell
/// background so the cursor stays visible even on low-contrast themes.
pub(super) fn cursor_fill_rgb(
    theme: &Theme,
    snap: &FrameSnapshot,
    text_rgb: noa_core::Rgb,
    bg_rgb: noa_core::Rgb,
) -> noa_core::Rgb {
    let base = snap.colors.cursor().unwrap_or(text_rgb);
    theme.contrast_adjusted_fg(base, bg_rgb)
}

pub(super) fn surface_output_rgb(rgb: noa_core::Rgb, target_format_is_srgb: bool) -> [f32; 4] {
    surface_output_rgba(rgba(rgb), target_format_is_srgb)
}

/// Emit the decoration-pass rect(s) for a non-block cursor shape. `visual`
/// must not be [`CursorVisual::Block`] or [`CursorVisual::None`] — those are
/// handled by the background-quad path (block) or emit nothing (none).
pub(super) fn push_cursor_decorations(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    visual: CursorVisual,
    color: [u8; 4],
    metrics: Metrics,
    span: u16,
) {
    let cell = DecorationCell {
        grid_x: x,
        grid_y: y,
        color,
    };
    let thickness = decoration_thickness(metrics);
    let width = decoration_width(metrics, span);
    let height = metrics.cell_h.round().max(1.0) as u16;

    match visual {
        CursorVisual::Bar => {
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, thickness, height),
            );
        }
        CursorVisual::Underline => {
            let base_y = underline_y(metrics, thickness, 0.0);
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, base_y, width, thickness),
            );
        }
        CursorVisual::Hollow => {
            let right = width.saturating_sub(thickness) as i16;
            let bottom = height.saturating_sub(thickness) as i16;
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, width, thickness),
            ); // top
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, bottom, width, thickness),
            ); // bottom
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(0, 0, thickness, height),
            ); // left
            push_cursor_decoration_rect(
                instances,
                cell,
                DecorationRect::new(right, 0, thickness, height),
            ); // right
        }
        CursorVisual::Block | CursorVisual::None => {}
    }
}

/// Like [`push_decoration_rect`], but also tags the instance `FLAG_CURSOR`
/// so it's identifiable as a cursor-shape overlay rather than a regular
/// text decoration (underline/strike/etc). The shader only checks
/// `FLAG_DECORATION` for this quad's vertex path, so the extra bit is inert
/// there — it exists for renderer-side introspection (tests).
pub(super) fn push_cursor_decoration_rect(
    instances: &mut Vec<CellInstance>,
    cell: DecorationCell,
    rect: DecorationRect,
) {
    instances.push(CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [rect.width.max(1), rect.height.max(1)],
        bearing: [rect.x, rect.y],
        grid_pos: [cell.grid_x, cell.grid_y],
        color: cell.color,
        flags: CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR,
    });
}

pub(super) fn push_cell_decorations(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    attrs: CellAttrs,
    color: [u8; 4],
    metrics: Metrics,
    span: u16,
) {
    let thickness = decoration_thickness(metrics);
    let width = decoration_width(metrics, span);
    let cell = DecorationCell {
        grid_x: x,
        grid_y: y,
        color,
    };

    if attrs.contains(CellAttrs::OVERLINE) {
        push_decoration_rect(instances, cell, DecorationRect::new(0, 0, width, thickness));
    }
    if attrs.contains(CellAttrs::STRIKETHROUGH) {
        let strike_y = clamp_decoration_y(metrics.ascent * 0.55, thickness, metrics);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, strike_y, width, thickness),
        );
    }

    if attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
        let first_y = underline_y(metrics, thickness, -1.0);
        let second_y = underline_y(metrics, thickness, thickness as f32 + 1.0);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, first_y, width, thickness),
        );
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, second_y, width, thickness),
        );
    } else if attrs.contains(CellAttrs::CURLY_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, CurlPattern);
    } else if attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, DotPattern);
    } else if attrs.contains(CellAttrs::DASHED_UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_segmented_decoration(instances, cell, width, thickness, base_y, DashPattern);
    } else if attrs.contains(CellAttrs::UNDERLINE) {
        let base_y = underline_y(metrics, thickness, 0.0);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(0, base_y, width, thickness),
        );
    }
}

/// Whether `cell` at `(x, y)` falls under the snapshot's current Cmd+hover
/// target (see [`HoverLink`]).
pub(super) fn is_hover_link_cell(snap: &FrameSnapshot, cell: &Cell, x: u16, y: u16) -> bool {
    match snap.hover_link {
        Some(HoverLink::Registry(id)) => cell.hyperlink.is_some_and(|link| link.get() == id),
        Some(HoverLink::Range {
            y: row_y,
            x_start,
            x_end,
        }) => y == row_y && x >= x_start && x <= x_end,
        None => false,
    }
}

/// Cmd+hover underline for an OSC 8 hyperlink or auto-detected URL — an
/// extra decoration-pass rect independent of the cell's own UNDERLINE/
/// CURLY_UNDERLINE/etc attrs (both can coexist), using the same plain
/// underline geometry and the cell's own (possibly selection/search-
/// recolored) foreground.
pub(super) fn push_hover_link_underline(
    instances: &mut Vec<CellInstance>,
    x: u16,
    y: u16,
    color: [u8; 4],
    metrics: Metrics,
    span: u16,
) {
    let thickness = decoration_thickness(metrics);
    let width = decoration_width(metrics, span);
    let base_y = underline_y(metrics, thickness, 0.0);
    push_decoration_rect(
        instances,
        DecorationCell {
            grid_x: x,
            grid_y: y,
            color,
        },
        DecorationRect::new(0, base_y, width, thickness),
    );
}

/// Pixel width of a decoration rect spanning `span` grid columns (2 for a
/// wide lead, else 1), so underline/strike/cursor overlays cover a wide
/// glyph's full footprint instead of its left half.
pub(super) fn decoration_width(metrics: Metrics, span: u16) -> u16 {
    (metrics.cell_w * span.max(1) as f32).round().max(1.0) as u16
}

pub(super) fn decoration_thickness(metrics: Metrics) -> u16 {
    metrics
        .underline_thickness
        .round()
        .max(1.0)
        .min(metrics.cell_h.max(1.0)) as u16
}

pub(super) fn underline_y(metrics: Metrics, thickness: u16, offset: f32) -> i16 {
    let center = metrics.ascent - metrics.underline_position + offset;
    clamp_decoration_y(center - thickness as f32 / 2.0, thickness, metrics)
}

pub(super) fn clamp_decoration_y(y: f32, thickness: u16, metrics: Metrics) -> i16 {
    let max_y = (metrics.cell_h - thickness as f32).max(0.0);
    y.round().clamp(0.0, max_y) as i16
}

pub(super) trait SegmentPattern {
    fn segment(
        &self,
        index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]);
    fn advance(&self, thickness: u16) -> u16;
}

pub(super) struct DotPattern;
pub(super) struct DashPattern;
pub(super) struct CurlPattern;

#[derive(Clone, Copy)]
pub(super) struct DecorationCell {
    pub(super) grid_x: u16,
    pub(super) grid_y: u16,
    pub(super) color: [u8; 4],
}

#[derive(Clone, Copy)]
pub(super) struct DecorationRect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

impl DecorationRect {
    pub(super) const fn new(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

impl SegmentPattern for DotPattern {
    fn segment(
        &self,
        _index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        ([x as i16, base_y], [width.min(thickness), thickness])
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(2).max(2)
    }
}

impl SegmentPattern for DashPattern {
    fn segment(
        &self,
        _index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        let dash_width = width.min(thickness.saturating_mul(4).max(4));
        ([x as i16, base_y], [dash_width, thickness])
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(6).max(6)
    }
}

impl SegmentPattern for CurlPattern {
    fn segment(
        &self,
        index: u16,
        x: u16,
        width: u16,
        thickness: u16,
        base_y: i16,
    ) -> ([i16; 2], [u16; 2]) {
        let y_offset = if index.is_multiple_of(2) {
            0
        } else {
            thickness as i16
        };
        (
            [x as i16, base_y.saturating_sub(y_offset)],
            [width.min(thickness.saturating_mul(2).max(2)), thickness],
        )
    }

    fn advance(&self, thickness: u16) -> u16 {
        thickness.saturating_mul(2).max(2)
    }
}

pub(super) fn push_segmented_decoration<P: SegmentPattern>(
    instances: &mut Vec<CellInstance>,
    cell: DecorationCell,
    width: u16,
    thickness: u16,
    base_y: i16,
    pattern: P,
) {
    let advance = pattern.advance(thickness);
    let mut index = 0;
    let mut x = 0;
    while x < width {
        let remaining = width - x;
        let (bearing, size) = pattern.segment(index, x, remaining, thickness, base_y);
        push_decoration_rect(
            instances,
            cell,
            DecorationRect::new(bearing[0], bearing[1], size[0], size[1]),
        );
        index += 1;
        x = x.saturating_add(advance);
    }
}

pub(super) fn push_decoration_rect(
    instances: &mut Vec<CellInstance>,
    cell: DecorationCell,
    rect: DecorationRect,
) {
    instances.push(CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [rect.width.max(1), rect.height.max(1)],
        bearing: [rect.x, rect.y],
        grid_pos: [cell.grid_x, cell.grid_y],
        color: cell.color,
        flags: CellInstance::FLAG_DECORATION,
    });
}
