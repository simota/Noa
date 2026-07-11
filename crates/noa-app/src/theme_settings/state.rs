use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use noa_config::{BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosTitlebarStyle};

use crate::command_palette::fuzzy_match;
use crate::debounce::Debouncer;

use super::{
    RevertValues, RowDraft, RowEffect, Section, SettingsRow, SettingsRowKind, ThemePairContext,
    ThemeSettingsInit, ThemeSettingsMode, background_image_fit_value,
    background_image_position_value,
};

/// The injectable config-writer seam [`ThemeSettings::commit`] takes
/// (R-12/AC-8/AC-23): production passes a thin closure over
/// [`noa_config::write_config_updates`]; tests pass a spy or a
/// failure-injecting closure.
pub(crate) type ConfigWriteFn<'a> = dyn FnMut(&Path, &[(String, String)]) -> io::Result<()> + 'a;

/// ~150ms debounce window for font-size (R-9/AC-6): a burst of ←→ presses or
/// digit keystrokes fires once, `apply_runtime_font_size` runs on the final
/// value.
const FONT_SIZE_DEBOUNCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

/// Font-size step per ←→ press. Mirrors the coarser `cmd+=`/`cmd+-` step
/// (`app/helpers.rs`'s runtime font actions use whole points); this row uses
/// a finer half-point step per the SHAPE table.
const FONT_SIZE_STEP: f32 = 0.5;
/// Local mirror of `app::helpers::{MIN,MAX}_RUNTIME_FONT_SIZE` — the pure
/// module can't reach that `pub(super)` constant across the `app` privacy
/// boundary (see the module doc on the file split). Harmless if it ever
/// drifts: `App::apply_runtime_font_size` re-clamps with the real constants
/// before touching the font, so this only bounds the *draft* value shown in
/// the row while editing.
const FONT_SIZE_MIN: f32 = 6.0;
const FONT_SIZE_MAX: f32 = 96.0;

/// Background-opacity step per ←→ press (SHAPE table: `0.0–1.0 step 0.05`).
const OPACITY_STEP: f32 = 0.05;
/// Background-image-opacity step per ←→ press.
const BACKGROUND_IMAGE_OPACITY_STEP: f32 = 0.05;
/// Background-image-interval step per ←→ press, in seconds.
const BACKGROUND_IMAGE_INTERVAL_STEP_SECS: u64 = 5;
/// Background-blur-radius step per ←→ press, and its config-documented cap
/// (`noa-config`'s `background_blur_radius` doc comment: `0..=64`).
const BLUR_STEP: i32 = 1;
const BLUR_MAX: u16 = 64;
/// Window-padding step per ←→ press (both x and y move together — a single
/// row adjusts uniform padding; see [`ThemeSettings::adjust`]'s doc for why).
const WINDOW_PADDING_STEP: f32 = 1.0;
/// Sidebar preview line count step per ←→ press.
const SIDEBAR_PREVIEW_LINES_STEP: i32 = 1;
/// Quick terminal height fraction step per ←→ press.
const QUICK_TERMINAL_SIZE_STEP: f32 = 0.05;
const QUICK_TERMINAL_SIZE_MIN: f32 = 0.1;
const QUICK_TERMINAL_SIZE_MAX: f32 = 1.0;

/// One theme catalog match: an index into `noa_theme::THEMES` plus the fuzzy
/// match char positions (for highlight rendering), reusing
/// [`crate::command_palette::fuzzy_match`] rather than a second matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeMatch {
    index: usize,
    positions: Vec<usize>,
}

/// The open theme-settings overlay's editable state (R-2..R-11, R-16). Holds
/// no window/GPU binding of its own — that lives in the `App`-side session,
/// mirroring [`crate::command_palette::CommandPalette`].
///
/// `Clone` exists so `App::redraw` can snapshot it out early (like
/// [`crate::command_palette::CommandPalette`]'s render payload) without
/// holding a live borrow of `App::theme_settings` across the redraw's later
/// `&mut self` calls. `filtered`/`available_font_families` are `Arc`-wrapped
/// (ADR-1/R-19/AC-25) so this clone never deep-copies the catalog-sized
/// match list — it shares the same allocation via a refcount bump, the same
/// zero-copy effect a dedicated render-payload type would have had, without
/// a second type or a second set of accessors for the two draw paths to
/// agree on.
#[derive(Clone)]
pub(crate) struct ThemeSettings {
    mode: ThemeSettingsMode,
    section: Section,
    filter: String,
    /// `Arc`-wrapped (ADR-1): always replaced wholesale on a real recompute
    /// (`recompute_filtered`/`refilter`), never mutated in place — so
    /// `Arc::as_ptr` identity doubles as an O(1) "did the result set
    /// change" signal for [`Self::view_fingerprint`] (ADR-2).
    filtered: Arc<Vec<ThemeMatch>>,
    /// Index into `filtered`, meaningless (and unused) while `filtered` is
    /// empty (AC-16: the picker stays empty without resetting anything).
    highlighted: usize,
    /// R-6: becomes `true` the first time the highlight actually changes
    /// (navigation or a non-empty-result filter edit) — opening the overlay
    /// previews nothing until then. Also feeds [`Self::badge_visible`].
    highlight_moved: bool,
    selected_row: usize,
    rows: [SettingsRow; SettingsRowKind::COUNT],
    snapshot: RevertValues,
    font_size_debounce: Debouncer<f32>,
    /// Accumulates digit keystrokes typed directly into the focused
    /// font-size row (R-2's "数値行は直接入力も可"); reset whenever
    /// navigation leaves the row. `None` between edits.
    font_size_digits: Option<String>,
    /// Text-entry buffer for `background-image`. `None` means the first typed
    /// printable character replaces the current path; after that, text appends.
    background_image_text: Option<String>,
    /// R-11 gate: set once at open from the opacity at that moment. A
    /// window can't transition opaque<->transparent at runtime, so this
    /// never changes for the life of one overlay session.
    opaque_at_startup: bool,
    /// `Arc`-wrapped for the same reason as `filtered` (ADR-1) — this list
    /// never changes after `open()`, so every clone would otherwise
    /// duplicate it for nothing.
    available_font_families: Arc<Vec<String>>,
    /// R-12/AC-23: set by a failed [`Self::commit`] write, rendered as a
    /// one-line error in the existing overlay text style. `None` normally,
    /// and on every successful [`Self::commit`] (a stale error from an
    /// earlier failed attempt must not survive a later success).
    commit_error: Option<String>,
    /// R-34/ADR-4: `Some` when the config's `theme` directive is a
    /// `light:X,dark:Y` pair — see [`ThemePairContext`]. Read only by
    /// [`Self::commit_updates`]; never mutated after `open()`.
    theme_pair: Option<ThemePairContext>,
}

