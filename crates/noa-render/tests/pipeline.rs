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
