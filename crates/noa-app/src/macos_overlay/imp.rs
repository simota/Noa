#[cfg(target_os = "macos")]
mod appkit;
#[cfg(target_os = "macos")]
pub(super) use appkit::*;

#[cfg(not(target_os = "macos"))]
mod noop;
#[cfg(not(target_os = "macos"))]
pub(super) use noop::*;