impl ThemeSettings {
    /// Open the overlay: theme picker focused, filter empty (full 574-entry
    /// catalog shown), the picker's initial highlight on the currently
    /// active theme (SHAPE), every settings row seeded from `init`'s live
    /// values with `touched = false`.
    pub(crate) fn open(init: ThemeSettingsInit) -> Self {
        let snapshot = RevertValues {
            theme_name: init.current_theme.clone(),
            font_size: init.font_size,
            cursor_style: init.cursor_style,
            background_opacity: init.background_opacity,
            background_blur_radius: init.background_blur_radius,
            background_image: init.background_image.clone(),
            background_image_opacity: init.background_image_opacity,
            background_image_position: init.background_image_position,
            background_image_fit: init.background_image_fit,
            background_image_repeat: init.background_image_repeat,
            background_image_interval_secs: init.background_image_interval_secs,
            sidebar_preview_lines: init.sidebar_preview_lines,
            quick_terminal_size: init.quick_terminal_size,
        };
        let rows = [
            SettingsRow {
                draft: RowDraft::FontSize(init.font_size),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundOpacity(init.background_opacity),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundBlurRadius(init.background_blur_radius),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImage(init.background_image),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImageOpacity(init.background_image_opacity),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImagePosition(init.background_image_position),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImageFit(init.background_image_fit),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImageRepeat(init.background_image_repeat),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::BackgroundImageInterval(init.background_image_interval_secs),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::CursorStyle(init.cursor_style),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::FontFamily(init.font_family),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::WindowPadding(init.window_padding_x, init.window_padding_y),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::MacosTitlebarStyle(init.macos_titlebar_style),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::SidebarPreviewLines(init.sidebar_preview_lines),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::QuickTerminalHeight(init.quick_terminal_size),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::ConfirmQuit(init.confirm_quit),
                touched: false,
            },
        ];
        let mut settings = ThemeSettings {
            mode: init.mode,
            section: init.mode.fixed_section(),
            filter: String::new(),
            filtered: Arc::new(Vec::new()),
            highlighted: 0,
            highlight_moved: false,
            selected_row: 0,
            rows,
            snapshot,
            font_size_debounce: Debouncer::new(FONT_SIZE_DEBOUNCE_WINDOW),
            font_size_digits: None,
            background_image_text: None,
            opaque_at_startup: init.background_opacity >= 1.0,
            available_font_families: Arc::new(init.available_font_families),
            commit_error: None,
            theme_pair: init.theme_pair,
        };
        settings.recompute_filtered();
        if let Some(pos) = settings
            .filtered
            .iter()
            .position(|m| noa_theme::THEMES[m.index].0 == settings.snapshot.theme_name)
        {
            settings.highlighted = pos;
        }
        settings
    }

    pub(crate) fn section(&self) -> Section {
        self.section
    }

    /// Which overlay this session is — "Theme" picker or "Settings" rows.
    pub(crate) fn mode(&self) -> ThemeSettingsMode {
        self.mode
    }

    /// Tab (R-2, AC-22 historical): a no-op now that a session's section is
    /// fixed for its whole lifetime by [`ThemeSettingsMode`] — the other
    /// half of the old combined overlay doesn't exist in this session to
    /// switch to.
    pub(crate) fn toggle_section(&mut self) {}

    pub(crate) fn filter(&self) -> &str {
        &self.filter
    }

    pub(crate) fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    pub(crate) fn highlighted_index(&self) -> usize {
        self.highlighted
    }

    /// The theme name + fuzzy match positions at `filtered` index `i`.
    pub(crate) fn filtered_entry(&self, i: usize) -> Option<(&'static str, &[usize])> {
        self.filtered
            .get(i)
            .map(|m| (noa_theme::THEMES[m.index].0, m.positions.as_slice()))
    }

