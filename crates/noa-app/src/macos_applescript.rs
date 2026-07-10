//! AppleScript / Apple Event bridge (applescript spec, Ghostty parity gap
//! `REQ-MACOS-003`). Registers a handful of Apple Event handlers with
//! `NSAppleEventManager` so `osascript`/Script Editor can drive the app through
//! a Ghostty-1.3-compatible dictionary (`assets/Noa.sdef`).
//!
//! Design (spec Pick A + Amendment 1): manual `NSAppleEventManager` handlers,
//! **not** full Cocoa Scripting. The handler runs on the main thread during
//! `NSApp` event processing, so it must never block on a round-trip: mutating
//! verbs are injected as [`UserEvent`]s through the [`EventLoopProxy`] (R-11),
//! and property reads are answered synchronously from a main-thread-maintained
//! [`AppStateSnapshot`] (Amendment 1.1). The AE handler never touches a winit
//! object directly (AC-12).
//!
//! Ghostty analog: the `apprt.macos` AppleScript support introduced in 1.3.
//!
//! Every four-char Apple Event code lives in [`codes`] and is cross-checked
//! against the shipped `.sdef` by a unit test (FM-3): the Rust table and the
//! dictionary can never silently drift.

use crate::events::{AppleScriptSpawnTarget, UserEvent};

