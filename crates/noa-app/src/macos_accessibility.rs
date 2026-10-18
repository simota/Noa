//! Terminal panes in the macOS accessibility tree (agent-automation
//! REQ-014/015). Each split pane is an `AXTextArea` element inside an
//! `AXGroup` container view laid over the tab's content (spec L2.2's
//! dedicated-view placement): assistants that read the AX tree find every
//! terminal, its on-screen frame and its text, while the content view's own
//! subviews (native overlay cards, text panels) keep their AX presence. The
//! container never takes mouse events.
//!
//! Values are computed on demand inside the AX getters (which AppKit calls on
//! the main thread); nothing is pushed or polled while no client asks
//! (agent-automation CFR-002). The main thread only keeps each element's
//! geometry/focus current via [`WindowAx::sync`], which is a no-op unless the
//! window's pane layout changed.
//!
//! Ghostty analog: `SurfaceView_AppKit`'s accessibility conformance.

use crate::macos_applescript::TerminalHandle;
use crate::split_tree::Rect;

/// One pane as the accessibility tree should show it.
#[derive(Clone)]
pub(crate) struct AxPane {
    /// The pane's `PaneId` (also its AppleScript terminal id).
    pub(crate) id: u64,
    /// Physical-pixel rect relative to the content view's top-left.
    pub(crate) rect: Rect,
    /// Whether this is the tab's focused pane.
    pub(crate) focused: bool,
    pub(crate) terminal: Option<TerminalHandle>,
}

/// The accessibility elements of one tab (native window).
#[derive(Default)]
pub(crate) struct WindowAx {
    /// The `(id, rect, focused)` layout the elements were last synced to.
    layout: Vec<(u64, Rect, bool)>,
    scale_factor: f64,
    #[cfg(target_os = "macos")]
    elements: Vec<(u64, objc2::rc::Retained<imp::TerminalAxElement>)>,
    #[cfg(target_os = "macos")]
    container: Option<objc2::rc::Retained<imp::TerminalAxContainer>>,
}

impl WindowAx {
    /// Bring the window's elements in line with `panes` (REQ-015). Cheap when
    /// nothing changed: compares the lock-free layout first.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    pub(crate) fn sync(
        &mut self,
        window: &winit::window::Window,
        scale_factor: f64,
        panes: &[AxPane],
    ) {
        let layout: Vec<(u64, Rect, bool)> = panes
            .iter()
            .map(|pane| (pane.id, pane.rect, pane.focused))
            .collect();
        if layout == self.layout && scale_factor == self.scale_factor {
            return;
        }
        #[cfg(target_os = "macos")]
        let synced = imp::sync(self, window, scale_factor, panes);
        #[cfg(not(target_os = "macos"))]
        let synced = true;
        // Remember the layout only once it is applied, so a sync that could
        // not reach the view is retried on the next pass.
        if synced {
            self.layout = layout;
            self.scale_factor = scale_factor;
        }
    }
}

/// A pane rect (physical pixels) in points, as `(x, y, w, h)`. A
/// non-positive or non-finite scale factor (a window not on a screen yet)
/// is treated as 1 rather than dividing by it.
pub(crate) fn pane_frame_points(rect: Rect, scale_factor: f64) -> (f64, f64, f64, f64) {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    (
        f64::from(rect.x) / scale,
        f64::from(rect.y) / scale,
        f64::from(rect.w) / scale,
        f64::from(rect.h) / scale,
    )
}

