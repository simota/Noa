//! Native macOS menu construction.

use muda::{
    CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Key as AcceleratorKey, KeyAccelerator, Modifiers},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{dpi::PhysicalPosition, event_loop::EventLoopProxy, window::Window};

use crate::{AppCommand, FontSizeAction, SearchAction, TerminalAction, UserEvent, ViewportScroll};

const AUTO_APPROVE_MENU_LABEL_OFF: &str = "Auto Approve: Off";
const AUTO_APPROVE_MENU_LABEL_ON: &str = "Auto Approve: On";

const SPLIT_CONTEXT_MENU_ITEMS: &[(AppCommand, &str)] = &[
    (AppCommand::NewSplitLeft, "Add Pane Left"),
    (AppCommand::NewSplitRight, "Add Pane Right"),
    (AppCommand::NewSplitUp, "Add Pane Up"),
    (AppCommand::NewSplitDown, "Add Pane Down"),
    (AppCommand::EqualizeSplits, "Equalize Splits"),
    (AppCommand::ToggleSplitZoom, "Toggle Split Zoom"),
    (AppCommand::ToggleAutoApprove, AUTO_APPROVE_MENU_LABEL_OFF),
    (AppCommand::SendSelectionToPane, "Send Selection to Pane"),
    (AppCommand::SetTabTitle, "Set Tab Title\u{2026}"),
];

const SPLIT_LEFT_ITEM: usize = 0;
const SPLIT_RIGHT_ITEM: usize = 1;
const SPLIT_UP_ITEM: usize = 2;
const SPLIT_DOWN_ITEM: usize = 3;
const EQUALIZE_SPLITS_ITEM: usize = 4;
const TOGGLE_SPLIT_ZOOM_ITEM: usize = 5;
const TOGGLE_AUTO_APPROVE_ITEM: usize = 6;
const SEND_SELECTION_TO_PANE_ITEM: usize = 7;
const SET_TAB_TITLE_ITEM: usize = 8;

fn auto_approve_menu_label(enabled: bool) -> &'static str {
    if enabled {
        AUTO_APPROVE_MENU_LABEL_ON
    } else {
        AUTO_APPROVE_MENU_LABEL_OFF
    }
}

/// Holds the native menu alive for the lifetime of the winit event loop.
pub(crate) struct MacosMenu {
    _menu: Menu,
    split_context_menu: Menu,
    split_context_splits: SplitContextSplitItems,
    /// The "Secure Keyboard Entry" item, retained so its checkmark can track
    /// the toggle state (see [`MacosMenu::set_secure_keyboard_entry_checked`]).
    secure_keyboard_entry: CheckMenuItem,
    auto_approve: CheckMenuItem,
    split_context_auto_approve: CheckMenuItem,
    split_context_send_selection: MenuItem,
    quick_terminal: MenuItem,
    /// The "Scratch Terminal" item (kaizen item 1), retained so its
    /// accelerator can track the keybind engine's effective
    /// `ToggleScratchTerminal` chord (see
    /// [`MacosMenu::set_scratch_terminal_chord`]) — same pattern as
    /// `sidebar` below, since `scratch-terminal-key` is an in-app-only
    /// chord, not a global hotkey like `quick_terminal_hotkey`.
    scratch_terminal: MenuItem,
    /// The "Sidebar" item, retained so its accelerator can track the
    /// `sidebar-hotkey` config (see [`MacosMenu::set_sidebar_hotkey`]).
    sidebar: MenuItem,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SplitContextMenuEnabled {
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) up: bool,
    pub(crate) down: bool,
}

struct SplitContextSplitItems {
    left: MenuItem,
    right: MenuItem,
    up: MenuItem,
    down: MenuItem,
}

type SplitContextMenuParts = (Menu, SplitContextSplitItems, CheckMenuItem, MenuItem);

impl SplitContextSplitItems {
    fn set_enabled(&self, enabled: SplitContextMenuEnabled) {
        self.left.set_enabled(enabled.left);
        self.right.set_enabled(enabled.right);
        self.up.set_enabled(enabled.up);
        self.down.set_enabled(enabled.down);
    }
}

impl MacosMenu {
    pub(crate) fn install(
        proxy: EventLoopProxy<UserEvent>,
        quick_terminal_hotkey: Option<&str>,
        scratch_terminal_chord: Option<&str>,
        sidebar_chord: Option<&str>,
    ) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let split_context_menu = build_split_context_menu()?;
        let app_menu = Submenu::with_id("noa.menu.app", "Noa", true);