    /// The currently highlighted theme's name, or `None` on an empty result
    /// set (AC-16).
    pub(crate) fn highlighted_theme_name(&self) -> Option<&'static str> {
        self.filtered
            .get(self.highlighted)
            .map(|m| noa_theme::THEMES[m.index].0)
    }

    /// R-6: whether `App` should (re)resolve [`Self::highlighted_theme_name`]
    /// into `gpu.preview_theme`. `true` from the first highlight-changing
    /// action onward for the life of this session.
    pub(crate) fn should_preview(&self) -> bool {
        self.highlight_moved
    }

    pub(crate) fn selected_row(&self) -> usize {
        self.selected_row
    }

    pub(crate) fn rows(&self) -> &[SettingsRow; SettingsRowKind::COUNT] {
        &self.rows
    }

    /// Currently read only from tests — `restart_note` below checks the
    /// `opaque_at_startup` field directly rather than through this accessor.
    /// Kept `pub(crate)` as inspection/test support (e.g. asserting a
    /// session's startup mode) rather than made test-only.
    #[allow(dead_code)]
    pub(crate) fn opaque_at_startup(&self) -> bool {
        self.opaque_at_startup
    }

    /// R-11: whether `row` should show the "applies after restart" note
    /// instead of a live preview right now. Two independent cases: a *live*
    /// opacity/blur row whose session started opaque (R-11's original
    /// case — `FontSize`/`CursorStyle` always apply live regardless), or
    /// any *commit-only* row (`FontFamily`/`WindowPadding`/
    /// `MacosTitlebarStyle`) the user has actually edited — those have
    /// no runtime-apply path at all (`App::commit_theme_settings`), so a
    /// touched edit persists to config but only takes effect on the next
    /// launch.
    pub(crate) fn restart_note(&self, row: SettingsRowKind) -> bool {
        if row.is_live() {
            return self.opaque_at_startup
                && matches!(
                    row,
                    SettingsRowKind::BackgroundOpacity | SettingsRowKind::BackgroundBlurRadius
                );
        }
        if matches!(
            row,
            SettingsRowKind::BackgroundImage
                | SettingsRowKind::BackgroundImageOpacity
                | SettingsRowKind::BackgroundImagePosition
                | SettingsRowKind::BackgroundImageFit
                | SettingsRowKind::BackgroundImageRepeat
                | SettingsRowKind::BackgroundImageInterval
                | SettingsRowKind::ConfirmQuit
                | SettingsRowKind::QuickTerminalHeight
        ) {
            return false;
        }
        let index = SettingsRowKind::ALL
            .iter()
            .position(|kind| *kind == row)
            .expect("SettingsRowKind::ALL contains every variant");
        self.rows[index].touched
    }

    /// AC-4a: the "Chrome updates on Save" badge is visible once the
    /// session has ever previewed a different theme, or any *live*-kind row
    /// has been edited away from its snapshot value.
    pub(crate) fn badge_visible(&self) -> bool {
        self.highlight_moved
            || SettingsRowKind::ALL
                .iter()
                .enumerate()
                .any(|(i, kind)| kind.is_live() && self.rows[i].touched)
    }

    /// ↑↓: theme-list highlight in `ThemePicker`, row selection in
    /// `SettingsRows` — never a value adjustment (R-2).
    pub(crate) fn move_up(&mut self) {
        match self.section {
            Section::ThemePicker => {
                if self.highlighted > 0 {
                    self.highlighted -= 1;
                    self.highlight_moved = true;
                }
            }
            Section::SettingsRows => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                    self.clear_row_input_state();
                }
            }
        }
    }

    pub(crate) fn move_down(&mut self) {
        match self.section {
            Section::ThemePicker => {
                if !self.filtered.is_empty() && self.highlighted + 1 < self.filtered.len() {
                    self.highlighted += 1;
                    self.highlight_moved = true;
                }
            }
            Section::SettingsRows => {
                if self.selected_row + 1 < SettingsRowKind::ALL.len() {
                    self.selected_row += 1;
                    self.clear_row_input_state();
                }
            }
        }
    }

    /// Printable text: fuzzy-filters the theme picker, or feeds direct digit
    /// entry into a focused font-size row (R-2). `now` drives the font-size
    /// debounce the same way [`Self::adjust`] does.
    pub(crate) fn push_text(&mut self, text: &str, now: Instant) {
        match self.section {
            Section::ThemePicker => {
                let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                if filtered.is_empty() {
                    return;
                }
                let previous_filter = self.filter.clone();
                self.filter.push_str(&filtered);
                self.refilter_and_mark(&previous_filter);
            }
            Section::SettingsRows => match SettingsRowKind::ALL[self.selected_row] {
                SettingsRowKind::FontSize => self.push_font_size_digits(text, now),
                SettingsRowKind::BackgroundImage => self.push_background_image_text(text),
                _ => {}
            },
        }
    }

    /// Backspace: pops one filter character in `ThemePicker`, or pops one
    /// digit from the in-progress font-size entry in `SettingsRows`.
    pub(crate) fn backspace(&mut self, now: Instant) {
        match self.section {
            Section::ThemePicker => {
                let previous_filter = self.filter.clone();
                if self.filter.pop().is_some() {
                    self.refilter_and_mark(&previous_filter);
                }
            }
            Section::SettingsRows => match SettingsRowKind::ALL[self.selected_row] {
                SettingsRowKind::FontSize => {
                    if let Some(digits) = &mut self.font_size_digits {
                        digits.pop();
                        if let Ok(value) = digits.parse::<f32>() {
                            self.set_font_size(value, now);
                        }
                    }
                }
                SettingsRowKind::BackgroundImage => {
                    let idx = self.selected_row;
                    let next = {
                        let text = self.background_image_text.get_or_insert_with(|| {
                            match &self.rows[idx].draft {
                                RowDraft::BackgroundImage(path) => path.clone(),
                                _ => String::new(),
                            }
                        });
                        text.pop();
                        text.clone()
                    };
                    self.set_background_image_text(next);
                }
                _ => {}
            },
        }
    }

    fn clear_row_input_state(&mut self) {
        self.font_size_digits = None;
        self.background_image_text = None;
    }

    fn push_font_size_digits(&mut self, text: &str, now: Instant) {
        if SettingsRowKind::ALL[self.selected_row] != SettingsRowKind::FontSize {
            return;
        }
        let digits = self.font_size_digits.get_or_insert_with(String::new);
        for ch in text.chars() {
            if ch.is_ascii_digit() || (ch == '.' && !digits.contains('.')) {
                digits.push(ch);
            }
        }
        if let Ok(value) = digits.parse::<f32>() {
            self.set_font_size(value, now);
        }
    }

    fn push_background_image_text(&mut self, text: &str) {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        if filtered.is_empty() {
            return;
        }
        let next = {
            let text = self.background_image_text.get_or_insert_with(String::new);
            text.push_str(&filtered);
            text.clone()
        };
        self.set_background_image_text(next);
    }

    fn set_background_image_text(&mut self, value: String) {
        let idx = self.selected_row;
        let RowDraft::BackgroundImage(current) = &self.rows[idx].draft else {
            return;
        };
        if current != &value {
            self.rows[idx].draft = RowDraft::BackgroundImage(value);
            self.rows[idx].touched = true;
        }
    }

    fn set_font_size(&mut self, value: f32, now: Instant) {
        let clamped = value.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        let RowDraft::FontSize(current) = &mut self.rows[0].draft else {
            return;
        };
        if (*current - clamped).abs() > f32::EPSILON {
            *current = clamped;
            self.rows[0].touched = true;
            self.font_size_debounce.submit(clamped, now);
        }
    }

    /// ←→ on the focused settings row: step a numeric row or cycle a
    /// sample-set row (R-2). A no-op (and `RowEffect::None`) while the theme
    /// picker owns the section, per R-2's "no-op in theme list". Window
    /// padding intentionally moves x and y together on a single ←→ step —
    /// the SHAPE table places both on one row and the spec doesn't carve out
    /// a distinct gesture for the second axis; a future increment can split
    /// them if that turns out to matter.
    pub(crate) fn adjust(&mut self, delta: i32, now: Instant) -> RowEffect {
        if self.section != Section::SettingsRows || delta == 0 {
            return RowEffect::None;
        }
        let idx = self.selected_row;
        match SettingsRowKind::ALL[idx] {
            SettingsRowKind::FontSize => {
                let RowDraft::FontSize(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                self.set_font_size(current + delta as f32 * FONT_SIZE_STEP, now);
                RowEffect::None
            }
            SettingsRowKind::BackgroundOpacity => {
                let RowDraft::BackgroundOpacity(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = (current + delta as f32 * OPACITY_STEP).clamp(0.0, 1.0);
                if (new - current).abs() > f32::EPSILON {
                    self.rows[idx].draft = RowDraft::BackgroundOpacity(new);
                    self.rows[idx].touched = true;
                }
                if self.opaque_at_startup {
                    RowEffect::None
                } else {
                    RowEffect::Opacity(new)
                }
            }
            SettingsRowKind::BackgroundBlurRadius => {
                let RowDraft::BackgroundBlurRadius(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new =
                    (i32::from(current) + delta * BLUR_STEP).clamp(0, i32::from(BLUR_MAX)) as u16;
                if new != current {
                    self.rows[idx].draft = RowDraft::BackgroundBlurRadius(new);
                    self.rows[idx].touched = true;
                }
                if self.opaque_at_startup {
                    RowEffect::None
                } else {
                    RowEffect::Blur(new)
                }
            }
            SettingsRowKind::BackgroundImage => RowEffect::None,
            SettingsRowKind::BackgroundImageOpacity => {
                let RowDraft::BackgroundImageOpacity(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = (current + delta as f32 * BACKGROUND_IMAGE_OPACITY_STEP).clamp(0.0, 1.0);
                if (new - current).abs() > f32::EPSILON {
                    self.rows[idx].draft = RowDraft::BackgroundImageOpacity(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::BackgroundImagePosition => {
                let RowDraft::BackgroundImagePosition(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[
                        BackgroundImagePosition::TopLeft,
                        BackgroundImagePosition::TopCenter,
                        BackgroundImagePosition::TopRight,
                        BackgroundImagePosition::CenterLeft,
                        BackgroundImagePosition::Center,
                        BackgroundImagePosition::CenterRight,
                        BackgroundImagePosition::BottomLeft,
                        BackgroundImagePosition::BottomCenter,
                        BackgroundImagePosition::BottomRight,
                    ],
                    current,
                    delta,
                );
                if new != current {
                    self.rows[idx].draft = RowDraft::BackgroundImagePosition(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::BackgroundImageFit => {
                let RowDraft::BackgroundImageFit(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[
                        BackgroundImageFit::None,
                        BackgroundImageFit::Contain,
                        BackgroundImageFit::Cover,
                        BackgroundImageFit::Stretch,
                    ],
                    current,
                    delta,
                );
                if new != current {
                    self.rows[idx].draft = RowDraft::BackgroundImageFit(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::BackgroundImageRepeat => {
                let RowDraft::BackgroundImageRepeat(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                self.rows[idx].draft = RowDraft::BackgroundImageRepeat(!current);
                self.rows[idx].touched = true;
                RowEffect::None
            }
            SettingsRowKind::BackgroundImageInterval => {
                let RowDraft::BackgroundImageInterval(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = adjust_background_image_interval(current, delta);
                if new != current {
                    self.rows[idx].draft = RowDraft::BackgroundImageInterval(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::CursorStyle => {
                let RowDraft::CursorStyle(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[
                        CursorShape::Block,
                        CursorShape::Bar,
                        CursorShape::Underline,
                        CursorShape::BlockHollow,
                    ],
                    current,
                    delta,
                );
                if new != current {
                    self.rows[idx].draft = RowDraft::CursorStyle(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::CursorStyle(new)
            }
            SettingsRowKind::FontFamily => {
                let RowDraft::FontFamily(current) = self.rows[idx].draft.clone() else {
                    return RowEffect::None;
                };
                let new = self.cycle_font_family(&current, delta);
                if new != current {
                    self.rows[idx].draft = RowDraft::FontFamily(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::WindowPadding => {
                let RowDraft::WindowPadding(x, y) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let step = delta as f32 * WINDOW_PADDING_STEP;
                let new_x = (x + step).max(0.0);
                let new_y = (y + step).max(0.0);
                if (new_x - x).abs() > f32::EPSILON {
                    self.rows[idx].draft = RowDraft::WindowPadding(new_x, new_y);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::MacosTitlebarStyle => {
                let RowDraft::MacosTitlebarStyle(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[MacosTitlebarStyle::Native, MacosTitlebarStyle::Transparent],
                    current,
                    delta,
                );
                if new != current {
                    self.rows[idx].draft = RowDraft::MacosTitlebarStyle(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::SidebarPreviewLines => {
                let RowDraft::SidebarPreviewLines(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = (current as i32 + delta * SIDEBAR_PREVIEW_LINES_STEP)
                    .clamp(0, noa_config::MAX_SIDEBAR_PREVIEW_LINES as i32)
                    as usize;
                if new != current {
                    self.rows[idx].draft = RowDraft::SidebarPreviewLines(new);
                    self.rows[idx].touched = true;
                    return RowEffect::SidebarPreviewLines(new);
                }
                RowEffect::None
            }
            SettingsRowKind::QuickTerminalHeight => {
                let RowDraft::QuickTerminalHeight(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = (current + delta as f32 * QUICK_TERMINAL_SIZE_STEP)
                    .clamp(QUICK_TERMINAL_SIZE_MIN, QUICK_TERMINAL_SIZE_MAX);
                if (new - current).abs() > f32::EPSILON {
                    self.rows[idx].draft = RowDraft::QuickTerminalHeight(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::ConfirmQuit => {
                let RowDraft::ConfirmQuit(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = !current;
                self.rows[idx].draft = RowDraft::ConfirmQuit(new);
                self.rows[idx].touched = true;
                RowEffect::None
            }
        }
    }

    fn cycle_font_family(&self, current: &str, delta: i32) -> String {
        if self.available_font_families.is_empty() {
            return current.to_string();
        }
        let len = self.available_font_families.len() as i32;
        let idx = self
            .available_font_families
            .iter()
            .position(|f| f == current)
            .map_or(0, |i| i as i32);
        let next = (idx + delta).rem_euclid(len) as usize;
        self.available_font_families[next].clone()
    }

    /// Always a full 574-entry catalog rescan — used only by [`Self::open`],
    /// where there is no previous `filtered` result set to narrow from.
    fn recompute_filtered(&mut self) {
        self.filtered = Arc::new(filter_themes(&self.filter));
        self.highlighted = 0;
    }

    /// R-21/NFR-8 (ADR-3): re-filter from `self.filter` after it changed
    /// from `previous_filter`, then mark the highlight moved — unless the
    /// new filter matches nothing, in which case the picker stays empty
    /// without disturbing the last preview (AC-16).
    ///
    /// A forward edit — `self.filter` strictly extends `previous_filter`
    /// (typing ahead) — only rescans the *previous* `filtered` result set
    /// (AC-28): a theme that didn't match the shorter filter can never
    /// match a longer one that starts with it, so the full catalog needs no
    /// second look. Anything else (Backspace breaking the prefix
    /// relationship, a wholesale replace) falls back to a full rescan
    /// (AC-29) — never a narrowed one, which could otherwise hide a theme
    /// the new, unrelated filter should have matched.
    fn refilter_and_mark(&mut self, previous_filter: &str) {
        self.filtered = if self.filter.len() > previous_filter.len()
            && self.filter.starts_with(previous_filter)
        {
            Arc::new(narrow_filtered(&self.filtered, &self.filter))
        } else {
            Arc::new(filter_themes(&self.filter))
        };
        self.highlighted = 0;
        if !self.filtered.is_empty() {
            self.highlight_moved = true;
        }
    }

    /// The live `background-opacity` draft (row 1), for `App` to re-apply
    /// alongside a blur-radius change (the two are drawn as a pair, R-10).
    pub(crate) fn live_background_opacity(&self) -> f32 {
        match self.rows[1].draft {
            RowDraft::BackgroundOpacity(v) => v,
            _ => 1.0,
        }
    }

    /// Whether a font-size value is still waiting out its debounce window —
    /// `App`'s timer tick uses this to decide whether to keep re-arming its
    /// wake-up (NFR-2: no busy-polling once the burst has settled).
    pub(crate) fn font_size_debounce_pending(&self) -> bool {
        self.font_size_debounce.is_pending()
    }

    /// R-9/AC-6: poll the font-size debouncer. `App` calls this on its own
    /// schedule (a timer tick while the overlay is open); a `Some` return
    /// means the burst has settled and `App` must apply it via the existing
    /// `runtime_font_size` path.
    pub(crate) fn poll_font_size(&mut self, now: Instant) -> Option<f32> {
        self.font_size_debounce.poll(now)
    }

    /// Esc (R-16): cancel any pending font-size debounce (an unfired value
    /// is discarded, never applied) and return the values `App` must
    /// restore live state to. Row drafts/touched flags need no explicit
    /// reset here — `App` drops the whole session right after this call.
    pub(crate) fn revert(&mut self) -> RevertValues {
        self.font_size_debounce.cancel();
        self.snapshot.clone()
    }

    /// The current error text set by a failed [`Self::commit`], if any
    /// (AC-23) — `App`'s render path shows this in place of the normal
    /// keybind hint line.
    pub(crate) fn commit_error(&self) -> Option<&str> {
        self.commit_error.as_deref()
    }

    /// Record a commit failure that happened before [`Self::commit`] could
    /// even be called — `App` has no writable config path to try (no home
    /// directory resolvable). Never exercised by [`Self::commit`] itself;
    /// kept separate so `commit`'s error path stays exclusively about the
    /// injected writer failing (AC-23's contract).
    pub(crate) fn set_commit_error(&mut self, message: String) {
        self.commit_error = Some(message);
    }

    /// R-12 step 1 / R-17, NFR-6: the exact config updates a commit should
    /// write — `theme = <highlighted name>` only if it differs from the
    /// theme active before this overlay session (`self.snapshot`), plus one
    /// entry per *touched* row (window-padding contributes two). An
    /// untouched row is never included: its `draft` may equal a CLI-only
    /// override value (the overlay seeds every draft from the live session
    /// value, CLI included), but only an actual edit flips `touched`, so a
    /// CLI override can never leak into the written config just by having
    /// been active while the user changed something else.
    pub(crate) fn commit_updates(&self) -> Vec<(String, String)> {
        let mut updates = Vec::new();
        if let Some(name) = self.highlighted_theme_name()
            && name != self.snapshot.theme_name
        {
            // R-34/ADR-4: a `light:X,dark:Y` pair config rewrites only the
            // currently active side, keeping the other side's value intact
            // — never the bare single-name overwrite below, which would
            // silently drop the pair syntax (AC-49/AC-50). `writer::
            // apply_updates` itself needs no change for this: it just
            // replaces the `theme` key's value verbatim, so handing it a
            // pre-built `light:_,dark:_` string is enough (NFR-9).
            match &self.theme_pair {
                Some(ctx) => {
                    let (light, dark) = if ctx.active_is_light {
                        (name, ctx.dark.as_str())
                    } else {
                        (ctx.light.as_str(), name)
                    };
                    updates.push(("theme".to_string(), format!("light:{light},dark:{dark}")));
                }
                None => updates.push(("theme".to_string(), name.to_string())),
            }
        }
        for row in &self.rows {
            if !row.touched {
                continue;
            }
            match &row.draft {
                RowDraft::FontSize(v) => updates.push(("font-size".to_string(), format!("{v}"))),
                RowDraft::BackgroundOpacity(v) => {
                    updates.push(("background-opacity".to_string(), format!("{v:.2}")));
                }
                RowDraft::BackgroundBlurRadius(v) => {
                    updates.push(("background-blur-radius".to_string(), v.to_string()));
                }
                RowDraft::BackgroundImage(path) => {
                    updates.push(("background-image".to_string(), path.clone()));
                }
                RowDraft::BackgroundImageOpacity(v) => {
                    updates.push(("background-image-opacity".to_string(), format!("{v:.2}")));
                }
                RowDraft::BackgroundImagePosition(position) => {
                    updates.push((
                        "background-image-position".to_string(),
                        background_image_position_value(*position).to_string(),
                    ));
                }
                RowDraft::BackgroundImageFit(fit) => {
                    updates.push((
                        "background-image-fit".to_string(),
                        background_image_fit_value(*fit).to_string(),
                    ));
                }
                RowDraft::BackgroundImageRepeat(repeat) => {
                    updates.push(("background-image-repeat".to_string(), repeat.to_string()));
                }
                RowDraft::BackgroundImageInterval(secs) => {
                    updates.push(("background-image-interval".to_string(), secs.to_string()));
                }
                RowDraft::CursorStyle(shape) => {
                    updates.push((
                        "cursor-style".to_string(),
                        cursor_shape_config_value(*shape).to_string(),
                    ));
                }
                RowDraft::FontFamily(name) => {
                    updates.push(("font-family".to_string(), name.clone()));
                }
                RowDraft::WindowPadding(x, y) => {
                    updates.push(("window-padding-x".to_string(), format!("{x}")));
                    updates.push(("window-padding-y".to_string(), format!("{y}")));
                }
                RowDraft::MacosTitlebarStyle(style) => {
                    updates.push((
                        "macos-titlebar-style".to_string(),
                        macos_titlebar_style_config_value(*style).to_string(),
                    ));
                }
                RowDraft::SidebarPreviewLines(lines) => {
                    updates.push(("sidebar-preview-lines".to_string(), lines.to_string()));
                }
                RowDraft::QuickTerminalHeight(size) => {
                    updates.push(("quick-terminal-size".to_string(), format!("{size:.2}")));
                }
                RowDraft::ConfirmQuit(confirm) => {
                    updates.push(("confirm-quit".to_string(), confirm.to_string()));
                }
            }
        }
        updates
    }

    /// Enter's failable step (R-12 step 2): write [`Self::commit_updates`]
    /// through the injected `write` callback (production: a thin closure
    /// over [`noa_config::write_config_updates`]; tests: a spy/failing
    /// closure, AC-8/AC-23) against `config_path`.
    ///
    /// On failure, records [`Self::commit_error`] and returns `None` —
    /// nothing else on `self` changes (no drafts/preview/touched-flag
    /// mutation happens anywhere in this method on the error path), so the
    /// overlay is left exactly as the user last saw it (AC-23). On success,
    /// clears any stale error and returns the updates that were just
    /// persisted, so `App` can derive the theme/chrome swap and commit-only
    /// row handling from them without recomputing anything.
    pub(crate) fn commit(
        &mut self,
        config_path: &Path,
        write: &mut ConfigWriteFn<'_>,
    ) -> Option<Vec<(String, String)>> {
        let updates = self.commit_updates();
        match write(config_path, &updates) {
            Ok(()) => {
                self.commit_error = None;
                Some(updates)
            }
            Err(err) => {
                self.commit_error = Some(format!("Failed to save settings: {err}"));
                None
            }
        }
    }

    /// Cheap, allocation-free identity for
    /// [`crate::macos_overlay::sync_theme_settings`]'s change-detection key
    /// (ADR-2/R-20/NFR-7): every field that can influence
    /// [`crate::macos_overlay::theme_settings_view_model`]'s output funnels
    /// into this hash, so a caller can tell "did the ViewModel just become
    /// stale" without ever constructing one. `filtered`/
    /// `available_font_families` compare by `Arc` pointer identity (ADR-1
    /// replaces the whole `Arc` on every real recompute, never mutates it
    /// in place) rather than hashing every catalog entry.
    ///
    /// Whoever adds a mutator that changes what the ViewModel shows must
    /// add the corresponding field here — AC-60's property test
    /// (`every_mutator_that_changes_state_changes_the_fingerprint`) walks
    /// every existing mutator and asserts this holds; extend it alongside
    /// any new one.
    pub(crate) fn view_fingerprint(&self, hasher: &mut impl Hasher) {
        self.mode.hash(hasher);
        self.section.hash(hasher);
        self.filter.hash(hasher);
        (Arc::as_ptr(&self.filtered) as usize).hash(hasher);
        self.highlighted.hash(hasher);
        self.highlight_moved.hash(hasher);
        self.selected_row.hash(hasher);
        for row in &self.rows {
            std::mem::discriminant(&row.draft).hash(hasher);
            hash_row_draft_value(&row.draft, hasher);
            row.touched.hash(hasher);
        }
        self.commit_error.hash(hasher);
        self.font_size_digits.hash(hasher);
        self.background_image_text.hash(hasher);
        self.opaque_at_startup.hash(hasher);
        (Arc::as_ptr(&self.available_font_families) as usize).hash(hasher);
    }

    /// Test-only convenience over [`Self::view_fingerprint`] — a `u64`
    /// digest instead of a raw hasher, so property tests can compare
    /// before/after values with a plain `assert_ne!`.
    #[cfg(test)]
    pub(crate) fn view_fingerprint_u64(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.view_fingerprint(&mut hasher);
        hasher.finish()
    }

    /// AC-25/ADR-1 (R-19): the `filtered` list's `Arc` strong count — a
    /// test-only introspection hook proving `Clone` shares the catalog-sized
    /// match list instead of deep-copying it.
    #[cfg(test)]
    pub(crate) fn filtered_arc_strong_count(&self) -> usize {
        Arc::strong_count(&self.filtered)
    }
}

/// `cursor-style` config value for `shape` (mirrors
/// `noa_config::parser::values::parse_cursor_style`'s inverse — that parser
/// has no matching serializer, so the write side owns this mapping).
fn cursor_shape_config_value(shape: CursorShape) -> &'static str {
    match shape {
        CursorShape::Block => "block",
        CursorShape::Bar => "bar",
        CursorShape::Underline => "underline",
        CursorShape::BlockHollow => "block_hollow",
    }
}

/// `macos-titlebar-style` config value for `style` (inverse of
/// `parse_macos_titlebar_style`; `"tabs"` is a parse-only alias for
/// `Native`, so the write side always emits the canonical `"native"`).
fn macos_titlebar_style_config_value(style: MacosTitlebarStyle) -> &'static str {
    match style {
        MacosTitlebarStyle::Native => "native",
        MacosTitlebarStyle::Transparent => "transparent",
    }
}

/// [`ThemeSettings::view_fingerprint`]'s per-`RowDraft` half — the
/// discriminant itself is hashed by the caller (once, per row), so this
/// only needs each variant's inner value. `f32` fields go through
/// `to_bits()` (floats aren't `Hash`); the `noa_config` enums without a
/// `Hash` impl reuse their existing config-string serializers instead of
/// adding one just for this.
fn hash_row_draft_value(draft: &RowDraft, hasher: &mut impl Hasher) {
    match draft {
        RowDraft::FontSize(v)
        | RowDraft::BackgroundOpacity(v)
        | RowDraft::BackgroundImageOpacity(v)
        | RowDraft::QuickTerminalHeight(v) => v.to_bits().hash(hasher),
        RowDraft::BackgroundBlurRadius(v) => v.hash(hasher),
        RowDraft::BackgroundImage(s) | RowDraft::FontFamily(s) => s.hash(hasher),
        RowDraft::BackgroundImagePosition(position) => {
            background_image_position_value(*position).hash(hasher);
        }
        RowDraft::BackgroundImageFit(fit) => background_image_fit_value(*fit).hash(hasher),
        RowDraft::BackgroundImageRepeat(v) | RowDraft::ConfirmQuit(v) => v.hash(hasher),
        RowDraft::BackgroundImageInterval(v) => v.hash(hasher),
        RowDraft::CursorStyle(shape) => cursor_shape_config_value(*shape).hash(hasher),
        RowDraft::WindowPadding(x, y) => {
            x.to_bits().hash(hasher);
            y.to_bits().hash(hasher);
        }
        RowDraft::MacosTitlebarStyle(style) => {
            macos_titlebar_style_config_value(*style).hash(hasher);
        }
        RowDraft::SidebarPreviewLines(v) => v.hash(hasher),
    }
}

/// Cycle `current` to the next (`delta > 0`) or previous (`delta < 0`) value
/// in `order`, wrapping. Falls back to `order[0]` if `current` isn't found
/// (never happens in practice — every row's draft is always one of `order`'s
/// values — but avoids a panic over an `unwrap`).
fn cycle<T: Copy + PartialEq>(order: &[T], current: T, delta: i32) -> T {
    let len = order.len() as i32;
    let idx = order.iter().position(|v| *v == current).unwrap_or(0) as i32;
    let next = (idx + delta).rem_euclid(len);
    order[next as usize]
}

fn adjust_background_image_interval(current: u64, delta: i32) -> u64 {
    let step = BACKGROUND_IMAGE_INTERVAL_STEP_SECS * u64::from(delta.unsigned_abs());
    let next = if delta.is_negative() {
        current.saturating_sub(step)
    } else {
        current.saturating_add(step)
    };
    next.max(noa_config::MIN_BACKGROUND_IMAGE_INTERVAL_SECS)
}

// Test-only scan-scope instrumentation for AC-28/AC-29/NFR-8 (ADR-3):
// `score_and_sort` records how many catalog *candidates* it visited
// (before scoring), so a test can assert the first keystroke scans the
// full 574-entry catalog while a forward-extension edit only rescans the
// previous `filtered` result set, and a non-prefix edit falls back to a
// full rescan again. Thread-local (not a shared static) so parallel test
// threads never see each other's counts.
#[cfg(test)]
thread_local! {
    static SCAN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_scan_count() -> usize {
    SCAN_COUNT.with(|c| c.replace(0))
}

/// Fuzzy-score `indices` against `filter` (reusing [`fuzzy_match`] — no
/// second matcher, per the contract), best match first. Shared by
/// [`filter_themes`] (the full catalog) and [`narrow_filtered`] (a prior
/// result set), so the scoring/sort logic lives in exactly one place.
fn score_and_sort(filter: &str, indices: impl ExactSizeIterator<Item = usize>) -> Vec<ThemeMatch> {
    #[cfg(test)]
    SCAN_COUNT.with(|c| c.set(indices.len()));

    let mut matches: Vec<(i32, ThemeMatch)> = indices
        .filter_map(|index| {
            let name = noa_theme::THEMES[index].0;
            fuzzy_match(filter, name)
                .map(|(score, positions)| (score, ThemeMatch { index, positions }))
        })
        .collect();
    matches.sort_by_key(|b| std::cmp::Reverse(b.0));
    matches.into_iter().map(|(_, m)| m).collect()
}

/// The full theme catalog fuzzy-filtered by `filter`. An empty filter
/// matches every entry in catalog order (score 0, no highlight), mirroring
/// [`crate::command_palette::command_palette_matches`]'s empty-query
/// behavior.
fn filter_themes(filter: &str) -> Vec<ThemeMatch> {
    score_and_sort(filter, 0..noa_theme::THEMES.len())
}

/// R-21/ADR-3: re-score only `prior`'s entries against `filter` — used when
/// `filter` is a strict extension of the filter that produced `prior`
/// (AC-28): a theme that didn't match the shorter filter can never match a
/// longer one that starts with it.
fn narrow_filtered(prior: &[ThemeMatch], filter: &str) -> Vec<ThemeMatch> {
    score_and_sort(filter, prior.iter().map(|m| m.index))
}
