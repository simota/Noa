//! Offscreen thumbnail resources for Session Overview.
//!
//! Two pipelines live here. [`BlitPipeline`] downscales a tab's full-resolution
//! scratch render into a small tile texture (REQ-NF-3). [`CardPipeline`]
//! composites those tile textures onto the Overview surface as rounded cards
//! with a border / focus ring (REQ-OV-12/14, v2 mockup parity), replacing the
//! earlier plain `copy_texture_to_texture`, which could not mask corners.

use noa_core::PixelSize;

use crate::renderer::Renderer;

#[derive(Clone, Copy, Debug, PartialEq)]
struct BlitViewport {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn wgpu_color(rgba: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
        a: f64::from(rgba[3]),
    }
}

fn overview_content_viewport(
    tile_size: PixelSize,
    title_bar_h: u32,
    source_size: PixelSize,
) -> BlitViewport {
    let content_w = tile_size.w as f32;
    let content_h = tile_size.h.saturating_sub(title_bar_h) as f32;
    if content_w <= 0.0 || content_h <= 0.0 {
        return BlitViewport {
            x: 0.0,
            y: title_bar_h as f32,
            w: 0.0,
            h: 0.0,
        };
    }

    let source_w = source_size.w.max(1) as f32;
    let source_h = source_size.h.max(1) as f32;
    let scale = (content_w / source_w).min(content_h / source_h);
    let w = (source_w * scale).min(content_w);
    let h = (source_h * scale).min(content_h);

    BlitViewport {
        x: (content_w - w) * 0.5,
        y: title_bar_h as f32 + (content_h - h) * 0.5,
        w,
        h,
    }
}

