#[cfg(test)]
use super::keybind::KeybindEngine;
use crate::split_tree::Direction;
use noa_grid::CopyDirection;
#[cfg(test)]
use winit::keyboard::{Key, ModifiersState};

/// Commands handled by the application layer rather than the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Preferences,
    /// R-23: the pre-R-22 behavior of `Preferences` (open the raw config
    /// file in the user's external editor), kept reachable as its own
    /// command once `Preferences` itself is redirected to the GUI settings
    /// overlay (`open_config_file()`, same as before — no behavior change,
    /// just a new command identity so both are independently reachable).
    EditConfigFile,
    ReloadConfig,
    Copy,
    Paste,
    /// Start the config-backed remote pane discovery/attach flow. The only
    /// connection-UX entry in v1; deliberately has no default keybinding or
    /// native menu item.
    AttachRemote,
    SendSelectionToPane,
    ExportScrollback,
    PipeScrollbackToPager,
    Terminal(TerminalAction),
    FontSize(FontSizeAction),
    Search(SearchAction),
    ScrollViewport(ViewportScroll),
    CopyMode(CopyModeAction),
    NewTab,
    NewWindow,
    NewSplitLeft,
    NewSplitRight,
    NewSplitUp,
    NewSplitDown,
    FocusDirection(Direction),
    ResizeSplit(Direction),
    EqualizeSplits,
    ToggleSplitZoom,
    ToggleTabOverview,
    ToggleFullscreen,
    CloseTab,
    SelectTab(usize),
    NextTab,
    PrevTab,
    /// Open the "Set Tab Title" prompt for the focused tab (tab-title
    /// REQ-TTL-1). No default chord — Ghostty ships `prompt_surface_title`
    /// unbound too — so it is reached via the palette and the Window menu.
    SetTabTitle,
    CloseWindow,
    Quit,
    ToggleCommandPalette,
    ToggleQuickTerminal,
    ToggleSecureKeyboardEntry,
    ToggleSidebar,
    ToggleAutoApprove,
    /// Open the "Theme" picker half of the split theme-settings overlay
    /// (theme-settings-ui R-1, later split into two overlays). Reachable
    /// only from the command palette — deliberately unbound in
    /// [`KeybindEngine::default`], so it carries no menu id either (mirrors
    /// `SelectTab`'s `menu_id() -> ""`).
    OpenThemePicker,
    /// Open the "Settings" rows half of the split theme-settings overlay.
    /// Reachable from the command palette like [`Self::OpenThemePicker`],
    /// plus (settings-panel-enrichment R-2) a native View-menu item —
    /// deliberately unbound (no accelerator), so it carries no keybind, only
    /// [`Self::OPEN_SETTINGS_MENU_ID`].
    OpenSettings,
    /// Open/close the process-monitor overlay (`docs/specs/panel-metrics-view.md`
    /// FR-1). Reachable only from the command palette — no default keybind
    /// (v1 scope: Open Questions), so it carries no menu id either (mirrors
    /// `SelectTab`'s `menu_id() -> ""`).
    ToggleProcessMonitor,
}

/// Config-addressable copy-mode entry actions. `CursorOnly` implements the
/// unbound `copy_mode` action; the directional actions power the rebindable
/// direct gestures and extend immediately on entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeAction {
    CursorOnly,
    Extend(CopyDirection),
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
    /// Jump to the nearest shell-integration prompt above the viewport top.
    PrevPrompt,
    /// Jump to the nearest shell-integration prompt below the viewport top.
    NextPrompt,
}

