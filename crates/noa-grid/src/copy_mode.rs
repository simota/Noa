//! Headless keyboard copy-mode state and movement semantics.

use noa_core::CellAttrs;

use crate::{Screen, SelectionPoint, Terminal};

/// One-cell keyboard movement in copy mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Result of the two-stage Escape operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyModeCancel {
    SelectionCleared,
    Exit,
}

/// GUI-independent copy-mode cursor and anchor state.
///
/// Points use the active screen's current `scrollback + live grid` storage
/// coordinates. The active [`Screen`] mirrors these points so structural edits
/// can rebase them and record whether their rows were evicted before
/// [`Self::repair_eviction`] runs under the terminal's single lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyModeState {
    cursor: SelectionPoint,
    anchor: Option<SelectionPoint>,
    active_is_alt: bool,
    screen_generation: u64,
    coordinate_generation: u64,
}

impl CopyModeState {
    /// Enter with a cursor at the shell cursor when it is visible, otherwise
    /// at the nearest visible selectable cell. The existing viewport is
    /// preserved and explicitly locked even when its offset is zero.
    pub fn enter(terminal: &mut Terminal) -> Option<Self> {
        terminal.lock_viewport();
        terminal.clear_selection();

        let (cursor, coordinate_generation) = {
            let screen = terminal.active();
            if screen.rows == 0 || screen.cols == 0 || screen.total_rows() == 0 {
                terminal.unlock_viewport();
                return None;
            }
            (entry_point(screen), screen.coordinate_generation())
        };

        terminal.set_copy_mode_points(cursor, None);
        Some(Self {
            cursor,
            anchor: None,
            active_is_alt: terminal.active_is_alt,
            screen_generation: terminal.screen_generation(),
            coordinate_generation,
        })
    }

    pub const fn cursor(&self) -> SelectionPoint {
        self.cursor
    }

    pub const fn anchor(&self) -> Option<SelectionPoint> {
        self.anchor
    }

    pub const fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    /// Repair coordinate changes and re-assert the state-owned selection in
    /// one terminal critical section. If the active coordinate space is
    /// replaced or collapsed, restart at the visible shell cursor instead of
    /// applying points from the previous space.
    pub fn repair_eviction(&mut self, terminal: &mut Terminal) {
        if self.active_is_alt != terminal.active_is_alt
            || self.screen_generation != terminal.screen_generation()
            || self.coordinate_generation != terminal.active().coordinate_generation()
        {
            self.restart_on_active_screen(terminal);
            return;
        }

        let Some(tracked) = terminal.copy_mode_points() else {
            self.restart_on_active_screen(terminal);
            return;
        };
        self.cursor = tracked.cursor;
        self.anchor = tracked.anchor;

        let screen = terminal.active();
        self.cursor = if tracked.cursor_was_evicted {
            clamp_vertical_point(screen, self.cursor)
        } else {
            normalize_grid_point(screen, self.cursor)
        };
        self.anchor = self.anchor.map(|point| {
            if tracked.anchor_was_evicted {
                clamp_vertical_point(screen, point)
            } else {
                normalize_grid_point(screen, point)
            }
        });
        self.cursor = clamp_to_visible_rows(screen, self.cursor);
        terminal.set_copy_mode_points(self.cursor, self.anchor);

        match self.anchor {
            Some(anchor) => terminal.set_selection(anchor, self.cursor),
            None => terminal.clear_selection(),
        }
    }

    fn restart_on_active_screen(&mut self, terminal: &mut Terminal) {
        terminal.lock_viewport();
        let (cursor, coordinate_generation) = {
            let screen = terminal.active();
            (entry_point(screen), screen.coordinate_generation())
        };
        self.cursor = cursor;
        self.anchor = None;
        self.active_is_alt = terminal.active_is_alt;
        self.screen_generation = terminal.screen_generation();
        self.coordinate_generation = coordinate_generation;
        terminal.set_copy_mode_points(self.cursor, self.anchor);
        terminal.clear_selection();
    }

