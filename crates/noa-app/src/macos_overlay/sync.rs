use std::hash::{Hash, Hasher};

use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot};
use winit::window::Window;

use super::imp;
use super::model::{
    ChildExitedBar, ModalAxActions, NativeOverlayCache, OverlayColors, PaneRectPt,
    ProcessMonitorViewModel, TITLE_PROMPT_HINT, ThemeSettingsViewModel, child_exited_bars_hash,
    format_dialog_announcement, palette_selected_ax_label, process_monitor_row_ax_label,
    process_monitor_view_model, theme_settings_selected_ax_label, theme_settings_view_model,
};

const PALETTE_OPEN_HINT: &str = "Arrow keys to navigate, Enter to select, Esc to close";
const PROCESS_MONITOR_OPEN_HINT: &str =
    "\u{2191}\u{2193} select \u{b7} Enter jump \u{b7} s sort \u{b7} Esc close";

fn hash_u64(f: impl FnOnce(&mut std::collections::hash_map::DefaultHasher)) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    f(&mut hasher);
    hasher.finish()
}

pub(super) fn palette_sync_decision(
    cache: &mut NativeOverlayCache,
    model: Option<(&CommandPaletteSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) -> Option<ModalAxActions> {
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
        return None;
    }
    cache.palette = hash;
    let actions = match model {
        Some((snap, _)) => {
            let selected = palette_selected_ax_label(snap);
            match cache.palette_ax.replace(selected.clone()) {
                None => {
                    let body = match &selected {
                        Some(item) => format!("Command Palette. {item}"),
                        None => "Command Palette. No matching commands".to_string(),
                    };
                    ModalAxActions {
                        focus_modal: true,
                        layout_changed: true,
                        announcement: Some((
                            format_dialog_announcement(&body, PALETTE_OPEN_HINT),
                            true,
                        )),
                    }
                }
                Some(prev) if prev != selected => ModalAxActions {
                    focus_modal: false,
                    layout_changed: false,
                    announcement: Some((
                        selected.unwrap_or_else(|| "No matching commands".to_string()),
                        false,
                    )),
                },
                Some(_) => ModalAxActions::default(),
            }
        }
        None => {
            let was_open = cache.palette_ax.take().is_some();
            ModalAxActions {
                layout_changed: was_open,
                ..ModalAxActions::default()
            }
        }
    };
    Some(actions)
}

/// Sync the native command-palette card: `None` removes it, `Some` builds or
/// rebuilds it when the snapshot/geometry/colors changed.
pub(crate) fn sync_command_palette(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&CommandPaletteSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let Some(ax) = palette_sync_decision(cache, model, colors) else {
        return;
    };
    imp::rebuild_palette(window, model, colors, &ax);
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
) -> Option<(Option<(ThemeSettingsViewModel, PaneRectPt)>, ModalAxActions)> {
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
    let built = model.map(|(state, rect)| (theme_settings_view_model(state), rect));
    let actions = match &built {
        Some((vm, _)) => {
            let selected = theme_settings_selected_ax_label(vm);
            let mode_title = match vm.mode {
                crate::theme_settings::ThemeSettingsMode::Theme => "Theme",
                crate::theme_settings::ThemeSettingsMode::Settings => "Settings",
            };
            match cache.theme_settings_ax.replace((vm.mode, selected.clone())) {
                None => {
                    let body = match &selected {
                        Some(item) => format!("{mode_title}. {item}"),
                        None => mode_title.to_string(),
                    };
                    ModalAxActions {
                        focus_modal: true,
                        layout_changed: true,
                        announcement: Some((format_dialog_announcement(&body, &vm.footer.0), true)),
                    }
                }
                Some((prev_mode, _)) if prev_mode != vm.mode => {
                    let body = match &selected {
                        Some(item) => format!("{mode_title}. {item}"),
                        None => mode_title.to_string(),
                    };
                    ModalAxActions {
                        focus_modal: true,
                        layout_changed: true,
                        announcement: Some((format_dialog_announcement(&body, &vm.footer.0), true)),
                    }
                }
                Some((_, prev_selected)) if prev_selected != selected => ModalAxActions {
                    focus_modal: false,
                    layout_changed: false,
                    announcement: Some((
                        selected.unwrap_or_else(|| "No matches".to_string()),
                        false,
                    )),
                },
                Some(_) => ModalAxActions::default(),
            }
        }
        None => {
            let was_open = cache.theme_settings_ax.take().is_some();
            ModalAxActions {
                layout_changed: was_open,
                ..ModalAxActions::default()
            }
        }
    };
    Some((built, actions))
}

/// Sync the native theme-settings card (same contract as
/// [`sync_command_palette`]).
pub(crate) fn sync_theme_settings(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&crate::theme_settings::ThemeSettings, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let Some((vm, ax)) = theme_settings_sync_decision(cache, model, colors) else {
        return;
    };
    imp::rebuild_theme_settings(window, vm, colors, &ax);
}

