//! Terminal background-image compositing.
//!
//! Ghostty analog: the `background-image*` config family (renderer side). A
//! single decoded PNG is laid behind the whole surface in the lowest z band —
//! below every pane's background quad, above the `LoadOp::Clear` color — so it
//! reads through wherever the terminal's default background is visible while
//! text and explicit-background cells still draw over it.
//!
//! [`BackgroundImageLayer`] owns the pipeline + sampler and (once set) one
//! uploaded texture. Placement is pure geometry: [`background_image_quads`]
//! resolves the fit/position/repeat config into a set of window-pixel
//! destination rectangles for the current surface size, recomputed every frame
//! so a resize is handled for free (spec AC-13/14). The image quad's alpha is
//! scaled only by `background-image-opacity`, never by `background-opacity`
//! (spec NFR-3 / AC-9): the clear color keeps `background-opacity`, the image
//! is a separate straight-alpha quad drawn on top.
//!
//! Structured after [`crate::image_layer::ImageLayer`]: a 6-vertex quad and a
//! `texture + sampler + uniform` bind group whose uniform is read in **both**
//! shader stages (visibility `VERTEX_FRAGMENT` — CLAUDE.md GPU gotcha).

use std::sync::Arc;

use noa_core::PixelSize;

/// `background-image-fit`: how the source image scales into the surface.
/// Mirrors Ghostty; the app-side [`noa_config::BackgroundImageFit`] maps onto
/// this render-side copy so `noa-render` keeps no `noa-config` dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackgroundImageFit {
    /// Native pixel size, no scaling.
    None,
    /// Fit inside the surface preserving aspect (letterbox). Ghostty default.
    #[default]
    Contain,
    /// Fill the surface preserving aspect, cropping the overflow.
    Cover,
    /// Fill the surface ignoring aspect ratio.
    Stretch,
}

/// `background-image-position`: the 9-anchor grid placing the image within the
/// surface for `contain`/`none` (and the crop anchor for `cover`; ignored for
/// `stretch`). Mirrors Ghostty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackgroundImagePosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    #[default]
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl BackgroundImagePosition {
    /// Anchor fractions `(horizontal, vertical)` in `{0.0, 0.5, 1.0}`. The
    /// destination origin is `(surface - content) * fraction` per axis, so
    /// `0.0` hugs the left/top edge and `1.0` the right/bottom. For `cover`,
    /// where content exceeds the surface, the same formula centers the crop.
    fn fractions(self) -> (f32, f32) {
        use BackgroundImagePosition::*;
        let h = match self {
            TopLeft | CenterLeft | BottomLeft => 0.0,
            TopCenter | Center | BottomCenter => 0.5,
            TopRight | CenterRight | BottomRight => 1.0,
        };
        let v = match self {
            TopLeft | TopCenter | TopRight => 0.0,
            CenterLeft | Center | CenterRight => 0.5,
            BottomLeft | BottomCenter | BottomRight => 1.0,
        };
        (h, v)
    }
}

/// Renderer-facing input: the decoded image plus its placement parameters.
/// Cheap to clone (the pixel buffer is shared via `Arc`) so `noa-app` can
/// decode once and hand a copy to every surface's renderer (spec FR-15).
#[derive(Clone)]
pub struct BackgroundImage {
    /// Straight (non-premultiplied) RGBA8, row-major, `width * height * 4`.
    pub rgba: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub fit: BackgroundImageFit,
    pub position: BackgroundImagePosition,
    pub repeat: bool,
    /// `background-image-opacity`, `0.0..=1.0`.
    pub opacity: f32,
}

