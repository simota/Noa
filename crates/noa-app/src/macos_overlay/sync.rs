use std::hash::{Hash, Hasher};

use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot};
use winit::window::Window;

use super::imp;
use super::model::{
    NativeOverlayCache, OverlayColors, PaneRectPt, ProcessMonitorViewModel, ThemeSettingsViewModel,
    process_monitor_view_model, theme_settings_view_model,
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

/// Sync the native process-monitor card (panel-metrics-view; same contract as
/// [`sync_command_palette`]). Unlike theme-settings' catalog-sized state, a
/// process-monitor session is cheap to turn into a [`ProcessMonitorViewModel`]
/// (row count is bounded by live panes), so this builds it unconditionally
/// each call and hashes the *built* model directly rather than a separate
/// fingerprint method.
pub(crate) fn sync_process_monitor(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&crate::process_monitor::ProcessMonitor, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let built: Option<(ProcessMonitorViewModel, PaneRectPt)> =
        model.map(|(state, rect)| (process_monitor_view_model(state), rect));
    let hash = built.as_ref().map(|(vm, rect)| {
        hash_u64(|h| {
            vm.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.process_monitor == hash {
        return;
    }
    cache.process_monitor = hash;
    imp::rebuild_process_monitor(window, built, colors);
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

/// Sync the scratch terminal popup's persistent identity badge (kaizen
/// cycle 2; window-centered like the toast, but top-anchored and left
/// showing for the popup's whole lifetime instead of fading — the caller
/// passes `Some` every redraw while the popup is shown, `None` to tear it
/// down).
pub(crate) fn sync_scratch_badge(
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
    if cache.scratch_badge == hash {
        return;
    }
    cache.scratch_badge = hash;
    imp::rebuild_scratch_badge(window, text, colors);
}
