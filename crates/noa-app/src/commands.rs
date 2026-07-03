//! App-level commands that must not be encoded as terminal input.

use crate::split_tree::Direction;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Commands handled by the application layer rather than the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Preferences,
    Copy,
    Paste,
    Terminal(TerminalAction),
    FontSize(FontSizeAction),
    Search(SearchAction),
    ScrollViewport(ViewportScroll),
    NewTab,
    NewSplitRight,
    NewSplitDown,
    FocusDirection(Direction),
    ResizeSplit(Direction),
    EqualizeSplits,
    ToggleSplitZoom,
    ToggleTabOverview,
    CloseTab,
    SelectTab(usize),
    NextTab,
    PrevTab,
    CloseWindow,
    Quit,
    ToggleCommandPalette,
}

/// Terminal-state commands handled by noa instead of sending escape bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAction {
    Clear,
    ClearScrollback,
    SelectAll,
}

/// Runtime font-size commands for the shared application font state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSizeAction {
    Increase,
    Decrease,
    Reset,
}

/// Search commands that can be triggered before a full search UI exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchAction {
    Find,
    FindNext,
    FindPrevious,
    Clear,
}

/// Local scrollback navigation that moves noa's viewport instead of the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportScroll {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

impl AppCommand {
    pub(crate) const ABOUT_MENU_ID: &'static str = "noa.app.about";
    pub(crate) const PREFERENCES_MENU_ID: &'static str = "noa.app.preferences";
    pub(crate) const COPY_MENU_ID: &'static str = "noa.edit.copy";
    pub(crate) const PASTE_MENU_ID: &'static str = "noa.edit.paste";
    pub(crate) const TERMINAL_SELECT_ALL_MENU_ID: &'static str = "noa.edit.select-all";
    pub(crate) const SEARCH_FIND_MENU_ID: &'static str = "noa.edit.find";
    pub(crate) const SEARCH_FIND_NEXT_MENU_ID: &'static str = "noa.edit.find-next";
    pub(crate) const SEARCH_FIND_PREVIOUS_MENU_ID: &'static str = "noa.edit.find-previous";
    pub(crate) const SEARCH_CLEAR_MENU_ID: &'static str = "noa.edit.clear-search";
    pub(crate) const TERMINAL_CLEAR_MENU_ID: &'static str = "noa.view.clear";
    pub(crate) const TERMINAL_CLEAR_SCROLLBACK_MENU_ID: &'static str = "noa.view.clear-scrollback";
    pub(crate) const FONT_SIZE_INCREASE_MENU_ID: &'static str = "noa.view.font-size-increase";
    pub(crate) const FONT_SIZE_DECREASE_MENU_ID: &'static str = "noa.view.font-size-decrease";
    pub(crate) const FONT_SIZE_RESET_MENU_ID: &'static str = "noa.view.font-size-reset";
    pub(crate) const SCROLL_LINE_UP_MENU_ID: &'static str = "noa.view.scroll-line-up";
    pub(crate) const SCROLL_LINE_DOWN_MENU_ID: &'static str = "noa.view.scroll-line-down";
    pub(crate) const SCROLL_PAGE_UP_MENU_ID: &'static str = "noa.view.scroll-page-up";
    pub(crate) const SCROLL_PAGE_DOWN_MENU_ID: &'static str = "noa.view.scroll-page-down";
    pub(crate) const SCROLL_TOP_MENU_ID: &'static str = "noa.view.scroll-top";
    pub(crate) const SCROLL_BOTTOM_MENU_ID: &'static str = "noa.view.scroll-bottom";
    pub(crate) const NEW_TAB_MENU_ID: &'static str = "noa.file.new-tab";
    pub(crate) const NEW_SPLIT_RIGHT_MENU_ID: &'static str = "noa.file.new-split-right";
    pub(crate) const NEW_SPLIT_DOWN_MENU_ID: &'static str = "noa.file.new-split-down";
    pub(crate) const FOCUS_SPLIT_LEFT_MENU_ID: &'static str = "noa.split.focus-left";
    pub(crate) const FOCUS_SPLIT_RIGHT_MENU_ID: &'static str = "noa.split.focus-right";
    pub(crate) const FOCUS_SPLIT_UP_MENU_ID: &'static str = "noa.split.focus-up";
    pub(crate) const FOCUS_SPLIT_DOWN_MENU_ID: &'static str = "noa.split.focus-down";
    pub(crate) const RESIZE_SPLIT_LEFT_MENU_ID: &'static str = "noa.split.resize-left";
    pub(crate) const RESIZE_SPLIT_RIGHT_MENU_ID: &'static str = "noa.split.resize-right";
    pub(crate) const RESIZE_SPLIT_UP_MENU_ID: &'static str = "noa.split.resize-up";
    pub(crate) const RESIZE_SPLIT_DOWN_MENU_ID: &'static str = "noa.split.resize-down";
    pub(crate) const EQUALIZE_SPLITS_MENU_ID: &'static str = "noa.split.equalize";
    pub(crate) const TOGGLE_SPLIT_ZOOM_MENU_ID: &'static str = "noa.split.toggle-zoom";
    pub(crate) const TOGGLE_TAB_OVERVIEW_MENU_ID: &'static str = "noa.view.toggle-tab-overview";
    pub(crate) const CLOSE_TAB_MENU_ID: &'static str = "noa.file.close-tab";
    pub(crate) const NEXT_TAB_MENU_ID: &'static str = "noa.window.next-tab";
    pub(crate) const PREV_TAB_MENU_ID: &'static str = "noa.window.previous-tab";
    pub(crate) const CLOSE_WINDOW_MENU_ID: &'static str = "noa.app.close-window";
    pub(crate) const QUIT_MENU_ID: &'static str = "noa.app.quit";
    pub(crate) const TOGGLE_COMMAND_PALETTE_MENU_ID: &'static str =
        "noa.view.toggle-command-palette";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::About => Self::ABOUT_MENU_ID,
            AppCommand::Preferences => Self::PREFERENCES_MENU_ID,
            AppCommand::Copy => Self::COPY_MENU_ID,
            AppCommand::Paste => Self::PASTE_MENU_ID,
            AppCommand::Terminal(TerminalAction::Clear) => Self::TERMINAL_CLEAR_MENU_ID,
            AppCommand::Terminal(TerminalAction::ClearScrollback) => {
                Self::TERMINAL_CLEAR_SCROLLBACK_MENU_ID
            }
            AppCommand::Terminal(TerminalAction::SelectAll) => Self::TERMINAL_SELECT_ALL_MENU_ID,
            AppCommand::FontSize(FontSizeAction::Increase) => Self::FONT_SIZE_INCREASE_MENU_ID,
            AppCommand::FontSize(FontSizeAction::Decrease) => Self::FONT_SIZE_DECREASE_MENU_ID,
            AppCommand::FontSize(FontSizeAction::Reset) => Self::FONT_SIZE_RESET_MENU_ID,
            AppCommand::Search(SearchAction::Find) => Self::SEARCH_FIND_MENU_ID,
            AppCommand::Search(SearchAction::FindNext) => Self::SEARCH_FIND_NEXT_MENU_ID,
            AppCommand::Search(SearchAction::FindPrevious) => Self::SEARCH_FIND_PREVIOUS_MENU_ID,
            AppCommand::Search(SearchAction::Clear) => Self::SEARCH_CLEAR_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::LineUp) => Self::SCROLL_LINE_UP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::LineDown) => Self::SCROLL_LINE_DOWN_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::PageUp) => Self::SCROLL_PAGE_UP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::PageDown) => Self::SCROLL_PAGE_DOWN_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::Top) => Self::SCROLL_TOP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::Bottom) => Self::SCROLL_BOTTOM_MENU_ID,
            AppCommand::NewTab => Self::NEW_TAB_MENU_ID,
            AppCommand::NewSplitRight => Self::NEW_SPLIT_RIGHT_MENU_ID,
            AppCommand::NewSplitDown => Self::NEW_SPLIT_DOWN_MENU_ID,
            AppCommand::FocusDirection(Direction::Left) => Self::FOCUS_SPLIT_LEFT_MENU_ID,
            AppCommand::FocusDirection(Direction::Right) => Self::FOCUS_SPLIT_RIGHT_MENU_ID,
            AppCommand::FocusDirection(Direction::Up) => Self::FOCUS_SPLIT_UP_MENU_ID,
            AppCommand::FocusDirection(Direction::Down) => Self::FOCUS_SPLIT_DOWN_MENU_ID,
            AppCommand::ResizeSplit(Direction::Left) => Self::RESIZE_SPLIT_LEFT_MENU_ID,
            AppCommand::ResizeSplit(Direction::Right) => Self::RESIZE_SPLIT_RIGHT_MENU_ID,
            AppCommand::ResizeSplit(Direction::Up) => Self::RESIZE_SPLIT_UP_MENU_ID,
            AppCommand::ResizeSplit(Direction::Down) => Self::RESIZE_SPLIT_DOWN_MENU_ID,
            AppCommand::EqualizeSplits => Self::EQUALIZE_SPLITS_MENU_ID,
            AppCommand::ToggleSplitZoom => Self::TOGGLE_SPLIT_ZOOM_MENU_ID,
            AppCommand::ToggleTabOverview => Self::TOGGLE_TAB_OVERVIEW_MENU_ID,
            AppCommand::CloseTab => Self::CLOSE_TAB_MENU_ID,
            AppCommand::SelectTab(_) => "",
            AppCommand::NextTab => Self::NEXT_TAB_MENU_ID,
            AppCommand::PrevTab => Self::PREV_TAB_MENU_ID,
            AppCommand::CloseWindow => Self::CLOSE_WINDOW_MENU_ID,
            AppCommand::Quit => Self::QUIT_MENU_ID,
            AppCommand::ToggleCommandPalette => Self::TOGGLE_COMMAND_PALETTE_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::ABOUT_MENU_ID => Some(Self::About),
            Self::PREFERENCES_MENU_ID => Some(Self::Preferences),
            Self::COPY_MENU_ID => Some(Self::Copy),
            Self::PASTE_MENU_ID => Some(Self::Paste),
            Self::TERMINAL_CLEAR_MENU_ID => Some(Self::Terminal(TerminalAction::Clear)),
            Self::TERMINAL_CLEAR_SCROLLBACK_MENU_ID => {
                Some(Self::Terminal(TerminalAction::ClearScrollback))
            }
            Self::TERMINAL_SELECT_ALL_MENU_ID => Some(Self::Terminal(TerminalAction::SelectAll)),
            Self::FONT_SIZE_INCREASE_MENU_ID => Some(Self::FontSize(FontSizeAction::Increase)),
            Self::FONT_SIZE_DECREASE_MENU_ID => Some(Self::FontSize(FontSizeAction::Decrease)),
            Self::FONT_SIZE_RESET_MENU_ID => Some(Self::FontSize(FontSizeAction::Reset)),
            Self::SEARCH_FIND_MENU_ID => Some(Self::Search(SearchAction::Find)),
            Self::SEARCH_FIND_NEXT_MENU_ID => Some(Self::Search(SearchAction::FindNext)),
            Self::SEARCH_FIND_PREVIOUS_MENU_ID => Some(Self::Search(SearchAction::FindPrevious)),
            Self::SEARCH_CLEAR_MENU_ID => Some(Self::Search(SearchAction::Clear)),
            Self::SCROLL_LINE_UP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::LineUp)),
            Self::SCROLL_LINE_DOWN_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::LineDown)),
            Self::SCROLL_PAGE_UP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::PageUp)),
            Self::SCROLL_PAGE_DOWN_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::PageDown)),
            Self::SCROLL_TOP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::Top)),
            Self::SCROLL_BOTTOM_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::Bottom)),
            Self::NEW_TAB_MENU_ID => Some(Self::NewTab),
            Self::NEW_SPLIT_RIGHT_MENU_ID => Some(Self::NewSplitRight),
            Self::NEW_SPLIT_DOWN_MENU_ID => Some(Self::NewSplitDown),
            Self::FOCUS_SPLIT_LEFT_MENU_ID => Some(Self::FocusDirection(Direction::Left)),
            Self::FOCUS_SPLIT_RIGHT_MENU_ID => Some(Self::FocusDirection(Direction::Right)),
            Self::FOCUS_SPLIT_UP_MENU_ID => Some(Self::FocusDirection(Direction::Up)),
            Self::FOCUS_SPLIT_DOWN_MENU_ID => Some(Self::FocusDirection(Direction::Down)),
            Self::RESIZE_SPLIT_LEFT_MENU_ID => Some(Self::ResizeSplit(Direction::Left)),
            Self::RESIZE_SPLIT_RIGHT_MENU_ID => Some(Self::ResizeSplit(Direction::Right)),
            Self::RESIZE_SPLIT_UP_MENU_ID => Some(Self::ResizeSplit(Direction::Up)),
            Self::RESIZE_SPLIT_DOWN_MENU_ID => Some(Self::ResizeSplit(Direction::Down)),
            Self::EQUALIZE_SPLITS_MENU_ID => Some(Self::EqualizeSplits),
            Self::TOGGLE_SPLIT_ZOOM_MENU_ID => Some(Self::ToggleSplitZoom),
            Self::TOGGLE_TAB_OVERVIEW_MENU_ID => Some(Self::ToggleTabOverview),
            Self::CLOSE_TAB_MENU_ID => Some(Self::CloseTab),
            Self::NEXT_TAB_MENU_ID => Some(Self::NextTab),
            Self::PREV_TAB_MENU_ID => Some(Self::PrevTab),
            Self::CLOSE_WINDOW_MENU_ID => Some(Self::CloseWindow),
            Self::QUIT_MENU_ID => Some(Self::Quit),
            Self::TOGGLE_COMMAND_PALETTE_MENU_ID => Some(Self::ToggleCommandPalette),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_cmd_character(character: &str) -> Option<Self> {
        KeybindEngine::default().resolve(
            &Key::Character(character.to_ascii_lowercase().into()),
            ModifiersState::SUPER,
        )
    }

    #[cfg(test)]
    pub(crate) fn from_key(logical_key: &Key, mods: ModifiersState) -> Option<Self> {
        KeybindEngine::default().resolve(logical_key, mods)
    }

    pub fn action_name(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::Preferences => "preferences",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::Terminal(TerminalAction::Clear) => "terminal.clear",
            Self::Terminal(TerminalAction::ClearScrollback) => "terminal.clear-scrollback",
            Self::Terminal(TerminalAction::SelectAll) => "terminal.select-all",
            Self::FontSize(FontSizeAction::Increase) => "font-size.increase",
            Self::FontSize(FontSizeAction::Decrease) => "font-size.decrease",
            Self::FontSize(FontSizeAction::Reset) => "font-size.reset",
            Self::Search(SearchAction::Find) => "search.find",
            Self::Search(SearchAction::FindNext) => "search.next",
            Self::Search(SearchAction::FindPrevious) => "search.previous",
            Self::Search(SearchAction::Clear) => "search.clear",
            Self::ScrollViewport(ViewportScroll::LineUp) => "scroll.line-up",
            Self::ScrollViewport(ViewportScroll::LineDown) => "scroll.line-down",
            Self::ScrollViewport(ViewportScroll::PageUp) => "scroll.page-up",
            Self::ScrollViewport(ViewportScroll::PageDown) => "scroll.page-down",
            Self::ScrollViewport(ViewportScroll::Top) => "scroll.top",
            Self::ScrollViewport(ViewportScroll::Bottom) => "scroll.bottom",
            Self::NewTab => "tab.new",
            Self::NewSplitRight => "split.new-right",
            Self::NewSplitDown => "split.new-down",
            Self::FocusDirection(Direction::Left) => "split.focus-left",
            Self::FocusDirection(Direction::Right) => "split.focus-right",
            Self::FocusDirection(Direction::Up) => "split.focus-up",
            Self::FocusDirection(Direction::Down) => "split.focus-down",
            Self::ResizeSplit(Direction::Left) => "split.resize-left",
            Self::ResizeSplit(Direction::Right) => "split.resize-right",
            Self::ResizeSplit(Direction::Up) => "split.resize-up",
            Self::ResizeSplit(Direction::Down) => "split.resize-down",
            Self::EqualizeSplits => "split.equalize",
            Self::ToggleSplitZoom => "split.toggle-zoom",
            Self::ToggleTabOverview => "tab-overview.toggle",
            Self::CloseTab => "tab.close",
            Self::SelectTab(index) => match index {
                1 => "tab.select-1",
                2 => "tab.select-2",
                3 => "tab.select-3",
                4 => "tab.select-4",
                5 => "tab.select-5",
                6 => "tab.select-6",
                7 => "tab.select-7",
                8 => "tab.select-8",
                9 => "tab.select-9",
                _ => "tab.select",
            },
            Self::NextTab => "tab.next",
            Self::PrevTab => "tab.previous",
            Self::CloseWindow => "window.close",
            Self::Quit => "app.quit",
            Self::ToggleCommandPalette => "command-palette.toggle",
        }
    }

    pub fn from_action_name(name: &str) -> Option<Self> {
        match name {
            "about" => Some(Self::About),
            "preferences" => Some(Self::Preferences),
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
            "terminal.clear" => Some(Self::Terminal(TerminalAction::Clear)),
            "terminal.clear-scrollback" => Some(Self::Terminal(TerminalAction::ClearScrollback)),
            "terminal.select-all" => Some(Self::Terminal(TerminalAction::SelectAll)),
            "font-size.increase" => Some(Self::FontSize(FontSizeAction::Increase)),
            "font-size.decrease" => Some(Self::FontSize(FontSizeAction::Decrease)),
            "font-size.reset" => Some(Self::FontSize(FontSizeAction::Reset)),
            "search.find" => Some(Self::Search(SearchAction::Find)),
            "search.next" => Some(Self::Search(SearchAction::FindNext)),
            "search.previous" => Some(Self::Search(SearchAction::FindPrevious)),
            "search.clear" => Some(Self::Search(SearchAction::Clear)),
            "scroll.line-up" => Some(Self::ScrollViewport(ViewportScroll::LineUp)),
            "scroll.line-down" => Some(Self::ScrollViewport(ViewportScroll::LineDown)),
            "scroll.page-up" => Some(Self::ScrollViewport(ViewportScroll::PageUp)),
            "scroll.page-down" => Some(Self::ScrollViewport(ViewportScroll::PageDown)),
            "scroll.top" => Some(Self::ScrollViewport(ViewportScroll::Top)),
            "scroll.bottom" => Some(Self::ScrollViewport(ViewportScroll::Bottom)),
            "tab.new" => Some(Self::NewTab),
            "split.new-right" => Some(Self::NewSplitRight),
            "split.new-down" => Some(Self::NewSplitDown),
            "split.focus-left" => Some(Self::FocusDirection(Direction::Left)),
            "split.focus-right" => Some(Self::FocusDirection(Direction::Right)),
            "split.focus-up" => Some(Self::FocusDirection(Direction::Up)),
            "split.focus-down" => Some(Self::FocusDirection(Direction::Down)),
            "split.resize-left" => Some(Self::ResizeSplit(Direction::Left)),
            "split.resize-right" => Some(Self::ResizeSplit(Direction::Right)),
            "split.resize-up" => Some(Self::ResizeSplit(Direction::Up)),
            "split.resize-down" => Some(Self::ResizeSplit(Direction::Down)),
            "split.equalize" => Some(Self::EqualizeSplits),
            "split.toggle-zoom" => Some(Self::ToggleSplitZoom),
            "tab-overview.toggle" => Some(Self::ToggleTabOverview),
            "tab.close" => Some(Self::CloseTab),
            "tab.select-1" => Some(Self::SelectTab(1)),
            "tab.select-2" => Some(Self::SelectTab(2)),
            "tab.select-3" => Some(Self::SelectTab(3)),
            "tab.select-4" => Some(Self::SelectTab(4)),
            "tab.select-5" => Some(Self::SelectTab(5)),
            "tab.select-6" => Some(Self::SelectTab(6)),
            "tab.select-7" => Some(Self::SelectTab(7)),
            "tab.select-8" => Some(Self::SelectTab(8)),
            "tab.select-9" => Some(Self::SelectTab(9)),
            "tab.next" => Some(Self::NextTab),
            "tab.previous" => Some(Self::PrevTab),
            "window.close" => Some(Self::CloseWindow),
            "app.quit" => Some(Self::Quit),
            "command-palette.toggle" => Some(Self::ToggleCommandPalette),
            _ => None,
        }
    }
}

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
    bindings: Vec<KeyBinding>,
}