    /// Move by one semantic cell. Extending starts an inclusive selection at
    /// the old cursor; non-extending movement clears the selection first.
    /// Returns whether the cursor moved.
    pub fn move_cursor(
        &mut self,
        terminal: &mut Terminal,
        direction: CopyDirection,
        extend: bool,
    ) -> bool {
        self.repair_eviction(terminal);
        if !extend {
            self.anchor = None;
            terminal.clear_selection();
        }

        let old_cursor = self.cursor;
        let (next, viewport_scroll) = movement(terminal.active(), self.cursor, direction);
        let Some(next) = next else {
            terminal.set_copy_mode_points(self.cursor, self.anchor);
            return false;
        };

        match viewport_scroll {
            ViewportScroll::None => {}
            ViewportScroll::Up => terminal.scroll_viewport_up(1),
            ViewportScroll::Down => terminal.scroll_viewport_down(1),
        }

        self.cursor = next;
        if extend {
            let anchor = *self.anchor.get_or_insert(old_cursor);
            terminal.set_selection(anchor, self.cursor);
        }
        terminal.set_copy_mode_points(self.cursor, self.anchor);
        true
    }

    /// Implement Escape's clear-then-exit behavior without ending the mode.
    pub fn cancel(&mut self, terminal: &mut Terminal) -> CopyModeCancel {
        self.repair_eviction(terminal);
        if self.anchor.take().is_some() {
            terminal.clear_selection();
            terminal.set_copy_mode_points(self.cursor, self.anchor);
            CopyModeCancel::SelectionCleared
        } else {
            CopyModeCancel::Exit
        }
    }
}