        let about = MenuItem::with_id(AppCommand::About.menu_id(), "About Noa", true, None);
        let (preferences_command, preferences_label, preferences_enabled, preferences_accelerator) =
            preferences_menu_item_spec();
        let preferences = MenuItem::with_id(
            preferences_command.menu_id(),
            preferences_label,
            preferences_enabled,
            Some(preferences_accelerator),
        );
        let (edit_config_command, edit_config_label, edit_config_enabled, edit_config_accelerator) =
            edit_config_file_menu_item_spec();
        let edit_config_file = MenuItem::with_id(
            edit_config_command.menu_id(),
            edit_config_label,
            edit_config_enabled,
            edit_config_accelerator,
        );
        let (open_theme_command, open_theme_label, open_theme_enabled, open_theme_accelerator) =
            open_theme_picker_menu_item_spec();
        let open_theme_picker = MenuItem::with_id(
            open_theme_command.menu_id(),
            open_theme_label,
            open_theme_enabled,
            Some(open_theme_accelerator),
        );
        let (fullscreen_command, fullscreen_label, fullscreen_enabled, fullscreen_accelerator) =
            fullscreen_menu_item_spec();
        let (open_settings_command, open_settings_label) = open_settings_menu_item_spec();
        let secure_keyboard_entry = CheckMenuItem::with_id(
            AppCommand::ToggleSecureKeyboardEntry.menu_id(),
            "Secure Keyboard Entry",
            true,
            false,
            None,
        );
        let quick_terminal = MenuItem::with_id(
            AppCommand::ToggleQuickTerminal.menu_id(),
            "Quick Terminal",
            true,
            quick_terminal_accelerator(quick_terminal_hotkey),
        );
        let scratch_terminal = MenuItem::with_id(
            AppCommand::ToggleScratchTerminal.menu_id(),
            "Scratch Terminal",
            true,
            scratch_terminal_accelerator(scratch_terminal_chord),
        );
        let sidebar = MenuItem::with_id(
            AppCommand::ToggleSidebar.menu_id(),
            "Sidebar",
            true,
            sidebar_accelerator(sidebar_chord),
        );
        let auto_approve = CheckMenuItem::with_id(
            AppCommand::ToggleAutoApprove.menu_id(),
            auto_approve_menu_label(false),
            true,
            false,
            None,
        );
        let close_tab = MenuItem::with_id(
            AppCommand::CloseTab.menu_id(),
            "Close Tab",
            true,
            Some(cmd_accelerator(Code::KeyW)),
        );
        let quit = MenuItem::with_id(
            AppCommand::Quit.menu_id(),
            "Quit Noa",
            true,
            Some(cmd_accelerator(Code::KeyQ)),
        );
        let separator_one = PredefinedMenuItem::separator();
        let separator_two = PredefinedMenuItem::separator();
        let separator_three = PredefinedMenuItem::separator();
        let separator_secure = PredefinedMenuItem::separator();
        let file_new_tab = MenuItem::with_id(
            AppCommand::NewTab.menu_id(),
            "New Tab",
            true,
            Some(cmd_accelerator(Code::KeyT)),
        );
        let file_new_window = MenuItem::with_id(
            AppCommand::NewWindow.menu_id(),
            "New Window",
            true,
            Some(cmd_accelerator(Code::KeyN)),
        );
        let file_export_scrollback = MenuItem::with_id(
            AppCommand::ExportScrollback.menu_id(),
            "Export Scrollback to File",
            true,
            None,
        );
        let file_pipe_scrollback_to_pager = MenuItem::with_id(
            AppCommand::PipeScrollbackToPager.menu_id(),
            "Pipe Scrollback to Pager",
            true,
            None,
        );
        let file_close_tab = MenuItem::with_id(
            AppCommand::CloseTab.menu_id(),
            "Close Tab",
            true,
            Some(cmd_accelerator(Code::KeyW)),
        );
        let file_close_window = MenuItem::with_id(
            AppCommand::CloseWindow.menu_id(),
            "Close Window",
            true,
            Some(cmd_shift_accelerator(Code::KeyW)),
        );
        let file_menu = Submenu::with_id_and_items(
            "noa.menu.file",
            "File",
            true,
            &[
                &file_new_tab,
                &file_new_window,
                &PredefinedMenuItem::separator(),
                &file_export_scrollback,
                &file_pipe_scrollback_to_pager,
                &PredefinedMenuItem::separator(),
                &file_close_tab,
                &file_close_window,
            ],
        )?;
        let edit_menu = Submenu::with_id_and_items(
            "noa.menu.edit",
            "Edit",
            true,
            &[
                &disabled_item("noa.edit.undo", "Undo"),
                &PredefinedMenuItem::separator(),
                &disabled_item("noa.edit.cut", "Cut"),
                &MenuItem::with_id(
                    AppCommand::Copy.menu_id(),
                    "Copy",
                    true,
                    Some(cmd_accelerator(Code::KeyC)),
                ),
                &MenuItem::with_id(
                    AppCommand::Paste.menu_id(),
                    "Paste",
                    true,
                    Some(cmd_accelerator(Code::KeyV)),
                ),
                &MenuItem::with_id(
                    AppCommand::Terminal(TerminalAction::SelectAll).menu_id(),
                    "Select All",
                    true,
                    Some(cmd_accelerator(Code::KeyA)),
                ),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    AppCommand::Search(SearchAction::Find).menu_id(),
                    "Find",
                    true,
                    Some(cmd_accelerator(Code::KeyF)),
                ),
                &MenuItem::with_id(
                    AppCommand::Search(SearchAction::FindNext).menu_id(),
                    "Find Next",
                    true,
                    Some(cmd_accelerator(Code::KeyG)),
                ),
                &MenuItem::with_id(
                    AppCommand::Search(SearchAction::FindPrevious).menu_id(),
                    "Find Previous",
                    true,
                    Some(cmd_shift_accelerator(Code::KeyG)),
                ),
                &MenuItem::with_id(
                    AppCommand::Search(SearchAction::Clear).menu_id(),
                    "Clear Search",
                    true,
                    None,
                ),
            ],
        )?;
        let view_menu = Submenu::with_id_and_items(
            "noa.menu.view",
            "View",
            true,
            &[
                &MenuItem::with_id(
                    AppCommand::Terminal(TerminalAction::Clear).menu_id(),
                    "Clear",
                    true,
                    Some(cmd_accelerator(Code::KeyK)),
                ),
                &MenuItem::with_id(
                    AppCommand::Terminal(TerminalAction::ClearScrollback).menu_id(),
                    "Clear Scrollback",
                    true,
                    None,
                ),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    AppCommand::FontSize(FontSizeAction::Increase).menu_id(),
                    "Increase Font Size",
                    true,
                    Some(cmd_accelerator(Code::Equal)),
                ),
                &MenuItem::with_id(
                    AppCommand::FontSize(FontSizeAction::Decrease).menu_id(),
                    "Decrease Font Size",
                    true,
                    Some(cmd_accelerator(Code::Minus)),
                ),
                &MenuItem::with_id(
                    AppCommand::FontSize(FontSizeAction::Reset).menu_id(),
                    "Reset Font Size",
                    true,
                    Some(cmd_accelerator(Code::Digit0)),
                ),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::LineUp).menu_id(),
                    "Scroll Line Up",
                    true,
                    Some(shift_accelerator(Code::ArrowUp)),
                ),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::LineDown).menu_id(),
                    "Scroll Line Down",
                    true,
                    Some(shift_accelerator(Code::ArrowDown)),
                ),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::PageUp).menu_id(),
                    "Scroll Page Up",
                    true,
                    Some(shift_accelerator(Code::PageUp)),
                ),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::PageDown).menu_id(),
                    "Scroll Page Down",
                    true,
                    Some(shift_accelerator(Code::PageDown)),
                ),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::Top).menu_id(),
                    "Scroll to Top",
                    true,
                    Some(shift_accelerator(Code::Home)),
                ),
                &MenuItem::with_id(
                    AppCommand::ScrollViewport(ViewportScroll::Bottom).menu_id(),
                    "Scroll to Bottom",
                    true,
                    Some(shift_accelerator(Code::End)),
                ),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    AppCommand::ToggleTabOverview.menu_id(),
                    "Session Overview",
                    true,
                    Some(cmd_shift_accelerator(Code::KeyO)),
                ),
                &MenuItem::with_id(
                    AppCommand::ToggleCommandPalette.menu_id(),
                    "Command Palette",
                    true,
                    Some(cmd_shift_accelerator(Code::KeyP)),
                ),
                &MenuItem::with_id(
                    open_settings_command.menu_id(),
                    open_settings_label,
                    true,
                    None,
                ),
                &open_theme_picker,
                &quick_terminal,
                &scratch_terminal,
                &sidebar,
                &auto_approve,
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    fullscreen_command.menu_id(),
                    fullscreen_label,
                    fullscreen_enabled,
                    Some(fullscreen_accelerator),
                ),
            ],
        )?;
        let window_menu = Submenu::with_id_and_items(
            "noa.menu.window",
            "Window",
            true,
            &[
                &disabled_item("noa.window.minimize", "Minimize"),
                &disabled_item("noa.window.zoom", "Zoom"),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(
                    AppCommand::PrevTab.menu_id(),
                    "Previous Tab",
                    true,
                    Some(cmd_shift_accelerator(Code::BracketLeft)),
                ),
                &MenuItem::with_id(
                    AppCommand::NextTab.menu_id(),
                    "Next Tab",
                    true,
                    Some(cmd_shift_accelerator(Code::BracketRight)),
                ),
                &PredefinedMenuItem::separator(),
                // No accelerator: unbound by default, matching Ghostty's
                // `prompt_surface_title`.
                &MenuItem::with_id(
                    AppCommand::SetTabTitle.menu_id(),
                    "Set Tab Title\u{2026}",
                    true,
                    None,
                ),
            ],
        )?;
        let help_menu = Submenu::with_id_and_items(
            "noa.menu.help",
            "Help",
            true,
            &[&disabled_item("noa.help.noa-help", "Noa Help")],
        )?;

        app_menu.append_items(&[
            &about,
            &separator_one,
            &preferences,
            &edit_config_file,
            &separator_secure,
            &secure_keyboard_entry,
            &separator_two,
            &close_tab,
            &separator_three,
            &quit,
        ])?;
        menu.append_items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ])?;

        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id.as_ref();
            let Some(command) = AppCommand::from_menu_id(id) else {
                log::debug!("ignoring unknown macOS menu id: {id}");
                return;
            };
            if proxy.send_event(UserEvent::AppCommand(command)).is_err() {
                log::debug!("dropping macOS menu command after event loop closed: {id}");
            }
        }));

        menu.init_for_nsapp();
        window_menu.set_as_windows_menu_for_nsapp();
        help_menu.set_as_help_menu_for_nsapp();

        Ok(Self {
            _menu: menu,
            split_context_menu: split_context_menu.0,
            split_context_splits: split_context_menu.1,
            split_context_auto_approve: split_context_menu.2,
            split_context_send_selection: split_context_menu.3,
            secure_keyboard_entry,
            auto_approve,
            quick_terminal,
            scratch_terminal,
            sidebar,
        })
    }

    /// Reflect the current Secure Keyboard Entry state in the menu checkmark.
    pub(crate) fn set_secure_keyboard_entry_checked(&self, checked: bool) {
        self.secure_keyboard_entry.set_checked(checked);
    }

    /// Reflect the focused tab's Auto Approve state in both the app menu and
    /// pane context menu so the toggle is discoverable without opening the
    /// sidebar.
    pub(crate) fn set_auto_approve_checked(&self, checked: bool) {
        let label = auto_approve_menu_label(checked);
        self.auto_approve.set_checked(checked);
        self.auto_approve.set_text(label);
        self.split_context_auto_approve.set_checked(checked);
        self.split_context_auto_approve.set_text(label);
    }

    /// Reflect the configured system-wide Quick Terminal hotkey in the native
    /// menu's shortcut column. The actual registration stays in the Carbon /
    /// CGEventTap hotkey path.
    pub(crate) fn set_quick_terminal_hotkey(&self, hotkey: Option<&str>) {
        if let Err(err) = self
            .quick_terminal
            .set_accelerator(quick_terminal_accelerator(hotkey))
        {
            log::warn!("failed to update Quick Terminal menu accelerator: {err}");
        }
    }

    /// Reflect the keybind engine's *effective* `ToggleSidebar` chord
    /// (`KeybindEngine::chord_for`) in the Sidebar menu item's shortcut
    /// column. AppKit dispatches menu key equivalents before winit sees the
    /// keypress, so the accelerator must track the engine — not the raw
    /// `sidebar-hotkey` value — or a stale/overridden chord would keep
    /// toggling the sidebar through menu dispatch.
    pub(crate) fn set_sidebar_chord(&self, chord: Option<&str>) {
        // muda 0.19's `set_accelerator(None)` never touches the native
        // NSMenuItem (its macOS impl only writes when the accelerator is
        // `Some`), so an unbound/unrepresentable chord must clear the key
        // equivalent explicitly via an empty-key `KeyAccelerator` — otherwise
        // the previous chord keeps firing through AppKit menu dispatch.
        let result = match sidebar_accelerator(chord) {
            Some(accelerator) => self.sidebar.set_accelerator(Some(accelerator)),
            None => self.sidebar.set_key_accelerator(Some(KeyAccelerator::new(
                None,
                AcceleratorKey::Character(String::new()),
            ))),
        };
        if let Err(err) = result {
            log::warn!("failed to update Sidebar menu accelerator: {err}");
        }
    }

    /// Reflect the keybind engine's *effective* `ToggleScratchTerminal`
    /// chord in the Scratch Terminal menu item's shortcut column — same
    /// reasoning as [`Self::set_sidebar_chord`] (`scratch-terminal-key` is
    /// in-app-only, so the accelerator must track the engine, not the raw
    /// config value).
    pub(crate) fn set_scratch_terminal_chord(&self, chord: Option<&str>) {
        let result = match scratch_terminal_accelerator(chord) {
            Some(accelerator) => self.scratch_terminal.set_accelerator(Some(accelerator)),
            None => self
                .scratch_terminal
                .set_key_accelerator(Some(KeyAccelerator::new(
                    None,
                    AcceleratorKey::Character(String::new()),
                ))),
        };
        if let Err(err) = result {
            log::warn!("failed to update Scratch Terminal menu accelerator: {err}");
        }
    }

    pub(crate) fn show_split_context_menu(
        &self,
        window: &Window,
        position: Option<PhysicalPosition<f64>>,
        auto_approve_enabled: bool,
        split_enabled: SplitContextMenuEnabled,
        send_selection_enabled: bool,
    ) -> anyhow::Result<()> {
        self.set_auto_approve_checked(auto_approve_enabled);
        self.split_context_splits.set_enabled(split_enabled);
        self.split_context_send_selection
            .set_enabled(send_selection_enabled);
        let raw_handle = window.window_handle()?.as_raw();
        let ns_view = match raw_handle {
            RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr(),
            _ => anyhow::bail!("expected AppKit window handle"),
        };
        let position = position.map(|position| {
            muda::dpi::PhysicalPosition {
                x: position.x,
                y: position.y,
            }
            .into()
        });

        // SAFETY: The NSView pointer comes from winit's live AppKit window
        // handle, and this is called from the main winit event loop thread.
        unsafe {
            self.split_context_menu
                .show_context_menu_for_nsview(ns_view, position);
        }
        Ok(())
    }
}

