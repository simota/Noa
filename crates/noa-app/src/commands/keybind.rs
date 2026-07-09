use super::command::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};
use super::key_token::{KeyTrigger, KeybindParseError};
use crate::split_tree::Direction;
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
}
