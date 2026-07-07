use std::hash::{Hash, Hasher};

use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot};
use winit::window::Window;

use super::imp;
use super::model::{NativeOverlayCache, OverlayColors, PaneRectPt, theme_settings_view_model};

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
                    } => {
                        1u8.hash(h);
                        title.hash(h);
                        hint.hash(h);
                        match_positions.hash(h);
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

/// Sync the native theme-settings card (same contract as
/// [`sync_command_palette`]).
pub(crate) fn sync_theme_settings(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&crate::theme_settings::ThemeSettings, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let hash = model.map(|(state, rect)| {
        hash_u64(|h| {
            theme_settings_view_model(state).hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.theme_settings == hash {
        return;
    }
    cache.theme_settings = hash;
    imp::rebuild_theme_settings(
        window,
        model.map(|(s, r)| (theme_settings_view_model(s), r)),
        colors,
    );
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
