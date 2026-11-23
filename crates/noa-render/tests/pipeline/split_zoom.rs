//! Split-zoom transition frames (`docs/specs/split-zoom-animation.md`): the
//! sibling fade through `Uniforms::pane_alpha`, the `ZoomUnderlay` that keeps
//! siblings from showing through the revealed pane, and the content origin
//! that anchors the transition pane's grid (cells and kitty images) to the
//! clip's top-left corner.

use super::shared::*;
use noa_core::{CellAttrs, Color, DEFAULT_GRID_PADDING, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row};
use noa_render::{
    FrameSnapshot, PaneFrame, PaneId, PaneRect, Renderer, SplitZoomFx, Theme, Uniforms,
};

const W: u32 = 240;
const H: u32 = 64;
const SIBLING: PaneId = PaneId::new(1);
const ZOOMED: PaneId = PaneId::new(2);
const SIBLING_TILE: PaneRect = PaneRect::new(0, 0, 120, 64);
const ZOOMED_TILE: PaneRect = PaneRect::new(121, 0, 119, 64);
const FULL: PaneRect = PaneRect::new(0, 0, W, H);
/// Half-way between the zoomed pane's tile and the full rect; its origin
/// differs from the full rect's.
const CLIP: PaneRect = PaneRect::new(60, 0, 180, 64);
const RED: Rgb = Rgb::new(200, 0, 0);
const GREEN: Rgb = Rgb::new(0, 200, 0);

fn cell(ch: char, fg: Color, bg: Color) -> Cell {
    Cell {
        ch,
        grapheme: None,
        fg,
        bg,
        underline_color: None,
        hyperlink: None,
        attrs: CellAttrs::empty(),
    }
}

/// A snapshot of `rows` with the cursor hidden, so no cursor quad lands on
/// the cells the test reads back.
fn grid_snapshot(rows: Vec<Vec<Cell>>) -> FrameSnapshot {
    let mut snap = snapshot_for_text(" ");
    let cols = rows[0].len() as u16;
    snap.rows_n = rows.len() as u16;
    snap.cols = cols;
    snap.row_dirty = vec![true; rows.len()];
    snap.rows = rows
        .into_iter()
        .map(|cells| Row::from_cells(cells, false, true))
        .collect::<Vec<_>>()
        .into();
    snap.cursor = Cursor {
        visible: false,
        ..Cursor::default()
    };
    snap
}

/// Row 0: explicit red background across the pane; row 1: 'M' glyphs.
fn sibling_snapshot() -> FrameSnapshot {
    grid_snapshot(vec![
        vec![cell(' ', Color::Default, Color::Rgb(RED)); 16],
        vec![cell('M', Color::Default, Color::Default); 16],
    ])
}

/// A solid green glyph cell at (0, 0), default-background cells after it.
fn zoomed_snapshot() -> FrameSnapshot {
    let mut row = vec![cell(' ', Color::Default, Color::Default); 24];
    row[0] = cell('X', Color::Rgb(GREEN), Color::Rgb(GREEN));
    grid_snapshot(vec![row])
}

