//! The AppKit side: one `NSStatusItem` with Ember's ghost, and a menu that is
//! rebuilt from the latest rows only as it opens. While it is open nothing
//! touches it, so the row you are reaching for never moves.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{NSData, NSRange, NSSize, NSString};
use winit::event_loop::EventLoopProxy;

use super::icon;
use super::model::{PaneRow, PaneState};
use crate::UserEvent;
use crate::commands::AppCommand;
use crate::session_store::SessionCardId;

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
const VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;
const IMAGE_LEFT: isize = 2;

/// Posts to the app's event loop (an `EventLoopProxy` outside tests).
type Post = Box<dyn Fn(UserEvent)>;

struct Ivars {
    send: Post,
    status_item: Retained<AnyObject>,
    menu_open: Cell<bool>,
    badge: Cell<usize>,
    rows: RefCell<Vec<PaneRow>>,
    /// The pane behind each row of the menu as it was last built; a row's
    /// tag indexes this.
    menu_panes: RefCell<Vec<SessionCardId>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The delegate and
    // every AppKit object it touches live on the main thread, where winit
    // runs the app.
    #[unsafe(super(NSObject))]
    #[name = "NoaEmberMenuDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &AnyObject) {
            self.rebuild(menu);
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

        #[unsafe(method(jump:))]
        fn jump(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let pane = usize::try_from(tag)
                .ok()
                .and_then(|index| self.ivars().menu_panes.borrow().get(index).copied());
            if let Some(pane) = pane {
                (self.ivars().send)(UserEvent::EmberJump(pane));
            }
        }

        #[unsafe(method(hide:))]
        fn hide(&self, _sender: &AnyObject) {
            (self.ivars().send)(UserEvent::AppCommand(AppCommand::ToggleEmber));
        }
    }
);

fn class(name: &std::ffi::CStr) -> &'static AnyClass {
    AnyClass::get(name).unwrap_or_else(|| panic!("AppKit class {name:?} is unavailable"))
}

