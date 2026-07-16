//! Diagnostic: stage-by-stage GPU-driver memory attribution probe.
//!
//! Launch with a stage name, then read the process's `footprint` from another
//! shell while it sleeps. Isolates which wgpu/Metal step materializes the
//! "Owned physical footprint (unmapped) (graphics)" driver pool.
//!
//! ```sh
//! cargo run --release -p noa-render --example mem_probe -- device
//! footprint <pid printed by the probe>
//! ```

fn main() {
    let stage = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    println!("mem_probe pid={} stage={stage}", std::process::id());

    if stage == "none" {
        sleep_forever();
    }

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    if stage == "instance" {
        sleep_forever();
    }

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("adapter");
    if stage == "adapter" {
        sleep_forever();
    }

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("mem-probe-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("device");
    if stage == "device" {
        sleep_forever();
    }

    // Build the real cell pipeline + one offscreen draw, mirroring
    // tests/pipeline.rs, to trigger PSO compilation and command submission.
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let pipeline = noa_render::SharedPipelines::new(&device, format);
    if stage == "pipeline" {
        sleep_forever();
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mem-probe-target"),
        size: wgpu::Extent3d {
            width: 1024,
            height: 768,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mem-probe-encoder"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mem-probe-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    let _keep = (&pipeline, &target);
    // stage == "draw" (or anything else) parks here.
    sleep_forever();
}

fn sleep_forever() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
