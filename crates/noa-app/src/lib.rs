//! `noa-app` — the apprt: winit event loop, window/surface, io thread, input
//! encoding, and the hardcoded inc-1 theme. The only crate in the workspace
//! (besides `noa-render`, which stays surface-less) that touches `winit` or
//! `wgpu`.

mod anim;
mod app;
mod app_actions;
mod branch_poll;
mod chrome;
mod cli;
mod clipboard;
mod command_palette;
mod commands;
mod events;
mod input;
mod io_thread;
mod link_open;
mod localtime;
mod macos_blur;
mod macos_hotkey;
#[cfg(target_os = "macos")]
mod macos_menu;
mod macos_window;
mod mouse;
mod notification;
mod search_prompt;
mod secure_input;
mod session;
mod session_store;
mod sidebar;
pub mod split_tree;
pub mod tab_overview;
mod theme;

pub use app::AppConfig;
pub use cli::{CliAction, Invocation, parse_invocation, run_action, unknown_action_message};
pub use commands::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};
pub use events::UserEvent;
pub use input::encode_paste;

use winit::event_loop::EventLoop;

/// Launch the terminal. Blocks until the window closes.
pub fn run(config: AppConfig) -> anyhow::Result<()> {
    let mut builder = EventLoop::<UserEvent>::with_user_event();

    // Present as a real foreground macOS app even when launched from a plain
    // `cargo run` (not just from the .app bundle). noa installs its own
    // native menu from the winit app lifecycle, so keep winit's default menu
    // disabled to avoid duplicate app menus.
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Regular);
        builder.with_default_menu(false);
    }

    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    let mut app = app::App::new(config, proxy);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use noa_core::{Color, GridSize};
    use noa_grid::Terminal;

    /// Proves the parse -> grid path `noa-app`'s io thread relies on: feed a
    /// byte fixture with SGR color through a `Stream` into a `Terminal`, and
    /// assert specific cells carry the expected char + palette fg. No GPU or
    /// window needed.
    #[test]
    fn sgr_color_feed_produces_expected_cells() {
        let mut terminal = Terminal::new(GridSize::new(80, 24));
        let mut stream = noa_vt::Stream::new();

        let fixture: &[u8] = b"\x1b[31mRED\x1b[0m normal\r\n\x1b[32mgreen\x1b[0m";
        stream.feed(fixture, &mut terminal);

        let row0 = &terminal.active().grid[0];
        // "RED" in red (palette 1), then " normal" in default fg.
        assert_eq!(row0.cells[0].ch, 'R');
        assert_eq!(row0.cells[0].fg, Color::Palette(1));
        assert_eq!(row0.cells[1].ch, 'E');
        assert_eq!(row0.cells[1].fg, Color::Palette(1));
        assert_eq!(row0.cells[2].ch, 'D');
        assert_eq!(row0.cells[2].fg, Color::Palette(1));

        assert_eq!(row0.cells[3].ch, ' ');
        assert_eq!(row0.cells[3].fg, Color::Default);
        assert_eq!(row0.cells[4].ch, 'n');
        assert_eq!(row0.cells[4].fg, Color::Default);

        let row1 = &terminal.active().grid[1];
        assert_eq!(row1.cells[0].ch, 'g');
        assert_eq!(row1.cells[0].fg, Color::Palette(2));
        assert_eq!(row1.cells[4].ch, 'n');
        assert_eq!(row1.cells[4].fg, Color::Palette(2));

        // Cursor ends up right after "green" on row 1.
        assert_eq!(terminal.active().cursor.y, 1);
        assert_eq!(terminal.active().cursor.x, 5);
    }
}
