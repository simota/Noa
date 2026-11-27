//! Packaged images: decoding, GPU upload and the image component's widgets.
//!
//! Bytes arrive already verified by `noa_widget` (format by magic bytes, size,
//! header dimensions and content digest). Decoding re-checks the decoded size
//! against the header, and anything that fails to decode is shown as its alt
//! text in a box of the same size, so the layout never depends on it.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::rc::Rc;

use noa_gpui_core::style::{AlignItems, JustifyContent, Size as LayoutSize, length};
use noa_gpui_core::widgets::{Container, ImageScaling, Label, image_rect};
use noa_gpui_core::{PaintCx, Role, Style, UiTree, Widget, WidgetId, accesskit};
use noa_gpui_scene::{ImageId, Rect, Size};
use noa_gpui_text::TextAlign;
use noa_widget::assets::MAX_ASSET_DIMENSION;
use noa_widget::{Asset, ImageFormat};
use serde_json::Value;

use crate::render::{Node, Shape};

/// Straight-alpha RGBA8 rows, top to bottom: what `create_image` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub fn decode(asset: &Asset) -> Result<Rgba, String> {
    let image = match asset.format {
        ImageFormat::Png => decode_png(&asset.bytes),
        ImageFormat::Jpeg => decode_jpeg(&asset.bytes),
        ImageFormat::Webp => decode_webp(&asset.bytes),
    }
    .map_err(|error| format!("asset {:?}: {error}", asset.id))?;
    if (image.width, image.height) != (asset.width, asset.height)
        || image.pixels.len() != image.width as usize * image.height as usize * 4
    {
        return Err(format!(
            "asset {:?} decoded to a size its header did not declare",
            asset.id
        ));
    }
    Ok(image)
}

fn checked(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_ASSET_DIMENSION || height > MAX_ASSET_DIMENSION {
        return Err(format!("{width}x{height} is outside the dimension limit"));
    }
    Ok(())
}

fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    checked(width, height)?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG output size overflows")?;
    let mut buffer = vec![0; size];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|error| error.to_string())?;
    buffer.truncate(frame.buffer_size());
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    if frame.bit_depth != png::BitDepth::Eight || frame.line_size != width as usize * channels {
        return Err("unexpected PNG output layout".into());
    }
    Ok(Rgba {
        width,
        height,
        pixels: to_rgba(&buffer, channels),
    })
}

fn decode_jpeg(bytes: &[u8]) -> Result<Rgba, String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_ASSET_DIMENSION as usize)
        .set_max_height(MAX_ASSET_DIMENSION as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = decoder.decode().map_err(|error| format!("{error:?}"))?;
    let info = decoder.info().ok_or("JPEG header is missing")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    checked(width, height)?;
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

fn decode_webp(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder =
        image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    if decoder.is_animated() {
        return Err("animated WebP is not supported".into());
    }
    let (width, height) = decoder.dimensions();
    checked(width, height)?;
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let mut buffer = vec![0; width as usize * height as usize * channels];
    decoder
        .read_image(&mut buffer)
        .map_err(|error| error.to_string())?;
    Ok(Rgba {
        width,
        height,
        pixels: to_rgba(&buffer, channels),
    })
}

fn to_rgba(buffer: &[u8], channels: usize) -> Vec<u8> {
    if channels == 4 {
        return buffer.to_vec();
    }
    let mut out = Vec::with_capacity(buffer.len() / channels * 4);
    for pixel in buffer.chunks_exact(channels) {
        match channels {
            1 => out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            2 => out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            _ => out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
        }
    }
    out
}

/// Smallest side a circle is baked at. The mask's edge is only as smooth as
/// the texture it is in, so small pictures are enlarged first (the largest
/// preset box, `lg`, is 384 device pixels at 2x).
const CIRCLE_MIN_SIDE: u32 = 512;

/// The toolkit has no rounded clip for images, so a circle is baked into the
/// pixels: the centered square of the picture, with an antialiased circular
/// alpha mask. The aspect ratio is kept (it is a crop, never a stretch).
pub fn circle(image: &Rgba) -> Rgba {
    let crop = image.width.min(image.height);
    let (left, top) = ((image.width - crop) / 2, (image.height - crop) / 2);
    let side = crop.max(CIRCLE_MIN_SIDE);
    let scale = crop as f32 / side as f32;
    let radius = side as f32 / 2.0;
    let texel = |x: u32, y: u32, channel: usize| {
        f32::from(image.pixels[(((top + y) * image.width + left + x) * 4) as usize + channel])
    };
    let mut pixels = Vec::with_capacity(side as usize * side as usize * 4);
    for y in 0..side {
        // Bilinear sample of the crop, clamped to its edge.
        let sy = ((y as f32 + 0.5) * scale - 0.5).clamp(0.0, (crop - 1) as f32);
        let (y0, fy) = (sy.floor() as u32, sy.fract());
        let y1 = (y0 + 1).min(crop - 1);
        for x in 0..side {
            let sx = ((x as f32 + 0.5) * scale - 0.5).clamp(0.0, (crop - 1) as f32);
            let (x0, fx) = (sx.floor() as u32, sx.fract());
            let x1 = (x0 + 1).min(crop - 1);
            let mut pixel = [0u8; 4];
            for (channel, value) in pixel.iter_mut().enumerate() {
                let top = texel(x0, y0, channel) * (1.0 - fx) + texel(x1, y0, channel) * fx;
                let bottom = texel(x0, y1, channel) * (1.0 - fx) + texel(x1, y1, channel) * fx;
                *value = (top * (1.0 - fy) + bottom * fy).round() as u8;
            }
            let dx = x as f32 + 0.5 - radius;
            let dy = y as f32 + 0.5 - radius;
            let coverage = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
            pixels.extend_from_slice(&pixel);
        }
    }
    Rgba {
        width: side,
        height: side,
        pixels,
    }
}

/// Where a decoded picture is drawn from. The id is filled in when the
/// screen's images are uploaded; until then the box paints nothing.
#[derive(Debug, Clone)]
pub struct Picture {
    slot: Rc<Cell<Option<ImageId>>>,
    natural: (u32, u32),
}

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.slot, &other.slot) && self.natural == other.natural
    }
}

