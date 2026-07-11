//! Native AppKit modal-overlay cards for macOS: the command palette, the
//! theme-settings overlay, the confirm dialog, and the resize toast.
//!
//! Display layer only. Input, focus, and IME all stay on the existing winit
//! path (`input_ops.rs` + `ModalImeTarget`): the native views never become
//! first responder, so every keyboard/IME test and behavior is untouched.
//! Each overlay is rebuilt from a plain-data view model when (and only when)
//! that model changes — the same identifier-lookup idempotency pattern as
//! `macos_window::install_titlebar_backdrop`, so no AppKit pointers are
//! stored on the Rust side.
//!
//! Like `notification.rs`/`macos_window.rs`, AppKit classes are looked up at
//! runtime with raw `msg_send!` (no extra objc2-app-kit features). All AppKit
//! calls happen on the main thread (the winit redraw path). Off macOS every
//! `sync_*` is a no-op and the wgpu card path (`app/sidebar/palette.rs`)
//! keeps drawing instead.

mod imp;
mod model;
mod sync;

#[cfg(test)]
mod tests;

#[cfg(target_os = "macos")]
pub(crate) use model::cg;
pub(crate) use model::{NativeOverlayCache, OverlayColors, PaneRectPt, TITLE_PROMPT_HINT};
pub(crate) use sync::{
    sync_command_palette, sync_confirm_dialog, sync_process_monitor, sync_theme_settings,
    sync_title_prompt, sync_toast,
};
