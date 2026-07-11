use crate::macos_overlay::model::{
    OverlayColors, PaneRectPt, ProcessMonitorViewModel, ThemeSettingsViewModel,
};
use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot};
use winit::window::Window;

pub(in crate::macos_overlay) fn rebuild_palette(
    _: &Window,
    _: Option<(&CommandPaletteSnapshot, PaneRectPt)>,
    _: &OverlayColors,
) {
}
pub(in crate::macos_overlay) fn rebuild_theme_settings(
    _: &Window,
    _: Option<(ThemeSettingsViewModel, PaneRectPt)>,
    _: &OverlayColors,
) {
}
pub(in crate::macos_overlay) fn rebuild_process_monitor(
    _: &Window,
    _: Option<(ProcessMonitorViewModel, PaneRectPt)>,
    _: &OverlayColors,
) {
}
pub(in crate::macos_overlay) fn rebuild_confirm(
    _: &Window,
    _: Option<(&ConfirmDialogSnapshot, PaneRectPt)>,
    _: &OverlayColors,
) {
}
pub(in crate::macos_overlay) fn rebuild_title_prompt(
    _: &Window,
    _: Option<(&str, PaneRectPt)>,
    _: &OverlayColors,
) {
}
pub(in crate::macos_overlay) fn rebuild_toast(_: &Window, _: Option<&str>, _: &OverlayColors) {}
