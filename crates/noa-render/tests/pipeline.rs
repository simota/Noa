//! Headless GPU regression tests: build the real pipeline AND render one frame
//! on an actual adapter, asserting no wgpu **validation** error.
//!
//! These catch two classes of bug a plain `cargo build` cannot, because they
//! only surface at device runtime:
//!   1. shader ↔ bind-group-layout mismatches (a binding used in a stage whose
//!      layout visibility omits it) — caught at pipeline creation;
//!   2. uniform/instance buffer layout mismatches (Rust `#[repr(C)]` vs WGSL
//!      std140) — caught at draw time ("Buffer is bound with size N where the
//!      shader expects M").
//!
//! Both skip gracefully where no GPU adapter is available (headless CI without
//! a Metal/Vulkan device).

#[path = "pipeline/shared.rs"]
mod shared;

#[path = "pipeline/cards.rs"]
mod cards;
#[path = "pipeline/cell.rs"]
mod cell;
#[path = "pipeline/images.rs"]
mod images;
#[path = "pipeline/overview.rs"]
mod overview;
#[path = "pipeline/shared_atlas.rs"]
mod shared_atlas;
#[path = "pipeline/split_atlas.rs"]
mod split_atlas;

/// Idle Ember uses a straight-alpha upload, including completely clear texels.
#[test]
fn idle_ember_overlay_reupload_two_sizes_preserves_destination_alpha() {
    use noa_core::PixelSize;
    use noa_render::{CardPipeline, CardStyle, CardTexturePlacement};
    let Some((device, queue)) = shared::device_queue() else {
        eprintln!("no wgpu adapter available — skipping idle Ember overlay test");
        return;
    };
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = CardPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        wgpu::BlendState::ALPHA_BLENDING,
    );
    let surface = PixelSize { w: 32, h: 32 };
    let style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    for (width, height) in [(8_u32, 8_u32), (12, 10)] {
        let (target, view) = shared::render_target(&device, surface.w, surface.h);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("idle-ember-alpha-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.2,
                            g: 0.1,
                            b: 0.3,
                            a: 0.375,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        queue.submit(Some(encoder.finish()));
        let before = shared::read_rgba_pixels(&device, &queue, &target, surface.w, surface.h);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("idle-ember-upload"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let source_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        for opaque in [false, true] {
            if opaque {
                for y in height / 4..3 * height / 4 {
                    for x in width / 4..3 * width / 4 {
                        let offset = ((y * width + x) * 4) as usize;
                        pixels[offset..offset + 4].copy_from_slice(&[247, 247, 242, 255]);
                    }
                }
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            pipeline.overlay_texture_cards_with_opacity(
                &device,
                &queue,
                &view,
                surface,
                &style,
                &[CardTexturePlacement {
                    texture_view: &source_view,
                    x: 4,
                    y: 4,
                    w: width,
                    h: height,
                    selected: false,
                }],
                1.0,
            );
            let after = shared::read_rgba_pixels(&device, &queue, &target, surface.w, surface.h);
            assert_eq!(
                pipeline.card_pool_len_for_test(),
                1,
                "reuse the uploaded view"
            );
            if !opaque {
                assert_eq!(after, before, "transparent overlay changed destination");
            } else {
                let corner = ((4 * surface.w + 4) * 4) as usize;
                assert_eq!(&after[corner..corner + 4], &before[corner..corner + 4]);
                let center = (((4 + height / 2) * surface.w + 4 + width / 2) * 4) as usize;
                assert_eq!(after[center + 3], 255, "opaque body must actually draw");
                for (actual, expected) in after[center..center + 3].iter().zip([247_u8, 247, 242]) {
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "sRGB upload changed body color"
                    );
                }
            }
        }
        pipeline.clear_texture_cache();
        assert_eq!(
            pipeline.card_pool_len_for_test(),
            0,
            "release the dismissed source"
        );
    }
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll idle Ember draw");
    assert!(pollster::block_on(device.pop_error_scope()).is_none());
}
