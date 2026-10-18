//! AppleScript / Apple Event bridge (applescript spec, Ghostty parity gap
//! `REQ-MACOS-003`; extended by the agent-automation spec). Registers Apple
//! Event handlers with `NSAppleEventManager` so `osascript`/Script Editor can
//! drive the app through a Ghostty-1.3-compatible dictionary
//! (`assets/Noa.sdef`).
//!
//! Design (applescript Pick A + Amendment 1): manual `NSAppleEventManager`
//! handlers, **not** full Cocoa Scripting. The handler runs on the main thread
//! during `NSApp` event processing, so it must never block on a round-trip:
//! mutating verbs are queued as [`ScriptAction`]s and the event loop is woken
//! with [`UserEvent::AppleScript`] (R-11), and property reads are answered
//! synchronously from a main-thread-maintained [`AppStateSnapshot`]
//! (Amendment 1.1). The AE handler never touches a winit object directly.
//!
//! Creation verbs return their object at once (agent-automation REQ-008): the
//! handler pre-allocates the new window/tab/terminal ids, records them in the
//! [`PendingRegistry`] (merged into every snapshot rebuild until the real
//! object appears), and hands the same ids to the main thread, which creates
//! the object under them.
//!
//! Every four-char Apple Event code lives in [`codes`] and is cross-checked
//! against the shipped `.sdef` by a unit test (FM-3).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use noa_grid::Terminal;
use parking_lot::Mutex;
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

use crate::applescript_keys::ScriptKeyDef;
use crate::events::UserEvent;

/// A pane's terminal, shared with its io thread.
pub(crate) type TerminalHandle = Arc<Mutex<Terminal>>;

/// A read-only projection of the window/tab/terminal tree that the AE handler
/// answers property queries from (Amendment 1.1). The main thread rebuilds it
/// (see `App::sync_applescript_snapshot`); the handler reads it and only ever
/// appends pending entries, so an Apple Event never waits on the event loop.
///
/// Mirrors the AppleScript object hierarchy: application → windows → tabs →
/// terminals. A "window" is one logical window (AppKit tab group), a "tab" is
/// one native tab, a "terminal" is one split pane. Element indices are
/// positions in these vectors (1-based in AppleScript).
#[derive(Default, Clone)]
pub struct AppStateSnapshot {
    /// Whether the app currently holds OS focus (`application.frontmost`).
    pub frontmost: bool,
    /// The app version string (`application.version`).
    pub version: String,
    pub windows: Vec<WindowSnapshot>,
    /// Position of `front window`: the window holding the key (or last key)
    /// tab. Never a pending window.
    pub front_window: Option<usize>,
}

#[derive(Clone)]
pub struct WindowSnapshot {
    /// Stable id: the logical window's `WindowGroupId`.
    pub id: u64,
    pub name: String,
    pub tabs: Vec<TabSnapshot>,
    pub pending: bool,
}

#[derive(Clone)]
pub struct TabSnapshot {
    /// Stable id: the native tab's `TabId`.
    pub id: u64,
    pub name: String,
    /// Whether this is the selected tab of its window.
    pub selected: bool,
    pub terminals: Vec<TerminalSnapshot>,
    pub pending: bool,
}

#[derive(Clone)]
pub struct TerminalSnapshot {
    /// Stable id: the split leaf's `PaneId`.
    pub id: u64,
    pub name: String,
    /// Whether this is the focused pane within its tab.
    pub selected: bool,
    /// The shell's OSC-7-reported working directory, if any.
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    pub tty: Option<String>,
    /// The live terminal, for `contents`/`selection`/`scrollback`. `None`
    /// while pending or for a pane with no local terminal.
    pub terminal: Option<TerminalHandle>,
    pub pending: bool,
}

/// Process-unique AppleScript tab ids (agent-automation L2.1). Tabs used to be
/// identified by their winit `WindowId`, which cannot be known before the
/// window exists; a counter can be handed out by the AE handler up front.
static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn alloc_tab_id() -> u64 {
    NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed)
}

/// Process-unique logical-window (tab group) ids, shared by the app's own
/// window creation and the AE handler's pre-allocation.
static NEXT_GROUP_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn alloc_group_id() -> u64 {
    NEXT_GROUP_ID.fetch_add(1, Ordering::Relaxed)
}

/// Objects a creation verb has promised but the main thread has not created
/// yet (agent-automation L2.1). Kept apart from the snapshot because the
/// snapshot is replaced wholesale on every rebuild; each rebuild re-merges the
/// registry, and an entry leaves once its live object appears (or the main
/// thread reports the spawn failed).
#[derive(Debug, Default)]
pub(crate) struct PendingRegistry {
    /// Pending window (group) ids.
    windows: Vec<u64>,
    /// `(tab id, window id)`.
    tabs: Vec<(u64, u64)>,
    /// `(pane id, tab id)`.
    terminals: Vec<(u64, u64)>,
}

impl PendingRegistry {
    pub(crate) fn add_window(&mut self, window: u64) {
        self.windows.push(window);
    }

    pub(crate) fn add_tab(&mut self, tab: u64, window: u64) {
        self.tabs.push((tab, window));
    }

    pub(crate) fn add_terminal(&mut self, pane: u64, tab: u64) {
        self.terminals.push((pane, tab));
    }

    /// Retire exactly the ids a successful spawn just created. Unlike
    /// [`Self::forget`] this does not cascade: a split already promised
    /// inside the new tab stays pending until its own action runs. The next
    /// snapshot rebuild shows the objects as live (or, if they already
    /// closed, not at all) — never as pending again.
    pub(crate) fn retire(&mut self, pane: u64, tab: Option<u64>, window: Option<u64>) {
        self.terminals.retain(|(id, _)| *id != pane);
        if let Some(tab) = tab {
            self.tabs.retain(|(id, _)| *id != tab);
        }
        if let Some(window) = window {
            self.windows.retain(|id| *id != window);
        }
    }

    /// Drop everything a failed spawn promised: the pane, and the tab/window
    /// it would have created.
    pub(crate) fn forget(&mut self, pane: u64, tab: Option<u64>, window: Option<u64>) {
        self.terminals.retain(|(id, _)| *id != pane);
        if let Some(tab) = tab {
            self.tabs.retain(|(id, _)| *id != tab);
            self.terminals.retain(|(_, owner)| *owner != tab);
        }
        if let Some(window) = window {
            self.windows.retain(|id| *id != window);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.tabs.is_empty() && self.terminals.is_empty()
    }
}

/// Merge `pending` into `snapshot` (agent-automation L2.1): prune entries
/// whose live object now exists, then append the rest after the live ones.
/// Idempotent — re-merging into a snapshot that already holds the pending
/// entries changes nothing. Inside a pending container the last pending child
/// is its `selected tab`/`focused terminal`; a live container's selection
/// never moves to a pending child.
pub(crate) fn merge_pending(snapshot: &mut AppStateSnapshot, pending: &mut PendingRegistry) {
    let live_windows: Vec<u64> = snapshot
        .windows
        .iter()
        .filter(|w| !w.pending)
        .map(|w| w.id)
        .collect();
    let live_tabs: Vec<u64> = snapshot
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .filter(|t| !t.pending)
        .map(|t| t.id)
        .collect();
    let live_panes: Vec<u64> = snapshot
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .flat_map(|t| t.terminals.iter())
        .filter(|k| !k.pending)
        .map(|k| k.id)
        .collect();
    pending.windows.retain(|id| !live_windows.contains(id));
    pending.tabs.retain(|(id, _)| !live_tabs.contains(id));
    pending.terminals.retain(|(id, _)| !live_panes.contains(id));

    for &window in &pending.windows {
        if !snapshot.windows.iter().any(|w| w.id == window) {
            snapshot.windows.push(WindowSnapshot {
                id: window,
                name: String::new(),
                tabs: Vec::new(),
                pending: true,
            });
        }
    }
    for &(tab, window) in &pending.tabs {
        if snapshot
            .windows
            .iter()
            .any(|w| w.tabs.iter().any(|t| t.id == tab))
        {
            continue;
        }
        let Some(win) = snapshot.windows.iter_mut().find(|w| w.id == window) else {
            continue;
        };
        if win.pending {
            for existing in &mut win.tabs {
                existing.selected = false;
            }
        }
        let selected = win.pending;
        win.tabs.push(TabSnapshot {
            id: tab,
            name: String::new(),
            selected,
            terminals: Vec::new(),
            pending: true,
        });
    }
    for &(pane, tab) in &pending.terminals {
        if snapshot
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .any(|t| t.terminals.iter().any(|k| k.id == pane))
        {
            continue;
        }
        let Some(owner) = snapshot
            .windows
            .iter_mut()
            .flat_map(|w| w.tabs.iter_mut())
            .find(|t| t.id == tab)
        else {
            continue;
        };
        if owner.pending {
            for existing in &mut owner.terminals {
                existing.selected = false;
            }
        }
        let selected = owner.pending;
        owner.terminals.push(TerminalSnapshot {
            id: pane,
            name: String::new(),
            selected,
            cwd: None,
            pid: None,
            tty: None,
            terminal: None,
            pending: true,
        });
    }
}

/// The six `surface configuration` fields (agent-automation L2.4), already
/// validated; empty strings arrive as `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SurfaceConfig {
    pub(crate) font_size: Option<f64>,
    pub(crate) cwd: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) initial_input: Option<String>,
    pub(crate) wait_after_command: bool,
    pub(crate) env: Vec<(String, String)>,
}

