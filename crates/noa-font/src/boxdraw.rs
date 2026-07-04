//! Built-in procedural rendering of box-drawing, block-element and Powerline
//! glyphs (Ghostty analog: `font/sprite/`).
//!
//! Terminal UIs (vim, btop, tmux borders, powerline prompts) rely on these
//! codepoints tiling seamlessly across cells. Font glyphs almost never do:
//! their strokes are positioned by the designer's bearings, not the terminal's
//! cell grid, so lines break at cell seams. Like Ghostty, noa draws them itself
//! at exact cell dimensions instead.
//!
//! Every mask this module produces is exactly `ceil(cell_w) x ceil(cell_h)`
//! pixels and is placed flush to the cell origin (no font bearings — see
//! `FontGrid::raster_shaped`'s builtin branch), so a horizontal line in one
//! cell is collinear with its neighbour's: both are centered on the same
//! cell-midline computed from the same integer formula.
//!
//! The bulk of the box-drawing block is data-driven: each joint/line char is
//! described by four arm weights `[up, right, down, left]` (see [`arms`]) and
//! rendered by [`draw_arms`]. Dashes, rounded corners, diagonals, block
//! elements and Powerline triangles are special-cased.

use crate::face::Metrics;

/// A procedurally-drawn glyph: an R8 coverage mask sized to the cell box.
pub struct BuiltinGlyph {
    pub width: u32,
    pub height: u32,
    /// R8 alpha coverage, `width * height` bytes, row-major.
    pub coverage: Vec<u8>,
}

/// `true` for every codepoint noa draws itself instead of using a font glyph:
/// the U+2500–U+259F box-drawing + block-elements blocks and the U+E0B0–U+E0B3
/// Powerline separators. Checked before font lookup so these never resolve to a
/// (cell-misaligned) font glyph.
pub fn is_builtin_glyph(ch: char) -> bool {
    matches!(ch as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3)
}

/// Stroke weight of one arm of a box-drawing joint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

use Weight::{Double as D, Heavy as H, Light as L, None as N};

/// Per-cell drawing geometry derived once from [`Metrics`]: integer cell box
/// plus the stroke thicknesses and midlines every primitive shares (so
/// neighbouring cells' lines line up).
struct Geo {
    w: usize,
    h: usize,
    cx: usize,
    cy: usize,
    light: usize,
    heavy: usize,
    /// Half-separation of the two rails of a double line (centers `2*sep`
    /// apart, each `light` thick, leaving a `light`-wide gap between them).
    sep: usize,
}

impl Geo {
    fn new(metrics: &Metrics) -> Self {
        let w = (metrics.cell_w.ceil() as usize).max(1);
        let h = (metrics.cell_h.ceil() as usize).max(1);
        let light = ((h as f32 / 15.0).round() as usize).max(1);
        let heavy = (light * 2).max(light + 1);
        Self {
            w,
            h,
            cx: w / 2,
            cy: h / 2,
            light,
            heavy,
            sep: light,
        }
    }

    fn weight_px(&self, weight: Weight) -> usize {
        match weight {
            Weight::None => 0,
            Weight::Light | Weight::Double => self.light,
            Weight::Heavy => self.heavy,
        }
    }
}

/// A drawing surface: the coverage buffer plus its geometry.
struct Canvas {
    geo: Geo,
    buf: Vec<u8>,
}

impl Canvas {
    fn new(geo: Geo) -> Self {
        let buf = vec![0u8; geo.w * geo.h];
        Self { geo, buf }
    }

    fn set(&mut self, x: usize, y: usize, val: u8) {
        if x < self.geo.w && y < self.geo.h {
            let i = y * self.geo.w + x;
            // Max-blend so overlapping primitives (e.g. arc + arm) never
            // punch a lighter value over a darker one.
            if val > self.buf[i] {
                self.buf[i] = val;
            }
        }
    }

