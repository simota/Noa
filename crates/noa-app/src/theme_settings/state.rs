use std::io;
use std::path::Path;
use std::time::Instant;

use noa_config::{
    BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosOptionAsAlt, MacosTitlebarStyle,
};

use crate::command_palette::fuzzy_match;
use crate::debounce::Debouncer;

use super::{
    Liveness, RestartReason, RevertValues, RowDraft, RowEffect, Section, SettingsRow,
    SettingsRowKind, ThemeSettingsInit, ThemeSettingsMode, background_image_fit_value,
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

/// R-7/C-5: how long the post-Reset row highlight flashes — the only
/// misfire-detection cue for a confirmation-free reset.
const RESET_FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(220);

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

/// R-9: `scrollback-limit` step per ←→ press (1 MB), and a pragmatic UI
/// ceiling (`noa-config` itself has no documented maximum — this only
/// bounds how far repeated presses can push the *draft*, matching
/// `FONT_SIZE_MIN`/`MAX`'s "bounds the draft only" role).
const SCROLLBACK_LIMIT_STEP: usize = 1_000_000;
const SCROLLBACK_LIMIT_MAX: usize = 1_000_000_000;
/// `minimum-contrast` step per ←→ press, over its documented `1.0..=21.0`
/// WCAG ratio range (`noa_config::parser::values::parse_minimum_contrast`).
const MINIMUM_CONTRAST_STEP: f32 = 1.0;
const MINIMUM_CONTRAST_MIN: f32 = 1.0;
const MINIMUM_CONTRAST_MAX: f32 = 21.0;

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
/// `Clone` exists so `App`'s `ThemeSettingsSession.state` can wrap this in an
/// `Arc` (settings-panel-enrichment R-4): `App::redraw` snapshots it out
/// early with `Arc::clone` (a refcount bump, not a deep copy of the
/// catalog-sized `filtered` list) instead of holding a live borrow of
/// `App::theme_settings` across the redraw's later `&mut self` calls, and
/// every mutating method is reached through `Arc::make_mut`, which only
/// actually invokes this `Clone` impl on the rare turn a render snapshot is
/// still alive when a mutation lands (verified never to allocate on the
/// steady-state redraw path by
/// `app::state::theme_settings_session_tests::consecutive_redraw_snapshots_share_the_same_allocation`,
/// AC-9/NFR-1).
#[derive(Clone)]
pub(crate) struct ThemeSettings {
    mode: ThemeSettingsMode,
    section: Section,
    filter: String,
    filtered: Vec<ThemeMatch>,
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
    available_font_families: Vec<String>,
    /// R-12/AC-23: set by a failed [`Self::commit`] write, rendered as a
    /// one-line error in the existing overlay text style. `None` normally,
    /// and on every successful [`Self::commit`] (a stale error from an
    /// earlier failed attempt must not survive a later success).
    commit_error: Option<String>,
    /// R-5: whether the `Section::SettingsRows` modal sub-state (Tab
    /// gesture) currently owns ↑↓/text input for row search instead of the
    /// normal row navigation/edit paths. Only ever `true` in
    /// [`ThemeSettingsMode::Settings`] sessions — Theme mode's Tab stays the
    /// existing `toggle_section` no-op (never routes here).
    settings_search_active: bool,
    settings_filter: String,
    /// Indices into `SettingsRowKind::ALL`/`rows`, best match first —
    /// mirrors `filtered`/`ThemeMatch` for the theme picker, minus the match
    /// positions (row labels are short enough not to need highlight spans).
    settings_filtered: Vec<usize>,
    /// Index into `settings_filtered` — a separate index space from
    /// `selected_row` (Addendum D-3/FM-02), never itself an index into
    /// `SettingsRowKind::ALL`.
    settings_highlight: usize,
    /// `selected_row` at the moment search was entered — restored on a
    /// Tab-exit-without-confirming (Addendum B: Tab exits search restoring
    /// the pre-search selection; only Enter confirms the highlighted row).
    settings_pre_search_selected: usize,
    /// R-7/C-5: brief post-Reset highlight deadline, the only misfire
    /// detection cue for a confirmation-free destructive-ish action. `App`
    /// polls this on its existing theme-settings timer tick and clears it
    /// (with a redraw) once elapsed; rendering only needs to know whether
    /// `now < deadline`.
    reset_flash_until: Option<Instant>,
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
            SettingsRow {
                draft: RowDraft::ScrollbackLimit(init.scrollback_limit),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::CursorStyleBlink(init.cursor_style_blink.unwrap_or(true)),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::MinimumContrast(init.minimum_contrast),
                touched: false,
            },
            SettingsRow {
                draft: RowDraft::MacosOptionAsAlt(init.macos_option_as_alt),
                touched: false,
            },
        ];
        let mut settings = ThemeSettings {
            mode: init.mode,
            section: init.mode.fixed_section(),
            filter: String::new(),
            filtered: Vec::new(),
            highlighted: 0,
            highlight_moved: false,
            selected_row: 0,
            rows,
            snapshot,
            font_size_debounce: Debouncer::new(FONT_SIZE_DEBOUNCE_WINDOW),
            font_size_digits: None,
            background_image_text: None,
            opaque_at_startup: init.background_opacity >= 1.0,
            available_font_families: init.available_font_families,
            commit_error: None,
            settings_search_active: false,
            settings_filter: String::new(),
            settings_filtered: Vec::new(),
            settings_highlight: 0,
            settings_pre_search_selected: 0,
            reset_flash_until: None,
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

    /// R-1/R-11: why `row` should show the "applies after restart" note
    /// instead of a live preview right now. Two independent cases: a *live*
    /// opacity/blur row whose session started opaque (R-11's original
    /// case — `FontSize`/`CursorStyle` always apply live regardless), or
    /// any *commit-only* row (`FontFamily`/`WindowPadding`/
    /// `MacosTitlebarStyle`) the user has actually edited — those have
    /// no runtime-apply path at all (`App::commit_theme_settings`), so a
    /// touched edit persists to config but only takes effect on the next
    /// launch. The two cases carry distinct [`RestartReason`] variants so
    /// the UI can explain *why* (AC-1/AC-2) instead of one blanket note.
    pub(crate) fn restart_reason(&self, row: SettingsRowKind) -> RestartReason {
        if row.is_live() {
            return if self.opaque_at_startup
                && matches!(
                    row,
                    SettingsRowKind::BackgroundOpacity | SettingsRowKind::BackgroundBlurRadius
                ) {
                RestartReason::OpaqueStartup
            } else {
                RestartReason::None
            };
        }
        if is_reload_exempt(row) {
            return RestartReason::None;
        }
        let index = SettingsRowKind::ALL
            .iter()
            .position(|kind| *kind == row)
            .expect("SettingsRowKind::ALL contains every variant");
        if self.rows[index].touched {
            RestartReason::CommitOnly
        } else {
            RestartReason::None
        }
    }

    /// Compatibility wrapper (Addendum C-2): the 28 existing test call sites
    /// keep calling this `bool` form unchanged. New code calls
    /// [`Self::restart_reason`] directly, so this has no production caller
    /// left — kept `pub(crate)` rather than `#[cfg(test)]` per C-2's "thin
    /// compatibility wrapper" framing, mirroring `opaque_at_startup` above.
    #[allow(dead_code)]
    pub(crate) fn restart_note(&self, row: SettingsRowKind) -> bool {
        self.restart_reason(row) != RestartReason::None
    }

    /// R-3: the always-visible live/next-launch/on-save badge for `row`,
    /// independent of `touched` (never lies — this is the same value the
    /// instant the overlay opens as after any amount of editing). C-6: a
    /// live-class row downgraded by [`RestartReason::OpaqueStartup`]
    /// reports its *effective* liveness (`OnLaunch`) for this session, not
    /// the static [`SettingsRowKind::is_live`] classification. A
    /// reload-exempt row (fix F1) reports `OnSave`, not `OnLaunch` — it has
    /// no live-preview-while-editing path, but `App::commit_theme_settings`
    /// (`sync_config_from_committed_live_rows`) fully applies it the moment
    /// it's saved, unlike a genuine restart-only row (`FontFamily`/
    /// `WindowPadding`/`MacosTitlebarStyle`), which persists to config but
    /// changes nothing this session.
    pub(crate) fn liveness(&self, row: SettingsRowKind) -> Liveness {
        if row.is_live() {
            if self.restart_reason(row) == RestartReason::OpaqueStartup {
                Liveness::OnLaunch
            } else {
                Liveness::Live
            }
        } else if is_reload_exempt(row) {
            Liveness::OnSave
        } else {
            Liveness::OnLaunch
        }
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

    /// ↑↓: theme-list highlight in `ThemePicker`, row selection (or, while
    /// R-5 search is active, the search highlight over `settings_filtered` —
    /// a separate index space, Addendum D-3/FM-02) in `SettingsRows` — never
    /// a value adjustment (R-2).
    pub(crate) fn move_up(&mut self) {
        match self.section {
            Section::ThemePicker => {
                if self.highlighted > 0 {
                    self.highlighted -= 1;
                    self.highlight_moved = true;
                }
            }
            Section::SettingsRows => {
                if self.settings_search_active {
                    if self.settings_highlight > 0 {
                        self.settings_highlight -= 1;
                    }
                    return;
                }
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
                if self.settings_search_active {
                    if !self.settings_filtered.is_empty()
                        && self.settings_highlight + 1 < self.settings_filtered.len()
                    {
                        self.settings_highlight += 1;
                    }
                    return;
                }
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
                self.filter.push_str(&filtered);
                self.refilter_and_mark();
            }
            Section::SettingsRows => {
                if self.settings_search_active {
                    let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                    if filtered.is_empty() {
                        return;
                    }
                    self.settings_filter.push_str(&filtered);
                    self.recompute_settings_filtered();
                    return;
                }
                match SettingsRowKind::ALL[self.selected_row] {
                    SettingsRowKind::FontSize => self.push_font_size_digits(text, now),
                    SettingsRowKind::BackgroundImage => self.push_background_image_text(text),
                    _ => {}
                }
            }
        }
    }

    /// Backspace: pops one filter character in `ThemePicker`, or pops one
    /// digit from the in-progress font-size entry in `SettingsRows`.
    pub(crate) fn backspace(&mut self, now: Instant) {
        match self.section {
            Section::ThemePicker => {
                if self.filter.pop().is_some() {
                    self.refilter_and_mark();
                }
            }
            Section::SettingsRows => {
                if self.settings_search_active {
                    if self.settings_filter.pop().is_some() {
                        self.recompute_settings_filtered();
                    }
                    return;
                }
                match SettingsRowKind::ALL[self.selected_row] {
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
                            let text =
                                self.background_image_text.get_or_insert_with(|| {
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
                }
            }
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
        if self.section != Section::SettingsRows || delta == 0 || self.settings_search_active {
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
            // R-9: all four rows are persist-only (no runtime-apply path
            // from this row directly — the reload-exempt three still show
            // `Liveness::OnSave` because `ConfigWatcher` re-applies them
            // after the commit lands, not because `adjust` does).
            SettingsRowKind::ScrollbackLimit => {
                let RowDraft::ScrollbackLimit(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let step = delta.unsigned_abs() as usize * SCROLLBACK_LIMIT_STEP;
                let new = if delta.is_negative() {
                    current.saturating_sub(step)
                } else if current >= SCROLLBACK_LIMIT_MAX {
                    // G2: a config-set value can start above the UI ceiling
                    // (this row's own steps can never produce that, but a
                    // hand-edited config file can) — clamping the increase
                    // down to the ceiling would make the increase key
                    // *decrease* the value, so it's a no-op instead.
                    current
                } else {
                    current.saturating_add(step).min(SCROLLBACK_LIMIT_MAX)
                };
                if new != current {
                    self.rows[idx].draft = RowDraft::ScrollbackLimit(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::CursorStyleBlink => {
                let RowDraft::CursorStyleBlink(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = !current;
                self.rows[idx].draft = RowDraft::CursorStyleBlink(new);
                self.rows[idx].touched = true;
                RowEffect::None
            }
            SettingsRowKind::MinimumContrast => {
                let RowDraft::MinimumContrast(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = (current + delta as f32 * MINIMUM_CONTRAST_STEP)
                    .clamp(MINIMUM_CONTRAST_MIN, MINIMUM_CONTRAST_MAX);
                if (new - current).abs() > f32::EPSILON {
                    self.rows[idx].draft = RowDraft::MinimumContrast(new);
                    self.rows[idx].touched = true;
                }
                RowEffect::None
            }
            SettingsRowKind::MacosOptionAsAlt => {
                let RowDraft::MacosOptionAsAlt(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[
                        MacosOptionAsAlt::None,
                        MacosOptionAsAlt::Left,
                        MacosOptionAsAlt::Right,
                        MacosOptionAsAlt::Both,
                    ],
                    current,
                    delta,
                );
                if new != current {
                    self.rows[idx].draft = RowDraft::MacosOptionAsAlt(new);
                    self.rows[idx].touched = true;
                }
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

    fn recompute_filtered(&mut self) {
        self.filtered = filter_themes(&self.filter);
        self.highlighted = 0;
    }

    /// R-5: whether the row-search modal sub-state currently owns ↑↓/text
    /// input in `Section::SettingsRows`.
    pub(crate) fn settings_search_active(&self) -> bool {
        self.settings_search_active
    }

    pub(crate) fn settings_filter(&self) -> &str {
        &self.settings_filter
    }

    pub(crate) fn settings_filtered_len(&self) -> usize {
        self.settings_filtered.len()
    }

    pub(crate) fn settings_highlighted_index(&self) -> usize {
        self.settings_highlight
    }

    /// The `SettingsRowKind::ALL`/`rows` index at filtered position `i`, or
    /// `None` past the end (AC-14: an empty result never panics on lookup).
    pub(crate) fn settings_filtered_row_index(&self, i: usize) -> Option<usize> {
        self.settings_filtered.get(i).copied()
    }

    /// Tab (R-5): enter search (seed the full row list, remember the
    /// pre-search selection) if inactive, or exit *without* confirming if
    /// already active — Addendum B: Tab-exit restores the pre-search
    /// selection, unlike Enter which confirms the highlight (see
    /// [`Self::confirm_settings_search`]). Only meaningful in
    /// `Section::SettingsRows`; Theme mode's Tab never calls this
    /// (`toggle_section` stays its existing no-op).
    pub(crate) fn toggle_settings_search(&mut self) {
        if self.settings_search_active {
            self.settings_search_active = false;
            self.selected_row = self.settings_pre_search_selected;
            self.clear_row_input_state();
            return;
        }
        self.settings_pre_search_selected = self.selected_row;
        self.settings_search_active = true;
        self.settings_filter.clear();
        self.recompute_settings_filtered();
        self.settings_highlight = self
            .settings_filtered
            .iter()
            .position(|&idx| idx == self.selected_row)
            .unwrap_or(0);
        self.clear_row_input_state();
    }

    /// Enter while searching (R-5/Addendum B): commit the highlighted match
    /// as the row selection and leave search. Never touches config/commit —
    /// the router gates this before `commit_theme_settings` ever runs
    /// (Addendum D-3/FM-02). A no-op selection change (but still exits
    /// search) when the filtered list is empty.
    pub(crate) fn confirm_settings_search(&mut self) {
        if let Some(&idx) = self.settings_filtered.get(self.settings_highlight) {
            self.selected_row = idx;
        }
        self.settings_search_active = false;
        self.clear_row_input_state();
    }

    fn recompute_settings_filtered(&mut self) {
        self.settings_filtered = filter_settings_rows(&self.settings_filter);
        self.settings_highlight = 0;
    }

    /// Delete / Cmd+Backspace (R-7): replace the selected row's draft with
    /// [`RowDraft::default_for`]'s `StartupConfig::default()`-derived value
    /// and mark it touched — an explicit reset always marks touched
    /// (AC-19), even when the default happens to equal the untouched
    /// snapshot, so the user's intent is never silently dropped by
    /// `commit_updates()`'s touched-gate. Clears any in-progress digit/path
    /// entry exactly like navigation does (Addendum D-3/FM-06) so a stale
    /// buffer can't resurrect the pre-reset value on the next keystroke. A
    /// no-op (both in effect and in the flash cue) outside
    /// `Section::SettingsRows` or while search is active — search owns the
    /// keyboard's editing semantics while it's up.
    pub(crate) fn reset_selected_row(&mut self, now: Instant) -> RowEffect {
        if self.section != Section::SettingsRows || self.settings_search_active {
            return RowEffect::None;
        }
        let idx = self.selected_row;
        let kind = SettingsRowKind::ALL[idx];
        let default = RowDraft::default_for(kind);
        self.rows[idx].draft = default.clone();
        self.rows[idx].touched = true;
        self.clear_row_input_state();
        // G1: `FontFamily`'s default is always the empty string (fix F2),
        // which `commit_updates()` deliberately never writes (noa-config's
        // writer has no key-deletion primitive, so an empty value would be
        // an invalid `font-family = ` line rather than a meaningful
        // "unset"). Flashing here would tell the user the reset "worked"
        // for a save that will actually write nothing — so this one case
        // skips the flash. Every other row's reset always writes on
        // commit, so it keeps the flash.
        let commits_nothing_on_save =
            matches!(&self.rows[idx].draft, RowDraft::FontFamily(name) if name.is_empty());
        if !commits_nothing_on_save {
            self.reset_flash_until = Some(now + RESET_FLASH_DURATION);
        }
        match (kind, default) {
            // Font-size never returns a live `RowEffect` directly — like
            // every other edit to this row, it always routes through the
            // debouncer (`poll_font_size`), per R-9's existing contract.
            (SettingsRowKind::FontSize, RowDraft::FontSize(value)) => {
                self.font_size_debounce.submit(value, now);
                RowEffect::None
            }
            (SettingsRowKind::BackgroundOpacity, RowDraft::BackgroundOpacity(value)) => {
                if self.opaque_at_startup {
                    RowEffect::None
                } else {
                    RowEffect::Opacity(value)
                }
            }
            (SettingsRowKind::BackgroundBlurRadius, RowDraft::BackgroundBlurRadius(value)) => {
                if self.opaque_at_startup {
                    RowEffect::None
                } else {
                    RowEffect::Blur(value)
                }
            }
            (SettingsRowKind::CursorStyle, RowDraft::CursorStyle(value)) => {
                RowEffect::CursorStyle(value)
            }
            (SettingsRowKind::SidebarPreviewLines, RowDraft::SidebarPreviewLines(value)) => {
                RowEffect::SidebarPreviewLines(value)
            }
            _ => RowEffect::None,
        }
    }

    /// R-7/C-5: whether the post-Reset highlight is still showing at `now` —
    /// the view-model build reads this every frame while the overlay is
    /// open.
    pub(crate) fn reset_flash_active(&self, now: Instant) -> bool {
        self.reset_flash_until.is_some_and(|until| now < until)
    }

    /// `App`'s timer tick (mirrors [`Self::poll_font_size`]'s poll shape):
    /// clears an elapsed flash and reports whether it just turned off — a
    /// `true` return means `App` must force one more redraw, since nothing
    /// else would repaint an otherwise-idle overlay right at the deadline.
    pub(crate) fn poll_reset_flash(&mut self, now: Instant) -> bool {
        if self.reset_flash_until.is_some_and(|until| now >= until) {
            self.reset_flash_until = None;
            return true;
        }
        false
    }

    /// The still-pending flash deadline, if any — `App`'s timer tick uses
    /// this to keep re-arming its wake-up until the flash actually clears
    /// (NFR-2: no busy-polling once it has).
    pub(crate) fn reset_flash_deadline(&self) -> Option<Instant> {
        self.reset_flash_until
    }

    /// Re-filter from `self.filter` and mark the highlight moved — unless
    /// the new filter matches nothing, in which case the picker stays empty
    /// without disturbing the last preview (AC-16).
    fn refilter_and_mark(&mut self) {
        self.recompute_filtered();
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
            updates.push(("theme".to_string(), name.to_string()));
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
                // Fix F2: an empty name is `RowDraft::default_for`'s reset
                // value (`StartupConfig::default().font.families` is empty
                // — "no override configured", the same value
                // `App::open_theme_settings` itself would have seeded from
                // an empty `self.config.font.families`). Writing a bare
                // `font-family = ` line would be a config value no parser
                // reads as "no override" — it would instead try to resolve
                // the literal empty string as a font. `write_config_updates`
                // has no key-deletion primitive (`noa-config/src/writer.rs`
                // only rewrites-in-place or appends), so a pre-existing
                // `font-family = X` line in the file is left as-is rather
                // than either deleting it or emitting an invalid value —
                // the row still resets in memory and `touched` still marks
                // the edit as intentional (AC-19), only the write is
                // skipped.
                RowDraft::FontFamily(name) if name.is_empty() => {}
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
                RowDraft::ScrollbackLimit(bytes) => {
                    updates.push(("scrollback-limit".to_string(), bytes.to_string()));
                }
                RowDraft::CursorStyleBlink(blink) => {
                    updates.push(("cursor-style-blink".to_string(), blink.to_string()));
                }
                RowDraft::MinimumContrast(v) => {
                    updates.push(("minimum-contrast".to_string(), format!("{v}")));
                }
                RowDraft::MacosOptionAsAlt(mode) => {
                    updates.push((
                        "macos-option-as-alt".to_string(),
                        macos_option_as_alt_config_value(*mode).to_string(),
                    ));
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
}

/// Fix F1: the non-live rows with no live-preview-while-editing path but a
/// full runtime apply the moment they're saved (`Enter` →
/// `App::commit_theme_settings`'s `sync_config_from_committed_live_rows`,
/// which mirrors every one of these into `self.config`/re-applies it
/// immediately) — distinct from a genuine restart-only row (`FontFamily`/
/// `WindowPadding`/`MacosTitlebarStyle`, deliberately excluded from that
/// same mirroring function, which persists to config but changes nothing
/// this session). Shared by [`ThemeSettings::restart_reason`] (always
/// `None` here — nothing to explain waiting on a restart) and
/// [`ThemeSettings::liveness`] (badges these `OnSave`, not `OnLaunch`) so
/// the two lists can never drift apart.
fn is_reload_exempt(row: SettingsRowKind) -> bool {
    matches!(
        row,
        SettingsRowKind::BackgroundImage
            | SettingsRowKind::BackgroundImageOpacity
            | SettingsRowKind::BackgroundImagePosition
            | SettingsRowKind::BackgroundImageFit
            | SettingsRowKind::BackgroundImageRepeat
            | SettingsRowKind::BackgroundImageInterval
            | SettingsRowKind::ConfirmQuit
            | SettingsRowKind::QuickTerminalHeight
            // R-9/Addendum D-1's FM-01 correction: these three are picked up
            // by `ConfigWatcher`'s 500ms poll (`app/config_reload.rs`'s
            // `terminal_policy_inputs_changed`/`cursor_inputs_changed`/
            // `theme_inputs_changed`) after any config write, including the
            // Settings panel's own commit — `macos-option-as-alt` is
            // deliberately absent (read only at pty spawn, genuinely
            // persist-only).
            | SettingsRowKind::ScrollbackLimit
            | SettingsRowKind::CursorStyleBlink
            | SettingsRowKind::MinimumContrast
    )
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

/// `macos-option-as-alt` config value for `mode` (inverse of
/// `parse_macos_option_as_alt`; `"false"`/`"true"`/`"only-left"`/
/// `"only-right"` are parse-only aliases — the write side always emits the
/// canonical `"none"`/`"both"`/`"left"`/`"right"`).
fn macos_option_as_alt_config_value(mode: MacosOptionAsAlt) -> &'static str {
    match mode {
        MacosOptionAsAlt::None => "none",
        MacosOptionAsAlt::Left => "left",
        MacosOptionAsAlt::Right => "right",
        MacosOptionAsAlt::Both => "both",
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

/// The full theme catalog fuzzy-filtered by `filter`, best match first,
/// reusing [`fuzzy_match`] (no second matcher, per the contract). An empty
/// filter matches every entry in catalog order (score 0, no highlight),
/// mirroring [`crate::command_palette::command_palette_matches`]'s empty-query
/// behavior.
fn filter_themes(filter: &str) -> Vec<ThemeMatch> {
    let mut matches: Vec<(i32, ThemeMatch)> = noa_theme::THEMES
        .iter()
        .enumerate()
        .filter_map(|(index, (name, _))| {
            fuzzy_match(filter, name)
                .map(|(score, positions)| (score, ThemeMatch { index, positions }))
        })
        .collect();
    matches.sort_by_key(|b| std::cmp::Reverse(b.0));
    matches.into_iter().map(|(_, m)| m).collect()
}

/// R-5: `SettingsRowKind::ALL` fuzzy-filtered by label, best match first,
/// reusing [`fuzzy_match`] like [`filter_themes`] does. An empty filter
/// matches every row in `ALL` order (AC-15), mirroring `filter_themes`'s own
/// empty-query behavior.
fn filter_settings_rows(filter: &str) -> Vec<usize> {
    let mut matches: Vec<(i32, usize)> = SettingsRowKind::ALL
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| {
            fuzzy_match(filter, kind.label()).map(|(score, _)| (score, index))
        })
        .collect();
    matches.sort_by_key(|b| std::cmp::Reverse(b.0));
    matches.into_iter().map(|(_, idx)| idx).collect()
}
