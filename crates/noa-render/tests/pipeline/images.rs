use super::shared::*;
use noa_core::{Color, DEFAULT_GRID_PADDING, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_render::{
    BackgroundImage, BackgroundImageFit, BackgroundImagePosition, FrameSnapshot, Renderer, Theme,
};
use std::sync::Arc;

// Kitty-graphics image layer (design Step R)

#[test]
fn image_layer_draws_image_and_text_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image-layer draw test");
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
    renderer.resize(PixelSize { w: 96, h: 64 });

    // A textful grid with an opaque image over it at z=0 (above text).
    let mut snap = image_snapshot(
        6,
        4,
        Color::Rgb(Rgb::new(180, 0, 0)),
        [0, 0, 255, 255],
        0,
        0,
    );
    snap.rows[0].cells[0].ch = 'A';
    snap.rows[0].cells[0].fg = Color::Palette(2);

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 96, 64);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing an image placement mixed with text: {err:?}"
    );
}

#[test]
fn image_z_band_controls_whether_it_covers_the_cell_background() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image z-band pixel test");
        return;
    };
    let width = 96u32;
    let height = 64u32;
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
    renderer.resize(PixelSize {
        w: width,
        h: height,
    });

    let center = |pixels: &[u8]| -> [u8; 4] {
        let x = (width / 2) as usize;
        let y = (height / 2) as usize;
        let i = (y * width as usize + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // z=0 (above text): the opaque blue image draws over the red cell background.
    let above = image_snapshot(
        40,
        40,
        Color::Rgb(Rgb::new(200, 0, 0)),
        [0, 0, 255, 255],
        0,
        0,
    );
    renderer.rebuild_cells(&above, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target, view) = render_target(&device, width, height);
    renderer.draw(&device, &queue, &view);
    let above_px = center(&read_rgba_pixels(&device, &queue, &target, width, height));
    assert!(
        above_px[2] > above_px[0],
        "z>=0 image must draw OVER the cell background (blue dominant), got {above_px:?}"
    );

    // z below the background threshold: the image draws UNDER the background, so
    // the red background covers it.
    let below = image_snapshot(
        40,
        40,
        Color::Rgb(Rgb::new(200, 0, 0)),
        [0, 0, 255, 255],
        0,
        -2_000_000_000,
    );
    renderer.rebuild_cells(&below, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target2, view2) = render_target(&device, width, height);
    renderer.draw(&device, &queue, &view2);
    let below_px = center(&read_rgba_pixels(&device, &queue, &target2, width, height));
    assert!(
        below_px[0] > below_px[2],
        "z<bg-threshold image must draw UNDER the cell background (red dominant), got {below_px:?}"
    );
}

#[test]
fn image_texture_reuploads_only_on_epoch_bump() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping image epoch-reupload test");
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
    renderer.resize(PixelSize { w: 64, h: 48 });
    let (_target, view) = render_target(&device, 64, 48);

    let draw = |renderer: &mut Renderer, font: &mut FontGrid, epoch: u64| {
        let snap = image_snapshot(
            4,
            4,
            Color::Rgb(Rgb::new(0, 80, 0)),
            [255, 255, 0, 255],
            epoch,
            0,
        );
        renderer.rebuild_cells(&snap, font, &Theme::new());
        renderer.sync_atlas(&device, &queue, font);
        renderer.draw(&device, &queue, &view);
    };

    draw(&mut renderer, &mut font, 0);
    let after_first = renderer.image_texture_upload_count();
    assert!(after_first >= 1, "first frame uploads the image texture");

    draw(&mut renderer, &mut font, 0);
    assert_eq!(
        renderer.image_texture_upload_count(),
        after_first,
        "same (id, epoch) must reuse the cached texture — no re-upload"
    );

    draw(&mut renderer, &mut font, 1);
    assert!(
        renderer.image_texture_upload_count() > after_first,
        "an epoch bump must force a texture re-upload"
    );
}

#[test]
fn unicode_placeholder_resolves_and_draws_over_text() {
    use noa_core::GridSize;
    use noa_grid::Terminal;
    use noa_vt::Stream;

    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping placeholder draw test");
        return;
    };
    let width = 96u32;
    let height = 64u32;
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
    renderer.resize(PixelSize {
        w: width,
        h: height,
    });

    // A terminal holding a solid-blue image placed as a virtual placement (U=1)
    // at z=0 (above text), referenced only through a Unicode placeholder cell.
    let mut term = Terminal::new(GridSize::new(6, 4));
    term.set_pixel_metrics(12, 16, width, height);
    let mut stream = Stream::new();
    let blue: Vec<u8> = [0u8, 0, 255, 255]
        .iter()
        .copied()
        .cycle()
        .take(4 * 4 * 4)
        .collect();
    let mut apc = b"\x1b_Ga=T,f=32,s=4,v=4,i=1,U=1,c=6,r=4,C=1;".to_vec();
    let mut b64 = Vec::new();
    noa_grid_test_base64(&blue, &mut b64);
    apc.extend_from_slice(&b64);
    apc.extend_from_slice(b"\x1b\\");
    stream.feed(&apc, &mut term);
    // Fill the whole grid with placeholder cells (image id 1). The first cell of
    // each grid row anchors row/column 0; the rest infer.
    for y in 0..4usize {
        for x in 0..6usize {
            let cell = &mut term.primary.grid[y].cells[x];
            cell.ch = noa_grid::PLACEHOLDER;
            cell.fg = Color::Rgb(Rgb::new(0, 0, 1));
            cell.combining.clear();
            if x == 0 {
                // Row index = y (diacritics table values 0..3).
                cell.combining.push(placeholder_diacritic(y as u32));
                cell.combining.push(placeholder_diacritic(0));
            }
        }
    }

    let snap = FrameSnapshot::from_terminal(&mut term);
    assert!(
        !snap.image_placements.is_empty(),
        "the placeholder cells must resolve to at least one image placement"
    );

    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (target, view) = render_target(&device, width, height);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "placeholder draw hit a wgpu validation error: {err:?}"
    );

    // Center pixel: the opaque blue image covers the grid.
    let pixels = read_rgba_pixels(&device, &queue, &target, width, height);
    let i = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
    let px = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
    assert!(
        px[2] > px[0],
        "placeholder image (blue) must be visible over the cells, got {px:?}"
    );
}