/// A read-only projection of the window/tab/terminal tree that the AE handler
/// answers property queries from (Amendment 1.1). The main thread rebuilds it
/// (see `App::sync_applescript_snapshot`); the handler only ever locks and
/// reads it, so an Apple Event never waits on the event loop.
///
/// Mirrors the AppleScript object hierarchy: application → windows → tabs →
/// terminals. A "window" is one logical window (AppKit tab group), a "tab" is
/// one native tab, a "terminal" is one split pane.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AppStateSnapshot {
    /// Whether the app currently holds OS focus (`application.frontmost`).
    pub frontmost: bool,
    /// The app version string (`application.version`).
    pub version: String,
    pub windows: Vec<WindowSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSnapshot {
    /// Stable id: the logical window's `WindowGroupId`.
    pub id: u64,
    pub name: String,
    /// 1-based position among windows.
    pub index: usize,
    pub tabs: Vec<TabSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSnapshot {
    /// Stable id: the native tab's winit `WindowId` as a `u64`.
    pub id: u64,
    pub name: String,
    /// 1-based position among the window's tabs.
    pub index: usize,
    /// Whether this is the app's currently focused tab.
    pub selected: bool,
    pub terminals: Vec<TerminalSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSnapshot {
    /// Stable id: the split leaf's `PaneId` as a `u64`.
    pub id: u64,
    pub name: String,
    /// 1-based position among the tab's terminals.
    pub index: usize,
    /// Whether this is the focused pane within its tab.
    pub selected: bool,
    /// The shell's OSC-7-reported working directory, if any.
    pub cwd: Option<String>,
}

/// Compile a four-byte ASCII code into a `FourCharCode`/`OSType` (big-endian,
/// as Apple Events store them). Panics at const-eval if `bytes` is not exactly
/// four bytes long.
pub(crate) const fn fourcc(bytes: &[u8]) -> u32 {
    assert!(bytes.len() == 4, "AE codes must be exactly four bytes");
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | (bytes[3] as u32)
}

/// The single source of truth for every four-char code the `.sdef` references
/// (spec Amendment 1.4). Kept as string constants so the dictionary XML and the
/// Rust dispatch use byte-for-byte identical codes; the cross-check test
/// enforces that every `code="…"` attribute in the `.sdef` appears here.
#[allow(dead_code)] // Some codes exist only for `.sdef` parity / the cross-check table.
pub(crate) mod codes {
    // Apple Event classes.
    /// Our custom command suite.
    pub const CLASS_NOA: &str = "noaX";
    /// Core suite (`get`).
    pub const CLASS_CORE: &str = "core";
    /// Standard/miscellaneous suite (`activate`).
    pub const CLASS_MISC: &str = "misc";

    // Custom command event ids (within `CLASS_NOA`).
    pub const CMD_NEW_WINDOW: &str = "nwin";
    pub const CMD_NEW_TAB: &str = "ntab";
    pub const CMD_SPLIT: &str = "splt";
    pub const CMD_FOCUS: &str = "focs";
    pub const CMD_ACTIVATE_WINDOW: &str = "actw";
    pub const CMD_SELECT_TAB: &str = "stab";
    pub const CMD_INPUT_TEXT: &str = "inpt";
    pub const CMD_PERFORM_ACTION: &str = "pact";

    // Standard event ids (Cocoa Standard Suite, handled by us).
    pub const EVT_GET_DATA: &str = "getd";
    pub const EVT_ACTIVATE: &str = "actv";
    pub const EVT_CLOSE: &str = "clos";

    // Class codes used inside object specifiers.
    pub const CLS_APPLICATION: &str = "capp";
    pub const CLS_WINDOW: &str = "cwin";
    pub const CLS_TAB: &str = "cTab";
    pub const CLS_TERMINAL: &str = "cTrm";

    // Property codes.
    pub const PROP_NAME: &str = "pnam";
    pub const PROP_ID: &str = "ID  ";
    pub const PROP_INDEX: &str = "pidx";
    pub const PROP_SELECTED: &str = "selc";
    pub const PROP_VERSION: &str = "vers";
    pub const PROP_FRONTMOST: &str = "pisf";
    pub const PROP_WORKING_DIRECTORY: &str = "pwd ";

    // `split` direction enumerators + their enumeration type.
    pub const ENUM_DIRECTION: &str = "Sdir";
    pub const DIR_RIGHT: &str = "rite";
    pub const DIR_LEFT: &str = "left";
    pub const DIR_DOWN: &str = "down";
    pub const DIR_UP: &str = "up  ";

    // Parameter keywords for `new window`/`new tab`.
    pub const PARAM_INITIAL_WORKING_DIRECTORY: &str = "iwd ";
    pub const PARAM_COMMAND: &str = "cmd ";
    /// `input text … to <terminal>` target keyword (optional → focused pane).
    pub const PARAM_TARGET: &str = "ttrg";

    /// Every four-char code declared above, for the `.sdef` cross-check test.
    pub const ALL: &[&str] = &[
        CLASS_NOA,
        CLASS_CORE,
        CLASS_MISC,
        CMD_NEW_WINDOW,
        CMD_NEW_TAB,
        CMD_SPLIT,
        CMD_FOCUS,
        CMD_ACTIVATE_WINDOW,
        CMD_SELECT_TAB,
        CMD_INPUT_TEXT,
        CMD_PERFORM_ACTION,
        EVT_GET_DATA,
        EVT_ACTIVATE,
        EVT_CLOSE,
        CLS_APPLICATION,
        CLS_WINDOW,
        CLS_TAB,
        CLS_TERMINAL,
        PROP_NAME,
        PROP_ID,
        PROP_INDEX,
        PROP_SELECTED,
        PROP_VERSION,
        PROP_FRONTMOST,
        PROP_WORKING_DIRECTORY,
        ENUM_DIRECTION,
        DIR_RIGHT,
        DIR_LEFT,
        DIR_DOWN,
        DIR_UP,
        PARAM_INITIAL_WORKING_DIRECTORY,
        PARAM_COMMAND,
        PARAM_TARGET,
    ];
}

/// Standard Apple Event error codes returned to `osascript` (spec R-10).
pub(crate) mod errors {
    /// `errAEEventNotHandled` — unknown verb/action (catch-all).
    pub const EVENT_NOT_HANDLED: i32 = -1708;
    /// `errAENoSuchObject` — the target object does not exist.
    pub const NO_SUCH_OBJECT: i32 = -1728;
    /// `errAEParamMissed` — a required parameter was missing/malformed.
    pub const PARAM_MISSED: i32 = -1715;
}

/// The closed set of `perform action` names (spec L2 table): each maps to an
/// [`AppCommand`] the app already handles. Anything not listed is rejected with
/// `errAEEventNotHandled` (R-8). Reuses the keybind action vocabulary so the
/// table cannot drift from the in-app command set.
pub(crate) fn command_for_perform_action(action: &str) -> Option<crate::AppCommand> {
    crate::commands::command_from_applescript_action(action)
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct Registration;

#[cfg(not(target_os = "macos"))]
impl Registration {
    pub(crate) fn install(
        _proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        _snapshot: std::sync::Arc<parking_lot::Mutex<AppStateSnapshot>>,
    ) -> Option<Self> {
        None
    }
}

#[cfg(target_os = "macos")]
pub(crate) use imp::Registration;

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Arc;

    use core::ffi::c_void;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, Sel};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSData, NSString};
    use parking_lot::Mutex;
    use winit::event_loop::EventLoopProxy;

    use super::{
        AppStateSnapshot, AppleScriptSpawnTarget, TabSnapshot, UserEvent, codes,
        command_for_perform_action, errors, fourcc,
    };

    /// `keyDirectObject` — the Apple Event parameter carrying a command's direct
    /// object (`----`).
    const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");
    /// `keyErrorNumber` — the reply parameter an AE handler sets to report an
    /// error (`errn`).
    const KEY_ERROR_NUMBER: u32 = fourcc(b"errn");

    // Object-specifier record keywords (`AEDataModel.h`).
    const KEY_AE_DESIRED_CLASS: u32 = fourcc(b"want");
    const KEY_AE_KEY_FORM: u32 = fourcc(b"form");
    const KEY_AE_KEY_DATA: u32 = fourcc(b"seld");
    const KEY_AE_CONTAINER: u32 = fourcc(b"from");

    // Descriptor / key-form type codes.
    const TYPE_NULL: u32 = fourcc(b"null");
    const TYPE_OBJECT_SPECIFIER: u32 = fourcc(b"obj ");
    const TYPE_PROPERTY: u32 = fourcc(b"prop");
    /// `typeSInt64` (`AEDataModel.h`): 64-bit integer payload. Ids are winit
    /// `WindowId`s (pointer-derived `u64`, always > 2^31), so they must travel
    /// as 64-bit, not `typeSInt32` which would truncate them.
    const TYPE_SINT64: u32 = fourcc(b"comp");
    const FORM_ABSOLUTE_POSITION: u32 = fourcc(b"indx");
    const FORM_UNIQUE_ID: u32 = fourcc(b"ID  ");
    const FORM_PROPERTY_ID: u32 = fourcc(b"prop");

    /// The boxed state the AE handler delegate owns for the app's lifetime.
    struct Ivars {
        proxy: EventLoopProxy<UserEvent>,
        snapshot: Arc<Mutex<AppStateSnapshot>>,
    }

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements.
        // - `NoaAppleEventHandler` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "NoaAppleEventHandler"]
        #[ivars = Ivars]
        struct Handler;

        impl Handler {
            /// The Apple Event callback (`NSAppleEventManager` target/selector).
            /// Runs on the main thread; dispatches by event class+id and either
            /// injects a `UserEvent` (mutations) or fills `reply` (reads).
            #[unsafe(method(handleAppleEvent:withReplyEvent:))]
            fn handle_apple_event(
                &self,
                event: &NSAppleEventDescriptor,
                reply: &NSAppleEventDescriptor,
            ) {
                let result = self.dispatch(event);
                if let Err(code) = result {
                    set_error_number(reply, code);
                } else if let Ok(Some(value)) = result {
                    set_direct_object(reply, &value);
                }
            }
        }
    );

    impl Handler {
        fn new(
            proxy: EventLoopProxy<UserEvent>,
            snapshot: Arc<Mutex<AppStateSnapshot>>,
        ) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { proxy, snapshot });
            unsafe { msg_send![super(this), init] }
        }

        /// Dispatch one Apple Event. `Ok(None)` means handled with no reply
        /// value, `Ok(Some(desc))` means handled with a reply value (a `get`),
        /// and `Err(code)` is an AE error to report (spec R-10).
        fn dispatch(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Option<Retained<NSAppleEventDescriptor>>, i32> {
            let class = event_class(event);
            let id = event_id(event);

            if class == fourcc(codes::CLASS_CORE.as_bytes()) {
                if id == fourcc(codes::EVT_GET_DATA.as_bytes()) {
                    return self.handle_get(event).map(Some);
                }
                if id == fourcc(codes::EVT_CLOSE.as_bytes()) {
                    return self.close(event).map(|()| None);
                }
                return Err(errors::EVENT_NOT_HANDLED);
            }
            if class == fourcc(codes::CLASS_MISC.as_bytes())
                && id == fourcc(codes::EVT_ACTIVATE.as_bytes())
            {
                // Application-level `activate`: bring the focused window front.
                self.activate_frontmost();
                return Ok(None);
            }
            if class != fourcc(codes::CLASS_NOA.as_bytes()) {
                return Err(errors::EVENT_NOT_HANDLED);
            }

            match id {
                x if x == fourcc(codes::CMD_NEW_WINDOW.as_bytes()) => {
                    self.spawn(event, AppleScriptSpawnTarget::NewWindow)
                }
                x if x == fourcc(codes::CMD_NEW_TAB.as_bytes()) => {
                    self.spawn(event, AppleScriptSpawnTarget::CurrentWindow)
                }
                x if x == fourcc(codes::CMD_SPLIT.as_bytes()) => self.split(event),
                x if x == fourcc(codes::CMD_INPUT_TEXT.as_bytes()) => self.input_text(event),
                x if x == fourcc(codes::CMD_PERFORM_ACTION.as_bytes()) => {
                    self.perform_action(event)
                }
                x if x == fourcc(codes::CMD_FOCUS.as_bytes()) => self.focus(event),
                x if x == fourcc(codes::CMD_SELECT_TAB.as_bytes()) => self.select_tab(event),
                x if x == fourcc(codes::CMD_ACTIVATE_WINDOW.as_bytes()) => {
                    self.activate_window(event)
                }
                _ => Err(errors::EVENT_NOT_HANDLED),
            }
            .map(|()| None)
        }

        fn proxy(&self) -> &EventLoopProxy<UserEvent> {
            &self.ivars().proxy
        }

        fn send(&self, event: UserEvent) {
            let _ = self.proxy().send_event(event);
        }

        fn spawn(
            &self,
            event: &NSAppleEventDescriptor,
            window_target: AppleScriptSpawnTarget,
        ) -> Result<(), i32> {
            let cwd = string_param(
                event,
                fourcc(codes::PARAM_INITIAL_WORKING_DIRECTORY.as_bytes()),
            );
            let command = string_param(event, fourcc(codes::PARAM_COMMAND.as_bytes()));
            self.send(UserEvent::SpawnTab {
                window_target,
                cwd,
                command,
            });
            Ok(())
        }

        fn split(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let direction = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let dir = enum_code(&direction);
            let command = if dir == fourcc(codes::DIR_RIGHT.as_bytes()) {
                crate::AppCommand::NewSplitRight
            } else if dir == fourcc(codes::DIR_LEFT.as_bytes()) {
                crate::AppCommand::NewSplitLeft
            } else if dir == fourcc(codes::DIR_DOWN.as_bytes()) {
                crate::AppCommand::NewSplitDown
            } else if dir == fourcc(codes::DIR_UP.as_bytes()) {
                crate::AppCommand::NewSplitUp
            } else {
                return Err(errors::PARAM_MISSED);
            };
            self.send(UserEvent::AppCommand(command));
            Ok(())
        }

        fn input_text(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            // The text is the direct parameter; the optional `to <terminal>`
            // parameter picks the target (focused pane when omitted).
            let text = direct_object(event)
                .and_then(|d| string_value(&d))
                .ok_or(errors::PARAM_MISSED)?;
            let (window_id, pane_id) = match param(event, fourcc(codes::PARAM_TARGET.as_bytes())) {
                Some(spec) => self.resolve_terminal_spec(&spec)?,
                None => self.focused_terminal()?,
            };
            self.send(UserEvent::WriteText {
                window_id,
                pane_id,
                text,
            });
            Ok(())
        }

        fn perform_action(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let action = direct_object(event)
                .and_then(|d| string_value(&d))
                .ok_or(errors::PARAM_MISSED)?;
            let command =
                command_for_perform_action(action.trim()).ok_or(errors::EVENT_NOT_HANDLED)?;
            self.send(UserEvent::AppCommand(command));
            Ok(())
        }

        /// Raise a resolved target: bring its native tab/window front and focus
        /// `pane_id`. `activate_app` additionally brings the whole app forward
        /// (application-level `activate` / `activate window`).
        fn raise(
            &self,
            window_id: winit::window::WindowId,
            pane_id: crate::split_tree::PaneId,
            activate_app: bool,
        ) {
            self.send(UserEvent::RaiseWindow {
                window_id,
                pane_id,
                activate_app,
            });
        }

        fn focus(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let (window_id, pane_id) = self.resolve_terminal_target(event)?;
            self.raise(window_id, pane_id, false);
            Ok(())
        }

        /// `select tab`: bring the target tab front by focusing its selected
        /// terminal (which also raises the tab's window).
        fn select_tab(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let (window_id, pane_id) = self.resolve_tab_target(event)?;
            self.raise(window_id, pane_id, false);
            Ok(())
        }

        /// `activate window`: bring the target window front by focusing its
        /// first tab's selected terminal.
        fn activate_window(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let reference = parse_specifier(&spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.ivars().snapshot.lock();
            let resolved = resolve(&reference, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
            let Resolved::Window(w) = resolved else {
                return Err(errors::NO_SUCH_OBJECT);
            };
            let tab = snapshot.windows[w]
                .tabs
                .first()
                .ok_or(errors::NO_SUCH_OBJECT)?;
            let (window_id, pane_id) = tab_focus_target(tab)?;
            // R-5: `activate window` brings the whole app to the front.
            self.raise(window_id, pane_id, true);
            Ok(())
        }

        /// Standard `close` (spec R-6): the direct object's resolved class picks
        /// the granularity — terminal → close pane, tab → close tab, window →
        /// close window. Tab/window closes focus the target first, then route
        /// through the same confirm/close path as the UI (AC-7).
        fn close(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let reference = parse_specifier(&spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.ivars().snapshot.lock();
            let resolved = resolve(&reference, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
            match resolved {
                Resolved::Terminal(w, t, k) => {
                    let tab = &snapshot.windows[w].tabs[t];
                    let window_id = winit::window::WindowId::from(tab.id);
                    let pane_id = crate::split_tree::PaneId::new(tab.terminals[k].id);
                    drop(snapshot);
                    self.send(UserEvent::ClosePane { window_id, pane_id });
                }
                Resolved::Tab(w, t) => {
                    let (window_id, pane_id) = tab_focus_target(&snapshot.windows[w].tabs[t])?;
                    drop(snapshot);
                    self.raise(window_id, pane_id, false);
                    self.send(UserEvent::AppCommand(crate::AppCommand::CloseTab));
                }
                Resolved::Window(w) => {
                    let tab = snapshot.windows[w]
                        .tabs
                        .first()
                        .ok_or(errors::NO_SUCH_OBJECT)?;
                    let (window_id, pane_id) = tab_focus_target(tab)?;
                    drop(snapshot);
                    self.raise(window_id, pane_id, false);
                    self.send(UserEvent::AppCommand(crate::AppCommand::CloseWindow));
                }
                Resolved::App => return Err(errors::NO_SUCH_OBJECT),
            }
            Ok(())
        }

        fn activate_frontmost(&self) {
            // Raise the currently-selected tab and bring the app forward.
            let snapshot = self.ivars().snapshot.lock();
            if let Some((window_id, pane_id)) = frontmost_target(&snapshot) {
                drop(snapshot);
                self.raise(window_id, pane_id, true);
            }
        }

        /// Resolve a command's direct object (an object specifier) to a concrete
        /// `(WindowId, PaneId)` terminal target.
        fn resolve_terminal_target(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<(winit::window::WindowId, crate::split_tree::PaneId), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            self.resolve_terminal_spec(&spec)
        }

        /// Resolve a terminal object specifier to a concrete `(WindowId,
        /// PaneId)`.
        fn resolve_terminal_spec(
            &self,
            spec: &NSAppleEventDescriptor,
        ) -> Result<(winit::window::WindowId, crate::split_tree::PaneId), i32> {
            let reference = parse_specifier(spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.ivars().snapshot.lock();
            let resolved = resolve(&reference, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
            match resolved {
                Resolved::Terminal(w, t, k) => {
                    let tab = &snapshot.windows[w].tabs[t];
                    Ok((
                        winit::window::WindowId::from(tab.id),
                        crate::split_tree::PaneId::new(tab.terminals[k].id),
                    ))
                }
                _ => Err(errors::NO_SUCH_OBJECT),
            }
        }

        /// The focused pane's `(WindowId, PaneId)`, for a target-less
        /// `input text`.
        fn focused_terminal(
            &self,
        ) -> Result<(winit::window::WindowId, crate::split_tree::PaneId), i32> {
            let snapshot = self.ivars().snapshot.lock();
            frontmost_target(&snapshot).ok_or(errors::NO_SUCH_OBJECT)
        }

        /// Resolve a direct object that is a tab specifier to `(WindowId of the
        /// tab, PaneId of its selected terminal)`.
        fn resolve_tab_target(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<(winit::window::WindowId, crate::split_tree::PaneId), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let reference = parse_specifier(&spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.ivars().snapshot.lock();
            let resolved = resolve(&reference, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
            match resolved {
                Resolved::Tab(w, t) => tab_focus_target(&snapshot.windows[w].tabs[t]),
                _ => Err(errors::NO_SUCH_OBJECT),
            }
        }

        /// Answer a `get` for a property specifier from the snapshot (R-9).
        fn handle_get(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let reference = parse_specifier(&spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.ivars().snapshot.lock();
            read_property(&reference, &snapshot)
        }
    }

    /// One installed AE bridge. Owns the boxed delegate for the app's lifetime;
    /// dropping it removes every handler (mirrors `macos_hotkey::Registration`).
    pub(crate) struct Registration {
        _handler: Retained<Handler>,
    }

    impl Registration {
        pub(crate) fn install(
            proxy: EventLoopProxy<UserEvent>,
            snapshot: Arc<Mutex<AppStateSnapshot>>,
        ) -> Option<Self> {
            let handler = Handler::new(proxy, snapshot);
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let selector: Sel = sel!(handleAppleEvent:withReplyEvent:);
            for (class, id) in handler_events() {
                // SAFETY: `handler` outlives every registration (owned by the
                // returned `Registration`) and the selector exists on it.
                unsafe {
                    let _: () = msg_send![
                        &*manager,
                        setEventHandler: &*handler as &AnyObject,
                        andSelector: selector,
                        forEventClass: class,
                        andEventID: id,
                    ];
                }
            }
            log::info!("registered AppleScript Apple Event handlers");
            Some(Self { _handler: handler })
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            for (class, id) in handler_events() {
                // SAFETY: removes the handlers installed in `install`; the boxed
                // delegate is freed only after this (it is a struct field).
                unsafe {
                    let _: () = msg_send![
                        &*manager,
                        removeEventHandlerForEventClass: class,
                        andEventID: id,
                    ];
                }
            }
        }
    }

    /// Every `(event class, event id)` pair the bridge registers.
    fn handler_events() -> Vec<(u32, u32)> {
        let noa = fourcc(codes::CLASS_NOA.as_bytes());
        let core = fourcc(codes::CLASS_CORE.as_bytes());
        let mut events = vec![
            (core, fourcc(codes::EVT_GET_DATA.as_bytes())),
            (core, fourcc(codes::EVT_CLOSE.as_bytes())),
            (
                fourcc(codes::CLASS_MISC.as_bytes()),
                fourcc(codes::EVT_ACTIVATE.as_bytes()),
            ),
        ];
        for id in [
            codes::CMD_NEW_WINDOW,
            codes::CMD_NEW_TAB,
            codes::CMD_SPLIT,
            codes::CMD_FOCUS,
            codes::CMD_ACTIVATE_WINDOW,
            codes::CMD_SELECT_TAB,
            codes::CMD_INPUT_TEXT,
            codes::CMD_PERFORM_ACTION,
        ] {
            events.push((noa, fourcc(id.as_bytes())));
        }
        events
    }

    // ── Object-specifier parsing + resolution ─────────────────────────────

    #[derive(Debug, Clone, PartialEq)]
    enum Sel2 {
        Index(i64),
        Id(i64),
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Reference {
        App,
        Element {
            class: u32,
            selector: Sel2,
            container: Box<Reference>,
        },
        Property {
            code: u32,
            container: Box<Reference>,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Resolved {
        App,
        Window(usize),
        Tab(usize, usize),
        Terminal(usize, usize, usize),
    }

    /// Parse an AEDesc that is `null` (the application) or a nested object
    /// specifier (index/id forms only — no `whose`, per R-9). Returns `None`
    /// on any unsupported form.
    fn parse_specifier(desc: &NSAppleEventDescriptor) -> Option<Reference> {
        let ty = descriptor_type(desc);
        if ty == TYPE_NULL {
            return Some(Reference::App);
        }
        if ty != TYPE_OBJECT_SPECIFIER {
            return None;
        }
        let want = keyword(desc, KEY_AE_DESIRED_CLASS).map(|d| type_code(&d))?;
        let form = keyword(desc, KEY_AE_KEY_FORM).map(|d| type_code(&d))?;
        let seld = keyword(desc, KEY_AE_KEY_DATA)?;
        let container = keyword(desc, KEY_AE_CONTAINER)
            .and_then(|c| parse_specifier(&c))
            .unwrap_or(Reference::App);

        if want == TYPE_PROPERTY || form == FORM_PROPERTY_ID {
            let code = type_code(&seld);
            return Some(Reference::Property {
                code,
                container: Box::new(container),
            });
        }

        let selector = if form == FORM_ABSOLUTE_POSITION {
            Sel2::Index(int64_value(&seld))
        } else if form == FORM_UNIQUE_ID {
            Sel2::Id(int64_value(&seld))
        } else {
            return None;
        };
        Some(Reference::Element {
            class: want,
            selector,
            container: Box::new(container),
        })
    }

    /// Resolve a parsed element reference to concrete snapshot indices.
    fn resolve(reference: &Reference, snapshot: &AppStateSnapshot) -> Option<Resolved> {
        match reference {
            Reference::App => Some(Resolved::App),
            Reference::Property { container, .. } => resolve(container, snapshot),
            Reference::Element {
                class,
                selector,
                container,
            } => {
                let container = resolve(container, snapshot)?;
                if *class == fourcc(codes::CLS_WINDOW.as_bytes()) {
                    let idx = pick(snapshot.windows.iter().map(|w| (w.id, w.index)), selector)?;
                    Some(Resolved::Window(idx))
                } else if *class == fourcc(codes::CLS_TAB.as_bytes()) {
                    let Resolved::Window(w) = container else {
                        return None;
                    };
                    let idx = pick(
                        snapshot.windows[w].tabs.iter().map(|t| (t.id, t.index)),
                        selector,
                    )?;
                    Some(Resolved::Tab(w, idx))
                } else if *class == fourcc(codes::CLS_TERMINAL.as_bytes()) {
                    let Resolved::Tab(w, t) = container else {
                        return None;
                    };
                    let idx = pick(
                        snapshot.windows[w].tabs[t]
                            .terminals
                            .iter()
                            .map(|k| (k.id, k.index)),
                        selector,
                    )?;
                    Some(Resolved::Terminal(w, t, idx))
                } else {
                    None
                }
            }
        }
    }

    /// Pick the element position matching `selector` from `(id, index)` pairs.
    /// Index form is 1-based (AppleScript convention); id form matches the
    /// stable snapshot id.
    fn pick(items: impl Iterator<Item = (u64, usize)>, selector: &Sel2) -> Option<usize> {
        match selector {
            Sel2::Index(n) => {
                let n = usize::try_from(*n).ok()?;
                items
                    .enumerate()
                    .find_map(|(pos, (_, index))| (index == n).then_some(pos))
            }
            Sel2::Id(id) => {
                // Ids round-trip as raw 64-bit patterns (a winit `WindowId`
                // often has its high bit set), so reinterpret rather than
                // reject negatives.
                let id = *id as u64;
                items
                    .enumerate()
                    .find_map(|(pos, (item_id, _))| (item_id == id).then_some(pos))
            }
        }
    }

    /// Read a property specifier's value into a reply descriptor (R-9). Only the
    /// properties the spec enumerates are answered; anything else is
    /// `errAENoSuchObject`.
    fn read_property(
        reference: &Reference,
        snapshot: &AppStateSnapshot,
    ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
        let Reference::Property { code, container } = reference else {
            return Err(errors::NO_SUCH_OBJECT);
        };
        let code = *code;
        let container = resolve(container, snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
        let name = fourcc(codes::PROP_NAME.as_bytes());
        let id = fourcc(codes::PROP_ID.as_bytes());
        let index = fourcc(codes::PROP_INDEX.as_bytes());
        let selected = fourcc(codes::PROP_SELECTED.as_bytes());
        let version = fourcc(codes::PROP_VERSION.as_bytes());
        let frontmost = fourcc(codes::PROP_FRONTMOST.as_bytes());
        let cwd = fourcc(codes::PROP_WORKING_DIRECTORY.as_bytes());

        match container {
            Resolved::App => {
                if code == name {
                    Ok(string_desc("Noa"))
                } else if code == version {
                    Ok(string_desc(&snapshot.version))
                } else if code == frontmost {
                    Ok(bool_desc(snapshot.frontmost))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Window(w) => {
                let win = &snapshot.windows[w];
                if code == name {
                    Ok(string_desc(&win.name))
                } else if code == id {
                    Ok(int64_desc(win.id))
                } else if code == index {
                    Ok(int_desc(win.index as u64))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Tab(w, t) => {
                let tab = &snapshot.windows[w].tabs[t];
                if code == name {
                    Ok(string_desc(&tab.name))
                } else if code == id {
                    Ok(int64_desc(tab.id))
                } else if code == index {
                    Ok(int_desc(tab.index as u64))
                } else if code == selected {
                    Ok(bool_desc(tab.selected))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Terminal(w, t, k) => {
                // Only the properties `.sdef` declares for a terminal (R-9):
                // id / name / working directory.
                let term = &snapshot.windows[w].tabs[t].terminals[k];
                if code == name {
                    Ok(string_desc(&term.name))
                } else if code == id {
                    Ok(int64_desc(term.id))
                } else if code == cwd {
                    Ok(string_desc(term.cwd.as_deref().unwrap_or("")))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
        }
    }

    /// `(WindowId, PaneId)` of a tab's selected terminal (falling back to its
    /// first terminal), for focus/close/select routing.
    fn tab_focus_target(
        tab: &TabSnapshot,
    ) -> Result<(winit::window::WindowId, crate::split_tree::PaneId), i32> {
        let terminal = tab
            .terminals
            .iter()
            .find(|t| t.selected)
            .or_else(|| tab.terminals.first())
            .ok_or(errors::NO_SUCH_OBJECT)?;
        Ok((
            winit::window::WindowId::from(tab.id),
            crate::split_tree::PaneId::new(terminal.id),
        ))
    }

    /// The focused tab's focus target, for application-level `activate`.
    fn frontmost_target(
        snapshot: &AppStateSnapshot,
    ) -> Option<(winit::window::WindowId, crate::split_tree::PaneId)> {
        snapshot
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .find(|t| t.selected)
            .and_then(|tab| tab_focus_target(tab).ok())
    }

    // ── Raw NSAppleEventDescriptor helpers ────────────────────────────────
    //
    // The keyword/type/event-code accessors are gated behind `objc2-core-
    // services` in the typed bindings, so they are called via raw `msg_send!`
    // with `u32` (`FourCharCode`) arguments, which needs no extra feature.

    fn event_class(desc: &NSAppleEventDescriptor) -> u32 {
        unsafe { msg_send![desc, eventClass] }
    }

    fn event_id(desc: &NSAppleEventDescriptor) -> u32 {
        unsafe { msg_send![desc, eventID] }
    }

    fn descriptor_type(desc: &NSAppleEventDescriptor) -> u32 {
        unsafe { msg_send![desc, descriptorType] }
    }

    fn type_code(desc: &NSAppleEventDescriptor) -> u32 {
        // A type/enum descriptor's four-char payload.
        unsafe { msg_send![desc, typeCodeValue] }
    }

    fn enum_code(desc: &NSAppleEventDescriptor) -> u32 {
        unsafe { msg_send![desc, enumCodeValue] }
    }

    /// Read an integer descriptor as 64-bit. Coerces to `typeSInt64` first so
    /// pointer-derived ids (which exceed `i32`) survive; falls back to the
    /// 32-bit accessor for ordinary small integers (e.g. `terminal 2`).
    fn int64_value(desc: &NSAppleEventDescriptor) -> i64 {
        let coerced: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![desc, coerceToDescriptorType: TYPE_SINT64] };
        if let Some(coerced) = coerced {
            let data: Retained<NSData> = unsafe { msg_send![&*coerced, data] };
            let bytes = data.to_vec();
            if let Ok(eight) = <[u8; 8]>::try_from(&bytes[..8.min(bytes.len())]) {
                return i64::from_le_bytes(eight);
            }
        }
        let v: i32 = unsafe { msg_send![desc, int32Value] };
        i64::from(v)
    }

    fn direct_object(event: &NSAppleEventDescriptor) -> Option<Retained<NSAppleEventDescriptor>> {
        param(event, KEY_DIRECT_OBJECT)
    }

    fn param(
        event: &NSAppleEventDescriptor,
        keyword: u32,
    ) -> Option<Retained<NSAppleEventDescriptor>> {
        unsafe { msg_send![event, paramDescriptorForKeyword: keyword] }
    }

    fn keyword(
        desc: &NSAppleEventDescriptor,
        keyword: u32,
    ) -> Option<Retained<NSAppleEventDescriptor>> {
        unsafe { msg_send![desc, descriptorForKeyword: keyword] }
    }

    fn string_param(event: &NSAppleEventDescriptor, keyword: u32) -> Option<String> {
        param(event, keyword).and_then(|d| string_value(&d))
    }

    fn string_value(desc: &NSAppleEventDescriptor) -> Option<String> {
        let ns: Option<Retained<NSString>> = unsafe { msg_send![desc, stringValue] };
        ns.map(|s| s.to_string())
    }

    fn string_desc(value: &str) -> Retained<NSAppleEventDescriptor> {
        let ns = NSString::from_str(value);
        NSAppleEventDescriptor::descriptorWithString(&ns)
    }

    fn int_desc(value: u64) -> Retained<NSAppleEventDescriptor> {
        NSAppleEventDescriptor::descriptorWithInt32(value as i32)
    }

    /// A 64-bit integer descriptor (`typeSInt64`) for ids that exceed `i32`
    /// (winit `WindowId`s). The `u64` bit pattern is transmitted verbatim and
    /// read back the same way, so `get id of tab` and `tab id <N>` round-trip.
    fn int64_desc(value: u64) -> Retained<NSAppleEventDescriptor> {
        let bytes = value.to_le_bytes();
        // `descriptorWithDescriptorType:bytes:length:` takes a `DescType`
        // (gated in the typed bindings), so build it via raw `msg_send!` with a
        // `u32` type code — no extra feature needed.
        let class = objc2::runtime::AnyClass::get(c"NSAppleEventDescriptor");
        let desc: Option<Retained<NSAppleEventDescriptor>> = class.and_then(|class| unsafe {
            // SAFETY: `bytes` is a live 8-byte buffer for the duration of the
            // call; the class responds to this standard constructor.
            msg_send![
                class,
                descriptorWithDescriptorType: TYPE_SINT64,
                bytes: bytes.as_ptr() as *const c_void,
                length: bytes.len(),
            ]
        });
        desc.unwrap_or_else(|| NSAppleEventDescriptor::descriptorWithInt32(value as i32))
    }

    fn bool_desc(value: bool) -> Retained<NSAppleEventDescriptor> {
        NSAppleEventDescriptor::descriptorWithBoolean(value as u8)
    }

    fn set_direct_object(reply: &NSAppleEventDescriptor, value: &NSAppleEventDescriptor) {
        unsafe {
            let _: () = msg_send![reply, setParamDescriptor: value, forKeyword: KEY_DIRECT_OBJECT];
        }
    }

    fn set_error_number(reply: &NSAppleEventDescriptor, code: i32) {
        let desc = NSAppleEventDescriptor::descriptorWithInt32(code);
        unsafe {
            let _: () = msg_send![reply, setParamDescriptor: &*desc, forKeyword: KEY_ERROR_NUMBER];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_packs_big_endian() {
        assert_eq!(fourcc(b"----"), 0x2D2D_2D2D);
        assert_eq!(fourcc(b"ID  "), 0x4944_2020);
    }

    // FM-3: every four-char `code="…"` the shipped `.sdef` references must be
    // present in the Rust code table, so the dictionary and dispatch can never
    // silently drift. 8-char command codes split into class + id, each checked.
    #[test]
    fn sdef_codes_are_all_in_the_rust_table() {
        let sdef = include_str!("../../../assets/Noa.sdef");
        let known: std::collections::HashSet<&str> = codes::ALL.iter().copied().collect();

        let mut checked = 0usize;
        for (idx, _) in sdef.match_indices("code=\"") {
            let rest = &sdef[idx + "code=\"".len()..];
            let end = rest.find('"').expect("unterminated code attribute");
            let code = &rest[..end];
            match code.len() {
                4 => assert!(
                    known.contains(code),
                    "sdef code {code:?} missing from table"
                ),
                8 => {
                    let (class, id) = code.split_at(4);
                    assert!(
                        known.contains(class),
                        "sdef command class {class:?} (in {code:?}) missing from table"
                    );
                    assert!(
                        known.contains(id),
                        "sdef command id {id:?} (in {code:?}) missing from table"
                    );
                }
                other => panic!("sdef code {code:?} has unexpected length {other}"),
            }
            checked += 1;
        }
        assert!(checked > 0, "no code attributes found in the sdef");
    }
}
