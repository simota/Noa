//! App-level commands that must not be encoded as terminal input.

/// Commands handled by the application layer rather than the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Preferences,
    CloseWindow,
    Quit,
}

impl AppCommand {
    pub(crate) const ABOUT_MENU_ID: &'static str = "noa.app.about";
    pub(crate) const PREFERENCES_MENU_ID: &'static str = "noa.app.preferences";
    pub(crate) const CLOSE_WINDOW_MENU_ID: &'static str = "noa.app.close-window";
    pub(crate) const QUIT_MENU_ID: &'static str = "noa.app.quit";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::About => Self::ABOUT_MENU_ID,
            AppCommand::Preferences => Self::PREFERENCES_MENU_ID,
            AppCommand::CloseWindow => Self::CLOSE_WINDOW_MENU_ID,
            AppCommand::Quit => Self::QUIT_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::ABOUT_MENU_ID => Some(Self::About),
            Self::PREFERENCES_MENU_ID => Some(Self::Preferences),
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
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppCommand;

    #[test]
    fn stable_menu_ids_map_to_commands() {
        for command in [
            AppCommand::About,
            AppCommand::Preferences,
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
        assert_eq!(AppCommand::from_cmd_character(","), None);
        assert_eq!(AppCommand::from_cmd_character("c"), None);
    }
}
