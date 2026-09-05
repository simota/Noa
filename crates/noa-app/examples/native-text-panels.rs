//! Explicit GUI smoke check with synthetic content, without a shell/clipboard writes.

#[cfg(target_os = "macos")]
pub use noa_app::{AppCommand, UserEvent, split_tree};
#[cfg(target_os = "macos")]
mod commands {
    pub use noa_app::{SearchAction, TerminalAction};
}
#[cfg(target_os = "macos")]
#[path = "../src/text_panel.rs"]
mod text_panel;

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    use objc2_foundation::{NSRange, NSString};
    use std::time::{Duration, Instant};
    use text_panel::{TextPanel, TextPanelMode};
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
        window::WindowId,
    };

    const PROMPT: &str =
        "日本語の指示を編集します。\n\n> error at src/main.rs:42\n\nPlease fix the regression.";
    struct Smoke {
        panel: Option<TextPanel>,
        proxy: EventLoopProxy<UserEvent>,
        phase: usize,
        ready: Instant,
        focus_deadline: Instant,
    }

    fn activate_panel(title: &str) {
        unsafe {
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
            let windows: *mut AnyObject = msg_send![app, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
                let name: objc2::rc::Retained<NSString> = msg_send![window, title];
                if name.to_string().starts_with(title) {
                    let _: () =
                        msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
                }
            }
        }
    }

    fn check_find_selection(panel: &TextPanel) {
        // Exercise the native find field without reading or writing the clipboard.
        unsafe {
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let window: *mut AnyObject = msg_send![app, keyWindow];
            let field: *mut AnyObject = msg_send![window, firstResponder];
            let is_editor: bool = msg_send![field, isFieldEditor];
            assert!(is_editor, "Find must focus its native field editor");
            let query = "日本語 query";
            let _: () = msg_send![field, setString: &*NSString::from_str(query)];
            assert!(
                panel.handle_command(AppCommand::Terminal(commands::TerminalAction::SelectAll))
            );
            let range: NSRange = msg_send![field, selectedRange];
            assert_eq!(range, NSRange::new(0, query.encode_utf16().count()));
        }
    }
    impl ApplicationHandler<UserEvent> for Smoke {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            unsafe {
                let app: *mut AnyObject =
                    msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
                let _: () = msg_send![app, activateIgnoringOtherApps: true];
            }
            self.panel = Some(
                TextPanel::open(
                    "Compose Prompt — Sample / feature/test",
                    PROMPT,
                    TextPanelMode::Compose,
                    WindowId::from(1u64),
                    split_tree::PaneId::new(1),
                    None,
                    self.proxy.clone(),
                )
                .unwrap(),
            );
            self.ready = Instant::now() + Duration::from_millis(300);
            self.focus_deadline = Instant::now() + Duration::from_secs(10);
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.phase == 6 {
                return;
            }
            if Instant::now() < self.ready {
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
                return;
            }
            let panel = self.panel.as_ref().unwrap();
            if self.phase == 0 || self.phase == 2 || self.phase == 4 {
                if !panel.handle_command(AppCommand::Search(commands::SearchAction::Find)) {
                    assert!(
                        Instant::now() < self.focus_deadline,
                        "native panel did not gain focus in phase {}",
                        self.phase
                    );
                    activate_panel(match self.phase {
                        0 => "Compose Prompt",
                        2 => "Output Snapshot",
                        _ => "Agent Workflows",
                    });
                    self.ready = Instant::now() + Duration::from_millis(100);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
                    return;
                }
                self.phase += 1;
                self.ready = Instant::now() + Duration::from_millis(300);
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
            } else if self.phase == 1 {
                check_find_selection(panel);
                assert_eq!(
                    panel.draft(),
                    Some((split_tree::PaneId::new(1), PROMPT.to_string()))
                );
                panel.close();
                self.panel = Some(TextPanel::open("Output Snapshot — Sample", "# Result\n\n日本語と English の説明。\n\n```rust\nfn main() {\n    println!(\"Hello\");\n}\n```\n\nThe terminal keeps running while this snapshot stays still.",
                    TextPanelMode::Output, WindowId::from(1u64), split_tree::PaneId::new(1), None, self.proxy.clone()).unwrap());
                self.phase = 2;
                self.focus_deadline = Instant::now() + Duration::from_secs(10);
                self.ready = Instant::now() + Duration::from_millis(300);
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
            } else {
                check_find_selection(panel);
                assert!(panel.draft().is_none());
                panel.note_output(split_tree::PaneId::new(1));
                panel.close();
                if self.phase == 3 {
                    self.panel = Some(
                        TextPanel::open(
                            "Agent Workflows — Sample",
                            include_str!("../../../docs/AGENT_WORKFLOW.md"),
                            TextPanelMode::Guide,
                            WindowId::from(1u64),
                            split_tree::PaneId::new(1),
                            None,
                            self.proxy.clone(),
                        )
                        .unwrap(),
                    );
                    self.phase = 4;
                    self.focus_deadline = Instant::now() + Duration::from_secs(10);
                    self.ready = Instant::now() + Duration::from_millis(300);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(self.ready));
                } else {
                    self.phase = 6;
                    event_loop.exit();
                }
            }
        }
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Regular)
        .build()
        .unwrap();
    let mut smoke = Smoke {
        panel: None,
        proxy: event_loop.create_proxy(),
        phase: 0,
        ready: Instant::now(),
        focus_deadline: Instant::now(),
    };
    event_loop.run_app(&mut smoke).unwrap();
    assert_eq!(smoke.phase, 6);
    println!(
        "Native composer, Japanese draft, reader, guide, find routing, and close checks passed."
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Native text panel smoke check requires macOS.");
}
