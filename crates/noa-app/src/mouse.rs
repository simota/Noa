//! Mouse-position and click gesture handling for local selection.

use std::time::{Duration, Instant};

use noa_core::{GridSize, Point};
use noa_grid::modes::MouseTracking;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::ModifiersState;

const MULTI_CLICK_MAX_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClickKind {
    Single,
    Double,
    Triple,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionGesture {
    None,
    Clear,
    Extend { anchor: Point, focus: Point },
    SelectWord(Point),
    SelectLine(Point),
}

#[derive(Clone, Debug)]
pub struct MouseSelectionState {
    last_cell: Option<Point>,
    left_down: bool,
    drag_anchor: Option<Point>,
    drag_started: bool,
    click_tracker: ClickTracker,
}

impl Default for MouseSelectionState {
    fn default() -> Self {
        Self {
            last_cell: None,
            left_down: false,
            drag_anchor: None,
            drag_started: false,
            click_tracker: ClickTracker::new(MULTI_CLICK_MAX_DELAY),
        }
    }
}

impl MouseSelectionState {
    pub fn cursor_moved(&mut self, cell: Point) -> SelectionGesture {
        self.last_cell = Some(cell);
        if !self.left_down {
            return SelectionGesture::None;
        }

        let Some(anchor) = self.drag_anchor else {
            return SelectionGesture::None;
        };
        if cell != anchor {
            self.drag_started = true;
        }
        if self.drag_started {
            SelectionGesture::Extend {
                anchor,
                focus: cell,
            }
        } else {
            SelectionGesture::None
        }
    }

    pub fn left_pressed(&mut self, now: Instant) -> SelectionGesture {
        let Some(cell) = self.last_cell else {
            return SelectionGesture::None;
        };

        self.left_down = true;
        self.drag_anchor = Some(cell);
        self.drag_started = false;

        match self.click_tracker.record_press(cell, now) {
            ClickKind::Single => SelectionGesture::Clear,
            ClickKind::Double => {
                self.cancel_drag();
                SelectionGesture::SelectWord(cell)
            }
            ClickKind::Triple => {
                self.cancel_drag();
                SelectionGesture::SelectLine(cell)
            }
        }
    }

    pub fn left_released(&mut self) -> SelectionGesture {
        self.cancel_drag();
        SelectionGesture::None
    }

    fn cancel_drag(&mut self) {
        self.left_down = false;
        self.drag_anchor = None;
        self.drag_started = false;
    }
}

#[derive(Clone, Debug)]
struct ClickTracker {
    max_delay: Duration,
    last_cell: Option<Point>,
    last_at: Option<Instant>,
    count: u8,
}

impl ClickTracker {
    fn new(max_delay: Duration) -> Self {
        Self {
            max_delay,
            last_cell: None,
            last_at: None,
            count: 0,
        }
    }

    fn record_press(&mut self, cell: Point, now: Instant) -> ClickKind {
        let continues = self.last_cell == Some(cell)
            && self
                .last_at
                .and_then(|last| now.checked_duration_since(last))
                .is_some_and(|elapsed| elapsed <= self.max_delay);

        self.count = if continues { (self.count % 3) + 1 } else { 1 };
        self.last_cell = Some(cell);
        self.last_at = Some(now);

        match self.count {
            2 => ClickKind::Double,
            3 => ClickKind::Triple,
            _ => ClickKind::Single,
        }
    }
}

pub fn physical_position_to_grid_point(
    x: f64,
    y: f64,
    cell_w: f32,
    cell_h: f32,
    grid_size: GridSize,
) -> Point {
    let col = coord_to_cell(x, cell_w, grid_size.cols);
    let row = coord_to_cell(y, cell_h, grid_size.rows);
    Point { x: col, y: row }
}

pub fn encode_sgr_mouse_input(
    button: MouseButton,
    state: ElementState,
    cell: Point,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    let button_code = button_code(button)?;
    let final_byte = match state {
        ElementState::Pressed => b'M',
        ElementState::Released => b'm',
    };
    Some(sgr_mouse_sequence(
        button_code + modifier_bits(mods),
        cell,
        final_byte,
    ))
}

pub fn encode_sgr_mouse_motion(
    tracking: MouseTracking,
    pressed_button: Option<MouseButton>,
    cell: Point,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    let button_code = match tracking {
        MouseTracking::Off | MouseTracking::Press => return None,
        MouseTracking::ButtonMotion => button_code(pressed_button?)?,
        MouseTracking::AnyMotion => pressed_button.and_then(button_code).unwrap_or(3),
    };

    Some(sgr_mouse_sequence(
        button_code + 32 + modifier_bits(mods),
        cell,
        b'M',
    ))
}

pub fn encode_sgr_mouse_wheel(delta_y: f32, cell: Point, mods: ModifiersState) -> Option<Vec<u8>> {
    let button_code = if delta_y > 0.0 {
        64
    } else if delta_y < 0.0 {
        65
    } else {
        return None;
    };
    Some(sgr_mouse_sequence(
        button_code + modifier_bits(mods),
        cell,
        b'M',
    ))
}

fn button_code(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

fn modifier_bits(mods: ModifiersState) -> u16 {
    let mut bits = 0;
    if mods.shift_key() {
        bits |= 4;
    }
    if mods.alt_key() {
        bits |= 8;
    }
    if mods.control_key() {
        bits |= 16;
    }
    bits
}

fn sgr_mouse_sequence(code: u16, cell: Point, final_byte: u8) -> Vec<u8> {
    format!(
        "\x1b[<{code};{};{}{}",
        cell.x + 1,
        cell.y + 1,
        final_byte as char
    )
    .into_bytes()
}

fn coord_to_cell(coord: f64, cell: f32, max_cells: u16) -> u16 {
    if max_cells == 0 {
        return 0;
    }
    let cell = f64::from(cell).max(f64::EPSILON);
    ((coord / cell).floor().max(0.0) as u16).min(max_cells - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: u16, y: u16) -> Point {
        Point { x, y }
    }

    #[test]
    fn physical_position_maps_to_clamped_grid_cell() {
        assert_eq!(
            physical_position_to_grid_point(25.0, 39.0, 10.0, 20.0, GridSize::new(3, 2)),
            point(2, 1)
        );
        assert_eq!(
            physical_position_to_grid_point(-5.0, -1.0, 10.0, 20.0, GridSize::new(3, 2)),
            point(0, 0)
        );
        assert_eq!(
            physical_position_to_grid_point(500.0, 500.0, 10.0, 20.0, GridSize::new(3, 2)),
            point(2, 1)
        );
    }

    #[test]
    fn click_tracker_detects_single_double_triple_same_cell() {
        let mut tracker = ClickTracker::new(Duration::from_millis(500));
        let start = Instant::now();

        assert_eq!(tracker.record_press(point(1, 1), start), ClickKind::Single);
        assert_eq!(
            tracker.record_press(point(1, 1), start + Duration::from_millis(100)),
            ClickKind::Double
        );
        assert_eq!(
            tracker.record_press(point(1, 1), start + Duration::from_millis(200)),
            ClickKind::Triple
        );
    }

    #[test]
    fn click_tracker_resets_after_delay_or_cell_change() {
        let mut tracker = ClickTracker::new(Duration::from_millis(500));
        let start = Instant::now();

        assert_eq!(tracker.record_press(point(1, 1), start), ClickKind::Single);
        assert_eq!(
            tracker.record_press(point(2, 1), start + Duration::from_millis(100)),
            ClickKind::Single
        );
        assert_eq!(
            tracker.record_press(point(2, 1), start + Duration::from_millis(700)),
            ClickKind::Single
        );
    }

    #[test]
    fn drag_extends_after_cell_changes() {
        let mut state = MouseSelectionState::default();
        let start = Instant::now();

        assert_eq!(state.cursor_moved(point(1, 1)), SelectionGesture::None);
        assert_eq!(state.left_pressed(start), SelectionGesture::Clear);
        assert_eq!(state.cursor_moved(point(1, 1)), SelectionGesture::None);
        assert_eq!(
            state.cursor_moved(point(3, 2)),
            SelectionGesture::Extend {
                anchor: point(1, 1),
                focus: point(3, 2)
            }
        );
        assert_eq!(state.left_released(), SelectionGesture::None);
    }

    #[test]
    fn double_and_triple_click_emit_word_and_line_gestures() {
        let mut state = MouseSelectionState::default();
        let start = Instant::now();

        state.cursor_moved(point(2, 0));
        assert_eq!(state.left_pressed(start), SelectionGesture::Clear);
        state.left_released();
        assert_eq!(
            state.left_pressed(start + Duration::from_millis(100)),
            SelectionGesture::SelectWord(point(2, 0))
        );
        state.left_released();
        assert_eq!(
            state.left_pressed(start + Duration::from_millis(200)),
            SelectionGesture::SelectLine(point(2, 0))
        );
    }

    #[test]
    fn sgr_mouse_input_uses_one_based_coordinates() {
        assert_eq!(
            encode_sgr_mouse_input(
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<0;3;4M".to_vec())
        );
        assert_eq!(
            encode_sgr_mouse_input(
                MouseButton::Left,
                ElementState::Released,
                point(2, 3),
                ModifiersState::SHIFT | ModifiersState::CONTROL
            ),
            Some(b"\x1b[<20;3;4m".to_vec())
        );
    }

    #[test]
    fn sgr_mouse_motion_respects_tracking_mode() {
        assert_eq!(
            encode_sgr_mouse_motion(
                MouseTracking::Press,
                Some(MouseButton::Left),
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
        assert_eq!(
            encode_sgr_mouse_motion(
                MouseTracking::ButtonMotion,
                Some(MouseButton::Left),
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<32;1;1M".to_vec())
        );
        assert_eq!(
            encode_sgr_mouse_motion(
                MouseTracking::AnyMotion,
                None,
                point(0, 0),
                ModifiersState::ALT
            ),
            Some(b"\x1b[<43;1;1M".to_vec())
        );
    }

    #[test]
    fn sgr_mouse_wheel_encodes_vertical_delta() {
        assert_eq!(
            encode_sgr_mouse_wheel(1.0, point(0, 0), ModifiersState::empty()),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
        assert_eq!(
            encode_sgr_mouse_wheel(-1.0, point(0, 0), ModifiersState::empty()),
            Some(b"\x1b[<65;1;1M".to_vec())
        );
        assert_eq!(
            encode_sgr_mouse_wheel(0.0, point(0, 0), ModifiersState::empty()),
            None
        );
    }
}
