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

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    background: [u8; 4],
}

impl Canvas {
    fn new(background: [u8; 4]) -> Self {
        Self {
            width: 0,
            height: 0,
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
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or(KittyError::TooBig)?;
        if bytes > TOTAL_BYTES_LIMIT {
            return Err(KittyError::TooBig);
        }

        let old_width = self.width;
        let old_height = self.height;
        let old = std::mem::take(&mut self.pixels);
        let mut new_pixels = vec![0u8; bytes];
        for px in new_pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&self.background);
        }
        for y in 0..old_height as usize {
            let old_start = y * old_width as usize * 4;
            let old_end = old_start + old_width as usize * 4;
            let new_start = y * width as usize * 4;
            new_pixels[new_start..new_start + old_width as usize * 4]
                .copy_from_slice(&old[old_start..old_end]);
        }

        self.width = width;
        self.height = height;
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
        let i = ((y as usize * self.width as usize) + x as usize) * 4;
        self.pixels[i..i + 4].copy_from_slice(&[color.r, color.g, color.b, 0xff]);
        Ok(())
    }

    fn finish(mut self, min_width: u32, min_height: u32) -> Result<SixelRaster, KittyError> {
        self.ensure_size(self.width.max(min_width), self.height.max(min_height))?;
        if self.width == 0 || self.height == 0 {
            return Err(KittyError::Invalid);
        }
        Ok(SixelRaster {
            width: self.width,
            height: self.height,
            rgba: self.pixels,
        })
    }
}

/// Rasterize a parsed SIXEL command into straight RGBA8.
pub fn rasterize(cmd: &SixelGraphicsCommand) -> Result<SixelRaster, KittyError> {
    let mut palette = xterm_palette();
    let background = if cmd.background == 2 {
        let c = palette[0];
        [c.r, c.g, c.b, 0xff]
    } else {
        [0, 0, 0, 0]
    };
    let mut canvas = Canvas::new(background);
    let mut current_color = 0usize;
    let mut x = 0u32;
    let mut y = 0u32;
    let mut declared_width = 0u32;
    let mut declared_height = 0u32;

    let mut i = 0usize;
    while i < cmd.data.len() {
        let b = cmd.data[i] & 0x7f;
        match b {
            b'?'..=b'~' => {
                draw_sixel(&mut canvas, x, y, b - b'?', 1, palette[current_color])?;
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
                    draw_sixel(&mut canvas, x, y, ch - b'?', count, palette[current_color])?;
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
) -> Result<(), KittyError> {
    canvas.advance_blank(x, y, count)?;
    if value == 0 {
        return Ok(());
    }
    for dx in 0..count {
        for bit in 0..6u32 {
            if value & (1 << bit) != 0 {
                canvas.set_pixel(x + dx, y + bit, color)?;
            }
        }
    }
    Ok(())
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

fn hls_to_rgb(hue: u32, lightness: u32, saturation: u32) -> Rgb {
    let h = (hue % 360) as f64 / 360.0;
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

    fn cmd(data: &[u8]) -> SixelGraphicsCommand {
        SixelGraphicsCommand {
            aspect_ratio: 0,
            background: 0,
            horizontal_grid_size: 0,
            data: data.to_vec(),
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
    fn raster_attributes_extend_transparent_canvas() {
        let image = rasterize(&cmd(br#""1;1;4;7#1;2;100;0;0@"#)).unwrap();

        assert_eq!((image.width, image.height), (4, 7));
        assert_eq!(&image.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&image.rgba[(4 * 6 + 3) * 4..(4 * 6 + 4) * 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn oversized_repeat_is_rejected_before_allocating() {
        let data = format!("!{}@", MAX_IMAGE_DIM + 1);

        assert_eq!(rasterize(&cmd(data.as_bytes())), Err(KittyError::TooBig));
    }

    #[test]
    fn oversized_raster_attributes_are_rejected() {
        let data = format!("\"1;1;{};1?", MAX_IMAGE_DIM + 1);

        assert_eq!(rasterize(&cmd(data.as_bytes())), Err(KittyError::TooBig));
    }
}