impl AppCommand {
    pub(crate) const ABOUT_MENU_ID: &'static str = "noa.app.about";
    pub(crate) const PREFERENCES_MENU_ID: &'static str = "noa.app.preferences";
    pub(crate) const EDIT_CONFIG_FILE_MENU_ID: &'static str = "noa.app.edit-config-file";
    pub(crate) const OPEN_THEME_PICKER_MENU_ID: &'static str = "noa.app.open-theme-picker";
    pub(crate) const COPY_MENU_ID: &'static str = "noa.edit.copy";
    pub(crate) const PASTE_MENU_ID: &'static str = "noa.edit.paste";
    pub(crate) const SEND_SELECTION_TO_PANE_MENU_ID: &'static str =
        "noa.edit.send-selection-to-pane";
    pub(crate) const EXPORT_SCROLLBACK_MENU_ID: &'static str = "noa.file.export-scrollback";
    pub(crate) const PIPE_SCROLLBACK_TO_PAGER_MENU_ID: &'static str =
        "noa.file.pipe-scrollback-to-pager";
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
    pub(crate) const SCROLL_PREV_PROMPT_MENU_ID: &'static str = "noa.view.scroll-prev-prompt";
    pub(crate) const SCROLL_NEXT_PROMPT_MENU_ID: &'static str = "noa.view.scroll-next-prompt";
    pub(crate) const NEW_TAB_MENU_ID: &'static str = "noa.file.new-tab";
    pub(crate) const NEW_WINDOW_MENU_ID: &'static str = "noa.file.new-window";
    pub(crate) const NEW_SPLIT_LEFT_MENU_ID: &'static str = "noa.file.new-split-left";
    pub(crate) const NEW_SPLIT_RIGHT_MENU_ID: &'static str = "noa.file.new-split-right";
    pub(crate) const NEW_SPLIT_UP_MENU_ID: &'static str = "noa.file.new-split-up";
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
    pub(crate) const TOGGLE_TAB_OVERVIEW_MENU_ID: &'static str = "noa.view.toggle-session-overview";
    pub(crate) const LEGACY_TOGGLE_TAB_OVERVIEW_MENU_ID: &'static str =
        "noa.view.toggle-tab-overview";
    pub(crate) const TOGGLE_FULLSCREEN_MENU_ID: &'static str = "noa.view.toggle-full-screen";
    pub(crate) const CLOSE_TAB_MENU_ID: &'static str = "noa.file.close-tab";
    pub(crate) const NEXT_TAB_MENU_ID: &'static str = "noa.window.next-tab";
    pub(crate) const PREV_TAB_MENU_ID: &'static str = "noa.window.previous-tab";
    pub(crate) const SET_TAB_TITLE_MENU_ID: &'static str = "noa.window.set-tab-title";
    pub(crate) const CLOSE_WINDOW_MENU_ID: &'static str = "noa.app.close-window";
    pub(crate) const QUIT_MENU_ID: &'static str = "noa.app.quit";
    pub(crate) const TOGGLE_COMMAND_PALETTE_MENU_ID: &'static str =
        "noa.view.toggle-command-palette";
    pub(crate) const TOGGLE_QUICK_TERMINAL_MENU_ID: &'static str = "noa.view.toggle-quick-terminal";
    pub(crate) const TOGGLE_SECURE_KEYBOARD_ENTRY_MENU_ID: &'static str =
        "noa.app.toggle-secure-keyboard-entry";
    pub(crate) const TOGGLE_SIDEBAR_MENU_ID: &'static str = "noa.view.toggle-sidebar";
    pub(crate) const TOGGLE_AUTO_APPROVE_MENU_ID: &'static str = "noa.view.toggle-auto-approve";
    pub(crate) const OPEN_SETTINGS_MENU_ID: &'static str = "noa.view.open-settings";