/// The destination rectangle `[x, y, w, h]` (window pixels) for one copy of the
/// image, given its native size, the surface size, the fit mode, and the anchor
/// position. Pure geometry (no GPU state) so it is unit-testable (spec AC-10/11).
///
/// - `stretch` fills the surface, ignoring aspect and position.
/// - `cover` fills preserving aspect, cropping the overflow (the rect can be
///   larger than the surface and have negative origin — the rasterizer clips).
/// - `contain` fits inside preserving aspect (letterbox).
/// - `none` uses the native pixel size.
pub fn background_image_dest_rect(
    image: (u32, u32),
    surface: (u32, u32),
    fit: BackgroundImageFit,
    position: BackgroundImagePosition,
) -> [f32; 4] {
    let (iw, ih) = (image.0.max(1) as f32, image.1.max(1) as f32);
    let (sw, sh) = (surface.0.max(1) as f32, surface.1.max(1) as f32);

    let (w, h) = match fit {
        BackgroundImageFit::Stretch => return [0.0, 0.0, sw, sh],
        BackgroundImageFit::None => (iw, ih),
        BackgroundImageFit::Contain => {
            let scale = (sw / iw).min(sh / ih);
            (iw * scale, ih * scale)
        }
        BackgroundImageFit::Cover => {
            let scale = (sw / iw).max(sh / ih);
            (iw * scale, ih * scale)
        }
    };

    let (fx, fy) = position.fractions();
    let x = (sw - w) * fx;
    let y = (sh - h) * fy;
    [x, y, w, h]
}

/// The single quad to draw for the image on `surface`: its destination
/// rectangle plus the UV extent across that quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackgroundImagePlacement {
    /// Window-pixel destination `[x, y, w, h]`.
    pub dest_rect: [f32; 4],
    /// UV span across the quad. `[1.0, 1.0]` for a single placement;
    /// `[surface_w / tile_w, surface_h / tile_h]` for a tiling `repeat`, where
    /// the `AddressMode::Repeat` sampler wraps the over-1.0 uv so ONE quad
    /// tiles the image across the whole surface (O(1) — no per-tile draws).
    pub uv_scale: [f32; 2],
}

/// Resolve the fit/position/repeat config into the single quad to draw. Without
/// `repeat` (or when the fitted image already fills the surface) this is the
/// [`background_image_dest_rect`] at `uv_scale = [1, 1]`. With `repeat` and an
/// under-sized image, the quad spans the whole surface and `uv_scale` is
/// `surface / tile` so the Repeat sampler tiles it from the origin — the tile
/// count that covers the surface is `ceil(uv_scale)` per axis (spec AC-12);
/// `repeat` is primarily meaningful with `fit = none`, where the tile is the
/// native image size (spec OQ-5). Pure geometry, so it is unit-testable.
pub fn background_image_placement(
    image: (u32, u32),
    surface: (u32, u32),
    fit: BackgroundImageFit,
    position: BackgroundImagePosition,
    repeat: bool,
) -> BackgroundImagePlacement {
    let base = background_image_dest_rect(image, surface, fit, position);
    let single = BackgroundImagePlacement {
        dest_rect: base,
        uv_scale: [1.0, 1.0],
    };
    if !repeat {
        return single;
    }

    let (tw, th) = (base[2], base[3]);
    let (sw, sh) = (surface.0.max(1) as f32, surface.1.max(1) as f32);
    // Degenerate tile size, or a fit that already fills the surface
    // (stretch/contain/cover), leaves nothing to tile into: keep one quad.
    if !(tw > 0.0 && th > 0.0) || (tw >= sw && th >= sh) {
        return single;
    }
    BackgroundImagePlacement {
        dest_rect: [0.0, 0.0, sw, sh],
        uv_scale: [sw / tw, sh / th],
    }
}

/// std140-matching uniform for `shaders/background_image.wgsl`. `dest_rect`
/// (vec4, 16) + `surface_size` (vec2, 8) + `uv_scale` (vec2, 8) + `opacity` (4)
/// = 36; std140 rounds the whole struct up to the vec4 alignment (16), i.e. 48
/// bytes, so the trailing padding is 12 bytes (`[f32; 3]`, three scalars rather
/// than a `vec3` — CLAUDE.md gotcha). The Rust size must equal that 48 or the
/// draw-time buffer-binding-size check fails.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BgImageUniformsRaw {
    dest_rect: [f32; 4],
    surface_size: [f32; 2],
    uv_scale: [f32; 2],
    opacity: f32,
    _pad: [f32; 3],
}

