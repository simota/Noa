//! Format-keyed shared GPU resources.
//!
//! Every [`crate::Renderer`] draws with the same three pipelines (cell, inline
//! image, background image); only the render-target format differs between
//! windows. [`SharedPipelines`] bundles one immutable set for a format, and
//! [`PipelineCache`] hands out clones so a new tab's `Renderer` skips shader
//! module + pipeline construction entirely after the first build per format.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::background_image::BackgroundImagePipeline;
use crate::image_layer::ImagePipeline;
use crate::pipeline::CellPipeline;
use noa_font::FontGrid;

/// R8 mask atlas: 1 byte per pixel.
pub(crate) const MASK_BYTES_PER_PX: u32 = 1;
/// RGBA8 color atlas: 4 bytes per pixel.
pub(crate) const COLOR_BYTES_PER_PX: u32 = 4;

/// The immutable pipeline set for one render-target format. Cheap to clone
/// (three `Arc`s); mutable texture resources live in separate caches.
#[derive(Clone)]
pub struct SharedPipelines {
    format: wgpu::TextureFormat,
    pub(crate) cell: Arc<CellPipeline>,
    pub(crate) image: Arc<ImagePipeline>,
    pub(crate) background_image: Arc<BackgroundImagePipeline>,
}

impl SharedPipelines {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            cell: Arc::new(CellPipeline::new(device, format)),
            image: Arc::new(ImagePipeline::new(device, format)),
            background_image: Arc::new(BackgroundImagePipeline::new(device, format)),
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

/// Lazily built [`SharedPipelines`] per format. In practice this holds one
/// entry (every surface on one adapter usually picks the same format), two if
/// e.g. sRGB and non-sRGB surfaces coexist — a `Vec` scan is exactly right.
#[derive(Default)]
pub struct PipelineCache {
    entries: Vec<SharedPipelines>,
}

impl PipelineCache {
    pub fn get(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> SharedPipelines {
        if let Some(entry) = self.entries.iter().find(|entry| entry.format == format) {
            return entry.clone();
        }
        let entry = SharedPipelines::new(device, format);
        self.entries.push(entry.clone());
        entry
    }
}

/// The color (emoji) atlas holds sRGB-encoded bytes as rasterized. The shader
/// passes samples through untinted, so the texture format must match the
/// target's transfer function: on an sRGB target the sampler must decode
/// sRGB->linear (the surface re-encodes on write; a plain `Rgba8Unorm` there
/// double-encodes and washes emoji out), while on a non-sRGB target the bytes
/// pass through verbatim.
pub(crate) fn color_atlas_format(target_is_srgb: bool) -> wgpu::TextureFormat {
    if target_is_srgb {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

fn upload_atlas(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    w: u32,
    h: u32,
    bytes_per_px: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bytes_per_px),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

fn create_atlas_texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

struct AtlasSyncInput<'a> {
    data: &'a [u8],
    size: (u32, u32),
    identity: u64,
    generation: u64,
    format: wgpu::TextureFormat,
    bytes_per_px: u32,
    label: &'static str,
}

struct SharedAtlasTexture {
    texture: Arc<wgpu::Texture>,
    view: Arc<wgpu::TextureView>,
    seen_identity: u64,
    seen_generation: u64,
}

impl SharedAtlasTexture {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: AtlasSyncInput<'_>,
    ) -> SharedAtlasTexture {
        let (w, h) = input.size;
        let texture = create_atlas_texture(device, w, h, input.format, input.label);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        upload_atlas(queue, &texture, input.data, w, h, input.bytes_per_px);
        SharedAtlasTexture {
            texture: Arc::new(texture),
            view: Arc::new(view),
            seen_identity: input.identity,
            seen_generation: input.generation,
        }
    }

    fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: AtlasSyncInput<'_>,
    ) -> bool {
        if input.identity == self.seen_identity
            && input.generation == self.seen_generation
            && input.size == (self.texture.width(), self.texture.height())
        {
            return false;
        }

        let (w, h) = input.size;
        let mut recreated = false;
        if w != self.texture.width() || h != self.texture.height() {
            let texture = create_atlas_texture(device, w, h, input.format, input.label);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.texture = Arc::new(texture);
            self.view = Arc::new(view);
            recreated = true;
        }
        upload_atlas(
            queue,
            self.texture.as_ref(),
            input.data,
            w,
            h,
            input.bytes_per_px,
        );
        self.seen_identity = input.identity;
        self.seen_generation = input.generation;
        recreated
    }
}

struct SharedGlyphAtlasState {
    mask: SharedAtlasTexture,
    color: SharedAtlasTexture,
    /// Monotonic counter for texture/view recreation only. Pixel uploads that
    /// keep the same texture do not require bind group rebuilds.
    texture_generation: u64,
}

/// Cloned view handles plus the texture/view generation they came from.
pub(crate) struct SharedGlyphAtlasViews {
    pub(crate) mask: Arc<wgpu::TextureView>,
    pub(crate) color: Arc<wgpu::TextureView>,
    pub(crate) texture_generation: u64,
}

/// Shared glyph atlas textures for one render-target format.
///
/// The mask atlas format is always `R8Unorm`, but the color atlas format
/// depends on the target's sRGB transfer function, so the cache key mirrors
/// [`PipelineCache`] and stays format-keyed.
#[derive(Clone)]
pub struct SharedGlyphAtlases {
    format: wgpu::TextureFormat,
    state: Arc<Mutex<SharedGlyphAtlasState>>,
}

impl SharedGlyphAtlases {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font: &FontGrid,
    ) -> SharedGlyphAtlases {
        let mask = SharedAtlasTexture::new(
            device,
            queue,
            AtlasSyncInput {
                data: font.mask_atlas_data(),
                size: font.mask_atlas_size(),
                identity: font.atlas_identity(),
                generation: font.mask_atlas_generation(),
                format: wgpu::TextureFormat::R8Unorm,
                bytes_per_px: MASK_BYTES_PER_PX,
                label: "noa-glyph-mask-atlas",
            },
        );
        let color = SharedAtlasTexture::new(
            device,
            queue,
            AtlasSyncInput {
                data: font.color_atlas_data(),
                size: font.color_atlas_size(),
                identity: font.atlas_identity(),
                generation: font.color_atlas_generation(),
                format: color_atlas_format(format.is_srgb()),
                bytes_per_px: COLOR_BYTES_PER_PX,
                label: "noa-glyph-color-atlas",
            },
        );
        SharedGlyphAtlases {
            format,
            state: Arc::new(Mutex::new(SharedGlyphAtlasState {
                mask,
                color,
                texture_generation: 0,
            })),
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Sync CPU-side atlas bytes into the shared GPU textures. The upload is
    /// guarded by the shared atlas identity/generation, so the first renderer
    /// drawing after a glyph mutation performs the write and the rest no-op.
    pub fn sync(&self, device: &wgpu::Device, queue: &wgpu::Queue, font: &FontGrid) -> u64 {
        let mut state = self.state.lock();
        let mask_recreated = state.mask.sync(
            device,
            queue,
            AtlasSyncInput {
                data: font.mask_atlas_data(),
                size: font.mask_atlas_size(),
                identity: font.atlas_identity(),
                generation: font.mask_atlas_generation(),
                format: wgpu::TextureFormat::R8Unorm,
                bytes_per_px: MASK_BYTES_PER_PX,
                label: "noa-glyph-mask-atlas",
            },
        );
        let color_recreated = state.color.sync(
            device,
            queue,
            AtlasSyncInput {
                data: font.color_atlas_data(),
                size: font.color_atlas_size(),
                identity: font.atlas_identity(),
                generation: font.color_atlas_generation(),
                format: color_atlas_format(self.format.is_srgb()),
                bytes_per_px: COLOR_BYTES_PER_PX,
                label: "noa-glyph-color-atlas",
            },
        );
        if mask_recreated || color_recreated {
            state.texture_generation = state.texture_generation.saturating_add(1);
        }
        state.texture_generation
    }

    pub(crate) fn views(&self) -> SharedGlyphAtlasViews {
        let state = self.state.lock();
        SharedGlyphAtlasViews {
            mask: state.mask.view.clone(),
            color: state.color.view.clone(),
            texture_generation: state.texture_generation,
        }
    }

    pub fn texture_generation(&self) -> u64 {
        let state = self.state.lock();
        state.texture_generation
    }

    pub fn mask_seen_identity(&self) -> u64 {
        let state = self.state.lock();
        state.mask.seen_identity
    }

    pub fn mask_seen_generation(&self) -> u64 {
        let state = self.state.lock();
        state.mask.seen_generation
    }

    pub fn color_seen_identity(&self) -> u64 {
        let state = self.state.lock();
        state.color.seen_identity
    }

    pub fn color_seen_generation(&self) -> u64 {
        let state = self.state.lock();
        state.color.seen_generation
    }
}

/// Lazily built shared glyph atlas texture pairs per target format.
#[derive(Default)]
pub struct GlyphAtlasCache {
    entries: Vec<SharedGlyphAtlases>,
}

impl GlyphAtlasCache {
    pub fn get(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font: &FontGrid,
    ) -> SharedGlyphAtlases {
        if let Some(entry) = self.entries.iter().find(|entry| entry.format == format) {
            entry.sync(device, queue, font);
            return entry.clone();
        }
        let entry = SharedGlyphAtlases::new(device, queue, format, font);
        self.entries.push(entry.clone());
        entry
    }
}
