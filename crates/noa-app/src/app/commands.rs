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
        #[cfg(target_os = "macos")]
        if self
            .text_panel
            .as_ref()
            .is_some_and(|panel| panel.handle_command(command))
        {
            return;
        }
        if overview_should_intercept_command(command, self.overview_visible, origin) {
            return;
        }
        if self.handle_copy_mode_before_app_command(command) {
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
        let remote_ui_loading = self
            .remote_ui
            .as_ref()
            .is_some_and(|session| session.is_loading());
        if should_dismiss_remote_ui(command, remote_ui_loading) {
            self.remote_ui = None;
        }
        // scratch-terminal R4-d/R6: while the scratch popup is focused,
        // `cmd+w` closes it (instead of the normal tab-close path, which
        // assumes a `window_order`-tracked tab) and window/tab-management
        // commands are no-ops — the popup has no splits/tabs/sidebar to act
        // on. Everything else (terminal-level commands, the toggle itself,
        // app-level commands like Quit) passes through unchanged.
        if let Some(window_id) = self.focused
            && self.is_scratch_terminal_window(window_id)
        {
            if command == AppCommand::CloseTab {
                self.destroy_scratch_terminal();
                return;
            }
            if super::scratch_terminal::scratch_terminal_command_blocked(command) {
                return;
            }
        }
        // floating-panes REQ-020: a float has no splits to act on.
        if command.is_split_action()
            && self
                .focused
                .and_then(|window_id| self.windows.get(&window_id))
                .is_some_and(|state| state.is_float(state.focused_pane))
        {
            return;
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
            AppCommand::EditConfigFileInNewWindow => {
                self.edit_config_file_in_new_window(event_loop)
            }
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
            AppCommand::Float(action) => {
                if let Some(window_id) = self.focused {
                    self.handle_float_action(window_id, action);
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
            AppCommand::MoveTabToNewWindow => self.move_tab_to_new_window(event_loop),
            AppCommand::MoveTab(amount) => self.move_tab(amount),
            AppCommand::Copy => {
                if !self.copy_search_prompt_query()
                    && !self.copy_theme_settings_background_image_to_clipboard()
                {
                    self.copy_selection_to_clipboard();
                }
            }
            AppCommand::Paste => {
                let pasted_to_overlay = self.paste_clipboard_to_search_prompt()
                    || self.focused.is_some_and(|window_id| {
                        self.paste_clipboard_to_theme_settings_background_image(window_id)
                    });
                if !pasted_to_overlay {
                    self.paste_clipboard_to_pty();
                }
            }
            AppCommand::AttachRemote => self.begin_attach_remote(),
            AppCommand::SendSelectionToPane => self.open_send_selection_picker(),
            AppCommand::ExportScrollback => self.export_scrollback_to_temp_file(),
            AppCommand::DiscardRestoredHistory => self.discard_focused_pane_record(),
            AppCommand::CheckpointScrollback => self.checkpoint_scrollback_now(),
            AppCommand::PipeScrollbackToPager => self.pipe_scrollback_to_pager(event_loop),
            AppCommand::Terminal(action) => self.handle_terminal_action(action),
            AppCommand::FontSize(action) => self.handle_font_size_action(action),
            AppCommand::Search(action) => self.handle_search_action(action),
            AppCommand::ScrollViewport(scroll) => self.scroll_viewport(scroll),
            AppCommand::CopyMode(action) => {
                if let Some(window_id) = self.focused {
                    self.start_copy_mode(window_id, action);
                }
            }
            AppCommand::ToggleCommandPalette => self.toggle_command_palette(),
            AppCommand::OpenThemePicker => self.open_theme_settings(ThemeSettingsMode::Theme),
            AppCommand::OpenSettings => self.open_theme_settings(ThemeSettingsMode::Settings),
            AppCommand::ToggleProcessMonitor => self.toggle_process_monitor(),
            AppCommand::NextNotification => self.focus_next_notification(),
            AppCommand::ComposePrompt => self.open_text_panel(true),
            AppCommand::ReadOutput => self.open_text_panel(false),
            AppCommand::ToggleFullscreen => self.toggle_fullscreen(),
            AppCommand::ToggleQuickTerminal => self.toggle_quick_terminal(event_loop),
            AppCommand::ToggleScratchTerminal => self.toggle_scratch_terminal(event_loop),
            AppCommand::ToggleSecureKeyboardEntry => self.toggle_secure_keyboard_entry(),
            AppCommand::ToggleBackgroundOpacity => self.toggle_background_opacity(),
            AppCommand::ToggleSidebar => self.toggle_sidebar(),
            AppCommand::ToggleAutoApprove => self.toggle_auto_approve(),
            AppCommand::ToggleEmber => {
                #[cfg(target_os = "macos")]
                self.toggle_ember();
            }
            AppCommand::CloseWindow => self.request_close_window(event_loop),
            AppCommand::Quit => self.request_quit(event_loop),
        }
        self.reconcile_copy_mode_after_app_command();
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
                palette: CommandPalette::open(self.palette_entries.clone()),
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

    /// Ghostty's `open_config:new_window`: run `$VISUAL` / `$EDITOR` (else
    /// `vi`) on the config file in a new window titled after it. Ghostty
    /// does this only on GTK; its macOS app opens the file the OS way
    /// whichever target is asked for, and so does this on macOS.
    fn edit_config_file_in_new_window(&mut self, event_loop: &ActiveEventLoop) {
        if cfg!(target_os = "macos") {
            crate::app_actions::open_config_file();
            return;
        }
        let Some(path) = crate::app_actions::config_file_for_editing() else {
            return;
        };
        // The command line is UTF-8 text; a lossy path would edit some
        // other file.
        if path.to_str().is_none() {
            log::warn!("cannot edit the config in a new window: its path is not UTF-8");
            return;
        }
        let command = crate::app_actions::config_edit_command(
            &path,
            std::env::var("VISUAL").ok(),
            std::env::var("EDITOR").ok(),
        );
        // The next spawned surface runs this instead of the shell, the way
        // the CLI's `-e` command does.
        *self.initial_command.lock() = Some(vec!["/bin/sh".to_string(), "-c".to_string(), command]);
        let spawned = self.spawn_tab(event_loop, SpawnTarget::NewWindow);
        // A spawn that failed before the surface leaves it for the next tab.
        self.initial_command.lock().take();
        let window_id = match spawned {
            Ok(window_id) => window_id,
            Err(err) => {
                log::warn!("failed to open a window to edit the config: {err:#}");
                return;
            }
        };
        if let Some(state) = self.windows.get_mut(&window_id) {
            let pane = state.focused_pane;
            if let Some(surface) = state.surfaces.get_mut(&pane) {
                surface.title_override =
                    Some(format!("Editing configuration file {}", path.display()));
            }
        }
        self.refresh_window_title(window_id);
    }

    /// Ghostty's `toggle_background_opacity`: hold every terminal window's
    /// background opaque, or let `background-opacity` show again. Like
    /// Ghostty, it does nothing when the configuration is already opaque or
    /// the focused window is fullscreen, and it is implemented only on macOS.
    fn toggle_background_opacity(&mut self) {
        if !cfg!(target_os = "macos") {
            log::warn!("toggle_background_opacity is only implemented on macOS");
            return;
        }
        if self.config.background_opacity >= 1.0 {
            return;
        }
        let Some(state) = self.focused.and_then(|id| self.windows.get(&id)) else {
            return;
        };
        if state.window.fullscreen().is_some() {
            return;
        }
        self.background_opaque = !self.background_opaque;
        self.apply_reloaded_background_opacity();
        for state in self.windows.values() {
            state.window.request_redraw();
        }
        self.request_sidebar_redraw();
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

fn should_dismiss_remote_ui(command: AppCommand, loading: bool) -> bool {
    command != AppCommand::AttachRemote && !loading
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_commands_keep_loading_remote_requests_owned() {
        assert!(!should_dismiss_remote_ui(AppCommand::NewTab, true));
        assert!(should_dismiss_remote_ui(AppCommand::NewTab, false));
        assert!(!should_dismiss_remote_ui(AppCommand::AttachRemote, false));
    }
}