/// One uploaded texture plus the placement parameters it was set with.
struct BackgroundImageGpu {
    /// Kept only to own the texture's lifetime; sampling goes through `view`.
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    fit: BackgroundImageFit,
    position: BackgroundImagePosition,
    repeat: bool,
    opacity: f32,
}

/// A per-tile draw resource: its uniform buffer and bind group, alive for the
/// whole render pass.
pub struct BgImageDraw {
    _buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Owns the background-image pipeline, sampler, and (once set) the uploaded
/// texture. One per [`crate::Renderer`], so each surface carries its own image.
/// The immutable GPU half of the background-image layer, shareable across
/// `Renderer`s via [`crate::SharedPipelines`].
pub struct BackgroundImagePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

pub struct BackgroundImageLayer {
    shared: Arc<BackgroundImagePipeline>,
    image: Option<BackgroundImageGpu>,
}

impl BackgroundImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("noa-background-image-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/background_image.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noa-background-image-bind-group-layout"),
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
                // `dest_rect`/`surface_size` are read in the vertex stage, so the
                // uniform's visibility must be VERTEX_FRAGMENT (CLAUDE.md gotcha).
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
            label: Some("noa-background-image-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("noa-background-image-pipeline"),
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

        // Repeat addressing so a single `repeat` quad tiles the image via
        // over-1.0 uv. For a non-repeat placement uv stays in `[0, 1]`, where
        // Repeat and ClampToEdge are identical — so one sampler serves both.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noa-background-image-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }
}

impl BackgroundImageLayer {
    pub fn new(shared: Arc<BackgroundImagePipeline>) -> Self {
        Self {
            shared,
            image: None,
        }
    }

    /// Whether a background image is currently set.
    pub fn has_image(&self) -> bool {
        self.image.is_some()
    }

