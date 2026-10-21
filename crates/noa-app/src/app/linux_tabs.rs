//! In-window tabs off macOS.
//!
//! macOS tabs are native: every tab is its own `NSWindow` and AppKit's
//! tabbing groups them into one frame. winit offers nothing comparable
//! elsewhere, so off macOS the tabs of a logical window (a `WindowState`
//! group) share a single OS window and its wgpu surface — Ghostty's GTK
//! (libadwaita `TabView`) shape. Each tab keeps its own `WindowState` under a
//! synthetic `WindowId`; `tab_hosts` maps the OS window to the tab in front,
//! and that tab paints a tab bar across the top once there are two or more.
//!
//! Tabs behind the front one reuse the occlusion machinery (`occluded`: no
//! drawing, background pane-cache refresh, reveal fast path) and also set
//! `background_tab`, because unlike a genuinely occluded window they do not
//! own the surface they hold and must never (re)configure it.

use super::*;

/// Synthetic tab ids live far above anything a windowing backend hands out
/// (X11 XIDs are 29-bit, Wayland ids are small pointers-as-ints in winit).
const SYNTHETIC_TAB_ID_BASE: u64 = 0xFFFF_0000_0000_0000;

/// Minimum segment width (columns) that still fits a close mark.
const MIN_CLOSABLE_SEGMENT_COLS: u16 = 6;
/// Trailing columns of a closable segment: space, `×`, space.
const CLOSE_ZONE_COLS: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabBarHit {
    Select(WindowId),
    Close(WindowId),
}

impl App {
    pub(super) fn mint_tab_window_id(&mut self) -> WindowId {
        self.next_tab_id += 1;
        WindowId::from(SYNTHETIC_TAB_ID_BASE + self.next_tab_id)
    }

    /// The OS window, surface and surface configuration a new tab joining
    /// `group` shares. Always `None` on macOS, where every tab is its own
    /// native window.
    pub(super) fn shared_tab_host(
        &self,
        group: WindowGroupId,
    ) -> Option<(
        Arc<Window>,
        Arc<wgpu::Surface<'static>>,
        wgpu::SurfaceConfiguration,
    )> {
        if cfg!(target_os = "macos") {
            return None;
        }
        let host = self
            .window_order
            .iter()
            .filter_map(|id| self.windows.get(id))
            .find(|state| state.group == group && !state.background_tab)?;
        Some((
            host.window.clone(),
            host.surface.clone(),
            host.surface_config.clone(),
        ))
    }

    /// Every tab sharing `tab`'s OS window, in tab order (`tab` included).
    /// On macOS a tab is its own window, so that is just `tab`.
    pub(super) fn tab_siblings(&self, tab: WindowId) -> Vec<WindowId> {
        let Some(group) = self.windows.get(&tab).map(|state| state.group) else {
            return Vec::new();
        };
        if cfg!(target_os = "macos") {
            return vec![tab];
        }
        ids_in_group(
            &self.window_order,
            |id| self.windows.get(&id).map(|state| state.group),
            group,
        )
    }

    pub(super) fn relayout_tab_siblings(&mut self, tab: WindowId) {
        for sibling in self.tab_siblings(tab) {
            self.relayout_and_resize_window(sibling);
        }
    }

