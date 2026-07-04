//! Mouse-position and click gesture handling for local selection.

use std::time::{Duration, Instant};

use noa_core::{GridPadding, GridSize, Point};
use noa_grid::modes::{MouseFormat, MouseTracking};
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
    padding: GridPadding,
) -> Point {
    let col = coord_to_cell(x - f64::from(padding.left), cell_w, grid_size.cols);
    let row = coord_to_cell(y - f64::from(padding.top), cell_h, grid_size.rows);
    Point { x: col, y: row }
}

pub fn encode_mouse_input(
    format: MouseFormat,
    tracking: MouseTracking,
    button: MouseButton,
    state: ElementState,
    cell: Point,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    let button_code = button_code(button)?;
    // X10 compatibility mode (DECSET 9): presses only, no modifier bits.
    if tracking == MouseTracking::X10 {
        return match state {
            ElementState::Pressed => encode_mouse_report(format, button_code, false, cell),
            ElementState::Released => None,
        };
    }
    match state {
        ElementState::Pressed => {
            encode_mouse_report(format, button_code + modifier_bits(mods), false, cell)
        }
        // SGR is the only format that can name the released button; the
        // legacy encodings all report a release as button value 3.
        ElementState::Released => {
            let code = if format == MouseFormat::Sgr {
                button_code
            } else {
                3
            };
            encode_mouse_report(format, code + modifier_bits(mods), true, cell)
        }
    }
}

pub fn encode_mouse_motion(
    format: MouseFormat,
    tracking: MouseTracking,
    pressed_button: Option<MouseButton>,
    cell: Point,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    let button_code = match tracking {
        MouseTracking::Off | MouseTracking::X10 | MouseTracking::Press => return None,
        MouseTracking::ButtonMotion => button_code(pressed_button?)?,
        MouseTracking::AnyMotion => pressed_button.and_then(button_code).unwrap_or(3),
    };

    encode_mouse_report(format, button_code + 32 + modifier_bits(mods), false, cell)
}

pub fn encode_mouse_wheel(
    format: MouseFormat,
    tracking: MouseTracking,
    delta_y: f32,
    cell: Point,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    // X10 compatibility mode predates wheel buttons: never reported.
    if tracking == MouseTracking::X10 {
        return None;
    }
    let button_code = if delta_y > 0.0 {
        64
    } else if delta_y < 0.0 {
        65
    } else {
        return None;
    };
    encode_mouse_report(format, button_code + modifier_bits(mods), false, cell)
}

/// Route a wheel event: `Some(bytes)` to report to the pty, `None` to scroll
/// the local viewport instead. A tracked mode that doesn't report this wheel
/// event (X10, or a zero delta) falls through to local scrolling rather than
/// swallowing it; so do tracking-off, a Shift override, and a missing mouse
/// cell.
pub fn route_mouse_wheel(
    tracking: MouseTracking,
    format: MouseFormat,
    shift: bool,
    delta_y: f32,
    cell: Option<Point>,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    if tracking == MouseTracking::Off || shift {
        return None;
    }
    encode_mouse_wheel(format, tracking, delta_y, cell?, mods)
}

/// Serialize one mouse report in the active format. `code` is the final
/// button value (button + modifier + motion bits) *without* the +32 bias;
/// `release` only matters for SGR, whose final byte distinguishes it.
fn encode_mouse_report(
    format: MouseFormat,
    code: u16,
    release: bool,
    cell: Point,
) -> Option<Vec<u8>> {
    let (cx, cy) = (cell.x + 1, cell.y + 1);
    match format {
        MouseFormat::Sgr => {
            let final_byte = if release { b'm' } else { b'M' };
            Some(sgr_mouse_sequence(code, cell, final_byte))
        }
        MouseFormat::Legacy => {
            // Each field is one raw byte: 223 + 32 = 255 is the last
            // representable coordinate; xterm drops events past it.
            if cx > 223 || cy > 223 {
                return None;
            }
            Some(vec![
                0x1b,
                b'[',
                b'M',
                (code + 32) as u8,
                (cx + 32) as u8,
                (cy + 32) as u8,
            ])
        }
        MouseFormat::Utf8 => {
            // Same values as Legacy but written as UTF-8 code points, which
            // extends the range to 2015 + 32 = 2047 (the two-byte limit).
            if cx > 2015 || cy > 2015 {
                return None;
            }
            let mut bytes = vec![0x1b, b'[', b'M'];
            for value in [code + 32, cx + 32, cy + 32] {
                push_utf8(&mut bytes, value);
            }
            Some(bytes)
        }
        MouseFormat::Urxvt => Some(format!("\x1b[{};{cx};{cy}M", code + 32).into_bytes()),
    }
}