pub struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl BlitPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("noa-overview-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noa-overview-blit-bind-group-layout"),
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("noa-overview-blit-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("noa-overview-blit-pipeline"),
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
                    blend: None,
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
            label: Some("noa-overview-blit-sampler"),
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
        }
    }

    /// Clear `dst` to `clear`, then (if `viewport` has a non-zero area)
    /// downscale-sample `src` into that sub-rectangle of `dst`. The clear runs
    /// regardless, so a live tile's title band (outside the content viewport)
    /// ends up filled with the card color even when the mirror only covers the
    /// content region below it.
    fn blit(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        clear: wgpu::Color,
        viewport: (f32, f32, f32, f32),
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noa-overview-blit-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noa-overview-blit-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("noa-overview-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let (vx, vy, vw, vh) = viewport;
            if vw > 0.0 && vh > 0.0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_viewport(vx, vy, vw, vh, 0.0, 1.0);
                pass.draw(0..6, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// Per-tile card styling for [`OverviewThumbnailResources::composite_cards`].
/// All values are compile-time constants owned by `noa-app` (⚠G: no config
/// knob); this struct just carries them across the crate boundary.
#[derive(Clone, Copy, Debug)]
pub struct CardStyle {
    pub background: [f32; 4],
    pub border_color: [f32; 4],
    pub focus_color: [f32; 4],
    pub corner_radius: f32,
    pub border_width: f32,
    pub focus_width: f32,
    pub focus_glow_width: f32,
}

/// One tile's placement + selection state for the card composite.
#[derive(Clone, Copy, Debug)]
pub struct CardTilePlacement {
    pub tile_index: usize,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub selected: bool,
}

/// One arbitrary texture's placement + selection state for the same rounded
/// card shader used by overview tiles.
#[derive(Clone, Copy, Debug)]
pub struct CardTexturePlacement<'a> {
    pub texture_view: &'a wgpu::TextureView,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub selected: bool,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CardUniformsRaw {
    rect: [f32; 4],
    border_color: [f32; 4],
    glow_color: [f32; 4],
    surface_size: [f32; 2],
    corner_radius: f32,
    border_width: f32,
    glow_width: f32,
    _padding: [f32; 3],
}

pub struct CardPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl CardPipeline {
    /// Alpha-replace blend for sidebar composites: color is the usual
    /// src-over, but the alpha channel is *written* (src·1 + dst·0) rather
    /// than accumulated. Overlapping band/card composites all settle to the
    /// same source alpha (last writer wins) instead of `ALPHA_BLENDING`'s
    /// `src + dst·(1-src)` over, which drives alpha toward opaque and defeats
    /// `background-opacity` on the sidebar.
    pub const ALPHA_REPLACE: wgpu::BlendState = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
    };

    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, blend: wgpu::BlendState) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("noa-overview-card-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/card.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noa-overview-card-bind-group-layout"),
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
                // The uniform is read in the vertex stage (`rect`,
                // `surface_size`) as well as the fragment stage, so its
                // visibility must be VERTEX_FRAGMENT (CLAUDE.md GPU gotcha).
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
            label: Some("noa-overview-card-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("noa-overview-card-pipeline"),
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
                    // Caller-selected blend: ALPHA_BLENDING over an opaque
                    // backdrop (overview), or ALPHA_REPLACE for the sidebar so
                    // rounded corners (coverage -> 0) reveal the backdrop
                    // without accumulating alpha toward opaque.
                    blend: Some(blend),
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
            label: Some("noa-overview-card-sampler"),
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
        }
    }

    /// Overlay already-rendered textures as rounded cards without clearing the
    /// target. The Session Overview uses this for the centered search and hint
    /// pills after the tile grid has been composited.
    pub fn overlay_texture_cards(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        surface_size: PixelSize,
        style: &CardStyle,
        placements: &[CardTexturePlacement<'_>],
    ) {
        self.draw_texture_cards(device, queue, target, surface_size, style, placements, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_texture_cards(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        surface_size: PixelSize,
        style: &CardStyle,
        placements: &[CardTexturePlacement<'_>],
        clear: Option<[f32; 4]>,
    ) {
        let surface = [surface_size.w.max(1) as f32, surface_size.h.max(1) as f32];

        // Each card needs its own uniform buffer + bind group live for the
        // whole pass, so build them up front and hold them until submit.
        let mut resources = Vec::with_capacity(placements.len());
        for placement in placements {
            let (border_color, border_width, glow_width) = if placement.selected {
                (style.focus_color, style.focus_width, style.focus_glow_width)
            } else {
                (style.border_color, style.border_width, 0.0)
            };
            let mut glow_color = style.focus_color;
            glow_color[3] = if placement.selected { 0.45 } else { 0.0 };
            let uniforms = CardUniformsRaw {
                rect: [
                    placement.x as f32,
                    placement.y as f32,
                    placement.w as f32,
                    placement.h as f32,
                ],
                border_color,
                glow_color,
                surface_size: surface,
                corner_radius: style.corner_radius,
                border_width,
                glow_width,
                _padding: [0.0; 3],
            };
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("noa-overview-card-uniform"),
                size: std::mem::size_of::<CardUniformsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniforms));
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("noa-overview-card-bind-group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(placement.texture_view),
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
            resources.push((buffer, bind_group));
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noa-overview-card-encoder"),
        });
        {
            let load = clear.map_or(wgpu::LoadOp::Load, |color| {
                wgpu::LoadOp::Clear(wgpu_color(color))
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("noa-overview-card-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (_buffer, bind_group) in &resources {
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

struct OverviewScratchTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: PixelSize,
}

struct OverviewTileTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

pub struct OverviewThumbnailResources {
    blit: BlitPipeline,
    card: CardPipeline,
    scratch: OverviewScratchTexture,
    /// Full card textures (`[title band | content]`), one per live or
    /// placeholder tile; sampled by [`CardPipeline`] at composite time.
    tiles: Vec<OverviewTileTexture>,
    /// Small title-band textures (`tile_w x title_bar_h`), drawn into by the
    /// app's label `Renderer` then stamped onto the top of `tiles` — kept
    /// separate because the label renderer clears its whole target, which
    /// would otherwise wipe a live tile's mirror (REQ-OV-12).
    title_tiles: Vec<OverviewTileTexture>,
    format: wgpu::TextureFormat,
    tile_size: PixelSize,
    title_bar_h: u32,
    card_color: [f32; 4],
}

impl OverviewThumbnailResources {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        scratch_size: PixelSize,
        tile_size: PixelSize,
        tile_count: usize,
        title_bar_h: u32,
        card_color: [f32; 4],
    ) -> Self {
        let blit = BlitPipeline::new(device, format);
        let card = CardPipeline::new(device, format, wgpu::BlendState::ALPHA_BLENDING);
        let scratch = OverviewScratchTexture::new(device, format, scratch_size);
        let tiles = (0..tile_count)
            .map(|_| OverviewTileTexture::new(device, format, tile_size))
            .collect();
        let title_size = PixelSize {
            w: tile_size.w,
            h: title_bar_h.max(1),
        };
        let title_tiles = (0..tile_count)
            .map(|_| OverviewTileTexture::new(device, format, title_size))
            .collect();

        Self {
            blit,
            card,
            scratch,
            tiles,
            title_tiles,
            format,
            tile_size,
            title_bar_h,
            card_color,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_renderer(
        device: &wgpu::Device,
        renderer: &Renderer,
        scratch_size: PixelSize,
        tile_size: PixelSize,
        tile_count: usize,
        title_bar_h: u32,
        card_color: [f32; 4],
    ) -> Self {
        Self::new(
            device,
            renderer.target_format(),
            scratch_size,
            tile_size,
            tile_count,
            title_bar_h,
            card_color,
        )
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn scratch_size(&self) -> PixelSize {
        self.scratch.size
    }

    fn ensure_scratch_size(&mut self, device: &wgpu::Device, source_size: PixelSize) {
        let source_size = PixelSize {
            w: source_size.w.max(1),
            h: source_size.h.max(1),
        };
        if self.scratch.size != source_size {
            self.scratch = OverviewScratchTexture::new(device, self.format, source_size);
        }
    }

    pub fn tile_size(&self) -> PixelSize {
        self.tile_size
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn title_bar_h(&self) -> u32 {
        self.title_bar_h
    }

    pub fn tile_texture_for_test(&self, index: usize) -> Option<&wgpu::Texture> {
        self.tiles.get(index).map(|tile| &tile.texture)
    }

    /// A view of `tile_index`'s full card texture, for compositing the tile a
    /// second time outside [`composite_cards`](Self::composite_cards) — the
    /// overview uses this for the hover accent ring and the Tab quick-look
    /// zoom overlay.
    pub fn tile_texture_view(&self, tile_index: usize) -> Option<wgpu::TextureView> {
        self.tiles.get(tile_index).map(|tile| {
            tile.texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        })
    }

    /// A view of the title-band texture for `tile_index`, for the app's label
    /// `Renderer` to draw the tab title into (REQ-OV-12). Stamp it onto the
    /// tile with [`stamp_title_band`] afterward.
    pub fn title_texture_view(&self, tile_index: usize) -> Option<wgpu::TextureView> {
        self.title_tiles.get(tile_index).map(|tile| {
            tile.texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        })
    }

    /// Copy the title band drawn into `title_tiles[tile_index]` onto the top
    /// `title_bar_h` rows of `tiles[tile_index]` (REQ-OV-12).
    pub fn stamp_title_band(&self, device: &wgpu::Device, queue: &wgpu::Queue, tile_index: usize) {
        let (Some(title), Some(tile)) =
            (self.title_tiles.get(tile_index), self.tiles.get(tile_index))
        else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noa-overview-title-stamp-encoder"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &title.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &tile.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.tile_size.w.max(1),
                height: self.title_bar_h.max(1),
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Clear a whole tile texture to the card color — used for placeholder
    /// tiles, whose content region below the title band has no live mirror.
    pub fn clear_tile(&self, device: &wgpu::Device, queue: &wgpu::Queue, tile_index: usize) {
        let Some(tile) = self.tiles.get(tile_index) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noa-overview-tile-clear-encoder"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("noa-overview-tile-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tile.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu_color(self.card_color)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        queue.submit(Some(encoder.finish()));
    }

    pub fn render_existing_renderer_to_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        source_size: PixelSize,
        tile_index: usize,
    ) -> anyhow::Result<()> {
        if renderer.target_format() != self.format {
            anyhow::bail!(
                "overview texture format {:?} does not match renderer target format {:?}",
                self.format,
                renderer.target_format()
            );
        }

        if tile_index >= self.tiles.len() {
            anyhow::bail!("overview tile index {tile_index} is out of range");
        }
        self.ensure_scratch_size(device, source_size);
        let tile = &self.tiles[tile_index];

        // Fit the mirror into the content region without non-uniform scaling.
        // Multiple-tab grids can make tiles much taller or narrower than the
        // source frame; filling the full content rect would visibly squeeze the
        // terminal font in one axis.
        let viewport =
            overview_content_viewport(self.tile_size, self.title_bar_h, self.scratch.size);
        renderer.draw(device, queue, &self.scratch.view);
        self.blit.blit(
            device,
            queue,
            &self.scratch.view,
            &tile.view,
            wgpu_color(self.card_color),
            (viewport.x, viewport.y, viewport.w, viewport.h),
        );
        Ok(())
    }

    /// Composite every placed tile onto `target` as a rounded card with a
    /// border / focus ring (REQ-OV-12/14). The pass clears `target` to the
    /// card `background`, so this both clears the surface and draws the cards.
    pub fn composite_cards(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        surface_size: PixelSize,
        style: &CardStyle,
        placements: &[CardTilePlacement],
    ) {
        let texture_placements: Vec<_> = placements
            .iter()
            .filter_map(|placement| {
                self.tiles
                    .get(placement.tile_index)
                    .map(|tile| CardTexturePlacement {
                        texture_view: &tile.view,
                        x: placement.x,
                        y: placement.y,
                        w: placement.w,
                        h: placement.h,
                        selected: placement.selected,
                    })
            })
            .collect();
        self.card.draw_texture_cards(
            device,
            queue,
            target,
            surface_size,
            style,
            &texture_placements,
            Some(style.background),
        );
    }
}

impl OverviewScratchTexture {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: PixelSize) -> Self {
        let texture = overview_texture(
            device,
            "noa-overview-shared-scratch",
            format,
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
            size,
        }
    }
}

impl OverviewTileTexture {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: PixelSize) -> Self {
        let texture = overview_texture(
            device,
            "noa-overview-tile",
            format,
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

fn overview_texture(
    device: &wgpu::Device,
    label: &'static str,
    format: wgpu::TextureFormat,
    size: PixelSize,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.w.max(1),
            height: size.h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_viewport_uses_full_region_when_aspect_matches() {
        let viewport = overview_content_viewport(
            PixelSize { w: 400, h: 228 },
            28,
            PixelSize { w: 800, h: 400 },
        );

        assert_eq!(
            viewport,
            BlitViewport {
                x: 0.0,
                y: 28.0,
                w: 400.0,
                h: 200.0
            }
        );
    }

    #[test]
    fn content_viewport_letterboxes_tall_tiles_to_preserve_font_width() {
        let viewport = overview_content_viewport(
            PixelSize { w: 300, h: 328 },
            28,
            PixelSize { w: 800, h: 400 },
        );

        assert_eq!(
            viewport,
            BlitViewport {
                x: 0.0,
                y: 103.0,
                w: 300.0,
                h: 150.0
            }
        );
    }

    #[test]
    fn content_viewport_pillarboxes_wide_tiles_to_preserve_font_height() {
        let viewport = overview_content_viewport(
            PixelSize { w: 300, h: 328 },
            28,
            PixelSize { w: 400, h: 800 },
        );

        assert_eq!(
            viewport,
            BlitViewport {
                x: 75.0,
                y: 28.0,
                w: 150.0,
                h: 300.0
            }
        );
    }

    #[test]
    fn content_viewport_collapses_when_title_consumes_the_tile() {
        let viewport = overview_content_viewport(
            PixelSize { w: 300, h: 20 },
            28,
            PixelSize { w: 800, h: 400 },
        );

        assert_eq!(
            viewport,
            BlitViewport {
                x: 0.0,
                y: 28.0,
                w: 0.0,
                h: 0.0
            }
        );
    }
}
