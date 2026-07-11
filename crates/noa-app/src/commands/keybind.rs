use super::command::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};
use super::key_token::{KeyTrigger, KeybindParseError};
use crate::split_tree::Direction;
use noa_config::KeybindConfig;
use winit::keyboard::{Key, ModifiersState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyBinding {
    trigger: KeyTrigger,
    command: AppCommand,
}

impl KeyBinding {
    pub(crate) fn parse(trigger: &str, command: AppCommand) -> Result<Self, KeybindParseError> {
        Ok(Self {
            trigger: KeyTrigger::parse(trigger)?,
            command,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeybindEngine {
    pub(super) bindings: Vec<KeyBinding>,
}

impl Default for KeybindEngine {
    fn default() -> Self {
        let specs = [
            ("cmd+q", AppCommand::Quit),
            ("cmd+t", AppCommand::NewTab),
            ("cmd+n", AppCommand::NewWindow),
            ("cmd+d", AppCommand::NewSplitRight),
            ("cmd+shift+d", AppCommand::NewSplitDown),
            ("cmd+w", AppCommand::CloseTab),
            ("cmd+shift+w", AppCommand::CloseWindow),
            ("cmd+1", AppCommand::SelectTab(1)),
            ("cmd+2", AppCommand::SelectTab(2)),
            ("cmd+3", AppCommand::SelectTab(3)),
            ("cmd+4", AppCommand::SelectTab(4)),
            ("cmd+5", AppCommand::SelectTab(5)),
            ("cmd+6", AppCommand::SelectTab(6)),
            ("cmd+7", AppCommand::SelectTab(7)),
            ("cmd+8", AppCommand::SelectTab(8)),
            ("cmd+9", AppCommand::SelectTab(9)),
            ("cmd+shift+]", AppCommand::NextTab),
            ("cmd+shift+[", AppCommand::PrevTab),
            ("cmd+c", AppCommand::Copy),
            ("cmd+v", AppCommand::Paste),
            ("cmd+shift+m", AppCommand::SendSelectionToPane),
            ("cmd+k", AppCommand::Terminal(TerminalAction::Clear)),
            ("cmd+a", AppCommand::Terminal(TerminalAction::SelectAll)),
            ("cmd+=", AppCommand::FontSize(FontSizeAction::Increase)),
            (
                "cmd+shift+plus",
                AppCommand::FontSize(FontSizeAction::Increase),
            ),
            ("cmd+-", AppCommand::FontSize(FontSizeAction::Decrease)),
            ("cmd+0", AppCommand::FontSize(FontSizeAction::Reset)),
            ("cmd+f", AppCommand::Search(SearchAction::Find)),
            ("cmd+g", AppCommand::Search(SearchAction::FindNext)),
            (
                "cmd+shift+g",
                AppCommand::Search(SearchAction::FindPrevious),
            ),
            (
                "shift+arrowup",
                AppCommand::ScrollViewport(ViewportScroll::LineUp),
            ),
            (
                "shift+arrowdown",
                AppCommand::ScrollViewport(ViewportScroll::LineDown),
            ),
            (
                "shift+pageup",
                AppCommand::ScrollViewport(ViewportScroll::PageUp),
            ),
            (
                "shift+pagedown",
                AppCommand::ScrollViewport(ViewportScroll::PageDown),
            ),
            (
                "shift+home",
                AppCommand::ScrollViewport(ViewportScroll::Top),
            ),
            (
                "shift+end",
                AppCommand::ScrollViewport(ViewportScroll::Bottom),
            ),
            (
                "cmd+arrowup",
                AppCommand::ScrollViewport(ViewportScroll::PrevPrompt),
            ),
            (
                "cmd+arrowdown",
                AppCommand::ScrollViewport(ViewportScroll::NextPrompt),
            ),
            (
                "cmd+ctrl+arrowleft",
                AppCommand::FocusDirection(Direction::Left),
            ),
            (
                "cmd+ctrl+arrowright",
                AppCommand::FocusDirection(Direction::Right),
            ),
            (
                "cmd+ctrl+arrowup",
                AppCommand::FocusDirection(Direction::Up),
            ),
            (
                "cmd+ctrl+arrowdown",
                AppCommand::FocusDirection(Direction::Down),
            ),
            (
                "cmd+alt+arrowleft",
                AppCommand::FocusDirection(Direction::Left),
            ),
            (
                "cmd+alt+arrowright",
                AppCommand::FocusDirection(Direction::Right),
            ),
            ("cmd+alt+arrowup", AppCommand::FocusDirection(Direction::Up)),
            (
                "cmd+alt+arrowdown",
                AppCommand::FocusDirection(Direction::Down),
            ),
            (
                "cmd+ctrl+shift+arrowleft",
                AppCommand::ResizeSplit(Direction::Left),
            ),
            (
                "cmd+ctrl+shift+arrowright",
                AppCommand::ResizeSplit(Direction::Right),
            ),
            (
                "cmd+ctrl+shift+arrowup",
                AppCommand::ResizeSplit(Direction::Up),
            ),
            (
                "cmd+ctrl+shift+arrowdown",
                AppCommand::ResizeSplit(Direction::Down),
            ),
            ("cmd+ctrl+=", AppCommand::EqualizeSplits),
            ("cmd+shift+enter", AppCommand::ToggleSplitZoom),
            ("cmd+shift+o", AppCommand::ToggleTabOverview),
            ("cmd+ctrl+f", AppCommand::ToggleFullscreen),
            ("cmd+shift+p", AppCommand::ToggleCommandPalette),
            ("cmd+shift+s", AppCommand::ToggleSidebar),
            // R-24: default chord for the theme picker half of the split
            // overlay (verified unused in this list before adding it).
            ("cmd+shift+,", AppCommand::OpenThemePicker),
        ];
        let bindings = specs
            .into_iter()
            .map(|(trigger, command)| {
                KeyBinding::parse(trigger, command).expect("default keybind should parse")
            })
            .collect();
        Self { bindings }
    }
}

impl KeybindEngine {
    pub(crate) fn from_config(configs: &[KeybindConfig]) -> (Self, Vec<String>) {
        let mut engine = Self::default();
        let mut diagnostics = Vec::new();

        for config in configs {
            match config {
                KeybindConfig::Clear => engine.bindings.clear(),
                KeybindConfig::Unbind { trigger } => match KeyTrigger::parse(trigger) {
                    Ok(trigger) => engine.remove_trigger(&trigger),
                    Err(error) => diagnostics.push(format!(
                        "invalid keybind `{}`: {error}; value ignored",
                        config.config_value()
                    )),
                },
                KeybindConfig::Bind { trigger, action } => {
                    let Some(command) = command_from_keybind_action(action) else {
                        diagnostics.push(format!(
                            "unknown keybind action `{action}` in `{}`; value ignored",
                            config.config_value()
                        ));
                        continue;
                    };
                    let trigger = match KeyTrigger::parse(trigger) {
                        Ok(trigger) => trigger,
                        Err(error) => {
                            diagnostics.push(format!(
                                "invalid keybind `{}`: {error}; value ignored",
                                config.config_value()
                            ));
                            continue;
                        }
                    };
                    engine.remove_trigger(&trigger);
                    engine.bindings.push(KeyBinding { trigger, command });
                }
            }
        }

        (engine, diagnostics)
    }

    pub(crate) fn resolve(&self, logical_key: &Key, mods: ModifiersState) -> Option<AppCommand> {
        self.bindings
            .iter()
            .find(|binding| binding.trigger.matches(logical_key, mods))
            .map(|binding| binding.command)
    }

    /// The chord text (e.g. `"cmd+shift+p"`) of the first binding for
    /// `command`, or `None` when it is unbound. Reverse of [`Self::resolve`],
    /// used for the command palette's keybind hints (R-4) — the engine stays
    /// the single source of truth rather than a duplicated hint table.
    /// "First" is deterministic: bindings keep their `default()` order.
    pub(crate) fn chord_for(&self, command: AppCommand) -> Option<String> {
        self.bindings
            .iter()
            .find(|binding| binding.command == command)
            .map(|binding| binding.trigger.to_string())
    }

    /// `(chord, action-name)` pairs for every binding, in `default()` order.
    /// Backs the `+list-keybinds` CLI action (cli.rs); like
    /// [`Self::chord_for`], the engine stays the single source of truth for
    /// the effective binding set.
    pub(crate) fn list(&self) -> Vec<(String, &'static str)> {
        self.bindings
            .iter()
            .map(|binding| (binding.trigger.to_string(), binding.command.action_name()))
            .collect()
    }

    fn remove_trigger(&mut self, trigger: &KeyTrigger) {
        self.bindings.retain(|binding| binding.trigger != *trigger);
    }
}

/// The closed `perform action` set for the AppleScript bridge (applescript
/// R-8/L2): only these action names are accepted; everything else yields
/// `errAEEventNotHandled`. Reuses [`AppCommand`] variants (no new commands),
/// deliberately narrower than the keybind vocabulary so scripting cannot reach
/// commands outside the ratified table.
pub(crate) fn command_from_applescript_action(action: &str) -> Option<AppCommand> {
    match action {
        "new_tab" => Some(AppCommand::NewTab),
        "new_window" => Some(AppCommand::NewWindow),
        "new_split:right" => Some(AppCommand::NewSplitRight),
        "new_split:left" => Some(AppCommand::NewSplitLeft),
        "new_split:up" => Some(AppCommand::NewSplitUp),
        "new_split:down" => Some(AppCommand::NewSplitDown),
        "close_tab" => Some(AppCommand::CloseTab),
        "close_window" => Some(AppCommand::CloseWindow),
        "next_tab" => Some(AppCommand::NextTab),
        "previous_tab" => Some(AppCommand::PrevTab),
        "toggle_fullscreen" => Some(AppCommand::ToggleFullscreen),
        "copy_to_clipboard" => Some(AppCommand::Copy),
        "paste_from_clipboard" => Some(AppCommand::Paste),
        "reload_config" => Some(AppCommand::ReloadConfig),
        "quit" => Some(AppCommand::Quit),
        _ => action
            .strip_prefix("goto_tab:")
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| (1..=9).contains(index))
            .map(AppCommand::SelectTab),
    }
}

fn command_from_keybind_action(action: &str) -> Option<AppCommand> {
    let action = action.trim();
    AppCommand::from_action_name(action)
        .or_else(|| AppCommand::from_action_name(&action.replace('_', "-")))
        .or_else(|| ghostty_action_alias(action))
}

fn ghostty_action_alias(action: &str) -> Option<AppCommand> {
    match action {
        "new_tab" => Some(AppCommand::NewTab),
        "new_window" => Some(AppCommand::NewWindow),
        "close_tab" | "close_surface" => Some(AppCommand::CloseTab),
        "close_window" => Some(AppCommand::CloseWindow),
        "quit" => Some(AppCommand::Quit),
        "copy_to_clipboard" => Some(AppCommand::Copy),
        "paste_from_clipboard" => Some(AppCommand::Paste),
        "send_selection_to_pane" => Some(AppCommand::SendSelectionToPane),
        "clear_screen" | "clear_terminal" => Some(AppCommand::Terminal(TerminalAction::Clear)),
        "select_all" => Some(AppCommand::Terminal(TerminalAction::SelectAll)),
        "increase_font_size" => Some(AppCommand::FontSize(FontSizeAction::Increase)),
        "decrease_font_size" => Some(AppCommand::FontSize(FontSizeAction::Decrease)),
        "reset_font_size" => Some(AppCommand::FontSize(FontSizeAction::Reset)),
        "find" => Some(AppCommand::Search(SearchAction::Find)),
        "find_next" => Some(AppCommand::Search(SearchAction::FindNext)),
        "find_previous" => Some(AppCommand::Search(SearchAction::FindPrevious)),
        "new_split:left" => Some(AppCommand::NewSplitLeft),
        "new_split:right" => Some(AppCommand::NewSplitRight),
        "new_split:up" => Some(AppCommand::NewSplitUp),
        "new_split:down" => Some(AppCommand::NewSplitDown),
        "focus_split:left" | "goto_split:left" => Some(AppCommand::FocusDirection(Direction::Left)),
        "focus_split:right" | "goto_split:right" => {
            Some(AppCommand::FocusDirection(Direction::Right))
        }
        "focus_split:up" | "goto_split:up" => Some(AppCommand::FocusDirection(Direction::Up)),
        "focus_split:down" | "goto_split:down" => Some(AppCommand::FocusDirection(Direction::Down)),
        "resize_split:left" => Some(AppCommand::ResizeSplit(Direction::Left)),
        "resize_split:right" => Some(AppCommand::ResizeSplit(Direction::Right)),
        "resize_split:up" => Some(AppCommand::ResizeSplit(Direction::Up)),
        "resize_split:down" => Some(AppCommand::ResizeSplit(Direction::Down)),
        "equalize_splits" => Some(AppCommand::EqualizeSplits),
        "toggle_split_zoom" => Some(AppCommand::ToggleSplitZoom),
        "toggle_tab_overview" | "toggle_session_overview" => Some(AppCommand::ToggleTabOverview),
        "toggle_fullscreen" => Some(AppCommand::ToggleFullscreen),
        "next_tab" => Some(AppCommand::NextTab),
        "previous_tab" | "prev_tab" => Some(AppCommand::PrevTab),
        "prompt_surface_title" | "set_tab_title" => Some(AppCommand::SetTabTitle),
        "toggle_command_palette" => Some(AppCommand::ToggleCommandPalette),
        "toggle_quick_terminal" => Some(AppCommand::ToggleQuickTerminal),
        "toggle_secure_keyboard_entry" => Some(AppCommand::ToggleSecureKeyboardEntry),
        "toggle_sidebar" => Some(AppCommand::ToggleSidebar),
        "toggle_auto_approve" => Some(AppCommand::ToggleAutoApprove),
        // Legacy: the combined overlay's Ghostty-style action name. Kept as
        // an alias for `OpenThemePicker` (the theme-picker half) so existing
        // user keybind configs keep working after the split (DEC-1).
        "open_theme_settings" | "open_theme" => Some(AppCommand::OpenThemePicker),
        "open_settings" => Some(AppCommand::OpenSettings),
        _ => action
            .strip_prefix("goto_tab:")
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| (1..=9).contains(index))
            .map(AppCommand::SelectTab),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, ModifiersState};

    #[test]
    fn configured_keybinds_override_unbind_and_clear_in_order() {
        let (engine, diagnostics) = KeybindEngine::from_config(&[
            KeybindConfig::Bind {
                trigger: "cmd+t".to_string(),
                action: "new_window".to_string(),
            },
            KeybindConfig::Unbind {
                trigger: "cmd+w".to_string(),
            },
            KeybindConfig::Bind {
                trigger: "cmd+i".to_string(),
                action: "prompt_surface_title".to_string(),
            },
        ]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        assert_eq!(
            engine.resolve(&Key::Character("t".into()), ModifiersState::SUPER),
            Some(AppCommand::NewWindow)
        );
        assert_eq!(
            engine.resolve(&Key::Character("w".into()), ModifiersState::SUPER),
            None
        );
        assert_eq!(
            engine.resolve(&Key::Character("i".into()), ModifiersState::SUPER),
            Some(AppCommand::SetTabTitle)
        );

        let (engine, diagnostics) = KeybindEngine::from_config(&[
            KeybindConfig::Clear,
            KeybindConfig::Bind {
                trigger: "cmd+i".to_string(),
                action: "tab.set-title".to_string(),
            },
        ]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            engine.resolve(&Key::Character("t".into()), ModifiersState::SUPER),
            None
        );
        assert_eq!(
            engine.list(),
            vec![("cmd+i".to_string(), AppCommand::SetTabTitle.action_name())]
        );
    }

    #[test]
    fn invalid_configured_keybinds_do_not_remove_existing_bindings() {
        let (engine, diagnostics) = KeybindEngine::from_config(&[
            KeybindConfig::Bind {
                trigger: "cmd+t".to_string(),
                action: "no.such.action".to_string(),
            },
            KeybindConfig::Bind {
                trigger: "cmd+not-a-key".to_string(),
                action: "window.new".to_string(),
            },
            KeybindConfig::Unbind {
                trigger: "cmd+not-a-key".to_string(),
            },
        ]);

        assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
        assert_eq!(
            engine.resolve(&Key::Character("t".into()), ModifiersState::SUPER),
            Some(AppCommand::NewTab)
        );
    }

    #[test]
    fn grave_aliases_bind_quick_terminal() {
        for trigger in ["cmd+grave", "cmd+backtick", "cmd+`"] {
            let (engine, diagnostics) = KeybindEngine::from_config(&[
                KeybindConfig::Clear,
                KeybindConfig::Bind {
                    trigger: trigger.to_string(),
                    action: "quick-terminal.toggle".to_string(),
                },
            ]);

            assert!(diagnostics.is_empty(), "{trigger}: {diagnostics:?}");
            assert_eq!(
                engine.resolve(&Key::Character("`".into()), ModifiersState::SUPER),
                Some(AppCommand::ToggleQuickTerminal),
                "{trigger} should bind Cmd+`"
            );
            assert_eq!(
                engine.list(),
                vec![(
                    "cmd+grave".to_string(),
                    AppCommand::ToggleQuickTerminal.action_name()
                )]
            );
        }
    }

    // AC-33 (R-24): the default engine resolves `cmd+shift+,` to
    // `OpenThemePicker`.
    #[test]
    fn default_engine_binds_cmd_shift_comma_to_open_theme_picker() {
        let engine = KeybindEngine::default();
        assert_eq!(
            engine.resolve(
                &Key::Character(",".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::OpenThemePicker)
        );
    }

    // DEC-1: a config keybind still using the pre-split combined overlay's
    // action name (Ghostty-style `open_theme_settings`, or its dotted
    // `theme-settings.open` id) keeps binding — to the theme-picker half,
    // since that's what the old combined overlay opened focused on.
    #[test]
    fn legacy_combined_overlay_action_names_alias_to_open_theme_picker() {
        for action in ["open_theme_settings", "theme-settings.open"] {
            let (engine, diagnostics) = KeybindEngine::from_config(&[
                KeybindConfig::Clear,
                KeybindConfig::Bind {
                    trigger: "cmd+shift+t".to_string(),
                    action: action.to_string(),
                },
            ]);
            assert!(diagnostics.is_empty(), "{action}: {diagnostics:?}");
            assert_eq!(
                engine.resolve(
                    &Key::Character("t".into()),
                    ModifiersState::SUPER | ModifiersState::SHIFT
                ),
                Some(AppCommand::OpenThemePicker),
                "{action} should still bind"
            );
        }
    }
}