/// The row/column diacritic encoding `value` (Kitty's table; values 0..=3 are
/// the first four entries). Kept in the test to avoid exposing the table.
fn placeholder_diacritic(value: u32) -> char {
    const FIRST_FOUR: [char; 4] = ['\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}'];
    FIRST_FOUR[value as usize]
}

/// Minimal base64 encoder for the placeholder test's image payload.
fn noa_grid_test_base64(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[(n >> 18 & 63) as usize]);
        out.push(ALPHABET[(n >> 12 & 63) as usize]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize]
        } else {
            b'='
        });
    }
}

/// AC-9 / NFR-3 (headless GPU): a valid background image draws one frame in the
/// lowest z band with no wgpu validation error, and its quad's alpha reflects
/// `background-image-opacity` INDEPENDENTLY of the clear color's
/// `background-opacity`. A 2x2 opaque image at `fit = contain` into a 64x32
/// surface covers a centered 32-wide band (letterbox left/right); reading the
/// rendered alpha back shows the covered band at the image-opacity-blended
/// alpha and the letterbox at the clear (background-opacity) alpha — proving
/// the image is not scaled by `background-opacity`.
#[test]
fn background_image_draws_below_cells_with_independent_opacity() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping background-image GPU draw test");
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
    let (w, h) = (64u32, 32u32);
    renderer.resize(PixelSize { w, h });

    // Clear color alpha = background-opacity = 0.3.
    renderer.set_background_opacity(0.3);
    // 2x2 fully-opaque red image, drawn at background-image-opacity = 0.5.
    let image = BackgroundImage {
        rgba: Arc::from(vec![255u8, 0, 0, 255].repeat(4)),
        width: 2,
        height: 2,
        fit: BackgroundImageFit::Contain,
        position: BackgroundImagePosition::Center,
        repeat: false,
        opacity: 0.5,
    };
    renderer.set_background_image(&device, &queue, Some(image));
    assert!(renderer.has_background_image());

    // Blank snapshot: the single default cell emits no background quad, so the
    // frame is just the clear color + the background image.
    let snap = snapshot_for_text(" ");
    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (target, view) = render_target(&device, w, h);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error drawing the background image: {err:?}"
    );

    let pixels = read_rgba_pixels(&device, &queue, &target, w, h);
    let alpha_at = |x: u32, y: u32| -> u8 {
        let idx = ((y * w + x) * 4 + 3) as usize;
        pixels[idx]
    };
    // contain: 2x2 -> scale 16 -> 32x32 centered, covering x in [16, 48).
    let covered = alpha_at(32, 16); // center of the image band
    let letterbox = alpha_at(4, 16); // left edge, image absent

    // Clear (background-opacity 0.3) -> ~0.3 * 255 = 76.
    assert!(
        (68..=84).contains(&letterbox),
        "letterbox alpha should reflect background-opacity (~76): got {letterbox}"
    );
    // Straight-alpha blend: 0.5 (image) + 0.3 * (1 - 0.5) = 0.65 -> ~166.
    assert!(
        (158..=174).contains(&covered),
        "covered alpha should reflect background-image-opacity blended over the \
         clear color (~166), independent of background-opacity: got {covered}"
    );
    assert!(
        covered > letterbox + 40,
        "the image quad's alpha ({covered}) must clearly exceed the clear-only \
         letterbox alpha ({letterbox}), proving the image is not scaled by \
         background-opacity (NFR-3)"
    );
}

#[test]
fn background_image_transition_draws_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping background-image transition GPU test");
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
    let (w, h) = (16u32, 16u32);
    renderer.resize(PixelSize { w, h });

    let image = |rgba: [u8; 4]| BackgroundImage {
        rgba: Arc::from(Vec::from(rgba)),
        width: 1,
        height: 1,
        fit: BackgroundImageFit::Stretch,
        position: BackgroundImagePosition::Center,
        repeat: false,
        opacity: 1.0,
    };
    let red = image([255, 0, 0, 255]);
    let green = image([0, 255, 0, 255]);
    let snap = snapshot_for_text(" ");
    renderer.rebuild_cells(&snap, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    renderer.set_background_image_transition(&device, &queue, Some(red), Some(green), 0.5);
    assert!(renderer.has_background_image());

    let (_target, view) = render_target(&device, w, h);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw(&device, &queue, &view);
    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error drawing background-image transition: {err:?}"
    );
}
