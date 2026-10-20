//! Ember: noa's companion in the macOS menu bar. Click the ghost to see every
//! pane ordered by what needs you first; pick one to jump there.
//!
//! A client of the local noa-server (`docs/api/noa-server-protocol.md`): it
//! reads `listPanels` and calls `focusPane`, so noa needs `server-enable =
//! true` and `control` in `server-scopes` for jumping.

mod icon;
mod link;
#[cfg(target_os = "macos")]
mod menu_bar;
mod model;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    #[cfg(target_os = "macos")]
    {
        let (shared, commands) = link::spawn();
        menu_bar::run(shared, commands);
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("Ember is a macOS menu bar app.");
        std::process::exit(1);
    }
}
