//! Repository widget definitions. This crate only reads and validates data
//! (including packaged image assets); it has no renderer, image decoder,
//! scripting engine, command execution or IPC authority.

pub mod assets;
pub mod discovery;
pub mod manifest;

pub use assets::{Asset, ImageFormat, definition_digest};
pub use manifest::{Action, AssetRef, Manifest, content_digest, resolve, schema_document};

use std::fmt;

pub const API_VERSION: &str = "noa.widget/v1";
pub const DEFINITIONS_DIR: &str = ".noa-widgets";
pub const MANIFEST_FILE: &str = "widget.json";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_DATA_BYTES: usize = 128 * 1024;
pub const MAX_ACTION_BYTES: usize = 16 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_VIEW_NODES: usize = 512;
pub const MAX_TABLE_ROWS: usize = 1000;
pub const MAX_RENDERED_ITEMS: usize = 8192;
pub const MAX_RENDERED_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_DEFINITIONS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetError {
    message: String,
}

impl WidgetError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WidgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WidgetError {}

impl From<std::io::Error> for WidgetError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for WidgetError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub fn check_id(id: &str) -> Result<(), WidgetError> {
    if id.is_empty()
        || id.len() > 64
        || !id.as_bytes()[0].is_ascii_lowercase()
        || id.ends_with('-')
        || id.contains("--")
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(WidgetError::new(format!(
            "invalid id {id:?}: use at most 64 lowercase letters, digits and single hyphens, starting with a letter"
        )));
    }
    Ok(())
}
