use super::*;
use noa_config::{QuickTerminalPosition, QuickTerminalSize, QuickTerminalSizeDim};

/// The quick terminal repositions at roughly this cadence while sliding
/// (approx. 60 fps), driven off the `about_to_wait` `WaitUntil` timer. The
/// presented frame is static (painted once, pre-slide) and moves with the
/// window — this cadence only repositions, it never re-renders.
const QUICK_TERMINAL_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// One resolved quick-terminal panel placement, re-derived fresh on every
/// show (`App::quick_terminal_geometry`) since the target monitor or config
/// may have changed since last time. All coordinates are absolute physical
/// px (the target monitor's origin folded in).
#[derive(Debug, Clone, Copy, PartialEq)]
struct QuickTerminalGeometry {
    /// Top-left origin when fully revealed.
    final_x: i32,
    final_y: i32,
    /// Panel width and height in physical pixels.
    width: u32,
    height: u32,
    /// Top-left origin when fully hidden. Equal to `(final_x, final_y)` for
    /// `Center`, which never slides — see `quick_terminal_position_geometry`.
    hidden_x: i32,
    hidden_y: i32,
}

/// Runtime state for the drop-down quick terminal. The window itself is a
/// normal [`WindowState`] entry in `App::windows`; this tracks the slide
/// geometry and animation.
pub(super) struct QuickTerminalState {
    pub(super) window_id: WindowId,
    /// The panel's currently resolved placement.
    geometry: QuickTerminalGeometry,
    /// Whether the panel is revealed (or animating toward revealed). When
    /// `false` and `anim` is `None`, the window is hidden.
    visible: bool,
    /// Whether this reveal cycle has received a real OS focus event. macOS can
    /// emit stale `Focused(false)` events while a borderless all-Spaces window
    /// is being ordered front; those must not immediately auto-hide the panel.
    focused_this_reveal: bool,
    /// The in-flight slide, if any.
    anim: Option<QuickTerminalAnim>,
    /// The pid of whatever app was frontmost right before this reveal cycle
    /// summoned the quick terminal (and thereby activated Noa), captured only
    /// on a hidden→shown transition (Ghostty parity). Restored on hide, then
    /// cleared — a re-toggle while already shown/animating must not overwrite
    /// it with Noa's own frontmost state.
    previous_app_pid: Option<i32>,
}

/// One in-flight quick-terminal slide.
#[derive(Clone, Copy)]
struct QuickTerminalAnim {
    start: Instant,
    /// Current reveal fraction at `start` (0 = hidden, 1 = fully shown).
    from_reveal: f32,
    /// Target reveal fraction for this slide.
    to_reveal: f32,
    /// This slide's duration, taken from `quick-terminal-animation-duration`
    /// at creation time. `0` collapses `reveal_at`/`done` to an instant jump
    /// to `to_reveal` (`linear_progress` treats a zero duration as complete),
    /// i.e. "no animation".
    duration: Duration,
}

impl QuickTerminalAnim {
    fn new(start: Instant, from_reveal: f32, to_reveal: f32, duration: Duration) -> Self {
        Self {
            start,
            from_reveal: from_reveal.clamp(0.0, 1.0),
            to_reveal: to_reveal.clamp(0.0, 1.0),
            duration,
        }
    }

    fn reveal_at(self, now: Instant) -> f32 {
        quick_terminal_slide_reveal(
            self.from_reveal,
            self.to_reveal,
            now.duration_since(self.start),
            self.duration,
        )
    }

    fn done(self, now: Instant) -> bool {
        quick_terminal_progress(now.duration_since(self.start), self.duration) >= 1.0
    }

    fn hides(self) -> bool {
        self.to_reveal <= 0.0
    }
}

impl QuickTerminalState {
    fn current_reveal(&self, now: Instant) -> f32 {
        if let Some(anim) = self.anim {
            return anim.reveal_at(now);
        }
        if self.visible { 1.0 } else { 0.0 }
    }

    fn should_autohide_on_focus_loss(&self) -> bool {
        quick_terminal_should_autohide_on_focus_loss(self.visible, self.focused_this_reveal)
    }
}

/// The shared house easing curve (see [`crate::anim`]), re-exported for the
/// slide math and its tests.
pub(super) use crate::anim::ease_out_cubic;
/// Linear slide progress (`0.0..=1.0`) for `elapsed` of `duration`.
pub(super) use crate::anim::linear_progress as quick_terminal_progress;

