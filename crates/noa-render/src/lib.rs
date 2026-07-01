//! `noa-render` — wgpu instanced-cell renderer (GPU-facing, but NOT
//! windowing: it receives an already-created [`wgpu::Device`] / [`wgpu::Queue`]
//! / surface texture format from `noa-app`, and never creates a
//! [`wgpu::Surface`] or touches `winit`).
//!
//! Ghostty analog: `renderer/generic.zig` + `Metal.zig`.

mod instance;
mod pipeline;
mod renderer;
mod snapshot;
mod theme;

pub use instance::{CellInstance, Uniforms};
pub use renderer::Renderer;
pub use snapshot::FrameSnapshot;
pub use theme::Theme;
