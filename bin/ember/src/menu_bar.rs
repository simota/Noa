//! The AppKit side: one `NSStatusItem` with Ember's ghost, and a menu that is
//! rebuilt from the latest [`Snapshot`] only as it opens. While it is open
//! nothing touches it, so the row you are reaching for never moves.

use std::cell::{Cell, RefCell};

use crossbeam_channel::Sender;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{NSData, NSRange, NSSize, NSString};

use crate::icon;
use crate::link::{CONTROL_REASON, Command, Link, Shared, Snapshot};
use crate::model::{PaneRow, PaneState};

const NOA_BUNDLE_ID: &str = "com.simota.noa";
/// Right-aligned tab stop for the status word; sets the menu's width.
const STATUS_TAB: f64 = 320.0;
/// Brand accent, reserved for `waiting.`.
const ACCENT: (f64, f64, f64) = (
    0xE8 as f64 / 255.0,
    0xA3 as f64 / 255.0,
    0x3D as f64 / 255.0,
);
/// macOS `NSTextAlignmentRight` (1 on macOS, unlike iOS).
const TEXT_ALIGN_RIGHT: isize = 1;
const ACTIVATION_POLICY_ACCESSORY: isize = 1;
const VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;
const IMAGE_LEFT: isize = 2;
const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;

struct Ivars {
    shared: Shared,
    commands: Sender<Command>,
    status_item: Retained<AnyObject>,
    menu_open: Cell<bool>,
    paused: Cell<bool>,
    badge: Cell<usize>,
    /// Server instance of the rows currently in the menu.
    menu_instance: RefCell<String>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Ember creates and
    // uses this delegate and every AppKit object only on the main thread.
    #[unsafe(super(NSObject))]
    #[name = "EmberMenuDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &AnyObject) {
            let snapshot = self.ivars().shared.lock().clone();
            self.rebuild(menu, &snapshot);
        }

        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &AnyObject) {
            self.ivars().menu_open.set(true);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &AnyObject) {
            self.ivars().menu_open.set(false);
            self.refresh_badge();
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &AnyObject) {
            if !self.ivars().menu_open.get() {
                self.refresh_badge();
            }
        }

        #[unsafe(method(jump:))]
        fn jump(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            activate_noa();
            let instance = self.ivars().menu_instance.borrow().clone();
            let _ = self.ivars().commands.send(Command::Focus { pane: tag as u64, instance });
        }

        #[unsafe(method(openNoa:))]
        fn open_noa(&self, _sender: &AnyObject) {
            open_noa();
        }

        #[unsafe(method(togglePause:))]
        fn toggle_pause(&self, _sender: &AnyObject) {
            let paused = !self.ivars().paused.get();
            self.ivars().paused.set(paused);
            let _ = self.ivars().commands.send(Command::Pause(paused));
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            unsafe {
                let app: *mut AnyObject = msg_send![class("NSApplication"), sharedApplication];
                let _: () = msg_send![app, terminate: std::ptr::null::<AnyObject>()];
            }
        }
    }
);

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSAppKitVersionNumber: f64;
}

fn class(name: &str) -> &'static AnyClass {
    // Reading an AppKit symbol keeps the framework linked; Ember otherwise
    // reaches AppKit only through the runtime, which the linker can't see.
    let _ = unsafe { NSAppKitVersionNumber };
    let name = std::ffi::CString::new(name).expect("class name");
    AnyClass::get(&name).unwrap_or_else(|| panic!("AppKit class {name:?} is unavailable"))
}

