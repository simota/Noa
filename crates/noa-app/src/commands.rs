//! App-level commands that must not be encoded as terminal input.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Commands handled by the application layer rather than the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Preferences,
    Copy,
    Paste,
    Search(SearchAction),
    ScrollViewport(ViewportScroll),
    NewTab,
    CloseTab,
    SelectTab(usize),
    NextTab,
    PrevTab,
    CloseWindow,
    Quit,
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
    pub(crate) const SEARCH_FIND_MENU_ID: &'static str = "noa.edit.find";
    pub(crate) const SEARCH_FIND_NEXT_MENU_ID: &'static str = "noa.edit.find-next";
    pub(crate) const SEARCH_FIND_PREVIOUS_MENU_ID: &'static str = "noa.edit.find-previous";
    pub(crate) const SEARCH_CLEAR_MENU_ID: &'static str = "noa.edit.clear-search";
    pub(crate) const SCROLL_LINE_UP_MENU_ID: &'static str = "noa.view.scroll-line-up";
    pub(crate) const SCROLL_LINE_DOWN_MENU_ID: &'static str = "noa.view.scroll-line-down";
    pub(crate) const SCROLL_PAGE_UP_MENU_ID: &'static str = "noa.view.scroll-page-up";
    pub(crate) const SCROLL_PAGE_DOWN_MENU_ID: &'static str = "noa.view.scroll-page-down";
    pub(crate) const SCROLL_TOP_MENU_ID: &'static str = "noa.view.scroll-top";
    pub(crate) const SCROLL_BOTTOM_MENU_ID: &'static str = "noa.view.scroll-bottom";
    pub(crate) const NEW_TAB_MENU_ID: &'static str = "noa.file.new-tab";
    pub(crate) const CLOSE_TAB_MENU_ID: &'static str = "noa.file.close-tab";
    pub(crate) const NEXT_TAB_MENU_ID: &'static str = "noa.window.next-tab";
    pub(crate) const PREV_TAB_MENU_ID: &'static str = "noa.window.previous-tab";
    pub(crate) const CLOSE_WINDOW_MENU_ID: &'static str = "noa.app.close-window";
    pub(crate) const QUIT_MENU_ID: &'static str = "noa.app.quit";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::About => Self::ABOUT_MENU_ID,
            AppCommand::Preferences => Self::PREFERENCES_MENU_ID,
            AppCommand::Copy => Self::COPY_MENU_ID,
            AppCommand::Paste => Self::PASTE_MENU_ID,
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
            AppCommand::CloseTab => Self::CLOSE_TAB_MENU_ID,
            AppCommand::SelectTab(_) => "",
            AppCommand::NextTab => Self::NEXT_TAB_MENU_ID,
            AppCommand::PrevTab => Self::PREV_TAB_MENU_ID,
            AppCommand::CloseWindow => Self::CLOSE_WINDOW_MENU_ID,
            AppCommand::Quit => Self::QUIT_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::ABOUT_MENU_ID => Some(Self::About),
            Self::PREFERENCES_MENU_ID => Some(Self::Preferences),
            Self::COPY_MENU_ID => Some(Self::Copy),
            Self::PASTE_MENU_ID => Some(Self::Paste),
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
            Self::CLOSE_TAB_MENU_ID => Some(Self::CloseTab),
            Self::NEXT_TAB_MENU_ID => Some(Self::NextTab),
            Self::PREV_TAB_MENU_ID => Some(Self::PrevTab),
            Self::CLOSE_WINDOW_MENU_ID => Some(Self::CloseWindow),
            Self::QUIT_MENU_ID => Some(Self::Quit),
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
        }
    }

    pub fn from_action_name(name: &str) -> Option<Self> {
        match name {
            "about" => Some(Self::About),
            "preferences" => Some(Self::Preferences),
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
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
}

impl NamedKeyToken {
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
        AppCommand, KeyBinding, KeybindEngine, KeybindParseError, SearchAction, ViewportScroll,
    };
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    #[test]
    fn stable_menu_ids_map_to_commands() {
        for command in [
            AppCommand::About,
            AppCommand::Preferences,
            AppCommand::Copy,
            AppCommand::Paste,
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
            AppCommand::from_cmd_character("f"),
            Some(AppCommand::Search(SearchAction::Find))
        );
        assert_eq!(AppCommand::from_cmd_character(","), None);
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
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::PageUp),
            AppCommand::NewTab,
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
