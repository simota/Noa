//! Headless GPU regression test: build the real cell render pipeline on an
//! actual adapter and assert it produces no wgpu **validation** error.
//!
//! This catches shader ↔ bind-group-layout mismatches (e.g. a resource used
//! in the vertex stage whose layout visibility omits VERTEX) that a plain
//! `cargo build` cannot — pipeline validation only happens at device runtime.
//! Skips gracefully where no GPU adapter is available (headless CI without a
//! Metal/Vulkan device).

use noa_font::FontGrid;
use noa_render::Renderer;

#[test]
fn cell_pipeline_has_no_validation_error() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("no wgpu adapter available — skipping GPU pipeline test");
        return;
    };

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("request_device");

    let mut font = FontGrid::new(14.0).expect("load a system monospace font");

    // Capture validation errors within a scope instead of letting wgpu's
    // default (fatal) handler abort the process.
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
    );
    let validation_error = pollster::block_on(device.pop_error_scope());

    assert!(renderer.is_ok(), "Renderer::new failed: {:?}", renderer.err());
    assert!(
        validation_error.is_none(),
        "wgpu validation error while building the cell pipeline: {validation_error:?}"
    );
}