/// Eased reveal fraction between the current and target reveal states. This
/// keeps interrupted show/hide transitions moving from their current position
/// instead of snapping back to an endpoint.
pub(super) fn quick_terminal_slide_reveal(
    from_reveal: f32,
    to_reveal: f32,
    elapsed: Duration,
    duration: Duration,
) -> f32 {
    crate::anim::lerp(
        from_reveal.clamp(0.0, 1.0),
        to_reveal.clamp(0.0, 1.0),
        ease_out_cubic(quick_terminal_progress(elapsed, duration)),
    )
}

/// The panel's absolute origin (physical px) for an already-eased reveal
/// fraction, lerping independently on each axis from `hidden` to `final`.
/// Generalizes the old top-only `quick_terminal_reveal_top_offset`: axes
/// where `hidden == final` (see `quick_terminal_position_geometry`) simply
/// don't move, and `center` (`hidden == final` on both axes) doesn't move at
/// all.
pub(super) fn quick_terminal_reveal_origin(
    final_origin: (i32, i32),
    hidden_origin: (i32, i32),
    reveal: f32,
) -> (i32, i32) {
    let reveal = reveal.clamp(0.0, 1.0);
    let x = crate::anim::lerp(hidden_origin.0 as f32, final_origin.0 as f32, reveal).round() as i32;
    let y = crate::anim::lerp(hidden_origin.1 as f32, final_origin.1 as f32, reveal).round() as i32;
    (x, y)
}

/// Fallback panel size (AppKit points, scaled to physical px like any other
/// `Pixels` side) for the short/cross axis: `top`/`bottom`'s height,
/// `left`/`right`'s width, and `center`'s short axis, whenever that side
/// isn't configured.
const QUICK_TERMINAL_DEFAULT_CROSS_AXIS_PX: f32 = 400.0;
/// Fallback size for `center`'s long axis when `primary` isn't configured.
const QUICK_TERMINAL_DEFAULT_CENTER_LONG_AXIS_PX: f32 = 800.0;

/// What an absent `QuickTerminalSizeDim` side falls back to.
#[derive(Clone, Copy)]
enum QuickTerminalSizeDefault {
    /// Fill the parent (monitor) dimension.
    FullParent,
    /// A fixed size in AppKit points, scaled to physical px like `Pixels`.
    FixedPoints(f32),
}

/// One `QuickTerminalSize` side resolved to physical px against `parent`
/// (the matching monitor dimension), clamped to `1..=parent`.
fn resolve_quick_terminal_dim(
    dim: Option<QuickTerminalSizeDim>,
    parent: u32,
    scale_factor: f64,
    default: QuickTerminalSizeDefault,
) -> u32 {
    let raw = match dim {
        Some(QuickTerminalSizeDim::Percent(pct)) => parent as f64 * (pct as f64 / 100.0),
        Some(QuickTerminalSizeDim::Pixels(px)) => px as f64 * scale_factor,
        None => match default {
            QuickTerminalSizeDefault::FullParent => parent as f64,
            QuickTerminalSizeDefault::FixedPoints(points) => points as f64 * scale_factor,
        },
    };
    raw.round().clamp(1.0, parent.max(1) as f64) as u32
}

/// The panel's width/height in physical px for `position`/`size` on a
/// monitor `monitor_width`x`monitor_height` physical px at `scale_factor` — a
/// port of Ghostty's `QuickTerminalSize.calculate`. Replaces/generalizes the
/// old top-only `quick_terminal_height`. Each dimension clamps to
/// `1..=<matching monitor dimension>`.
pub(super) fn quick_terminal_size_footprint(
    position: QuickTerminalPosition,
    size: QuickTerminalSize,
    monitor_width: u32,
    monitor_height: u32,
    scale_factor: f64,
) -> (u32, u32) {
    use QuickTerminalSizeDefault::{FixedPoints, FullParent};
    match position {
        QuickTerminalPosition::Top | QuickTerminalPosition::Bottom => {
            let width =
                resolve_quick_terminal_dim(size.secondary, monitor_width, scale_factor, FullParent);
            let height = resolve_quick_terminal_dim(
                size.primary,
                monitor_height,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CROSS_AXIS_PX),
            );
            (width, height)
        }
        QuickTerminalPosition::Left | QuickTerminalPosition::Right => {
            let width = resolve_quick_terminal_dim(
                size.primary,
                monitor_width,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CROSS_AXIS_PX),
            );
            let height = resolve_quick_terminal_dim(
                size.secondary,
                monitor_height,
                scale_factor,
                FullParent,
            );
            (width, height)
        }
        QuickTerminalPosition::Center if monitor_width >= monitor_height => {
            let width = resolve_quick_terminal_dim(
                size.primary,
                monitor_width,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CENTER_LONG_AXIS_PX),
            );
            let height = resolve_quick_terminal_dim(
                size.secondary,
                monitor_height,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CROSS_AXIS_PX),
            );
            (width, height)
        }
        QuickTerminalPosition::Center => {
            // Portrait monitor: the long axis is now vertical.
            let width = resolve_quick_terminal_dim(
                size.secondary,
                monitor_width,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CROSS_AXIS_PX),
            );
            let height = resolve_quick_terminal_dim(
                size.primary,
                monitor_height,
                scale_factor,
                FixedPoints(QUICK_TERMINAL_DEFAULT_CENTER_LONG_AXIS_PX),
            );
            (width, height)
        }
    }
}