fn build_split_context_menu() -> anyhow::Result<SplitContextMenuParts> {
    let split_left = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_LEFT_ITEM]);
    let split_right = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_RIGHT_ITEM]);
    let split_up = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_UP_ITEM]);
    let split_down = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_DOWN_ITEM]);
    let separator = PredefinedMenuItem::separator();
    let equalize = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[EQUALIZE_SPLITS_ITEM]);
    let toggle_zoom = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[TOGGLE_SPLIT_ZOOM_ITEM]);
    let auto_approve_spec = SPLIT_CONTEXT_MENU_ITEMS[TOGGLE_AUTO_APPROVE_ITEM];
    let auto_approve = CheckMenuItem::with_id(
        auto_approve_spec.0.menu_id(),
        auto_approve_spec.1,
        true,
        false,
        None,
    );
    // Ghostty parity: "Change Title" lives on the surface context menu too.
    let title_separator = PredefinedMenuItem::separator();
    let send_selection = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SEND_SELECTION_TO_PANE_ITEM]);
    let set_tab_title = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SET_TAB_TITLE_ITEM]);

    let menu = Menu::with_id_and_items(
        "noa.menu.split-context",
        &[
            &split_left,
            &split_right,
            &split_up,
            &split_down,
            &separator,
            &equalize,
            &toggle_zoom,
            &auto_approve,
            &title_separator,
            &send_selection,
            &set_tab_title,
        ],
    )?;
    Ok((
        menu,
        SplitContextSplitItems {
            left: split_left,
            right: split_right,
            up: split_up,
            down: split_down,
        },
        auto_approve,
        send_selection,
    ))
}