    /// Bring `tab` to the front of its shared OS window. The tab it replaces
    /// becomes a background tab and loses (terminal) focus; `tab` inherits
    /// the shared surface's configuration and, when the OS window has focus,
    /// the input focus.
    pub(super) fn activate_tab(&mut self, tab: WindowId) {
        let Some(os_window) = self.windows.get(&tab).map(|state| state.window.id()) else {
            return;
        };
        let prev = self.tab_hosts.get(&os_window).copied();
        if prev == Some(tab) {
            return;
        }
        let was_os_focused = prev.is_some_and(|prev| self.os_focused == Some(prev));
        let mut inherited = None;
        if let Some(prev) = prev {
            self.end_copy_mode_for_window(prev);
            self.cancel_mouse_capture(prev);
            self.finish_active_split_drag(prev);
            if was_os_focused {
                self.report_focus_event(prev, false);
            }
            if let Some(state) = self.windows.get_mut(&prev) {
                inherited = Some((
                    state.occluded,
                    state.surface_config.clone(),
                    state.modifiers,
                    std::mem::take(&mut state.key_modifiers),
                    // The pointer stays where it was; the incoming tab must
                    // see it without waiting for the next motion event.
                    state.last_mouse_point.take(),
                    state.last_mouse_physical_position.take(),
                ));
                state.background_tab = true;
                state.occluded = true;
                state.modifiers = ModifiersState::empty();
                // Same stash invalidation as a real `Occluded(true)`: a
                // reveal snapshot that never got its follow-up redraw already
                // consumed the terminal's row damage.
                for (&pane_id, surface) in state.surfaces.iter_mut() {
                    if surface.pending_reveal_snapshot.take().is_some() {
                        state.renderer.invalidate_pane(render_pane_id(pane_id));
                    }
                }
            }
        }
        let Some(state) = self.windows.get_mut(&tab) else {
            return;
        };
        state.background_tab = false;
        if let Some((
            occluded,
            surface_config,
            modifiers,
            key_modifiers,
            mouse_point,
            mouse_physical_position,
        )) = inherited
        {
            state.occluded = occluded;
            state.surface_config = surface_config;
            state.modifiers = modifiers;
            state.key_modifiers = key_modifiers;
            state.last_mouse_point = mouse_point;
            state.last_mouse_physical_position = mouse_physical_position;
        }
        state.reveal_fast_path_pending = !state.occluded;
        // Force the OS window title to re-resolve from this tab.
        state.title.clear();
        self.tab_hosts.insert(os_window, tab);
        self.dirty_occluded_windows.remove(&tab);
        self.sync_current_background_image_to_window(tab);
        if prev.is_none() || self.focused == prev {
            self.focused = Some(tab);
        }
        if was_os_focused {
            self.os_focused = Some(tab);
        }
        self.relayout_and_resize_window(tab);
        if was_os_focused {
            self.reset_cursor_blink_phase();
            self.report_focus_event(tab, true);
        }
        self.request_window_redraw(tab);
    }

    /// Bring `tab` to the front of its OS window (off macOS) and give that
    /// window OS focus.
    pub(super) fn focus_tab(&mut self, tab: WindowId) {
        if self
            .windows
            .get(&tab)
            .is_some_and(|state| state.background_tab)
        {
            self.activate_tab(tab);
        }
        self.focused = Some(tab);
        if let Some(state) = self.windows.get(&tab) {
            state.window.focus_window();
        }
    }

    /// Height of the in-window tab bar `tab` paints: zero on macOS and while
    /// the OS window holds a single tab.
    pub(super) fn tab_bar_height_px(&self, tab: WindowId) -> u32 {
        #[cfg(target_os = "macos")]
        {
            let _ = tab;
            0
        }
        #[cfg(not(target_os = "macos"))]
        {
            if self.tab_siblings(tab).len() < 2 {
                return 0;
            }
            let (Some(gpu), Some(state)) = (self.gpu.as_ref(), self.windows.get(&tab)) else {
                return 0;
            };
            sidebar::tab_bar_geometry(gpu.fonts.get(state.font_px).metrics()).1
        }
    }

    /// Titles of `tab`'s siblings plus the index of `tab` among them, for the
    /// tab bar `tab` paints; `None` while there is no bar.
    pub(super) fn tab_bar_model(&self, tab: WindowId) -> Option<(Vec<String>, usize)> {
        let siblings = self.tab_siblings(tab);
        if cfg!(target_os = "macos") || siblings.len() < 2 {
            return None;
        }
        let active = siblings.iter().position(|id| *id == tab)?;
        let titles = siblings
            .iter()
            .map(|id| {
                self.windows
                    .get(id)
                    .map(|state| state.title.as_str())
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Noa")
                    .to_string()
            })
            .collect();
        Some((titles, active))
    }

