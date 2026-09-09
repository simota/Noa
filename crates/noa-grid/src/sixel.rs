//! SIXEL rasterization into straight RGBA8.
//!
//! This is intentionally terminal-state free: `noa-vt` recognizes the DCS
//! envelope, this module turns the SIXEL bytecode into pixels, and
//! `terminal::kitty_graphics` stores/places the resulting image through the
//! existing image layer.

use noa_core::{Rgb, xterm_palette};
use noa_vt::SixelGraphicsCommand;

use crate::kitty::{KittyError, MAX_IMAGE_DIM, TOTAL_BYTES_LIMIT};

/// A fully rasterized SIXEL image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SixelRaster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

const COLOR_REGISTERS: usize = 256;

/// Upper bound on the number of pixel writes one SIXEL command may perform.
///
/// The image dimension and byte limits bound the *canvas*, not the *work*:
/// `!<count><sixel>` followed by `$` (carriage return) repaints the same
/// row band without growing the image, so a few kilobytes of data could
/// otherwise demand hundreds of millions of pixel writes under the terminal
/// lock. Set to twice the pixel count of the largest allowed image so a
/// legitimately painted maximum-size image (each pixel written once) fits
/// with room for ordinary overdraw.
const MAX_SIXEL_PIXEL_WRITES: u64 = 2 * (TOTAL_BYTES_LIMIT / 4) as u64;

/// Growable RGBA canvas. Storage is over-allocated geometrically (row
/// stride `cap_width`, `cap_height` rows) so that a stream which widens the
/// image one column at a time costs amortized O(pixels) rather than the
/// O(height × width²) of reallocating an exact-fit buffer per column.
struct Canvas {
    width: u32,
    height: u32,
    cap_width: u32,
    cap_height: u32,
    pixels: Vec<u8>,
    background: [u8; 4],
}

impl Canvas {
    fn new(background: [u8; 4]) -> Self {
        Self {
            width: 0,
            height: 0,
            cap_width: 0,
            cap_height: 0,
            pixels: Vec::new(),
            background,
        }
    }

    fn ensure_size(&mut self, width: u32, height: u32) -> Result<(), KittyError> {
        if width <= self.width && height <= self.height {
            return Ok(());
        }
        let width = width.max(self.width);
        let height = height.max(self.height);
        if width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return Err(KittyError::TooBig);
        }
        if bytes_for(width, height)? > TOTAL_BYTES_LIMIT {
            return Err(KittyError::TooBig);
        }
        if width <= self.cap_width && height <= self.cap_height {
            self.width = width;
            self.height = height;
            return Ok(());
        }

        // Grow only the axes that exceed capacity; fall back to the exact
        // requested size when doubling would overshoot the global byte budget
        // (the request itself is known to fit).
        let mut cap_w = if width > self.cap_width {
            width
                .max(self.cap_width.saturating_mul(2))
                .min(MAX_IMAGE_DIM)
        } else {
            self.cap_width
        };
        let mut cap_h = if height > self.cap_height {
            height
                .max(self.cap_height.saturating_mul(2))
                .min(MAX_IMAGE_DIM)
        } else {
            self.cap_height
        };
        if bytes_for(cap_w, cap_h)? > TOTAL_BYTES_LIMIT {
            cap_w = width;
            cap_h = height;
        }
        let bytes = bytes_for(cap_w, cap_h)?;