fn frames<'a>(
    sibling: &'a FrameSnapshot,
    zoomed: &'a FrameSnapshot,
    zoomed_rect: PaneRect,
) -> [PaneFrame<'a>; 2] {
    [
        PaneFrame {
            pane: SIBLING,
            rect: SIBLING_TILE,
            snapshot: sibling,
        },
        PaneFrame {
            pane: ZOOMED,
            rect: zoomed_rect,
            snapshot: zoomed,
        },
    ]
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn is_green(px: [u8; 4]) -> bool {
    px[1] > 150 && px[0] < 80 && px[2] < 80
}

fn is_blue(px: [u8; 4]) -> bool {
    px[2] > 150 && px[0] < 80 && px[1] < 80
}

fn draw(
    renderer: &mut Renderer,
    font: &mut FontGrid,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    panes: &[PaneFrame<'_>],
    fx: Option<SplitZoomFx>,
) -> Vec<u8> {
    renderer.set_split_zoom_fx(fx);
    renderer.rebuild_panes(panes, font, &Theme::new());
    renderer.sync_atlas(device, queue, font);
    let (target, view) = render_target(device, W, H);
    renderer.draw_rebuilt_panes(device, queue, &view, None, None, false);
    read_rgba_pixels(device, queue, &target, W, H)
}

#[test]
fn split_zoom_fx_fades_siblings_underlays_the_clip_and_anchors_content() {
    // The Rust/WGSL uniform lockstep: `pane_alpha` replaced a pad float.
    assert_eq!(std::mem::size_of::<Uniforms>(), 160);
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split-zoom fx test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: W, h: H });
    let cell_w = font.metrics().cell_w;
    let cell_h = font.metrics().cell_h;
    let pad_left = DEFAULT_GRID_PADDING.left;
    let pad_top = DEFAULT_GRID_PADDING.top;
    let fx = SplitZoomFx {
        pane: ZOOMED,
        clip: CLIP,
        content_origin: (CLIP.x, CLIP.y),
        sibling_alpha: 0.5,
    };
    // Where the zoomed pane's grid starts with and without the content origin.
    let content_x = fx.content_origin.0 as f32 + pad_left;
    let content_y = fx.content_origin.1 as f32 + pad_top;
    let untranslated_x = FULL.x as f32 + pad_left;
    let row0_y = (pad_top + cell_h * 0.5) as u32;

    let sibling = sibling_snapshot();
    let zoomed = zoomed_snapshot();
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    // The plain split, before any fx was ever set.
    let unzoomed = draw(
        &mut renderer,
        &mut font,
        &device,
        &queue,
        &frames(&sibling, &zoomed, ZOOMED_TILE),
        None,
    );
    // Mid-transition: the zoomed pane's grid already spans the full rect.
    let tween = draw(
        &mut renderer,
        &mut font,
        &device,
        &queue,
        &frames(&sibling, &zoomed, FULL),
        Some(fx),
    );
    // The plain split again after the fx is cleared.
    let after = draw(
        &mut renderer,
        &mut font,
        &device,
        &queue,
        &frames(&sibling, &zoomed, ZOOMED_TILE),
        None,
    );

    // Underlay: the revealed area outside the zoomed pane's tile holds the
    // frame's base, not the sibling (whose red row 0 and 'M' row 1 reach it
    // in the plain split), past the zoomed pane's one green cell.
    let background = pixel(&tween, W - 10, H - 4);
    assert_eq!(
        pixel(&unzoomed, W - 10, H - 4),
        background,
        "the underlay must restore exactly the base the frame cleared to"
    );
    let underlay_x0 = (content_x + cell_w).ceil() as u32 + 2;
    assert!(underlay_x0 < SIBLING_TILE.right());
    let red_in_plain = pixel(&unzoomed, SIBLING_TILE.right() - 4, row0_y);
    assert!(
        red_in_plain[0] > 150 && red_in_plain[1] < 80,
        "the sibling's red row must reach the revealed area in the plain split: {red_in_plain:?}"
    );
    for y in 0..H {
        for x in underlay_x0..SIBLING_TILE.right() {
            assert_eq!(
                pixel(&tween, x, y),
                background,
                "sibling showed through the revealed area at ({x}, {y})"
            );
        }
    }

    // Sibling fade: outside the clip its red background lies strictly
    // between the base and the opaque red of the plain split.
    let fade_x = (pad_left + cell_w * 1.5) as u32;
    assert!(fade_x < CLIP.x);
    let opaque = pixel(&unzoomed, fade_x, row0_y);
    let faded = pixel(&tween, fade_x, row0_y);
    assert!(
        background[0] < faded[0] && faded[0] < opaque[0],
        "faded sibling {faded:?} must lie strictly between base {background:?} and opaque {opaque:?}"
    );
    for channel in 0..3 {
        let (lo, hi) = (
            background[channel].min(opaque[channel]),
            background[channel].max(opaque[channel]),
        );
        assert!(
            (lo..=hi).contains(&faded[channel]),
            "channel {channel}: {faded:?} outside {background:?}..{opaque:?}"
        );
    }

    // Content origin: cell (0, 0) is drawn at content_origin + padding, not
    // at the full rect's origin + padding.
    let cell0_x = content_x as u32 + 1;
    let cell0_y = content_y as u32 + 1;
    assert!(
        is_green(pixel(&tween, cell0_x, cell0_y)),
        "cell (0, 0) must start at the content origin: {:?}",
        pixel(&tween, cell0_x, cell0_y)
    );
    assert!(is_green(pixel(
        &tween,
        (content_x + cell_w) as u32 - 2,
        (content_y + cell_h) as u32 - 2
    )));
    assert!(!is_green(pixel(
        &tween,
        (content_x + cell_w).ceil() as u32 + 1,
        cell0_y
    )));
    assert!(!is_green(pixel(&tween, untranslated_x as u32 + 1, cell0_y)));

    // `pane_alpha = 1` and a cleared fx draw the plain split unchanged.
    assert!(
        unzoomed == after,
        "clearing the fx must restore the plain split's pixels"
    );

    // A kitty image at the zoomed pane's (0, 0) moves with the content origin.
    let mut imaged = image_snapshot(2, 1, Color::Default, [0, 0, 255, 255], 0, 0);
    imaged.cursor.visible = false;
    let with_image = draw(
        &mut renderer,
        &mut font,
        &device,
        &queue,
        &frames(&sibling, &imaged, FULL),
        Some(fx),
    );
    let image_y = (content_y + cell_h * 0.5) as u32;
    assert!(
        is_blue(pixel(&with_image, cell0_x, image_y)),
        "the image must start at the content origin: {:?}",
        pixel(&with_image, cell0_x, image_y)
    );
    assert!(!is_blue(pixel(&with_image, content_x as u32 - 3, image_y)));
    assert!(!is_blue(pixel(
        &with_image,
        (content_x + 2.0 * cell_w).ceil() as u32 + 3,
        image_y
    )));

    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll split-zoom draws");
    let err = pollster::block_on(error_scope.pop());
    assert!(
        err.is_none(),
        "wgpu validation error during split-zoom fx draws: {err:?}"
    );
}