    /// Fill the half-open rect `[x0,x1) x [y0,y1)` (clamped) with `val`.
    fn fill(&mut self, x0: usize, x1: usize, y0: usize, y1: usize, val: u8) {
        let x1 = x1.min(self.geo.w);
        let y1 = y1.min(self.geo.h);
        for y in y0..y1 {
            for x in x0..x1 {
                self.set(x, y, val);
            }
        }
    }

    /// Horizontal stroke of thickness `t` centered on row `cy`, spanning
    /// columns `[x0,x1)`.
    fn hstroke(&mut self, x0: usize, x1: usize, cy: usize, t: usize) {
        let y0 = cy.saturating_sub(t / 2);
        self.fill(x0, x1, y0, y0 + t, 255);
    }

    /// Vertical stroke of thickness `t` centered on column `cx`, spanning
    /// rows `[y0,y1)`.
    fn vstroke(&mut self, y0: usize, y1: usize, cx: usize, t: usize) {
        let x0 = cx.saturating_sub(t / 2);
        self.fill(x0, x0 + t, y0, y1, 255);
    }

    fn into_glyph(self) -> BuiltinGlyph {
        BuiltinGlyph {
            width: self.geo.w as u32,
            height: self.geo.h as u32,
            coverage: self.buf,
        }
    }
}

/// Draw the builtin glyph for `ch` (caller guarantees [`is_builtin_glyph`]).
/// Always returns a mask of exactly `ceil(cell_w) x ceil(cell_h)` — a blank
/// mask of the right size for any codepoint with nothing to draw.
pub fn draw_builtin(ch: char, metrics: &Metrics) -> BuiltinGlyph {
    let mut canvas = Canvas::new(Geo::new(metrics));
    match ch as u32 {
        0x2504..=0x250B | 0x254C..=0x254F => draw_dash(&mut canvas, ch),
        0x256D..=0x2570 => draw_arc(&mut canvas, ch),
        0x2571..=0x2573 => draw_diagonal(&mut canvas, ch),
        0x2580..=0x259F => draw_block(&mut canvas, ch),
        0xE0B0..=0xE0B3 => draw_powerline(&mut canvas, ch),
        _ => {
            if let Some(arms) = arms(ch) {
                draw_arms(&mut canvas, arms);
            }
        }
    }
    canvas.into_glyph()
}

/// Draw a joint/line described by four arm weights `[up, right, down, left]`.
///
/// Each present arm is a stroke from its cell edge to the center; opposite
/// arms overlap at the center column/row, so a straight line is continuous and
/// a joint's arms meet cleanly. Double arms are two `light` rails offset by
/// `±sep` from the midline.
fn draw_arms(canvas: &mut Canvas, arms: [Weight; 4]) {
    let Geo {
        w, h, cx, cy, sep, ..
    } = canvas.geo;
    let [up, right, down, left] = arms;

    // Horizontal arms span to (and one past) the center so a horizontal line
    // is unbroken and always crosses the vertical stroke.
    for (weight, x0, x1) in [(left, 0, cx + 1), (right, cx, w)] {
        let t = canvas.geo.weight_px(weight);
        match weight {
            Weight::None => {}
            Weight::Double => {
                canvas.hstroke(x0, x1, cy.saturating_sub(sep), canvas.geo.light);
                canvas.hstroke(x0, x1, cy + sep, canvas.geo.light);
            }
            _ => canvas.hstroke(x0, x1, cy, t),
        }
    }
    for (weight, y0, y1) in [(up, 0, cy + 1), (down, cy, h)] {
        let t = canvas.geo.weight_px(weight);
        match weight {
            Weight::None => {}
            Weight::Double => {
                canvas.vstroke(y0, y1, cx.saturating_sub(sep), canvas.geo.light);
                canvas.vstroke(y0, y1, cx + sep, canvas.geo.light);
            }
            _ => canvas.vstroke(y0, y1, cx, t),
        }
    }
}

