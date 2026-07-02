//! Native macOS menu construction.

use muda::{
    Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};
use winit::event_loop::EventLoopProxy;

use crate::{AppCommand, SearchAction, UserEvent, ViewportScroll};

/// Holds the native menu alive for the lifetime of the winit event loop.
pub(crate) struct MacosMenu {
    _menu: Menu,
}

impl MacosMenu {
    pub(crate) fn install(proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let app_menu = Submenu::with_id("noa.menu.app", "noa", true);

        let about = MenuItem::with_id(AppCommand::About.menu_id(), "About noa", true, None);
        let preferences = MenuItem::with_id(
            AppCommand::Preferences.menu_id(),
            "Preferences...",
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
            "Quit noa",
            true,
            Some(cmd_accelerator(Code::KeyQ)),
        );
        let separator_one = PredefinedMenuItem::separator();
        let separator_two = PredefinedMenuItem::separator();
        let separator_three = PredefinedMenuItem::separator();
        let file_new_tab = MenuItem::with_id(
            AppCommand::NewTab.menu_id(),
            "New Tab",
            true,
            Some(cmd_accelerator(Code::KeyT)),
        );
        let file_close_tab = MenuItem::with_id(
            AppCommand::CloseTab.menu_id(),
            "Close Tab",
            true,
            Some(cmd_accelerator(Code::KeyW)),
        );
        let file_menu = Submenu::with_id_and_items(
            "noa.menu.file",
            "File",
            true,
            &[
                &file_new_tab,
                &PredefinedMenuItem::separator(),
                &file_close_tab,
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
                &disabled_item("noa.edit.select-all", "Select All"),
            ],
        )?;
        let view_menu = Submenu::with_id_and_items(
            "noa.menu.view",
            "View",
            true,
            &[
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
            ],
        )?;
        let help_menu = Submenu::with_id_and_items(
            "noa.menu.help",
            "Help",
            true,
            &[&disabled_item("noa.help.noa-help", "noa Help")],
        )?;

        app_menu.append_items(&[
            &about,
            &separator_one,
            &preferences,
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

        Ok(Self { _menu: menu })
    }
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
