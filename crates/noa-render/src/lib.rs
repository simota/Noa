//! `noa-render` — wgpu instanced-cell renderer (GPU-facing, but NOT
//! windowing: it receives an already-created [`wgpu::Device`] / [`wgpu::Queue`]
//! / surface texture format from `noa-app`, and never creates a
//! [`wgpu::Surface`] or touches `winit`).
//!
//! Ghostty analog: `renderer/generic.zig` + `Metal.zig`.

mod background_image;
mod blit;
mod draw_plan;
mod image_layer;
mod instance;
mod pipeline;
mod renderer;
mod segment;
mod snapshot;
mod theme;

pub use background_image::{
    BackgroundImage, BackgroundImageFit, BackgroundImagePlacement, BackgroundImagePosition,
    background_image_dest_rect, background_image_placement,
};
pub use blit::{
    BlitPipeline, CardPipeline, CardStyle, CardTexturePlacement, CardTilePlacement,
    OverviewThumbnailResources,
};
pub use draw_plan::{DrawOp, PaneId, PaneRect, build_draw_plan};
pub use image_layer::{ImageBand, Z_BG_THRESHOLD, classify_band, resolve_image_quad};
pub use instance::{CellInstance, PaneUniformParams, Uniforms, populate_pane_uniform};
pub use renderer::{
    PaletteLayout, PaneFrame, Renderer, command_palette_layout, renderer_construction_count,
};
pub use snapshot::{
    CommandPaletteSnapshot, ConfirmDialogSnapshot, FrameSnapshot, HoverLink,
    ImagePlacementSnapshot, PaletteRow, Preedit, SnapshotImage,
};
pub use theme::{OverlayStyle, Theme, blend};
