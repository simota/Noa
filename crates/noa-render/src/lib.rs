//! `noa-render` — wgpu instanced-cell renderer (GPU-facing, but NOT
//! windowing: it receives an already-created [`wgpu::Device`] / [`wgpu::Queue`]
//! / surface texture format from `noa-app`, and never creates a
//! [`wgpu::Surface`] or touches `winit`).
//!
//! Ghostty analog: `renderer/generic.zig` + `Metal.zig`.

mod blit;
mod draw_plan;
mod instance;
mod pipeline;
mod renderer;
mod segment;
mod snapshot;
mod theme;

pub use blit::{BlitPipeline, OverviewThumbnailResources};
pub use draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
pub use instance::{CellInstance, PaneUniformParams, Uniforms, populate_pane_uniform};
pub use renderer::{PaneFrame, Renderer, renderer_construction_count};
pub use snapshot::{CommandPaletteSnapshot, FrameSnapshot, HoverLink};
pub use theme::Theme;
