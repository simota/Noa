//! Board Timeline store and service (`docs/specs/board-timeline.md` L2-4,
//! L2-5): one durable, append-only feed of posts per board under
//! `<data-dir>/noa/boards/`, written and searched only on the `noa-timeline`
//! service thread, which `noa.timeline.*` requests reach off the UI thread.

mod ids;
mod replay;
mod search;
mod service;
mod store;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

pub(crate) use service::{TimelineHandle, start};
