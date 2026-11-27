//! Bounded decoding of stored evidence for native thumbnails and inspection.
//! Callers run this outside the live window's UI thread. The original stored
//! bytes, size, digest, and detected format are checked before decoding.

use std::io::Cursor;
use std::path::Path;

use crate::model::types::{Evidence, Media};

pub const MAX_DIMENSION: u32 = 8192;
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;
pub const THUMB_WIDTH: u32 = 96;
pub const THUMB_HEIGHT: u32 = 64;
pub const MAX_QUADS: usize = (THUMB_WIDTH * THUMB_HEIGHT) as usize;
const MEMORY_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub original: (u32, u32),
    pub size: (u32, u32),
    pub runs: Vec<Run>,
}

pub fn supported(media: Media) -> bool {
    matches!(media, Media::Png | Media::Jpeg | Media::Webp)
}

pub fn read(base: &Path, reply: &Path, evidence: &Evidence) -> Result<Rgba, String> {
    let (bytes, format) = crate::preview::checked_image(base, reply, evidence, 0)?;
    decode(&bytes, format)
}

fn checked(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(format!(
            "画像の寸法 {width}×{height} は上限 8192 px を超えています"
        ));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("画像の画素数が上限 16 メガピクセルを超えています".into());
    }
    Ok(())
}

pub fn decode(bytes: &[u8], format: Media) -> Result<Rgba, String> {
    if bytes.len() as u64 > crate::model::types::MAX_IMAGE_BYTES {
        return Err("画像は 15 MiB までです".into());
    }
    let image = match format {
        Media::Png => png(bytes),
        Media::Jpeg => jpeg(bytes),
        Media::Webp => webp(bytes),
        _ => Err("この形式は保存済みコピーを外部ビューアで開いてください".into()),
    }?;
    checked(image.width, image.height)?;
    if image.pixels.len() != image.width as usize * image.height as usize * 4 {
        return Err("画像のデコード結果がヘッダーの寸法と一致しません".into());
    }
    Ok(image)
}

fn png(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: MEMORY_LIMIT,
    });
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    checked(width, height)?;
    if reader.info().animation_control.is_some() {
        return Err("アニメーション PNG は外部ビューアで開いてください".into());
    }
    let size = reader
        .output_buffer_size()
        .ok_or("PNG のサイズが不正です")?;
    if size > MAX_PIXELS as usize * 4 {
        return Err("PNG の展開サイズが上限を超えています".into());
    }
    let mut pixels = vec![0; size];
    let frame = reader
        .next_frame(&mut pixels)
        .map_err(|error| error.to_string())?;
    pixels.truncate(frame.buffer_size());
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("PNG パレットを展開できません".into()),
    };
    if frame.bit_depth != png::BitDepth::Eight || frame.line_size != width as usize * channels {
        return Err("PNG の画素配置が不正です".into());
    }
    Ok(Rgba {
        width,
        height,
        pixels: rgba(pixels, channels),
    })
}

fn jpeg(bytes: &[u8]) -> Result<Rgba, String> {
    use zune_jpeg::zune_core::{
        bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions,
    };
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|error| format!("{error:?}"))?;
    let info = decoder.info().ok_or("JPEG の寸法がありません")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    checked(width, height)?;
    let pixels = decoder.decode().map_err(|error| format!("{error:?}"))?;
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

fn webp(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder =
        image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    decoder.set_memory_limit(MEMORY_LIMIT);
    if decoder.is_animated() {
        return Err("アニメーション WebP は外部ビューアで開いてください".into());
    }
    let (width, height) = decoder.dimensions();
    checked(width, height)?;
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let mut pixels = vec![0; width as usize * height as usize * channels];
    decoder
        .read_image(&mut pixels)
        .map_err(|error| error.to_string())?;
    Ok(Rgba {
        width,
        height,
        pixels: rgba(pixels, channels),
    })
}

fn rgba(pixels: Vec<u8>, channels: usize) -> Vec<u8> {
    if channels == 4 {
        return pixels;
    }
    let mut out = Vec::with_capacity(pixels.len() / channels * 4);
    for pixel in pixels.chunks_exact(channels) {
        match channels {
            1 => out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            2 => out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            _ => out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
        }
    }
    out
}

