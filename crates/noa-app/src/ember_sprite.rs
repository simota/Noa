//! Analytic, straight-alpha sticker raster. No font or image asset is involved.

use crate::idle_ember::Pose;

const SS: u32 = 2;
const _: () = assert!(SS <= 4);
pub(crate) const MAX_PIXELS: u32 = 576;

fn ghost_distance(x: f32, y: f32) -> f32 {
    let radius = 0.31;
    if y < 0.56 {
        // Distance in box-height units keeps the sticker outline equally
        // thick along both axes of the 0.8-aspect box.
        let dx = (x - 0.5) * 0.8;
        let dy = y - 0.56;
        let rx = radius * 0.8;
        let k0 = ((dx / rx).powi(2) + (dy / radius).powi(2)).sqrt();
        let k1 = ((dx / (rx * rx)).powi(2) + (dy / (radius * radius)).powi(2)).sqrt();
        k0 * (k0 - 1.0) / k1
    } else {
        let left = 0.5 - radius;
        let local = ((x - left) / (2.0 * radius / 3.0)).rem_euclid(1.0) * 2.0 - 1.0;
        let hem = 0.87 + 0.045 * (1.0 - local * local).max(0.0).sqrt();
        (((x - 0.5).abs() - radius) * 0.8).max(y - hem)
    }
}

fn ellipse(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
}

fn sample(x: f32, y: f32, pose: Pose, outline: f32, hairline: f32) -> [f32; 4] {
    // Inverse transform around the hem; translation and opacity belong to the card.
    let (sin, cos) = pose.tilt.sin_cos();
    let dx = (x - 0.5) * 0.8;
    let dy = y - 0.915;
    let bx = (cos * dx + sin * dy) / (pose.sx * 0.8) + 0.5;
    let by = (-sin * dx + cos * dy) / pose.sy + 0.915;
    let d = ghost_distance(bx, by);
    let mut color = if d <= 0.0 {
        let shade = ((by - 0.6) / 0.31).clamp(0.0, 1.0);
        [
            247.0 - 27.0 * shade,
            247.0 - 27.0 * shade,
            242.0 - 28.0 * shade,
            255.0,
        ]
    } else if d <= outline {
        [255.0; 4]
    } else if d <= outline + hairline {
        [0.0, 0.0, 0.0, 255.0 * 0.45]
    } else {
        [0.0; 4]
    };
    if d <= 0.0 {
        for ex in [0.39, 0.61] {
            let ex = ex + pose.eye_dx * 0.055;
            let open = pose.eye_open.clamp(0.0, 1.0);
            if open > 0.0 && ellipse(bx, by, ex, 0.57, 0.037, 0.055 * open) {
                color = [26.0, 26.0, 26.0, 255.0];
                if open > 0.2
                    && ellipse(bx, by, ex - 0.011, 0.57 - 0.02 * open, 0.011, 0.0165 * open)
                {
                    color = [255.0; 4];
                }
            }
        }
        if ellipse(bx, by, 0.5, 0.69, 0.024, 0.018) {
            color = [26.0, 26.0, 26.0, 255.0];
        }
        if [0.31, 0.69]
            .into_iter()
            .any(|cx| ellipse(bx, by, cx, 0.66, 0.044, 0.023))
        {
            for (channel, blush) in color[..3].iter_mut().zip([244.0, 166.0, 166.0]) {
                *channel = *channel * 0.45 + blush * 0.55;
            }
        }
    }
    if pose.q_scale > 0.0 && pose.q_alpha > 0.0 {
        // Counter-rotate the emote relative to the tilted body: it stays upright.
        let qx = (x - 0.77) * 0.8 / pose.q_scale;
        let qy = (y - 0.14) / pose.q_scale;
        let arc = ((qx * qx + qy * qy).sqrt() - 0.055).abs() - 0.014;
        let hook = (qx.abs() - 0.014).max((qy - 0.072).abs() - 0.025);
        let dot = (qx * qx + (qy - 0.135).powi(2)).sqrt() - 0.015;
        let arc = if qy <= 0.0 || qx >= 0.0 {
            arc
        } else {
            f32::INFINITY
        };
        let qd = arc.min(hook).min(dot);
        if qd <= outline + hairline {
            let mut src = if qd <= 0.0 {
                [43.0, 43.0, 43.0, 255.0]
            } else if qd <= outline {
                [255.0; 4]
            } else {
                [0.0, 0.0, 0.0, 255.0 * 0.45]
            };
            src[3] *= pose.q_alpha;
            let src_alpha = src[3] / 255.0;
            let dst_alpha = color[3] / 255.0;
            let alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
            for i in 0..3 {
                color[i] = (src[i] * src_alpha + color[i] * dst_alpha * (1.0 - src_alpha)) / alpha;
            }
            color[3] = alpha * 255.0;
        }
    }
    color
}

