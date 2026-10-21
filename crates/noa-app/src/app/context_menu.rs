//! Right-click context menu off macOS.
//!
//! macOS pops a native AppKit menu (`show_macos_split_context_menu`). winit
//! has no popup-menu API elsewhere, so off macOS the menu is an in-surface
//! card painted with the overlay renderer (`sidebar::draw_context_menu`) —
//! Ghostty's GTK build shows a `PopoverMenu` on right-click in the same
//! spirit. While open it owns the pointer and the keyboard: hovering
//! highlights a row, a left click or Enter runs it, and any other press,
//! Escape, a wheel turn, focus loss or a resize dismisses it. It never opens
//! over a modal (dialog, palette, prompt) that already owns the keyboard.

#![cfg_attr(target_os = "macos", allow(dead_code))]

use super::*;
use crate::split_tree::Point as MousePoint;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ContextMenuEntry {
    Item {
        command: AppCommand,
        label: &'static str,
        enabled: bool,
    },
    Separator,
}

#[derive(Clone, Debug)]
pub(super) struct ContextMenuSession {
    pub(super) window_id: WindowId,
    pub(super) entries: Vec<ContextMenuEntry>,
    pub(super) hovered: Option<usize>,
    /// Card origin and size in window pixels.
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) size: PixelSize,
    /// Text-block inset inside the card and the row height, for hit-testing.
    pub(super) pad_y: f32,
    pub(super) cell_h: f32,
}

impl ContextMenuSession {
    /// Text columns of the menu: the longest label plus one space each side.
    pub(super) fn cols(entries: &[ContextMenuEntry]) -> u16 {
        let widest = entries
            .iter()
            .filter_map(|entry| match entry {
                ContextMenuEntry::Item { label, .. } => Some(label.chars().count()),
                ContextMenuEntry::Separator => None,
            })
            .max()
            .unwrap_or(0);
        (widest + 2).min(u16::MAX as usize) as u16
    }

    fn contains(&self, point: MousePoint) -> bool {
        (self.x..self.x + self.size.w).contains(&point.x)
            && (self.y..self.y + self.size.h).contains(&point.y)
    }

    /// The enabled item under `point`, if any.
    fn item_at(&self, point: MousePoint) -> Option<usize> {
        if !self.contains(point) {
            return None;
        }
        let index = entry_index_at((point.y - self.y) as f32, self.pad_y, self.cell_h)?;
        is_selectable(self.entries.get(index)?).then_some(index)
    }

    /// Move the keyboard highlight to the next enabled item in `step`'s
    /// direction, wrapping around.
    fn step_hover(&mut self, step: isize) {
        let len = self.entries.len() as isize;
        let start = self
            .hovered
            .map_or(if step > 0 { -1 } else { len }, |index| index as isize);
        for offset in 1..=len {
            let index = (start + step * offset).rem_euclid(len) as usize;
            if is_selectable(&self.entries[index]) {
                self.hovered = Some(index);
                return;
            }
        }
    }

    fn hovered_command(&self) -> Option<AppCommand> {
        match self.entries.get(self.hovered?)? {
            ContextMenuEntry::Item {
                command,
                enabled: true,
                ..
            } => Some(*command),
            _ => None,
        }
    }
}

fn is_selectable(entry: &ContextMenuEntry) -> bool {
    matches!(entry, ContextMenuEntry::Item { enabled: true, .. })
}

/// Row index at `y` pixels below the card's top edge, given the text block's
/// top inset and the row height. `None` inside the top padding.
fn entry_index_at(y: f32, pad_y: f32, cell_h: f32) -> Option<usize> {
    let row = (y - pad_y) / cell_h;
    (row >= 0.0).then_some(row as usize)
}

/// Clamp a card of `size` anchored at `(x, y)` so it stays inside `surface`.
fn clamp_origin(point: MousePoint, size: PixelSize, surface: PixelSize) -> (u32, u32) {
    (
        point.x.min(surface.w.saturating_sub(size.w)),
        point.y.min(surface.h.saturating_sub(size.h)),
    )
}