/// Dashed horizontal/vertical lines (2/3/4 dashes, light or heavy).
fn draw_dash(canvas: &mut Canvas, ch: char) {
    let (count, heavy, vertical) = match ch as u32 {
        0x2504 => (3, false, false),
        0x2505 => (3, true, false),
        0x2506 => (3, false, true),
        0x2507 => (3, true, true),
        0x2508 => (4, false, false),
        0x2509 => (4, true, false),
        0x250A => (4, false, true),
        0x250B => (4, true, true),
        0x254C => (2, false, false),
        0x254D => (2, true, false),
        0x254E => (2, false, true),
        0x254F => (2, true, true),
        _ => return,
    };
    let t = if heavy {
        canvas.geo.heavy
    } else {
        canvas.geo.light
    };
    let (cx, cy) = (canvas.geo.cx, canvas.geo.cy);
    // `count` dashes and `count-1` gaps: split the run into `2*count-1` equal
    // units and ink the even ones.
    let units = 2 * count - 1;
    if vertical {
        let h = canvas.geo.h;
        for y in 0..h {
            if (y * units / h).is_multiple_of(2) {
                canvas.vstroke(y, y + 1, cx, t);
            }
        }
    } else {
        let w = canvas.geo.w;
        for x in 0..w {
            if (x * units / w).is_multiple_of(2) {
                canvas.hstroke(x, x + 1, cy, t);
            }
        }
    }
}

/// Rounded corners (╭ ╮ ╯ ╰): a quarter-circle arc joining two light arms.
fn draw_arc(canvas: &mut Canvas, ch: char) {
    let Geo {
        cx,
        cy,
        light,
        w,
        h,
        ..
    } = canvas.geo;
    let r = cx.min(cy);
    if r == 0 {
        // Degenerate cell: fall back to a square corner so something draws.
        let arms = match ch as u32 {
            0x256D => [N, L, L, N],
            0x256E => [N, N, L, L],
            0x256F => [L, N, N, L],
            _ => [L, L, N, N],
        };
        draw_arms(canvas, arms);
        return;
    }

    // (down?, right?) and the arc-center offset direction for this corner.
    // `acx`/`acy` is the circle center; the arc is its quadrant nearest the
    // cell center.
    let (down, right, acx, acy) = match ch as u32 {
        0x256D => (true, true, cx + r, cy + r),   // ╭ down+right
        0x256E => (true, false, cx - r, cy + r),  // ╮ down+left
        0x256F => (false, false, cx - r, cy - r), // ╯ up+left
        _ => (false, true, cx + r, cy - r),       // ╰ up+right
    };

    // Straight arms from the arc endpoints to the cell edges.
    if right {
        canvas.hstroke(cx + r, w, cy, light);
    } else {
        canvas.hstroke(0, cx.saturating_sub(r) + 1, cy, light);
    }
    if down {
        canvas.vstroke(cy + r, h, cx, light);
    } else {
        canvas.vstroke(0, cy.saturating_sub(r) + 1, cx, light);
    }

    // Arc: pixels within a light-thick band of radius `r` from (acx,acy),
    // restricted to the quadrant facing the cell center.
    let half = light as f32 / 2.0 + 0.5;
    let rf = r as f32;
    for y in 0..h {
        for x in 0..w {
            let in_quadrant = (x <= acx) == right && (y <= acy) == down;
            if !in_quadrant {
                continue;
            }
            let dx = x as f32 - acx as f32;
            let dy = y as f32 - acy as f32;
            let d = (dx * dx + dy * dy).sqrt();
            if (d - rf).abs() <= half {
                canvas.set(x, y, 255);
            }
        }
    }
}

/// Diagonals ╱ ╲ ╳.
fn draw_diagonal(canvas: &mut Canvas, ch: char) {
    let (fwd, back) = match ch as u32 {
        0x2571 => (true, false), // ╱ bottom-left to top-right
        0x2572 => (false, true), // ╲ top-left to bottom-right
        _ => (true, true),       // ╳ both
    };
    let (w, h, t) = (canvas.geo.w, canvas.geo.h, canvas.geo.light);
    // Iterate both axes so steep/shallow slopes leave no gaps.
    let plot = |canvas: &mut Canvas, xc: usize, yc: usize| {
        let x0 = xc.saturating_sub(t / 2);
        let y0 = yc.saturating_sub(t / 2);
        canvas.fill(x0, x0 + t, y0, y0 + t, 255);
    };
    if back {
        // (0,0) -> (w,h)
        for x in 0..w {
            plot(canvas, x, x * h / w);
        }
        for y in 0..h {
            plot(canvas, y * w / h, y);
        }
    }
    if fwd {
        // (0,h) -> (w,0)
        for x in 0..w {
            plot(canvas, x, h.saturating_sub(1).saturating_sub(x * h / w));
        }
        for y in 0..h {
            plot(canvas, y * w / h, h.saturating_sub(1).saturating_sub(y));
        }
    }
}