fn clamp_to_visible_rows(screen: &Screen, point: SelectionPoint) -> SelectionPoint {
    let visible_top = screen.visible_row_base();
    let visible_bottom = visible_top
        .saturating_add(screen.rows as usize)
        .saturating_sub(1)
        .min(screen.total_rows().saturating_sub(1));
    let y = point.y.clamp(visible_top, visible_bottom);
    if y == point.y {
        point
    } else {
        clamp_vertical_point(screen, SelectionPoint::new(point.x, y))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewportScroll {
    None,
    Up,
    Down,
}

fn movement(
    screen: &Screen,
    cursor: SelectionPoint,
    direction: CopyDirection,
) -> (Option<SelectionPoint>, ViewportScroll) {
    let cursor = normalize_grid_point(screen, cursor);
    let visible_top = screen.visible_row_base();
    let visible_bottom = visible_top
        .saturating_add(screen.rows as usize)
        .saturating_sub(1)
        .min(screen.total_rows().saturating_sub(1));

    match direction {
        CopyDirection::Left => (
            previous_selectable_x(screen, cursor).map(|x| SelectionPoint::new(x, cursor.y)),
            ViewportScroll::None,
        ),
        CopyDirection::Right => (
            next_selectable_x(screen, cursor).map(|x| SelectionPoint::new(x, cursor.y)),
            ViewportScroll::None,
        ),
        CopyDirection::Up => {
            if cursor.y > visible_top {
                let next = SelectionPoint::new(cursor.x, cursor.y - 1);
                (
                    Some(clamp_vertical_point(screen, next)),
                    ViewportScroll::None,
                )
            } else if visible_top > 0 {
                let next = SelectionPoint::new(cursor.x, cursor.y - 1);
                (Some(clamp_vertical_point(screen, next)), ViewportScroll::Up)
            } else {
                (None, ViewportScroll::None)
            }
        }
        CopyDirection::Down => {
            if cursor.y < visible_bottom {
                let next = SelectionPoint::new(cursor.x, cursor.y + 1);
                (
                    Some(clamp_vertical_point(screen, next)),
                    ViewportScroll::None,
                )
            } else if screen.viewport_offset() > 0 && cursor.y + 1 < screen.total_rows() {
                let next = SelectionPoint::new(cursor.x, cursor.y + 1);
                (
                    Some(clamp_vertical_point(screen, next)),
                    ViewportScroll::Down,
                )
            } else {
                (None, ViewportScroll::None)
            }
        }
    }
}

fn entry_point(screen: &Screen) -> SelectionPoint {
    let visible_top = screen.visible_row_base();
    let visible_bottom = visible_top
        .saturating_add(screen.rows as usize)
        .saturating_sub(1)
        .min(screen.total_rows().saturating_sub(1));
    let shell_y = screen.scrollback_len() + usize::from(screen.cursor.y);
    let y = shell_y.clamp(visible_top, visible_bottom);
    let point = SelectionPoint::new(screen.cursor.x, y);
    if y == shell_y {
        normalize_grid_point(screen, point)
    } else {
        clamp_vertical_point(screen, point)
    }
}

fn clamp_vertical_point(screen: &Screen, point: SelectionPoint) -> SelectionPoint {
    let y = point.y.min(screen.total_rows().saturating_sub(1));
    normalize_grid_point(
        screen,
        SelectionPoint::new(point.x.min(semantic_row_end(screen, y)), y),
    )
}

fn normalize_grid_point(screen: &Screen, point: SelectionPoint) -> SelectionPoint {
    let y = point.y.min(screen.total_rows().saturating_sub(1));
    let mut x = point.x.min(screen.cols.saturating_sub(1));
    if x > 0
        && screen.absolute_row(y).is_some_and(|row| {
            row.cells
                .get(usize::from(x))
                .is_some_and(|cell| cell.attrs.contains(CellAttrs::WIDE_SPACER))
        })
    {
        x -= 1;
    }
    SelectionPoint::new(x, y)
}

fn semantic_row_end(screen: &Screen, y: usize) -> u16 {
    let Some(row) = screen.absolute_row(y) else {
        return 0;
    };
    row.cells
        .iter()
        .enumerate()
        .rev()
        .find(|(_, cell)| !cell.is_blank() && !cell.attrs.contains(CellAttrs::WIDE_SPACER))
        .map_or(0, |(x, _)| x as u16)
}

fn previous_selectable_x(screen: &Screen, cursor: SelectionPoint) -> Option<u16> {
    let row = screen.absolute_row(cursor.y)?;
    let mut x = cursor.x.checked_sub(1)? as usize;
    while row
        .cells
        .get(x)
        .is_some_and(|cell| cell.attrs.contains(CellAttrs::WIDE_SPACER))
    {
        x = x.checked_sub(1)?;
    }
    Some(x as u16)
}

fn next_selectable_x(screen: &Screen, cursor: SelectionPoint) -> Option<u16> {
    let row = screen.absolute_row(cursor.y)?;
    let mut x = usize::from(cursor.x).checked_add(1)?;
    while x < row.cells.len()
        && row
            .cells
            .get(x)
            .is_some_and(|cell| cell.attrs.contains(CellAttrs::WIDE_SPACER))
    {
        x += 1;
    }
    (x < row.cells.len()).then_some(x as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::{GridSize, Point};

    fn terminal(cols: u16, rows: u16) -> Terminal {
        Terminal::new(GridSize::new(cols, rows))
    }

    fn set_row(term: &mut Terminal, y: usize, text: &str) {
        for (x, ch) in text.chars().enumerate() {
            term.primary.grid[y].cells[x].ch = ch;
        }
        // Direct cell pokes bypass the print path's occupancy tracking.
        term.primary.grid[y].mark_occupied(text.chars().count());
    }

    fn push_history_row(term: &mut Terminal, text: &str) {
        set_row(term, 0, text);
        let rows = term.primary.rows;
        term.primary.region.bottom = rows.saturating_sub(1);
        term.primary.scroll_up_region(1);
    }

    #[test]
    fn first_extended_move_creates_an_inclusive_two_cell_selection() {
        let mut term = terminal(5, 2);
        set_row(&mut term, 0, "abcd");
        term.primary.cursor.x = 1;

        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));

        assert_eq!(state.cursor(), SelectionPoint::new(2, 0));
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                SelectionPoint::new(1, 0),
                SelectionPoint::new(2, 0)
            ))
        );
    }

    #[test]
    fn entry_and_redraw_preserve_a_visible_shell_cursor_in_trailing_blanks() {
        let mut term = terminal(6, 2);
        set_row(&mut term, 0, "abc");
        term.primary.cursor.x = 3;

        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        state.repair_eviction(&mut term);
        assert_eq!(state.cursor(), SelectionPoint::new(3, 0));

        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        assert_eq!(state.cursor(), SelectionPoint::new(4, 0));
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                SelectionPoint::new(3, 0),
                SelectionPoint::new(4, 0)
            ))
        );

        term.exit_copy_mode();
        let mut state = CopyModeState::enter(&mut term).expect("copy mode re-enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Left, true));
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                SelectionPoint::new(3, 0),
                SelectionPoint::new(2, 0)
            ))
        );
    }

    #[test]
    fn unmodified_move_clears_selection_before_moving() {
        let mut term = terminal(5, 2);
        set_row(&mut term, 0, "abcd");
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        state.move_cursor(&mut term, CopyDirection::Right, true);

        assert!(state.move_cursor(&mut term, CopyDirection::Right, false));
        assert_eq!(state.cursor(), SelectionPoint::new(2, 0));
        assert_eq!(state.anchor(), None);
        assert_eq!(term.active().selection, None);
    }

    #[test]
    fn vertical_move_clamps_to_semantic_row_end_without_sticky_column() {
        let mut term = terminal(8, 3);
        set_row(&mut term, 0, "abcdef");
        set_row(&mut term, 1, "xy");
        set_row(&mut term, 2, "abcdef");
        term.primary.cursor.x = 5;

        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Down, false));
        assert_eq!(state.cursor(), SelectionPoint::new(1, 1));
        assert!(state.move_cursor(&mut term, CopyDirection::Down, false));
        assert_eq!(state.cursor(), SelectionPoint::new(1, 2));
    }

    #[test]
    fn vertical_move_normalizes_a_wide_spacer_to_its_lead_cell() {
        let mut term = terminal(6, 2);
        set_row(&mut term, 0, "abcdef");
        term.primary.grid[1].cells[2].ch = '界';
        term.primary.grid[1].cells[2].attrs.insert(CellAttrs::WIDE);
        term.primary.grid[1].cells[3]
            .attrs
            .insert(CellAttrs::WIDE_SPACER);
        term.primary.grid[1].cells[4].ch = 'z';
        term.primary.grid[1].mark_occupied(5);
        term.primary.cursor.x = 3;

        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Down, false));

        assert_eq!(state.cursor(), SelectionPoint::new(2, 1));
    }

    #[test]
    fn edge_move_scrolls_one_row_and_oldest_boundary_is_a_noop() {
        let mut term = terminal(4, 2);
        push_history_row(&mut term, "one");
        push_history_row(&mut term, "two");
        term.scroll_viewport_to_top();
        term.primary.cursor.y = 0;

        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert_eq!(state.cursor().y, 1);
        assert!(state.move_cursor(&mut term, CopyDirection::Down, false));
        assert_eq!(state.cursor().y, 2);
        assert_eq!(term.viewport_offset(), 1);

        term.scroll_viewport_to_top();
        state.cursor = SelectionPoint::new(0, 0);
        term.set_copy_mode_points(state.cursor, state.anchor);
        state.repair_eviction(&mut term);
        let before = (state.cursor(), term.viewport_offset());
        assert!(!state.move_cursor(&mut term, CopyDirection::Up, true));
        assert_eq!((state.cursor(), term.viewport_offset()), before);
    }

    #[test]
    fn live_bottom_edge_move_is_a_noop() {
        let mut term = terminal(4, 2);
        set_row(&mut term, 1, "last");
        term.primary.cursor.y = 1;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        let before = (
            state.cursor(),
            term.viewport_offset(),
            term.active().selection,
        );

        assert!(!state.move_cursor(&mut term, CopyDirection::Down, true));

        assert_eq!(
            (
                state.cursor(),
                term.viewport_offset(),
                term.active().selection
            ),
            before
        );
    }

    #[test]
    fn cancel_clears_selection_then_requests_exit() {
        let mut term = terminal(4, 2);
        set_row(&mut term, 0, "text");
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));

        assert_eq!(state.cancel(&mut term), CopyModeCancel::SelectionCleared);
        assert!(!state.has_selection());
        assert_eq!(term.active().selection, None);
        assert_eq!(state.cancel(&mut term), CopyModeCancel::Exit);
    }

    #[test]
    fn entry_preserves_scrollback_and_clamps_an_offscreen_shell_cursor() {
        let mut term = terminal(6, 2);
        for text in ["one", "two", "three"] {
            push_history_row(&mut term, text);
        }
        term.scroll_viewport_to_top();
        let offset = term.viewport_offset();
        term.primary.cursor.x = 5;
        term.primary.cursor.y = 1;

        let state = CopyModeState::enter(&mut term).expect("copy mode enters");

        assert_eq!(term.viewport_offset(), offset);
        assert_eq!(state.cursor().y, 1);
        assert_eq!(state.cursor().x, 2);
    }

    #[test]
    fn explicit_viewport_lock_pins_output_even_from_live_offset_zero() {
        let mut term = terminal(4, 2);
        set_row(&mut term, 0, "one");
        let _state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert_eq!(term.viewport_offset(), 0);

        term.primary.scroll_up_region(1);

        assert_eq!(term.viewport_offset(), 1);
        assert_eq!(term.active().visible_row(0).expect("row").cells[0].ch, 'o');
    }

    #[test]
    fn repair_keeps_cursor_visible_after_external_viewport_changes() {
        let mut term = terminal(5, 2);
        for text in ["one", "two", "three"] {
            push_history_row(&mut term, text);
        }
        set_row(&mut term, 0, "live");
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));

        term.scroll_viewport_to_top();
        state.repair_eviction(&mut term);
        let top = term.active().visible_row_base();
        let bottom = top + usize::from(term.size.rows) - 1;
        assert!((top..=bottom).contains(&state.cursor().y));
        assert_eq!(state.cursor().y, bottom);
        assert_eq!(
            term.active().selection.expect("selection").focus,
            state.cursor()
        );

        term.scroll_viewport_to_bottom();
        state.repair_eviction(&mut term);
        let top = term.active().visible_row_base();
        let bottom = top + usize::from(term.size.rows) - 1;
        assert!((top..=bottom).contains(&state.cursor().y));
        assert_eq!(state.cursor().y, top);
    }

    #[test]
    fn partial_top_region_scroll_preserves_fixed_copy_points_and_viewport() {
        let mut term = terminal(6, 3);
        set_row(&mut term, 0, "top");
        set_row(&mut term, 1, "move");
        set_row(&mut term, 2, "fixed");
        term.primary.cursor.x = 0;
        term.primary.cursor.y = 2;
        term.primary.region.bottom = 1;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        assert_eq!(term.selected_text().as_deref(), Some("fi"));

        term.primary.scroll_up_region(1);
        assert_eq!(term.viewport_offset(), 0);
        assert_eq!(
            term.active().visible_row(2).expect("fixed row").cells[0].ch,
            'f'
        );

        state.repair_eviction(&mut term);

        assert_eq!(state.anchor(), Some(SelectionPoint::new(0, 3)));
        assert_eq!(state.cursor(), SelectionPoint::new(1, 3));
        assert_eq!(term.selected_text().as_deref(), Some("fi"));
    }

    #[test]
    fn eviction_uses_structurally_shifted_copy_points() {
        let mut term = terminal(6, 3);
        set_row(&mut term, 2, "f");
        term.primary.cursor.x = 4;
        term.primary.cursor.y = 2;
        term.primary.region.bottom = 1;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        assert_eq!(state.anchor(), Some(SelectionPoint::new(4, 2)));
        assert_eq!(state.cursor(), SelectionPoint::new(5, 2));

        for text in ["one", "two", "three"] {
            set_row(&mut term, 0, text);
            term.primary.scroll_up_region(1);
        }
        let tracked = term.copy_mode_points().expect("tracked copy points");
        assert_eq!(tracked.anchor, Some(SelectionPoint::new(4, 5)));
        assert_eq!(tracked.cursor, SelectionPoint::new(5, 5));

        let evicted_before = term.selection_rows_evicted();
        term.primary.set_scrollback_limit_bytes(0);
        assert_eq!(term.selection_rows_evicted() - evicted_before, 3);
        let tracked = term.copy_mode_points().expect("rebased copy points");
        assert_eq!(tracked.anchor, Some(SelectionPoint::new(4, 2)));
        assert_eq!(tracked.cursor, SelectionPoint::new(5, 2));
        assert!(!tracked.anchor_was_evicted);
        assert!(!tracked.cursor_was_evicted);

        state.repair_eviction(&mut term);

        assert_eq!(state.anchor(), Some(SelectionPoint::new(4, 2)));
        assert_eq!(state.cursor(), SelectionPoint::new(5, 2));
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                SelectionPoint::new(4, 2),
                SelectionPoint::new(5, 2)
            ))
        );
    }

    #[test]
    fn partial_scroll_eviction_preserves_fixed_copy_points() {
        let mut term = terminal(4096, 3);
        term.primary.set_scrollback_limit_bytes(1);
        for ch in ['a', 'b'] {
            for cell in &mut term.primary.grid[0].cells {
                cell.ch = ch;
            }
            term.primary.grid[0].mark_all();
            term.primary.scroll_up_region(1);
        }
        assert_eq!(term.scrollback_len(), 2);

        set_row(&mut term, 2, "fixed!");
        term.primary.cursor.x = 4;
        term.primary.cursor.y = 2;
        term.primary.region.bottom = 1;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        assert_eq!(term.selected_text().as_deref(), Some("d!"));

        for cell in &mut term.primary.grid[0].cells {
            cell.ch = 'c';
        }
        term.primary.grid[0].mark_all();
        let generation = term.active().coordinate_generation();
        let evicted_before = term.selection_rows_evicted();
        term.primary.scroll_up_region(1);

        assert_eq!(term.selection_rows_evicted() - evicted_before, 2);
        assert_eq!(term.active().coordinate_generation(), generation);
        let tracked = term.copy_mode_points().expect("tracked copy points");
        assert_eq!(tracked.anchor, Some(SelectionPoint::new(4, 3)));
        assert_eq!(tracked.cursor, SelectionPoint::new(5, 3));
        assert!(!tracked.anchor_was_evicted);
        assert!(!tracked.cursor_was_evicted);
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                SelectionPoint::new(4, 3),
                SelectionPoint::new(5, 3)
            ))
        );

        state.repair_eviction(&mut term);

        assert_eq!(state.anchor(), Some(SelectionPoint::new(4, 3)));
        assert_eq!(state.cursor(), SelectionPoint::new(5, 3));
        assert_eq!(term.selected_text().as_deref(), Some("d!"));
    }

    #[test]
    fn erase_scrollback_restarts_copy_state_in_the_shrunken_coordinate_space() {
        let mut term = terminal(6, 2);
        push_history_row(&mut term, "old");
        push_history_row(&mut term, "past");
        set_row(&mut term, 0, "live");
        term.primary.cursor.x = 0;
        term.primary.cursor.y = 0;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        assert_eq!(term.selected_text().as_deref(), Some("li"));
        let generation = term.active().coordinate_generation();

        noa_vt::Stream::new().feed(b"\x1b[3J", &mut term);
        assert_ne!(term.active().coordinate_generation(), generation);
        assert_eq!(term.scrollback_len(), 0);

        state.repair_eviction(&mut term);

        assert_eq!(state.cursor(), SelectionPoint::new(0, 0));
        assert_eq!(state.anchor(), None);
        assert_eq!(term.selected_text(), None);
        assert!(term.primary.viewport_locked());
    }

    #[test]
    fn full_reset_restarts_copy_state_and_relocks_the_new_primary_screen() {
        let mut term = terminal(6, 2);
        set_row(&mut term, 0, "abcd");
        term.primary.cursor.x = 1;
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        let generation = term.screen_generation();

        noa_vt::Stream::new().feed(b"\x1bc", &mut term);
        assert_ne!(term.screen_generation(), generation);
        assert!(!term.primary.viewport_locked());

        state.repair_eviction(&mut term);

        assert_eq!(state.cursor(), SelectionPoint::new(0, 0));
        assert_eq!(state.anchor(), None);
        assert_eq!(term.active().selection, None);
        assert!(term.primary.viewport_locked());
    }

    #[test]
    fn eviction_repair_clamps_evicted_cursor_and_anchor_to_oldest_row() {
        let mut term = terminal(4, 2);
        push_history_row(&mut term, "one");
        push_history_row(&mut term, "two");
        term.scroll_viewport_to_top();
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        state.cursor = SelectionPoint::new(2, 0);
        state.anchor = Some(SelectionPoint::new(0, 0));
        term.set_copy_mode_points(state.cursor, state.anchor);
        state.repair_eviction(&mut term);

        term.primary.set_scrollback_limit_bytes(0);
        state.repair_eviction(&mut term);

        assert_eq!(state.cursor().y, 0);
        assert_eq!(state.anchor().expect("anchor").y, 0);
        assert_eq!(term.active().selection.expect("selection").anchor.y, 0);
    }

    #[test]
    fn eviction_repair_preserves_surviving_logical_cursor_and_anchor_rows() {
        let mut term = terminal(80, 3);
        let full_row = "x".repeat(80);
        for _ in 0..400 {
            push_history_row(&mut term, &full_row);
        }
        set_row(&mut term, 0, &full_row);
        let mut state = CopyModeState::enter(&mut term).expect("copy mode enters");
        assert!(state.move_cursor(&mut term, CopyDirection::Right, true));
        let cursor_before = state.cursor();
        let anchor_before = state.anchor().expect("selection anchor");
        let evicted_before = term.selection_rows_evicted();

        term.primary.set_scrollback_limit_bytes(1);
        let evicted = term.selection_rows_evicted() - evicted_before;
        assert!(evicted > 0, "shrinking scrollback must evict old pages");
        state.repair_eviction(&mut term);

        assert_eq!(state.cursor().y, cursor_before.y - evicted);
        assert_eq!(
            state.anchor().expect("surviving anchor").y,
            anchor_before.y - evicted
        );
        assert_eq!(
            term.active().selection,
            Some(crate::Selection::new(
                state.anchor().expect("selection anchor"),
                state.cursor()
            ))
        );
        assert_eq!(semantic_row_end(term.active(), state.cursor().y), 79);
        assert!(state.move_cursor(&mut term, CopyDirection::Left, false));
        assert_eq!(state.cursor().x, cursor_before.x - 1);
    }

    #[test]
    fn empty_trailing_blank_and_wide_rows_have_semantic_endpoints() {
        let mut term = terminal(6, 3);
        set_row(&mut term, 0, "ab");
        term.primary.grid[1].cells[2].ch = '界';
        term.primary.grid[1].cells[2].attrs.insert(CellAttrs::WIDE);
        term.primary.grid[1].cells[3]
            .attrs
            .insert(CellAttrs::WIDE_SPACER);

        assert_eq!(semantic_row_end(&term.primary, 0), 1);
        assert_eq!(semantic_row_end(&term.primary, 1), 2);
        assert_eq!(semantic_row_end(&term.primary, 2), 0);
        assert_eq!(
            term.viewport_point_to_selection_point(Point { x: 0, y: 0 }),
            SelectionPoint::new(0, 0)
        );
    }

    #[test]
    fn exit_clears_selection_lock_and_offset_on_primary_and_alt() {
        let mut term = terminal(4, 2);
        push_history_row(&mut term, "one");
        push_history_row(&mut term, "two");
        term.primary.scroll_viewport_to_top();
        term.primary.set_viewport_locked(true);
        term.primary
            .set_selection(SelectionPoint::new(0, 0), SelectionPoint::new(1, 0));

        let mut stream = noa_vt::Stream::new();
        stream.feed(b"\x1b[?1049h", &mut term);
        term.lock_viewport();
        term.set_selection(SelectionPoint::new(0, 0), SelectionPoint::new(1, 0));

        term.exit_copy_mode();

        assert_eq!(term.primary.selection, None);
        assert_eq!(term.primary.viewport_offset(), 0);
        assert!(!term.primary.viewport_locked());
        let alt = term.alt.as_ref().expect("alternate screen exists");
        assert_eq!(alt.selection, None);
        assert_eq!(alt.viewport_offset(), 0);
        assert!(!alt.viewport_locked());
    }
}