        let old = std::mem::take(&mut self.pixels);
        let mut new_pixels = vec![0u8; bytes];
        for px in new_pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&self.background);
        }
        let old_stride = self.cap_width as usize * 4;
        let new_stride = cap_w as usize * 4;
        let row_bytes = self.width as usize * 4;
        for y in 0..self.height as usize {
            new_pixels[y * new_stride..y * new_stride + row_bytes]
                .copy_from_slice(&old[y * old_stride..y * old_stride + row_bytes]);
        }

        self.width = width;
        self.height = height;
        self.cap_width = cap_w;
        self.cap_height = cap_h;
        self.pixels = new_pixels;
        Ok(())
    }

    fn advance_blank(&mut self, x: u32, y: u32, count: u32) -> Result<(), KittyError> {
        if count == 0 {
            return Ok(());
        }
        self.ensure_size(x.saturating_add(count), y.saturating_add(6))
    }

    fn set_pixel(&mut self, x: u32, y: u32, color: Rgb) -> Result<(), KittyError> {
        self.ensure_size(x.saturating_add(1), y.saturating_add(1))?;
        let i = ((y as usize * self.cap_width as usize) + x as usize) * 4;
        self.pixels[i..i + 4].copy_from_slice(&[color.r, color.g, color.b, 0xff]);
        Ok(())
    }

    fn finish(mut self, min_width: u32, min_height: u32) -> Result<SixelRaster, KittyError> {
        self.ensure_size(self.width.max(min_width), self.height.max(min_height))?;
        if self.width == 0 || self.height == 0 {
            return Err(KittyError::Invalid);
        }
        let rgba = if self.cap_width == self.width && self.cap_height == self.height {
            self.pixels
        } else {
            let stride = self.cap_width as usize * 4;
            let row_bytes = self.width as usize * 4;
            let mut compact = Vec::with_capacity(row_bytes * self.height as usize);
            for y in 0..self.height as usize {
                compact.extend_from_slice(&self.pixels[y * stride..y * stride + row_bytes]);
            }
            compact
        };
        Ok(SixelRaster {
            width: self.width,
            height: self.height,
            rgba,
        })
    }
}

fn bytes_for(width: u32, height: u32) -> Result<usize, KittyError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or(KittyError::TooBig)
}

/// Rasterize a parsed SIXEL command into straight RGBA8.
///
/// `terminal_bg` is the terminal's current default background: per DEC
/// STD 070, `P2` = 0 (or omitted) and 2 paint blank pixels with the
/// background color, while `P2` = 1 leaves them transparent (the existing
/// screen content shows through).
pub fn rasterize(cmd: &SixelGraphicsCommand, terminal_bg: Rgb) -> Result<SixelRaster, KittyError> {
    rasterize_with_budget(cmd, terminal_bg, MAX_SIXEL_PIXEL_WRITES)
}

/// [`rasterize`] with an explicit pixel-write budget (`max_pixel_writes`);
/// the public entry point always uses [`MAX_SIXEL_PIXEL_WRITES`].
fn rasterize_with_budget(
    cmd: &SixelGraphicsCommand,
    terminal_bg: Rgb,
    max_pixel_writes: u64,
) -> Result<SixelRaster, KittyError> {
    let mut palette = xterm_palette();
    let background = if cmd.background == 1 {
        [0, 0, 0, 0]
    } else {
        [terminal_bg.r, terminal_bg.g, terminal_bg.b, 0xff]
    };
    let mut canvas = Canvas::new(background);
    let mut current_color = 0usize;
    let mut x = 0u32;
    let mut y = 0u32;
    let mut declared_width = 0u32;
    let mut declared_height = 0u32;
    let mut budget = PixelWriteBudget {
        used: 0,
        max: max_pixel_writes,
    };

    let mut i = 0usize;
    while i < cmd.data.len() {
        let b = cmd.data[i] & 0x7f;
        match b {
            b'?'..=b'~' => {
                draw_sixel(
                    &mut canvas,
                    x,
                    y,
                    b - b'?',
                    1,
                    palette[current_color],
                    &mut budget,
                )?;
                x = x.saturating_add(1);
                i += 1;
            }
            b'!' => {
                let (count, next) = parse_decimal(&cmd.data, i + 1);
                if next >= cmd.data.len() {
                    break;
                }
                let ch = cmd.data[next] & 0x7f;
                if (b'?'..=b'~').contains(&ch) {
                    let count = count.unwrap_or(1).max(1);
                    draw_sixel(
                        &mut canvas,
                        x,
                        y,
                        ch - b'?',
                        count,
                        palette[current_color],
                        &mut budget,
                    )?;
                    x = x.saturating_add(count);
                }
                i = next + 1;
            }
            b'#' => {
                let (params, next) = parse_params(&cmd.data, i + 1);
                if let Some(&reg) = params.first() {
                    current_color = (reg as usize).min(COLOR_REGISTERS - 1);
                    if params.len() >= 5
                        && let Some(color) =
                            decode_color(params[1], params[2], params[3], params[4])
                    {
                        palette[current_color] = color;
                    }
                }
                i = next;
            }
            b'"' => {
                let (params, next) = parse_params(&cmd.data, i + 1);
                if params.len() >= 4 {
                    declared_width = params[2];
                    declared_height = params[3];
                    // Pre-size so a declared image is allocated once.
                    canvas.ensure_size(declared_width, declared_height)?;
                }
                i = next;
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                y = y.saturating_add(6);
                i += 1;
            }
            _ => i += 1,
        }
    }

    canvas.finish(declared_width, declared_height)
}