/// The panel's final (fully revealed) origin and fully-hidden origin — both
/// absolute physical-px screen coordinates — for `position` on the monitor at
/// `(monitor_x, monitor_y)` sized `monitor_width`x`monitor_height`, given the
/// panel's own `width`/`height` (from `quick_terminal_size_footprint`). Uses
/// the monitor's full bounds, not `visibleFrame`, for every position — a
/// known simplification shared with the pre-existing `top` behavior (which
/// may overlap the menu bar); `bottom` may similarly overlap the Dock.
pub(super) fn quick_terminal_position_geometry(
    position: QuickTerminalPosition,
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: u32,
    monitor_height: u32,
    width: u32,
    height: u32,
) -> ((i32, i32), (i32, i32)) {
    let centered_x = monitor_x + (monitor_width as i32 - width as i32) / 2;
    let centered_y = monitor_y + (monitor_height as i32 - height as i32) / 2;
    match position {
        QuickTerminalPosition::Top => (
            (centered_x, monitor_y),
            (centered_x, monitor_y - height as i32),
        ),
        QuickTerminalPosition::Bottom => {
            let final_y = monitor_y + monitor_height as i32 - height as i32;
            (
                (centered_x, final_y),
                (centered_x, monitor_y + monitor_height as i32),
            )
        }
        QuickTerminalPosition::Left => (
            (monitor_x, centered_y),
            (monitor_x - width as i32, centered_y),
        ),
        QuickTerminalPosition::Right => {
            let final_x = monitor_x + monitor_width as i32 - width as i32;
            (
                (final_x, centered_y),
                (monitor_x + monitor_width as i32, centered_y),
            )
        }
        QuickTerminalPosition::Center => {
            // No travel (see the type's doc comment): Ghostty fades `center`
            // in/out via window alpha, and noa has no window-alpha animation
            // machinery, so hidden == final here.
            let origin = (centered_x, centered_y);
            (origin, origin)
        }
    }
}

pub(super) fn quick_terminal_should_autohide_on_focus_loss(
    visible: bool,
    focused_this_reveal: bool,
) -> bool {
    visible && focused_this_reveal
}

/// Whether redraws should be suppressed for a fully hidden, non-animating
/// quick terminal. Once the hide slide finishes and the window is ordered
/// out, pty output must not keep re-presenting frames to it — there is no
/// occlusion event to gate on (the quick terminal is exempt, see
/// `event_loop.rs`'s `Occluded` handler) so it gates itself instead.
pub(super) fn quick_terminal_should_suppress_redraw(visible: bool, animating: bool) -> bool {
    !visible && !animating
}

pub(super) fn quick_terminal_anchor_window_id<W: Copy>(
    os_focused: Option<W>,
    focused: Option<W>,
    window_order: &[W],
    mut is_regular_window: impl FnMut(W) -> bool,
) -> Option<W> {
    os_focused
        .into_iter()
        .chain(focused)
        .chain(window_order.iter().rev().copied())
        .find(|id| is_regular_window(*id))
}

/// Quick terminal (drop-down) support.
impl App {
    pub(super) fn is_quick_terminal_window(&self, window_id: WindowId) -> bool {
        self.quick_terminal
            .as_ref()
            .is_some_and(|qt| qt.window_id == window_id)
    }