    /// Upload (or clear) the background image. A startup-time call: `noa-app`
    /// decodes the PNG once and calls this right after building each surface's
    /// renderer. `None`, or a zero-sized image, clears any existing image.
    pub fn set_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: Option<BackgroundImage>,
    ) {
        let Some(image) = image else {
            self.image = None;
            return;
        };
        if image.width == 0 || image.height == 0 || image.rgba.is_empty() {
            self.image = None;
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-background-image-texture"),
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
        self.image = Some(BackgroundImageGpu {
            _texture: texture,
            view,
            width: image.width,
            height: image.height,
            fit: image.fit,
            position: image.position,
            repeat: image.repeat,
            opacity: image.opacity.clamp(0.0, 1.0),
        });
    }

    /// Build this frame's single draw resource for the current `surface` size,
    /// or `None` when no image is set (the common path allocates nothing). Even
    /// a tiling `repeat` is ONE quad + ONE buffer + ONE bind group — the Repeat
    /// sampler does the tiling — so per-frame cost is O(1) regardless of image
    /// size vs. surface size. The placement recomputes from `surface` every
    /// call, so a resize needs no extra bookkeeping (spec AC-13/14).
    pub fn build_draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: PixelSize,
    ) -> Option<BgImageDraw> {
        let image = self.image.as_ref()?;
        if surface.w == 0 || surface.h == 0 {
            return None;
        }
        let placement = background_image_placement(
            (image.width, image.height),
            (surface.w, surface.h),
            image.fit,
            image.position,
            image.repeat,
        );
        let uniforms = BgImageUniformsRaw {
            dest_rect: placement.dest_rect,
            surface_size: [surface.w as f32, surface.h as f32],
            uv_scale: placement.uv_scale,
            opacity: image.opacity,
            _pad: [0.0; 3],
        };
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("noa-background-image-uniform"),
            size: std::mem::size_of::<BgImageUniformsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniforms));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noa-background-image-bind-group"),
            layout: &self.shared.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&image.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.shared.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        Some(BgImageDraw {
            _buffer: buffer,
            bind_group,
        })
    }

    /// Draw the prepared background-image quad, if any. The caller has left the
    /// default full-surface scissor; a single 6-vertex instance-1 draw.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, draw: Option<&BgImageDraw>) {
        let Some(draw) = draw else {
            return;
        };
        pass.set_pipeline(&self.shared.pipeline);
        pass.set_bind_group(0, &draw.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-10: dest rect for each fit against a known image + surface.
    #[test]
    fn dest_rect_matches_each_fit_mode() {
        // 100x50 image into a 400x200 surface, centered.
        let img = (100, 50);
        let surf = (400, 200);
        let pos = BackgroundImagePosition::Center;

        // stretch: fills, ignores aspect + position.
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::Stretch, pos),
            [0.0, 0.0, 400.0, 200.0]
        );
        // none: native size, centered -> x=(400-100)/2=150, y=(200-50)/2=75.
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::None, pos),
            [150.0, 75.0, 100.0, 50.0]
        );
        // contain: scale=min(400/100, 200/50)=min(4,4)=4 -> 400x200, no letterbox here.
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::Contain, pos),
            [0.0, 0.0, 400.0, 200.0]
        );
        // cover: scale=max(4,4)=4 -> 400x200 as well (aspect already matches).
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::Cover, pos),
            [0.0, 0.0, 400.0, 200.0]
        );
    }

    #[test]
    fn contain_letterboxes_and_cover_crops_on_mismatched_aspect() {
        // Wide 200x100 image into a tall 100x100 surface.
        let img = (200, 100);
        let surf = (100, 100);
        let pos = BackgroundImagePosition::Center;

        // contain: scale=min(100/200, 100/100)=0.5 -> 100x50, centered vertically
        // (letterbox top/bottom): y=(100-50)/2=25.
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::Contain, pos),
            [0.0, 25.0, 100.0, 50.0]
        );
        // cover: scale=max(0.5, 1.0)=1.0 -> 200x100, cropped horizontally
        // (centered): x=(100-200)/2=-50.
        assert_eq!(
            background_image_dest_rect(img, surf, BackgroundImageFit::Cover, pos),
            [-50.0, 0.0, 200.0, 100.0]
        );
    }

    // AC-11: contain honors all 9 anchors; stretch ignores position.
    #[test]
    fn contain_anchors_at_each_of_nine_positions() {
        // 200x100 image, 100x100 surface, contain -> content 100x50, so the
        // free space is (0, 50): x anchors are all 0, y anchors are 0/25/50.
        let img = (200, 100);
        let surf = (100, 100);
        let fit = BackgroundImageFit::Contain;
        let cases = [
            (BackgroundImagePosition::TopLeft, [0.0, 0.0]),
            (BackgroundImagePosition::TopCenter, [0.0, 0.0]),
            (BackgroundImagePosition::TopRight, [0.0, 0.0]),
            (BackgroundImagePosition::CenterLeft, [0.0, 25.0]),
            (BackgroundImagePosition::Center, [0.0, 25.0]),
            (BackgroundImagePosition::CenterRight, [0.0, 25.0]),
            (BackgroundImagePosition::BottomLeft, [0.0, 50.0]),
            (BackgroundImagePosition::BottomCenter, [0.0, 50.0]),
            (BackgroundImagePosition::BottomRight, [0.0, 50.0]),
        ];
        for (pos, [ex, ey]) in cases {
            let rect = background_image_dest_rect(img, surf, fit, pos);
            assert_eq!([rect[0], rect[1]], [ex, ey], "{pos:?}");
            assert_eq!([rect[2], rect[3]], [100.0, 50.0], "{pos:?}");
        }
    }

    #[test]
    fn contain_anchors_horizontally_when_letterbox_is_side() {
        // Tall 100x200 image into a wide 200x100 surface: contain scale=0.5 ->
        // 50x100, free space (150, 0). Horizontal anchor varies, vertical is 0.
        let img = (100, 200);
        let surf = (200, 100);
        let fit = BackgroundImageFit::Contain;
        assert_eq!(
            background_image_dest_rect(img, surf, fit, BackgroundImagePosition::CenterLeft),
            [0.0, 0.0, 50.0, 100.0]
        );
        assert_eq!(
            background_image_dest_rect(img, surf, fit, BackgroundImagePosition::Center),
            [75.0, 0.0, 50.0, 100.0]
        );
        assert_eq!(
            background_image_dest_rect(img, surf, fit, BackgroundImagePosition::CenterRight),
            [150.0, 0.0, 50.0, 100.0]
        );
    }

    #[test]
    fn stretch_ignores_position() {
        let img = (100, 50);
        let surf = (400, 200);
        for pos in [
            BackgroundImagePosition::TopLeft,
            BackgroundImagePosition::Center,
            BackgroundImagePosition::BottomRight,
        ] {
            assert_eq!(
                background_image_dest_rect(img, surf, BackgroundImageFit::Stretch, pos),
                [0.0, 0.0, 400.0, 200.0],
                "{pos:?}"
            );
        }
    }

    // AC-12: repeat tiles an under-sized image via a single full-surface quad
    // whose uv_scale = surface/tile — `ceil(uv_scale)` per axis is the tile
    // count that covers the surface (O(1) draw, no per-tile allocation).
    #[test]
    fn repeat_covers_the_surface_via_uv_scale() {
        // 30x20 image, none fit, 100x50 surface, repeat -> tile 30x20, one quad
        // over the whole surface, uv_scale = [100/30, 50/20].
        let placement = background_image_placement(
            (30, 20),
            (100, 50),
            BackgroundImageFit::None,
            BackgroundImagePosition::Center,
            true,
        );
        assert_eq!(placement.dest_rect, [0.0, 0.0, 100.0, 50.0]);
        assert!((placement.uv_scale[0] - 100.0 / 30.0).abs() < 1e-6);
        assert!((placement.uv_scale[1] - 50.0 / 20.0).abs() < 1e-6);
        // ceil(uv_scale) matches the tile count that covers the surface:
        // cols=ceil(100/30)=4, rows=ceil(50/20)=3.
        assert_eq!(placement.uv_scale[0].ceil() as u32, 4);
        assert_eq!(placement.uv_scale[1].ceil() as u32, 3);
    }

    #[test]
    fn repeat_with_a_filling_fit_stays_a_single_untiled_quad() {
        // stretch already fills the surface -> nothing to tile: one quad, uv 1x.
        let placement = background_image_placement(
            (30, 20),
            (100, 50),
            BackgroundImageFit::Stretch,
            BackgroundImagePosition::Center,
            true,
        );
        assert_eq!(placement.dest_rect, [0.0, 0.0, 100.0, 50.0]);
        assert_eq!(placement.uv_scale, [1.0, 1.0]);
    }

    #[test]
    fn no_repeat_is_a_single_quad_with_unit_uv() {
        let placement = background_image_placement(
            (30, 20),
            (100, 50),
            BackgroundImageFit::None,
            BackgroundImagePosition::Center,
            false,
        );
        // none fit, centered: 30x20 at (35, 15), uv 1x (no tiling).
        assert_eq!(placement.dest_rect, [35.0, 15.0, 30.0, 20.0]);
        assert_eq!(placement.uv_scale, [1.0, 1.0]);
    }

    // AC-13: recomputing against a new surface size yields the resized rect.
    #[test]
    fn dest_rect_recomputes_on_surface_resize() {
        let img = (100, 100);
        let pos = BackgroundImagePosition::Center;
        let fit = BackgroundImageFit::Contain;
        let before = background_image_dest_rect(img, (200, 200), fit, pos);
        let after = background_image_dest_rect(img, (400, 200), fit, pos);
        // 200x200: contain scale=2 -> 200x200 filling.
        assert_eq!(before, [0.0, 0.0, 200.0, 200.0]);
        // 400x200: contain scale=min(4,2)=2 -> 200x200, centered horizontally.
        assert_eq!(after, [100.0, 0.0, 200.0, 200.0]);
        assert_ne!(before, after);
    }

    #[test]
    fn uniforms_are_std140_sized() {
        // dest_rect(16) + surface_size(8) + uv_scale(8) + opacity(4) + pad(12)
        // = 48, the std140 size (rounded up to the vec4 alignment).
        assert_eq!(std::mem::size_of::<BgImageUniformsRaw>(), 48);
    }
}