fn draw_sixel(
    canvas: &mut Canvas,
    x: u32,
    y: u32,
    value: u8,
    count: u32,
    color: Rgb,
    budget: &mut PixelWriteBudget,
) -> Result<(), KittyError> {
    canvas.advance_blank(x, y, count)?;
    if value == 0 {
        return Ok(());
    }
    // Charge the full 6-row band per column before painting so the budget
    // check runs ahead of the work, not after it.
    budget.charge(u64::from(count) * 6)?;
    for dx in 0..count {
        for bit in 0..6u32 {
            if value & (1 << bit) != 0 {
                canvas.set_pixel(x + dx, y + bit, color)?;
            }
        }
    }
    Ok(())
}

/// Running count of pixel writes for one `rasterize` call (B02).
struct PixelWriteBudget {
    used: u64,
    max: u64,
}

impl PixelWriteBudget {
    fn charge(&mut self, writes: u64) -> Result<(), KittyError> {
        self.used = self.used.saturating_add(writes);
        if self.used > self.max {
            return Err(KittyError::TooBig);
        }
        Ok(())
    }
}

fn parse_decimal(bytes: &[u8], mut i: usize) -> (Option<u32>, usize) {
    let start = i;
    let mut value = 0u32;
    while i < bytes.len() {
        let b = bytes[i] & 0x7f;
        if !b.is_ascii_digit() {
            break;
        }
        value = value.saturating_mul(10).saturating_add(u32::from(b - b'0'));
        i += 1;
    }
    ((i > start).then_some(value), i)
}

fn parse_params(bytes: &[u8], mut i: usize) -> (Vec<u32>, usize) {
    let mut params = Vec::new();
    let mut current = 0u32;
    let mut saw_digit = false;
    let mut saw_any = false;
    while i < bytes.len() {
        let b = bytes[i] & 0x7f;
        match b {
            b'0'..=b'9' => {
                saw_any = true;
                saw_digit = true;
                current = current
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
                i += 1;
            }
            b';' => {
                saw_any = true;
                params.push(if saw_digit { current } else { 0 });
                current = 0;
                saw_digit = false;
                i += 1;
            }
            _ => break,
        }
    }
    if saw_any {
        params.push(if saw_digit { current } else { 0 });
    }
    (params, i)
}

fn decode_color(space: u32, a: u32, b: u32, c: u32) -> Option<Rgb> {
    match space {
        1 => Some(hls_to_rgb(a, b, c)),
        2 => Some(Rgb::new(percent(a), percent(b), percent(c))),
        _ => None,
    }
}

fn percent(value: u32) -> u8 {
    ((value.min(100) * 255 + 50) / 100) as u8
}