fn context_menu_item((command, label): (AppCommand, &'static str)) -> MenuItem {
    MenuItem::with_id(command.menu_id(), label, true, None)
}

fn preferences_menu_item_spec() -> (AppCommand, &'static str, bool, Accelerator) {
    (
        AppCommand::Preferences,
        "Settings...",
        true,
        cmd_accelerator(Code::Comma),
    )
}

/// R-23: `EditConfigFile`'s menu item spec — same "identity + label +
/// enabled" shape as [`preferences_menu_item_spec`], but no accelerator (the
/// spec deliberately leaves this unbound: Cmd+, now means something
/// different post-R-22, so no chord should collide with a user's muscle
/// memory here — reachable via the menu item or a config keybind only).
fn edit_config_file_menu_item_spec() -> (AppCommand, &'static str, bool, Option<Accelerator>) {
    (
        AppCommand::EditConfigFile,
        "Edit Config File...",
        true,
        None,
    )
}

/// R-24: `OpenThemePicker`'s menu item spec — default `cmd+shift+,`
/// (verified unused in [`KeybindEngine::default`]'s existing chord set).
fn open_theme_picker_menu_item_spec() -> (AppCommand, &'static str, bool, Accelerator) {
    (
        AppCommand::OpenThemePicker,
        "Open Theme...",
        true,
        cmd_shift_accelerator(Code::Comma),
    )
}

fn fullscreen_menu_item_spec() -> (AppCommand, &'static str, bool, Accelerator) {
    (
        AppCommand::ToggleFullscreen,
        "Toggle Full Screen",
        true,
        cmd_ctrl_accelerator(Code::KeyF),
    )
}

/// R-2: the View-menu "Open Settings…" item — label matches the existing
/// command-palette entry (`command_palette.rs`), deliberately unbound (no
/// accelerator) so it can't be confused with the existing ⌘,
/// (`AppCommand::Preferences`, unchanged, opens the config file in an
/// external editor) — that decision is locked (`theme-settings-ui.md` R-1)
/// and this item does not touch it.
fn open_settings_menu_item_spec() -> (AppCommand, &'static str) {
    (AppCommand::OpenSettings, "Open Settings\u{2026}")
}

fn disabled_item(id: &'static str, text: &'static str) -> MenuItem {
    MenuItem::with_id(id, text, false, None)
}

fn quick_terminal_accelerator(hotkey: Option<&str>) -> Option<Accelerator> {
    hotkey.and_then(accelerator_from_hotkey)
}

/// The Sidebar item's accelerator for the keybind engine's effective
/// `ToggleSidebar` chord (`KeybindEngine::chord_for`). `None` (unbound)
/// yields no accelerator; a chord the accelerator path can't represent also
/// yields none (the keybind engine still handles the keypress; only the
/// menu's shortcut column goes blank — never a chord the engine would route
/// elsewhere).
fn sidebar_accelerator(chord: Option<&str>) -> Option<Accelerator> {
    chord.and_then(accelerator_from_hotkey)
}

/// The Scratch Terminal item's accelerator for the keybind engine's
/// effective `ToggleScratchTerminal` chord — same shape as
/// [`sidebar_accelerator`] (an in-app-only chord, not a global hotkey).
fn scratch_terminal_accelerator(chord: Option<&str>) -> Option<Accelerator> {
    chord.and_then(accelerator_from_hotkey)
}

fn accelerator_from_hotkey(hotkey: &str) -> Option<Accelerator> {
    let chord = crate::macos_hotkey::parse_hotkey(hotkey)?;
    Some(Accelerator::new(
        Some(accelerator_modifiers(chord.modifiers)),
        accelerator_code(chord.keycode)?,
    ))
}

const CARBON_CMD_KEY: u32 = 0x0100;
const CARBON_SHIFT_KEY: u32 = 0x0200;
const CARBON_OPTION_KEY: u32 = 0x0800;
const CARBON_CONTROL_KEY: u32 = 0x1000;

fn accelerator_modifiers(carbon_modifiers: u32) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    if carbon_modifiers & CARBON_CMD_KEY != 0 {
        modifiers |= Modifiers::SUPER;
    }
    if carbon_modifiers & CARBON_SHIFT_KEY != 0 {
        modifiers |= Modifiers::SHIFT;
    }
    if carbon_modifiers & CARBON_OPTION_KEY != 0 {
        modifiers |= Modifiers::ALT;
    }
    if carbon_modifiers & CARBON_CONTROL_KEY != 0 {
        modifiers |= Modifiers::CONTROL;
    }
    modifiers
}