    /// Whether `window_id` is the quick terminal's window and it is fully
    /// hidden (not visible, no slide in flight) — used by `render.rs` to skip
    /// redraws to an ordered-out window instead of relying on occlusion
    /// events (the quick terminal is exempt from those, see `event_loop.rs`).
    pub(super) fn quick_terminal_redraw_suppressed(&self, window_id: WindowId) -> bool {
        self.quick_terminal.as_ref().is_some_and(|qt| {
            qt.window_id == window_id
                && quick_terminal_should_suppress_redraw(qt.visible, qt.anim.is_some())
        })
    }

    /// Register the global `quick-terminal-hotkey` and `sidebar-hotkey` once,
    /// after the app is running. A no-op per chord when unset or explicitly
    /// disabled; a registration failure is logged, not fatal. Both go through
    /// the same `parse_hotkey` path (FR-13).
    pub(super) fn install_global_hotkey_if_needed(&mut self) {
        if self.hotkey_install_attempted {
            return;
        }
        self.hotkey_install_attempted = true;

        // Empty spec is the "explicitly disabled" sentinel (config `none`).
        if let Some(spec) = self.config.quick_terminal_hotkey.clone()
            && !spec.trim().is_empty()
        {
            match crate::macos_hotkey::GlobalHotKey::register(
                &spec,
                self.proxy.clone(),
                crate::macos_hotkey::HotkeyAction::QuickTerminal,
            ) {
                Some(hotkey) => self.quick_terminal_hotkey = Some(hotkey),
                None => log::warn!("failed to register quick-terminal-hotkey `{spec}`"),
            }
        }

        if let Some(spec) = self.config.sidebar_hotkey.clone()
            && !spec.trim().is_empty()
        {
            match crate::macos_hotkey::GlobalHotKey::register(
                &spec,
                self.proxy.clone(),
                crate::macos_hotkey::HotkeyAction::Sidebar,
            ) {
                Some(hotkey) => self.sidebar_hotkey = Some(hotkey),
                None => log::warn!("failed to register sidebar-hotkey `{spec}`"),
            }
        }
    }

    /// Toggle the quick terminal: reveal it (creating its window on first use)
    /// when hidden, slide it away when shown. A no-op before the GPU exists
    /// (i.e. before the first real window), which also means it can't be the
    /// app's only window.
    pub(super) fn toggle_quick_terminal(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        match self.quick_terminal.as_ref() {
            Some(qt) if qt.visible => self.start_quick_terminal_hide(),
            _ => self.start_quick_terminal_show(event_loop),
        }
    }

