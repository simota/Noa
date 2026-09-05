//! Modeless native text editing/reading, independent of the live terminal grid.

pub(crate) const TEXT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextPanelMode {
    Compose,
    Output,
    File,
}

pub(crate) fn bounded_text(text: &str) -> String {
    let mut end = text.len().min(TEXT_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Ranges use UTF-16 coordinates, as required by NSTextView.
pub(crate) fn code_blocks(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut blocks = Vec::new();
    let mut fence: Option<(char, usize, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let len = line.encode_utf16().count();
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() <= 3
            && let Some(mark @ ('`' | '~')) = trimmed.chars().next()
        {
            let width = trimmed.chars().take_while(|c| *c == mark).count();
            if width >= 3 {
                if let Some((open_mark, open_width, begin)) = fence {
                    if mark == open_mark
                        && width >= open_width
                        && trimmed[width..].trim().is_empty()
                    {
                        blocks.push(begin..offset);
                        fence = None;
                    }
                } else {
                    fence = Some((mark, width, offset + len));
                }
            }
        }
        offset += len;
    }
    if let Some((_, _, begin)) = fence {
        blocks.push(begin..offset);
    }
    blocks
}

#[cfg(target_os = "macos")]
pub(crate) use native::TextPanel;

#[cfg(target_os = "macos")]
mod native {
    use std::cell::Cell;
    use std::io;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
    use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};
    use winit::event_loop::EventLoopProxy;
    use winit::window::WindowId;

    use crate::{AppCommand, UserEvent, split_tree::PaneId};

    struct Ivars {
        proxy: EventLoopProxy<UserEvent>,
        window_id: WindowId,
        pane_id: PaneId,
        process: Option<String>,
        text_view: Retained<AnyObject>,
        editable: bool,
        mode: super::TextPanelMode,
        blocks: Vec<std::ops::Range<usize>>,
        block_index: Cell<usize>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements. App owns this
        // delegate and accesses all AppKit objects exclusively on the main thread.
        #[unsafe(super(NSObject))]
        #[name = "NoaTextPanelDelegate"]
        #[ivars = Ivars]
        struct Delegate;

        impl Delegate {
            #[unsafe(method(pasteToPane:))]
            fn paste_to_pane(&self, _sender: &AnyObject) {
                let marked: bool = unsafe { msg_send![&*self.ivars().text_view, hasMarkedText] };
                if marked { return; }
                self.send(true);
            }

            #[unsafe(method(returnToPane:))]
            fn return_to_pane(&self, _sender: &AnyObject) {
                let ivars = self.ivars();
                let _ = ivars.proxy.send_event(UserEvent::TextPanelReturn { window_id: ivars.window_id, pane_id: ivars.pane_id });
            }

            #[unsafe(method(windowWillClose:))]
            fn window_will_close(&self, _notification: &AnyObject) {
                self.send(false);
            }

            #[unsafe(method(windowShouldClose:))]
            fn window_should_close(&self, _sender: &AnyObject) -> bool {
                self.check_length()
            }

            #[unsafe(method(copyCode:))]
            fn copy_code(&self, _sender: &AnyObject) {
                let ivars = self.ivars();
                if ivars.blocks.is_empty() { return; }
                let index = ivars.block_index.get() % ivars.blocks.len();
                let block = &ivars.blocks[index];
                let range = NSRange::new(block.start, block.end - block.start);
                // SAFETY: ranges were computed from this immutable text in UTF-16.
                unsafe {
                    let _: () = msg_send![&*ivars.text_view, setSelectedRange: range];
                    let _: () = msg_send![&*ivars.text_view, scrollRangeToVisible: range];
                    let _: () = msg_send![&*ivars.text_view, copy: std::ptr::null::<AnyObject>()];
                }
                ivars.block_index.set(index + 1);
            }
        }
    );

    impl Delegate {
        fn text(&self) -> String {
            // SAFETY: the retained NSTextView owns the NSString returned by string.
            let value: Retained<NSString> = unsafe { msg_send![&*self.ivars().text_view, string] };
            value.to_string()
        }

        fn check_length(&self) -> bool {
            if self.ivars().editable && self.text().len() > super::TEXT_LIMIT {
                unsafe {
                    let window: *mut AnyObject = msg_send![&*self.ivars().text_view, window];
                    let _: () = msg_send![window, setTitle: &*NSString::from_str("Prompt exceeds 1 MiB — shorten it before pasting or closing")];
                }
                return false;
            }
            true
        }

        fn send(&self, paste: bool) {
            let ivars = self.ivars();
            if !ivars.editable {
                return;
            }
            if !self.check_length() {
                return;
            }
            let _ = ivars.proxy.send_event(UserEvent::TextPanelInput {
                window_id: ivars.window_id,
                pane_id: ivars.pane_id,
                process: ivars.process.clone(),
                text: self.text(),
                paste,
            });
        }
    }

    pub(crate) struct TextPanel {
        window: Retained<AnyObject>,
        delegate: Retained<Delegate>,
        find_button: Retained<AnyObject>,
        stale: Cell<bool>,
        title: String,
    }

    unsafe fn owned(object: *mut AnyObject) -> io::Result<Retained<AnyObject>> {
        unsafe { Retained::from_raw(object) }
            .ok_or_else(|| io::Error::other("could not create native text view"))
    }

    impl TextPanel {
        pub(crate) fn open(
            title: &str,
            text: &str,
            mode: super::TextPanelMode,
            window_id: WindowId,
            pane_id: PaneId,
            process: Option<String>,
            proxy: EventLoopProxy<UserEvent>,
        ) -> io::Result<Self> {
            let editable = mode == super::TextPanelMode::Compose;
            let class = |name| {
                AnyClass::get(name)
                    .ok_or_else(|| io::Error::other("native text panel is unavailable"))
            };
            let panel_class = class(c"NSPanel")?;
            let scroll_class = class(c"NSScrollView")?;
            let text_class = class(c"NSTextView")?;
            let button_class = class(c"NSButton")?;
            let font_class = class(c"NSFont")?;
            let rect = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            let text = super::bounded_text(text);

            // SAFETY: main-thread App command dispatch; selectors and their
            // argument/return types are documented AppKit APIs. Retained values
            // own +1 alloc/init results; views are also retained by their parents.
            unsafe {
                let allocated: *mut AnyObject = msg_send![panel_class, alloc];
                let window = owned(msg_send![allocated,
                    initWithContentRect: rect(0.0, 0.0, 760.0, 520.0),
                    styleMask: 11_usize, backing: 2_usize, defer: false])?;
                let _: () = msg_send![&*window, setReleasedWhenClosed: false];
                let _: () = msg_send![&*window, setFloatingPanel: false];
                let _: () = msg_send![&*window, setTitle: &*NSString::from_str(title)];
                let _: () = msg_send![&*window, setMinSize: NSSize::new(640.0, 300.0)];
                let content: *mut AnyObject = msg_send![&*window, contentView];
                let allocated: *mut AnyObject = msg_send![scroll_class, alloc];
                let scroll =
                    owned(msg_send![allocated, initWithFrame: rect(16.0, 56.0, 728.0, 448.0)])?;
                let _: () = msg_send![&*scroll, setAutoresizingMask: 18_usize];
                let _: () = msg_send![&*scroll, setHasVerticalScroller: true];
                let _: () = msg_send![&*scroll, setBorderType: 1_usize];
                let allocated: *mut AnyObject = msg_send![text_class, alloc];
                let text_view =
                    owned(msg_send![allocated, initWithFrame: rect(0.0, 0.0, 708.0, 448.0)])?;
                let _: () = msg_send![&*text_view, setEditable: editable];
                let _: () = msg_send![&*text_view, setSelectable: true];
                let _: () = msg_send![&*text_view, setRichText: false];
                let _: () = msg_send![&*text_view, setAllowsUndo: editable];
                let _: () = msg_send![&*text_view, setUsesFindBar: true];
                let _: () = msg_send![&*text_view, setAutomaticQuoteSubstitutionEnabled: false];
                let _: () = msg_send![&*text_view, setAutomaticDashSubstitutionEnabled: false];
                let _: () = msg_send![&*text_view, setAutomaticTextReplacementEnabled: false];
                let _: () = msg_send![&*text_view, setVerticallyResizable: true];
                let _: () = msg_send![&*text_view, setHorizontallyResizable: false];
                let _: () = msg_send![&*text_view, setAutoresizingMask: 2_usize];
                let _: () = msg_send![&*text_view, setMaxSize: NSSize::new(f64::MAX, f64::MAX)];
                let _: () = msg_send![&*text_view, setTextContainerInset: NSSize::new(12.0, 12.0)];
                let container: *mut AnyObject = msg_send![&*text_view, textContainer];
                let _: () = msg_send![container, setWidthTracksTextView: true];
                let _: () = msg_send![container, setContainerSize: NSSize::new(708.0, f64::MAX)];
                let font: *mut AnyObject =
                    msg_send![font_class, monospacedSystemFontOfSize: 14.0_f64, weight: 0.0_f64];
                let _: () = msg_send![&*text_view, setFont: font];
                let _: () = msg_send![&*text_view, setString: &*NSString::from_str(&text)];
                let _: () = msg_send![&*scroll, setDocumentView: &*text_view];
                let _: () = msg_send![content, addSubview: &*scroll];
                let blocks = if mode == super::TextPanelMode::File {
                    vec![0..text.encode_utf16().count()]
                } else {
                    super::code_blocks(&text)
                };
                if mode == super::TextPanelMode::Output {
                    let prose: *mut AnyObject = msg_send![font_class, systemFontOfSize: 15.0_f64];
                    let _: () = msg_send![&*text_view, setFont: prose];
                    for block in &blocks {
                        let _: () = msg_send![&*text_view, setFont: font, range: NSRange::new(block.start, block.end - block.start)];
                    }
                    let mut offset = 0;
                    for line in text.split_inclusive('\n') {
                        let length = line.encode_utf16().count();
                        if (line.starts_with("# ")
                            || line.starts_with("## ")
                            || line.starts_with("### "))
                            && !blocks.iter().any(|block| block.contains(&offset))
                        {
                            let heading: *mut AnyObject =
                                msg_send![font_class, boldSystemFontOfSize: 18.0_f64];
                            let _: () = msg_send![&*text_view, setFont: heading, range: NSRange::new(offset, length)];
                        }
                        offset += length;
                    }
                }
                let allocated = Delegate::alloc().set_ivars(Ivars {
                    proxy,
                    window_id,
                    pane_id,
                    process,
                    text_view: text_view.clone(),
                    editable,
                    mode,
                    blocks,
                    block_index: Cell::new(0),
                });
                let delegate: Retained<Delegate> = msg_send![super(allocated), init];
                let _: () = msg_send![&*window, setDelegate: &*delegate];

                let allocated: *mut AnyObject = msg_send![button_class, alloc];
                let find =
                    owned(msg_send![allocated, initWithFrame: rect(16.0, 14.0, 100.0, 30.0)])?;
                let _: () = msg_send![&*find, setTitle: &*NSString::from_str("Find")];
                let _: () = msg_send![&*find, setBezelStyle: 1_usize];
                let _: () = msg_send![&*find, setTag: 1_isize];
                let _: () = msg_send![&*find, setTarget: &*text_view];
                let _: () = msg_send![&*find, setAction: sel!(performFindPanelAction:)];
                let _: () = msg_send![content, addSubview: &*find];

                if !editable {
                    let allocated: *mut AnyObject = msg_send![button_class, alloc];
                    let latest =
                        owned(msg_send![allocated, initWithFrame: rect(125.0, 14.0, 160.0, 30.0)])?;
                    let _: () =
                        msg_send![&*latest, setTitle: &*NSString::from_str("Return to Latest")];
                    let _: () = msg_send![&*latest, setBezelStyle: 1_usize];
                    let _: () = msg_send![&*latest, setTarget: &*delegate];
                    let _: () = msg_send![&*latest, setAction: sel!(returnToPane:)];
                    let _: () = msg_send![content, addSubview: &*latest];
                }

                let allocated: *mut AnyObject = msg_send![button_class, alloc];
                let action =
                    owned(msg_send![allocated, initWithFrame: rect(490.0, 14.0, 250.0, 30.0)])?;
                let _: () = msg_send![&*action, setAutoresizingMask: 1_usize];
                let _: () = msg_send![&*action, setTitle: &*NSString::from_str(if editable { "Paste to Pane" } else if mode == super::TextPanelMode::File { "Copy File" } else { "Copy Next Code Block" })];
                let _: () = msg_send![&*action, setBezelStyle: 1_usize];
                let _: () = msg_send![&*action, setEnabled: editable || !delegate.ivars().blocks.is_empty()];
                let _: () = msg_send![&*action, setTarget: &*delegate];
                let _: () = msg_send![&*action, setAction: if editable { sel!(pasteToPane:) } else { sel!(copyCode:) }];
                let _: () = msg_send![content, addSubview: &*action];
                let _: () = msg_send![&*window, center];
                let _: () =
                    msg_send![&*window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
                let _: bool = msg_send![&*window, makeFirstResponder: &*text_view];
                Ok(Self {
                    window,
                    delegate,
                    find_button: find,
                    stale: Cell::new(false),
                    title: title.to_string(),
                })
            }
        }

        pub(crate) fn draft(&self) -> Option<(PaneId, String)> {
            self.delegate
                .ivars()
                .editable
                .then(|| (self.delegate.ivars().pane_id, self.delegate.text()))
        }

        pub(crate) fn can_close(&self) -> bool {
            self.delegate.check_length()
        }

        pub(crate) fn close(&self) {
            if !self.can_close() {
                return;
            }
            unsafe {
                let _: () = msg_send![&*self.window, close];
            }
        }

        pub(crate) fn set_title(&self, title: &str) {
            unsafe {
                let _: () = msg_send![&*self.window, setTitle: &*NSString::from_str(title)];
            }
        }

        pub(crate) fn pane_id(&self) -> PaneId {
            self.delegate.ivars().pane_id
        }

        pub(crate) fn note_output(&self, pane: PaneId) {
            if pane == self.pane_id()
                && self.delegate.ivars().mode == super::TextPanelMode::Output
                && !self.stale.replace(true)
            {
                self.set_title(&format!("{} — New output available", self.title));
            }
        }

        pub(crate) fn handle_command(&self, command: AppCommand) -> bool {
            use crate::commands::{SearchAction, TerminalAction};
            // Menus are shared with winit windows; route editing to the native
            // first responder while this panel is key, never to the terminal.
            unsafe {
                let key: bool = msg_send![&*self.window, isKeyWindow];
                if !key {
                    return false;
                }
                let view = &*self.delegate.ivars().text_view;
                let nil = std::ptr::null::<AnyObject>();
                let application: *mut AnyObject =
                    msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
                match command {
                    AppCommand::Copy => {
                        let _: bool =
                            msg_send![application, sendAction: sel!(copy:), to: nil, from: nil];
                    }
                    AppCommand::Paste => {
                        let _: bool = msg_send![application, sendAction: sel!(pasteAsPlainText:), to: nil, from: nil];
                    }
                    AppCommand::Terminal(TerminalAction::SelectAll) => {
                        let _: bool = msg_send![application, sendAction: sel!(selectAll:), to: nil, from: nil];
                    }
                    AppCommand::Search(action) => {
                        let tag = match action {
                            SearchAction::FindNext => 2_isize,
                            SearchAction::FindPrevious => 3,
                            _ => 1,
                        };
                        let _: () = msg_send![&*self.find_button, setTag: tag];
                        let _: () = msg_send![view, performFindPanelAction: &*self.find_button];
                        let _: () = msg_send![&*self.find_button, setTag: 1_isize];
                    }
                    AppCommand::CloseTab | AppCommand::CloseWindow => self.close(),
                    AppCommand::Quit | AppCommand::About => return false,
                    _ => {}
                }
                true
            }
        }
    }

    impl Drop for TextPanel {
        fn drop(&mut self) {
            // Clear the weak native delegate before releasing the Rust owner.
            unsafe {
                let _: () = msg_send![&*self.window, setDelegate: std::ptr::null::<AnyObject>()];
                let _: () = msg_send![&*self.window, close];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_drafts_keep_utf8_intact() {
        let text = "あ".repeat(TEXT_LIMIT);
        let result = bounded_text(&text);
        assert!(result.len() <= TEXT_LIMIT);
        assert!(text.starts_with(&result));
        assert_eq!(result.len() % 3, 0);
    }

    #[test]
    fn code_block_copy_excludes_fences_and_uses_utf16() {
        let text = "🦀\n```rust\nlet x = 1;\n```\n";
        let blocks = code_blocks(text);
        let utf16: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            String::from_utf16(&utf16[blocks[0].clone()]).unwrap(),
            "let x = 1;\n"
        );
        assert!(code_blocks("no code").is_empty());
    }

    #[test]
    fn code_blocks_preserve_shorter_fences_inside_a_block() {
        let text = "````markdown\n```rust\nx\n```\n````\n~~~\ny\n~~~\n";
        let utf16: Vec<_> = text.encode_utf16().collect();
        let blocks: Vec<_> = code_blocks(text)
            .into_iter()
            .map(|range| String::from_utf16(&utf16[range]).unwrap())
            .collect();
        assert_eq!(blocks, ["```rust\nx\n```\n", "y\n"]);
    }
}
