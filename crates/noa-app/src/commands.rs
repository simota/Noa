//! App-level commands that must not be encoded as terminal input.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Commands handled by the application layer rather than the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Preferences,
    Copy,
    Paste,
    ScrollViewport(ViewportScroll),
    CloseWindow,
    Quit,
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
    pub(crate) const SCROLL_LINE_UP_MENU_ID: &'static str = "noa.view.scroll-line-up";
    pub(crate) const SCROLL_LINE_DOWN_MENU_ID: &'static str = "noa.view.scroll-line-down";
    pub(crate) const SCROLL_PAGE_UP_MENU_ID: &'static str = "noa.view.scroll-page-up";
    pub(crate) const SCROLL_PAGE_DOWN_MENU_ID: &'static str = "noa.view.scroll-page-down";
    pub(crate) const SCROLL_TOP_MENU_ID: &'static str = "noa.view.scroll-top";
    pub(crate) const SCROLL_BOTTOM_MENU_ID: &'static str = "noa.view.scroll-bottom";
    pub(crate) const CLOSE_WINDOW_MENU_ID: &'static str = "noa.app.close-window";
    pub(crate) const QUIT_MENU_ID: &'static str = "noa.app.quit";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::About => Self::ABOUT_MENU_ID,
            AppCommand::Preferences => Self::PREFERENCES_MENU_ID,
            AppCommand::Copy => Self::COPY_MENU_ID,
            AppCommand::Paste => Self::PASTE_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::LineUp) => Self::SCROLL_LINE_UP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::LineDown) => Self::SCROLL_LINE_DOWN_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::PageUp) => Self::SCROLL_PAGE_UP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::PageDown) => Self::SCROLL_PAGE_DOWN_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::Top) => Self::SCROLL_TOP_MENU_ID,
            AppCommand::ScrollViewport(ViewportScroll::Bottom) => Self::SCROLL_BOTTOM_MENU_ID,
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
            Self::SCROLL_LINE_UP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::LineUp)),
            Self::SCROLL_LINE_DOWN_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::LineDown)),
            Self::SCROLL_PAGE_UP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::PageUp)),
            Self::SCROLL_PAGE_DOWN_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::PageDown)),
            Self::SCROLL_TOP_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::Top)),
            Self::SCROLL_BOTTOM_MENU_ID => Some(Self::ScrollViewport(ViewportScroll::Bottom)),
            Self::CLOSE_WINDOW_MENU_ID => Some(Self::CloseWindow),
            Self::QUIT_MENU_ID => Some(Self::Quit),
            _ => None,
        }
    }

    pub(crate) fn from_cmd_character(character: &str) -> Option<Self> {
        if character.eq_ignore_ascii_case("q") {
            Some(Self::Quit)
        } else if character.eq_ignore_ascii_case("w") {
            Some(Self::CloseWindow)
        } else if character.eq_ignore_ascii_case("c") {
            Some(Self::Copy)
        } else if character.eq_ignore_ascii_case("v") {
            Some(Self::Paste)
        } else {
            None
        }
    }

    pub(crate) fn from_key(logical_key: &Key, mods: ModifiersState) -> Option<Self> {
        if !mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key() {
            return None;
        }

        match logical_key {
            Key::Named(NamedKey::ArrowUp) => Some(Self::ScrollViewport(ViewportScroll::LineUp)),
            Key::Named(NamedKey::ArrowDown) => Some(Self::ScrollViewport(ViewportScroll::LineDown)),
            Key::Named(NamedKey::PageUp) => Some(Self::ScrollViewport(ViewportScroll::PageUp)),
            Key::Named(NamedKey::PageDown) => Some(Self::ScrollViewport(ViewportScroll::PageDown)),
            Key::Named(NamedKey::Home) => Some(Self::ScrollViewport(ViewportScroll::Top)),
            Key::Named(NamedKey::End) => Some(Self::ScrollViewport(ViewportScroll::Bottom)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppCommand, ViewportScroll};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    #[test]
    fn stable_menu_ids_map_to_commands() {
        for command in [
            AppCommand::About,
            AppCommand::Preferences,
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::ScrollViewport(ViewportScroll::LineUp),
            AppCommand::ScrollViewport(ViewportScroll::LineDown),
            AppCommand::ScrollViewport(ViewportScroll::PageUp),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::ScrollViewport(ViewportScroll::Top),
            AppCommand::ScrollViewport(ViewportScroll::Bottom),
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
            Some(AppCommand::CloseWindow)
        );
        assert_eq!(AppCommand::from_cmd_character("c"), Some(AppCommand::Copy));
        assert_eq!(AppCommand::from_cmd_character("V"), Some(AppCommand::Paste));
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
}