impl App {
    fn context_menu_entries(&self, window_id: WindowId) -> Vec<ContextMenuEntry> {
        let item = |command, label| ContextMenuEntry::Item {
            command,
            label,
            enabled: self.command_is_enabled(window_id, command),
        };
        let auto_approve = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.auto_approve_enabled.load(Ordering::Relaxed));
        let send_selection = self
            .windows
            .get(&window_id)
            .map(|state| state.focused_pane)
            .is_some_and(|pane| self.can_open_send_selection_picker_for_pane(window_id, pane));
        vec![
            item(AppCommand::Copy, "Copy"),
            item(AppCommand::Paste, "Paste"),
            item(AppCommand::Terminal(TerminalAction::Clear), "Clear"),
            ContextMenuEntry::Separator,
            item(AppCommand::NewSplitLeft, "Add Pane Left"),
            item(AppCommand::NewSplitRight, "Add Pane Right"),
            item(AppCommand::NewSplitUp, "Add Pane Up"),
            item(AppCommand::NewSplitDown, "Add Pane Down"),
            ContextMenuEntry::Separator,
            item(AppCommand::EqualizeSplits, "Equalize Splits"),
            item(AppCommand::ToggleSplitZoom, "Toggle Split Zoom"),
            item(
                AppCommand::ToggleAutoApprove,
                if auto_approve {
                    "Auto Approve: On"
                } else {
                    "Auto Approve: Off"
                },
            ),
            ContextMenuEntry::Separator,
            ContextMenuEntry::Item {
                command: AppCommand::SendSelectionToPane,
                label: "Send Selection to Pane",
                enabled: send_selection,
            },
            item(AppCommand::SetTabTitle, "Set Tab Title\u{2026}"),
            ContextMenuEntry::Separator,
            item(AppCommand::NewTab, "New Tab"),
            item(AppCommand::CloseTab, "Close Tab"),
        ]
    }

    /// Open the menu with its top-left corner at the pointer, pulled back
    /// inside the window when it would overflow.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn open_context_menu(&mut self, window_id: WindowId) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        if self.modal_ime_target(window_id).is_some()
            || self
                .send_selection_picker
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
        {
            return;
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let Some(point) = state.last_mouse_point else {
            return;
        };
        let surface = state.window.inner_size();
        let surface = PixelSize {
            w: surface.width,
            h: surface.height,
        };
        let metrics = gpu.fonts.get(self.window_font_px(window_id)).metrics();
        let entries = self.context_menu_entries(window_id);
        let cols = ContextMenuSession::cols(&entries);
        let rows = entries.len().min(u16::MAX as usize) as u16;
        let (interior, size) = sidebar::context_menu_geometry(metrics, cols, rows);
        let (x, y) = clamp_origin(point, size, surface);
        self.context_menu = Some(ContextMenuSession {
            window_id,
            entries,
            hovered: None,
            x,
            y,
            size,
            pad_y: interior.top,
            cell_h: metrics.cell_h,
        });
        self.request_window_redraw(window_id);
    }

    pub(super) fn close_context_menu(&mut self, window_id: WindowId) {
        if self
            .context_menu
            .as_ref()
            .is_some_and(|menu| menu.window_id == window_id)
        {
            self.context_menu = None;
            self.request_window_redraw(window_id);
        }
    }

    pub(super) fn context_menu_open_in(&self, window_id: WindowId) -> bool {
        self.context_menu
            .as_ref()
            .is_some_and(|menu| menu.window_id == window_id)
    }

    /// Track the pointer over the open menu: the row under it is highlighted,
    /// and leaving the card clears the highlight so Enter can't run a row the
    /// pointer has left. Returns `true` while the pointer is inside the card,
    /// so the motion goes no further (no link hover underneath).
    pub(super) fn context_menu_cursor_moved(
        &mut self,
        window_id: WindowId,
        point: MousePoint,
    ) -> bool {
        let Some(menu) = self
            .context_menu
            .as_mut()
            .filter(|menu| menu.window_id == window_id)
        else {
            return false;
        };
        let hovered = menu.item_at(point);
        let inside = menu.contains(point);
        if menu.hovered != hovered {
            menu.hovered = hovered;
            self.request_window_redraw(window_id);
        }
        inside
    }

    /// The pointer left the window (or has no usable position): drop the
    /// highlight so Enter can't run a row the pointer is no longer over.
    pub(super) fn context_menu_pointer_left(&mut self, window_id: WindowId) {
        let Some(menu) = self
            .context_menu
            .as_mut()
            .filter(|menu| menu.window_id == window_id && menu.hovered.is_some())
        else {
            return;
        };
        menu.hovered = None;
        self.request_window_redraw(window_id);
    }

    /// A mouse press while the menu is open. Returns `true` when the press is
    /// consumed. A left press on an enabled item runs it; any other press
    /// dismisses the menu — a right press then falls through so it reopens
    /// the menu at the new spot.
    pub(super) fn context_menu_mouse_press(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        button: MouseButton,
    ) -> bool {
        // A second press of the armed button means its release landed in
        // another window; disarm so this press's own release goes through.
        // A different button pressed mid-hold leaves the arm alone.
        if self.context_menu_swallowed_release == Some(button) {
            self.context_menu_swallowed_release = None;
        }
        let Some(menu) = self.context_menu.take() else {
            return false;
        };
        self.request_window_redraw(menu.window_id);
        if menu.window_id != window_id {
            return false;
        }
        if button != MouseButton::Right {
            self.context_menu_swallowed_release = Some(button);
        }
        let point = self
            .windows
            .get(&window_id)
            .and_then(|s| s.last_mouse_point);
        if button == MouseButton::Left
            && let Some(command) = point
                .and_then(|point| menu.item_at(point))
                .and_then(|index| match menu.entries[index] {
                    ContextMenuEntry::Item { command, .. } => Some(command),
                    ContextMenuEntry::Separator => None,
                })
        {
            self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
        }
        button != MouseButton::Right
    }

    /// Whether this release pairs with a press the menu consumed.
    pub(super) fn context_menu_swallows_release(&mut self, button: MouseButton) -> bool {
        if self.context_menu_swallowed_release == Some(button) {
            self.context_menu_swallowed_release = None;
            return true;
        }
        false
    }

    pub(super) fn handle_context_menu_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.context_menu = None;
            }
            Key::Named(NamedKey::ArrowUp) => menu.step_hover(-1),
            Key::Named(NamedKey::ArrowDown) => menu.step_hover(1),
            Key::Named(NamedKey::Enter) => {
                let command = menu.hovered_command();
                self.context_menu = None;
                self.request_window_redraw(window_id);
                if let Some(command) = command {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                }
                return;
            }
            _ => {}
        }
        self.request_window_redraw(window_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(entries: Vec<ContextMenuEntry>) -> ContextMenuSession {
        ContextMenuSession {
            window_id: WindowId::from(1),
            entries,
            hovered: None,
            x: 100,
            y: 50,
            size: PixelSize { w: 200, h: 120 },
            pad_y: 10.0,
            cell_h: 20.0,
        }
    }

    fn item(label: &'static str, enabled: bool) -> ContextMenuEntry {
        ContextMenuEntry::Item {
            command: AppCommand::Copy,
            label,
            enabled,
        }
    }

    #[test]
    fn entry_index_skips_top_padding() {
        assert_eq!(entry_index_at(5.0, 10.0, 20.0), None);
        assert_eq!(entry_index_at(10.0, 10.0, 20.0), Some(0));
        assert_eq!(entry_index_at(29.9, 10.0, 20.0), Some(0));
        assert_eq!(entry_index_at(30.0, 10.0, 20.0), Some(1));
    }

    #[test]
    fn item_at_ignores_separators_disabled_and_outside() {
        let menu = session(vec![
            item("Copy", true),
            ContextMenuEntry::Separator,
            item("Off", false),
        ]);
        assert_eq!(menu.item_at(MousePoint { x: 150, y: 65 }), Some(0));
        assert_eq!(menu.item_at(MousePoint { x: 150, y: 85 }), None);
        assert_eq!(menu.item_at(MousePoint { x: 150, y: 105 }), None);
        assert_eq!(menu.item_at(MousePoint { x: 99, y: 65 }), None);
        assert_eq!(menu.item_at(MousePoint { x: 150, y: 170 }), None);
    }

    #[test]
    fn step_hover_wraps_over_selectable_items() {
        let mut menu = session(vec![
            item("A", true),
            ContextMenuEntry::Separator,
            item("B", false),
            item("C", true),
        ]);
        menu.step_hover(1);
        assert_eq!(menu.hovered, Some(0));
        menu.step_hover(1);
        assert_eq!(menu.hovered, Some(3));
        menu.step_hover(1);
        assert_eq!(menu.hovered, Some(0));
        menu.step_hover(-1);
        assert_eq!(menu.hovered, Some(3));
        menu.hovered = None;
        menu.step_hover(-1);
        assert_eq!(menu.hovered, Some(3));
    }

    #[test]
    fn origin_is_clamped_inside_surface() {
        let size = PixelSize { w: 200, h: 100 };
        let surface = PixelSize { w: 800, h: 600 };
        let at = |x, y| clamp_origin(MousePoint { x, y }, size, surface);
        assert_eq!(at(10, 20), (10, 20));
        assert_eq!(at(750, 580), (600, 500));
        assert_eq!(
            clamp_origin(MousePoint { x: 5, y: 5 }, size, PixelSize { w: 100, h: 50 }),
            (0, 0)
        );
    }

    #[test]
    fn cols_fit_the_widest_label() {
        assert_eq!(
            ContextMenuSession::cols(&[item("Copy", true), item("Equalize", true)]),
            10
        );
    }
}