fn accelerator_code(carbon_keycode: u32) -> Option<Code> {
    Some(match carbon_keycode {
        0x00 => Code::KeyA,
        0x01 => Code::KeyS,
        0x02 => Code::KeyD,
        0x03 => Code::KeyF,
        0x04 => Code::KeyH,
        0x05 => Code::KeyG,
        0x06 => Code::KeyZ,
        0x07 => Code::KeyX,
        0x08 => Code::KeyC,
        0x09 => Code::KeyV,
        0x0B => Code::KeyB,
        0x0C => Code::KeyQ,
        0x0D => Code::KeyW,
        0x0E => Code::KeyE,
        0x0F => Code::KeyR,
        0x10 => Code::KeyY,
        0x11 => Code::KeyT,
        0x12 => Code::Digit1,
        0x13 => Code::Digit2,
        0x14 => Code::Digit3,
        0x15 => Code::Digit4,
        0x16 => Code::Digit6,
        0x17 => Code::Digit5,
        0x18 => Code::Equal,
        0x19 => Code::Digit9,
        0x1A => Code::Digit7,
        0x1B => Code::Minus,
        0x1C => Code::Digit8,
        0x1D => Code::Digit0,
        0x1E => Code::BracketRight,
        0x1F => Code::KeyO,
        0x20 => Code::KeyU,
        0x21 => Code::BracketLeft,
        0x22 => Code::KeyI,
        0x23 => Code::KeyP,
        0x25 => Code::KeyL,
        0x26 => Code::KeyJ,
        0x28 => Code::KeyK,
        0x29 => Code::Semicolon,
        0x2A => Code::Backslash,
        0x5D => Code::IntlYen,
        0x5E => Code::IntlRo,
        0x2B => Code::Comma,
        0x2C => Code::Slash,
        0x2D => Code::KeyN,
        0x2E => Code::KeyM,
        0x2F => Code::Period,
        0x32 => Code::Backquote,
        0x24 => Code::Enter,
        0x30 => Code::Tab,
        0x31 => Code::Space,
        0x35 => Code::Escape,
        _ => return None,
    })
}