/// Block elements U+2580–U+259F: half/eighth blocks, shades, quadrants.
fn draw_block(canvas: &mut Canvas, ch: char) {
    let Geo { w, h, cx, cy, .. } = canvas.geo;
    // Fraction k/8 helpers.
    let from_bottom = |k: usize| h - h * k / 8;
    let from_left = |k: usize| w * k / 8;
    match ch as u32 {
        0x2580 => canvas.fill(0, w, 0, cy, 255), // ▀ upper half
        0x2581 => canvas.fill(0, w, from_bottom(1), h, 255), // ▁ lower 1/8
        0x2582 => canvas.fill(0, w, from_bottom(2), h, 255), // ▂
        0x2583 => canvas.fill(0, w, from_bottom(3), h, 255), // ▃
        0x2584 => canvas.fill(0, w, from_bottom(4), h, 255), // ▄ lower half
        0x2585 => canvas.fill(0, w, from_bottom(5), h, 255), // ▅
        0x2586 => canvas.fill(0, w, from_bottom(6), h, 255), // ▆
        0x2587 => canvas.fill(0, w, from_bottom(7), h, 255), // ▇
        0x2588 => canvas.fill(0, w, 0, h, 255),  // █ full
        0x2589 => canvas.fill(0, from_left(7), 0, h, 255), // ▉ left 7/8
        0x258A => canvas.fill(0, from_left(6), 0, h, 255), // ▊
        0x258B => canvas.fill(0, from_left(5), 0, h, 255), // ▋
        0x258C => canvas.fill(0, cx, 0, h, 255), // ▌ left half
        0x258D => canvas.fill(0, from_left(3), 0, h, 255), // ▍
        0x258E => canvas.fill(0, from_left(2), 0, h, 255), // ▎
        0x258F => canvas.fill(0, from_left(1), 0, h, 255), // ▏ left 1/8
        0x2590 => canvas.fill(cx, w, 0, h, 255), // ▐ right half
        0x2591 => canvas.fill(0, w, 0, h, 64),   // ░ 25% shade
        0x2592 => canvas.fill(0, w, 0, h, 128),  // ▒ 50% shade
        0x2593 => canvas.fill(0, w, 0, h, 192),  // ▓ 75% shade
        0x2594 => canvas.fill(0, w, 0, h / 8, 255), // ▔ upper 1/8
        0x2595 => canvas.fill(from_left(7), w, 0, h, 255), // ▕ right 1/8
        0x2596 => quadrants(canvas, [false, false, true, false]), // ▖ LL
        0x2597 => quadrants(canvas, [false, false, false, true]), // ▗ LR
        0x2598 => quadrants(canvas, [true, false, false, false]), // ▘ UL
        0x2599 => quadrants(canvas, [true, false, true, true]), // ▙
        0x259A => quadrants(canvas, [true, false, false, true]), // ▚ UL+LR
        0x259B => quadrants(canvas, [true, true, true, false]), // ▛
        0x259C => quadrants(canvas, [true, true, false, true]), // ▜
        0x259D => quadrants(canvas, [false, true, false, false]), // ▝ UR
        0x259E => quadrants(canvas, [false, true, true, false]), // ▞ UR+LL
        0x259F => quadrants(canvas, [false, true, true, true]), // ▟
        _ => {}
    }
}