pub(super) fn process_monitor_sync_decision(
    cache: &mut NativeOverlayCache,
    built: Option<(ProcessMonitorViewModel, PaneRectPt)>,
    colors: &OverlayColors,
) -> Option<(
    Option<(ProcessMonitorViewModel, PaneRectPt)>,
    ModalAxActions,
)> {
    let hash = built.as_ref().map(|(vm, rect)| {
        hash_u64(|h| {
            vm.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.process_monitor == hash {
        return None;
    }
    cache.process_monitor = hash;
    let actions = match &built {
        Some((vm, _)) => {
            let selected_row = vm.rows.iter().enumerate().find(|(_, row)| row.selected);
            let selected_key =
                selected_row.map(|(i, row)| (i, row.process.clone(), row.location.clone()));
            let selected_label = selected_row.map(|(_, row)| process_monitor_row_ax_label(row));
            match cache.process_monitor_ax.replace(selected_key.clone()) {
                None => {
                    let header = format!("Process Monitor, sort: {}", vm.sort_label);
                    let body = match &selected_label {
                        Some(item) => format!("{header}. {item}"),
                        None => header,
                    };
                    ModalAxActions {
                        focus_modal: true,
                        layout_changed: true,
                        announcement: Some((
                            format_dialog_announcement(&body, PROCESS_MONITOR_OPEN_HINT),
                            true,
                        )),
                    }
                }
                Some(prev) if prev != selected_key => ModalAxActions {
                    focus_modal: false,
                    layout_changed: false,
                    announcement: selected_label.map(|label| (label, false)),
                },
                Some(_) => ModalAxActions::default(),
            }
        }
        None => {
            let was_open = cache.process_monitor_ax.take().is_some();
            ModalAxActions {
                layout_changed: was_open,
                ..ModalAxActions::default()
            }
        }
    };
    Some((built, actions))
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
    let Some((built, ax)) = process_monitor_sync_decision(cache, built, colors) else {
        return;
    };
    imp::rebuild_process_monitor(window, built, colors, &ax);
}

pub(super) fn confirm_sync_decision(
    cache: &mut NativeOverlayCache,
    model: Option<(&ConfirmDialogSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) -> Option<ModalAxActions> {
    let hash = model.map(|(snap, rect)| {
        hash_u64(|h| {
            snap.message.hash(h);
            snap.hint.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.confirm == hash {
        return None;
    }
    cache.confirm = hash;
    let actions = match model {
        Some((snap, _)) => {
            let next = (snap.message.clone(), snap.hint.clone());
            match cache.confirm_ax.replace(next.clone()) {
                None => ModalAxActions {
                    focus_modal: true,
                    layout_changed: true,
                    announcement: Some((
                        format_dialog_announcement(&snap.message, &snap.hint),
                        true,
                    )),
                },
                Some(prev) if prev != next => ModalAxActions {
                    focus_modal: true,
                    layout_changed: false,
                    announcement: Some((
                        format_dialog_announcement(&snap.message, &snap.hint),
                        true,
                    )),
                },
                Some(_) => ModalAxActions::default(),
            }
        }
        None => {
            let was_open = cache.confirm_ax.take().is_some();
            ModalAxActions {
                layout_changed: was_open,
                ..ModalAxActions::default()
            }
        }
    };
    Some(actions)
}

/// Sync the native confirm-dialog card (same contract as
/// [`sync_command_palette`]).
pub(crate) fn sync_confirm_dialog(
    window: &Window,
    cache: &mut NativeOverlayCache,
    model: Option<(&ConfirmDialogSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let Some(ax) = confirm_sync_decision(cache, model, colors) else {
        return;
    };
    imp::rebuild_confirm(window, model, colors, &ax);
}

pub(super) fn title_prompt_sync_decision(
    cache: &mut NativeOverlayCache,
    model: Option<(&str, PaneRectPt)>,
    colors: &OverlayColors,
) -> Option<ModalAxActions> {
    let hash = model.map(|(input, rect)| {
        hash_u64(|h| {
            input.hash(h);
            rect.hash_into(h);
            colors.hash_into(h);
        })
    });
    if cache.title_prompt == hash {
        return None;
    }
    cache.title_prompt = hash;
    let actions = match model {
        Some((input, _)) => {
            let was_open = std::mem::replace(&mut cache.title_prompt_open, true);
            if !was_open {
                let heading = if input.is_empty() {
                    "Set Tab Title".to_string()
                } else {
                    format!("Set Tab Title: {input}")
                };
                ModalAxActions {
                    focus_modal: true,
                    layout_changed: true,
                    announcement: Some((
                        format_dialog_announcement(&heading, TITLE_PROMPT_HINT),
                        true,
                    )),
                }
            } else {
                ModalAxActions::default()
            }
        }
        None => {
            let was_open = std::mem::replace(&mut cache.title_prompt_open, false);
            ModalAxActions {
                layout_changed: was_open,
                ..ModalAxActions::default()
            }
        }
    };
    Some(actions)
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
    let Some(ax) = title_prompt_sync_decision(cache, model, colors) else {
        return;
    };
    imp::rebuild_title_prompt(window, model, colors, &ax);
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

/// Sync the child-exited bars of a window's panes: all of them are rebuilt
/// when any changes, and an empty slice removes them.
pub(crate) fn sync_child_exited_bars(
    window: &Window,
    cache: &mut NativeOverlayCache,
    bars: &[ChildExitedBar],
) {
    let hash = child_exited_bars_hash(bars);
    if cache.child_exited == hash {
        return;
    }
    cache.child_exited = hash;
    imp::rebuild_child_exited_bars(window, bars);
}