fn push_utf8(bytes: &mut Vec<u8>, value: u16) {
    let mut buf = [0u8; 4];
    let ch = char::from_u32(u32::from(value)).expect("mouse report values stay below surrogates");
    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
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
        let padding = GridPadding::ZERO;
        let grid_size = GridSize::new(3, 2);

        assert_eq!(
            physical_position_to_grid_point(25.0, 39.0, 10.0, 20.0, grid_size, padding),
            point(2, 1)
        );
        assert_eq!(
            physical_position_to_grid_point(-5.0, -1.0, 10.0, 20.0, grid_size, padding),
            point(0, 0)
        );
        assert_eq!(
            physical_position_to_grid_point(500.0, 500.0, 10.0, 20.0, grid_size, padding),
            point(2, 1)
        );
    }

    #[test]
    fn physical_position_subtracts_grid_padding_before_mapping_cells() {
        let padding = GridPadding::new(4.0, 0.0, 0.0, 8.0);
        let grid_size = GridSize::new(3, 3);

        assert_eq!(
            physical_position_to_grid_point(27.0, 43.0, 10.0, 20.0, grid_size, padding),
            point(1, 1)
        );
        assert_eq!(
            physical_position_to_grid_point(4.0, 2.0, 10.0, 20.0, grid_size, padding),
            point(0, 0)
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
            encode_mouse_input(
                MouseFormat::Sgr,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<0;3;4M".to_vec())
        );
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Sgr,
                MouseTracking::Press,
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
            encode_mouse_motion(
                MouseFormat::Sgr,
                MouseTracking::Press,
                Some(MouseButton::Left),
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
        assert_eq!(
            encode_mouse_motion(
                MouseFormat::Sgr,
                MouseTracking::ButtonMotion,
                Some(MouseButton::Left),
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<32;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_motion(
                MouseFormat::Sgr,
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
            encode_mouse_wheel(
                MouseFormat::Sgr,
                MouseTracking::Press,
                1.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_wheel(
                MouseFormat::Sgr,
                MouseTracking::Press,
                -1.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<65;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_wheel(
                MouseFormat::Sgr,
                MouseTracking::Press,
                0.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
    }

    #[test]
    fn legacy_mouse_input_emits_raw_bytes() {
        // Press: Cb = 0 + 32, coordinates 1-based + 32.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32, 35, 36])
        );
        // Release: button value 3, modifiers still encoded.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Released,
                point(0, 0),
                ModifiersState::SHIFT
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 3 + 4, 33, 33])
        );
        // Middle/right buttons map to Cb 1 and 2.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::Press,
                MouseButton::Right,
                ElementState::Pressed,
                point(0, 0),
                ModifiersState::CONTROL
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 2 + 16, 33, 33])
        );
    }

    #[test]
    fn legacy_mouse_motion_and_wheel_share_the_raw_encoding() {
        assert_eq!(
            encode_mouse_motion(
                MouseFormat::Legacy,
                MouseTracking::ButtonMotion,
                Some(MouseButton::Left),
                point(4, 5),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 32, 37, 38])
        );
        assert_eq!(
            encode_mouse_wheel(
                MouseFormat::Legacy,
                MouseTracking::Press,
                1.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 64, 33, 33])
        );
    }

    #[test]
    fn legacy_mouse_drops_coordinates_past_223() {
        // Column 223 (0-based 222) is the last representable byte: 255.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(222, 0),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32, 255, 33])
        );
        // Column 224 (0-based 223) would overflow a byte: dropped.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(223, 0),
                ModifiersState::empty()
            ),
            None
        );
    }

    #[test]
    fn utf8_mouse_encodes_wide_coordinates_as_two_bytes() {
        // Coordinates that fit in one byte match the Legacy encoding.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Utf8,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32, 35, 36])
        );
        // Column 300 (0-based 299): 300 + 32 = 332 = U+014C → 0xC5 0x8C.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Utf8,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(299, 0),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32, 0xC5, 0x8C, 33])
        );
        // Column 2015 is the two-byte UTF-8 limit (2047 = U+07FF)…
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Utf8,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2014, 0),
                ModifiersState::empty()
            ),
            Some(vec![0x1b, b'[', b'M', 32, 0xDF, 0xBF, 33])
        );
        // …and column 2016 is past it: dropped.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Utf8,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2015, 0),
                ModifiersState::empty()
            ),
            None
        );
    }

    #[test]
    fn urxvt_mouse_uses_decimal_csi_with_unlimited_coordinates() {
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Urxvt,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(b"\x1b[32;3;4M".to_vec())
        );
        // Release reports button value 3 with the press final byte.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Urxvt,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Released,
                point(2, 3),
                ModifiersState::empty()
            ),
            Some(b"\x1b[35;3;4M".to_vec())
        );
        // No coordinate ceiling: columns past the legacy limits still encode.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Urxvt,
                MouseTracking::Press,
                MouseButton::Left,
                ElementState::Pressed,
                point(2999, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[32;3000;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_motion(
                MouseFormat::Urxvt,
                MouseTracking::AnyMotion,
                None,
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[67;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_wheel(
                MouseFormat::Urxvt,
                MouseTracking::Press,
                -1.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            Some(b"\x1b[97;1;1M".to_vec())
        );
    }

    #[test]
    fn wheel_routes_to_local_scroll_when_mode_does_not_report_it() {
        // X10 never reports wheels → route yields None so the caller scrolls
        // the local viewport instead of eating the event.
        assert_eq!(
            route_mouse_wheel(
                MouseTracking::X10,
                MouseFormat::Sgr,
                false,
                1.0,
                Some(point(0, 0)),
                ModifiersState::empty()
            ),
            None
        );
        // A reporting mode produces bytes and suppresses local scroll.
        assert_eq!(
            route_mouse_wheel(
                MouseTracking::Press,
                MouseFormat::Sgr,
                false,
                1.0,
                Some(point(0, 0)),
                ModifiersState::empty()
            ),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
        // Shift is a temporary override → local scroll even while tracking.
        assert_eq!(
            route_mouse_wheel(
                MouseTracking::Press,
                MouseFormat::Sgr,
                true,
                1.0,
                Some(point(0, 0)),
                ModifiersState::empty()
            ),
            None
        );
        // Tracking off and a missing cell both fall through to local scroll.
        assert_eq!(
            route_mouse_wheel(
                MouseTracking::Off,
                MouseFormat::Sgr,
                false,
                1.0,
                Some(point(0, 0)),
                ModifiersState::empty()
            ),
            None
        );
        assert_eq!(
            route_mouse_wheel(
                MouseTracking::Press,
                MouseFormat::Sgr,
                false,
                1.0,
                None,
                ModifiersState::empty()
            ),
            None
        );
    }

    #[test]
    fn x10_tracking_reports_presses_only_without_modifiers() {
        // Press: reported, and modifier bits are omitted.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::X10,
                MouseButton::Left,
                ElementState::Pressed,
                point(0, 0),
                ModifiersState::SHIFT | ModifiersState::CONTROL
            ),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        // Release, motion, and wheel are all suppressed.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Legacy,
                MouseTracking::X10,
                MouseButton::Left,
                ElementState::Released,
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
        assert_eq!(
            encode_mouse_motion(
                MouseFormat::Legacy,
                MouseTracking::X10,
                Some(MouseButton::Left),
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
        assert_eq!(
            encode_mouse_wheel(
                MouseFormat::Legacy,
                MouseTracking::X10,
                1.0,
                point(0, 0),
                ModifiersState::empty()
            ),
            None
        );
        // The format stays orthogonal: X10 presses honor SGR encoding too.
        assert_eq!(
            encode_mouse_input(
                MouseFormat::Sgr,
                MouseTracking::X10,
                MouseButton::Left,
                ElementState::Pressed,
                point(2, 3),
                ModifiersState::CONTROL
            ),
            Some(b"\x1b[<0;3;4M".to_vec())
        );
    }
}