/// Parse `KEY=VALUE` assignments (spec L2.4): split at the first `=`, key must
/// be non-empty. `None` on any malformed entry.
pub(crate) fn parse_env_assignments(items: &[String]) -> Option<Vec<(String, String)>> {
    items
        .iter()
        .map(|item| {
            let (key, value) = item.split_once('=')?;
            (!key.is_empty()).then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptSplit {
    Right,
    Left,
    Down,
    Up,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScriptMouse {
    Button {
        button: MouseButton,
        pressed: bool,
    },
    /// Pane-local point coordinates, origin at the pane's top-left.
    Position {
        x: f64,
        y: f64,
    },
    /// `precise == false`: whole wheel notches; `true`: points.
    Scroll {
        dx: f64,
        dy: f64,
        precise: bool,
    },
}

/// One mutation the AE handler hands the main thread. Ids are raw `PaneId`/
/// `TabId`/`WindowGroupId` values, resolved against live state when the
/// action runs; an action whose target has gone is dropped.
#[derive(Debug, Clone)]
pub(crate) enum ScriptAction {
    SpawnTab {
        /// The logical window to join, or to create when `new_group`.
        group: u64,
        new_group: bool,
        tab_id: u64,
        pane_id: u64,
        config: SurfaceConfig,
    },
    Split {
        target: u64,
        direction: ScriptSplit,
        pane_id: u64,
        config: SurfaceConfig,
    },
    WriteText {
        pane: u64,
        text: String,
    },
    Raise {
        pane: u64,
        activate_app: bool,
    },
    ClosePane {
        pane: u64,
    },
    CloseTab {
        pane: u64,
    },
    CloseWindow {
        pane: u64,
    },
    Perform {
        pane: Option<u64>,
        command: crate::AppCommand,
    },
    SendKey {
        pane: u64,
        key: &'static ScriptKeyDef,
        mods: ModifiersState,
        pressed: bool,
    },
    SendMouse {
        pane: u64,
        mods: ModifiersState,
        event: ScriptMouse,
    },
}

/// Everything the AE handler shares with the main thread.
#[derive(Default)]
pub(crate) struct ScriptShared {
    pub(crate) snapshot: Mutex<AppStateSnapshot>,
    pub(crate) pending: Mutex<PendingRegistry>,
    pub(crate) actions: Mutex<VecDeque<ScriptAction>>,
}

/// A terminal's `name` — and its AXTitle (agent-automation L2.3): the OSC
/// 0/2 title, else the OSC 7 working directory, else empty.
pub(crate) fn terminal_name(title: &str, cwd: Option<&str>) -> String {
    if title.is_empty() {
        cwd.unwrap_or_default().to_string()
    } else {
        title.to_string()
    }
}

/// `contents`: the visible viewport's text (agent-automation Glossary).
pub(crate) fn terminal_contents(terminal: &Terminal) -> String {
    crate::ipc_bridge::screen_text(&terminal.active().visible_rows())
}

/// `selection`: the selected text, empty without a selection.
pub(crate) fn terminal_selection(terminal: &Terminal) -> String {
    terminal.selected_text().unwrap_or_default()
}

/// `scrollback`: the tail of the whole history (screen included), capped at
/// `noa_ipc::protocol::DEFAULT_TEXT_MAX_BYTES` on a UTF-8 boundary.
pub(crate) fn terminal_scrollback(terminal: &mut Terminal) -> String {
    terminal
        .scrollback_text_tail(noa_ipc::protocol::DEFAULT_TEXT_MAX_BYTES)
        .map(|(text, _)| text)
        .unwrap_or_default()
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
    /// The Noa Extensions suite (agent-automation REQ-012).
    pub const SUITE_NOA_EXT: &str = "noaE";
    /// Ghostty's command class, reused verbatim for the terms Noa adopts from
    /// Ghostty 1.3 (agent-automation Appendix B).
    pub const CLASS_GHOSTTY: &str = "Ghst";
    /// Core suite (`get`, `close`, `count`, `exists`).
    pub const CLASS_CORE: &str = "core";
    /// Standard/miscellaneous suite (`activate`).
    pub const CLASS_MISC: &str = "misc";
    /// Required suite (`quit`), answered by Cocoa.
    pub const CLASS_AEVT: &str = "aevt";

    // Custom command event ids (within `CLASS_NOA`).
    pub const CMD_NEW_WINDOW: &str = "nwin";
    pub const CMD_NEW_TAB: &str = "ntab";
    pub const CMD_SPLIT: &str = "splt";
    pub const CMD_FOCUS: &str = "focs";
    pub const CMD_ACTIVATE_WINDOW: &str = "actw";
    pub const CMD_SELECT_TAB: &str = "stab";
    pub const CMD_INPUT_TEXT: &str = "inpt";
    pub const CMD_PERFORM_ACTION: &str = "pact";

    // Ghostty command event ids (within `CLASS_GHOSTTY`).
    pub const CMD_CLOSE_TAB: &str = "ClTb";
    pub const CMD_CLOSE_WINDOW: &str = "ClWn";
    pub const CMD_NEW_SURFACE_CONFIGURATION: &str = "NSCf";
    pub const CMD_SEND_KEY: &str = "SKey";
    pub const CMD_SEND_MOUSE_BUTTON: &str = "SMBt";
    pub const CMD_SEND_MOUSE_POSITION: &str = "SMPs";
    pub const CMD_SEND_MOUSE_SCROLL: &str = "SMSc";

    // Standard event ids (handled by us).
    pub const EVT_GET_DATA: &str = "getd";
    pub const EVT_ACTIVATE: &str = "actv";
    pub const EVT_CLOSE: &str = "clos";
    pub const EVT_COUNT: &str = "cnte";
    pub const EVT_EXISTS: &str = "doex";
    pub const EVT_QUIT: &str = "quit";

    // Class codes used inside object specifiers.
    pub const CLS_APPLICATION: &str = "capp";
    pub const CLS_WINDOW: &str = "cwin";
    pub const CLS_TAB: &str = "cTab";
    pub const CLS_TERMINAL: &str = "cTrm";
    /// `item` — a generic element (`close tab 2 of window 1` reaches us as
    /// `close tab (item 2 of window 1)` once `close tab` is a command).
    pub const CLS_ITEM: &str = "cobj";

    // Property codes.
    pub const PROP_NAME: &str = "pnam";
    pub const PROP_ID: &str = "ID  ";
    pub const PROP_INDEX: &str = "pidx";
    pub const PROP_SELECTED: &str = "selc";
    pub const PROP_VERSION: &str = "vers";
    pub const PROP_FRONTMOST: &str = "pisf";
    pub const PROP_WORKING_DIRECTORY: &str = "pwd ";
    pub const PROP_FRONT_WINDOW: &str = "GFWn";
    pub const PROP_SELECTED_TAB: &str = "GWsT";
    pub const PROP_FOCUSED_TERMINAL: &str = "GTfT";
    pub const PROP_PID: &str = "Gpid";
    pub const PROP_TTY: &str = "Gtty";
    pub const PROP_CONTENTS: &str = "NEct";
    pub const PROP_SELECTION: &str = "NEsl";
    pub const PROP_SCROLLBACK: &str = "NEsb";

    // `split` direction enumerators + their enumeration type.
    pub const ENUM_DIRECTION: &str = "Sdir";
    pub const DIR_RIGHT: &str = "rite";
    pub const DIR_LEFT: &str = "left";
    pub const DIR_DOWN: &str = "down";
    pub const DIR_UP: &str = "up  ";

    // `input action` / `mouse button` / `scroll momentum` enumerations.
    pub const ENUM_INPUT_ACTION: &str = "GIAc";
    pub const ACTION_PRESS: &str = "GIpr";
    pub const ACTION_RELEASE: &str = "GIrl";
    pub const ENUM_MOUSE_BUTTON: &str = "GMBt";
    pub const BUTTON_LEFT: &str = "GMlf";
    pub const BUTTON_RIGHT: &str = "GMrt";
    pub const BUTTON_MIDDLE: &str = "GMmd";
    pub const ENUM_SCROLL_MOMENTUM: &str = "GSMo";
    pub const MOMENTUM_NONE: &str = "SMno";
    pub const MOMENTUM_BEGAN: &str = "SMbg";
    pub const MOMENTUM_CHANGED: &str = "SMch";
    pub const MOMENTUM_ENDED: &str = "SMen";
    pub const MOMENTUM_CANCELLED: &str = "SMcn";
    pub const MOMENTUM_MAY_BEGIN: &str = "SMmb";
    pub const MOMENTUM_STATIONARY: &str = "SMst";

    // `surface configuration` record type + fields.
    pub const REC_SURFACE_CONFIGURATION: &str = "GScf";
    pub const FIELD_FONT_SIZE: &str = "GScF";
    pub const FIELD_WORKING_DIRECTORY: &str = "GScD";
    pub const FIELD_COMMAND: &str = "GScC";
    pub const FIELD_INITIAL_INPUT: &str = "GScI";
    pub const FIELD_WAIT_AFTER_COMMAND: &str = "GScW";
    pub const FIELD_ENVIRONMENT: &str = "GScE";

    // Parameter keywords.
    pub const PARAM_INITIAL_WORKING_DIRECTORY: &str = "iwd ";
    pub const PARAM_COMMAND: &str = "cmd ";
    /// `input text … to <terminal>` target keyword (optional → focused pane).
    pub const PARAM_TARGET: &str = "ttrg";
    pub const PARAM_NEW_WINDOW_CONFIGURATION: &str = "GNwS";
    pub const PARAM_NEW_TAB_IN: &str = "GNtW";
    pub const PARAM_NEW_TAB_CONFIGURATION: &str = "GNtS";
    pub const PARAM_SPLIT_DIRECTION: &str = "GSpd";
    pub const PARAM_SPLIT_CONFIGURATION: &str = "GSpS";
    pub const PARAM_PERFORM_ON: &str = "GonT";
    pub const PARAM_CONFIGURATION_FROM: &str = "GScS";
    pub const PARAM_KEY_ACTION: &str = "GKeA";
    pub const PARAM_KEY_MODIFIERS: &str = "GKeM";
    pub const PARAM_KEY_TO: &str = "GKeT";
    pub const PARAM_BUTTON_ACTION: &str = "GMbA";
    pub const PARAM_BUTTON_MODIFIERS: &str = "GMbM";
    pub const PARAM_BUTTON_TO: &str = "GMbT";
    pub const PARAM_POSITION_X: &str = "GMpX";
    pub const PARAM_POSITION_Y: &str = "GMpY";
    pub const PARAM_POSITION_MODIFIERS: &str = "GMpM";
    pub const PARAM_POSITION_TO: &str = "GMpT";
    pub const PARAM_SCROLL_X: &str = "GMsX";
    pub const PARAM_SCROLL_Y: &str = "GMsY";
    pub const PARAM_SCROLL_PRECISION: &str = "GMsP";
    pub const PARAM_SCROLL_MOMENTUM: &str = "GMsM";
    pub const PARAM_SCROLL_TO: &str = "GMsT";
    /// `count … each <class>` (`keyAEObjectClass`).
    pub const PARAM_COUNT_EACH: &str = "kocl";

    /// Every four-char code declared above, for the `.sdef` cross-check test.
    pub const ALL: &[&str] = &[
        CLASS_NOA,
        SUITE_NOA_EXT,
        CLASS_GHOSTTY,
        CLASS_CORE,
        CLASS_MISC,
        CLASS_AEVT,
        CMD_NEW_WINDOW,
        CMD_NEW_TAB,
        CMD_SPLIT,
        CMD_FOCUS,
        CMD_ACTIVATE_WINDOW,
        CMD_SELECT_TAB,
        CMD_INPUT_TEXT,
        CMD_PERFORM_ACTION,
        CMD_CLOSE_TAB,
        CMD_CLOSE_WINDOW,
        CMD_NEW_SURFACE_CONFIGURATION,
        CMD_SEND_KEY,
        CMD_SEND_MOUSE_BUTTON,
        CMD_SEND_MOUSE_POSITION,
        CMD_SEND_MOUSE_SCROLL,
        EVT_GET_DATA,
        EVT_ACTIVATE,
        EVT_CLOSE,
        EVT_COUNT,
        EVT_EXISTS,
        EVT_QUIT,
        CLS_APPLICATION,
        CLS_WINDOW,
        CLS_TAB,
        CLS_TERMINAL,
        CLS_ITEM,
        PROP_NAME,
        PROP_ID,
        PROP_INDEX,
        PROP_SELECTED,
        PROP_VERSION,
        PROP_FRONTMOST,
        PROP_WORKING_DIRECTORY,
        PROP_FRONT_WINDOW,
        PROP_SELECTED_TAB,
        PROP_FOCUSED_TERMINAL,
        PROP_PID,
        PROP_TTY,
        PROP_CONTENTS,
        PROP_SELECTION,
        PROP_SCROLLBACK,
        ENUM_DIRECTION,
        DIR_RIGHT,
        DIR_LEFT,
        DIR_DOWN,
        DIR_UP,
        ENUM_INPUT_ACTION,
        ACTION_PRESS,
        ACTION_RELEASE,
        ENUM_MOUSE_BUTTON,
        BUTTON_LEFT,
        BUTTON_RIGHT,
        BUTTON_MIDDLE,
        ENUM_SCROLL_MOMENTUM,
        MOMENTUM_NONE,
        MOMENTUM_BEGAN,
        MOMENTUM_CHANGED,
        MOMENTUM_ENDED,
        MOMENTUM_CANCELLED,
        MOMENTUM_MAY_BEGIN,
        MOMENTUM_STATIONARY,
        REC_SURFACE_CONFIGURATION,
        FIELD_FONT_SIZE,
        FIELD_WORKING_DIRECTORY,
        FIELD_COMMAND,
        FIELD_INITIAL_INPUT,
        FIELD_WAIT_AFTER_COMMAND,
        FIELD_ENVIRONMENT,
        PARAM_INITIAL_WORKING_DIRECTORY,
        PARAM_COMMAND,
        PARAM_TARGET,
        PARAM_NEW_WINDOW_CONFIGURATION,
        PARAM_NEW_TAB_IN,
        PARAM_NEW_TAB_CONFIGURATION,
        PARAM_SPLIT_DIRECTION,
        PARAM_SPLIT_CONFIGURATION,
        PARAM_PERFORM_ON,
        PARAM_CONFIGURATION_FROM,
        PARAM_KEY_ACTION,
        PARAM_KEY_MODIFIERS,
        PARAM_KEY_TO,
        PARAM_BUTTON_ACTION,
        PARAM_BUTTON_MODIFIERS,
        PARAM_BUTTON_TO,
        PARAM_POSITION_X,
        PARAM_POSITION_Y,
        PARAM_POSITION_MODIFIERS,
        PARAM_POSITION_TO,
        PARAM_SCROLL_X,
        PARAM_SCROLL_Y,
        PARAM_SCROLL_PRECISION,
        PARAM_SCROLL_MOMENTUM,
        PARAM_SCROLL_TO,
        PARAM_COUNT_EACH,
    ];
}

/// Standard Apple Event error codes returned to `osascript` (spec R-10).
pub(crate) mod errors {
    /// `errAECoercionFail` — a parameter had the wrong type or value.
    pub const COERCION_FAIL: i32 = -1700;
    /// `errAEEventNotHandled` — unknown verb/action (catch-all).
    pub const EVENT_NOT_HANDLED: i32 = -1708;
    /// `errAENoSuchObject` — the target object does not exist.
    pub const NO_SUCH_OBJECT: i32 = -1728;
    /// `errAEParamMissed` — a required parameter was missing/malformed.
    pub const PARAM_MISSED: i32 = -1715;
}

/// The closed set of `perform action` names (spec L2 table): each maps to an
/// [`AppCommand`] the app already handles. Anything not listed is rejected with
/// `errAEEventNotHandled` (R-8).
pub(crate) fn command_for_perform_action(action: &str) -> Option<crate::AppCommand> {
    crate::commands::command_from_applescript_action(action)
}

// ── Object specifiers (platform-independent so they unit-test) ───────────

/// An element selector inside an object specifier.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Selector {
    /// 1-based; negative counts from the end (`-1` = last).
    Index(i64),
    Id(i64),
    All,
    First,
    Middle,
    Last,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reference {
    App,
    Element {
        class: u32,
        selector: Selector,
        container: Box<Reference>,
    },
    Property {
        code: u32,
        container: Box<Reference>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Resolved {
    App,
    Window(usize),
    Tab(usize, usize),
    Terminal(usize, usize, usize),
    List(Vec<Resolved>),
}

/// Whether `code` is a property whose value is another object (resolved to an
/// object reference rather than read as a value).
pub(crate) fn is_object_property(code: u32) -> bool {
    code == fourcc(codes::PROP_FRONT_WINDOW.as_bytes())
        || code == fourcc(codes::PROP_SELECTED_TAB.as_bytes())
        || code == fourcc(codes::PROP_FOCUSED_TERMINAL.as_bytes())
}

/// Resolve a reference to snapshot positions. A value property resolves to
/// its container; an object property resolves to the object it names.
pub(crate) fn resolve(reference: &Reference, snapshot: &AppStateSnapshot) -> Option<Resolved> {
    match reference {
        Reference::App => Some(Resolved::App),
        Reference::Property { code, container } => {
            let container = resolve(container, snapshot)?;
            if !is_object_property(*code) {
                return Some(container);
            }
            resolve_object_property(*code, &container, snapshot)
        }
        Reference::Element {
            class,
            selector,
            container,
        } => {
            let container = resolve(container, snapshot)?;
            let candidates = element_candidates(*class, &container, snapshot)?;
            pick(candidates, selector, snapshot)
        }
    }
}

fn resolve_object_property(
    code: u32,
    container: &Resolved,
    snapshot: &AppStateSnapshot,
) -> Option<Resolved> {
    if code == fourcc(codes::PROP_FRONT_WINDOW.as_bytes()) {
        let Resolved::App = container else {
            return None;
        };
        return snapshot.front_window.map(Resolved::Window);
    }
    if code == fourcc(codes::PROP_SELECTED_TAB.as_bytes()) {
        let Resolved::Window(w) = *container else {
            return None;
        };
        let window = &snapshot.windows[w];
        // A live window's selection never moves to a pending tab; inside a
        // pending window its (last) pending tab is the selected one.
        return window
            .tabs
            .iter()
            .position(|tab| tab.selected && (window.pending || !tab.pending))
            .map(|t| Resolved::Tab(w, t));
    }
    if code == fourcc(codes::PROP_FOCUSED_TERMINAL.as_bytes()) {
        let Resolved::Tab(w, t) = *container else {
            return None;
        };
        let tab = &snapshot.windows[w].tabs[t];
        return tab
            .terminals
            .iter()
            .position(|term| term.selected && (tab.pending || !term.pending))
            .map(|k| Resolved::Terminal(w, t, k));
    }
    None
}

/// The elements of `class` inside `container`, in AppleScript index order.
fn element_candidates(
    class: u32,
    container: &Resolved,
    snapshot: &AppStateSnapshot,
) -> Option<Vec<Resolved>> {
    let window = fourcc(codes::CLS_WINDOW.as_bytes());
    let tab = fourcc(codes::CLS_TAB.as_bytes());
    let terminal = fourcc(codes::CLS_TERMINAL.as_bytes());
    let item = fourcc(codes::CLS_ITEM.as_bytes());
    let all_terminals = |w: usize| -> Vec<Resolved> {
        snapshot.windows[w]
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(t, tab)| (0..tab.terminals.len()).map(move |k| Resolved::Terminal(w, t, k)))
            .collect()
    };
    // Terminals gathered across tabs (or windows) are indexed in PaneId order
    // over the whole container (REQ-001), live ones first and pending ones
    // after them — not tab by tab.
    let by_pane_id = |mut terminals: Vec<Resolved>| -> Vec<Resolved> {
        terminals.sort_by_key(|resolved| match *resolved {
            Resolved::Terminal(w, t, k) => {
                let term = &snapshot.windows[w].tabs[t].terminals[k];
                (term.pending, term.id)
            }
            _ => (true, u64::MAX),
        });
        terminals
    };
    match *container {
        Resolved::App if class == window || class == item => {
            Some((0..snapshot.windows.len()).map(Resolved::Window).collect())
        }
        Resolved::App if class == terminal => Some(by_pane_id(
            (0..snapshot.windows.len())
                .flat_map(all_terminals)
                .collect(),
        )),
        Resolved::Window(w) if class == tab || class == item => Some(
            (0..snapshot.windows[w].tabs.len())
                .map(|t| Resolved::Tab(w, t))
                .collect(),
        ),
        Resolved::Window(w) if class == terminal => Some(by_pane_id(all_terminals(w))),
        Resolved::Tab(w, t) if class == terminal || class == item => Some(
            (0..snapshot.windows[w].tabs[t].terminals.len())
                .map(|k| Resolved::Terminal(w, t, k))
                .collect(),
        ),
        _ => None,
    }
}

/// The stable id of a resolved object.
pub(crate) fn object_id(resolved: &Resolved, snapshot: &AppStateSnapshot) -> Option<u64> {
    match *resolved {
        Resolved::Window(w) => Some(snapshot.windows[w].id),
        Resolved::Tab(w, t) => Some(snapshot.windows[w].tabs[t].id),
        Resolved::Terminal(w, t, k) => Some(snapshot.windows[w].tabs[t].terminals[k].id),
        Resolved::App | Resolved::List(_) => None,
    }
}

fn pick(
    candidates: Vec<Resolved>,
    selector: &Selector,
    snapshot: &AppStateSnapshot,
) -> Option<Resolved> {
    let len = candidates.len() as i64;
    let at = |n: i64| -> Option<Resolved> {
        let pos = if n > 0 { n - 1 } else { len + n };
        (0..len)
            .contains(&pos)
            .then(|| candidates[pos as usize].clone())
    };
    match selector {
        Selector::Index(0) => None,
        Selector::Index(n) => at(*n),
        Selector::First => at(1),
        Selector::Last => at(-1),
        // AppleScript's `middle`: item (count + 1) div 2.
        Selector::Middle => at((len + 1).div_euclid(2)),
        Selector::All => Some(Resolved::List(candidates)),
        Selector::Id(id) => {
            // Ids round-trip as raw 64-bit patterns, so reinterpret rather
            // than reject negatives.
            let id = *id as u64;
            candidates
                .into_iter()
                .find(|candidate| object_id(candidate, snapshot) == Some(id))
        }
    }
}

/// A script's 1-based index as a position among `len` items, or `None` when
/// it is out of range. Never overflows: `i64::MIN` arrives straight from an
/// Apple Event's `typeSInt64` payload.
pub(crate) fn one_based_index(n: i64, len: usize) -> Option<usize> {
    let pos = usize::try_from(n.checked_sub(1)?).ok()?;
    (pos < len).then_some(pos)
}

/// The `(tab, focused pane)` a tab-level verb targets: its focused terminal,
/// else its first.
pub(crate) fn tab_focus_pane(tab: &TabSnapshot) -> Option<u64> {
    tab.terminals
        .iter()
        .find(|t| t.selected)
        .or_else(|| tab.terminals.first())
        .map(|t| t.id)
}

/// The focused terminal of the front window's selected tab — the default
/// target of target-less verbs.
pub(crate) fn focused_pane(snapshot: &AppStateSnapshot) -> Option<u64> {
    let window = &snapshot.windows[snapshot.front_window?];
    let tab = window
        .tabs
        .iter()
        .find(|t| t.selected && !t.pending)
        .or_else(|| window.tabs.iter().find(|t| !t.pending))?;
    tab.terminals
        .iter()
        .find(|t| t.selected && !t.pending)
        .or_else(|| tab.terminals.iter().find(|t| !t.pending))
        .map(|t| t.id)
}

/// The id of the tab holding `pane`.
pub(crate) fn tab_of_pane(snapshot: &AppStateSnapshot, pane: u64) -> Option<u64> {
    snapshot
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .find(|t| t.terminals.iter().any(|k| k.id == pane))
        .map(|t| t.id)
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct Registration;

#[cfg(not(target_os = "macos"))]
impl Registration {
    pub(crate) fn install(
        _proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        _shared: Arc<ScriptShared>,
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
    use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSData, NSString};
    use winit::event::MouseButton;
    use winit::event_loop::EventLoopProxy;
    use winit::keyboard::ModifiersState;

    use super::{
        AppStateSnapshot, Reference, Resolved, ScriptAction, ScriptMouse, ScriptShared,
        ScriptSplit, Selector, SurfaceConfig, UserEvent, codes, command_for_perform_action, errors,
        focused_pane, fourcc, is_object_property, merge_pending, object_id, one_based_index,
        parse_env_assignments, resolve, tab_focus_pane, tab_of_pane, terminal_contents,
        terminal_scrollback, terminal_selection,
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
    const TYPE_ENUMERATED: u32 = fourcc(b"enum");
    const TYPE_ABSOLUTE_ORDINAL: u32 = fourcc(b"abso");
    const TYPE_LIST: u32 = fourcc(b"list");
    const TYPE_RECORD: u32 = fourcc(b"reco");
    const TYPE_UTF16: u32 = fourcc(b"utxt");
    const TYPE_IEEE64: u32 = fourcc(b"doub");
    const TYPE_BOOLEAN: u32 = fourcc(b"bool");
    const TYPE_SINT32: u32 = fourcc(b"long");
    /// `typeSInt64` (`AEDataModel.h`): 64-bit integer payload, so ids travel
    /// intact whatever their size.
    const TYPE_SINT64: u32 = fourcc(b"comp");
    const FORM_ABSOLUTE_POSITION: u32 = fourcc(b"indx");
    const FORM_UNIQUE_ID: u32 = fourcc(b"ID  ");
    const FORM_PROPERTY_ID: u32 = fourcc(b"prop");
    const ORDINAL_ALL: u32 = fourcc(b"all ");
    const ORDINAL_FIRST: u32 = fourcc(b"firs");
    const ORDINAL_MIDDLE: u32 = fourcc(b"midd");
    const ORDINAL_LAST: u32 = fourcc(b"last");

    /// The boxed state the AE handler delegate owns for the app's lifetime.
    struct Ivars {
        proxy: EventLoopProxy<UserEvent>,
        shared: Arc<ScriptShared>,
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
            /// queues a `ScriptAction` (mutations) or fills `reply` (reads).
            #[unsafe(method(handleAppleEvent:withReplyEvent:))]
            fn handle_apple_event(
                &self,
                event: &NSAppleEventDescriptor,
                reply: &NSAppleEventDescriptor,
            ) {
                match self.dispatch(event) {
                    Err(code) => set_error_number(reply, code),
                    Ok(Some(value)) => set_direct_object(reply, &value),
                    Ok(None) => {}
                }
            }
        }
    );

    type Reply = Result<Option<Retained<NSAppleEventDescriptor>>, i32>;

    #[derive(Clone, Copy)]
    enum SpawnKind {
        Window,
        Tab,
    }

    impl Handler {
        fn new(proxy: EventLoopProxy<UserEvent>, shared: Arc<ScriptShared>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { proxy, shared });
            unsafe { msg_send![super(this), init] }
        }

        fn shared(&self) -> &ScriptShared {
            &self.ivars().shared
        }

        /// Dispatch one Apple Event (spec R-10 error mapping).
        fn dispatch(&self, event: &NSAppleEventDescriptor) -> Reply {
            let class = event_class(event);
            let id = event_id(event);
            let is = |code: &str| id == fourcc(code.as_bytes());

            if class == fourcc(codes::CLASS_CORE.as_bytes()) {
                return if is(codes::EVT_GET_DATA) {
                    self.handle_get(event).map(Some)
                } else if is(codes::EVT_CLOSE) {
                    self.close(event).map(|()| None)
                } else if is(codes::EVT_COUNT) {
                    self.count(event).map(Some)
                } else if is(codes::EVT_EXISTS) {
                    Ok(Some(self.exists(event)))
                } else {
                    Err(errors::EVENT_NOT_HANDLED)
                };
            }
            if class == fourcc(codes::CLASS_MISC.as_bytes()) && is(codes::EVT_ACTIVATE) {
                // Application-level `activate`: bring the focused tab front.
                self.activate_frontmost();
                return Ok(None);
            }
            if class == fourcc(codes::CLASS_GHOSTTY.as_bytes()) {
                return if is(codes::CMD_CLOSE_TAB) {
                    self.close_tab(event).map(|()| None)
                } else if is(codes::CMD_CLOSE_WINDOW) {
                    self.close_window(event).map(|()| None)
                } else if is(codes::CMD_NEW_SURFACE_CONFIGURATION) {
                    self.new_surface_configuration(event).map(Some)
                } else if is(codes::CMD_SEND_KEY) {
                    self.send_key(event).map(|()| None)
                } else if is(codes::CMD_SEND_MOUSE_BUTTON) {
                    self.send_mouse_button(event).map(|()| None)
                } else if is(codes::CMD_SEND_MOUSE_POSITION) {
                    self.send_mouse_position(event).map(|()| None)
                } else if is(codes::CMD_SEND_MOUSE_SCROLL) {
                    self.send_mouse_scroll(event).map(|()| None)
                } else {
                    Err(errors::EVENT_NOT_HANDLED)
                };
            }
            if class != fourcc(codes::CLASS_NOA.as_bytes()) {
                return Err(errors::EVENT_NOT_HANDLED);
            }
            if is(codes::CMD_NEW_WINDOW) {
                self.spawn(event, SpawnKind::Window).map(Some)
            } else if is(codes::CMD_NEW_TAB) {
                self.spawn(event, SpawnKind::Tab).map(Some)
            } else if is(codes::CMD_SPLIT) {
                self.split(event).map(Some)
            } else if is(codes::CMD_INPUT_TEXT) {
                self.input_text(event).map(|()| None)
            } else if is(codes::CMD_PERFORM_ACTION) {
                self.perform_action(event).map(Some)
            } else if is(codes::CMD_FOCUS) {
                self.focus(event).map(|()| None)
            } else if is(codes::CMD_SELECT_TAB) {
                self.select_tab(event).map(|()| None)
            } else if is(codes::CMD_ACTIVATE_WINDOW) {
                self.activate_window(event).map(|()| None)
            } else {
                Err(errors::EVENT_NOT_HANDLED)
            }
        }

        /// Queue a mutation and wake the event loop. Every action is followed
        /// by its own wake, so it runs no later than events sent after it.
        fn queue(&self, action: ScriptAction) {
            self.shared().actions.lock().push_back(action);
            let _ = self.ivars().proxy.send_event(UserEvent::AppleScript);
        }

        // ── Resolution helpers ───────────────────────────────────────────

        fn resolve_desc(
            &self,
            desc: &NSAppleEventDescriptor,
            snapshot: &AppStateSnapshot,
        ) -> Result<Resolved, i32> {
            let reference = parse_specifier(desc).ok_or(errors::NO_SUCH_OBJECT)?;
            resolve(&reference, snapshot).ok_or(errors::NO_SUCH_OBJECT)
        }

        /// The pane a terminal-targeted verb addresses: the resolved `param`,
        /// or the focused terminal when the parameter is absent.
        fn terminal_param(&self, event: &NSAppleEventDescriptor, keyword: u32) -> Result<u64, i32> {
            let snapshot = self.shared().snapshot.lock();
            match param(event, keyword) {
                Some(spec) => match self.resolve_desc(&spec, &snapshot)? {
                    resolved @ Resolved::Terminal(..) => {
                        object_id(&resolved, &snapshot).ok_or(errors::NO_SUCH_OBJECT)
                    }
                    _ => Err(errors::NO_SUCH_OBJECT),
                },
                None => focused_pane(&snapshot).ok_or(errors::NO_SUCH_OBJECT),
            }
        }

        // ── Reads ────────────────────────────────────────────────────────

        /// Answer a `get` (R-9 + agent-automation REQ-001/012): an element
        /// or object property yields an object reference (or a list of
        /// them); a value property yields its value.
        fn handle_get(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let reference = parse_specifier(&spec).ok_or(errors::NO_SUCH_OBJECT)?;
            let snapshot = self.shared().snapshot.lock();
            match &reference {
                Reference::Property { code, container } if !is_object_property(*code) => {
                    let container = resolve(container, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
                    read_value(*code, &container, &snapshot)
                }
                _ => {
                    let resolved = resolve(&reference, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
                    Ok(object_desc(&resolved, &snapshot))
                }
            }
        }

        fn count(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let snapshot = self.shared().snapshot.lock();
            let target = match direct_object(event) {
                Some(spec) => self.resolve_desc(&spec, &snapshot)?,
                None => Resolved::App,
            };
            let each = param(event, fourcc(codes::PARAM_COUNT_EACH.as_bytes()))
                .map(|desc| type_code(&desc));
            let n = match (&target, each) {
                (Resolved::List(items), _) => items.len(),
                // `count windows` may arrive as the application plus
                // `each window`: count `class` inside the resolved container.
                (_, Some(class)) => {
                    super::element_count(class, &target, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?
                }
                (_, None) => 1,
            };
            Ok(int_desc(n as u64))
        }

        fn exists(&self, event: &NSAppleEventDescriptor) -> Retained<NSAppleEventDescriptor> {
            let snapshot = self.shared().snapshot.lock();
            let found = direct_object(event)
                .and_then(|spec| self.resolve_desc(&spec, &snapshot).ok())
                .is_some_and(
                    |resolved| !matches!(&resolved, Resolved::List(items) if items.is_empty()),
                );
            bool_desc(found)
        }

        // ── Creation ─────────────────────────────────────────────────────

        fn spawn(
            &self,
            event: &NSAppleEventDescriptor,
            kind: SpawnKind,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let config_key = match kind {
                SpawnKind::Window => codes::PARAM_NEW_WINDOW_CONFIGURATION,
                SpawnKind::Tab => codes::PARAM_NEW_TAB_CONFIGURATION,
            };
            let mut config = match param(event, fourcc(config_key.as_bytes())) {
                Some(record) => parse_surface_configuration(&record)?,
                None => SurfaceConfig::default(),
            };
            // Legacy parameters fill only what the record left empty (L2.4).
            if config.cwd.is_none() {
                config.cwd = optional_text(event, codes::PARAM_INITIAL_WORKING_DIRECTORY)?;
            }
            if config.command.is_none() {
                config.command = optional_text(event, codes::PARAM_COMMAND)?;
            }

            let tab_id = super::alloc_tab_id();
            let pane_id = crate::split_tree::PaneId::alloc().get();
            let mut snapshot = self.shared().snapshot.lock();
            let (group, new_group) = match kind {
                SpawnKind::Window => (super::alloc_group_id(), true),
                SpawnKind::Tab => {
                    match param(event, fourcc(codes::PARAM_NEW_TAB_IN.as_bytes())) {
                        Some(spec) => match self.resolve_desc(&spec, &snapshot)? {
                            Resolved::Window(w) => (snapshot.windows[w].id, false),
                            _ => return Err(errors::NO_SUCH_OBJECT),
                        },
                        // ASSUME-6: the live front window at AE time; a new
                        // window when there is none.
                        None => match snapshot.front_window {
                            Some(w) => (snapshot.windows[w].id, false),
                            None => (super::alloc_group_id(), true),
                        },
                    }
                }
            };
            {
                let mut pending = self.shared().pending.lock();
                if new_group {
                    pending.add_window(group);
                }
                pending.add_tab(tab_id, group);
                pending.add_terminal(pane_id, tab_id);
                merge_pending(&mut snapshot, &mut pending);
            }
            let reply = match kind {
                SpawnKind::Window => find_window(&snapshot, group),
                SpawnKind::Tab => find_tab(&snapshot, tab_id),
            }
            .map(|resolved| object_desc(&resolved, &snapshot));
            drop(snapshot);
            self.queue(ScriptAction::SpawnTab {
                group,
                new_group,
                tab_id,
                pane_id,
                config,
            });
            reply.ok_or(errors::NO_SUCH_OBJECT)
        }

        /// `split <terminal> direction <dir>` or the legacy `split <dir>`
        /// (focused terminal). Returns the new terminal.
        fn split(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let direct = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let config = match param(event, fourcc(codes::PARAM_SPLIT_CONFIGURATION.as_bytes())) {
                Some(record) => parse_surface_configuration(&record)?,
                None => SurfaceConfig::default(),
            };
            let mut snapshot = self.shared().snapshot.lock();
            let (target, direction) = if descriptor_type(&direct) == TYPE_ENUMERATED {
                let direction = split_direction(enum_code(&direct)).ok_or(errors::PARAM_MISSED)?;
                (
                    focused_pane(&snapshot).ok_or(errors::NO_SUCH_OBJECT)?,
                    direction,
                )
            } else {
                let resolved = self.resolve_desc(&direct, &snapshot)?;
                let Resolved::Terminal(..) = resolved else {
                    return Err(errors::NO_SUCH_OBJECT);
                };
                let target = object_id(&resolved, &snapshot).ok_or(errors::NO_SUCH_OBJECT)?;
                let direction = param(event, fourcc(codes::PARAM_SPLIT_DIRECTION.as_bytes()))
                    .and_then(|desc| split_direction(enum_code(&desc)))
                    .ok_or(errors::PARAM_MISSED)?;
                (target, direction)
            };
            let tab = tab_of_pane(&snapshot, target).ok_or(errors::NO_SUCH_OBJECT)?;
            let pane_id = crate::split_tree::PaneId::alloc().get();
            {
                let mut pending = self.shared().pending.lock();
                pending.add_terminal(pane_id, tab);
                merge_pending(&mut snapshot, &mut pending);
            }
            let reply =
                find_terminal(&snapshot, pane_id).map(|resolved| object_desc(&resolved, &snapshot));
            drop(snapshot);
            self.queue(ScriptAction::Split {
                target,
                direction,
                pane_id,
                config,
            });
            reply.ok_or(errors::NO_SUCH_OBJECT)
        }

        fn new_surface_configuration(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let config = match param(event, fourcc(codes::PARAM_CONFIGURATION_FROM.as_bytes())) {
                Some(record) => parse_surface_configuration(&record)?,
                None => SurfaceConfig::default(),
            };
            Ok(surface_configuration_desc(&config))
        }

        // ── Mutations on existing objects ────────────────────────────────

        fn input_text(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let text = direct_object(event)
                .and_then(|d| string_value(&d))
                .ok_or(errors::PARAM_MISSED)?;
            let pane = self.terminal_param(event, fourcc(codes::PARAM_TARGET.as_bytes()))?;
            self.queue(ScriptAction::WriteText { pane, text });
            Ok(())
        }

        fn perform_action(
            &self,
            event: &NSAppleEventDescriptor,
        ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
            let action = direct_object(event)
                .and_then(|d| string_value(&d))
                .ok_or(errors::PARAM_MISSED)?;
            let command =
                command_for_perform_action(action.trim()).ok_or(errors::EVENT_NOT_HANDLED)?;
            let on = fourcc(codes::PARAM_PERFORM_ON.as_bytes());
            let pane = match param(event, on) {
                Some(_) => Some(self.terminal_param(event, on)?),
                None => None,
            };
            self.queue(ScriptAction::Perform { pane, command });
            Ok(bool_desc(true))
        }

        fn focus(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let pane = self.direct_terminal(event)?;
            self.queue(ScriptAction::Raise {
                pane,
                activate_app: false,
            });
            Ok(())
        }

        fn direct_terminal(&self, event: &NSAppleEventDescriptor) -> Result<u64, i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let snapshot = self.shared().snapshot.lock();
            match self.resolve_desc(&spec, &snapshot)? {
                resolved @ Resolved::Terminal(..) => {
                    object_id(&resolved, &snapshot).ok_or(errors::NO_SUCH_OBJECT)
                }
                _ => Err(errors::NO_SUCH_OBJECT),
            }
        }

        /// `select tab`: raise the tab by focusing its focused terminal.
        fn select_tab(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let pane = {
                let snapshot = self.shared().snapshot.lock();
                match self.resolve_desc(&spec, &snapshot)? {
                    Resolved::Tab(w, t) => tab_focus_pane(&snapshot.windows[w].tabs[t]),
                    _ => None,
                }
            }
            .ok_or(errors::NO_SUCH_OBJECT)?;
            self.queue(ScriptAction::Raise {
                pane,
                activate_app: false,
            });
            Ok(())
        }

        /// `activate window`: raise the window's selected tab and bring the
        /// app forward (R-5).
        fn activate_window(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let pane = {
                let snapshot = self.shared().snapshot.lock();
                match self.resolve_desc(&spec, &snapshot)? {
                    Resolved::Window(w) => window_focus_pane(&snapshot, w),
                    _ => None,
                }
            }
            .ok_or(errors::NO_SUCH_OBJECT)?;
            self.queue(ScriptAction::Raise {
                pane,
                activate_app: true,
            });
            Ok(())
        }

        /// Standard `close` (spec R-6): the resolved class picks the
        /// granularity, routed through the same confirm/close path as the UI.
        fn close(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let action = {
                let snapshot = self.shared().snapshot.lock();
                match self.resolve_desc(&spec, &snapshot)? {
                    Resolved::Terminal(w, t, k) => ScriptAction::ClosePane {
                        pane: snapshot.windows[w].tabs[t].terminals[k].id,
                    },
                    Resolved::Tab(w, t) => ScriptAction::CloseTab {
                        pane: tab_focus_pane(&snapshot.windows[w].tabs[t])
                            .ok_or(errors::NO_SUCH_OBJECT)?,
                    },
                    Resolved::Window(w) => ScriptAction::CloseWindow {
                        pane: window_focus_pane(&snapshot, w).ok_or(errors::NO_SUCH_OBJECT)?,
                    },
                    Resolved::App | Resolved::List(_) => return Err(errors::NO_SUCH_OBJECT),
                }
            };
            self.queue(action);
            Ok(())
        }

        /// Ghostty's `close tab <tab>`. Also accepts what the legacy
        /// `close tab 2 of window 1` compiles to once `close tab` is a
        /// command (`item 2 of window 1`, or a bare index into the front
        /// window).
        fn close_tab(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let pane = {
                let snapshot = self.shared().snapshot.lock();
                let resolved = if is_integer(&spec) {
                    let w = snapshot.front_window.ok_or(errors::NO_SUCH_OBJECT)?;
                    let t = one_based_index(int64_value(&spec), snapshot.windows[w].tabs.len())
                        .ok_or(errors::NO_SUCH_OBJECT)?;
                    Resolved::Tab(w, t)
                } else {
                    self.resolve_desc(&spec, &snapshot)?
                };
                match resolved {
                    Resolved::Tab(w, t) => tab_focus_pane(&snapshot.windows[w].tabs[t]),
                    _ => None,
                }
            }
            .ok_or(errors::NO_SUCH_OBJECT)?;
            self.queue(ScriptAction::CloseTab { pane });
            Ok(())
        }

        /// Ghostty's `close window <window>` (a bare index addresses
        /// `window N`).
        fn close_window(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let spec = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let pane = {
                let snapshot = self.shared().snapshot.lock();
                let resolved = if is_integer(&spec) {
                    let w = one_based_index(int64_value(&spec), snapshot.windows.len())
                        .ok_or(errors::NO_SUCH_OBJECT)?;
                    Resolved::Window(w)
                } else {
                    self.resolve_desc(&spec, &snapshot)?
                };
                match resolved {
                    Resolved::Window(w) => window_focus_pane(&snapshot, w),
                    _ => None,
                }
            }
            .ok_or(errors::NO_SUCH_OBJECT)?;
            self.queue(ScriptAction::CloseWindow { pane });
            Ok(())
        }

        fn activate_frontmost(&self) {
            let pane = focused_pane(&self.shared().snapshot.lock());
            if let Some(pane) = pane {
                self.queue(ScriptAction::Raise {
                    pane,
                    activate_app: true,
                });
            }
        }

        // ── Synthetic input (agent-automation REQ-010/011) ───────────────

        fn send_key(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let name = direct_object(event)
                .and_then(|d| string_value(&d))
                .ok_or(errors::PARAM_MISSED)?;
            let key = crate::applescript_keys::lookup(&name).ok_or(errors::COERCION_FAIL)?;
            let pressed = input_action(event, codes::PARAM_KEY_ACTION)?;
            let mods = modifiers_param(event, codes::PARAM_KEY_MODIFIERS)?;
            let pane = self.terminal_param(event, fourcc(codes::PARAM_KEY_TO.as_bytes()))?;
            self.queue(ScriptAction::SendKey {
                pane,
                key,
                mods,
                pressed,
            });
            Ok(())
        }

        fn send_mouse_button(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let button = direct_object(event).ok_or(errors::PARAM_MISSED)?;
            let code = enum_code(&button);
            let button = if code == fourcc(codes::BUTTON_LEFT.as_bytes()) {
                MouseButton::Left
            } else if code == fourcc(codes::BUTTON_RIGHT.as_bytes()) {
                MouseButton::Right
            } else if code == fourcc(codes::BUTTON_MIDDLE.as_bytes()) {
                MouseButton::Middle
            } else {
                return Err(errors::COERCION_FAIL);
            };
            let pressed = input_action(event, codes::PARAM_BUTTON_ACTION)?;
            let mods = modifiers_param(event, codes::PARAM_BUTTON_MODIFIERS)?;
            let pane = self.terminal_param(event, fourcc(codes::PARAM_BUTTON_TO.as_bytes()))?;
            self.queue(ScriptAction::SendMouse {
                pane,
                mods,
                event: ScriptMouse::Button { button, pressed },
            });
            Ok(())
        }

        fn send_mouse_position(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let x = required_real(event, codes::PARAM_POSITION_X)?;
            let y = required_real(event, codes::PARAM_POSITION_Y)?;
            let mods = modifiers_param(event, codes::PARAM_POSITION_MODIFIERS)?;
            let pane = self.terminal_param(event, fourcc(codes::PARAM_POSITION_TO.as_bytes()))?;
            self.queue(ScriptAction::SendMouse {
                pane,
                mods,
                event: ScriptMouse::Position { x, y },
            });
            Ok(())
        }

        fn send_mouse_scroll(&self, event: &NSAppleEventDescriptor) -> Result<(), i32> {
            let dx = required_real(event, codes::PARAM_SCROLL_X)?;
            let dy = required_real(event, codes::PARAM_SCROLL_Y)?;
            let precise = match param(event, fourcc(codes::PARAM_SCROLL_PRECISION.as_bytes())) {
                Some(desc) => boolean_value(&desc).ok_or(errors::COERCION_FAIL)?,
                None => false,
            };
            // `momentum` is validated and accepted for Ghostty dictionary
            // parity, but has no effect: Noa's wheel handling ignores the
            // phase of physical events too (spec L2.2).
            if let Some(desc) = param(event, fourcc(codes::PARAM_SCROLL_MOMENTUM.as_bytes())) {
                let code = enum_code(&desc);
                let known = [
                    codes::MOMENTUM_NONE,
                    codes::MOMENTUM_BEGAN,
                    codes::MOMENTUM_CHANGED,
                    codes::MOMENTUM_ENDED,
                    codes::MOMENTUM_CANCELLED,
                    codes::MOMENTUM_MAY_BEGIN,
                    codes::MOMENTUM_STATIONARY,
                ];
                if !known.iter().any(|m| fourcc(m.as_bytes()) == code) {
                    return Err(errors::COERCION_FAIL);
                }
            }
            let pane = self.terminal_param(event, fourcc(codes::PARAM_SCROLL_TO.as_bytes()))?;
            self.queue(ScriptAction::SendMouse {
                pane,
                mods: ModifiersState::empty(),
                event: ScriptMouse::Scroll { dx, dy, precise },
            });
            Ok(())
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
            shared: Arc<ScriptShared>,
        ) -> Option<Self> {
            let handler = Handler::new(proxy, shared);
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
        let code = |s: &str| fourcc(s.as_bytes());
        let core = code(codes::CLASS_CORE);
        let noa = code(codes::CLASS_NOA);
        let ghostty = code(codes::CLASS_GHOSTTY);
        let mut events = vec![
            (core, code(codes::EVT_GET_DATA)),
            (core, code(codes::EVT_CLOSE)),
            (core, code(codes::EVT_COUNT)),
            (core, code(codes::EVT_EXISTS)),
            (code(codes::CLASS_MISC), code(codes::EVT_ACTIVATE)),
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
            events.push((noa, code(id)));
        }
        for id in [
            codes::CMD_CLOSE_TAB,
            codes::CMD_CLOSE_WINDOW,
            codes::CMD_NEW_SURFACE_CONFIGURATION,
            codes::CMD_SEND_KEY,
            codes::CMD_SEND_MOUSE_BUTTON,
            codes::CMD_SEND_MOUSE_POSITION,
            codes::CMD_SEND_MOUSE_SCROLL,
        ] {
            events.push((ghostty, code(id)));
        }
        events
    }

    // ── Snapshot lookups ──────────────────────────────────────────────────

    fn find_window(snapshot: &AppStateSnapshot, id: u64) -> Option<Resolved> {
        snapshot
            .windows
            .iter()
            .position(|w| w.id == id)
            .map(Resolved::Window)
    }

    fn find_tab(snapshot: &AppStateSnapshot, id: u64) -> Option<Resolved> {
        snapshot.windows.iter().enumerate().find_map(|(w, window)| {
            window
                .tabs
                .iter()
                .position(|t| t.id == id)
                .map(|t| Resolved::Tab(w, t))
        })
    }

    fn find_terminal(snapshot: &AppStateSnapshot, id: u64) -> Option<Resolved> {
        snapshot.windows.iter().enumerate().find_map(|(w, window)| {
            window.tabs.iter().enumerate().find_map(|(t, tab)| {
                tab.terminals
                    .iter()
                    .position(|k| k.id == id)
                    .map(|k| Resolved::Terminal(w, t, k))
            })
        })
    }

    /// A window-level verb's pane: the selected tab's focused terminal.
    fn window_focus_pane(snapshot: &AppStateSnapshot, w: usize) -> Option<u64> {
        let window = &snapshot.windows[w];
        window
            .tabs
            .iter()
            .find(|t| t.selected)
            .or_else(|| window.tabs.first())
            .and_then(tab_focus_pane)
    }

    fn split_direction(code: u32) -> Option<ScriptSplit> {
        if code == fourcc(codes::DIR_RIGHT.as_bytes()) {
            Some(ScriptSplit::Right)
        } else if code == fourcc(codes::DIR_LEFT.as_bytes()) {
            Some(ScriptSplit::Left)
        } else if code == fourcc(codes::DIR_DOWN.as_bytes()) {
            Some(ScriptSplit::Down)
        } else if code == fourcc(codes::DIR_UP.as_bytes()) {
            Some(ScriptSplit::Up)
        } else {
            None
        }
    }

    /// The `action` parameter: `true` for press (the default).
    fn input_action(event: &NSAppleEventDescriptor, keyword: &str) -> Result<bool, i32> {
        match param(event, fourcc(keyword.as_bytes())) {
            None => Ok(true),
            Some(desc) => {
                let code = enum_code(&desc);
                if code == fourcc(codes::ACTION_PRESS.as_bytes()) {
                    Ok(true)
                } else if code == fourcc(codes::ACTION_RELEASE.as_bytes()) {
                    Ok(false)
                } else {
                    Err(errors::COERCION_FAIL)
                }
            }
        }
    }

    fn modifiers_param(
        event: &NSAppleEventDescriptor,
        keyword: &str,
    ) -> Result<ModifiersState, i32> {
        match optional_text(event, keyword)? {
            None => Ok(ModifiersState::empty()),
            Some(spec) => {
                crate::applescript_keys::parse_modifiers(&spec).ok_or(errors::COERCION_FAIL)
            }
        }
    }

    fn required_real(event: &NSAppleEventDescriptor, keyword: &str) -> Result<f64, i32> {
        let desc = param(event, fourcc(keyword.as_bytes())).ok_or(errors::PARAM_MISSED)?;
        real_value(&desc).ok_or(errors::COERCION_FAIL)
    }

    /// An optional text parameter; present-but-not-text is a coercion error,
    /// and an empty string counts as absent (L2.4).
    fn optional_text(event: &NSAppleEventDescriptor, keyword: &str) -> Result<Option<String>, i32> {
        match param(event, fourcc(keyword.as_bytes())) {
            None => Ok(None),
            Some(desc) => text_value(&desc)
                .map(|text| (!text.is_empty()).then_some(text))
                .ok_or(errors::COERCION_FAIL),
        }
    }

    // ── surface configuration records ─────────────────────────────────────

    fn parse_surface_configuration(record: &NSAppleEventDescriptor) -> Result<SurfaceConfig, i32> {
        // Anything that is not a record (a string, a number, a list) is the
        // wrong type, not an empty configuration (L2.4).
        let record = if descriptor_type(record) == TYPE_RECORD {
            objc2::Message::retain(record)
        } else {
            coerce(record, TYPE_RECORD).ok_or(errors::COERCION_FAIL)?
        };
        let record = &*record;
        let field = |code: &str| keyword(record, fourcc(code.as_bytes()));
        let text_field = |code: &str| -> Result<Option<String>, i32> {
            match field(code) {
                None => Ok(None),
                Some(desc) => text_value(&desc)
                    .map(|text| (!text.is_empty()).then_some(text))
                    .ok_or(errors::COERCION_FAIL),
            }
        };
        let font_size = match field(codes::FIELD_FONT_SIZE) {
            None => None,
            Some(desc) => {
                // Only 0 means "not given"; any other finite size (negative or
                // huge included) is clamped to the runtime range when applied
                // (L2.4).
                let size = real_value(&desc).ok_or(errors::COERCION_FAIL)?;
                (size != 0.0).then_some(size)
            }
        };
        let wait_after_command = match field(codes::FIELD_WAIT_AFTER_COMMAND) {
            None => false,
            // A boolean, or any number: 0 is false, anything else true (L2.4).
            Some(desc) => boolean_value(&desc)
                .or_else(|| real_value(&desc).map(|n| n != 0.0))
                .ok_or(errors::COERCION_FAIL)?,
        };
        let env = match field(codes::FIELD_ENVIRONMENT) {
            None => Vec::new(),
            Some(desc) => {
                let items = text_list(&desc).ok_or(errors::COERCION_FAIL)?;
                parse_env_assignments(&items).ok_or(errors::COERCION_FAIL)?
            }
        };
        Ok(SurfaceConfig {
            font_size,
            cwd: text_field(codes::FIELD_WORKING_DIRECTORY)?,
            command: text_field(codes::FIELD_COMMAND)?,
            initial_input: text_field(codes::FIELD_INITIAL_INPUT)?,
            wait_after_command,
            env,
        })
    }

    /// The reply record for `new surface configuration`: every field
    /// present, unset ones at their neutral value (Ghostty parity).
    fn surface_configuration_desc(config: &SurfaceConfig) -> Retained<NSAppleEventDescriptor> {
        let record = NSAppleEventDescriptor::recordDescriptor();
        let set = |code: &str, value: &NSAppleEventDescriptor| unsafe {
            let _: () = msg_send![
                &*record,
                setDescriptor: value,
                forKeyword: fourcc(code.as_bytes()),
            ];
        };
        set(
            codes::FIELD_FONT_SIZE,
            &real_desc(config.font_size.unwrap_or(0.0)),
        );
        set(
            codes::FIELD_WORKING_DIRECTORY,
            &string_desc(config.cwd.as_deref().unwrap_or("")),
        );
        set(
            codes::FIELD_COMMAND,
            &string_desc(config.command.as_deref().unwrap_or("")),
        );
        set(
            codes::FIELD_INITIAL_INPUT,
            &string_desc(config.initial_input.as_deref().unwrap_or("")),
        );
        set(
            codes::FIELD_WAIT_AFTER_COMMAND,
            &bool_desc(config.wait_after_command),
        );
        let env = NSAppleEventDescriptor::listDescriptor();
        for (key, value) in &config.env {
            let item = string_desc(&format!("{key}={value}"));
            unsafe {
                let _: () = msg_send![&*env, insertDescriptor: &*item, atIndex: 0isize];
            }
        }
        set(codes::FIELD_ENVIRONMENT, &env);
        record
    }

    // ── Value reads ───────────────────────────────────────────────────────

    /// Read a value property of `container` (R-9 + agent-automation
    /// REQ-001/012). A list container yields a list of values.
    fn read_value(
        code: u32,
        container: &Resolved,
        snapshot: &AppStateSnapshot,
    ) -> Result<Retained<NSAppleEventDescriptor>, i32> {
        if let Resolved::List(items) = container {
            let list = NSAppleEventDescriptor::listDescriptor();
            for item in items {
                let value = read_value(code, item, snapshot)?;
                unsafe {
                    let _: () = msg_send![&*list, insertDescriptor: &*value, atIndex: 0isize];
                }
            }
            return Ok(list);
        }
        let is = |s: &str| code == fourcc(s.as_bytes());
        match *container {
            Resolved::App => {
                if is(codes::PROP_NAME) {
                    Ok(string_desc("Noa"))
                } else if is(codes::PROP_VERSION) {
                    Ok(string_desc(&snapshot.version))
                } else if is(codes::PROP_FRONTMOST) {
                    Ok(bool_desc(snapshot.frontmost))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Window(w) => {
                let win = &snapshot.windows[w];
                if is(codes::PROP_NAME) {
                    Ok(string_desc(&win.name))
                } else if is(codes::PROP_ID) {
                    Ok(int64_desc(win.id))
                } else if is(codes::PROP_INDEX) {
                    Ok(int_desc(w as u64 + 1))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Tab(w, t) => {
                let tab = &snapshot.windows[w].tabs[t];
                if is(codes::PROP_NAME) {
                    Ok(string_desc(&tab.name))
                } else if is(codes::PROP_ID) {
                    Ok(int64_desc(tab.id))
                } else if is(codes::PROP_INDEX) {
                    Ok(int_desc(t as u64 + 1))
                } else if is(codes::PROP_SELECTED) {
                    Ok(bool_desc(tab.selected))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::Terminal(w, t, k) => {
                let term = &snapshot.windows[w].tabs[t].terminals[k];
                if is(codes::PROP_NAME) {
                    Ok(string_desc(&term.name))
                } else if is(codes::PROP_ID) {
                    Ok(int64_desc(term.id))
                } else if is(codes::PROP_WORKING_DIRECTORY) {
                    Ok(string_desc(term.cwd.as_deref().unwrap_or("")))
                } else if is(codes::PROP_PID) {
                    Ok(term
                        .pid
                        .map(|pid| int_desc(u64::from(pid)))
                        .unwrap_or_else(missing_value))
                } else if is(codes::PROP_TTY) {
                    Ok(term
                        .tty
                        .as_deref()
                        .map(string_desc)
                        .unwrap_or_else(missing_value))
                } else if is(codes::PROP_CONTENTS)
                    || is(codes::PROP_SELECTION)
                    || is(codes::PROP_SCROLLBACK)
                {
                    let Some(handle) = term.terminal.clone() else {
                        return Ok(string_desc(""));
                    };
                    let mut terminal = handle.lock();
                    let text = if is(codes::PROP_CONTENTS) {
                        terminal_contents(&terminal)
                    } else if is(codes::PROP_SELECTION) {
                        terminal_selection(&terminal)
                    } else {
                        terminal_scrollback(&mut terminal)
                    };
                    Ok(string_desc(&text))
                } else {
                    Err(errors::NO_SUCH_OBJECT)
                }
            }
            Resolved::List(_) => Err(errors::NO_SUCH_OBJECT),
        }
    }

    // ── Object specifier parsing / construction ───────────────────────────

    /// Parse an AEDesc that is `null` (the application) or a nested object
    /// specifier (index/ordinal/id/property forms — no `whose`, per R-9).
    fn parse_specifier(desc: &NSAppleEventDescriptor) -> Option<Reference> {
        let ty = descriptor_type(desc);
        if ty == TYPE_NULL {
            return Some(Reference::App);
        }
        if ty != TYPE_OBJECT_SPECIFIER {
            return None;
        }
        let want = keyword(desc, KEY_AE_DESIRED_CLASS).map(|d| type_code(&d))?;
        let form = keyword(desc, KEY_AE_KEY_FORM).map(|d| enum_code(&d))?;
        let seld = keyword(desc, KEY_AE_KEY_DATA)?;
        let container = match keyword(desc, KEY_AE_CONTAINER) {
            Some(container) => parse_specifier(&container)?,
            None => Reference::App,
        };

        if want == TYPE_PROPERTY || form == FORM_PROPERTY_ID {
            return Some(Reference::Property {
                code: type_code(&seld),
                container: Box::new(container),
            });
        }
        let selector = if form == FORM_ABSOLUTE_POSITION {
            if descriptor_type(&seld) == TYPE_ABSOLUTE_ORDINAL {
                match payload_code(&seld) {
                    code if code == ORDINAL_ALL => Selector::All,
                    code if code == ORDINAL_FIRST => Selector::First,
                    code if code == ORDINAL_MIDDLE => Selector::Middle,
                    code if code == ORDINAL_LAST => Selector::Last,
                    _ => return None,
                }
            } else {
                Selector::Index(int64_value(&seld))
            }
        } else if form == FORM_UNIQUE_ID {
            Selector::Id(int64_value(&seld))
        } else {
            return None;
        };
        Some(Reference::Element {
            class: want,
            selector,
            container: Box::new(container),
        })
    }

    /// An object reference (or a list of them) for a resolved object, as
    /// nested id-form specifiers (`terminal id N of tab id M of window id W`).
    fn object_desc(
        resolved: &Resolved,
        snapshot: &AppStateSnapshot,
    ) -> Retained<NSAppleEventDescriptor> {
        match resolved {
            Resolved::App => NSAppleEventDescriptor::nullDescriptor(),
            Resolved::List(items) => {
                let list = NSAppleEventDescriptor::listDescriptor();
                for item in items {
                    let desc = object_desc(item, snapshot);
                    unsafe {
                        let _: () = msg_send![&*list, insertDescriptor: &*desc, atIndex: 0isize];
                    }
                }
                list
            }
            Resolved::Window(w) => id_specifier(
                codes::CLS_WINDOW,
                snapshot.windows[*w].id,
                &NSAppleEventDescriptor::nullDescriptor(),
            ),
            Resolved::Tab(w, t) => id_specifier(
                codes::CLS_TAB,
                snapshot.windows[*w].tabs[*t].id,
                &object_desc(&Resolved::Window(*w), snapshot),
            ),
            Resolved::Terminal(w, t, k) => id_specifier(
                codes::CLS_TERMINAL,
                snapshot.windows[*w].tabs[*t].terminals[*k].id,
                &object_desc(&Resolved::Tab(*w, *t), snapshot),
            ),
        }
    }

    fn id_specifier(
        class: &str,
        id: u64,
        container: &NSAppleEventDescriptor,
    ) -> Retained<NSAppleEventDescriptor> {
        let record = NSAppleEventDescriptor::recordDescriptor();
        let class_desc: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![
                AnyClass::get(c"NSAppleEventDescriptor").expect("Foundation class"),
                descriptorWithTypeCode: fourcc(class.as_bytes()),
            ]
        };
        let form_desc: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![
                AnyClass::get(c"NSAppleEventDescriptor").expect("Foundation class"),
                descriptorWithEnumCode: FORM_UNIQUE_ID,
            ]
        };
        let id_desc = int64_desc(id);
        unsafe {
            let _: () =
                msg_send![&*record, setDescriptor: &*class_desc, forKeyword: KEY_AE_DESIRED_CLASS];
            let _: () =
                msg_send![&*record, setDescriptor: &*form_desc, forKeyword: KEY_AE_KEY_FORM];
            let _: () = msg_send![&*record, setDescriptor: &*id_desc, forKeyword: KEY_AE_KEY_DATA];
            let _: () = msg_send![&*record, setDescriptor: container, forKeyword: KEY_AE_CONTAINER];
        }
        // An object specifier is an AE record with the `obj ` type; the AE
        // Manager coerces between record-structured types directly.
        let coerced: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![&*record, coerceToDescriptorType: TYPE_OBJECT_SPECIFIER] };
        coerced.unwrap_or(record)
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

    /// The raw four-char payload of a code-valued descriptor (`abso`), stored
    /// in host order.
    fn payload_code(desc: &NSAppleEventDescriptor) -> u32 {
        let data: Retained<NSData> = unsafe { msg_send![desc, data] };
        let bytes = data.to_vec();
        <[u8; 4]>::try_from(&bytes[..4.min(bytes.len())])
            .map(u32::from_ne_bytes)
            .unwrap_or(0)
    }

    fn is_integer(desc: &NSAppleEventDescriptor) -> bool {
        let ty = descriptor_type(desc);
        ty == TYPE_SINT32 || ty == TYPE_SINT64
    }

    /// Read an integer descriptor as 64-bit. Coerces to `typeSInt64` first so
    /// large ids survive; falls back to the 32-bit accessor otherwise.
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

    fn coerce(desc: &NSAppleEventDescriptor, ty: u32) -> Option<Retained<NSAppleEventDescriptor>> {
        unsafe { msg_send![desc, coerceToDescriptorType: ty] }
    }

    fn real_value(desc: &NSAppleEventDescriptor) -> Option<f64> {
        let coerced = coerce(desc, TYPE_IEEE64)?;
        let data: Retained<NSData> = unsafe { msg_send![&*coerced, data] };
        let bytes = data.to_vec();
        <[u8; 8]>::try_from(&bytes[..8.min(bytes.len())])
            .ok()
            .map(f64::from_ne_bytes)
            .filter(|v| v.is_finite())
    }

    fn boolean_value(desc: &NSAppleEventDescriptor) -> Option<bool> {
        let coerced = coerce(desc, TYPE_BOOLEAN)?;
        let value: u8 = unsafe { msg_send![&*coerced, booleanValue] };
        Some(value != 0)
    }

    fn text_value(desc: &NSAppleEventDescriptor) -> Option<String> {
        let coerced = coerce(desc, TYPE_UTF16)?;
        string_value(&coerced)
    }

    /// A list of text (a lone text item counts as a one-element list).
    fn text_list(desc: &NSAppleEventDescriptor) -> Option<Vec<String>> {
        if descriptor_type(desc) != TYPE_LIST {
            return text_value(desc).map(|text| vec![text]);
        }
        let count: isize = unsafe { msg_send![desc, numberOfItems] };
        (1..=count)
            .map(|index| {
                let item: Option<Retained<NSAppleEventDescriptor>> =
                    unsafe { msg_send![desc, descriptorAtIndex: index] };
                item.as_deref().and_then(text_value)
            })
            .collect()
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

    /// A 64-bit integer descriptor (`typeSInt64`) so any id round-trips.
    fn int64_desc(value: u64) -> Retained<NSAppleEventDescriptor> {
        let bytes = value.to_le_bytes();
        let class = AnyClass::get(c"NSAppleEventDescriptor");
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

    fn real_desc(value: f64) -> Retained<NSAppleEventDescriptor> {
        let bytes = value.to_ne_bytes();
        let class = AnyClass::get(c"NSAppleEventDescriptor");
        let desc: Option<Retained<NSAppleEventDescriptor>> = class.and_then(|class| unsafe {
            // SAFETY: as in `int64_desc`.
            msg_send![
                class,
                descriptorWithDescriptorType: TYPE_IEEE64,
                bytes: bytes.as_ptr() as *const c_void,
                length: bytes.len(),
            ]
        });
        desc.unwrap_or_else(|| NSAppleEventDescriptor::descriptorWithInt32(value as i32))
    }

    fn bool_desc(value: bool) -> Retained<NSAppleEventDescriptor> {
        NSAppleEventDescriptor::descriptorWithBoolean(value as u8)
    }

    /// AppleScript's `missing value` (a type descriptor of `msng`).
    fn missing_value() -> Retained<NSAppleEventDescriptor> {
        unsafe {
            msg_send![
                AnyClass::get(c"NSAppleEventDescriptor").expect("Foundation class"),
                descriptorWithTypeCode: fourcc(b"msng"),
            ]
        }
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

    /// Descriptor-level rejection and error reporting: what the handler does
    /// with malformed parameters, independent of a running event loop.
    #[cfg(test)]
    mod tests {
        use super::*;

        fn record(
            fields: &[(&str, Retained<NSAppleEventDescriptor>)],
        ) -> Retained<NSAppleEventDescriptor> {
            let record = NSAppleEventDescriptor::recordDescriptor();
            for (code, value) in fields {
                unsafe {
                    let _: () = msg_send![
                        &*record,
                        setDescriptor: &**value,
                        forKeyword: fourcc(code.as_bytes()),
                    ];
                }
            }
            record
        }

        fn list(items: &[&str]) -> Retained<NSAppleEventDescriptor> {
            let list = NSAppleEventDescriptor::listDescriptor();
            for item in items {
                let item = string_desc(item);
                unsafe {
                    let _: () = msg_send![&*list, insertDescriptor: &*item, atIndex: 0isize];
                }
            }
            list
        }

        #[test]
        fn surface_configuration_accepts_valid_and_neutral_values() {
            let config = parse_surface_configuration(&record(&[
                (codes::FIELD_FONT_SIZE, real_desc(28.0)),
                (codes::FIELD_WORKING_DIRECTORY, string_desc("")),
                (codes::FIELD_COMMAND, string_desc("top")),
                (codes::FIELD_WAIT_AFTER_COMMAND, int_desc(2)),
                (codes::FIELD_ENVIRONMENT, list(&["FOO=bar", "E="])),
            ]))
            .expect("valid record");
            assert_eq!(config.font_size, Some(28.0));
            // Empty strings mean "not given" (L2.4).
            assert_eq!(config.cwd, None);
            assert_eq!(config.command.as_deref(), Some("top"));
            // Any non-zero number is true.
            assert!(config.wait_after_command);
            assert_eq!(
                config.env,
                vec![("FOO".into(), "bar".into()), ("E".into(), String::new())]
            );

            // Boundaries: only 0 is "unset"; -1 and 1 are kept and clamped
            // when applied (L2.4); wait-after 0 is false, 1 is true.
            for (size, want) in [(-1.0, Some(-1.0)), (0.0, None), (1.0, Some(1.0))] {
                let config = parse_surface_configuration(&record(&[(
                    codes::FIELD_FONT_SIZE,
                    real_desc(size),
                )]))
                .expect("numeric size");
                assert_eq!(config.font_size, want, "font size {size}");
            }
            for (wait, want) in [(0, false), (1, true)] {
                let config = parse_surface_configuration(&record(&[(
                    codes::FIELD_WAIT_AFTER_COMMAND,
                    int_desc(wait),
                )]))
                .expect("numeric wait");
                assert_eq!(config.wait_after_command, want, "wait {wait}");
            }
            // An empty record is the default configuration.
            assert_eq!(
                parse_surface_configuration(&record(&[])),
                Ok(SurfaceConfig::default())
            );
        }

        #[test]
        fn surface_configuration_rejects_wrong_types_with_coercion_fail() {
            let bad: [Retained<NSAppleEventDescriptor>; 5] = [
                record(&[(codes::FIELD_ENVIRONMENT, list(&["NOEQ"]))]),
                record(&[(codes::FIELD_ENVIRONMENT, list(&["=value"]))]),
                record(&[(codes::FIELD_FONT_SIZE, string_desc("large"))]),
                record(&[(codes::FIELD_WAIT_AFTER_COMMAND, string_desc("maybe"))]),
                // Not a record at all.
                string_desc("font size 20"),
            ];
            for desc in bad {
                assert_eq!(
                    parse_surface_configuration(&desc),
                    Err(errors::COERCION_FAIL)
                );
            }
        }

        #[test]
        fn specifiers_parse_supported_forms_and_reject_the_rest() {
            let window = id_specifier(
                codes::CLS_WINDOW,
                5,
                &NSAppleEventDescriptor::nullDescriptor(),
            );
            let tab = id_specifier(codes::CLS_TAB, 9, &window);
            assert_eq!(
                parse_specifier(&tab),
                Some(Reference::Element {
                    class: fourcc(codes::CLS_TAB.as_bytes()),
                    selector: Selector::Id(9),
                    container: Box::new(Reference::Element {
                        class: fourcc(codes::CLS_WINDOW.as_bytes()),
                        selector: Selector::Id(5),
                        container: Box::new(Reference::App),
                    }),
                })
            );
            assert_eq!(
                parse_specifier(&NSAppleEventDescriptor::nullDescriptor()),
                Some(Reference::App)
            );
            // Plain values are not object specifiers.
            assert_eq!(parse_specifier(&string_desc("window 1")), None);
            assert_eq!(parse_specifier(&int_desc(1)), None);
            // An ids beyond i32 survive the round trip.
            let big = id_specifier(codes::CLS_TERMINAL, u64::from(u32::MAX) + 7, &window);
            let Some(Reference::Element { selector, .. }) = parse_specifier(&big) else {
                panic!("terminal specifier");
            };
            assert_eq!(selector, Selector::Id(i64::from(u32::MAX) + 7));
        }

        #[test]
        fn errors_are_reported_in_the_reply_error_number() {
            let reply: Retained<NSAppleEventDescriptor> = unsafe {
                msg_send![
                    AnyClass::get(c"NSAppleEventDescriptor").expect("Foundation class"),
                    appleEventWithEventClass: fourcc(b"aevt"),
                    eventID: fourcc(b"ansr"),
                    targetDescriptor: &*NSAppleEventDescriptor::nullDescriptor(),
                    returnID: -1i16,
                    transactionID: 0i32,
                ]
            };
            for code in [
                errors::COERCION_FAIL,
                errors::PARAM_MISSED,
                errors::EVENT_NOT_HANDLED,
                errors::NO_SUCH_OBJECT,
            ] {
                set_error_number(&reply, code);
                let errn = param(&reply, KEY_ERROR_NUMBER).expect("errn set");
                assert_eq!(int64_value(&errn), i64::from(code));
            }
        }
    }
}

/// How many `class` elements `container` holds (for `count … each`).
pub(crate) fn element_count(
    class: u32,
    container: &Resolved,
    snapshot: &AppStateSnapshot,
) -> Option<usize> {
    element_candidates(class, container, snapshot).map(|items| items.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_name_prefers_the_title_then_the_cwd() {
        assert_eq!(terminal_name("vim", Some("/tmp")), "vim");
        assert_eq!(terminal_name("", Some("/tmp")), "/tmp");
        assert_eq!(terminal_name("", None), "");
    }

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

    /// AC-028: Noa's dictionary carries every Ghostty 1.3 term (spec
    /// Appendix B, transcribed into the fixture), and the terms Noa adopted
    /// new keep Ghostty's codes.
    #[test]
    fn sdef_contains_every_ghostty_term() {
        let sdef = include_str!("../../../assets/Noa.sdef");
        let fixture = include_str!("../tests/fixtures/ghostty-sdef-terms.txt");
        let mut terms = 0usize;
        for pair in fixture
            .lines()
            .filter(|line| !line.starts_with('#'))
            .flat_map(|line| line.split(';'))
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            let (name, code) = pair.split_once('=').expect("name=code");
            assert!(
                sdef.contains(&format!("name=\"{name}\"")),
                "Ghostty term {name:?} missing from Noa.sdef"
            );
            terms += 1;
            // Pre-existing Noa terms keep their own codes (spec L2.2); every
            // term new in agent-automation must carry Ghostty's code.
            let existing = [
                "application",
                "name",
                "frontmost",
                "version",
                "window",
                "id",
                "tab",
                "index",
                "selected",
                "terminal",
                "working directory",
                "split direction",
                "right",
                "left",
                "down",
                "up",
                "new window",
                "new tab",
                "split",
                "focus",
                "close",
                "activate window",
                "select tab",
                "input text",
                "to",
                "perform action",
                "initial working directory",
                "command",
            ];
            if !existing.contains(&name) {
                assert!(
                    sdef.contains(&format!("name=\"{name}\" code=\"{code}\"")),
                    "new term {name:?} must keep Ghostty's code {code:?}"
                );
            }
        }
        assert!(terms > 50, "fixture parsed too few terms ({terms})");
    }

    fn terminal(id: u64, selected: bool) -> TerminalSnapshot {
        TerminalSnapshot {
            id,
            name: String::new(),
            selected,
            cwd: None,
            pid: None,
            tty: None,
            terminal: None,
            pending: false,
        }
    }

    fn tab(id: u64, selected: bool, terminals: Vec<TerminalSnapshot>) -> TabSnapshot {
        TabSnapshot {
            id,
            name: String::new(),
            selected,
            terminals,
            pending: false,
        }
    }

    /// AC-001 fixture: window 1 holds tabs 10 and 11 (11 selected, panes 100
    /// and 101, 101 focused); window 2 holds tab 20 with pane 200.
    fn fixture() -> AppStateSnapshot {
        AppStateSnapshot {
            frontmost: true,
            version: "0".into(),
            windows: vec![
                WindowSnapshot {
                    id: 1,
                    name: String::new(),
                    tabs: vec![
                        tab(10, false, vec![terminal(90, true)]),
                        tab(11, true, vec![terminal(100, false), terminal(101, true)]),
                    ],
                    pending: false,
                },
                WindowSnapshot {
                    id: 2,
                    name: String::new(),
                    tabs: vec![tab(20, true, vec![terminal(200, true)])],
                    pending: false,
                },
            ],
            front_window: Some(0),
        }
    }

    fn el(class: &str, selector: Selector, container: Reference) -> Reference {
        Reference::Element {
            class: fourcc(class.as_bytes()),
            selector,
            container: Box::new(container),
        }
    }

    fn prop(code: &str, container: Reference) -> Reference {
        Reference::Property {
            code: fourcc(code.as_bytes()),
            container: Box::new(container),
        }
    }

    #[test]
    fn applescript_resolve_object_properties_and_elements() {
        let snap = fixture();
        let window1 = el(codes::CLS_WINDOW, Selector::Index(1), Reference::App);
        let tab2 = el(codes::CLS_TAB, Selector::Index(2), window1.clone());

        assert_eq!(
            resolve(&prop(codes::PROP_FRONT_WINDOW, Reference::App), &snap),
            Some(Resolved::Window(0))
        );
        assert_eq!(
            resolve(&prop(codes::PROP_SELECTED_TAB, window1.clone()), &snap),
            Some(Resolved::Tab(0, 1))
        );
        assert_eq!(
            resolve(&prop(codes::PROP_FOCUSED_TERMINAL, tab2), &snap),
            Some(Resolved::Terminal(0, 1, 1))
        );
        assert_eq!(
            resolve(
                &el(codes::CLS_TERMINAL, Selector::All, window1.clone()),
                &snap
            ),
            Some(Resolved::List(vec![
                Resolved::Terminal(0, 0, 0),
                Resolved::Terminal(0, 1, 0),
                Resolved::Terminal(0, 1, 1),
            ]))
        );
        let Some(Resolved::List(all)) = resolve(
            &el(codes::CLS_TERMINAL, Selector::All, Reference::App),
            &snap,
        ) else {
            panic!("app terminals");
        };
        assert_eq!(all.len(), 4);
        assert_eq!(
            element_count(fourcc(codes::CLS_WINDOW.as_bytes()), &Resolved::App, &snap),
            Some(2)
        );
        // Id form, last ordinal, negative index, `item` into a window.
        assert_eq!(
            resolve(
                &el(
                    codes::CLS_TAB,
                    Selector::Id(20),
                    el(codes::CLS_WINDOW, Selector::Id(2), Reference::App)
                ),
                &snap
            ),
            Some(Resolved::Tab(1, 0))
        );
        assert_eq!(
            resolve(
                &el(codes::CLS_WINDOW, Selector::Last, Reference::App),
                &snap
            ),
            Some(Resolved::Window(1))
        );
        assert_eq!(
            resolve(
                &el(codes::CLS_TAB, Selector::Index(-1), window1.clone()),
                &snap
            ),
            Some(Resolved::Tab(0, 1))
        );
        assert_eq!(
            resolve(&el(codes::CLS_ITEM, Selector::Index(2), window1), &snap),
            Some(Resolved::Tab(0, 1))
        );
        assert_eq!(
            resolve(
                &el(codes::CLS_WINDOW, Selector::Index(3), Reference::App),
                &snap
            ),
            None
        );
    }

    /// `pick`'s index arithmetic at its edges: 0 is never an element,
    /// 1 / -1 are the first / last, ±(count + 1) fall off either end, and
    /// ordinals on an empty container resolve to nothing.
    #[test]
    fn applescript_index_boundaries() {
        let snap = fixture();
        let window1 = el(codes::CLS_WINDOW, Selector::Index(1), Reference::App);
        let tab = |selector| resolve(&el(codes::CLS_TAB, selector, window1.clone()), &snap);
        assert_eq!(tab(Selector::Index(0)), None);
        assert_eq!(tab(Selector::Index(1)), Some(Resolved::Tab(0, 0)));
        assert_eq!(tab(Selector::Index(2)), Some(Resolved::Tab(0, 1)));
        assert_eq!(tab(Selector::Index(3)), None);
        assert_eq!(tab(Selector::Index(-1)), Some(Resolved::Tab(0, 1)));
        assert_eq!(tab(Selector::Index(-2)), Some(Resolved::Tab(0, 0)));
        assert_eq!(tab(Selector::Index(-3)), None);
        assert_eq!(tab(Selector::Index(i64::MIN)), None);
        assert_eq!(tab(Selector::Index(i64::MAX)), None);
        assert_eq!(tab(Selector::Id(999)), None);
        assert_eq!(tab(Selector::Id(-1)), None);

        // AppleScript's `middle item`: (count + 1) div 2.
        let middle = |n: usize| {
            pick(
                (0..n).map(Resolved::Window).collect(),
                &Selector::Middle,
                &snap,
            )
        };
        assert_eq!(middle(0), None);
        assert_eq!(middle(1), Some(Resolved::Window(0)));
        assert_eq!(middle(2), Some(Resolved::Window(0)));
        assert_eq!(middle(3), Some(Resolved::Window(1)));
        assert_eq!(middle(4), Some(Resolved::Window(1)));

        let empty = AppStateSnapshot::default();
        for selector in [
            Selector::First,
            Selector::Last,
            Selector::Index(1),
            Selector::Index(-1),
        ] {
            assert_eq!(
                resolve(&el(codes::CLS_WINDOW, selector, Reference::App), &empty),
                None
            );
        }
        // An element of the wrong class for its container never resolves.
        assert_eq!(
            resolve(
                &el(codes::CLS_TAB, Selector::Index(1), Reference::App),
                &snap
            ),
            None
        );
        assert_eq!(
            resolve(&el(codes::CLS_WINDOW, Selector::Index(1), window1), &snap),
            None
        );
    }

    /// `close tab N` / `close window N` take a raw 64-bit index from the
    /// Apple Event; every edge maps to a position or to `None` (-1728)
    /// without overflowing.
    #[test]
    fn one_based_index_rejects_out_of_range_without_overflow() {
        assert_eq!(one_based_index(i64::MIN, 3), None);
        assert_eq!(one_based_index(-1, 3), None);
        assert_eq!(one_based_index(0, 3), None);
        assert_eq!(one_based_index(1, 3), Some(0));
        assert_eq!(one_based_index(3, 3), Some(2));
        assert_eq!(one_based_index(4, 3), None);
        assert_eq!(one_based_index(i64::MAX, 3), None);
        assert_eq!(one_based_index(1, 0), None);
    }

    /// A spawn that succeeded and whose objects closed before the next
    /// snapshot rebuild must not linger as pending (the rebuild cannot prune
    /// what it no longer sees live), while a split promised inside the new
    /// tab stays pending until it runs.
    #[test]
    fn applescript_spawned_then_closed_objects_do_not_linger() {
        let mut pending = PendingRegistry::default();
        pending.add_window(7);
        pending.add_tab(70, 7);
        pending.add_terminal(700, 70);
        pending.add_terminal(701, 70);
        pending.retire(700, Some(70), Some(7));
        assert!(pending.windows.is_empty() && pending.tabs.is_empty());
        assert_eq!(pending.terminals, vec![(701, 70)]);
        // The window and pane 700 already closed: the rebuild shows neither,
        // and the orphaned split has no tab to attach to.
        let mut snap = fixture();
        merge_pending(&mut snap, &mut pending);
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(
            resolve(
                &el(codes::CLS_WINDOW, Selector::Id(7), Reference::App),
                &snap
            ),
            None
        );
        assert_eq!(
            element_count(fourcc(codes::CLS_WINDOW.as_bytes()), &Resolved::App, &snap),
            Some(2)
        );
    }

    /// A failed `new window` spawn forgets the window, its tab, and every
    /// terminal promised inside that tab (`script_spawn_tab`'s error path).
    #[test]
    fn applescript_failed_window_spawn_forgets_everything_it_promised() {
        let mut pending = PendingRegistry::default();
        pending.add_window(7);
        pending.add_tab(70, 7);
        pending.add_terminal(700, 70);
        // A split queued against the promised pane before the spawn ran.
        pending.add_terminal(701, 70);
        // Unrelated: a pending split in a live tab.
        pending.add_terminal(201, 20);
        pending.forget(700, Some(70), Some(7));
        assert_eq!(pending.terminals, vec![(201, 20)]);
        assert!(pending.tabs.is_empty() && pending.windows.is_empty());

        let mut snap = fixture();
        merge_pending(&mut snap, &mut pending);
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(
            resolve(
                &el(codes::CLS_WINDOW, Selector::Id(7), Reference::App),
                &snap
            ),
            None
        );
    }

    /// A split added to the first tab after a second tab exists must not
    /// renumber the second tab's terminal: a window's (and the
    /// application's) terminals are indexed in PaneId order overall.
    #[test]
    fn container_terminals_are_indexed_in_pane_id_order() {
        let mut snap = AppStateSnapshot {
            frontmost: true,
            version: "0".into(),
            windows: vec![WindowSnapshot {
                id: 1,
                name: String::new(),
                tabs: vec![
                    tab(10, true, vec![terminal(1, false), terminal(3, true)]),
                    tab(11, false, vec![terminal(2, true)]),
                ],
                pending: false,
            }],
            front_window: Some(0),
        };
        let window1 = el(codes::CLS_WINDOW, Selector::Index(1), Reference::App);
        let nth = |snap: &AppStateSnapshot, container: Reference, n: i64| {
            resolve(
                &el(codes::CLS_TERMINAL, Selector::Index(n), container),
                snap,
            )
            .and_then(|r| object_id(&r, snap))
        };
        assert_eq!(nth(&snap, window1.clone(), 1), Some(1));
        assert_eq!(nth(&snap, window1.clone(), 2), Some(2));
        assert_eq!(nth(&snap, window1.clone(), 3), Some(3));
        assert_eq!(nth(&snap, Reference::App, 2), Some(2));
        // Within one tab the order is unchanged.
        let tab1 = el(codes::CLS_TAB, Selector::Index(1), window1.clone());
        assert_eq!(nth(&snap, tab1, 2), Some(3));
        // A pending terminal sorts after every live one, whatever its id.
        let mut pending = PendingRegistry::default();
        pending.add_terminal(0, 11);
        merge_pending(&mut snap, &mut pending);
        assert_eq!(nth(&snap, window1.clone(), 4), Some(0));
        assert_eq!(nth(&snap, window1, 1), Some(1));
    }

    #[test]
    fn applescript_resolve_with_no_windows() {
        let snap = AppStateSnapshot::default();
        assert_eq!(
            resolve(&prop(codes::PROP_FRONT_WINDOW, Reference::App), &snap),
            None
        );
        assert_eq!(
            resolve(
                &el(codes::CLS_TERMINAL, Selector::All, Reference::App),
                &snap
            ),
            Some(Resolved::List(Vec::new()))
        );
        assert_eq!(
            element_count(fourcc(codes::CLS_WINDOW.as_bytes()), &Resolved::App, &snap),
            Some(0)
        );
        assert_eq!(focused_pane(&snap), None);
    }

    /// AC-030: pending entries survive rebuilds, leave when live, and resolve
    /// as their container's selection only inside a pending container.
    #[test]
    fn applescript_pending_survives_rebuild_and_retires_when_live() {
        let mut pending = PendingRegistry::default();
        // `new window` → window 7 / tab 70 / pane 700, and a split of the
        // live pane 200 → pane 201.
        pending.add_window(7);
        pending.add_tab(70, 7);
        pending.add_terminal(700, 70);
        pending.add_terminal(201, 20);

        let mut snap = fixture();
        merge_pending(&mut snap, &mut pending);
        // Idempotent.
        merge_pending(&mut snap, &mut pending);
        assert_eq!(snap.windows.len(), 3);
        assert_eq!(snap.windows[1].tabs[0].terminals.len(), 2);

        let w = el(codes::CLS_WINDOW, Selector::Id(7), Reference::App);
        let selected = prop(codes::PROP_SELECTED_TAB, w.clone());
        assert_eq!(resolve(&selected, &snap), Some(Resolved::Tab(2, 0)));
        assert_eq!(
            resolve(&prop(codes::PROP_FOCUSED_TERMINAL, selected), &snap),
            Some(Resolved::Terminal(2, 0, 0))
        );
        // The live tab keeps its live focus despite the pending split.
        let live_tab = el(
            codes::CLS_TAB,
            Selector::Id(20),
            el(codes::CLS_WINDOW, Selector::Id(2), Reference::App),
        );
        assert_eq!(
            resolve(&prop(codes::PROP_FOCUSED_TERMINAL, live_tab), &snap),
            Some(Resolved::Terminal(1, 0, 0))
        );
        // `front window` never moves to a pending window.
        assert_eq!(
            resolve(&prop(codes::PROP_FRONT_WINDOW, Reference::App), &snap),
            Some(Resolved::Window(0))
        );

        // A rebuild from live state (which knows nothing pending) re-merges.
        let mut rebuilt = fixture();
        merge_pending(&mut rebuilt, &mut pending);
        assert_eq!(rebuilt.windows.len(), 3);

        // The window comes alive: its pending entries retire.
        let mut live = fixture();
        live.windows.push(WindowSnapshot {
            id: 7,
            name: String::new(),
            tabs: vec![tab(70, true, vec![terminal(700, true)])],
            pending: false,
        });
        merge_pending(&mut live, &mut pending);
        assert!(!live.windows[2].pending);
        assert_eq!(pending.windows.len(), 0);
        assert_eq!(pending.tabs.len(), 0);
        assert_eq!(pending.terminals, vec![(201, 20)]);

        // The split fails: forgotten, and gone after the next rebuild.
        pending.forget(201, None, None);
        assert!(pending.is_empty());
        let mut after = fixture();
        merge_pending(&mut after, &mut pending);
        assert_eq!(
            resolve(
                &el(
                    codes::CLS_TERMINAL,
                    Selector::Id(201),
                    el(
                        codes::CLS_TAB,
                        Selector::Id(20),
                        el(codes::CLS_WINDOW, Selector::Id(2), Reference::App)
                    )
                ),
                &after
            ),
            None
        );
    }

    #[test]
    fn applescript_pending_tab_in_pending_window_selects_the_last() {
        let mut pending = PendingRegistry::default();
        pending.add_window(7);
        pending.add_tab(70, 7);
        pending.add_terminal(700, 70);
        pending.add_tab(71, 7);
        pending.add_terminal(710, 71);
        let mut snap = AppStateSnapshot::default();
        merge_pending(&mut snap, &mut pending);
        let w = el(codes::CLS_WINDOW, Selector::Id(7), Reference::App);
        assert_eq!(
            resolve(&prop(codes::PROP_SELECTED_TAB, w), &snap),
            Some(Resolved::Tab(0, 1))
        );
    }

    fn fed_terminal(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
        let mut terminal = Terminal::new(noa_core::GridSize::new(cols, rows));
        noa_vt::Stream::new().feed(bytes, &mut terminal);
        terminal
    }

    /// AC-017: `contents` is the viewport text, `selection` the selected text
    /// (empty without one), `scrollback` the capped tail of the whole history;
    /// `initial input` is capped like `input text`.
    #[test]
    fn applescript_text_reads() {
        let mut terminal = fed_terminal(20, 3, b"one\r\ntwo\r\nthree");
        assert_eq!(terminal_contents(&terminal), "one\ntwo\nthree\n");
        assert_eq!(
            terminal_contents(&terminal),
            crate::ipc_bridge::screen_text(&terminal.active().visible_rows())
        );
        assert_eq!(terminal_selection(&terminal), "");
        terminal.set_viewport_selection(
            noa_core::Point { x: 0, y: 1 },
            noa_core::Point { x: 2, y: 1 },
        );
        assert_eq!(terminal_selection(&terminal), "two");

        // 300 KiB of history: the read keeps the newest ≤ 256 KiB.
        let line = "x".repeat(99) + "\r\n";
        let mut history = line.repeat(300 * 1024 / 101);
        history.push_str("TAIL-MARK");
        let mut big = fed_terminal(100, 24, history.as_bytes());
        let text = terminal_scrollback(&mut big);
        assert!(text.len() <= noa_ipc::protocol::DEFAULT_TEXT_MAX_BYTES);
        assert!(text.len() > 200 * 1024);
        assert!(
            text.trim_end().ends_with("TAIL-MARK"),
            "{:?}",
            &text[text.len() - 40..]
        );

        let input = "y".repeat(9 * 1024 * 1024);
        let bytes = crate::input::raw_input_bytes(&input).expect("bytes");
        assert_eq!(bytes.len(), noa_pty::WRITE_BYTE_CAP);
    }

    fn percentile(samples: &mut [std::time::Duration], p: f64) -> std::time::Duration {
        samples.sort_unstable();
        let idx = ((samples.len() as f64 * p).ceil() as usize).saturating_sub(1);
        samples[idx.min(samples.len() - 1)]
    }

    fn time_reads(
        handle: &TerminalHandle,
        iterations: usize,
        read: impl Fn(&mut Terminal) -> String,
    ) -> Vec<std::time::Duration> {
        (0..iterations)
            .map(|_| {
                let start = std::time::Instant::now();
                let text = read(&mut handle.lock());
                std::hint::black_box(text);
                start.elapsed()
            })
            .collect()
    }

    /// AC-022 / CFR-001: main-thread cost of an AE/AX text read, lock
    /// included. Run with
    /// `cargo test -p noa-app --release applescript_read_perf -- --ignored --nocapture`.
    #[test]
    #[ignore = "release-mode measurement (agent-automation AC-022)"]
    fn applescript_read_perf() {
        use std::time::Duration;

        // The reads run on the main thread, which AppKit runs at
        // user-interactive QoS; measure at the same QoS so the scheduler
        // doesn't park the measurement on an efficiency core.
        #[cfg(target_os = "macos")]
        // SAFETY: adjusts only the calling thread's QoS class.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
        }

        // (a) 200×60 of full-width cells, uncontended.
        let row = "あ".repeat(100);
        let screen: String = (0..60).map(|_| format!("{row}\r\n")).collect();
        let handle: TerminalHandle = Arc::new(Mutex::new(fed_terminal(200, 60, screen.as_bytes())));
        let mut a = time_reads(&handle, 1000, |t| terminal_contents(t));
        let a95 = percentile(&mut a, 0.95);

        // (b) the same read while another thread feeds 10 MB in 64 KiB chunks
        // (the io thread's lock pattern).
        let feeder = {
            let handle = handle.clone();
            std::thread::spawn(move || {
                let chunk = "0123456789abcdef".repeat(4096);
                let mut stream = noa_vt::Stream::new();
                for _ in 0..(10 * 1024 * 1024 / chunk.len()) {
                    stream.feed(chunk.as_bytes(), &mut *handle.lock());
                }
            })
        };
        let mut b = time_reads(&handle, 1000, |t| terminal_contents(t));
        feeder.join().expect("feeder");
        let b99 = percentile(&mut b, 0.99);

        // (c) a 256 KiB scrollback read, uncontended.
        let history = ("y".repeat(99) + "\r\n").repeat(4 * 1024);
        let big: TerminalHandle = Arc::new(Mutex::new(fed_terminal(100, 24, history.as_bytes())));
        let mut c = time_reads(&big, 1000, terminal_scrollback);
        let c95 = percentile(&mut c, 0.95);

        eprintln!("AC-022 contents p95 {a95:?} | contended p99 {b99:?} | scrollback p95 {c95:?}");
        assert!(a95 <= Duration::from_millis(1), "(a) p95 {a95:?} > 1 ms");
        assert!(
            b99 <= Duration::from_micros(8_300),
            "(b) p99 {b99:?} > 8.3 ms"
        );
        assert!(
            c95 <= Duration::from_micros(4_200),
            "(c) p95 {c95:?} > 4.2 ms"
        );
    }

    #[test]
    fn surface_configuration_env_parsing() {
        assert_eq!(
            parse_env_assignments(&["FOO=bar".into(), "A=b=c".into(), "E=".into()]),
            Some(vec![
                ("FOO".into(), "bar".into()),
                ("A".into(), "b=c".into()),
                ("E".into(), String::new()),
            ])
        );
        assert_eq!(parse_env_assignments(&["NOEQ".into()]), None);
        assert_eq!(parse_env_assignments(&["=v".into()]), None);
    }
}
