//! Live config reload: file polling, reload command, and runtime apply paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::*;

const CONFIG_WATCH_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(super) struct ConfigWatcher {
    path: Option<PathBuf>,
    signature: Option<ConfigFileSignature>,
    next_check: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigFileSignature {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigWatchTick {
    Inactive,
    Waiting(Instant),
    Changed(Instant),
}

impl ConfigWatcher {
    pub(super) fn new() -> Self {
        Self::with_path(noa_config::default_config_path())
    }

    fn with_path(path: Option<PathBuf>) -> Self {
        let signature = path.as_deref().and_then(config_file_signature);
        let next_check = path
            .as_ref()
            .map(|_| Instant::now() + CONFIG_WATCH_INTERVAL);
        Self {
            path,
            signature,
            next_check,
        }
    }

    fn tick(&mut self, now: Instant) -> ConfigWatchTick {
        let Some(path) = self.path.as_deref() else {
            return ConfigWatchTick::Inactive;
        };
        let Some(next_check) = self.next_check else {
            return ConfigWatchTick::Inactive;
        };
        if now < next_check {
            return ConfigWatchTick::Waiting(next_check);
        }

        let next = now + CONFIG_WATCH_INTERVAL;
        self.next_check = Some(next);
        let signature = config_file_signature(path);
        if signature != self.signature {
            self.signature = signature;
            ConfigWatchTick::Changed(next)
        } else {
            ConfigWatchTick::Waiting(next)
        }
    }

    fn mark_current(&mut self) {
        if let Some(path) = self.path.as_deref() {
            self.signature = config_file_signature(path);
        }
    }
}

fn config_file_signature(path: &Path) -> Option<ConfigFileSignature> {
    let metadata = fs::metadata(path).ok()?;
    metadata.is_file().then_some(ConfigFileSignature {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

impl App {
    pub(super) fn tick_config_watch(&mut self) -> Option<Instant> {
        match self.config_watcher.tick(Instant::now()) {
            ConfigWatchTick::Inactive => None,
            ConfigWatchTick::Waiting(deadline) => Some(deadline),
            ConfigWatchTick::Changed(deadline) => {
                self.reload_config_from_disk();
                Some(deadline)
            }
        }
    }

    pub(in crate::app) fn reload_config_from_disk(&mut self) {
        let cli_overrides = self.config.cli_overrides.clone();
        let (startup, diagnostics) = match noa_config::load_startup_config(cli_overrides.clone()) {
            Ok(loaded) => loaded,
            Err(error) => {
                log::warn!("config reload failed: {error:#}");
                return;
            }
        };
        for diagnostic in diagnostics {
            log::warn!("config reload: {}", diagnostic.message);
        }

        let next = AppConfig::from_startup(startup, self.config.cli_grid_override, cli_overrides);
        self.apply_reloaded_config(next);
        self.config_watcher.mark_current();
        log::info!("config reloaded");
    }

    fn apply_reloaded_config(&mut self, next: AppConfig) {
        let previous = self.config.clone();
        let mut applied = next;

        let font_changed = previous.font != applied.font || previous.font_size != applied.font_size;
        let font_applied = if font_changed {
            self.rebuild_runtime_fonts(&applied.font, applied.font_size)
        } else {
            false
        };
        if font_changed && !font_applied {
            applied.font = previous.font.clone();
            applied.font_size = previous.font_size;
        }

        let padding_changed = previous.window_padding_x != applied.window_padding_x
            || previous.window_padding_y != applied.window_padding_y;
        let theme_changed = theme_inputs_changed(&previous, &applied);
        let cursor_changed = previous.cursor_style != applied.cursor_style
            || previous.cursor_style_blink != applied.cursor_style_blink;
        let background_image_changed = background_image_inputs_changed(&previous, &applied);
        let opacity_changed = previous.background_opacity != applied.background_opacity;
        let blur_changed = previous.background_blur_radius != applied.background_blur_radius;
        let terminal_policy_changed = terminal_policy_inputs_changed(&previous, &applied);
        let sidebar_preview_changed =
            previous.sidebar_preview_lines != applied.sidebar_preview_lines;
        let keybinds_changed = previous.keybinds != applied.keybinds;

        self.config = applied;

        if padding_changed {
            self.padding =
                resolve_grid_padding(self.config.window_padding_x, self.config.window_padding_y);
            for state in self.windows.values_mut() {
                state.renderer.set_grid_padding(self.padding);
            }
        }

        if theme_changed {
            self.apply_reloaded_theme();
        }
        if background_image_changed {
            self.apply_reloaded_background_image();
        }
        if opacity_changed {
            self.apply_reloaded_background_opacity();
        }
        if blur_changed || opacity_changed {
            self.apply_reloaded_background_blur();
        }
        if cursor_changed {
            self.apply_reloaded_cursor_style();
        }
        if terminal_policy_changed {
            self.apply_reloaded_terminal_policies();
        }
        if sidebar_preview_changed {
            self.sidebar_preview_lines_gate
                .store(self.config.sidebar_preview_lines, Ordering::Relaxed);
            self.request_sidebar_redraw();
        }
        if keybinds_changed {
            let (keybinds, diagnostics) = KeybindEngine::from_config(&self.config.keybinds);
            for diagnostic in diagnostics {
                log::warn!("config reload keybind: {diagnostic}");
            }
            self.keybinds = keybinds;
            self.request_all_windows_redraw();
            self.request_overview_redraw();
        }

        if font_applied || padding_changed || self.config.sidebar_width != previous.sidebar_width {
            self.relayout_all_windows();
        } else if theme_changed || background_image_changed || opacity_changed || blur_changed {
            self.request_all_windows_redraw();
        }
    }

    fn rebuild_runtime_fonts(&mut self, font: &noa_config::FontConfig, point_size: f32) -> bool {
        let point_size = clamp_runtime_font_size(point_size);
        let scale_factor = self
            .focused
            .or_else(|| self.window_order.first().copied())
            .and_then(|window_id| {
                self.windows
                    .get(&window_id)
                    .map(|state| state.window.scale_factor())
            })
            .unwrap_or(1.0);

        let runtime_font = match FontGrid::new(
            font_pixel_size(point_size, scale_factor),
            font_config_from_noa_config(font),
        ) {
            Ok(font) => font,
            Err(err) => {
                log::warn!(
                    "config reload: failed to rebuild font for size {point_size} at scale factor {scale_factor}: {err}"
                );
                return false;
            }
        };
        let sidebar_font = match FontGrid::new(
            sidebar_font_pixel_size(scale_factor),
            font_config_from_noa_config(font),
        ) {
            Ok(font) => font,
            Err(err) => {
                log::warn!(
                    "config reload: failed to rebuild sidebar font at scale factor {scale_factor}: {err}"
                );
                return false;
            }
        };

        self.runtime_font_size = point_size;
        let Some(gpu) = self.gpu.as_mut() else {
            return true;
        };
        gpu.font = runtime_font;
        gpu.sidebar_font = sidebar_font;
        for state in self.windows.values_mut() {
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        }
        true
    }

    fn apply_reloaded_theme(&mut self) {
        let overrides = theme_overrides_for_config(&self.config);
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        gpu.theme =
            crate::theme::resolve_theme_with_overrides(self.config.theme.as_deref(), &overrides);
        gpu.preview_theme = None;
        crate::chrome::select_palette(gpu.theme.is_light());
        gpu.chrome_textures.reset();

        let default_fg = gpu.theme.default_fg;
        let default_bg = gpu.theme.default_bg;
        let cursor = gpu.theme.cursor;
        let palette = gpu.theme.palette;
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                surface
                    .terminal
                    .lock()
                    .set_base_colors(default_fg, default_bg, cursor, palette);
            }
        }
        self.refresh_macos_window_backgrounds();
    }

    fn apply_reloaded_background_image(&mut self) {
        self.background_image = decode_background_image(&self.config);
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        for state in self.windows.values_mut() {
            state.renderer.set_background_image(
                &gpu.device,
                &gpu.queue,
                self.background_image.clone(),
            );
        }
    }

    fn apply_reloaded_background_opacity(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let transparent = self.config.background_opacity < 1.0;
        for state in self.windows.values_mut() {
            let caps = state.surface.get_capabilities(&gpu.adapter);
            let alpha_mode = preferred_surface_alpha_mode(&caps, transparent);
            if state.surface_config.alpha_mode != alpha_mode {
                state.surface_config.alpha_mode = alpha_mode;
                configure_wgpu_surface(
                    &state.surface,
                    &gpu.device,
                    &state.surface_config,
                    state.occluded,
                );
            }
            state
                .renderer
                .set_background_opacity(self.config.background_opacity);
        }
        self.refresh_macos_window_backgrounds();
    }

    fn apply_reloaded_background_blur(&self) {
        for state in self.windows.values() {
            crate::macos_blur::apply_background_blur(
                &state.window,
                self.config.background_blur_radius,
                self.config.background_opacity,
            );
        }
    }

    fn apply_reloaded_cursor_style(&mut self) {
        self.initial_cursor_style =
            resolve_cursor_style(self.config.cursor_style, self.config.cursor_style_blink);
        let style = self.initial_cursor_style.unwrap_or_default();
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                surface.terminal.lock().set_default_cursor_style(style);
            }
        }
        self.reset_cursor_blink_phase();
    }

    fn apply_reloaded_terminal_policies(&mut self) {
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                let mut terminal = surface.terminal.lock();
                terminal.osc52_policy.allow_read =
                    self.config.clipboard_read != noa_config::ClipboardAccess::Deny;
                terminal.title_report = self.config.title_report;
                terminal.set_scrollback_limit_bytes(self.config.scrollback_limit);
            }
        }
    }

    fn relayout_all_windows(&mut self) {
        let windows = self
            .windows
            .iter()
            .map(|(id, state)| (*id, state.window.clone()))
            .collect::<Vec<_>>();
        for (window_id, _) in &windows {
            self.relayout_and_resize_window(*window_id);
        }
        for (_, window) in windows {
            window.request_redraw();
        }
    }

    fn request_all_windows_redraw(&self) {
        for state in self.windows.values() {
            state.window.request_redraw();
        }
        self.request_overview_redraw();
    }

    #[cfg(target_os = "macos")]
    fn refresh_macos_window_backgrounds(&self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        for state in self.windows.values() {
            crate::macos_window::set_window_background_color(
                &state.window,
                gpu.theme.default_bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&state.window, gpu.theme.default_bg);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn refresh_macos_window_backgrounds(&self) {}
}

fn theme_inputs_changed(previous: &AppConfig, next: &AppConfig) -> bool {
    previous.theme != next.theme
        || previous.background != next.background
        || previous.foreground != next.foreground
        || previous.cursor_color != next.cursor_color
        || previous.selection_foreground != next.selection_foreground
        || previous.selection_background != next.selection_background
        || previous.minimum_contrast != next.minimum_contrast
}

fn background_image_inputs_changed(previous: &AppConfig, next: &AppConfig) -> bool {
    previous.background_image != next.background_image
        || previous.background_image_opacity != next.background_image_opacity
        || previous.background_image_position != next.background_image_position
        || previous.background_image_fit != next.background_image_fit
        || previous.background_image_repeat != next.background_image_repeat
}

fn terminal_policy_inputs_changed(previous: &AppConfig, next: &AppConfig) -> bool {
    previous.clipboard_read != next.clipboard_read
        || previous.title_report != next.title_report
        || previous.scrollback_limit != next.scrollback_limit
}

fn theme_overrides_for_config(config: &AppConfig) -> crate::theme::ThemeOverrides {
    crate::theme::ThemeOverrides {
        background: config.background,
        foreground: config.foreground,
        cursor: config.cursor_color,
        selection_fg: config.selection_foreground,
        selection_bg: config.selection_background,
        minimum_contrast: config.minimum_contrast,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noa-config-watch-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn watcher_detects_file_create_update_and_delete() {
        let path = temp_config_path("config");
        let mut watcher = ConfigWatcher::with_path(Some(path.clone()));
        let due = Instant::now() + CONFIG_WATCH_INTERVAL;

        assert!(matches!(watcher.tick(due), ConfigWatchTick::Waiting(_)));

        fs::write(&path, "font-size = 15\n").unwrap();
        assert!(matches!(
            watcher.tick(due + CONFIG_WATCH_INTERVAL),
            ConfigWatchTick::Changed(_)
        ));

        fs::write(&path, "font-size = 16\nbackground-opacity = 0.9\n").unwrap();
        assert!(matches!(
            watcher.tick(due + CONFIG_WATCH_INTERVAL * 2),
            ConfigWatchTick::Changed(_)
        ));

        fs::remove_file(&path).unwrap();
        assert!(matches!(
            watcher.tick(due + CONFIG_WATCH_INTERVAL * 3),
            ConfigWatchTick::Changed(_)
        ));
    }

    #[test]
    fn changed_input_helpers_are_narrow() {
        let base = AppConfig::from_startup(
            noa_config::StartupConfig::default(),
            false,
            noa_config::ConfigOverrides::default(),
        );
        let mut themed = base.clone();
        themed.cursor_color = Some(noa_core::Rgb::new(1, 2, 3));
        assert!(theme_inputs_changed(&base, &themed));
        assert!(!background_image_inputs_changed(&base, &themed));

        let mut image = base.clone();
        image.background_image_opacity = 0.5;
        assert!(background_image_inputs_changed(&base, &image));
        assert!(!terminal_policy_inputs_changed(&base, &image));
    }
}
