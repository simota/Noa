//! Ember's menu-bar glyph: the ghost silhouette with two eye cut-outs,
//! rasterized at launch as a template PNG (AppKit tints it for light/dark
//! menu bars, so the body stays "white on dark" without hard-coding color).

/// 18 pt at @2x.
pub const PIXELS: u32 = 36;
const SUPERSAMPLE: u32 = 4;

/// Coverage of the ghost at a point in 0..18 point space.
fn inside(x: f32, y: f32) -> bool {
    let (cx, top, radius) = (9.0, 2.0, 6.5);
    let (left, right, hem) = (cx - radius, cx + radius, 15.5);
    // Eyes: two small vertical capsules, cut out of the body.
    let eye = |ex: f32| {
        let dy = (y - 7.6).abs() - 0.8;
        let d = ((x - ex).powi(2) + dy.max(0.0).powi(2)).sqrt();
        d < 0.95
    };
    if eye(cx - 2.4) || eye(cx + 2.4) {
        return false;
    }
    let head_y = top + radius;
    if y < head_y {
        return (x - cx).powi(2) + (y - head_y).powi(2) <= radius * radius;
    }
    if x < left || x > right {
        return false;
    }
    // Hem: three scallops along the bottom edge.
    let scallop = (right - left) / 3.0;
    let local = ((x - left) % scallop) / scallop - 0.5;
    y <= hem + 1.2 * (1.0 - (2.0 * local).powi(2)).max(0.0).sqrt()
}

/// Straight-alpha black RGBA, `PIXELS`² pixels.
pub fn rgba() -> Vec<u8> {
    let scale = 18.0 / PIXELS as f32;
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    let mut out = Vec::with_capacity((PIXELS * PIXELS * 4) as usize);
    for py in 0..PIXELS {
        for px in 0..PIXELS {
            let mut hits = 0u32;
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let x = (px as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) * scale;
                    hits += inside(x, y) as u32;
                }
            }
            let alpha = (hits as f32 / samples * 255.0).round() as u8;
            out.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    out
}

pub fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, PIXELS, PIXELS);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(&rgba()).expect("png data");
    writer.finish().expect("png finish");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(x: u32, y: u32) -> u8 {
        rgba()[((y * PIXELS + x) * 4 + 3) as usize]
    }

    #[test]
    fn body_is_solid_eyes_and_corners_are_clear() {
        let center = PIXELS / 2;
        assert_eq!(alpha(center, 22), 255, "body below the eyes");
        assert_eq!(alpha(0, 0), 0, "outside the head");
        // Eye centers at (9 ± 2.4, 7.6) pt.
        assert_eq!(alpha(13, 15), 0, "left eye");
        assert_eq!(alpha(22, 15), 0, "right eye");
    }

    #[test]
    fn encodes_as_a_png() {
        let bytes = png();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