struct Variant {
    rgba: Option<Rgba>,
    picture: Picture,
}

/// Every decoded asset of one screen, in both shapes the view can use.
#[derive(Default)]
pub struct Images {
    variants: BTreeMap<(String, Shape), Variant>,
    failures: Vec<String>,
}

impl Images {
    /// Decode every asset up front: a data update can point an image at any
    /// declared asset, and images can only be uploaded while the screen is
    /// set up. Circle variants exist only if the view has a circle image.
    pub fn new(assets: &[Asset], view: &Value) -> Self {
        let circles = uses_circle(view);
        let mut images = Self::default();
        for asset in assets {
            let decoded = match decode(asset) {
                Ok(decoded) => decoded,
                Err(error) => {
                    images.failures.push(error);
                    continue;
                }
            };
            if circles {
                images.insert(&asset.id, Shape::Circle, circle(&decoded));
            }
            images.insert(&asset.id, Shape::Square, decoded);
        }
        images
    }

    fn insert(&mut self, id: &str, shape: Shape, rgba: Rgba) {
        let picture = Picture {
            slot: Rc::new(Cell::new(None)),
            natural: (rgba.width, rgba.height),
        };
        self.variants.insert(
            (id.to_owned(), shape),
            Variant {
                rgba: Some(rgba),
                picture,
            },
        );
    }

    /// Assets that could not be decoded; their images show alt text.
    pub fn failures(&self) -> &[String] {
        &self.failures
    }

    /// Upload each decoded picture once and release its pixels.
    pub fn upload(&mut self, create: &mut dyn FnMut(u32, u32, &[u8]) -> ImageId) {
        for variant in self.variants.values_mut() {
            if let Some(rgba) = variant.rgba.take() {
                variant
                    .picture
                    .slot
                    .set(Some(create(rgba.width, rgba.height, &rgba.pixels)));
            }
        }
    }

    /// Give each resolved image node its decoded picture, if there is one.
    pub fn attach(&self, node: &mut Node) {
        match node {
            Node::Column { children, .. }
            | Node::Row { children, .. }
            | Node::Section { children, .. } => {
                for child in children {
                    self.attach(child);
                }
            }
            Node::Image {
                asset,
                shape,
                picture,
                ..
            } => {
                *picture = self
                    .variants
                    .get(&(asset.clone(), *shape))
                    .map(|variant| variant.picture.clone());
            }
            _ => {}
        }
    }
}

fn uses_circle(view: &Value) -> bool {
    (view["type"] == "image" && view["shape"] == "circle")
        || view
            .get("children")
            .and_then(Value::as_array)
            .is_some_and(|children| children.iter().any(uses_circle))
}