    /// What a pointer press at `point` (window pixels) inside `tab`'s tab
    /// bar targets, if it lands on the bar at all.
    pub(super) fn tab_bar_hit(&self, tab: WindowId, point: split_tree::Point) -> Option<TabBarHit> {
        #[cfg(target_os = "macos")]
        {
            let _ = (tab, point);
            None
        }
        #[cfg(not(target_os = "macos"))]
        {
            let height = self.tab_bar_height_px(tab);
            let x0 = self.window_sidebar_inset_px(tab);
            if point.y >= height || point.x < x0 {
                return None;
            }
            let gpu = self.gpu.as_ref()?;
            let state = self.windows.get(&tab)?;
            let metrics = gpu.fonts.get(state.font_px).metrics();
            let (pad_x, _) = sidebar::tab_bar_geometry(metrics);
            let width = state.surface_config.width.saturating_sub(x0);
            let cols = tab_bar_cols(width, pad_x, metrics.cell_w);
            let rel = (point.x - x0).checked_sub(pad_x)?;
            let col = (rel as f32 / metrics.cell_w) as u16;
            let siblings = self.tab_siblings(tab);
            Some(match tab_bar_hit_at(siblings.len(), cols, col)? {
                SegmentHit::Select(index) => TabBarHit::Select(siblings[index]),
                SegmentHit::Close(index) => TabBarHit::Close(siblings[index]),
            })
        }
    }
}

/// Columns of tab bar text that fit `width_px` after the side padding.
pub(super) fn tab_bar_cols(width_px: u32, pad_x: u32, cell_w: f32) -> u16 {
    let usable = width_px.saturating_sub(2 * pad_x) as f32;
    ((usable / cell_w).floor().max(1.0) as usize).min(u16::MAX as usize) as u16
}

