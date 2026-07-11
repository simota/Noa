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
        AppCommand::SendSelectionToPane,
        AppCommand::ExportScrollback,
        AppCommand::PipeScrollbackToPager,
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
        AppCommand::ScrollViewport(ViewportScroll::PrevPrompt),
        AppCommand::ScrollViewport(ViewportScroll::NextPrompt),
        AppCommand::NewTab,
        AppCommand::NewWindow,
        AppCommand::NewSplitLeft,
        AppCommand::NewSplitRight,
        AppCommand::NewSplitUp,
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
        AppCommand::ToggleFullscreen,
        AppCommand::CloseTab,
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::CloseWindow,
        AppCommand::Quit,
    ] {
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
    }

    assert_eq!(
        AppCommand::from_menu_id(AppCommand::LEGACY_TOGGLE_TAB_OVERVIEW_MENU_ID),
        Some(AppCommand::ToggleTabOverview)
    );
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
        AppCommand::from_key(
            &Key::Character("m".into()),
            ModifiersState::SUPER | ModifiersState::SHIFT
        ),
        Some(AppCommand::SendSelectionToPane)
    );
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
    // Quick Terminal uses the system-wide `quick-terminal-hotkey` by default.
    // Keeping Cmd+grave out of the in-app defaults prevents one physical key
    // press from toggling twice when the app is already focused.
    assert_eq!(AppCommand::from_cmd_character("`"), None);
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
    let directions = [
        (NamedKey::ArrowLeft, Direction::Left),
        (NamedKey::ArrowRight, Direction::Right),
        (NamedKey::ArrowUp, Direction::Up),
        (NamedKey::ArrowDown, Direction::Down),
    ];
    for (key, direction) in directions {
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(key),
                ModifiersState::SUPER | ModifiersState::CONTROL
            ),
            Some(AppCommand::FocusDirection(direction))
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(key),
                ModifiersState::SUPER | ModifiersState::ALT
            ),
            Some(AppCommand::FocusDirection(direction))
        );
        assert_eq!(
            AppCommand::from_key(
                &Key::Named(key),
                ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            Some(AppCommand::ResizeSplit(direction))
        );
    }
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
    assert_eq!(
        AppCommand::from_key(
            &Key::Character("f".into()),
            ModifiersState::SUPER | ModifiersState::CONTROL
        ),
        Some(AppCommand::ToggleFullscreen)
    );
}

#[test]
fn new_window_binds_to_cmd_n_and_round_trips() {
    let command = AppCommand::NewWindow;
    assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
    assert_eq!(
        AppCommand::from_action_name(command.action_name()),
        Some(command)
    );
    assert_eq!(command.action_name(), "window.new");
    assert_eq!(
        AppCommand::from_cmd_character("n"),
        Some(AppCommand::NewWindow)
    );
    // cmd+n must not shadow cmd+t (New Tab) — they stay distinct.
    assert_eq!(
        AppCommand::from_cmd_character("t"),
        Some(AppCommand::NewTab)
    );
}

#[test]
fn close_window_binds_to_cmd_shift_w_distinct_from_close_tab() {
    // cmd+w closes the tab; cmd+shift+w closes the whole window.
    assert_eq!(
        AppCommand::from_cmd_character("w"),
        Some(AppCommand::CloseTab)
    );
    assert_eq!(
        AppCommand::from_key(
            &Key::Character("w".into()),
            ModifiersState::SUPER | ModifiersState::SHIFT
        ),
        Some(AppCommand::CloseWindow)
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
        ("s", AppCommand::ToggleSidebar),
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
    assert_eq!(
        engine.resolve(&Key::Character("m".into()), cmd_shift),
        Some(AppCommand::SendSelectionToPane)
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
        engine.chord_for(AppCommand::ToggleSidebar).as_deref(),
        Some("cmd+shift+s")
    );
    assert_eq!(
        engine.chord_for(AppCommand::SendSelectionToPane).as_deref(),
        Some("cmd+shift+m")
    );
    assert_eq!(
        engine.chord_for(AppCommand::ToggleFullscreen).as_deref(),
        Some("cmd+ctrl+f")
    );
    assert_eq!(
        engine
            .chord_for(AppCommand::ScrollViewport(ViewportScroll::LineUp))
            .as_deref(),
        Some("shift+arrowup")
    );
    assert_eq!(
        engine
            .chord_for(AppCommand::FocusDirection(Direction::Left))
            .as_deref(),
        Some("cmd+ctrl+arrowleft")
    );
    assert_eq!(
        engine
            .chord_for(AppCommand::ResizeSplit(Direction::Right))
            .as_deref(),
        Some("cmd+ctrl+shift+arrowright")
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
        AppCommand::SendSelectionToPane,
        AppCommand::ExportScrollback,
        AppCommand::PipeScrollbackToPager,
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
        AppCommand::NewWindow,
        AppCommand::NewSplitLeft,
        AppCommand::NewSplitRight,
        AppCommand::NewSplitUp,
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
        AppCommand::ToggleFullscreen,
        AppCommand::CloseTab,
        AppCommand::SelectTab(3),
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::CloseWindow,
        AppCommand::Quit,
        AppCommand::OpenThemePicker,
        AppCommand::OpenSettings,
        AppCommand::ReloadConfig,
    ] {
        assert_eq!(
            AppCommand::from_action_name(command.action_name()),
            Some(command)
        );
    }
    assert_eq!(
        AppCommand::ToggleTabOverview.action_name(),
        "session-overview.toggle"
    );
    assert_eq!(
        AppCommand::from_action_name("tab-overview.toggle"),
        Some(AppCommand::ToggleTabOverview)
    );
    // DEC-1: the pre-split combined overlay's id keeps parsing, aliased to
    // the theme-picker half (the old overlay opened focused on the picker).
    assert_eq!(AppCommand::OpenThemePicker.action_name(), "theme.open");
    assert_eq!(
        AppCommand::from_action_name("theme-settings.open"),
        Some(AppCommand::OpenThemePicker)
    );
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
