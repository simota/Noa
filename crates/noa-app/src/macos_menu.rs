//! Native macOS menu construction.

use muda::{
    Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};
use winit::event_loop::EventLoopProxy;

use crate::{AppCommand, UserEvent};

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
        let close_window = MenuItem::with_id(
            AppCommand::CloseWindow.menu_id(),
            "Close Window",
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
        let file_close_window = MenuItem::with_id(
            AppCommand::CloseWindow.menu_id(),
            "Close Window",
            true,
            Some(cmd_accelerator(Code::KeyW)),
        );
        let file_menu =
            Submenu::with_id_and_items("noa.menu.file", "File", true, &[&file_close_window])?;
        let edit_menu = Submenu::with_id_and_items(
            "noa.menu.edit",
            "Edit",
            true,
            &[
                &disabled_item("noa.edit.undo", "Undo"),
                &PredefinedMenuItem::separator(),
                &disabled_item("noa.edit.cut", "Cut"),
                &disabled_item("noa.edit.copy", "Copy"),
                &disabled_item("noa.edit.paste", "Paste"),
                &disabled_item("noa.edit.select-all", "Select All"),
            ],
        )?;
        let view_menu = Submenu::with_id_and_items(
            "noa.menu.view",
            "View",
            true,
            &[&disabled_item(
                "noa.view.toggle-full-screen",
                "Toggle Full Screen",
            )],
        )?;
        let window_menu = Submenu::with_id_and_items(
            "noa.menu.window",
            "Window",
            true,
            &[
                &disabled_item("noa.window.minimize", "Minimize"),
                &disabled_item("noa.window.zoom", "Zoom"),
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
            &close_window,
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