fn alloc(name: &str) -> Allocated<AnyObject> {
    unsafe { msg_send![class(name), alloc] }
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl Delegate {
    fn new(
        shared: Shared,
        commands: Sender<Command>,
        status_item: Retained<AnyObject>,
    ) -> Retained<Self> {
        let allocated = Delegate::alloc().set_ivars(Ivars {
            shared,
            commands,
            status_item,
            menu_open: Cell::new(false),
            paused: Cell::new(false),
            badge: Cell::new(0),
            menu_instance: RefCell::new(String::new()),
        });
        unsafe { msg_send![super(allocated), init] }
    }

    /// The waiting count beside the ghost; nothing when nobody is waiting.
    fn refresh_badge(&self) {
        let waiting = self
            .ivars()
            .shared
            .lock()
            .rows
            .iter()
            .filter(|row| row.state == PaneState::Waiting)
            .count();
        if waiting == self.ivars().badge.get() {
            return;
        }
        self.ivars().badge.set(waiting);
        let title = if waiting == 0 {
            String::new()
        } else {
            format!(" {waiting}")
        };
        unsafe {
            let button: *mut AnyObject = msg_send![&*self.ivars().status_item, button];
            let _: () = msg_send![button, setTitle: &*ns(&title)];
        }
    }

    fn rebuild(&self, menu: &AnyObject, snapshot: &Snapshot) {
        self.ivars()
            .menu_instance
            .replace(snapshot.instance.clone());
        unsafe {
            let _: () = msg_send![menu, removeAllItems];
        }
        let (header, hint) = header_lines(snapshot);
        add_note(menu, &header, None);
        if let Some(hint) = hint {
            add_note(menu, hint, None);
        }

        let control = matches!(snapshot.link, Link::Live { control: true });
        for row in &snapshot.rows {
            self.add_row(menu, row, control);
        }
        if matches!(snapshot.link, Link::Live { control: false }) {
            add_separator(menu);
            add_note(
                menu,
                CONTROL_REASON,
                Some("Add control to server-scopes in ~/.config/noa/config to jump."),
            );
        }

        add_separator(menu);
        self.add_action(menu, "Open noa", sel!(openNoa:), "");
        let pause = if self.ivars().paused.get() {
            "Resume Ember"
        } else {
            "Pause Ember"
        };
        self.add_action(menu, pause, sel!(togglePause:), "");
        self.add_action(menu, "Quit Ember", sel!(quit:), "q");
    }

    fn add_row(&self, menu: &AnyObject, row: &PaneRow, enabled: bool) {
        let item = self.add_action(menu, "", sel!(jump:), "");
        let title = row_title(row);
        unsafe {
            let _: () = msg_send![&*item, setAttributedTitle: &*title];
            let _: () = msg_send![&*item, setTag: row.pane_id as isize];
            let _: () = msg_send![&*item, setEnabled: enabled];
        }
    }

    fn add_action(
        &self,
        menu: &AnyObject,
        title: &str,
        action: Sel,
        key: &str,
    ) -> Retained<AnyObject> {
        unsafe {
            let item: Retained<AnyObject> = msg_send![
                alloc("NSMenuItem"),
                initWithTitle: &*ns(title),
                action: action,
                keyEquivalent: &*ns(key)
            ];
            let _: () = msg_send![&*item, setTarget: self];
            let _: () = msg_send![&*item, setEnabled: true];
            let _: () = msg_send![menu, addItem: &*item];
            item
        }
    }
}

fn header_lines(snapshot: &Snapshot) -> (String, Option<&'static str>) {
    match &snapshot.link {
        Link::Live { .. } => {
            let count = snapshot.rows.len();
            let noun = if count == 1 { "pane" } else { "panes" };
            (format!("noa · {count} {noun}"), None)
        }
        Link::Connecting => ("noa · connecting…".to_string(), None),
        Link::ServerOff => (
            "noa · server is off".to_string(),
            Some("Set server-enable = true in ~/.config/noa/config."),
        ),
        Link::NoToken => (
            "noa · no server token yet".to_string(),
            Some("Start noa once with server-enable = true."),
        ),
        Link::Unreachable(reason) => (format!("noa · {reason}"), None),
        Link::Paused => ("ember is paused".to_string(), None),
    }
}

fn add_note(menu: &AnyObject, text: &str, tooltip: Option<&str>) {
    unsafe {
        let item: Retained<AnyObject> = msg_send![
            alloc("NSMenuItem"),
            initWithTitle: &*ns(text),
            action: Option::<Sel>::None,
            keyEquivalent: &*ns("")
        ];
        let _: () = msg_send![&*item, setEnabled: false];
        if let Some(tooltip) = tooltip {
            let _: () = msg_send![&*item, setToolTip: &*ns(tooltip)];
        }
        let _: () = msg_send![menu, addItem: &*item];
    }
}

fn add_separator(menu: &AnyObject) {
    unsafe {
        let item: *mut AnyObject = msg_send![class("NSMenuItem"), separatorItem];
        let _: () = msg_send![menu, addItem: item];
    }
}

/// Two lines: `title⇥state` over a smaller, dimmer `detail`.
fn row_title(row: &PaneRow) -> Retained<AnyObject> {
    let first = format!("{}\t{}", row.title, row.state.word());
    let text = format!("{first}\n{}", row.detail);
    let word_start = utf16_len(&row.title) + 1;
    let word_len = utf16_len(row.state.word());
    let detail_start = utf16_len(&first) + 1;
    let detail_len = utf16_len(&row.detail);
    let all = NSRange::new(0, utf16_len(&text));

    unsafe {
        let string: Retained<AnyObject> = msg_send![
            alloc("NSMutableAttributedString"),
            initWithString: &*ns(&text)
        ];
        let set = |key: &str, value: &AnyObject, range: NSRange| {
            let _: () = msg_send![&*string, addAttribute: &*ns(key), value: value, range: range];
        };

        let no_options: Retained<AnyObject> = msg_send![class("NSDictionary"), dictionary];
        let tab: Retained<AnyObject> = msg_send![
            alloc("NSTextTab"),
            initWithTextAlignment: TEXT_ALIGN_RIGHT,
            location: STATUS_TAB,
            options: &*no_options
        ];
        let tabs: *mut AnyObject = msg_send![class("NSArray"), arrayWithObject: &*tab];
        let paragraph: Retained<AnyObject> = msg_send![class("NSMutableParagraphStyle"), new];
        let _: () = msg_send![&*paragraph, setTabStops: tabs];
        let _: () = msg_send![&*paragraph, setParagraphSpacing: 1.0f64];

        let menu_font: *mut AnyObject = msg_send![class("NSFont"), menuFontOfSize: 0.0f64];
        let small_font: *mut AnyObject = msg_send![class("NSFont"), menuFontOfSize: 11.0f64];
        let dim: *mut AnyObject = msg_send![class("NSColor"), secondaryLabelColor];

        set("NSParagraphStyle", &paragraph, all);
        set("NSFont", &*menu_font, all);
        set(
            "NSFont",
            &*small_font,
            NSRange::new(detail_start, detail_len),
        );
        set("NSColor", &*dim, NSRange::new(detail_start, detail_len));
        if row.state == PaneState::Waiting {
            let (r, g, b) = ACCENT;
            let accent: *mut AnyObject = msg_send![
                class("NSColor"),
                colorWithSRGBRed: r,
                green: g,
                blue: b,
                alpha: 1.0f64
            ];
            set("NSColor", &*accent, NSRange::new(word_start, word_len));
        }
        string
    }
}

fn running_noa() -> Option<Retained<AnyObject>> {
    unsafe {
        let apps: *mut AnyObject = msg_send![
            class("NSRunningApplication"),
            runningApplicationsWithBundleIdentifier: &*ns(NOA_BUNDLE_ID)
        ];
        let first: *mut AnyObject = msg_send![apps, firstObject];
        Retained::retain(first)
    }
}

/// Hands activation to noa as the menu closes; the server's `focusPane` then
/// picks the window and pane.
fn activate_noa() {
    if let Some(app) = running_noa() {
        unsafe {
            let _: bool = msg_send![&*app, activateWithOptions: ACTIVATE_IGNORING_OTHER_APPS];
        }
    }
}

fn open_noa() {
    if running_noa().is_some() {
        activate_noa();
        return;
    }
    unsafe {
        let workspace: *mut AnyObject = msg_send![class("NSWorkspace"), sharedWorkspace];
        let url: *mut AnyObject =
            msg_send![workspace, URLForApplicationWithBundleIdentifier: &*ns(NOA_BUNDLE_ID)];
        if url.is_null() {
            log::warn!("noa ({NOA_BUNDLE_ID}) is not installed");
            return;
        }
        let _: bool = msg_send![workspace, openURL: url];
    }
}

pub fn run(shared: Shared, commands: Sender<Command>) {
    unsafe {
        let app: *mut AnyObject = msg_send![class("NSApplication"), sharedApplication];
        let _: bool = msg_send![app, setActivationPolicy: ACTIVATION_POLICY_ACCESSORY];

        let bar: *mut AnyObject = msg_send![class("NSStatusBar"), systemStatusBar];
        let item_ptr: *mut AnyObject =
            msg_send![bar, statusItemWithLength: VARIABLE_STATUS_ITEM_LENGTH];
        let status_item = Retained::retain(item_ptr).expect("status item");

        let png = icon::png();
        let data = NSData::with_bytes(&png);
        let image: Retained<AnyObject> = msg_send![alloc("NSImage"), initWithData: &*data];
        let _: () = msg_send![&*image, setSize: NSSize::new(18.0, 18.0)];
        let _: () = msg_send![&*image, setTemplate: true];
        let button: *mut AnyObject = msg_send![&*status_item, button];
        let _: () = msg_send![button, setImage: &*image];
        let _: () = msg_send![button, setImagePosition: IMAGE_LEFT];
        let _: () = msg_send![button, setToolTip: &*ns("Ember · noa panes")];

        let menu: Retained<AnyObject> = msg_send![alloc("NSMenu"), initWithTitle: &*ns("Ember")];
        let _: () = msg_send![&*menu, setAutoenablesItems: false];

        let delegate = Delegate::new(shared, commands, status_item.clone());
        let _: () = msg_send![&*menu, setDelegate: &*delegate];
        let _: () = msg_send![&*status_item, setMenu: &*menu];

        let _: *mut AnyObject = msg_send![
            class("NSTimer"),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: &*delegate,
            selector: sel!(tick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ];

        let _: () = msg_send![app, run];
        // `run` only returns on terminate; keep the AppKit objects alive until then.
        drop((delegate, menu, image));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaneRow;

    fn row(id: u64, state: PaneState) -> PaneRow {
        PaneRow {
            pane_id: id,
            title: "api-server · claude".to_string(),
            detail: "feat/rate-limit · ~/src/api".to_string(),
            state,
        }
    }

    fn titles(menu: &AnyObject) -> Vec<(String, bool, isize)> {
        unsafe {
            let count: isize = msg_send![menu, numberOfItems];
            (0..count)
                .map(|index| {
                    let item: *mut AnyObject = msg_send![menu, itemAtIndex: index];
                    let title: Retained<NSString> = msg_send![item, title];
                    let enabled: bool = msg_send![item, isEnabled];
                    let tag: isize = msg_send![item, tag];
                    (title.to_string(), enabled, tag)
                })
                .collect()
        }
    }

    /// Exercises every AppKit message the menu sends (debug objc2 verifies
    /// each selector's argument/return encoding at runtime).
    fn build(link: Link, rows: Vec<PaneRow>) -> Vec<(String, bool, isize)> {
        let (commands, _rx) = crossbeam_channel::unbounded();
        let shared: Shared = std::sync::Arc::new(parking_lot::Mutex::new(Snapshot {
            link,
            rows,
            instance: String::new(),
        }));
        unsafe {
            let menu: Retained<AnyObject> =
                msg_send![alloc("NSMenu"), initWithTitle: &*ns("Ember")];
            let _: () = msg_send![&*menu, setAutoenablesItems: false];
            let placeholder: Retained<AnyObject> = msg_send![class("NSObject"), new];
            let delegate = Delegate::new(shared.clone(), commands, placeholder);
            let snapshot = shared.lock().clone();
            delegate.rebuild(&menu, &snapshot);
            titles(&menu)
        }
    }

    #[test]
    fn live_menu_lists_rows_in_order_with_tags_and_footer() {
        let items = build(
            Link::Live { control: true },
            vec![row(7, PaneState::Waiting), row(3, PaneState::Running)],
        );
        assert_eq!(items[0], ("noa · 2 panes".to_string(), false, 0));
        assert_eq!(
            items[1].0,
            "api-server · claude\twaiting.\nfeat/rate-limit · ~/src/api"
        );
        assert_eq!((items[1].1, items[1].2), (true, 7));
        assert_eq!((items[2].1, items[2].2), (true, 3));
        let tail: Vec<&str> = items[items.len() - 3..]
            .iter()
            .map(|i| i.0.as_str())
            .collect();
        assert_eq!(tail, ["Open noa", "Pause Ember", "Quit Ember"]);
    }

    #[test]
    fn without_control_rows_are_disabled_and_the_reason_is_one_line() {
        let items = build(Link::Live { control: false }, vec![row(1, PaneState::Idle)]);
        assert!(!items[1].1, "row must not be clickable without control");
        let reasons = items.iter().filter(|i| i.0 == CONTROL_REASON).count();
        assert_eq!(reasons, 1);
    }

    #[test]
    fn server_off_explains_how_to_pair() {
        let items = build(Link::ServerOff, Vec::new());
        assert_eq!(items[0].0, "noa · server is off");
        assert_eq!(
            items[1].0,
            "Set server-enable = true in ~/.config/noa/config."
        );
    }

    #[test]
    fn status_icon_decodes_as_an_nsimage() {
        unsafe {
            let data = NSData::with_bytes(&icon::png());
            let image: Option<Retained<AnyObject>> =
                msg_send![alloc("NSImage"), initWithData: &*data];
            assert!(image.is_some());
        }
    }
}
