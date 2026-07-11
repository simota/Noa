//! App command dispatch and app-scoped toggles.

use super::*;
use crate::theme_settings::ThemeSettingsMode;

impl App {
    pub(in crate::app) fn command_is_enabled(
        &self,
        window_id: WindowId,
        command: AppCommand,
    ) -> bool {
        match command {
            AppCommand::NewSplitLeft => self.can_create_split_in_window(window_id, Direction::Left),
            AppCommand::NewSplitRight => {
                self.can_create_split_in_window(window_id, Direction::Right)
            }
            AppCommand::NewSplitUp => self.can_create_split_in_window(window_id, Direction::Up),
            AppCommand::NewSplitDown => self.can_create_split_in_window(window_id, Direction::Down),
            _ => true,
        }
    }

    pub(in crate::app) fn can_create_split_in_window(
        &self,
        window_id: WindowId,
        direction: Direction,
    ) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| {
                let rect = state.focused_surface()?.rect;
                Some(
                    can_create_split_in_direction(state.pane_count(), rect, direction)
                        && can_add_pane_in_direction(
                            &state.split_tree,
                            state.focused_pane,
                            direction,
                        ),
                )
            })
            .unwrap_or(false)
    }

    pub(super) fn handle_app_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: AppCommand,
        origin: CommandOrigin,
    ) {
        if overview_should_intercept_command(command, self.overview_visible, origin) {
            return;
        }
        // C1 (FM1): dispatching any command means leaving the palette. Close
        // it here so a command routed around the palette's own Enter path —
        // notably a menu-bar click while the palette is open — can't leave
        // two modals owning the keyboard. Idempotent with the Enter-path
        // close; skipped for the toggle itself so re-pressing still works.
        if command != AppCommand::ToggleCommandPalette {
            self.command_palette = None;
        }
        if command != AppCommand::SendSelectionToPane {
            self.send_selection_picker = None;
        }
        match command {
            AppCommand::About => crate::app_actions::show_about(),
            // R-22: Cmd+, now opens the GUI settings overlay instead of the
            // raw config file — the menu item id/accelerator are unchanged
            // (AC-31), only this dispatch target moves.
            AppCommand::Preferences => self.open_theme_settings(ThemeSettingsMode::Settings),
            // R-23: the pre-R-22 behavior, kept reachable under its own
            // command identity.
            AppCommand::EditConfigFile => crate::app_actions::open_config_file(),
            AppCommand::ReloadConfig => self.reload_config_from_disk(),
            AppCommand::NewTab => {
                if let Err(err) = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow) {
                    log::warn!("failed to spawn new tab: {err:#}");
                }
            }
            AppCommand::NewWindow => {
                if let Err(err) = self.spawn_tab(event_loop, SpawnTarget::NewWindow) {
                    log::warn!("failed to spawn new window: {err:#}");
                }
            }
            AppCommand::NewSplitLeft => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, Direction::Left);
                }
            }
            AppCommand::NewSplitRight => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, Direction::Right);
                }
            }
            AppCommand::NewSplitUp => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, Direction::Up);
                }
            }
            AppCommand::NewSplitDown => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, Direction::Down);
                }
            }
            AppCommand::FocusDirection(direction) => {
                if let Some(window_id) = self.focused {
                    self.focus_split_direction(window_id, direction);
                }
            }
            AppCommand::ResizeSplit(direction) => {
                if let Some(window_id) = self.focused {
                    self.resize_focused_split(window_id, direction);
                }
            }
            AppCommand::EqualizeSplits => {
                if let Some(window_id) = self.focused {
                    self.equalize_splits(window_id);
                }
            }
            AppCommand::ToggleSplitZoom => {
                if let Some(window_id) = self.focused {
                    self.toggle_split_zoom(window_id);
                }
            }
            AppCommand::ToggleTabOverview => self.toggle_tab_overview(),
            AppCommand::CloseTab => {
                if let Some(window_id) = self.focused {
                    self.request_close_focused_pane_or_tab(event_loop, window_id);
                }
            }
            AppCommand::SelectTab(index) => self.select_tab(index),
            AppCommand::SetTabTitle => self.open_tab_title_prompt(),
            AppCommand::NextTab => self.select_next_tab(),
            AppCommand::PrevTab => self.select_previous_tab(),
            AppCommand::Copy => {
                if !self.copy_theme_settings_background_image_to_clipboard() {
                    self.copy_selection_to_clipboard();
                }
            }
            AppCommand::Paste => {
                let pasted_to_theme_settings = self.focused.is_some_and(|window_id| {
                    self.paste_clipboard_to_theme_settings_background_image(window_id)
                });
                if !pasted_to_theme_settings {
                    self.paste_clipboard_to_pty();
                }
            }
            AppCommand::SendSelectionToPane => self.open_send_selection_picker(),
            AppCommand::ExportScrollback => self.export_scrollback_to_temp_file(),
            AppCommand::PipeScrollbackToPager => self.pipe_scrollback_to_pager(event_loop),
            AppCommand::Terminal(action) => self.handle_terminal_action(action),
            AppCommand::FontSize(action) => self.handle_font_size_action(action),
            AppCommand::Search(action) => self.handle_search_action(action),
            AppCommand::ScrollViewport(scroll) => self.scroll_viewport(scroll),
            AppCommand::ToggleCommandPalette => self.toggle_command_palette(),
            AppCommand::OpenThemePicker => self.open_theme_settings(ThemeSettingsMode::Theme),
            AppCommand::OpenSettings => self.open_theme_settings(ThemeSettingsMode::Settings),
            AppCommand::ToggleFullscreen => self.toggle_fullscreen(),
            AppCommand::ToggleQuickTerminal => self.toggle_quick_terminal(event_loop),
            AppCommand::ToggleSecureKeyboardEntry => self.toggle_secure_keyboard_entry(),
            AppCommand::ToggleSidebar => self.toggle_sidebar(),
            AppCommand::ToggleAutoApprove => self.toggle_auto_approve(),
            AppCommand::CloseWindow => self.request_close_window(event_loop),
            AppCommand::Quit => self.request_quit(event_loop),
        }
    }

    /// Toggle the single app-wide command palette (R-5). Opening binds it to
    /// the focused window with an empty query and every entry shown;
    /// re-firing while open closes it. A no-op when there is no focused
    /// window to bind to.
    fn toggle_command_palette(&mut self) {
        if self.command_palette.is_some() {
            self.command_palette = None;
        } else if let Some(window_id) = self.focused
            && self.active_overlay(window_id) == ActiveOverlay::None
        {
            self.command_palette = Some(CommandPaletteSession {
                window_id,
                palette: CommandPalette::open(),
                opened_at: Instant::now(),
            });
        }
        if let Some(window_id) = self.focused
            && let Some(state) = self.windows.get(&window_id)
        {
            state.window.request_redraw();
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };

        #[cfg(target_os = "macos")]
        if !self.config.macos_non_native_fullscreen
            && crate::macos_window::toggle_native_fullscreen(&state.window)
        {
            return;
        }

        toggle_borderless_fullscreen(&state.window);
    }

    /// Toggle Secure Keyboard Entry. A toggle only reaches us while the app is
    /// frontmost, so the switch takes effect immediately; focus changes and app
    /// exit reconcile it afterwards. The menu checkmark tracks the user intent.
    fn toggle_secure_keyboard_entry(&mut self) {
        let desired = self
            .secure_input
            .toggle(true, &mut crate::secure_input::CarbonSecureInput);
        #[cfg(target_os = "macos")]
        if let Some(menu) = self.macos_menu.as_ref() {
            menu.set_secure_keyboard_entry_checked(desired);
        }
        let _ = desired;
    }
}

fn toggle_borderless_fullscreen(window: &Window) {
    let fullscreen = if window.fullscreen().is_some() {
        None
    } else {
        Some(winit::window::Fullscreen::Borderless(
            window.current_monitor(),
        ))
    };
    window.set_fullscreen(fullscreen);
}
