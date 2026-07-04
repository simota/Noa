//! Kitty-graphics image compositing (design Step R).
//!
//! [`ImageLayer`] owns the image pipeline, a sampler, and an id-keyed texture
//! cache. The [`Renderer`](crate::renderer::Renderer) drives it: it uploads the
//! frame's [`SnapshotImage`]s into GPU textures, resolves each visible
//! [`ImagePlacementSnapshot`] into a window-pixel quad (pure geometry, see
//! [`resolve_image_quad`]), and interleaves those quads with the cell passes in
//! three z bands (below background, below text, above text).
//!
//! Structured after [`crate::blit::CardPipeline`]: a 6-vertex quad, a
//! `texture + sampler + uniform` bind group whose uniform is read in **both**
//! shader stages (so its layout visibility must be `VERTEX_FRAGMENT` — CLAUDE.md
//! GPU gotcha), and `ALPHA_BLENDING` with straight (non-premultiplied) alpha.

use std::collections::HashMap;

use noa_core::{GridPadding, PixelSize};

use crate::draw_plan::{PaneId, PaneRect};
use crate::snapshot::{ImagePlacementSnapshot, SnapshotImage};

/// Placements with `z` below this composite *under* the cell background
/// (Ghostty/kitty convention). `-2^30`, matching the design's band split.
pub const Z_BG_THRESHOLD: i32 = -1_073_741_824;

/// Which of the three composite bands a placement's `z` falls into. The
/// renderer draws each band at a different point relative to the cell passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageBand {
    /// `z < Z_BG_THRESHOLD` — drawn before the cell background quads.
    BelowBackground,
    /// `Z_BG_THRESHOLD <= z < 0` — drawn between background and text.
    BelowText,
    /// `z >= 0` — drawn over text, but still under UI overlays.
    AboveText,
}

impl ImageBand {
    /// Index into a per-pane `[Vec; 3]` grouping, band order back-to-front.
    pub fn index(self) -> usize {
        match self {
            ImageBand::BelowBackground => 0,
            ImageBand::BelowText => 1,
            ImageBand::AboveText => 2,
        }
    }
}

/// Classify a placement's `z-index` into one of the three composite bands.
pub fn classify_band(z: i32) -> ImageBand {
    if z < Z_BG_THRESHOLD {
        ImageBand::BelowBackground
    } else if z < 0 {
        ImageBand::BelowText
    } else {
        ImageBand::AboveText
    }
}

/// A placement resolved to a window-pixel destination rectangle and a
/// normalized source-uv rectangle — the exact inputs the shader uniform needs.
/// Pure geometry (no GPU state), so [`resolve_image_quad`] is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedImageQuad {
    pub image_id: u32,
    pub epoch: u64,
    /// Window pixels: `[x, y, w, h]` (x/y may be negative when the image spills
    /// above/left of the pane; the pane scissor clips it).
    pub dest_rect: [f32; 4],
    /// Normalized source uv: `[x, y, w, h]` in `[0,1]`.
    pub src_uv: [f32; 4],
    pub band: ImageBand,
}

/// Resolve one placement against its pane geometry. `dest_rect` reuses the same
/// pane-origin + padding + cell-size math the cell uniforms use, so an image
/// aligns exactly with the grid cells it is anchored to.
pub fn resolve_image_quad(
    placement: &ImagePlacementSnapshot,
    image: &SnapshotImage,
    pane: PaneRect,
    padding: GridPadding,
    cell_w: f32,
    cell_h: f32,
) -> ResolvedImageQuad {
    let origin_x = pane.x as f32
        + padding.left
        + placement.grid_x as f32 * cell_w
        + f32::from(placement.cell_x_off);
    let origin_y = pane.y as f32
        + padding.top
        + placement.grid_y as f32 * cell_h
        + f32::from(placement.cell_y_off);
    let dest_w = f32::from(placement.cols) * cell_w;
    let dest_h = f32::from(placement.rows) * cell_h;

    let src_uv = match placement.src {
        Some([x, y, w, h]) => {
            let iw = image.width.max(1) as f32;
            let ih = image.height.max(1) as f32;
            [x as f32 / iw, y as f32 / ih, w as f32 / iw, h as f32 / ih]
        }
        None => [0.0, 0.0, 1.0, 1.0],
    };

    ResolvedImageQuad {
        image_id: placement.image_id,
        epoch: placement.epoch,
        dest_rect: [origin_x, origin_y, dest_w, dest_h],
        src_uv,
        band: classify_band(placement.z),
    }
}

/// Drop cache entries older than this many frames when they are also over the
/// byte budget — a briefly-scrolled-off image survives a few frames so it need
/// not re-upload every time it flickers in and out of the viewport.
const MAX_UNUSED_FRAMES: u64 = 300;
/// Total resident texture-byte budget before the LRU starts evicting.
const TOTAL_TEXTURE_BYTES_LIMIT: usize = 512 * 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageUniformsRaw {
    dest_rect: [f32; 4],
    src_uv: [f32; 4],
    surface_size: [f32; 2],
    _pad: [f32; 2],
}