/// Samples the original without upscaling and joins adjacent identical pixels.
/// Even a noisy image costs no more than MAX_QUADS drawing commands.
pub fn sample(image: &Rgba) -> Sample {
    let scale = (THUMB_WIDTH as f64 / image.width as f64)
        .min(THUMB_HEIGHT as f64 / image.height as f64)
        .min(1.0);
    let width = (image.width as f64 * scale).round().max(1.0) as u32;
    let height = (image.height as f64 * scale).round().max(1.0) as u32;
    let mut runs: Vec<Run> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let sx = (u64::from(x) * u64::from(image.width) / u64::from(width)) as u32;
            let sy = (u64::from(y) * u64::from(image.height) / u64::from(height)) as u32;
            let at = (sy as usize * image.width as usize + sx as usize) * 4;
            let color = image.pixels[at..at + 4].try_into().expect("RGBA pixel");
            if let Some(last) = runs.last_mut()
                && last.y == y
                && last.color == color
            {
                last.width += 1;
            } else {
                runs.push(Run {
                    x,
                    y,
                    width: 1,
                    color,
                });
            }
        }
    }
    debug_assert!(runs.len() <= MAX_QUADS);
    Sample {
        original: (image.width, image.height),
        size: (width, height),
        runs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::digest_bytes;
    use crate::model::types::{EvidenceBody, EvidenceKey, Stored};

    #[test]
    fn supported_formats_decode_and_samples_bound_the_paint_work() {
        let fixtures: [(&[u8], Media); 3] = [
            (
                include_bytes!("../../../.noa-widgets/image-gallery/assets/logo.png"),
                Media::Png,
            ),
            (
                include_bytes!("../../../.noa-widgets/image-gallery/assets/landscape.jpg"),
                Media::Jpeg,
            ),
            (
                include_bytes!("../../../.noa-widgets/image-gallery/assets/pattern.webp"),
                Media::Webp,
            ),
        ];
        for (bytes, format) in fixtures {
            let image = decode(bytes, format).unwrap();
            let thumbnail = sample(&image);
            assert_eq!(thumbnail.original, (image.width, image.height));
            assert!(thumbnail.size.0 <= THUMB_WIDTH && thumbnail.size.1 <= THUMB_HEIGHT);
            assert!(thumbnail.runs.len() <= MAX_QUADS);
        }
        let noisy = Rgba {
            width: 200,
            height: 200,
            pixels: (0..200 * 200)
                .flat_map(|i| [(i % 251) as u8, (i / 251) as u8, 0, 255])
                .collect(),
        };
        assert!(sample(&noisy).runs.len() <= MAX_QUADS);
        assert!(checked(8193, 1).is_err());
        assert!(checked(8192, 8192).is_err());
        assert!(checked(0, 1).is_err());
        assert!(
            decode(
                &vec![0; crate::model::types::MAX_IMAGE_BYTES as usize + 1],
                Media::Png
            )
            .is_err()
        );
        assert!(decode(b"GIF89a", Media::Gif).is_err());
    }

    #[test]
    fn stored_image_must_keep_its_hash_size_and_link_free_path() {
        let root = std::env::temp_dir().join(format!("noa-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let bytes = include_bytes!("../../../.noa-widgets/image-gallery/assets/logo.png");
        let path = root.join("image.png");
        std::fs::write(&path, bytes).unwrap();
        let mut evidence = Evidence {
            key: EvidenceKey::new("user/image"),
            body: EvidenceBody::File {
                path: "image.png".into(),
                caption: None,
                media: Some(Media::Png),
                marks: vec![],
                shows: vec![],
            },
            stored: Some(Stored {
                path: "image.png".into(),
                bytes: bytes.len() as u64,
                digest: digest_bytes(bytes),
            }),
            rerun: None,
            attached: None,
        };
        assert!(read(&root, &root, &evidence).is_ok());
        evidence.stored.as_mut().unwrap().digest ^= 1;
        assert!(read(&root, &root, &evidence).is_err());
        evidence.stored.as_mut().unwrap().digest ^= 1;
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        assert!(read(&root, &root, &evidence).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
