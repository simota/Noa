//! Viewport-scroll and terminal-action application helpers.

use super::*;

/// Apply `action` to `terminal`. Returns whether the caller must write a
/// form feed (`0x0C`) to the pty afterward — only `Clear` at a shell prompt
/// asks for this (see [`Terminal::clear_screen_and_scrollback`]).
pub(crate) fn apply_terminal_action(terminal: &mut Terminal, action: TerminalAction) -> bool {
    match action {
        TerminalAction::Clear => return terminal.clear_screen_and_scrollback(),
        TerminalAction::ClearScrollback => terminal.clear_scrollback(),
        TerminalAction::SelectAll => terminal.select_all(),
    }
    false
}

pub(crate) fn apply_viewport_scroll(
    terminal: &mut Terminal,
    grid_size: GridSize,
    scroll: ViewportScroll,
) {
    let page_rows = usize::from(grid_size.rows.saturating_sub(1).max(1));
    match scroll {
        ViewportScroll::LineUp => terminal.scroll_viewport_up(1),
        ViewportScroll::LineDown => terminal.scroll_viewport_down(1),
        ViewportScroll::PageUp => terminal.scroll_viewport_up(page_rows),
        ViewportScroll::PageDown => terminal.scroll_viewport_down(page_rows),
        ViewportScroll::Top => terminal.scroll_viewport_to_top(),
        ViewportScroll::Bottom => terminal.scroll_viewport_to_bottom(),
        ViewportScroll::PrevPrompt => {
            terminal.scroll_to_prompt(PromptJump::Prev);
        }
        ViewportScroll::NextPrompt => {
            terminal.scroll_to_prompt(PromptJump::Next);
        }
    }
}

pub(crate) fn apply_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    grid_size: GridSize,
    scroll: ViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_viewport_scroll(terminal, grid_size, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
}

pub(crate) fn mouse_wheel_viewport_scroll(
    delta: MouseScrollDelta,
    cell_height: f32,
) -> Option<MouseWheelViewportScroll> {
    let (delta_y, rows) = match delta {
        MouseScrollDelta::LineDelta(_, y) => (y, y.abs().ceil() as usize),
        MouseScrollDelta::PixelDelta(position) => {
            let y = position.y as f32;
            let rows = (y.abs() / cell_height.max(f32::EPSILON)).ceil() as usize;
            (y, rows)
        }
    };

    if !delta_y.is_finite() || delta_y == 0.0 || rows == 0 {
        return None;
    }

    if delta_y > 0.0 {
        Some(MouseWheelViewportScroll::Up(rows))
    } else {
        Some(MouseWheelViewportScroll::Down(rows))
    }
}

pub(crate) fn mouse_wheel_should_send_cursor_keys(
    tracking: MouseTracking,
    active_is_alt: bool,
    alternate_scroll_mode: bool,
) -> bool {
    if tracking != MouseTracking::Off || !alternate_scroll_mode {
        return false;
    }
    active_is_alt
}

pub(crate) fn apply_mouse_wheel_viewport_scroll(
    terminal: &mut Terminal,
    scroll: MouseWheelViewportScroll,
) {
    match scroll {
        MouseWheelViewportScroll::Up(rows) => terminal.scroll_viewport_up(rows),
        MouseWheelViewportScroll::Down(rows) => terminal.scroll_viewport_down(rows),
    }
}

pub(crate) fn apply_mouse_wheel_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    scroll: MouseWheelViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_mouse_wheel_viewport_scroll(terminal, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
}