/// Fill any of the four cell quadrants: `[upper_left, upper_right,
/// lower_left, lower_right]`.
fn quadrants(canvas: &mut Canvas, q: [bool; 4]) {
    let Geo { w, h, cx, cy, .. } = canvas.geo;
    if q[0] {
        canvas.fill(0, cx, 0, cy, 255);
    }
    if q[1] {
        canvas.fill(cx, w, 0, cy, 255);
    }
    if q[2] {
        canvas.fill(0, cx, cy, h, 255);
    }
    if q[3] {
        canvas.fill(cx, w, cy, h, 255);
    }
}

/// Powerline separators U+E0B0–U+E0B3: solid triangles and chevron outlines.
fn draw_powerline(canvas: &mut Canvas, ch: char) {
    let Geo {
        w, h, cy, heavy, ..
    } = canvas.geo;
    // Triangle half-width at row `y`: full at the vertical midline, zero at
    // top/bottom. Point is at (w, cy) for right-facing, (0, cy) for left.
    let extent = |y: usize| -> usize {
        let dy = (y as f32 - cy as f32).abs();
        let frac = 1.0 - (dy / cy.max(1) as f32);
        (frac.max(0.0) * w as f32).round() as usize
    };
    match ch as u32 {
        0xE0B0 => {
            // Solid right triangle: (0,0),(w,cy),(0,h).
            for y in 0..h {
                let x1 = extent(y).min(w - 1) + 1;
                canvas.fill(0, x1, y, y + 1, 255);
            }
        }
        0xE0B2 => {
            // Solid left triangle: (w,0),(0,cy),(w,h).
            for y in 0..h {
                let x0 = w.saturating_sub(extent(y).min(w - 1) + 1);
                canvas.fill(x0, w, y, y + 1, 255);
            }
        }
        0xE0B1 => chevron(canvas, true, heavy), // right chevron outline
        0xE0B3 => chevron(canvas, false, heavy), // left chevron outline
        _ => {}
    }
}

/// A right (`>`) or left (`<`) chevron outline: two slanted strokes of
/// thickness `t` meeting at the vertical midline.
fn chevron(canvas: &mut Canvas, right: bool, t: usize) {
    let Geo { w, h, cy, .. } = canvas.geo;
    for y in 0..h {
        // Edge x at row y: point at (w-1, cy) for right / (0, cy) for left.
        let dy = (y as f32 - cy as f32).abs();
        let frac = 1.0 - (dy / cy.max(1) as f32);
        let ex = if right {
            (frac.max(0.0) * (w - 1) as f32).round() as usize
        } else {
            ((1.0 - frac.max(0.0)) * (w - 1) as f32).round() as usize
        };
        let x0 = ex.saturating_sub(t / 2);
        canvas.fill(x0, x0 + t, y, y + 1, 255);
    }
}