    /// The panel's fully resolved placement (final rect + hidden-state
    /// origin), all in physical pixels. The target monitor is resolved fresh
    /// on every call: `quick-terminal-screen` first (macOS only), falling
    /// back to the anchor window's monitor, then the primary monitor.
    fn quick_terminal_geometry(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Option<QuickTerminalGeometry> {
        let monitor = self
            .quick_terminal_screen_monitor(event_loop)
            .or_else(|| {
                self.quick_terminal_anchor_window()
                    .and_then(|window| window.current_monitor())
            })
            .or_else(|| event_loop.primary_monitor())?;
        let position = monitor.position();
        let size = monitor.size();
        let scale_factor = monitor.scale_factor();
        let (width, height) = quick_terminal_size_footprint(
            self.config.quick_terminal_position,
            self.config.quick_terminal_size,
            size.width,
            size.height,
            scale_factor,
        );
        let (final_origin, hidden_origin) = quick_terminal_position_geometry(
            self.config.quick_terminal_position,
            position.x,
            position.y,
            size.width,
            size.height,
            width,
            height,
        );
        Some(QuickTerminalGeometry {
            final_x: final_origin.0,
            final_y: final_origin.1,
            width,
            height,
            hidden_x: hidden_origin.0,
            hidden_y: hidden_origin.1,
        })
    }

    /// The monitor resolved by `quick-terminal-screen` (`main` / `mouse` /
    /// `macos-menu-bar`), matched back to a winit [`MonitorHandle`] by native
    /// `CGDirectDisplayID`. `None` off macOS, when AppKit can't resolve a
    /// screen for the configured mode (e.g. `mouse` with no screen under the
    /// pointer), or when the resolved display doesn't match any monitor
    /// winit currently reports.
    #[cfg(target_os = "macos")]
    fn quick_terminal_screen_monitor(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Option<winit::monitor::MonitorHandle> {
        let display_id =
            crate::macos_window::quick_terminal_target_display(self.config.quick_terminal_screen)?;
        event_loop
            .available_monitors()
            .find(|monitor| monitor.native_id() == display_id)
    }

    #[cfg(not(target_os = "macos"))]
    fn quick_terminal_screen_monitor(
        &self,
        _event_loop: &ActiveEventLoop,
    ) -> Option<winit::monitor::MonitorHandle> {
        None
    }

    fn quick_terminal_anchor_window(&self) -> Option<Arc<Window>> {
        quick_terminal_anchor_window_id(
            self.os_focused,
            self.focused,
            &self.window_order,
            |window_id| {
                self.windows.contains_key(&window_id) && !self.is_quick_terminal_window(window_id)
            },
        )
        .and_then(|window_id| self.windows.get(&window_id))
        .map(|state| state.window.clone())
    }

    fn start_quick_terminal_show(&mut self, event_loop: &ActiveEventLoop) {
        // Capture whatever app was frontmost before anything below activates
        // Noa (Ghostty parity: restored on hide, see `start_quick_terminal_hide`).
        // Only on a hidden→shown edge — a re-toggle while already
        // shown/animating-in must not clobber the stored pid with Noa's own
        // frontmost state once `show_quick_terminal_window` below has run.
        let was_hidden = self.quick_terminal.as_ref().is_none_or(|qt| !qt.visible);
        let captured_app_pid = was_hidden
            .then(crate::macos_window::frontmost_app_pid)
            .flatten();

        let Some(geometry) = self.quick_terminal_geometry(event_loop) else {
            return;
        };
        if self.quick_terminal.is_none() {
            let Some(window_id) = self.create_quick_terminal(event_loop, geometry) else {
                return;
            };
            self.quick_terminal = Some(QuickTerminalState {
                window_id,
                geometry,
                visible: false,
                focused_this_reveal: false,
                anim: None,
                previous_app_pid: captured_app_pid,
            });
        } else if let Some(qt) = self.quick_terminal.as_mut() {
            // Re-derive geometry each open: the active monitor (or its
            // resolution) may have changed since last time.
            let window_id = qt.window_id;
            let geometry_changed =
                qt.geometry.width != geometry.width || qt.geometry.height != geometry.height;
            qt.geometry = geometry;
            if was_hidden {
                qt.previous_app_pid = captured_app_pid;
            }
            if geometry_changed {
                let new_size = self.windows.get(&window_id).and_then(|state| {
                    state
                        .window
                        .request_inner_size(PhysicalSize::new(geometry.width, geometry.height))
                });
                // macOS applies `request_inner_size` synchronously and
                // returns the new size directly (RC4) — fold it into the
                // surface/renderer/layout now so they're consistent before
                // the pre-paint below, instead of waiting on a `Resized`
                // event that would otherwise land mid-slide.
                if let Some(new_size) = new_size {
                    self.on_resize(window_id, new_size);
                }
            }
        }

        let slide_duration = Duration::from_secs_f32(self.config.quick_terminal_animation_duration);
        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        let now = Instant::now();
        let from_reveal = qt.current_reveal(now);
        qt.visible = true;
        qt.focused_this_reveal = false;
        let anim = QuickTerminalAnim::new(now, from_reveal, 1.0, slide_duration);
        qt.anim = Some(anim);
        let window_id = qt.window_id;
        let geometry = qt.geometry;
        // `anim.reveal_at(now)` rather than the raw `from_reveal`: with a
        // zero `quick-terminal-animation-duration` this is already the fully
        // revealed position (`linear_progress` treats zero duration as
        // complete), so the drop-down appears fully shown with no animation
        // instead of one frame at the pre-slide position.
        let (current_x, current_y) = quick_terminal_reveal_origin(
            (geometry.final_x, geometry.final_y),
            (geometry.hidden_x, geometry.hidden_y),
            anim.reveal_at(now),
        );
        if let Some(state) = self.windows.get(&window_id) {
            state
                .window
                .set_outer_position(PhysicalPosition::new(current_x, current_y));
        }
        // Pre-paint a valid frame while the window is still hidden/off-screen
        // (RC1/RC2), before it is ever ordered front or starts sliding.
        self.redraw(window_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.set_visible(true);
            crate::macos_window::show_quick_terminal_window(&state.window);
        }
        self.focused = Some(window_id);
    }

    pub(super) fn start_quick_terminal_hide(&mut self) {
        let slide_duration = Duration::from_secs_f32(self.config.quick_terminal_animation_duration);
        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        let already_hiding = !qt.visible && qt.anim.as_ref().is_some_and(|anim| anim.hides());
        if already_hiding {
            return;
        }
        let now = Instant::now();
        let from_reveal = qt.current_reveal(now);
        qt.visible = false;
        qt.focused_this_reveal = false;
        qt.anim = Some(QuickTerminalAnim::new(
            now,
            from_reveal,
            0.0,
            slide_duration,
        ));
        // No redraw here: content doesn't change at hide start, and
        // `about_to_wait` runs right after this event and picks up
        // `tick_quick_terminal`'s deadline to drive the slide.

        // Restore the previously frontmost app now, at hide *start* (Ghostty
        // parity: "when the animation completes macOS will bring forward
        // another window" — done up front so the slide-out and the other
        // app's raise happen together). Only when Noa is still the active
        // app: if the user already switched away on their own (e.g. the
        // autohide-on-focus-loss path), that app is already frontmost and
        // stealing focus back would fight them.
        let previous_app_pid = qt.previous_app_pid.take();
        if let Some(pid) = previous_app_pid
            && crate::macos_window::app_is_active()
        {
            crate::macos_window::activate_app_with_pid(pid);
        }
    }

    /// Hide the quick terminal when it loses focus, if `quick-terminal-autohide`
    /// is enabled. Called from the window's `Focused(false)` event.
    pub(super) fn maybe_autohide_quick_terminal(&mut self) {
        if !self.config.quick_terminal_autohide {
            return;
        }
        if self
            .quick_terminal
            .as_ref()
            .is_some_and(QuickTerminalState::should_autohide_on_focus_loss)
        {
            self.start_quick_terminal_hide();
        }
    }

    pub(super) fn mark_quick_terminal_focused(&mut self, window_id: WindowId) {
        if let Some(qt) = self.quick_terminal.as_mut()
            && qt.window_id == window_id
            && qt.visible
        {
            qt.focused_this_reveal = true;
        }
    }

    /// Advance the slide, repositioning the window each frame — the presented
    /// frame is static (painted once, pre-slide) and moves with the window,
    /// so this never redraws. Reports the next wake instant while animating
    /// (folded into `about_to_wait`'s deadline), and `None` once the slide
    /// settles — hiding the window on a completed slide-out.
    pub(super) fn tick_quick_terminal(&mut self) -> Option<Instant> {
        let (window_id, geometry, anim) = {
            let qt = self.quick_terminal.as_ref()?;
            let anim = *qt.anim.as_ref()?;
            (qt.window_id, qt.geometry, anim)
        };
        let now = Instant::now();
        let reveal = anim.reveal_at(now);
        let (x, y) = quick_terminal_reveal_origin(
            (geometry.final_x, geometry.final_y),
            (geometry.hidden_x, geometry.hidden_y),
            reveal,
        );
        if let Some(state) = self.windows.get(&window_id) {
            state.window.set_outer_position(PhysicalPosition::new(x, y));
        }
        if anim.done(now) {
            if let Some(qt) = self.quick_terminal.as_mut() {
                qt.anim = None;
            }
            if anim.hides()
                && let Some(state) = self.windows.get(&window_id)
            {
                state.window.set_visible(false);
            }
            return None;
        }
        Some(now + QUICK_TERMINAL_FRAME_INTERVAL)
    }

    /// Tear down the quick terminal outright (its shell exited). Unlike hide,
    /// this drops the window and io thread so a fresh one is spawned next open.
    ///
    /// No session-store reconcile is needed here: a quick-terminal pane is never
    /// sidebar-eligible, so `apply_session_delta` drops its `Upsert`/`Bell`
    /// before they reach the store (FR-14/AC-16b) — there is never a QT card to
    /// leave behind.
    pub(super) fn destroy_quick_terminal(&mut self) {
        // `qt` (including any stored `previous_app_pid`) is dropped whole at
        // the end of this function; unlike `start_quick_terminal_hide` this
        // path never restores focus — the shell exiting isn't a user-driven
        // hide, so there is no "previous app" hand-off to honor.
        let Some(qt) = self.quick_terminal.take() else {
            return;
        };
        if let Some(mut state) = self.windows.remove(&qt.window_id) {
            state.shutdown();
        }
        if self.focused == Some(qt.window_id) {
            self.focused = self.window_order.last().copied();
            if let Some(window_id) = self.focused
                && let Some(state) = self.windows.get(&window_id)
            {
                state.window.focus_window();
            }
        }
    }

    /// Build the quick-terminal window + its single pane, inserting it into
    /// `windows` (but deliberately not `window_order`). Assumes the GPU is
    /// already initialized (guaranteed by `toggle_quick_terminal`).
    fn create_quick_terminal(
        &mut self,
        event_loop: &ActiveEventLoop,
        geometry: QuickTerminalGeometry,
    ) -> Option<WindowId> {
        let attrs = WindowAttributes::default()
            .with_title("Quick Terminal")
            .with_decorations(false)
            .with_inner_size(PhysicalSize::new(geometry.width, geometry.height))
            .with_position(PhysicalPosition::new(geometry.hidden_x, geometry.hidden_y))
            .with_transparent(self.config.background_opacity < 1.0)
            // Never on screen until the show path explicitly reveals it
            // (RC1): avoids ordering an unpainted window front.
            .with_visible(false);
        #[cfg(target_os = "macos")]
        let attrs = attrs.with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt));
        let window = Arc::new(event_loop.create_window(attrs).ok()?);
        window.set_ime_allowed(true);
        crate::macos_blur::apply_background_blur(
            &window,
            self.config.background_blur_radius,
            self.config.background_opacity,
        );
        crate::macos_window::configure_quick_terminal_window(&window);

        let surface = {
            let gpu = self.gpu.as_ref()?;
            gpu.instance.create_surface(window.clone()).ok()?
        };
        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut()?;
            let caps = surface.get_capabilities(&gpu.adapter);
            let alpha_blending = alpha_blending_mode(&self.config.font);
            let surface_format = preferred_surface_format(&caps.formats, alpha_blending);
            let size = window.inner_size();
            let surface_config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: preferred_surface_alpha_mode(
                    &caps,
                    self.config.background_opacity < 1.0,
                ),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);
            let pipelines = gpu.pipelines.get(&gpu.device, surface_format);
            let font_atlases =
                gpu.font_atlases
                    .get(&gpu.device, &gpu.queue, surface_format, &gpu.font);
            let mut renderer = Renderer::with_pipelines(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                &font_atlases,
                &mut gpu.font,
                self.padding,
            )
            .ok()?;
            renderer.set_background_opacity(self.config.background_opacity);
            renderer.set_alpha_blending(alpha_blending);
            renderer.set_background_image(
                &gpu.device,
                &gpu.queue,
                self.background_image.current_image(),
            );
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = PaneRectApp::new(0, 0, surface_config.width, surface_config.height);
        let metrics = self.gpu.as_ref()?.font.metrics();
        let grid = grid_size_for_pane_rect(initial_rect, metrics, self.padding);
        let auto_approve_enabled = Arc::new(AtomicBool::new(false));
        let redraw_floor = crate::io_thread::RedrawFloor::new(
            crate::io_thread::redraw_floor_from_refresh_millihertz(
                window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz()),
            ),
        );
        let initial_surface = self
            .spawn_pane_surface(
                window_id,
                initial_pane,
                grid,
                initial_rect,
                None,
                auto_approve_enabled.clone(),
                redraw_floor.clone(),
            )
            .ok()?;
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);
        let group = self.allocate_group_id();
        self.windows.insert(
            window_id,
            WindowState {
                window,
                group,
                surface,
                surface_config,
                renderer,
                split_tree: SplitTree::leaf(initial_pane),
                zoomed: None,
                focused_pane: initial_pane,
                next_pane_id: 2,
                surfaces,
                last_mouse_pane: Some(initial_pane),
                last_mouse_point: None,
                last_mouse_physical_position: None,
                active_split_drag: None,
                occluded: false,
                title: "Noa".to_string(),
                title_override: None,
                proxy_icon_cwd: None,
                last_touchpad_stage: 0,
                auto_approve_enabled,
                redraw_floor,
                sidebar_scroll: 0,
                sidebar_button_hover: false,
                sidebar_card_hover: None,
                sidebar_menu: None,
                sidebar_drag: None,
                link_click_in_flight: false,
                file_drop: FileDropState::default(),
                last_grid: None,
                resize_overlay: None,
                bell_flash_until: None,
                native_overlays: Default::default(),
            },
        );
        self.relayout_and_resize_window(window_id);
        Some(window_id)
    }
}