impl Default for KeybindEngine {
    fn default() -> Self {
        let specs = [
            ("cmd+q", AppCommand::Quit),
            ("cmd+t", AppCommand::NewTab),
            ("cmd+d", AppCommand::NewSplitRight),
            ("cmd+shift+d", AppCommand::NewSplitDown),
            ("cmd+w", AppCommand::CloseTab),
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
                "cmd+ctrl+arrowleft",
                AppCommand::ResizeSplit(Direction::Left),
            ),
            (
                "cmd+ctrl+arrowright",
                AppCommand::ResizeSplit(Direction::Right),
            ),
            ("cmd+ctrl+arrowup", AppCommand::ResizeSplit(Direction::Up)),
            (
                "cmd+ctrl+arrowdown",
                AppCommand::ResizeSplit(Direction::Down),
            ),
            ("cmd+ctrl+=", AppCommand::EqualizeSplits),
            ("cmd+shift+enter", AppCommand::ToggleSplitZoom),
            ("cmd+shift+o", AppCommand::ToggleTabOverview),
            ("cmd+shift+p", AppCommand::ToggleCommandPalette),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyTrigger {
    mods: TriggerMods,
    key: KeyToken,
}

impl KeyTrigger {
    fn parse(input: &str) -> Result<Self, KeybindParseError> {
        let mut mods = TriggerMods::default();
        let mut key = None;
        for token in input
            .split('+')
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
        {
            let normalized = token.to_ascii_lowercase();
            match normalized.as_str() {
                "cmd" | "command" | "super" | "meta" => mods.super_key = true,
                "ctrl" | "control" => mods.control = true,
                "alt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => {
                    if key.is_some() {
                        return Err(KeybindParseError::MultipleKeys);
                    }
                    key = Some(KeyToken::parse(&normalized)?);
                }
            }
        }
        let Some(key) = key else {
            return Err(KeybindParseError::MissingKey);
        };
        Ok(Self { mods, key })
    }

    fn matches(&self, logical_key: &Key, mods: ModifiersState) -> bool {
        self.mods.matches(mods) && self.key.matches(logical_key)
    }
}

impl std::fmt::Display for KeyTrigger {
    /// Renders the config-style chord text (`cmd+ctrl+alt+shift+key`), in the
    /// same modifier order the parser accepts, so the output round-trips back
    /// through [`KeyTrigger::parse`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.super_key {
            f.write_str("cmd+")?;
        }
        if self.mods.control {
            f.write_str("ctrl+")?;
        }
        if self.mods.alt {
            f.write_str("alt+")?;
        }
        if self.mods.shift {
            f.write_str("shift+")?;
        }
        write!(f, "{}", self.key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TriggerMods {
    shift: bool,
    control: bool,
    alt: bool,
    super_key: bool,
}

impl TriggerMods {
    fn matches(self, mods: ModifiersState) -> bool {
        self.shift == mods.shift_key()
            && self.control == mods.control_key()
            && self.alt == mods.alt_key()
            && self.super_key == mods.super_key()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyToken {
    Character(char),
    Named(NamedKeyToken),
}

impl KeyToken {
    fn parse(token: &str) -> Result<Self, KeybindParseError> {
        if token == "plus" {
            return Ok(Self::Character('+'));
        }
        let mut chars = token.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            return Ok(Self::Character(ch));
        }
        Ok(Self::Named(match token {
            "arrowup" | "up" => NamedKeyToken::ArrowUp,
            "arrowdown" | "down" => NamedKeyToken::ArrowDown,
            "arrowleft" | "left" => NamedKeyToken::ArrowLeft,
            "arrowright" | "right" => NamedKeyToken::ArrowRight,
            "pageup" => NamedKeyToken::PageUp,
            "pagedown" => NamedKeyToken::PageDown,
            "home" => NamedKeyToken::Home,
            "end" => NamedKeyToken::End,
            "enter" | "return" => NamedKeyToken::Enter,
            _ => return Err(KeybindParseError::UnknownKey(token.to_string())),
        }))
    }

    fn matches(self, logical_key: &Key) -> bool {
        match (self, logical_key) {
            (Self::Character(expected), Key::Character(actual)) => {
                actual.chars().next().is_some_and(|actual| {
                    actual.eq_ignore_ascii_case(&expected)
                        || (expected == '[' && actual == '{')
                        || (expected == ']' && actual == '}')
                })
            }
            (Self::Named(expected), Key::Named(actual)) => expected.matches(*actual),
            _ => false,
        }
    }
}

impl std::fmt::Display for KeyToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `+` parses from the `plus` alias, so render it back that way
            // (a bare `+` would read as a separator on re-parse).
            Self::Character('+') => f.write_str("plus"),
            Self::Character(ch) => write!(f, "{ch}"),
            Self::Named(named) => f.write_str(named.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedKeyToken {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
}

impl NamedKeyToken {
    /// The canonical chord token for this key, matching a name
    /// [`KeyToken::parse`] accepts (so [`KeyTrigger`]'s `Display` round-trips).
    fn as_str(self) -> &'static str {
        match self {
            Self::ArrowUp => "arrowup",
            Self::ArrowDown => "arrowdown",
            Self::ArrowLeft => "arrowleft",
            Self::ArrowRight => "arrowright",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Home => "home",
            Self::End => "end",
            Self::Enter => "enter",
        }
    }

    fn matches(self, key: NamedKey) -> bool {
        matches!(
            (self, key),
            (Self::ArrowUp, NamedKey::ArrowUp)
                | (Self::ArrowDown, NamedKey::ArrowDown)
                | (Self::ArrowLeft, NamedKey::ArrowLeft)
                | (Self::ArrowRight, NamedKey::ArrowRight)
                | (Self::PageUp, NamedKey::PageUp)
                | (Self::PageDown, NamedKey::PageDown)
                | (Self::Home, NamedKey::Home)
                | (Self::End, NamedKey::End)
                | (Self::Enter, NamedKey::Enter)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeybindParseError {
    MissingKey,
    MultipleKeys,
    UnknownKey(String),
}

impl std::fmt::Display for KeybindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => f.write_str("keybind is missing a key"),
            Self::MultipleKeys => f.write_str("keybind contains multiple keys"),
            Self::UnknownKey(key) => write!(f, "unknown key in keybind: {key}"),
        }
    }
}

impl std::error::Error for KeybindParseError {}

#[cfg(test)]
mod tests {
    use super::{
        AppCommand, FontSizeAction, KeyBinding, KeybindEngine, KeybindParseError, SearchAction,
        TerminalAction, ViewportScroll,
    };
    use crate::split_tree::Direction;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    #[test]
    fn stable_menu_ids_map_to_commands() {
        for command in [
            AppCommand::About,
            AppCommand::Preferences,
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::LineUp),
            AppCommand::ScrollViewport(ViewportScroll::LineDown),
            AppCommand::ScrollViewport(ViewportScroll::PageUp),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::ScrollViewport(ViewportScroll::Top),
            AppCommand::ScrollViewport(ViewportScroll::Bottom),
            AppCommand::NewTab,
            AppCommand::NewSplitRight,
            AppCommand::NewSplitDown,
            AppCommand::FocusDirection(Direction::Left),
            AppCommand::FocusDirection(Direction::Right),
            AppCommand::FocusDirection(Direction::Up),
            AppCommand::FocusDirection(Direction::Down),
            AppCommand::ResizeSplit(Direction::Left),
            AppCommand::ResizeSplit(Direction::Right),
            AppCommand::ResizeSplit(Direction::Up),
            AppCommand::ResizeSplit(Direction::Down),
            AppCommand::EqualizeSplits,
            AppCommand::ToggleSplitZoom,
            AppCommand::ToggleTabOverview,
            AppCommand::CloseTab,
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::CloseWindow,
            AppCommand::Quit,
        ] {
            assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        }

        assert_eq!(AppCommand::from_menu_id("noa.app.unknown"), None);
    }

    #[test]
    fn cmd_characters_map_only_supported_shortcuts() {
        assert_eq!(AppCommand::from_cmd_character("q"), Some(AppCommand::Quit));
        assert_eq!(AppCommand::from_cmd_character("Q"), Some(AppCommand::Quit));
        assert_eq!(
            AppCommand::from_cmd_character("w"),
            Some(AppCommand::CloseTab)
        );
        assert_eq!(
            AppCommand::from_cmd_character("t"),
            Some(AppCommand::NewTab)
        );
        assert_eq!(
            AppCommand::from_cmd_character("d"),
            Some(AppCommand::NewSplitRight)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("D".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::NewSplitDown)
        );
        assert_eq!(
            AppCommand::from_cmd_character("1"),
            Some(AppCommand::SelectTab(1))
        );
        assert_eq!(
            AppCommand::from_cmd_character("9"),
            Some(AppCommand::SelectTab(9))
        );
        assert_eq!(AppCommand::from_cmd_character("c"), Some(AppCommand::Copy));
        assert_eq!(AppCommand::from_cmd_character("V"), Some(AppCommand::Paste));
        assert_eq!(
            AppCommand::from_cmd_character("k"),
            Some(AppCommand::Terminal(TerminalAction::Clear))
        );
        assert_eq!(
            AppCommand::from_cmd_character("A"),
            Some(AppCommand::Terminal(TerminalAction::SelectAll))
        );
        assert_eq!(
            AppCommand::from_cmd_character("="),
            Some(AppCommand::FontSize(FontSizeAction::Increase))
        );
        assert_eq!(
            AppCommand::from_cmd_character("-"),
            Some(AppCommand::FontSize(FontSizeAction::Decrease))
        );
        assert_eq!(
            AppCommand::from_cmd_character("0"),
            Some(AppCommand::FontSize(FontSizeAction::Reset))
        );
        assert_eq!(
            AppCommand::from_cmd_character("f"),
            Some(AppCommand::Search(SearchAction::Find))
        );
        assert_eq!(
            AppCommand::from_cmd_character("g"),
            Some(AppCommand::Search(SearchAction::FindNext))
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("+".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::FontSize(FontSizeAction::Increase))
        );
        assert_eq!(AppCommand::from_cmd_character(","), None);
    }

    #[test]
    fn find_action_is_addressable_and_bound_to_cmd_f() {
        let command = AppCommand::Search(SearchAction::Find);

        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(
            AppCommand::from_action_name(command.action_name()),
            Some(command)
        );
        assert_eq!(
            AppCommand::from_key(&Key::Character("f".into()), ModifiersState::SUPER),
            Some(command)
        );
        // cmd+shift+f is deliberately unbound (reserved, e.g. for a future
        // "find and replace" or case-sensitive toggle).
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("F".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            None
        );
    }

    #[test]
    fn shift_navigation_keys_map_to_viewport_scroll_commands() {
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::ArrowUp), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::LineUp))
        );
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::ArrowDown), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::LineDown))
        );
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::PageUp), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::PageUp))
        );
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::PageDown), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::PageDown))
        );
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::Home), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::Top))
        );
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::End), ModifiersState::SHIFT),
            Some(AppCommand::ScrollViewport(ViewportScroll::Bottom))
        );
    }

    #[test]
    fn split_shortcuts_map_to_pane_commands() {
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(NamedKey::ArrowLeft),
                ModifiersState::SUPER | ModifiersState::ALT
            ),
            Some(AppCommand::FocusDirection(Direction::Left))
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(NamedKey::ArrowRight),
                ModifiersState::SUPER | ModifiersState::CONTROL
            ),
            Some(AppCommand::ResizeSplit(Direction::Right))
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("=".into()),
                ModifiersState::SUPER | ModifiersState::CONTROL
            ),
            Some(AppCommand::EqualizeSplits)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(NamedKey::Enter),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::ToggleSplitZoom)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("o".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::ToggleTabOverview)
        );
    }

    #[test]
    fn tab_cycle_shortcuts_use_cmd_shift_brackets() {
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("]".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::NextTab)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("}".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::NextTab)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("[".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::PrevTab)
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("{".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::PrevTab)
        );
    }

    #[test]
    fn command_palette_toggle_round_trips_and_binds_to_cmd_shift_p() {
        let command = AppCommand::ToggleCommandPalette;

        // AC-1: menu-id / action-name / keybind all round-trip.
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(
            AppCommand::from_action_name(command.action_name()),
            Some(command)
        );
        assert_eq!(command.action_name(), "command-palette.toggle");
        assert_eq!(
            AppCommand::from_key(
                &Key::Character("p".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(command)
        );
    }

    #[test]
    fn command_palette_binding_does_not_shadow_existing_cmd_shift_shortcuts() {
        // AC-2: adding cmd+shift+p leaves the other cmd+shift+* bindings intact.
        let engine = KeybindEngine::default();
        let cmd_shift = ModifiersState::SUPER | ModifiersState::SHIFT;
        for (key, expected) in [
            ("o", AppCommand::ToggleTabOverview),
            ("d", AppCommand::NewSplitDown),
            ("g", AppCommand::Search(SearchAction::FindPrevious)),
            ("]", AppCommand::NextTab),
            ("[", AppCommand::PrevTab),
        ] {
            assert_eq!(
                engine.resolve(&Key::Character(key.into()), cmd_shift),
                Some(expected),
                "cmd+shift+{key} must stay bound to {expected:?}"
            );
        }
        assert_eq!(
            engine.resolve(&Key::Character("p".into()), cmd_shift),
            Some(AppCommand::ToggleCommandPalette)
        );
    }

    #[test]
    fn chord_for_reverse_maps_bound_commands_and_reports_none_for_unbound() {
        // AC-5 (engine layer): chord text round-trips modifier order.
        let engine = KeybindEngine::default();
        assert_eq!(engine.chord_for(AppCommand::Copy).as_deref(), Some("cmd+c"));
        assert_eq!(
            engine
                .chord_for(AppCommand::ToggleCommandPalette)
                .as_deref(),
            Some("cmd+shift+p")
        );
        assert_eq!(
            engine
                .chord_for(AppCommand::ScrollViewport(ViewportScroll::LineUp))
                .as_deref(),
            Some("shift+arrowup")
        );
        assert_eq!(
            engine.chord_for(AppCommand::Terminal(TerminalAction::ClearScrollback)),
            None
        );
    }

    #[test]
    fn viewport_scroll_shortcuts_require_shift_only() {
        assert_eq!(
            AppCommand::from_key(&Key::Named(NamedKey::PageUp), ModifiersState::empty()),
            None
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(NamedKey::PageUp),
                ModifiersState::SHIFT | ModifiersState::SUPER
            ),
            None
        );
        assert_eq!(
            AppCommand::from_key(&Key::Character("x".into()), ModifiersState::SHIFT),
            None
        );
    }

    #[test]
    fn action_names_map_to_commands() {
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::PageUp),
            AppCommand::NewTab,
            AppCommand::NewSplitRight,
            AppCommand::NewSplitDown,
            AppCommand::FocusDirection(Direction::Left),
            AppCommand::FocusDirection(Direction::Right),
            AppCommand::FocusDirection(Direction::Up),
            AppCommand::FocusDirection(Direction::Down),
            AppCommand::ResizeSplit(Direction::Left),
            AppCommand::ResizeSplit(Direction::Right),
            AppCommand::ResizeSplit(Direction::Up),
            AppCommand::ResizeSplit(Direction::Down),
            AppCommand::EqualizeSplits,
            AppCommand::ToggleSplitZoom,
            AppCommand::ToggleTabOverview,
            AppCommand::CloseTab,
            AppCommand::SelectTab(3),
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::Quit,
        ] {
            assert_eq!(
                AppCommand::from_action_name(command.action_name()),
                Some(command)
            );
        }
        assert_eq!(AppCommand::from_action_name("nope"), None);
    }

    #[test]
    fn keybind_parser_accepts_config_style_chords() {
        let binding = KeyBinding::parse(
            "cmd+shift+g",
            AppCommand::Search(SearchAction::FindPrevious),
        )
        .expect("keybind should parse");
        let engine = KeybindEngine {
            bindings: vec![binding],
        };

        assert_eq!(
            engine.resolve(
                &Key::Character("g".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::Search(SearchAction::FindPrevious))
        );
        assert_eq!(
            engine.resolve(&Key::Character("g".into()), ModifiersState::SUPER),
            None
        );
    }

    #[test]
    fn plus_alias_matches_logical_plus_key() {
        let binding = KeyBinding::parse(
            "cmd+shift+plus",
            AppCommand::FontSize(FontSizeAction::Increase),
        )
        .expect("plus alias should parse");
        let engine = KeybindEngine {
            bindings: vec![binding],
        };

        assert_eq!(
            engine.resolve(
                &Key::Character("+".into()),
                ModifiersState::SUPER | ModifiersState::SHIFT
            ),
            Some(AppCommand::FontSize(FontSizeAction::Increase))
        );
        assert_eq!(
            engine.resolve(&Key::Character("+".into()), ModifiersState::SUPER),
            None
        );
    }

    #[test]
    fn keybind_parser_rejects_missing_or_unknown_key() {
        assert!(matches!(
            KeyBinding::parse("cmd+shift", AppCommand::Copy),
            Err(KeybindParseError::MissingKey)
        ));
        assert!(matches!(
            KeyBinding::parse("cmd+no-such-key", AppCommand::Copy),
            Err(KeybindParseError::UnknownKey(_))
        ));
    }
}