    pub(crate) fn menu_id(self) -> &'static str {
        match self {
            AppCommand::About => Self::ABOUT_MENU_ID,
            AppCommand::Preferences => Self::PREFERENCES_MENU_ID,
            AppCommand::EditConfigFile => Self::EDIT_CONFIG_FILE_MENU_ID,
            AppCommand::ReloadConfig => "",
            AppCommand::Copy => Self::COPY_MENU_ID,
            AppCommand::Paste => Self::PASTE_MENU_ID,
            AppCommand::AttachRemote => "",
            AppCommand::SendSelectionToPane => Self::SEND_SELECTION_TO_PANE_MENU_ID,
            AppCommand::ExportScrollback => Self::EXPORT_SCROLLBACK_MENU_ID,
            AppCommand::PipeScrollbackToPager => Self::PIPE_SCROLLBACK_TO_PAGER_MENU_ID,
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
            AppCommand::ScrollViewport(ViewportScroll::PrevPrompt) => {
                Self::SCROLL_PREV_PROMPT_MENU_ID
            }
            AppCommand::ScrollViewport(ViewportScroll::NextPrompt) => {
                Self::SCROLL_NEXT_PROMPT_MENU_ID
            }
            AppCommand::CopyMode(_) => "",
            AppCommand::NewTab => Self::NEW_TAB_MENU_ID,
            AppCommand::NewWindow => Self::NEW_WINDOW_MENU_ID,
            AppCommand::NewSplitLeft => Self::NEW_SPLIT_LEFT_MENU_ID,
            AppCommand::NewSplitRight => Self::NEW_SPLIT_RIGHT_MENU_ID,
            AppCommand::NewSplitUp => Self::NEW_SPLIT_UP_MENU_ID,
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
            AppCommand::ToggleFullscreen => Self::TOGGLE_FULLSCREEN_MENU_ID,
            AppCommand::CloseTab => Self::CLOSE_TAB_MENU_ID,
            AppCommand::SelectTab(_) => "",
            AppCommand::OpenThemePicker => Self::OPEN_THEME_PICKER_MENU_ID,
            AppCommand::OpenSettings => Self::OPEN_SETTINGS_MENU_ID,
            AppCommand::ToggleProcessMonitor => "",
            AppCommand::NextTab => Self::NEXT_TAB_MENU_ID,
            AppCommand::PrevTab => Self::PREV_TAB_MENU_ID,
            AppCommand::SetTabTitle => Self::SET_TAB_TITLE_MENU_ID,
            AppCommand::CloseWindow => Self::CLOSE_WINDOW_MENU_ID,
            AppCommand::Quit => Self::QUIT_MENU_ID,
            AppCommand::ToggleCommandPalette => Self::TOGGLE_COMMAND_PALETTE_MENU_ID,
            AppCommand::ToggleQuickTerminal => Self::TOGGLE_QUICK_TERMINAL_MENU_ID,
            AppCommand::ToggleSecureKeyboardEntry => Self::TOGGLE_SECURE_KEYBOARD_ENTRY_MENU_ID,
            AppCommand::ToggleSidebar => Self::TOGGLE_SIDEBAR_MENU_ID,
            AppCommand::ToggleAutoApprove => Self::TOGGLE_AUTO_APPROVE_MENU_ID,
        }
    }

    pub(crate) fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            Self::ABOUT_MENU_ID => Some(Self::About),
            Self::PREFERENCES_MENU_ID => Some(Self::Preferences),
            Self::EDIT_CONFIG_FILE_MENU_ID => Some(Self::EditConfigFile),
            Self::OPEN_THEME_PICKER_MENU_ID => Some(Self::OpenThemePicker),
            Self::COPY_MENU_ID => Some(Self::Copy),
            Self::PASTE_MENU_ID => Some(Self::Paste),
            Self::SEND_SELECTION_TO_PANE_MENU_ID => Some(Self::SendSelectionToPane),
            Self::EXPORT_SCROLLBACK_MENU_ID => Some(Self::ExportScrollback),
            Self::PIPE_SCROLLBACK_TO_PAGER_MENU_ID => Some(Self::PipeScrollbackToPager),
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
            Self::SCROLL_PREV_PROMPT_MENU_ID => {
                Some(Self::ScrollViewport(ViewportScroll::PrevPrompt))
            }
            Self::SCROLL_NEXT_PROMPT_MENU_ID => {
                Some(Self::ScrollViewport(ViewportScroll::NextPrompt))
            }
            Self::NEW_TAB_MENU_ID => Some(Self::NewTab),
            Self::NEW_WINDOW_MENU_ID => Some(Self::NewWindow),
            Self::NEW_SPLIT_LEFT_MENU_ID => Some(Self::NewSplitLeft),
            Self::NEW_SPLIT_RIGHT_MENU_ID => Some(Self::NewSplitRight),
            Self::NEW_SPLIT_UP_MENU_ID => Some(Self::NewSplitUp),
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
            Self::TOGGLE_TAB_OVERVIEW_MENU_ID | Self::LEGACY_TOGGLE_TAB_OVERVIEW_MENU_ID => {
                Some(Self::ToggleTabOverview)
            }
            Self::TOGGLE_FULLSCREEN_MENU_ID => Some(Self::ToggleFullscreen),
            Self::CLOSE_TAB_MENU_ID => Some(Self::CloseTab),
            Self::NEXT_TAB_MENU_ID => Some(Self::NextTab),
            Self::PREV_TAB_MENU_ID => Some(Self::PrevTab),
            Self::SET_TAB_TITLE_MENU_ID => Some(Self::SetTabTitle),
            Self::CLOSE_WINDOW_MENU_ID => Some(Self::CloseWindow),
            Self::QUIT_MENU_ID => Some(Self::Quit),
            Self::TOGGLE_COMMAND_PALETTE_MENU_ID => Some(Self::ToggleCommandPalette),
            Self::TOGGLE_QUICK_TERMINAL_MENU_ID => Some(Self::ToggleQuickTerminal),
            Self::TOGGLE_SECURE_KEYBOARD_ENTRY_MENU_ID => Some(Self::ToggleSecureKeyboardEntry),
            Self::TOGGLE_SIDEBAR_MENU_ID => Some(Self::ToggleSidebar),
            Self::TOGGLE_AUTO_APPROVE_MENU_ID => Some(Self::ToggleAutoApprove),
            Self::OPEN_SETTINGS_MENU_ID => Some(Self::OpenSettings),
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
            Self::EditConfigFile => "preferences.edit-config-file",
            Self::ReloadConfig => "config.reload",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::AttachRemote => "remote.attach",
            Self::SendSelectionToPane => "pane.send-selection",
            Self::ExportScrollback => "terminal.export-scrollback",
            Self::PipeScrollbackToPager => "terminal.pipe-scrollback-to-pager",
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
            Self::ScrollViewport(ViewportScroll::PrevPrompt) => "scroll.prev-prompt",
            Self::ScrollViewport(ViewportScroll::NextPrompt) => "scroll.next-prompt",
            Self::CopyMode(CopyModeAction::CursorOnly) => "copy-mode",
            Self::CopyMode(CopyModeAction::Extend(CopyDirection::Left)) => "copy-mode.left",
            Self::CopyMode(CopyModeAction::Extend(CopyDirection::Right)) => "copy-mode.right",
            Self::CopyMode(CopyModeAction::Extend(CopyDirection::Up)) => "copy-mode.up",
            Self::CopyMode(CopyModeAction::Extend(CopyDirection::Down)) => "copy-mode.down",
            Self::NewTab => "tab.new",
            Self::NewWindow => "window.new",
            Self::NewSplitLeft => "split.new-left",
            Self::NewSplitRight => "split.new-right",
            Self::NewSplitUp => "split.new-up",
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
            Self::ToggleTabOverview => "session-overview.toggle",
            Self::ToggleFullscreen => "fullscreen.toggle",
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
            Self::SetTabTitle => "tab.set-title",
            Self::CloseWindow => "window.close",
            Self::Quit => "app.quit",
            Self::ToggleCommandPalette => "command-palette.toggle",
            Self::ToggleQuickTerminal => "quick-terminal.toggle",
            Self::ToggleSecureKeyboardEntry => "secure-keyboard-entry.toggle",
            Self::ToggleSidebar => "sidebar.toggle",
            Self::ToggleAutoApprove => "auto-approve.toggle",
            Self::OpenThemePicker => "theme.open",
            Self::OpenSettings => "settings.open",
            Self::ToggleProcessMonitor => "process-monitor.toggle",
        }
    }

    pub fn from_action_name(name: &str) -> Option<Self> {
        match name {
            "about" => Some(Self::About),
            "preferences" => Some(Self::Preferences),
            "preferences.edit-config-file" => Some(Self::EditConfigFile),
            "config.reload" => Some(Self::ReloadConfig),
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
            "remote.attach" => Some(Self::AttachRemote),
            "pane.send-selection" => Some(Self::SendSelectionToPane),
            "terminal.export-scrollback" => Some(Self::ExportScrollback),
            "terminal.pipe-scrollback-to-pager" => Some(Self::PipeScrollbackToPager),
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
            "scroll.prev-prompt" => Some(Self::ScrollViewport(ViewportScroll::PrevPrompt)),
            "scroll.next-prompt" => Some(Self::ScrollViewport(ViewportScroll::NextPrompt)),
            "copy-mode" => Some(Self::CopyMode(CopyModeAction::CursorOnly)),
            "copy-mode.left" => Some(Self::CopyMode(CopyModeAction::Extend(CopyDirection::Left))),
            "copy-mode.right" => Some(Self::CopyMode(CopyModeAction::Extend(CopyDirection::Right))),
            "copy-mode.up" => Some(Self::CopyMode(CopyModeAction::Extend(CopyDirection::Up))),
            "copy-mode.down" => Some(Self::CopyMode(CopyModeAction::Extend(CopyDirection::Down))),
            "tab.new" => Some(Self::NewTab),
            "window.new" => Some(Self::NewWindow),
            "split.new-left" => Some(Self::NewSplitLeft),
            "split.new-right" => Some(Self::NewSplitRight),
            "split.new-up" => Some(Self::NewSplitUp),
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
            "session-overview.toggle" | "tab-overview.toggle" => Some(Self::ToggleTabOverview),
            "fullscreen.toggle" => Some(Self::ToggleFullscreen),
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
            "tab.set-title" => Some(Self::SetTabTitle),
            "window.close" => Some(Self::CloseWindow),
            "app.quit" => Some(Self::Quit),
            "command-palette.toggle" => Some(Self::ToggleCommandPalette),
            "quick-terminal.toggle" => Some(Self::ToggleQuickTerminal),
            "secure-keyboard-entry.toggle" => Some(Self::ToggleSecureKeyboardEntry),
            "sidebar.toggle" => Some(Self::ToggleSidebar),
            "auto-approve.toggle" => Some(Self::ToggleAutoApprove),
            "theme.open" | "theme-settings.open" => Some(Self::OpenThemePicker),
            "settings.open" => Some(Self::OpenSettings),
            "process-monitor.toggle" => Some(Self::ToggleProcessMonitor),
            _ => None,
        }
    }
}