fn cmd_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SUPER), code)
}

fn cmd_shift_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SUPER | Modifiers::SHIFT), code)
}

fn cmd_ctrl_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SUPER | Modifiers::CONTROL), code)
}

fn shift_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SHIFT), code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_context_menu_items_use_app_commands() {
        assert_eq!(
            SPLIT_CONTEXT_MENU_ITEMS,
            &[
                (AppCommand::NewSplitLeft, "Add Pane Left"),
                (AppCommand::NewSplitRight, "Add Pane Right"),
                (AppCommand::NewSplitUp, "Add Pane Up"),
                (AppCommand::NewSplitDown, "Add Pane Down"),
                (AppCommand::EqualizeSplits, "Equalize Splits"),
                (AppCommand::ToggleSplitZoom, "Toggle Split Zoom"),
                (AppCommand::ToggleAutoApprove, "Auto Approve: Off"),
                (AppCommand::SendSelectionToPane, "Send Selection to Pane"),
                (AppCommand::SetTabTitle, "Set Tab Title\u{2026}"),
            ]
        );
        for (command, _) in SPLIT_CONTEXT_MENU_ITEMS {
            assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(*command));
        }
    }

    #[test]
    fn split_context_split_items_apply_enabled_state() {
        let items = SplitContextSplitItems {
            left: context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_LEFT_ITEM]),
            right: context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_RIGHT_ITEM]),
            up: context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_UP_ITEM]),
            down: context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[SPLIT_DOWN_ITEM]),
        };

        items.set_enabled(SplitContextMenuEnabled {
            left: false,
            right: true,
            up: false,
            down: true,
        });

        assert!(!items.left.is_enabled());
        assert!(items.right.is_enabled());
        assert!(!items.up.is_enabled());
        assert!(items.down.is_enabled());
    }

    #[test]
    fn preferences_menu_item_is_enabled_and_routes_to_preferences() {
        let (command, label, enabled, accelerator) = preferences_menu_item_spec();

        assert_eq!(command, AppCommand::Preferences);
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(label, "Settings...");
        assert!(enabled);
        assert_eq!(accelerator, cmd_accelerator(Code::Comma));
    }

    #[test]
    fn fullscreen_menu_item_is_enabled_and_routes_to_toggle_fullscreen() {
        let (command, label, enabled, accelerator) = fullscreen_menu_item_spec();

        assert_eq!(command, AppCommand::ToggleFullscreen);
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(label, "Toggle Full Screen");
        assert!(enabled);
        assert_eq!(accelerator, cmd_ctrl_accelerator(Code::KeyF));
    }

    // R-2/AC-4: a non-empty, round-tripping menu id — Critical#2's "add a
    // native menu item" without touching the existing ⌘,/Preferences
    // wiring (AC-5 is a diff/code-review check, not exercisable here).
    #[test]
    fn open_settings_menu_item_routes_to_open_settings_and_stays_unbound() {
        let (command, label) = open_settings_menu_item_spec();

        assert_eq!(command, AppCommand::OpenSettings);
        assert_ne!(command.menu_id(), "");
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(label, "Open Settings\u{2026}");
        // Unbound: distinct from the existing ⌘,/Preferences item, which
        // this addition must not alter.
        assert_ne!(command, AppCommand::Preferences);
    }

    // AC-32 (R-23): `EditConfigFile`'s menu item spec round-trips its id and
    // carries no accelerator (Cmd+, now means something else post-R-22).
    #[test]
    fn edit_config_file_menu_item_is_enabled_and_has_no_accelerator() {
        let (command, label, enabled, accelerator) = edit_config_file_menu_item_spec();

        assert_eq!(command, AppCommand::EditConfigFile);
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(label, "Edit Config File...");
        assert!(enabled);
        assert_eq!(accelerator, None);
    }

    // AC-33 (R-24) at the menu layer: `OpenThemePicker`'s menu item spec
    // round-trips its id and carries the `cmd+shift+,` accelerator.
    #[test]
    fn open_theme_picker_menu_item_is_enabled_and_routes_to_open_theme_picker() {
        let (command, label, enabled, accelerator) = open_theme_picker_menu_item_spec();

        assert_eq!(command, AppCommand::OpenThemePicker);
        assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(command));
        assert_eq!(label, "Open Theme...");
        assert!(enabled);
        assert_eq!(accelerator, cmd_shift_accelerator(Code::Comma));
    }

    #[test]
    fn quick_terminal_hotkey_maps_to_menu_accelerator() {
        let accelerator = accelerator_from_hotkey("cmd+shift+backslash").expect("accelerator");
        assert_eq!(accelerator.modifiers(), Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(accelerator.key(), Code::Backslash);

        let accelerator = accelerator_from_hotkey("cmd+shift+yen").expect("accelerator");
        assert_eq!(accelerator.modifiers(), Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(accelerator.key(), Code::IntlYen);

        let accelerator = accelerator_from_hotkey("cmd+shift+intl-ro").expect("accelerator");
        assert_eq!(accelerator.modifiers(), Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(accelerator.key(), Code::IntlRo);

        let accelerator = accelerator_from_hotkey("ctrl+alt+shift+t").expect("accelerator");
        assert_eq!(
            accelerator.modifiers(),
            Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT
        );
        assert_eq!(accelerator.key(), Code::KeyT);
    }

    #[test]
    fn quick_terminal_hotkey_accelerator_ignores_disabled_or_invalid_chords() {
        assert!(quick_terminal_accelerator(None).is_none());
        assert!(quick_terminal_accelerator(Some("")).is_none());
        assert!(quick_terminal_accelerator(Some("cmd+unknown-key")).is_none());
        assert!(quick_terminal_accelerator(Some("cmd+t+x")).is_none());
    }

    // The accelerator tracks the engine's effective `ToggleSidebar` chord:
    // unbound → no accelerator; the engine-default chord and a custom chord
    // both map through; a chord the accelerator path can't represent leaves
    // the shortcut column blank (never a chord the engine routes elsewhere).
    #[test]
    fn sidebar_accelerator_tracks_the_effective_engine_chord() {
        assert!(sidebar_accelerator(None).is_none());
        assert_eq!(
            sidebar_accelerator(Some("cmd+shift+s")),
            Some(cmd_shift_accelerator(Code::KeyS))
        );
        assert_eq!(
            sidebar_accelerator(Some("cmd+alt+b")),
            accelerator_from_hotkey("cmd+alt+b")
        );
        assert!(sidebar_accelerator(Some("cmd+unknown-key")).is_none());
    }

    // Kaizen item 1: `ToggleScratchTerminal` now has a real menu id, and its
    // accelerator tracks the keybind engine's effective chord exactly like
    // `ToggleSidebar`'s does (same underlying `accelerator_from_hotkey`
    // parser, same in-app-only semantics).
    #[test]
    fn scratch_terminal_menu_item_has_a_real_menu_id() {
        assert_ne!(AppCommand::ToggleScratchTerminal.menu_id(), "");
        assert_eq!(
            AppCommand::from_menu_id(AppCommand::ToggleScratchTerminal.menu_id()),
            Some(AppCommand::ToggleScratchTerminal)
        );
    }

    #[test]
    fn scratch_terminal_accelerator_tracks_the_effective_engine_chord() {
        assert!(scratch_terminal_accelerator(None).is_none());
        assert_eq!(
            scratch_terminal_accelerator(Some("cmd+shift+t")),
            Some(cmd_shift_accelerator(Code::KeyT))
        );
        assert_eq!(
            scratch_terminal_accelerator(Some("cmd+alt+b")),
            accelerator_from_hotkey("cmd+alt+b")
        );
        assert!(scratch_terminal_accelerator(Some("cmd+unknown-key")).is_none());
    }
}