pub(crate) fn raster(out: &mut Vec<u8>, width: u32, height: u32, cell_h: f32, pose: Pose) {
    let width = width.clamp(1, MAX_PIXELS);
    let height = height.clamp(1, MAX_PIXELS);
    out.resize((width * height * 4) as usize, 0);
    let outline = 0.12 * cell_h / height as f32;
    let hairline = 1.0 / height as f32;
    for py in 0..height {
        for px in 0..width {
            let mut sum = [0.0; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / width as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / height as f32;
                    let c = sample(x, y, pose, outline, hairline);
                    for i in 0..3 {
                        sum[i] += c[i] * c[3] / 255.0;
                    }
                    sum[3] += c[3];
                }
            }
            let offset = ((py * width + px) * 4) as usize;
            for i in 0..3 {
                out[offset + i] = if sum[3] > 0.0 {
                    (sum[i] * 255.0 / sum[3]).round() as u8
                } else {
                    0
                };
            }
            out[offset + 3] = (sum[3] / (SS * SS) as f32).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idle_ember::{T_END, pose};

    #[test]
    fn sticker_has_body_eyes_outline_and_clear_corners() {
        let p = pose(0.4);
        assert_eq!(
            sample(0.5, 0.5, p, 0.04, 0.01),
            [247.0, 247.0, 242.0, 255.0]
        );
        assert_eq!(sample(0.39, 0.59, p, 0.04, 0.01), [26.0, 26.0, 26.0, 255.0]);
        assert_eq!(sample(0.17, 0.7, p, 0.04, 0.01), [255.0; 4]);
        assert!(sample(0.145, 0.7, p, 0.04, 0.01)[3] > 0.0);
        assert_eq!(sample(0.0, 0.0, p, 0.04, 0.01)[3], 0.0);
    }

    #[test]
    fn closed_eyes_have_no_dark_pixels_or_catchlights() {
        let p = Pose {
            eye_open: 0.0,
            ..pose(0.4)
        };
        let mut pixels = Vec::new();
        raster(&mut pixels, 160, 200, 20.0, p);
        for ex in [0.39_f32, 0.61] {
            for y in 104..124 {
                for x in ((ex - 0.03) * 160.0) as usize..((ex + 0.03) * 160.0) as usize {
                    let offset = (y * 160 + x) * 4;
                    assert!(
                        pixels[offset..offset + 3]
                            .iter()
                            .all(|c| (220..255).contains(c))
                    );
                    assert_eq!(pixels[offset + 3], 255);
                }
            }
        }
    }

    #[test]
    fn emote_outline_over_body_preserves_body_alpha() {
        let body = pose(0.4);
        let emote = Pose {
            q_scale: 1.0,
            q_alpha: 0.5,
            ..body
        };
        let before = sample(0.77, 0.36, body, 0.06, 0.02);
        let after = sample(0.77, 0.36, emote, 0.06, 0.02);
        assert_eq!(before[3], 255.0);
        assert!(after[3] >= before[3]);
        assert!(
            after[0] < before[0],
            "the emote hairline must overlap the body"
        );
    }

    #[test]
    fn emote_dot_is_round_in_box_height_units() {
        let p = Pose {
            q_scale: 1.0,
            q_alpha: 1.0,
            ..pose(0.4)
        };
        assert_eq!(
            sample(0.77 + 0.014 / 0.8, 0.275, p, 0.0, 0.0),
            [43.0, 43.0, 43.0, 255.0]
        );
        assert_eq!(
            sample(0.77, 0.275 + 0.014, p, 0.0, 0.0),
            [43.0, 43.0, 43.0, 255.0]
        );
    }

    #[test]
    fn raster_reuses_buffer_and_emote_is_analytic() {
        let mut buffer = Vec::new();
        raster(&mut buffer, 58, 72, 24.0, pose(T_END));
        let ptr = buffer.as_ptr();
        let resting = buffer.clone();
        raster(&mut buffer, 58, 72, 24.0, pose(0.4));
        assert_eq!(ptr, buffer.as_ptr());
        assert_ne!(resting, buffer);
        assert!(resting.chunks_exact(4).any(|p| p == [43, 43, 43, 255]));
        assert_eq!(&buffer[..4], &[0; 4]);
    }

    #[test]
    fn raster_caps_dimensions_and_accepts_zero_size() {
        let mut buffer = Vec::new();
        raster(&mut buffer, 0, 0, 1.0, pose(0.4));
        assert_eq!(buffer.len(), 4);
        raster(&mut buffer, MAX_PIXELS + 1, 1, 1.0, pose(0.4));
        assert_eq!(buffer.len(), (MAX_PIXELS * 4) as usize);
    }
}

/// Black-box acceptance tests (AC-15/AC-19); expected colours come from the
/// loop-contract tokens: body #F7F7F2, eyes #1A1A1A, white outline + dark hairline.
#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use crate::idle_ember::{T_END, pose};

    const W: u32 = 48;
    const H: u32 = 60;

    fn rest_sprite() -> Vec<u8> {
        let mut out = Vec::new();
        raster(&mut out, W, H, 20.0, pose(T_END));
        assert_eq!(out.len(), (W * H * 4) as usize);
        out
    }

    fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
        let o = ((y * W + x) * 4) as usize;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
    }

    /// Body-family pixel: light, neutral red/green, warm (blue below red).
    /// Distinguishes body #F7F7F2 / shade #DCDCD6 from white outline, grey hairline,
    /// dark eyes and pink blush.
    fn is_body(p: [u8; 4]) -> bool {
        p[3] > 0
            && p[0] >= 0xC0
            && p[0].abs_diff(p[1]) <= 3
            && u16::from(p[2]) + 3 <= u16::from(p[0])
    }

    #[test]
    fn ac15_corners_are_transparent() {
        let buf = rest_sprite();
        for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
            assert_eq!(px(&buf, x, y)[3], 0, "corner ({x},{y}) alpha");
        }
    }

    #[test]
    fn ac15_has_opaque_body_colour() {
        let buf = rest_sprite();
        let target = [0xF7u8, 0xF7, 0xF2];
        let found = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .any(|(x, y)| {
                let p = px(&buf, x, y);
                p[3] == 255 && (0..3).all(|i| p[i].abs_diff(target[i]) <= 3)
            });
        assert!(found, "no opaque pixel within ±3 of #F7F7F2");
    }

    #[test]
    fn ac15_has_opaque_dark_eye_pixel() {
        let buf = rest_sprite();
        let found = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .any(|(x, y)| {
                let p = px(&buf, x, y);
                p[3] == 255 && p[..3].iter().all(|&c| c <= 0x30)
            });
        assert!(found, "no opaque dark (<= #303030) pixel for eyes");
    }

    #[test]
    fn ac15_outline_ring_surrounds_body() {
        let buf = rest_sprite();
        let mut rows = 0;
        for y in 0..H {
            let xs: Vec<u32> = (0..W).filter(|&x| is_body(px(&buf, x, y))).collect();
            let (Some(&l), Some(&r)) = (xs.first(), xs.last()) else {
                continue;
            };
            rows += 1;
            assert!(l > 0 && r < W - 1, "body touches the image edge in row {y}");
            assert!(
                px(&buf, l - 1, y)[3] > 0,
                "no outline left of body, row {y}"
            );
            assert!(
                px(&buf, r + 1, y)[3] > 0,
                "no outline right of body, row {y}"
            );
        }
        assert!(rows > 10, "body found in only {rows} rows");
        let mut cols = 0;
        for x in 0..W {
            let ys: Vec<u32> = (0..H).filter(|&y| is_body(px(&buf, x, y))).collect();
            let (Some(&t), Some(&b)) = (ys.first(), ys.last()) else {
                continue;
            };
            cols += 1;
            assert!(
                t > 0 && b < H - 1,
                "body touches the image edge in column {x}"
            );
            assert!(
                px(&buf, x, t - 1)[3] > 0,
                "no outline above body, column {x}"
            );
            assert!(
                px(&buf, x, b + 1)[3] > 0,
                "no outline below body, column {x}"
            );
        }
        assert!(cols > 10, "body found in only {cols} columns");
    }
}