fn alloc(name: &std::ffi::CStr) -> Allocated<AnyObject> {
    unsafe { msg_send![class(name), alloc] }
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl Delegate {
    fn new(send: Post, status_item: Retained<AnyObject>) -> Retained<Self> {
        let allocated = Delegate::alloc().set_ivars(Ivars {
            send,
            status_item,
            menu_open: Cell::new(false),
            badge: Cell::new(0),
            rows: RefCell::new(Vec::new()),
            menu_panes: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(allocated), init] }
    }

    /// The waiting count beside the ghost; nothing when nobody is waiting.
    fn refresh_badge(&self) {
        let waiting = self
            .ivars()
            .rows
            .borrow()
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

    fn rebuild(&self, menu: &AnyObject) {
        let rows = self.ivars().rows.borrow();
        self.ivars()
            .menu_panes
            .replace(rows.iter().map(|row| row.id).collect());
        unsafe {
            let _: () = msg_send![menu, removeAllItems];
        }
        add_note(menu, &header(rows.len()));
        for (index, row) in rows.iter().enumerate() {
            let item = self.add_action(menu, "", sel!(jump:), "");
            unsafe {
                let _: () = msg_send![&*item, setAttributedTitle: &*row_title(row)];
                let _: () = msg_send![&*item, setTag: index as isize];
            }
        }
        add_separator(menu);
        self.add_action(menu, "Hide Ember", sel!(hide:), "");
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
                alloc(c"NSMenuItem"),
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

fn header(count: usize) -> String {
    let noun = if count == 1 { "pane" } else { "panes" };
    format!("noa · {count} {noun}")
}

fn add_note(menu: &AnyObject, text: &str) {
    unsafe {
        let item: Retained<AnyObject> = msg_send![
            alloc(c"NSMenuItem"),
            initWithTitle: &*ns(text),
            action: Option::<Sel>::None,
            keyEquivalent: &*ns("")
        ];
        let _: () = msg_send![&*item, setEnabled: false];
        let _: () = msg_send![menu, addItem: &*item];
    }
}

fn add_separator(menu: &AnyObject) {
    unsafe {
        let item: *mut AnyObject = msg_send![class(c"NSMenuItem"), separatorItem];
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
            alloc(c"NSMutableAttributedString"),
            initWithString: &*ns(&text)
        ];
        let set = |key: &str, value: &AnyObject, range: NSRange| {
            let _: () = msg_send![&*string, addAttribute: &*ns(key), value: value, range: range];
        };

        let no_options: Retained<AnyObject> = msg_send![class(c"NSDictionary"), dictionary];
        let tab: Retained<AnyObject> = msg_send![
            alloc(c"NSTextTab"),
            initWithTextAlignment: TEXT_ALIGN_RIGHT,
            location: STATUS_TAB,
            options: &*no_options
        ];
        let tabs: *mut AnyObject = msg_send![class(c"NSArray"), arrayWithObject: &*tab];
        let paragraph: Retained<AnyObject> = msg_send![class(c"NSMutableParagraphStyle"), new];
        let _: () = msg_send![&*paragraph, setTabStops: tabs];
        let _: () = msg_send![&*paragraph, setParagraphSpacing: 1.0f64];

        let menu_font: *mut AnyObject = msg_send![class(c"NSFont"), menuFontOfSize: 0.0f64];
        let small_font: *mut AnyObject = msg_send![class(c"NSFont"), menuFontOfSize: 11.0f64];
        let dim: *mut AnyObject = msg_send![class(c"NSColor"), secondaryLabelColor];

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
                class(c"NSColor"),
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

/// Ember's status item. Dropping it takes the ghost out of the menu bar.
pub(crate) struct EmberStatusItem {
    status_item: Retained<AnyObject>,
    delegate: Retained<Delegate>,
    // The status item holds its menu weakly enough that we keep our own
    // reference for the item's lifetime.
    _menu: Retained<AnyObject>,
}

impl EmberStatusItem {
    pub(crate) fn install(proxy: EventLoopProxy<UserEvent>) -> Self {
        unsafe {
            let bar: *mut AnyObject = msg_send![class(c"NSStatusBar"), systemStatusBar];
            let item_ptr: *mut AnyObject =
                msg_send![bar, statusItemWithLength: VARIABLE_STATUS_ITEM_LENGTH];
            let status_item = Retained::retain(item_ptr).expect("status item");

            let png = icon::png();
            let data = NSData::with_bytes(&png);
            let image: Retained<AnyObject> = msg_send![alloc(c"NSImage"), initWithData: &*data];
            let _: () = msg_send![&*image, setSize: NSSize::new(18.0, 18.0)];
            let _: () = msg_send![&*image, setTemplate: true];
            let button: *mut AnyObject = msg_send![&*status_item, button];
            let _: () = msg_send![button, setImage: &*image];
            let _: () = msg_send![button, setImagePosition: IMAGE_LEFT];
            let _: () = msg_send![button, setToolTip: &*ns("Ember · noa panes")];

            let menu: Retained<AnyObject> =
                msg_send![alloc(c"NSMenu"), initWithTitle: &*ns("Ember")];
            let _: () = msg_send![&*menu, setAutoenablesItems: false];

            let send: Post = Box::new(move |event| {
                let _ = proxy.send_event(event);
            });
            let delegate = Delegate::new(send, status_item.clone());
            let _: () = msg_send![&*menu, setDelegate: &*delegate];
            let _: () = msg_send![&*status_item, setMenu: &*menu];

            Self {
                status_item,
                delegate,
                _menu: menu,
            }
        }
    }

    /// Replace the rows the next menu opening shows. The open menu itself is
    /// never touched; only the badge follows, and only while it is closed.
    pub(crate) fn set_rows(&self, rows: Vec<PaneRow>) {
        self.delegate.ivars().rows.replace(rows);
        if !self.delegate.ivars().menu_open.get() {
            self.delegate.refresh_badge();
        }
    }
}

impl Drop for EmberStatusItem {
    fn drop(&mut self) {
        unsafe {
            let bar: *mut AnyObject = msg_send![class(c"NSStatusBar"), systemStatusBar];
            let _: () = msg_send![bar, removeStatusItem: &*self.status_item];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::SessionWindowId;
    use crate::split_tree::PaneId;

    fn row(id: u64, state: PaneState) -> PaneRow {
        PaneRow {
            id: SessionCardId::new(SessionWindowId(1), PaneId::new(id)),
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
    #[test]
    fn menu_lists_rows_in_order_with_index_tags_and_footer() {
        unsafe {
            let menu: Retained<AnyObject> =
                msg_send![alloc(c"NSMenu"), initWithTitle: &*ns("Ember")];
            let _: () = msg_send![&*menu, setAutoenablesItems: false];
            let placeholder: Retained<AnyObject> = msg_send![class(c"NSObject"), new];
            let delegate = Delegate::new(Box::new(|_| {}), placeholder);
            delegate
                .ivars()
                .rows
                .replace(vec![row(7, PaneState::Waiting), row(3, PaneState::Running)]);
            delegate.rebuild(&menu);
            let items = titles(&menu);

            assert_eq!(items[0], ("noa · 2 panes".to_string(), false, 0));
            assert_eq!(
                items[1].0,
                "api-server · claude\twaiting.\nfeat/rate-limit · ~/src/api"
            );
            assert_eq!((items[1].1, items[1].2), (true, 0));
            assert_eq!((items[2].1, items[2].2), (true, 1));
            assert_eq!(items.last().unwrap().0, "Hide Ember");
            let panes: Vec<u64> = delegate
                .ivars()
                .menu_panes
                .borrow()
                .iter()
                .map(|id| id.pane_id.get())
                .collect();
            assert_eq!(panes, vec![7, 3]);
        }
    }

    #[test]
    fn status_icon_decodes_as_an_nsimage() {
        unsafe {
            let data = NSData::with_bytes(&icon::png());
            let image: Option<Retained<AnyObject>> =
                msg_send![alloc(c"NSImage"), initWithData: &*data];
            assert!(image.is_some());
        }
    }
}