/// Arm weights `[up, right, down, left]` for a box-drawing joint/line char,
/// or `None` for codepoints handled by a specialised path (dashes, arcs,
/// diagonals, blocks).
fn arms(ch: char) -> Option<[Weight; 4]> {
    let a = match ch as u32 {
        // Straight lines.
        0x2500 => [N, L, N, L],
        0x2501 => [N, H, N, H],
        0x2502 => [L, N, L, N],
        0x2503 => [H, N, H, N],
        // Corners: down+right, down+left, up+right, up+left with light/heavy mixes.
        0x250C => [N, L, L, N],
        0x250D => [N, H, L, N],
        0x250E => [N, L, H, N],
        0x250F => [N, H, H, N],
        0x2510 => [N, N, L, L],
        0x2511 => [N, N, L, H],
        0x2512 => [N, N, H, L],
        0x2513 => [N, N, H, H],
        0x2514 => [L, L, N, N],
        0x2515 => [L, H, N, N],
        0x2516 => [H, L, N, N],
        0x2517 => [H, H, N, N],
        0x2518 => [L, N, N, L],
        0x2519 => [L, N, N, H],
        0x251A => [H, N, N, L],
        0x251B => [H, N, N, H],
        // Vertical + right tees (├ family).
        0x251C => [L, L, L, N],
        0x251D => [L, H, L, N],
        0x251E => [H, L, L, N],
        0x251F => [L, L, H, N],
        0x2520 => [H, L, H, N],
        0x2521 => [H, H, L, N],
        0x2522 => [L, H, H, N],
        0x2523 => [H, H, H, N],
        // Vertical + left tees (┤ family).
        0x2524 => [L, N, L, L],
        0x2525 => [L, N, L, H],
        0x2526 => [H, N, L, L],
        0x2527 => [L, N, H, L],
        0x2528 => [H, N, H, L],
        0x2529 => [H, N, L, H],
        0x252A => [L, N, H, H],
        0x252B => [H, N, H, H],
        // Horizontal + down tees (┬ family).
        0x252C => [N, L, L, L],
        0x252D => [N, L, L, H],
        0x252E => [N, H, L, L],
        0x252F => [N, H, L, H],
        0x2530 => [N, L, H, L],
        0x2531 => [N, L, H, H],
        0x2532 => [N, H, H, L],
        0x2533 => [N, H, H, H],
        // Horizontal + up tees (┴ family).
        0x2534 => [L, L, N, L],
        0x2535 => [L, L, N, H],
        0x2536 => [L, H, N, L],
        0x2537 => [L, H, N, H],
        0x2538 => [H, L, N, L],
        0x2539 => [H, L, N, H],
        0x253A => [H, H, N, L],
        0x253B => [H, H, N, H],
        // Crosses (┼ family).
        0x253C => [L, L, L, L],
        0x253D => [L, L, L, H],
        0x253E => [L, H, L, L],
        0x253F => [L, H, L, H],
        0x2540 => [H, L, L, L],
        0x2541 => [L, L, H, L],
        0x2542 => [H, L, H, L],
        0x2543 => [H, L, L, H],
        0x2544 => [H, H, L, L],
        0x2545 => [L, L, H, H],
        0x2546 => [L, H, H, L],
        0x2547 => [H, H, L, H],
        0x2548 => [L, H, H, H],
        0x2549 => [H, L, H, H],
        0x254A => [H, H, H, L],
        0x254B => [H, H, H, H],
        // Double lines and single/double joints.
        0x2550 => [N, D, N, D],
        0x2551 => [D, N, D, N],
        0x2552 => [N, D, L, N],
        0x2553 => [N, L, D, N],
        0x2554 => [N, D, D, N],
        0x2555 => [N, N, L, D],
        0x2556 => [N, N, D, L],
        0x2557 => [N, N, D, D],
        0x2558 => [L, D, N, N],
        0x2559 => [D, L, N, N],
        0x255A => [D, D, N, N],
        0x255B => [L, N, N, D],
        0x255C => [D, N, N, L],
        0x255D => [D, N, N, D],
        0x255E => [L, D, L, N],
        0x255F => [D, L, D, N],
        0x2560 => [D, D, D, N],
        0x2561 => [L, N, L, D],
        0x2562 => [D, N, D, L],
        0x2563 => [D, N, D, D],
        0x2564 => [N, D, L, D],
        0x2565 => [N, L, D, L],
        0x2566 => [N, D, D, D],
        0x2567 => [L, D, N, D],
        0x2568 => [D, L, N, L],
        0x2569 => [D, D, N, D],
        0x256A => [L, D, L, D],
        0x256B => [D, L, D, L],
        0x256C => [D, D, D, D],
        // Half lines and mixed light/heavy stubs.
        0x2574 => [N, N, N, L],
        0x2575 => [L, N, N, N],
        0x2576 => [N, L, N, N],
        0x2577 => [N, N, L, N],
        0x2578 => [N, N, N, H],
        0x2579 => [H, N, N, N],
        0x257A => [N, H, N, N],
        0x257B => [N, N, H, N],
        0x257C => [N, H, N, L],
        0x257D => [L, N, H, N],
        0x257E => [N, L, N, H],
        0x257F => [H, N, L, N],
        _ => return None,
    };
    Some(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic metrics (no real font): 10x20 cell.
    fn metrics() -> Metrics {
        Metrics {
            cell_w: 10.0,
            cell_h: 20.0,
            ascent: 16.0,
            descent: 4.0,
            line_gap: 0.0,
            underline_position: -2.0,
            underline_thickness: 1.0,
        }
    }

    fn at(g: &BuiltinGlyph, x: usize, y: usize) -> u8 {
        g.coverage[y * g.width as usize + x]
    }

    fn all_builtin() -> impl Iterator<Item = char> {
        (0x2500..=0x259F)
            .chain(0xE0B0..=0xE0B3)
            .filter_map(char::from_u32)
    }

    #[test]
    fn every_covered_codepoint_is_builtin_and_masks_to_cell_dims() {
        let m = metrics();
        let (ew, eh) = (m.cell_w.ceil() as u32, m.cell_h.ceil() as u32);
        for ch in all_builtin() {
            assert!(is_builtin_glyph(ch), "{ch:?} must be a builtin glyph");
            let g = draw_builtin(ch, &m);
            assert_eq!(
                (g.width, g.height),
                (ew, eh),
                "{ch:?} mask must be exactly the cell box"
            );
            assert_eq!(g.coverage.len(), (ew * eh) as usize);
        }
    }

    #[test]
    fn non_builtin_codepoints_are_rejected() {
        assert!(!is_builtin_glyph('A'));
        assert!(!is_builtin_glyph('日'));
        assert!(!is_builtin_glyph('\u{24FF}'));
        assert!(!is_builtin_glyph('\u{25A0}')); // just past the block
        assert!(!is_builtin_glyph('\u{E0AF}'));
        assert!(!is_builtin_glyph('\u{E0B4}'));
    }

    #[test]
    fn horizontal_light_line_spans_full_width_and_is_centered() {
        let g = draw_builtin('\u{2500}', &metrics()); // ─
        let (w, h) = (g.width as usize, g.height as usize);
        let cy = h / 2;
        for x in 0..w {
            assert!(at(&g, x, cy) > 0, "column {x} of ─ must have ink");
        }
        // Continuity: leftmost and rightmost columns inked so neighbours join.
        assert!(at(&g, 0, cy) > 0, "leftmost column must be inked");
        assert!(at(&g, w - 1, cy) > 0, "rightmost column must be inked");
        // Centered: top and bottom rows are empty.
        for x in 0..w {
            assert_eq!(at(&g, x, 0), 0, "top row of ─ must be empty");
            assert_eq!(at(&g, x, h - 1), 0, "bottom row of ─ must be empty");
        }
    }

    #[test]
    fn vertical_light_line_spans_full_height() {
        let g = draw_builtin('\u{2502}', &metrics()); // │
        let (w, h) = (g.width as usize, g.height as usize);
        let cx = w / 2;
        for y in 0..h {
            assert!(at(&g, cx, y) > 0, "row {y} of │ must have ink");
        }
        assert!(at(&g, cx, 0) > 0, "top row must be inked");
        assert!(at(&g, cx, h - 1) > 0, "bottom row must be inked");
        // Left/right edges empty.
        for y in 0..h {
            assert_eq!(at(&g, 0, y), 0, "left edge of │ must be empty");
        }
    }

    #[test]
    fn full_block_is_fully_opaque() {
        let g = draw_builtin('\u{2588}', &metrics()); // █
        assert!(
            g.coverage.iter().all(|&c| c == 255),
            "█ must be fully opaque"
        );
    }

    #[test]
    fn upper_half_block_fills_top_only() {
        let g = draw_builtin('\u{2580}', &metrics()); // ▀
        let (w, h) = (g.width as usize, g.height as usize);
        let cy = h / 2;
        for y in 0..cy {
            for x in 0..w {
                assert_eq!(at(&g, x, y), 255, "top half of ▀ must be opaque");
            }
        }
        for y in cy..h {
            for x in 0..w {
                assert_eq!(at(&g, x, y), 0, "bottom half of ▀ must be empty");
            }
        }
    }

    #[test]
    fn shades_use_partial_uniform_alpha() {
        for (ch, expect) in [('\u{2591}', 64u8), ('\u{2592}', 128), ('\u{2593}', 192)] {
            let g = draw_builtin(ch, &metrics());
            assert!(
                g.coverage.iter().all(|&c| c == expect),
                "{ch:?} must be uniform alpha {expect}"
            );
        }
    }

    #[test]
    fn top_left_corner_has_right_and_down_arms_only() {
        let g = draw_builtin('\u{250C}', &metrics()); // ┌
        let (w, h) = (g.width as usize, g.height as usize);
        let (cx, cy) = (w / 2, h / 2);
        // Right arm reaches the right edge at the middle row.
        assert!(
            at(&g, w - 1, cy) > 0,
            "┌ must ink the right edge middle row"
        );
        // Down arm reaches the bottom edge at the middle column.
        assert!(
            at(&g, cx, h - 1) > 0,
            "┌ must ink the bottom edge middle column"
        );
        // No left or top arm.
        for y in 0..h {
            assert_eq!(at(&g, 0, y), 0, "┌ must not ink the left edge");
        }
        for x in 0..w {
            assert_eq!(at(&g, x, 0), 0, "┌ must not ink the top edge");
        }
    }

    #[test]
    fn other_corners_have_expected_arms() {
        let m = metrics();
        let (w, h) = (m.cell_w.ceil() as usize, m.cell_h.ceil() as usize);
        let (cx, cy) = (w / 2, h / 2);
        // ┐ down+left
        let g = draw_builtin('\u{2510}', &m);
        assert!(at(&g, 0, cy) > 0 && at(&g, cx, h - 1) > 0);
        assert_eq!(at(&g, w - 1, cy), 0, "┐ has no right arm");
        // └ up+right
        let g = draw_builtin('\u{2514}', &m);
        assert!(at(&g, w - 1, cy) > 0 && at(&g, cx, 0) > 0);
        assert_eq!(at(&g, 0, cy), 0, "└ has no left arm");
        // ┘ up+left
        let g = draw_builtin('\u{2518}', &m);
        assert!(at(&g, 0, cy) > 0 && at(&g, cx, 0) > 0);
        assert_eq!(at(&g, w - 1, cy), 0, "┘ has no right arm");
    }

    #[test]
    fn double_horizontal_has_two_separated_rails() {
        let g = draw_builtin('\u{2550}', &metrics()); // ═
        let (w, h) = (g.width as usize, g.height as usize);
        let cx = w / 2;
        // Profile of the middle column: two inked runs split by a gap.
        let mut runs = 0;
        let mut prev = false;
        for y in 0..h {
            let inked = at(&g, cx, y) > 0;
            if inked && !prev {
                runs += 1;
            }
            prev = inked;
        }
        assert_eq!(runs, 2, "═ must show two separated horizontal rails");
    }

    #[test]
    fn powerline_right_triangle_is_solid_left_and_a_point_at_right() {
        let g = draw_builtin('\u{E0B0}', &metrics()); //
        let (w, h) = (g.width as usize, g.height as usize);
        // Leftmost column fully inked (the triangle's flat left edge).
        for y in 0..h {
            assert!(at(&g, 0, y) > 0, "E0B0 leftmost column must be fully inked");
        }
        // Rightmost column tapers to ~a single pixel at the vertical midline.
        let right_inked = (0..h).filter(|&y| at(&g, w - 1, y) > 0).count();
        assert!(
            right_inked <= 3,
            "E0B0 rightmost column must taper to ~a point, got {right_inked}"
        );
    }

    #[test]
    fn dashes_leave_gaps() {
        let g = draw_builtin('\u{2504}', &metrics()); // ┄ light triple dash
        let (w, h) = (g.width as usize, g.height as usize);
        let cy = h / 2;
        let inked: usize = (0..w).filter(|&x| at(&g, x, cy) > 0).count();
        assert!(
            inked > 0 && inked < w,
            "┄ must be dashed, not solid: {inked}/{w}"
        );
    }
}
