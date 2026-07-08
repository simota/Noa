//! Native macOS menu construction.

use muda::{
    CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{dpi::PhysicalPosition, event_loop::EventLoopProxy, window::Window};

use crate::{AppCommand, FontSizeAction, SearchAction, TerminalAction, UserEvent, ViewportScroll};

const SPLIT_CONTEXT_MENU_ITEMS: &[(AppCommand, &str)] = &[
    (AppCommand::NewSplitRight, "Split Right"),
    (AppCommand::NewSplitDown, "Split Down"),
    (AppCommand::EqualizeSplits, "Equalize Splits"),
    (AppCommand::ToggleSplitZoom, "Toggle Split Zoom"),
    (AppCommand::ToggleAutoApprove, "Auto Approve: Off"),
    (AppCommand::SetTabTitle, "Set Tab Title\u{2026}"),
];

fn auto_approve_menu_label(enabled: bool) -> &'static str {
    if enabled {
        "Auto Approve: On"
    } else {
        "Auto Approve: Off"
    }
}

/// Holds the native menu alive for the lifetime of the winit event loop.
pub(crate) struct MacosMenu {
    _menu: Menu,
    split_context_menu: Menu,
    /// The "Secure Keyboard Entry" item, retained so its checkmark can track
    /// the toggle state (see [`MacosMenu::set_secure_keyboard_entry_checked`]).
    secure_keyboard_entry: CheckMenuItem,
    auto_approve: CheckMenuItem,
    split_context_auto_approve: CheckMenuItem,
}

impl MacosMenu {
    pub(crate) fn install(proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
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
        let secure_keyboard_entry = CheckMenuItem::with_id(
            AppCommand::ToggleSecureKeyboardEntry.menu_id(),
            "Secure Keyboard Entry",
            true,
            false,
            None,
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
                // No accelerator: the quick terminal is driven by the global
                // `quick-terminal-hotkey` (a system-wide Carbon hotkey), which
                // muda's app-local accelerators can't represent.
                &MenuItem::with_id(
                    AppCommand::ToggleQuickTerminal.menu_id(),
                    "Quick Terminal",
                    true,
                    None,
                ),
                &MenuItem::with_id(
                    AppCommand::ToggleSidebar.menu_id(),
                    "Sidebar",
                    true,
                    Some(cmd_shift_accelerator(Code::KeyS)),
                ),
                &auto_approve,
                &PredefinedMenuItem::separator(),
                &disabled_item("noa.view.toggle-full-screen", "Toggle Full Screen"),
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
            split_context_auto_approve: split_context_menu.1,
            split_context_menu: split_context_menu.0,
            secure_keyboard_entry,
            auto_approve,
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

    pub(crate) fn show_split_context_menu(
        &self,
        window: &Window,
        position: Option<PhysicalPosition<f64>>,
        auto_approve_enabled: bool,
    ) -> anyhow::Result<()> {
        self.set_auto_approve_checked(auto_approve_enabled);
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

fn build_split_context_menu() -> anyhow::Result<(Menu, CheckMenuItem)> {
    let split_right = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[0]);
    let split_down = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[1]);
    let separator = PredefinedMenuItem::separator();
    let equalize = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[2]);
    let toggle_zoom = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[3]);
    let auto_approve = CheckMenuItem::with_id(
        AppCommand::ToggleAutoApprove.menu_id(),
        auto_approve_menu_label(false),
        true,
        false,
        None,
    );
    // Ghostty parity: "Change Title" lives on the surface context menu too.
    let title_separator = PredefinedMenuItem::separator();
    let set_tab_title = context_menu_item(SPLIT_CONTEXT_MENU_ITEMS[5]);

    let menu = Menu::with_id_and_items(
        "noa.menu.split-context",
        &[
            &split_right,
            &split_down,
            &separator,
            &equalize,
            &toggle_zoom,
            &auto_approve,
            &title_separator,
            &set_tab_title,
        ],
    )?;
    Ok((menu, auto_approve))
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

fn disabled_item(id: &'static str, text: &'static str) -> MenuItem {
    MenuItem::with_id(id, text, false, None)
}

fn cmd_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SUPER), code)
}

fn cmd_shift_accelerator(code: Code) -> Accelerator {
    Accelerator::new(Some(Modifiers::SUPER | Modifiers::SHIFT), code)
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
                (AppCommand::NewSplitRight, "Split Right"),
                (AppCommand::NewSplitDown, "Split Down"),
                (AppCommand::EqualizeSplits, "Equalize Splits"),
                (AppCommand::ToggleSplitZoom, "Toggle Split Zoom"),
                (AppCommand::ToggleAutoApprove, "Auto Approve: Off"),
                (AppCommand::SetTabTitle, "Set Tab Title\u{2026}"),
            ]
        );
        for (command, _) in SPLIT_CONTEXT_MENU_ITEMS {
            assert_eq!(AppCommand::from_menu_id(command.menu_id()), Some(*command));
        }
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
}