/// One cached GPU texture for a `(pane, image id)` pair. Keyed on the pane too
/// because image ids are per-terminal, so id `1` in one split is unrelated to
/// id `1` in another.
struct ImageTextureEntry {
    epoch: u64,
    /// Kept only to own the texture's lifetime; sampling goes through `view`.
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bytes: usize,
    last_used_frame: u64,
}

/// A per-placement draw resource: its uniform buffer and bind group, alive for
/// the whole render pass (mirrors [`crate::blit::CardPipeline`]'s resource
/// lifetime management).
pub struct ImageDraw {
    _buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    cache: HashMap<(u64, u32), ImageTextureEntry>,
    total_bytes: usize,
    frame: u64,
    /// Count of `write_texture` uploads over the layer's lifetime, exposed for
    /// the headless test asserting an epoch bump forces a re-upload.
    uploads: u64,
}

impl ImageLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("noa-image-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noa-image-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // The uniform's `dest_rect`/`surface_size`/`src_uv` are read in
                // the vertex stage, so its visibility must be VERTEX_FRAGMENT
                // (CLAUDE.md GPU gotcha).
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("noa-image-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("noa-image-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noa-image-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            cache: HashMap::new(),
            total_bytes: 0,
            frame: 0,
            uploads: 0,
        }
    }

    /// Lifetime count of texture uploads (`write_texture`), for the headless
    /// epoch-reupload test.
    pub fn upload_count(&self) -> u64 {
        self.uploads
    }

    /// Begin a new frame's compositing: advance the LRU clock. Call once per
    /// `draw_panes`, before uploading any pane's images.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Ensure every image in `images` has an up-to-date texture cached for
    /// `pane`, uploading (or re-uploading on an epoch bump) as needed. Marks
    /// each touched entry as used this frame.
    pub fn upload_pane_images(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pane: PaneId,
        images: &[SnapshotImage],
    ) {
        for image in images {
            let key = (pane.get(), image.id);
            if let Some(entry) = self.cache.get_mut(&key)
                && entry.epoch == image.epoch
            {
                entry.last_used_frame = self.frame;
                continue;
            }
            if image.width == 0 || image.height == 0 {
                continue;
            }
            let bytes = image.rgba.len();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("noa-image-texture"),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            if let Some(old) = self.cache.insert(
                key,
                ImageTextureEntry {
                    epoch: image.epoch,
                    _texture: texture,
                    view,
                    bytes,
                    last_used_frame: self.frame,
                },
            ) {
                self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
            }
            self.total_bytes += bytes;
            self.uploads += 1;
        }
    }

    /// Build the per-placement draw resources for one pane, grouped into the
    /// three z bands. Returns `None` entries elided: `out[band]` holds the
    /// draws for that band in the placements' (z-ascending) order.
    #[allow(clippy::too_many_arguments)]
    pub fn build_pane_draws(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pane: PaneId,
        placements: &[ImagePlacementSnapshot],
        images: &[SnapshotImage],
        pane_rect: PaneRect,
        padding: GridPadding,
        cell_size: (f32, f32),
        surface: PixelSize,
    ) -> [Vec<ImageDraw>; 3] {
        let mut bands: [Vec<ImageDraw>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let surface_size = [surface.w.max(1) as f32, surface.h.max(1) as f32];
        for placement in placements {
            let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
                continue;
            };
            let Some(entry) = self.cache.get(&(pane.get(), placement.image_id)) else {
                continue;
            };
            let quad = resolve_image_quad(
                placement,
                image,
                pane_rect,
                padding,
                cell_size.0,
                cell_size.1,
            );
            let uniforms = ImageUniformsRaw {
                dest_rect: quad.dest_rect,
                src_uv: quad.src_uv,
                surface_size,
                _pad: [0.0, 0.0],
            };
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("noa-image-uniform"),
                size: std::mem::size_of::<ImageUniformsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniforms));
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("noa-image-bind-group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&entry.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            });
            bands[quad.band.index()].push(ImageDraw {
                _buffer: buffer,
                bind_group,
            });
        }
        bands
    }

    /// Draw one band's quads with the image pipeline. The caller has already
    /// set the pane scissor; each quad is a 6-vertex instance-1 draw.
    pub fn draw_band(&self, pass: &mut wgpu::RenderPass<'_>, draws: &[ImageDraw]) {
        if draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for draw in draws {
            pass.set_bind_group(0, &draw.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
    }

    /// Evict textures not used this frame once over budget, and any that have
    /// gone unused for a long time. Call once per `draw_panes`, after all panes'
    /// images are uploaded.
    pub fn evict(&mut self) {
        let frame = self.frame;
        // Long-idle entries always go, regardless of budget.
        self.cache.retain(|_, entry| {
            let keep = frame.wrapping_sub(entry.last_used_frame) <= MAX_UNUSED_FRAMES;
            if !keep {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            }
            keep
        });
        if self.total_bytes <= TOTAL_TEXTURE_BYTES_LIMIT {
            return;
        }
        // Over budget: drop least-recently-used entries (never this frame's) until
        // back under the limit.
        let mut victims: Vec<((u64, u32), u64)> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.last_used_frame != frame)
            .map(|(key, entry)| (*key, entry.last_used_frame))
            .collect();
        victims.sort_by_key(|(_, last_used)| *last_used);
        for (key, _) in victims {
            if self.total_bytes <= TOTAL_TEXTURE_BYTES_LIMIT {
                break;
            }
            if let Some(entry) = self.cache.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn snapshot_image(id: u32, width: u32, height: u32) -> SnapshotImage {
        SnapshotImage {
            id,
            epoch: 0,
            width,
            height,
            rgba: Arc::from(vec![0u8; (width * height * 4) as usize]),
        }
    }

    #[test]
    fn band_classification_splits_at_zero_and_bg_threshold() {
        assert_eq!(classify_band(-2_000_000_000), ImageBand::BelowBackground);
        assert_eq!(classify_band(Z_BG_THRESHOLD - 1), ImageBand::BelowBackground);
        assert_eq!(classify_band(Z_BG_THRESHOLD), ImageBand::BelowText);
        assert_eq!(classify_band(-1), ImageBand::BelowText);
        assert_eq!(classify_band(0), ImageBand::AboveText);
        assert_eq!(classify_band(7), ImageBand::AboveText);
    }

    #[test]
    fn resolve_places_quad_at_pane_origin_plus_cell_offset() {
        let placement = ImagePlacementSnapshot {
            image_id: 1,
            epoch: 0,
            grid_x: 2,
            grid_y: 3,
            cell_x_off: 4,
            cell_y_off: 5,
            cols: 6,
            rows: 7,
            src: None,
            z: 0,
        };
        let image = snapshot_image(1, 100, 50);
        let quad = resolve_image_quad(
            &placement,
            &image,
            PaneRect::new(10, 20, 800, 600),
            GridPadding::new(1.0, 0.0, 0.0, 8.0),
            9.0,
            18.0,
        );
        // x = pane.x(10) + pad.left(8) + grid_x(2)*9 + cell_x_off(4) = 40
        // y = pane.y(20) + pad.top(1) + grid_y(3)*18 + cell_y_off(5) = 80
        assert_eq!(quad.dest_rect, [40.0, 80.0, 6.0 * 9.0, 7.0 * 18.0]);
        assert_eq!(quad.src_uv, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(quad.band, ImageBand::AboveText);
    }

    #[test]
    fn resolve_negative_grid_coords_spill_above_left() {
        let placement = ImagePlacementSnapshot {
            image_id: 1,
            epoch: 0,
            grid_x: -1,
            grid_y: -2,
            cell_x_off: 0,
            cell_y_off: 0,
            cols: 3,
            rows: 3,
            src: None,
            z: -5,
        };
        let image = snapshot_image(1, 10, 10);
        let quad = resolve_image_quad(
            &placement,
            &image,
            PaneRect::new(0, 0, 100, 100),
            GridPadding::new(0.0, 0.0, 0.0, 0.0),
            10.0,
            10.0,
        );
        assert_eq!(quad.dest_rect, [-10.0, -20.0, 30.0, 30.0]);
        assert_eq!(quad.band, ImageBand::BelowText);
    }

    #[test]
    fn resolve_src_crop_becomes_normalized_uv() {
        let placement = ImagePlacementSnapshot {
            image_id: 1,
            epoch: 0,
            grid_x: 0,
            grid_y: 0,
            cell_x_off: 0,
            cell_y_off: 0,
            cols: 1,
            rows: 1,
            src: Some([25, 10, 50, 20]),
            z: 0,
        };
        let image = snapshot_image(1, 100, 40);
        let quad = resolve_image_quad(
            &placement,
            &image,
            PaneRect::new(0, 0, 100, 100),
            GridPadding::new(0.0, 0.0, 0.0, 0.0),
            10.0,
            10.0,
        );
        assert_eq!(quad.src_uv, [0.25, 0.25, 0.5, 0.5]);
    }

    #[test]
    fn image_uniforms_are_std140_sized() {
        // dest_rect(16) + src_uv(16) + surface_size(8) + pad(8) = 48, a
        // multiple of 16 with no interior padding (matches the WGSL struct).
        assert_eq!(std::mem::size_of::<ImageUniformsRaw>(), 48);
    }
}