/// Insert one image box: the picture when it decoded, otherwise its alt text
/// in a box of exactly the same size.
pub fn insert(
    tree: &mut UiTree,
    parent: WidgetId,
    alt: &str,
    size: (f32, f32),
    shape: Shape,
    picture: Option<&Picture>,
) -> WidgetId {
    match picture {
        Some(picture) => tree.insert(
            Some(parent),
            PictureBox {
                picture: picture.clone(),
                size: Size::new(size.0, size.1),
                alt: alt.to_owned(),
            },
        ),
        None => {
            let radius = match shape {
                Shape::Circle => size.0.min(size.1) / 2.0,
                Shape::Square => 6.0,
            };
            let placeholder = tree.insert(
                Some(parent),
                Container::new(Style {
                    size: LayoutSize {
                        width: length(size.0),
                        height: length(size.1),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                })
                .align_items(AlignItems::CENTER)
                .justify_content(JustifyContent::CENTER)
                .background(Role::SurfaceAlt)
                .border(1.0, Role::Separator)
                .radius(radius)
                .padding(crate::appearance::space(6.0)),
            );
            tree.insert(
                Some(placeholder),
                Label::new(alt)
                    .font_size(crate::appearance::font(11.0))
                    .color(Role::Muted)
                    .align(TextAlign::Center)
                    .max_lines(4),
            );
            placeholder
        }
    }
}

/// A fixed-size box that fits its picture inside, keeping the aspect ratio.
struct PictureBox {
    picture: Picture,
    size: Size,
    alt: String,
}

impl Widget for PictureBox {
    fn style(&self) -> Style {
        Style {
            size: LayoutSize {
                width: length(self.size.width),
                height: length(self.size.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let Some(image) = self.picture.slot.get() else {
            return;
        };
        let bounds = cx.bounds();
        let natural = Size::new(self.picture.natural.0 as f32, self.picture.natural.1 as f32);
        let area = Rect::new(bounds.x, bounds.y, bounds.width, bounds.height);
        cx.image(image_rect(ImageScaling::Fit, natural, area), image);
    }

    fn accessibility_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Image)
    }

    fn accessibility(&self, node: &mut accesskit::Node) {
        node.set_label(self.alt.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGO: &[u8] = include_bytes!("../../../.noa-widgets/image-gallery/assets/logo.png");
    const LANDSCAPE: &[u8] =
        include_bytes!("../../../.noa-widgets/image-gallery/assets/landscape.jpg");
    const PATTERN: &[u8] =
        include_bytes!("../../../.noa-widgets/image-gallery/assets/pattern.webp");

    fn asset(id: &str, bytes: &[u8]) -> Asset {
        Asset::verify(id, bytes.to_vec(), None).unwrap()
    }

    #[test]
    fn png_jpeg_and_webp_decode_to_their_declared_size() {
        for (bytes, size) in [
            (LOGO, (96, 96)),
            (LANDSCAPE, (160, 96)),
            (PATTERN, (96, 96)),
        ] {
            let image = decode(&asset("picture", bytes)).unwrap();
            assert_eq!((image.width, image.height), size);
            assert_eq!(image.pixels.len(), (size.0 * size.1 * 4) as usize);
            assert!(
                image
                    .pixels
                    .chunks(4)
                    .any(|p| p[3] == 255 && p[..3] != [0, 0, 0])
            );
        }
    }

    #[test]
    fn a_corrupt_body_with_a_valid_header_is_a_decode_failure_not_a_panic() {
        let mut bytes = LOGO.to_vec();
        let len = bytes.len();
        bytes[40..len - 12].fill(0x5a);
        let images = Images::new(&[asset("broken", &bytes)], &serde_json::json!({}));
        assert_eq!(images.failures().len(), 1);
        assert!(images.variants.is_empty());
    }

    #[test]
    fn circles_crop_the_center_square_and_clear_the_corners() {
        let landscape = decode(&asset("landscape", LANDSCAPE)).unwrap();
        let round = circle(&landscape);
        let side = CIRCLE_MIN_SIDE;
        assert_eq!((round.width, round.height), (side, side));
        let pixel = |x: u32, y: u32| {
            let at = ((y * side + x) * 4) as usize;
            &round.pixels[at..at + 4]
        };
        assert_eq!(pixel(0, 0)[3], 0);
        assert_eq!(pixel(side - 1, side - 1)[3], 0);
        assert_eq!(pixel(side / 2, side / 2)[3], 255);
        // The crop keeps the center column of the original: the left and
        // right 32 columns of the 160x96 landscape are cut, not squeezed.
        let original = |x: u32, y: u32| {
            let at = ((y * 160 + x) * 4) as usize;
            &landscape.pixels[at..at + 3]
        };
        for (x, y) in [(48u32, 10u32), (60, 60), (30, 80)] {
            let scaled = pixel(x * side / 96 + 2, y * side / 96 + 2);
            let source = original(x + 32, y);
            assert!(
                (0..3).all(|i| scaled[i].abs_diff(source[i]) <= 24),
                "{x},{y}: {scaled:?} vs {source:?}"
            );
        }
    }
}
