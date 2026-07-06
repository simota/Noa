//! Split out of the former monolithic `renderer.rs` — glyph-atlas texture upload and sync.

/// R8 mask atlas: 1 byte per pixel.
pub(super) const MASK_BYTES_PER_PX: u32 = 1;
/// RGBA8 color atlas: 4 bytes per pixel.
pub(super) const COLOR_BYTES_PER_PX: u32 = 4;

/// The color (emoji) atlas holds sRGB-encoded bytes as rasterized. The shader
/// passes samples through untinted, so the texture format must match the
/// target's transfer function: on an sRGB target the sampler must decode
/// sRGB→linear (the surface re-encodes on write; a plain `Rgba8Unorm` there
/// double-encodes and washes emoji out), while on a non-sRGB target the bytes
/// pass through verbatim.
pub(super) fn color_atlas_format(target_is_srgb: bool) -> wgpu::TextureFormat {
    if target_is_srgb {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

pub(super) fn upload_atlas(
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

pub(super) fn create_atlas_texture(
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

/// Inputs for one atlas's [`sync_one_atlas`] call.
pub(super) struct AtlasSyncInput<'a> {
    pub(super) data: &'a [u8],
    pub(super) size: (u32, u32),
    pub(super) identity: u64,
    pub(super) generation: u64,
    pub(super) format: wgpu::TextureFormat,
    pub(super) bytes_per_px: u32,
    pub(super) label: &'static str,
}

/// Sync one atlas's GPU texture to its CPU-side generation: re-upload the
/// pixel data whenever the generation advanced, first recreating the texture
/// and view if the atlas grew. Returns `true` iff the texture was recreated,
/// so callers know whether dependent bind groups need rebuilding.
///
/// Shared by [`Renderer::sync_atlas`] for both the mask and color atlas
/// (FM-09 mitigation) rather than duplicating this logic per atlas.
pub(super) fn sync_one_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &mut wgpu::Texture,
    view: &mut wgpu::TextureView,
    seen_identity: &mut u64,
    seen_generation: &mut u64,
    input: AtlasSyncInput<'_>,
) -> bool {
    if input.identity == *seen_identity
        && input.generation == *seen_generation
        && input.size == (texture.width(), texture.height())
    {
        return false;
    }
    let (w, h) = input.size;
    let mut recreated = false;
    if w != texture.width() || h != texture.height() {
        *texture = create_atlas_texture(device, w, h, input.format, input.label);
        *view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        recreated = true;
    }
    upload_atlas(queue, texture, input.data, w, h, input.bytes_per_px);
    *seen_identity = input.identity;
    *seen_generation = input.generation;
    recreated
}
