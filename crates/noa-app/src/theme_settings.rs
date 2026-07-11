//! Theme-settings overlay (theme-settings-ui) — the GUI-agnostic half.
//! Mirrors `command_palette.rs`/`search_prompt.rs`: pure state + pure logic
//! with no winit/window/GPU types, so the state machine is unit-testable
//! without a display. `App` owns a `ThemeSettingsSession` wrapping
//! [`ThemeSettings`]; its `KeyboardInput` handler drives it, applies the
//! live-preview side effects ([`RowEffect`]) to `GpuState`/live terminals,
//! and feeds the rendered result into the overlay card (mirroring the
//! command palette's own card).
//!
//! Increment D landed the picker/rows/live-preview/Esc-revert state machine
//! plus the sample-pane data (R-1..R-11, R-16). Increment E adds the Enter
//! commit sequence's pure half: [`ThemeSettings::commit_updates`] (the
//! config write's payload) and [`ThemeSettings::commit`] (the injectable
//! write call itself, R-12); `App::commit_theme_settings`
//! (`app/input_ops.rs`) drives the GPU/window side effects that follow a
//! successful write.

mod rows;
mod sample;
mod state;

pub(crate) use rows::{
    RevertValues, RowDraft, RowEffect, Section, SettingsRow, SettingsRowKind, ThemeSettingsInit,
    ThemeSettingsMode, background_image_fit_value, background_image_position_value,
    settings_row_display_value,
};
pub(crate) use sample::{Swatch, sample_swatches};
#[allow(unused_imports)]
pub(crate) use state::ConfigWriteFn;
pub(crate) use state::ThemeSettings;
#[cfg(test)]
pub(crate) use state::take_scan_count;

#[cfg(test)]
mod tests;