/// `(start, width)` of each of `count` equal tab segments across `cols`
/// columns; the remainder goes one column each to the leading segments.
fn tab_segments(count: usize, cols: u16) -> Vec<(u16, u16)> {
    if count == 0 {
        return Vec::new();
    }
    let count_u16 = count.min(cols.max(1) as usize) as u16;
    let base = cols / count_u16;
    let extra = cols % count_u16;
    let mut start = 0;
    (0..count)
        .map(|i| {
            let i = i as u16;
            let width = if i < count_u16 {
                base + u16::from(i < extra)
            } else {
                0
            };
            let segment = (start, width);
            start += width;
            segment
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SegmentHit {
    Select(usize),
    Close(usize),
}

fn tab_bar_hit_at(count: usize, cols: u16, col: u16) -> Option<SegmentHit> {
    tab_segments(count, cols)
        .into_iter()
        .enumerate()
        .find(|(_, (start, width))| col >= *start && col < start + width)
        .map(|(index, (start, width))| {
            if width >= MIN_CLOSABLE_SEGMENT_COLS && col >= start + width - CLOSE_ZONE_COLS {
                SegmentHit::Close(index)
            } else {
                SegmentHit::Select(index)
            }
        })
}

/// `title` centered in exactly `width` display columns, truncated with `…`
/// when it does not fit. Control characters are dropped: the label is fed
/// through a VT stream.
fn centered_label(title: &str, width: u16) -> String {
    use unicode_width::UnicodeWidthChar;
    let width = width as usize;
    let chars: Vec<(char, usize)> = title
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| (c, c.width().unwrap_or(0)))
        .collect();
    let full: usize = chars.iter().map(|(_, w)| w).sum();
    let mut text = String::new();
    let mut used = 0;
    if full <= width {
        text.extend(chars.iter().map(|(c, _)| c));
        used = full;
    } else if width > 0 {
        for (c, w) in &chars {
            if used + w > width - 1 {
                break;
            }
            text.push(*c);
            used += w;
        }
        text.push('…');
        used += 1;
    }
    let left = (width - used) / 2;
    let right = width - used - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

/// One tab's segment text, exactly `width` columns: the centered title and,
/// when it fits, a trailing close mark.
fn segment_label(title: &str, width: u16) -> String {
    if width >= MIN_CLOSABLE_SEGMENT_COLS {
        format!(" {} × ", centered_label(title, width - 1 - CLOSE_ZONE_COLS))
    } else {
        centered_label(title, width)
    }
}

/// Each tab's segment text across a `cols`-wide bar, in tab order.
pub(super) fn tab_bar_labels(titles: &[String], cols: u16) -> Vec<String> {
    tab_segments(titles.len(), cols)
        .into_iter()
        .zip(titles)
        .map(|((_, width), title)| segment_label(title, width))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn segments_split_evenly_with_leading_remainder() {
        assert_eq!(tab_segments(3, 20), vec![(0, 7), (7, 7), (14, 6)]);
        assert_eq!(tab_segments(2, 10), vec![(0, 5), (5, 5)]);
        assert!(tab_segments(0, 10).is_empty());
    }

    #[test]
    fn more_tabs_than_columns_leaves_trailing_segments_empty() {
        assert_eq!(tab_segments(3, 2), vec![(0, 1), (1, 1), (2, 0)]);
    }

    #[test]
    fn labels_fill_their_segment_exactly() {
        let titles = vec!["zsh".to_string(), "日本語のとても長いタイトル".to_string()];
        let labels = tab_bar_labels(&titles, 31);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].width(), 16);
        assert_eq!(labels[1].width(), 15);
        assert!(labels[0].ends_with(" × "));
        assert!(labels[1].contains('…'));
    }

    #[test]
    fn short_title_is_centered() {
        assert_eq!(centered_label("ab", 6), "  ab  ");
        assert_eq!(centered_label("abc", 6), " abc  ");
    }

    #[test]
    fn long_title_is_truncated_with_ellipsis() {
        assert_eq!(centered_label("abcdefgh", 5), "abcd…");
        // A wide char that would straddle the limit is dropped, and the
        // shortfall is padded.
        assert_eq!(centered_label("日本語", 4).width(), 4);
        assert_eq!(centered_label("日本語", 4), "日… ");
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(centered_label("a\x1bb", 2), "ab");
    }

    #[test]
    fn narrow_segment_has_no_close_mark() {
        assert_eq!(segment_label("abc", 5), " abc ");
        assert_eq!(tab_bar_hit_at(2, 10, 4), Some(SegmentHit::Select(0)));
    }

    #[test]
    fn hit_test_separates_title_and_close_zone() {
        // Two 10-column segments: close zone is the last 3 columns of each.
        assert_eq!(tab_bar_hit_at(2, 20, 0), Some(SegmentHit::Select(0)));
        assert_eq!(tab_bar_hit_at(2, 20, 6), Some(SegmentHit::Select(0)));
        assert_eq!(tab_bar_hit_at(2, 20, 7), Some(SegmentHit::Close(0)));
        assert_eq!(tab_bar_hit_at(2, 20, 9), Some(SegmentHit::Close(0)));
        assert_eq!(tab_bar_hit_at(2, 20, 10), Some(SegmentHit::Select(1)));
        assert_eq!(tab_bar_hit_at(2, 20, 19), Some(SegmentHit::Close(1)));
        assert_eq!(tab_bar_hit_at(2, 20, 20), None);
    }

    #[test]
    fn cols_respect_padding_and_never_drop_to_zero() {
        assert_eq!(tab_bar_cols(100, 10, 8.0), 10);
        assert_eq!(tab_bar_cols(10, 10, 8.0), 1);
    }
}
