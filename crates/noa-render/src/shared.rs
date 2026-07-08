//! Format-keyed shared GPU pipelines.
//!
//! Every [`crate::Renderer`] draws with the same three pipelines (cell, inline
//! image, background image); only the render-target format differs between
//! windows. [`SharedPipelines`] bundles one immutable set for a format, and
//! [`PipelineCache`] hands out clones so a new tab's `Renderer` skips shader
//! module + pipeline construction entirely after the first build per format.

use std::sync::Arc;

use crate::background_image::BackgroundImagePipeline;
use crate::image_layer::ImagePipeline;
use crate::pipeline::CellPipeline;

/// The immutable pipeline set for one render-target format. Cheap to clone
/// (three `Arc`s); per-`Renderer` mutable state (atlas textures, image
/// caches) stays out of it by construction.
#[derive(Clone)]
pub struct SharedPipelines {
    format: wgpu::TextureFormat,
    pub(crate) cell: Arc<CellPipeline>,
    pub(crate) image: Arc<ImagePipeline>,
    pub(crate) background_image: Arc<BackgroundImagePipeline>,
}

impl SharedPipelines {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            cell: Arc::new(CellPipeline::new(device, format)),
            image: Arc::new(ImagePipeline::new(device, format)),
            background_image: Arc::new(BackgroundImagePipeline::new(device, format)),
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

/// Lazily built [`SharedPipelines`] per format. In practice this holds one
/// entry (every surface on one adapter usually picks the same format), two if
/// e.g. sRGB and non-sRGB surfaces coexist — a `Vec` scan is exactly right.
#[derive(Default)]
pub struct PipelineCache {
    entries: Vec<SharedPipelines>,
}

impl PipelineCache {
    pub fn get(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> SharedPipelines {
        if let Some(entry) = self.entries.iter().find(|entry| entry.format == format) {
            return entry.clone();
        }
        let entry = SharedPipelines::new(device, format);
        self.entries.push(entry.clone());
        entry
    }
}
