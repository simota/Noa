use std::hash::{Hash, Hasher};

use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot};
use winit::window::Window;

use super::imp;
use super::model::{
    NativeOverlayCache, OverlayColors, PaneRectPt, ThemeSettingsViewModel,
    theme_settings_view_model,
};

fn hash_u64(f: impl FnOnce(&mut std::collections::hash_map::DefaultHasher)) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    f(&mut hasher);
    hasher.finish()
}

/// Sync the native command-palette card: `None` removes it, `Some` builds or
/// rebuilds it when the snapshot/geometry/colors changed.
pub(crate) fn sync_command_palette(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&CommandPaletteSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let hash = model.map(|(snap, rect)| {
        hash_u64(|h| {
            snap.query.hash(h);
            snap.selected.hash(h);
            snap.total_entries.hash(h);
            for row in &snap.rows {
                match row {
                    noa_render::PaletteRow::Header { label } => {
                        0u8.hash(h);
                        label.hash(h);
                    }
                    noa_render::PaletteRow::Entry {
                        title,
                        hint,
                        match_positions,
                        enabled,
                    } => {
                        1u8.hash(h);
                        title.hash(h);
                        hint.hash(h);
                        match_positions.hash(h);
                        enabled.hash(h);
                    }
                }
            }
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.palette == hash {
        return;
    }
    cache.palette = hash;
    imp::rebuild_palette(window, model, colors);
}

/// The window-free half of [`sync_theme_settings`] (ADR-2/R-20/NFR-7):
/// hashes [`crate::theme_settings::ThemeSettings::view_fingerprint`] (never
/// the ViewModel itself — building one is exactly the cost this exists to
/// avoid) and compares it against `cache.theme_settings`. Returns `None` on
/// an idempotent frame (no ViewModel built at all); `Some` on a real change,
/// carrying the freshly built ViewModel — built here exactly once, never
/// twice like the old hash-then-rebuild sequence. Also records the debug
/// rebuild counter (NFR-7/AC-58) on every `Some`, so the whole "decide +
/// build + count" sequence is unit-testable without a real `Window`
/// (AC-26/AC-27/AC-58) — [`sync_theme_settings`] itself is left as a thin
/// wrapper that only adds the final AppKit dispatch.
pub(super) fn theme_settings_sync_decision(
    cache: &mut NativeOverlayCache,
    model: Option<(&crate::theme_settings::ThemeSettings, PaneRectPt)>,
    colors: &OverlayColors,
) -> Option<Option<(ThemeSettingsViewModel, PaneRectPt)>> {
    let key = model.map(|(state, rect)| {
        hash_u64(|h| {
            state.view_fingerprint(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.theme_settings == key {
        return None;
    }
    cache.theme_settings = key;
    #[cfg(debug_assertions)]
    cache.record_theme_settings_rebuild();
    Some(model.map(|(state, rect)| (theme_settings_view_model(state), rect)))
}

/// Sync the native theme-settings card (same contract as
/// [`sync_command_palette`]).
pub(crate) fn sync_theme_settings(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&crate::theme_settings::ThemeSettings, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let Some(vm) = theme_settings_sync_decision(cache, model, colors) else {
        return;
    };
    imp::rebuild_theme_settings(window, vm, colors);
}

/// Sync the native confirm-dialog card (same contract as
/// [`sync_command_palette`]).
pub(crate) fn sync_confirm_dialog(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&ConfirmDialogSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let hash = model.map(|(snap, rect)| {
        hash_u64(|h| {
            snap.message.hash(h);
            snap.hint.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.confirm == hash {
        return;
    }
    cache.confirm = hash;
    imp::rebuild_confirm(window, model, colors);
}

/// Sync the native "Set Tab Title" prompt card (same contract as
/// [`sync_command_palette`]); `model`'s `&str` is the live input text with
/// any IME composition already appended.
pub(crate) fn sync_title_prompt(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&str, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let hash = model.map(|(input, rect)| {
        hash_u64(|h| {
            input.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.title_prompt == hash {
        return;
    }
    cache.title_prompt = hash;
    imp::rebuild_title_prompt(window, model, colors);
}

/// Sync the native resize toast (window-centered; not pane-bound).
pub(crate) fn sync_toast(
    window: &Window,
    cache: &mut NativeOverlayCache,
    text: Option<&str>,
    colors: &OverlayColors,
) {
    let hash = text.map(|t| {
        hash_u64(|h| {
            t.hash(h);
            colors.hash_into(h);
        })
    });
    if cache.toast == hash {
        return;
    }
    cache.toast = hash;
    imp::rebuild_toast(window, text, colors);
}