/// The viewport row (0-based) the cursor sits on, or `None` when the viewport
/// is scrolled away from it (`AXInsertionPointLineNumber`, spec L2.2).
pub(crate) fn cursor_viewport_row(terminal: &noa_grid::Terminal) -> Option<usize> {
    let screen = terminal.active();
    let row = usize::from(screen.cursor.y) + terminal.viewport_offset();
    (row < usize::from(screen.rows)).then_some(row)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::sync::Weak as ArcWeak;

    use objc2::rc::{Retained, Weak};
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSAccessibilityElement, NSResponder, NSView};
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSObject, NSPoint, NSRect, NSSize, NSString,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{AxPane, WindowAx, cursor_viewport_row};
    use crate::macos_applescript::{terminal_contents, terminal_selection};

    pub(crate) struct AxState {
        id: u64,
        /// Frame in the container's (flipped, point) coordinates.
        frame: NSRect,
        focused: bool,
        /// The `AXGroup` container: the element's AX parent and the view its
        /// frame is relative to.
        container: Weak<AnyObject>,
        /// Weak, so an AX client holding a closed pane's element does not
        /// keep that pane's terminal and scrollback alive.
        terminal: Option<ArcWeak<parking_lot::Mutex<noa_grid::Terminal>>>,
    }

    define_class!(
        // SAFETY:
        // - NSView has no subclassing requirements.
        // - `NoaTerminalAxContainer` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "NoaTerminalAxContainer"]
        pub(crate) struct TerminalAxContainer;

        impl TerminalAxContainer {
            /// Never a mouse target: every event falls through to winit's
            /// content view underneath.
            #[unsafe(method(hitTest:))]
            fn hit_test(&self, _point: NSPoint) -> *mut AnyObject {
                std::ptr::null_mut()
            }

            /// Top-left origin, like winit's content view, so pane rects map
            /// straight across.
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            #[unsafe(method(isAccessibilityElement))]
            fn is_accessibility_element(&self) -> bool {
                true
            }

            #[unsafe(method_id(accessibilityRole))]
            fn role(&self) -> Retained<NSString> {
                NSString::from_str("AXGroup")
            }

            #[unsafe(method_id(accessibilityIdentifier))]
            fn identifier(&self) -> Retained<NSString> {
                NSString::from_str("noa-terminals")
            }
        }
    );

    impl TerminalAxContainer {
        /// A container covering `view`, inserted beneath its other subviews
        /// (overlay cards stay on top).
        fn install(mtm: MainThreadMarker, view: &AnyObject) -> Retained<Self> {
            // SAFETY: main thread; `view` is winit's live content view.
            unsafe {
                let bounds: NSRect = msg_send![view, bounds];
                let this = Self::alloc(mtm).set_ivars(());
                let this: Retained<Self> = msg_send![super(this), initWithFrame: bounds];
                // NSViewWidthSizable | NSViewHeightSizable.
                let _: () = msg_send![&*this, setAutoresizingMask: 2usize | 16usize];
                // NSWindowBelow, relative to nothing: the bottom of the stack.
                let _: () = msg_send![
                    view,
                    addSubview: &*this,
                    positioned: -1isize,
                    relativeTo: std::ptr::null::<AnyObject>(),
                ];
                this
            }
        }
    }

    define_class!(
        // SAFETY:
        // - NSAccessibilityElement has no subclassing requirements.
        // - `NoaTerminalAxElement` does not implement `Drop`.
        #[unsafe(super(NSAccessibilityElement, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "NoaTerminalAxElement"]
        #[ivars = RefCell<AxState>]
        pub(crate) struct TerminalAxElement;

        impl TerminalAxElement {
            #[unsafe(method(isAccessibilityElement))]
            fn is_accessibility_element(&self) -> bool {
                true
            }

            #[unsafe(method_id(accessibilityRole))]
            fn role(&self) -> Retained<NSString> {
                NSString::from_str("AXTextArea")
            }

            #[unsafe(method_id(accessibilityRoleDescription))]
            fn role_description(&self) -> Retained<NSString> {
                NSString::from_str("terminal")
            }

            #[unsafe(method_id(accessibilityIdentifier))]
            fn identifier(&self) -> Retained<NSString> {
                NSString::from_str(&format!("noa-terminal-{}", self.ivars().borrow().id))
            }

            #[unsafe(method_id(accessibilityTitle))]
            fn title(&self) -> Retained<NSString> {
                let name = self
                    .with_terminal(|terminal| {
                        crate::macos_applescript::terminal_name(
                            &terminal.title,
                            terminal.cwd.as_deref(),
                        )
                    })
                    .unwrap_or_default();
                NSString::from_str(&name)
            }

            #[unsafe(method_id(accessibilityValue))]
            fn value(&self) -> Retained<NSString> {
                NSString::from_str(&self.contents())
            }

            #[unsafe(method(accessibilityNumberOfCharacters))]
            fn number_of_characters(&self) -> isize {
                self.contents().encode_utf16().count() as isize
            }

            #[unsafe(method_id(accessibilitySelectedText))]
            fn selected_text(&self) -> Retained<NSString> {
                let text = self
                    .with_terminal(terminal_selection)
                    .unwrap_or_default();
                NSString::from_str(&text)
            }

            #[unsafe(method(accessibilityInsertionPointLineNumber))]
            fn insertion_point_line_number(&self) -> isize {
                self.with_terminal(cursor_viewport_row)
                    .flatten()
                    .map_or(0, |row| row as isize)
            }

            /// Hide `AXInsertionPointLineNumber` while the cursor is off the
            /// viewport (spec L2.2: no value then).
            #[unsafe(method(isAccessibilitySelectorAllowed:))]
            fn is_selector_allowed(&self, selector: Sel) -> bool {
                selector != sel!(accessibilityInsertionPointLineNumber)
                    || self
                        .with_terminal(|terminal| cursor_viewport_row(terminal).is_some())
                        .unwrap_or(false)
            }

            #[unsafe(method(accessibilityFrame))]
            fn frame(&self) -> NSRect {
                let state = self.ivars().borrow();
                let Some(view) = state.container.load() else {
                    return NSRect::ZERO;
                };
                // SAFETY: `view` is winit's live content view (main thread);
                // `window` may be nil, checked before use.
                unsafe {
                    let in_window: NSRect =
                        msg_send![&*view, convertRect: state.frame, toView: std::ptr::null::<AnyObject>()];
                    let window: *mut AnyObject = msg_send![&*view, window];
                    if window.is_null() {
                        return NSRect::ZERO;
                    }
                    msg_send![window, convertRectToScreen: in_window]
                }
            }

            #[unsafe(method(isAccessibilityFocused))]
            fn is_focused(&self) -> bool {
                self.focused_in_key_window()
            }

            #[unsafe(method_id(accessibilityParent))]
            fn parent(&self) -> Option<Retained<AnyObject>> {
                self.ivars().borrow().container.load()
            }
        }
    );

    impl TerminalAxElement {
        fn new(mtm: MainThreadMarker, state: AxState) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(RefCell::new(state));
            unsafe { msg_send![super(this), init] }
        }

        fn with_terminal<T>(&self, read: impl FnOnce(&noa_grid::Terminal) -> T) -> Option<T> {
            let handle = self.ivars().borrow().terminal.as_ref()?.upgrade()?;
            let terminal = handle.lock();
            Some(read(&terminal))
        }

        fn focused_in_key_window(&self) -> bool {
            let state = self.ivars().borrow();
            if !state.focused {
                return false;
            }
            let Some(view) = state.container.load() else {
                return false;
            };
            // SAFETY: the live container view (main thread); `window` may be
            // nil, checked before use.
            unsafe {
                let window: *mut AnyObject = msg_send![&*view, window];
                !window.is_null() && msg_send![window, isKeyWindow]
            }
        }

        fn contents(&self) -> String {
            self.with_terminal(terminal_contents).unwrap_or_default()
        }
    }

    fn frame_in_view(pane: &AxPane, scale_factor: f64) -> NSRect {
        let (x, y, w, h) = super::pane_frame_points(pane.rect, scale_factor);
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// The tab's content view (winit's `NSView`).
    fn content_view(window: &winit::window::Window) -> Option<Retained<AnyObject>> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: winit owns this live view for the window's lifetime; we
        // only retain it for the duration of the sync.
        unsafe { Retained::retain(view) }
    }

    /// Apply `panes` to the window's elements. `false` when the window's view
    /// could not be reached (the caller retries on its next pass).
    pub(super) fn sync(
        ax: &mut WindowAx,
        window: &winit::window::Window,
        scale_factor: f64,
        panes: &[AxPane],
    ) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some(view) = content_view(window) else {
            return false;
        };
        let container = ax
            .container
            .get_or_insert_with(|| TerminalAxContainer::install(mtm, &view))
            .clone();
        let container_object: &AnyObject = &container;
        let terminal = |pane: &AxPane| pane.terminal.as_ref().map(std::sync::Arc::downgrade);
        let mut elements = Vec::with_capacity(panes.len());
        for pane in panes {
            let frame = frame_in_view(pane, scale_factor);
            let existing = ax
                .elements
                .iter()
                .find(|(id, _)| *id == pane.id)
                .map(|(_, element)| element.clone());
            let element = match existing {
                Some(element) => {
                    {
                        let mut state = element.ivars().borrow_mut();
                        state.frame = frame;
                        state.focused = pane.focused;
                        state.terminal = terminal(pane);
                    }
                    element
                }
                None => TerminalAxElement::new(
                    mtm,
                    AxState {
                        id: pane.id,
                        frame,
                        focused: pane.focused,
                        container: Weak::new(container_object),
                        terminal: terminal(pane),
                    },
                ),
            };
            elements.push((pane.id, element));
        }
        let children: Vec<&AnyObject> = elements
            .iter()
            .map(|(_, element)| -> &AnyObject { element })
            .collect();
        let array = NSArray::from_slice(&children);
        // SAFETY: `setAccessibilityChildren:` is NSView's NSAccessibility
        // setter; the array retains every element.
        unsafe {
            let _: () = msg_send![&*container, setAccessibilityChildren: &*array];
        }
        ax.elements = elements;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_frame_scale_boundaries() {
        let rect = Rect::new(20, 40, 200, 100);
        // Retina halves pixels into points; 1 is identity.
        assert_eq!(pane_frame_points(rect, 2.0), (10.0, 20.0, 100.0, 50.0));
        assert_eq!(pane_frame_points(rect, 1.0), (20.0, 40.0, 200.0, 100.0));
        // 0, negative and non-finite scales fall back to 1 instead of
        // producing inf/NaN or a mirrored frame.
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                pane_frame_points(rect, scale),
                (20.0, 40.0, 200.0, 100.0),
                "scale {scale}"
            );
        }
    }

    #[test]
    fn insertion_point_row_follows_the_viewport() {
        let mut terminal = noa_grid::Terminal::new(noa_core::GridSize::new(10, 3));
        let mut stream = noa_vt::Stream::new();
        stream.feed(b"a\r\nb\r\nc", &mut terminal);
        // Cursor on the last row (row 2), viewport at the bottom.
        assert_eq!(cursor_viewport_row(&terminal), Some(2));
        // History exists after more lines; scrolling back 1 row pushes the
        // cursor off the bottom edge (row 3 of a 3-row viewport).
        stream.feed(b"\r\nd\r\ne", &mut terminal);
        terminal.scroll_viewport_up(1);
        assert_eq!(cursor_viewport_row(&terminal), None);
        // Still scrolled back by 1, the cursor moved to the top active row
        // shows on viewport row 1.
        stream.feed(b"\x1b[H", &mut terminal);
        assert_eq!(terminal.viewport_offset(), 1);
        assert_eq!(cursor_viewport_row(&terminal), Some(1));
    }
}
