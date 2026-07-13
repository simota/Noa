use super::*;
#[cfg(all(test, target_os = "macos"))]
use winit::platform::macos::OptionAsAlt;

pub(super) const MIN_RUNTIME_FONT_SIZE: f32 = 6.0;

pub(super) const MAX_RUNTIME_FONT_SIZE: f32 = 96.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RuntimeFontSizeUpdate {
    pub(super) point_size: f32,
    pub(super) changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneResizeAction<Id> {
    GridResize(Id, GridSize),
    PtyResize(Id, GridSize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseConfirmTarget {
    Pane,
    Session,
    Window,
    App,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseWheelViewportScroll {
    Up(usize),
    Down(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabCloseOutcome<Id> {
    Stale,
    Quit,
    Continue { focused: Option<Id> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabCloseFocusDecision<Id> {
    NoTarget,
    Immediate(Id),
    Deferred(Id),
}

/// Which tab group a spawned tab should join, given the spawn target and the
/// focused window's group (if any). The `Fresh` arm defers minting an id to
/// the caller ([`App::allocate_group_id`]) so this stays a pure decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupChoice<G> {
    Existing(G),
    Fresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetedRedrawDecision {
    Stale,
    Suppress,
    Request,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandScope {
    App,
    FocusedTab,
    NativeTabGroup,
    Overview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandOrigin {
    App,
    TerminalWindow,
    OverviewWindow,
}

mod dispatch;
mod geometry;
mod lifecycle;
mod resize;
mod scroll;
mod surface;

pub(super) use dispatch::*;
pub(super) use geometry::*;
pub(super) use lifecycle::*;
pub(super) use resize::*;
pub(super) use scroll::*;
pub(super) use surface::*;

#[cfg(test)]
mod tests;
