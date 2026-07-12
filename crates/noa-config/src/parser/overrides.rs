use std::path::Path;

use crate::{ConfigOverrides, FontConfig, KeybindConfig};

use super::diagnostics::{
    invalid_value_diagnostic, unknown_key_diagnostic, window_pair_diagnostic,
};
use super::includes::{SourcedDirective, expand_directives};
use super::values::*;
use super::{Diagnostic, Directive};

pub fn parse_overrides(path: &Path, source: &str) -> (ConfigOverrides, Vec<Diagnostic>) {
    let (directives, mut diagnostics) = expand_directives(path, source);
    let (overrides, mut build_diagnostics) = build_overrides(&directives);
    diagnostics.append(&mut build_diagnostics);
    (overrides, diagnostics)
}

pub(crate) fn build_overrides(
    directives: &[SourcedDirective],
) -> (ConfigOverrides, Vec<Diagnostic>) {
    let mut cols = None;
    let mut rows = None;
    let mut font_size = None;
    let mut theme = None;
    let mut theme_appearance = None;
    let mut font = FontConfig::default();
    let mut palette = Vec::new();
    let mut clipboard_read = None;
    let mut clipboard_paste_protection = None;
    let mut confirm_quit = None;
    let mut title_report = None;
    let mut window_padding_x = None;
    let mut window_padding_y = None;
    let mut background = None;
    let mut foreground = None;
    let mut cursor_color = None;
    let mut selection_foreground = None;
    let mut selection_background = None;
    let mut minimum_contrast = None;
    let mut cursor_style = None;
    let mut cursor_style_blink = None;
    let mut background_opacity = None;
    let mut background_blur_radius = None;
    let mut background_image = None;
    let mut background_image_opacity = None;
    let mut background_image_position = None;
    let mut background_image_fit = None;
    let mut background_image_repeat = None;
    let mut background_image_interval_secs = None;
    let mut scrollback_limit = None;
    let mut image_storage_limit = None;
    let mut window_save_state = None;
    let mut macos_option_as_alt = None;
    let mut macos_titlebar_style = None;
    let mut macos_non_native_fullscreen = None;
    let mut macos_titlebar_proxy_icon = None;
    let mut macos_applescript = None;
    let mut quick_terminal_hotkey = None;
    let mut quick_terminal_size = None;
    let mut quick_terminal_autohide = None;
    let mut quick_terminal_screen = None;
    let mut quick_terminal_position = None;
    let mut quick_terminal_animation_duration = None;
    let mut sidebar_enabled = None;
    let mut sidebar_width = None;
    let mut sidebar_hotkey = None;
    let mut sidebar_preview_lines = None;
    let mut resize_overlay = None;
    let mut visual_bell = None;
    let mut audible_bell = None;
    let mut audible_bell_when_unfocused = None;
    let mut audible_bell_dock_bounce = None;
    let mut auto_approve = None;
    let mut keybinds = Vec::new();
    let mut server_enable = None;
    let mut server_port = None;
    let mut server_bind = None;
    let mut server_token = None;
    let mut server_scopes = None;
    let mut diagnostics = Vec::new();
    let mut window_pair_path = std::path::PathBuf::new();

    for sourced in directives {
        let path = sourced.path.as_path();
        let directive = &sourced.directive;
        match directive.key.as_str() {
            "window-width" => {
                window_pair_path = path.to_path_buf();
                cols = parse_u16(path, directive, &mut diagnostics);
            }
            "window-height" => {
                window_pair_path = path.to_path_buf();
                rows = parse_u16(path, directive, &mut diagnostics);
            }
            "font-size" => {
                font_size = parse_font_size(path, directive, &mut diagnostics);
            }
            "theme" => match parse_theme(path, directive, &mut diagnostics) {
                Some(ThemeSetting::Single(name)) => theme = Some(name),
                Some(ThemeSetting::Pair(pair)) => theme_appearance = Some(pair),
                None => {}
            },
            "font-family" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families);
            }
            "font-family-bold" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families_bold);
            }
            "font-family-italic" => {
                parse_family(path, directive, &mut diagnostics, &mut font.families_italic);
            }
            "font-family-bold-italic" => {
                parse_family(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.families_bold_italic,
                );
            }
            "font-feature" => {
                parse_font_feature(path, directive, &mut diagnostics, &mut font.features);
            }
            "font-variation" => {
                parse_font_variation(path, directive, &mut diagnostics, &mut font.variations);
            }
            "font-variation-bold" => {
                parse_font_variation(path, directive, &mut diagnostics, &mut font.variations_bold);
            }
            "font-variation-italic" => {
                parse_font_variation(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.variations_italic,
                );
            }
            "font-variation-bold-italic" => {
                parse_font_variation(
                    path,
                    directive,
                    &mut diagnostics,
                    &mut font.variations_bold_italic,
                );
            }
            "font-synthetic-style" => {
                font.synthetic_style = parse_synthetic_style(path, directive, &mut diagnostics);
            }
            "alpha-blending" => {
                font.alpha_blending = parse_alpha_blending(path, directive, &mut diagnostics);
            }
            "font-thicken" => {
                font.thicken = parse_font_thicken(path, directive, &mut diagnostics);
            }
            "font-thicken-strength" => {
                font.thicken_strength =
                    parse_font_thicken_strength(path, directive, &mut diagnostics);
            }
            "clipboard-read" => {
                clipboard_read = parse_clipboard_read(path, directive, &mut diagnostics);
            }
            "clipboard-paste-protection" => {
                clipboard_paste_protection =
                    parse_bool_directive(path, directive, &mut diagnostics);
            }
            "confirm-quit" => {
                confirm_quit = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "title-report" => {
                title_report = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "window-padding-x" => {
                window_padding_x = parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "window-padding-y" => {
                window_padding_y = parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "background" => {
                background = parse_color(path, directive, &mut diagnostics);
            }
            "foreground" => {
                foreground = parse_color(path, directive, &mut diagnostics);
            }
            "cursor-color" => {
                cursor_color = parse_color(path, directive, &mut diagnostics);
            }
            "selection-foreground" => {
                selection_foreground = parse_color(path, directive, &mut diagnostics);
            }
            "selection-background" => {
                selection_background = parse_color(path, directive, &mut diagnostics);
            }
            "minimum-contrast" => {
                minimum_contrast = parse_minimum_contrast(path, directive, &mut diagnostics);
            }
            "cursor-style" => {
                cursor_style = parse_cursor_style(path, directive, &mut diagnostics);
            }
            "cursor-style-blink" => {
                cursor_style_blink = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "background-opacity" => {
                background_opacity = parse_opacity(path, directive, &mut diagnostics);
            }
            "background-blur-radius" => {
                background_blur_radius = parse_blur_radius(path, directive, &mut diagnostics);
            }
            "background-image" => {
                background_image = parse_background_image(directive);
            }
            "background-image-opacity" => {
                background_image_opacity = parse_opacity(path, directive, &mut diagnostics);
            }
            "background-image-position" => {
                background_image_position =
                    parse_background_image_position(path, directive, &mut diagnostics);
            }
            "background-image-fit" => {
                background_image_fit =
                    parse_background_image_fit(path, directive, &mut diagnostics);
            }
            "background-image-repeat" => {
                background_image_repeat = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "background-image-interval" => {
                background_image_interval_secs =
                    parse_background_image_interval(path, directive, &mut diagnostics);
            }
            "scrollback-limit" => {
                scrollback_limit = parse_usize(path, directive, &mut diagnostics);
            }
            "image-storage-limit" => {
                image_storage_limit = parse_usize(path, directive, &mut diagnostics);
            }
            "window-save-state" => {
                window_save_state = parse_window_save_state(path, directive, &mut diagnostics);
            }
            "macos-option-as-alt" => {
                macos_option_as_alt = parse_macos_option_as_alt(path, directive, &mut diagnostics);
            }
            "macos-titlebar-style" => {
                macos_titlebar_style =
                    parse_macos_titlebar_style(path, directive, &mut diagnostics);
            }
            "macos-non-native-fullscreen" => {
                macos_non_native_fullscreen =
                    parse_bool_directive(path, directive, &mut diagnostics);
            }
            "macos-titlebar-proxy-icon" => {
                macos_titlebar_proxy_icon =
                    parse_macos_titlebar_proxy_icon(path, directive, &mut diagnostics);
            }
            "macos-applescript" => {
                macos_applescript = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "quick-terminal-hotkey" => {
                // `none`/`off`/`false`/empty explicitly disable the hotkey,
                // normalized to the empty-string sentinel so it overrides the
                // built-in default through the `.or()` merge.
                quick_terminal_hotkey = Some(match directive.value.as_deref() {
                    None => String::new(),
                    Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                        "" | "none" | "off" | "false" => String::new(),
                        _ => value.to_string(),
                    },
                });
            }
            "quick-terminal-size" => {
                quick_terminal_size = parse_quick_terminal_size(path, directive, &mut diagnostics);
            }
            "quick-terminal-autohide" => {
                quick_terminal_autohide = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "quick-terminal-screen" => {
                quick_terminal_screen =
                    parse_quick_terminal_screen(path, directive, &mut diagnostics);
            }
            "quick-terminal-position" => {
                quick_terminal_position =
                    parse_quick_terminal_position(path, directive, &mut diagnostics);
            }
            "quick-terminal-animation-duration" => {
                quick_terminal_animation_duration =
                    parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "sidebar-enabled" => {
                sidebar_enabled = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "sidebar-width" => {
                sidebar_width = parse_non_negative_f32(path, directive, &mut diagnostics);
            }
            "sidebar-hotkey" => {
                // Mirror `quick-terminal-hotkey`: the chord is stored verbatim
                // for the app-layer parser to interpret; `none`/`off`/`false`/
                // empty normalize to the empty-string sentinel that disables it.
                sidebar_hotkey = Some(match directive.value.as_deref() {
                    None => String::new(),
                    Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                        "" | "none" | "off" | "false" => String::new(),
                        _ => value.to_string(),
                    },
                });
            }
            "sidebar-preview-lines" => {
                sidebar_preview_lines =
                    parse_sidebar_preview_lines(path, directive, &mut diagnostics);
            }
            "resize-overlay" => {
                resize_overlay = parse_resize_overlay(path, directive, &mut diagnostics);
            }
            "visual-bell" => {
                visual_bell = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "audible-bell" => {
                audible_bell = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "audible-bell-when-unfocused" => {
                audible_bell_when_unfocused =
                    parse_bool_directive(path, directive, &mut diagnostics);
            }
            "audible-bell-dock-bounce" => {
                audible_bell_dock_bounce = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "auto-approve" => {
                auto_approve = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "keybind" => {
                if let Some(keybind) = parse_keybind_config(path, directive, &mut diagnostics) {
                    keybinds.push(keybind);
                }
            }
            "palette" => {
                parse_palette_entry(path, directive, &mut diagnostics, &mut palette);
            }
            "server-enable" => {
                server_enable = parse_bool_directive(path, directive, &mut diagnostics);
            }
            "server-port" => {
                server_port = parse_u16(path, directive, &mut diagnostics);
            }
            "server-bind" => {
                server_bind = parse_ip_addr_string(path, directive, &mut diagnostics);
            }
            "server-token" => {
                server_token = directive.value.clone();
            }
            "server-scopes" => {
                server_scopes = directive.value.clone();
            }
            unknown => {
                diagnostics.push(unknown_key_diagnostic(path, unknown));
            }
        }
    }

    if cols.is_some() ^ rows.is_some() {
        diagnostics.push(window_pair_diagnostic(&window_pair_path));
        cols = None;
        rows = None;
    } else if let (Some(width), Some(height)) = (cols, rows) {
        cols = Some(width.max(10));
        rows = Some(height.max(4));
    }

    (
        ConfigOverrides {
            cols,
            rows,
            font_size,
            theme,
            theme_appearance,
            font,
            palette,
            clipboard_read,
            clipboard_paste_protection,
            confirm_quit,
            title_report,
            window_padding_x,
            window_padding_y,
            background,
            foreground,
            cursor_color,
            selection_foreground,
            selection_background,
            minimum_contrast,
            cursor_style,
            cursor_style_blink,
            background_opacity,
            background_blur_radius,
            background_image,
            background_image_opacity,
            background_image_position,
            background_image_fit,
            background_image_repeat,
            background_image_interval_secs,
            scrollback_limit,
            image_storage_limit,
            window_save_state,
            macos_option_as_alt,
            macos_titlebar_style,
            macos_non_native_fullscreen,
            macos_titlebar_proxy_icon,
            macos_applescript,
            quick_terminal_hotkey,
            quick_terminal_size,
            quick_terminal_autohide,
            quick_terminal_screen,
            quick_terminal_position,
            quick_terminal_animation_duration,
            sidebar_enabled,
            sidebar_width,
            sidebar_hotkey,
            sidebar_preview_lines,
            resize_overlay,
            visual_bell,
            audible_bell,
            audible_bell_when_unfocused,
            audible_bell_dock_bounce,
            auto_approve,
            keybinds,
            server_enable,
            server_port,
            server_bind,
            server_token,
            server_scopes,
        },
        diagnostics,
    )
}

fn parse_keybind_config(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<KeybindConfig> {
    let Some(value) = directive.value.as_deref().map(str::trim) else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, ""));
        return None;
    };
    if value.eq_ignore_ascii_case("clear") {
        return Some(KeybindConfig::Clear);
    }

    let Some((trigger, action)) = value.split_once('=') else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    let trigger = trigger.trim();
    let action = action.trim();
    if trigger.is_empty() || action.is_empty() {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    if action.eq_ignore_ascii_case("unbind") {
        return Some(KeybindConfig::Unbind {
            trigger: trigger.to_string(),
        });
    }
    Some(KeybindConfig::Bind {
        trigger: trigger.to_string(),
        action: action.to_string(),
    })
}

pub(crate) fn is_supported_scalar_key(key: &str) -> bool {
    matches!(
        key,
        "window-width"
            | "window-height"
            | "font-size"
            | "theme"
            | "clipboard-read"
            | "clipboard-paste-protection"
            | "confirm-quit"
            | "title-report"
            | "window-padding-x"
            | "window-padding-y"
            | "background"
            | "foreground"
            | "cursor-color"
            | "selection-foreground"
            | "selection-background"
            | "minimum-contrast"
            | "cursor-style"
            | "cursor-style-blink"
            | "alpha-blending"
            | "background-opacity"
            | "background-blur-radius"
            | "background-image"
            | "background-image-opacity"
            | "background-image-position"
            | "background-image-fit"
            | "background-image-repeat"
            | "background-image-interval"
            | "scrollback-limit"
            | "image-storage-limit"
            | "window-save-state"
            | "macos-option-as-alt"
            | "macos-titlebar-style"
            | "macos-non-native-fullscreen"
            | "macos-titlebar-proxy-icon"
            | "macos-applescript"
            | "quick-terminal-hotkey"
            | "quick-terminal-size"
            | "quick-terminal-autohide"
            | "quick-terminal-screen"
            | "quick-terminal-position"
            | "quick-terminal-animation-duration"
            | "sidebar-enabled"
            | "sidebar-width"
            | "sidebar-hotkey"
            | "sidebar-preview-lines"
            | "resize-overlay"
            | "visual-bell"
            | "audible-bell"
            | "audible-bell-when-unfocused"
            | "audible-bell-dock-bounce"
            | "auto-approve"
    )
}