/// DEC HLS: hue 0° is *blue* (120° red, 240° green), unlike the usual HSL
/// convention where 0° is red — rotate by 240° before the standard conversion.
fn hls_to_rgb(hue: u32, lightness: u32, saturation: u32) -> Rgb {
    let h = ((hue % 360 + 240) % 360) as f64 / 360.0;
    let l = lightness.min(100) as f64 / 100.0;
    let s = saturation.min(100) as f64 / 100.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return Rgb::new(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    Rgb::new(
        channel(p, q, h + 1.0 / 3.0),
        channel(p, q, h),
        channel(p, q, h - 1.0 / 3.0),
    )
}

fn channel(p: f64, q: f64, mut t: f64) -> u8 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    let v = if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: Rgb = Rgb::new(10, 20, 30);
    const BG_PX: [u8; 4] = [10, 20, 30, 255];

    fn cmd(data: &[u8]) -> SixelGraphicsCommand {
        cmd_bg(data, 0)
    }

    fn cmd_bg(data: &[u8], background: u16) -> SixelGraphicsCommand {
        SixelGraphicsCommand {
            aspect_ratio: 0,
            background,
            horizontal_grid_size: 0,
            data: data.to_vec(),
        }
    }

    fn rasterize(cmd: &SixelGraphicsCommand) -> Result<SixelRaster, KittyError> {
        super::rasterize(cmd, BG)
    }

    fn unlimited() -> PixelWriteBudget {
        PixelWriteBudget {
            used: 0,
            max: u64::MAX,
        }
    }

    #[test]
    fn rasterizes_basic_sixel_columns() {
        let image = rasterize(&cmd(b"#1;2;100;0;0@A")).unwrap();

        assert_eq!((image.width, image.height), (2, 6));
        assert_eq!(&image.rgba[0..4], &[255, 0, 0, 255]);
        let second_col_row_1 = ((image.width as usize) + 1) * 4;
        assert_eq!(
            &image.rgba[second_col_row_1..second_col_row_1 + 4],
            &[255, 0, 0, 255]
        );
    }

    #[test]
    fn repeat_advances_width() {
        let image = rasterize(&cmd(b"#2;2;0;100;0!3@")).unwrap();

        assert_eq!((image.width, image.height), (3, 6));
        for x in 0..3 {
            let i = x * 4;
            assert_eq!(&image.rgba[i..i + 4], &[0, 255, 0, 255]);
        }
    }

    #[test]
    fn raster_attributes_extend_canvas_with_background() {
        // P2 omitted/0 → blank pixels take the terminal background (DEC STD 070).
        let image = rasterize(&cmd(br#""1;1;4;7#1;2;100;0;0@"#)).unwrap();

        assert_eq!((image.width, image.height), (4, 7));
        assert_eq!(&image.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&image.rgba[(4 * 6 + 3) * 4..(4 * 6 + 4) * 4], &BG_PX);
    }

    #[test]
    fn background_select_one_is_transparent_and_two_is_opaque() {
        let transparent = rasterize(&cmd_bg(b"?", 1)).unwrap();
        assert_eq!(&transparent.rgba[0..4], &[0, 0, 0, 0]);

        let opaque = rasterize(&cmd_bg(b"?", 2)).unwrap();
        assert_eq!(&opaque.rgba[0..4], &BG_PX);
    }

    #[test]
    fn column_growth_preserves_height_capacity() {
        let mut canvas = Canvas::new(BG_PX);
        let red = Rgb::new(255, 0, 0);
        for x in 0..4096 {
            draw_sixel(&mut canvas, x, 0, b'@' - b'?', 1, red, &mut unlimited()).unwrap();
            assert_eq!(canvas.cap_height, 6);
        }
        assert_eq!(canvas.pixels.len(), 4096 * 6 * 4);

        let image = canvas.finish(0, 0).unwrap();
        assert_eq!((image.width, image.height), (4096, 6));
        for (i, pixel) in image.rgba.chunks_exact(4).enumerate() {
            assert_eq!(pixel, if i < 4096 { &[255, 0, 0, 255] } else { &BG_PX });
        }
    }

    #[test]
    fn row_growth_preserves_width_capacity() {
        let mut canvas = Canvas::new(BG_PX);
        let red = Rgb::new(255, 0, 0);
        for y in (0..4096).step_by(6) {
            draw_sixel(&mut canvas, 0, y, b'@' - b'?', 1, red, &mut unlimited()).unwrap();
            assert_eq!(canvas.cap_width, 1);
        }
        assert!(canvas.pixels.len() < 2 * 4098 * 4);

        let image = canvas.finish(0, 0).unwrap();
        assert_eq!((image.width, image.height), (1, 4098));
        for (y, pixel) in image.rgba.chunks_exact(4).enumerate() {
            assert_eq!(
                pixel,
                if y % 6 == 0 {
                    &[255, 0, 0, 255]
                } else {
                    &BG_PX
                }
            );
        }
    }

    #[test]
    fn column_at_a_time_growth_matches_exact_fit_output() {
        // Advance far down, then widen one column per sixel: the geometric
        // growth path (stride ≠ width) must compact to the same pixels an
        // exact-fit canvas would produce.
        let mut data = Vec::new();
        for _ in 0..40 {
            data.extend_from_slice(b"-");
        }
        data.extend_from_slice(b"#1;2;100;0;0");
        for _ in 0..300 {
            data.extend_from_slice(b"@");
        }
        let image = rasterize(&cmd(&data)).unwrap();

        assert_eq!((image.width, image.height), (300, 246));
        assert_eq!(image.rgba.len(), 300 * 246 * 4);
        let top_left = &image.rgba[0..4];
        assert_eq!(top_left, &BG_PX);
        let last_row_first_px = 240 * 300 * 4;
        assert_eq!(
            &image.rgba[last_row_first_px..last_row_first_px + 4],
            &[255, 0, 0, 255]
        );
        let last_row_last_px = (240 * 300 + 299) * 4;
        assert_eq!(
            &image.rgba[last_row_last_px..last_row_last_px + 4],
            &[255, 0, 0, 255]
        );
        let row_241_first = 241 * 300 * 4;
        assert_eq!(&image.rgba[row_241_first..row_241_first + 4], &BG_PX);
    }

    #[test]
    fn dec_hls_hue_zero_is_blue() {
        assert_eq!(hls_to_rgb(0, 50, 100), Rgb::new(0, 0, 255));
        assert_eq!(hls_to_rgb(120, 50, 100), Rgb::new(255, 0, 0));
        assert_eq!(hls_to_rgb(240, 50, 100), Rgb::new(0, 255, 0));
        assert_eq!(hls_to_rgb(360, 50, 100), Rgb::new(0, 0, 255));
        assert_eq!(hls_to_rgb(0, 50, 0), Rgb::new(128, 128, 128));
        // HLS and RGB color specs must agree on the primaries.
        assert_eq!(hls_to_rgb(120, 50, 100), Rgb::new(percent(100), 0, 0));
    }

    #[test]
    fn oversized_repeat_is_rejected_before_allocating() {
        let data = format!("!{}@", MAX_IMAGE_DIM + 1);

        assert_eq!(rasterize(&cmd(data.as_bytes())), Err(KittyError::TooBig));
    }

    #[test]
    fn repeated_overdraw_without_growth_is_rejected_by_work_budget() {
        // 10,000 × 6 image, repainted 8,000 times via `$` carriage returns:
        // ~64 KiB of data, an unchanged image size, and 480M pixel writes.
        let mut data = format!("\"1;1;{MAX_IMAGE_DIM};6");
        for _ in 0..8_000 {
            data.push_str(&format!("!{MAX_IMAGE_DIM}~$"));
        }
        // The rejection is driven by the write count, not the data or image
        // size, so a small budget exercises the same path fast enough for a
        // debug build; the production constant is checked separately.
        let budget = u64::from(MAX_IMAGE_DIM) * 6 * 10;

        assert_eq!(
            rasterize_with_budget(&cmd(data.as_bytes()), BG, budget),
            Err(KittyError::TooBig)
        );
    }

    #[test]
    fn work_budget_admits_a_fully_painted_maximum_image() {
        let max_pixels = (TOTAL_BYTES_LIMIT / 4) as u64;
        assert!(MAX_SIXEL_PIXEL_WRITES >= max_pixels);
        // …and is still a finite multiple of it (no unbounded work).
        assert!(MAX_SIXEL_PIXEL_WRITES <= 4 * max_pixels);
    }

    #[test]
    fn moderate_overdraw_under_budget_still_rasterizes() {
        // Paint a 4-wide band red, return, then overpaint it green.
        let image = rasterize(&cmd(b"#1;2;100;0;0#2;2;0;100;0#1!4~$#2!4~")).unwrap();

        assert_eq!((image.width, image.height), (4, 6));
        for px in image.rgba.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    }

    #[test]
    fn oversized_raster_attributes_are_rejected() {
        let data = format!("\"1;1;{};1?", MAX_IMAGE_DIM + 1);

        assert_eq!(rasterize(&cmd(data.as_bytes())), Err(KittyError::TooBig));
    }
}
