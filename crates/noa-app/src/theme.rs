//! Theme construction for the app. The palette table + `Color -> [f32;4]`
//! resolution logic lives in `noa-render` (it's needed there to build GPU
//! instance colors); this module is the app-level seam that constructs the
//! default theme noa-app hands to the renderer.

pub use noa_render::Theme;

/// The single hardcoded inc-1 theme.
pub fn default_theme() -> Theme {
    Theme::default()
}
