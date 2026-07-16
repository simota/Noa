//! The single io thread: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize, input, and
//! explicit IPC viewport-refresh requests come in from the main thread over
//! crossbeam channels.

// Not directly used at this level — brought into scope only so
// `io_thread::tests` can resolve these unqualified via `use super::*;`, same
// as every item glob-imported from a submodule below.
#[allow(unused_imports)]
use std::sync::Arc;
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[allow(unused_imports)]
use parking_lot::Mutex;
#[allow(unused_imports)]
use std::time::{Duration, Instant};

#[allow(unused_imports)]
use noa_core::GridSize;
#[allow(unused_imports)]
use noa_grid::Terminal;

#[allow(unused_imports)]
use crate::session_overview::OVERVIEW_TILE_MIN_RENDER_INTERVAL;

mod auto_approve;
mod feed;
mod input_queue;
mod ipc_tap;
mod overview;
mod raw_attach;
mod redraw;
pub(crate) mod sidebar;
mod spawn;

// Not directly used at this level — these glob-imports flatten every
// submodule's `pub(super)` surface back into `io_thread`'s own namespace so
// `io_thread::tests` (a descendant, like every submodule) can resolve the
// same unqualified names the pre-split single file exposed via its own
// top-level items, through a plain `use super::*;`.
#[allow(unused_imports)]
use auto_approve::*;
#[allow(unused_imports)]
use feed::*;
#[allow(unused_imports)]
use input_queue::*;
#[allow(unused_imports)]
use ipc_tap::*;
#[allow(unused_imports)]
use overview::*;
#[allow(unused_imports)]
use raw_attach::*;
#[allow(unused_imports)]
use redraw::*;
#[allow(unused_imports)]
use sidebar::*;
#[allow(unused_imports)]
use spawn::*;

pub(crate) use auto_approve::{AutoApproveFeedback, AutoApprovePublish};
pub(crate) use input_queue::{EchoStampedInput, PtyInputQueue, QueueInputResult, input_channel};
pub(crate) use ipc_tap::IpcOutputTap;
pub(crate) use overview::{OverviewPublish, publish_overview_snapshot};
pub(crate) use raw_attach::RawAttachTap;
pub(crate) use redraw::{
    RedrawFloor, SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION, redraw_floor_from_refresh_millihertz,
};
pub(crate) use sidebar::SidebarPublish;
pub(crate) use spawn::{IoThreadHandle, IoThreadTarget, spawn};

#[cfg(test)]
mod tests;
