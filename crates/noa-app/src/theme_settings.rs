//! Theme-settings overlay (theme-settings-ui) — the GUI-agnostic half.
//! Mirrors `command_palette.rs`/`search_prompt.rs`: pure state + pure logic
//! with no winit/window/GPU types, so the state machine is unit-testable
//! without a display. `App` owns a `ThemeSettingsSession` wrapping
//! [`ThemeSettings`]; its `KeyboardInput` handler drives it, applies the
//! live-preview side effects ([`RowEffect`]) to `GpuState`/live terminals,
//! and feeds the rendered result into the overlay card (mirroring the
//! command palette's own card).
//!
//! Increment D landed the picker/rows/live-preview/Esc-revert state machine
//! plus the sample-pane data (R-1..R-11, R-16). Increment E adds the Enter
//! commit sequence's pure half: [`ThemeSettings::commit_updates`] (the
//! config write's payload) and [`ThemeSettings::commit`] (the injectable
//! write call itself, R-12); `App::commit_theme_settings`
//! (`app/input_ops.rs`) drives the GPU/window side effects that follow a
//! successful write.

use std::io;
use std::path::Path;
use std::time::Instant;

use noa_config::{CursorShape, MacosTitlebarStyle};
use noa_core::Rgb;

use crate::command_palette::fuzzy_match;
use crate::debounce::Debouncer;

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
/// Background-blur-radius step per ←→ press, and its config-documented cap
/// (`noa-config`'s `background_blur_radius` doc comment: `0..=64`).
const BLUR_STEP: i32 = 1;
const BLUR_MAX: u16 = 64;
/// Window-padding step per ←→ press (both x and y move together — a single
/// row adjusts uniform padding; see [`ThemeSettings::adjust`]'s doc for why).
const WINDOW_PADDING_STEP: f32 = 1.0;
/// Sidebar preview line count step per ←→ press.
const SIDEBAR_PREVIEW_LINES_STEP: i32 = 1;

/// Which half of the overlay currently owns ↑↓/←→ navigation (R-2). Tab
/// toggles between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    ThemePicker,
    SettingsRows,
}

/// The fixed settings rows (SHAPE table), in display/array order.
/// `SettingsRow` storage in [`ThemeSettings::rows`] uses this same order, so
/// `ALL[i]` always names the kind stored at `rows[i]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsRowKind {
    FontSize,
    BackgroundOpacity,
    BackgroundBlurRadius,
    CursorStyle,
    FontFamily,
    WindowPadding,
    MacosTitlebarStyle,
    SidebarPreviewLines,
    ConfirmQuit,
}

impl SettingsRowKind {
    pub(crate) const COUNT: usize = 9;
    pub(crate) const ALL: [SettingsRowKind; Self::COUNT] = [
        Self::FontSize,
        Self::BackgroundOpacity,
        Self::BackgroundBlurRadius,
        Self::CursorStyle,
        Self::FontFamily,
        Self::WindowPadding,
        Self::MacosTitlebarStyle,
        Self::SidebarPreviewLines,
        Self::ConfirmQuit,
    ];

    /// R-8: the fixed live/commit-only classification, one row's kind at a
    /// time — never toggled at runtime.
    pub(crate) fn is_live(self) -> bool {
        matches!(
            self,
            Self::FontSize
                | Self::BackgroundOpacity
                | Self::BackgroundBlurRadius
                | Self::CursorStyle
                | Self::SidebarPreviewLines
        )
    }

    /// Row label. Deviation: the spec's SHAPE table and prose are Japanese,
    /// but every existing noa UI string (command palette titles, menu items)
    /// is English (see `command_palette.rs`'s title registry) — these labels
    /// follow that established convention instead of the spec's literal JP
    /// text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::FontSize => "Font Size",
            Self::BackgroundOpacity => "Background Opacity",
            Self::BackgroundBlurRadius => "Background Blur Radius",
            Self::CursorStyle => "Cursor Style",
            Self::FontFamily => "Font Family",
            Self::WindowPadding => "Window Padding",
            Self::MacosTitlebarStyle => "Titlebar Style",
            Self::SidebarPreviewLines => "Sidebar Preview Lines",
            Self::ConfirmQuit => "Confirm Quit",
        }
    }
}

/// One settings row's current draft value, keyed by [`SettingsRowKind`] (the
/// variant always matches `SettingsRowKind::ALL[i]` for `rows[i]`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RowDraft {
    FontSize(f32),
    BackgroundOpacity(f32),
    BackgroundBlurRadius(u16),
    CursorStyle(CursorShape),
    FontFamily(String),
    WindowPadding(f32, f32),
    MacosTitlebarStyle(MacosTitlebarStyle),
    SidebarPreviewLines(usize),
    ConfirmQuit(bool),
}

/// One settings row: its draft value and whether the user has actually
/// edited it (pre-mortem RPN 252 — only `touched` rows may ever reach the
/// Increment E writer; navigation/rendering must never flip this).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SettingsRow {
    pub(crate) draft: RowDraft,
    pub(crate) touched: bool,
}

/// The side effect `App` must apply outside the pure state machine after an
/// [`ThemeSettings::adjust`] call. Font-size has no immediate effect here —
/// it always routes through the debouncer (`poll_font_size`), per R-9.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowEffect {
    /// Nothing to apply outside the state machine (commit-only rows, or a
    /// live row whose value didn't actually change).
    None,
    /// Cursor-style changed and must be applied to every live terminal now
    /// (R-10: immediate).
    CursorStyle(CursorShape),
    /// Background opacity changed; `None` restart-note case is signaled
    /// separately by [`ThemeSettings::opaque_at_startup`] — `App` checks
    /// that before treating this as a live apply (R-11).
    Opacity(f32),
    /// Background blur radius changed; same opaque-at-startup gating as
    /// `Opacity`.
    Blur(u16),
    /// Sidebar card preview line count changed and should apply immediately.
    SidebarPreviewLines(usize),
}

/// The pre-open snapshot every value reverts to on Esc (R-16). Also doubles
/// as the "initial highlight" reference for the theme picker (SHAPE: initial
/// highlight = the currently active theme).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RevertValues {
    pub(crate) theme_name: String,
    pub(crate) font_size: f32,
    pub(crate) cursor_style: CursorShape,
    pub(crate) background_opacity: f32,
    pub(crate) background_blur_radius: u16,
    pub(crate) sidebar_preview_lines: usize,
}

/// Everything `App` must supply to open the overlay — the session's live
/// values at the moment `cmd`+palette-entry is invoked, plus the font-family
/// discovery list (queried once by `App` via `noa_font::list_families`, kept
/// out of this pure module so it stays deterministic/testable without
/// font-kit).
pub(crate) struct ThemeSettingsInit {
    pub(crate) current_theme: String,
    pub(crate) font_size: f32,
    pub(crate) cursor_style: CursorShape,
    pub(crate) background_opacity: f32,
    pub(crate) background_blur_radius: u16,
    pub(crate) window_padding_x: f32,
    pub(crate) window_padding_y: f32,
    pub(crate) macos_titlebar_style: MacosTitlebarStyle,
    pub(crate) sidebar_preview_lines: usize,
    pub(crate) confirm_quit: bool,
    pub(crate) font_family: String,
    pub(crate) available_font_families: Vec<String>,
}

/// One theme catalog match: an index into `noa_theme::THEMES` plus the fuzzy
/// match char positions (for highlight rendering), reusing
/// [`crate::command_palette::fuzzy_match`] rather than a second matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeMatch {
    index: usize,
    positions: Vec<usize>,
}

/// A swatch shown in the sample pane (R-5): the 16 ANSI palette entries,
/// fg/bg/cursor/selection, and one fixed truecolor sample — all derived from
/// a `ThemeDef`, never hand-authored (AC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Swatch {
    /// One of the 16 base ANSI palette slots (0..16) and its resolved color.
    Ansi(u8, Rgb),
    Foreground(Rgb),
    Background(Rgb),
    Cursor(Rgb),
    Selection(Rgb),
    /// A fixed truecolor sample outside the 16-slot palette — proves the
    /// pane isn't limited to indexed color (R-5's "truecolor見本").
    Truecolor(Rgb),
}

/// The sample-pane swatch list for `theme` (AC-3): 16 ANSI + 4 semantic +
/// 1 truecolor, always in this fixed order.
pub(crate) fn sample_swatches(theme: &noa_theme::ThemeDef) -> Vec<Swatch> {
    let mut swatches = Vec::with_capacity(16 + 4 + 1);
    for (index, color) in theme.palette.iter().take(16).enumerate() {
        swatches.push(Swatch::Ansi(index as u8, *color));
    }
    swatches.push(Swatch::Foreground(theme.default_fg));
    swatches.push(Swatch::Background(theme.default_bg));
    swatches.push(Swatch::Cursor(theme.cursor));
    swatches.push(Swatch::Selection(theme.selection_bg));
    swatches.push(Swatch::Truecolor(Rgb::new(0x40, 0x80, 0xc0)));
    swatches
}

/// The open theme-settings overlay's editable state (R-2..R-11, R-16). Holds
/// no window/GPU binding of its own — that lives in the `App`-side session,
/// mirroring [`crate::command_palette::CommandPalette`].
///
/// `Clone` exists solely so `App::redraw` can snapshot it out early (like
/// [`crate::command_palette::CommandPalette`]'s render payload) without
/// holding a live borrow of `App::theme_settings` across the redraw's later
/// `&mut self` calls — the catalog-sized `filtered` list makes this a real
/// (if small) per-frame allocation while the overlay is open, which is an
/// accepted deviation for this increment rather than a proper zero-copy
/// render-payload type (mirroring `command_palette_snapshot`'s approach)
/// that a follow-up could still add if this ever shows up on a profile.
#[derive(Clone)]
pub(crate) struct ThemeSettings {
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
            sidebar_preview_lines: init.sidebar_preview_lines,
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
                draft: RowDraft::ConfirmQuit(init.confirm_quit),
                touched: false,
            },
        ];
        let mut settings = ThemeSettings {
            section: Section::ThemePicker,
            filter: String::new(),
            filtered: Vec::new(),
            highlighted: 0,
            highlight_moved: false,
            selected_row: 0,
            rows,
            snapshot,
            font_size_debounce: Debouncer::new(FONT_SIZE_DEBOUNCE_WINDOW),
            font_size_digits: None,
            opaque_at_startup: init.background_opacity >= 1.0,
            available_font_families: init.available_font_families,
            commit_error: None,
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

    /// Tab: swap which half of the overlay owns ↑↓/←→ (R-2, AC-22).
    pub(crate) fn toggle_section(&mut self) {
        self.section = match self.section {
            Section::ThemePicker => Section::SettingsRows,
            Section::SettingsRows => Section::ThemePicker,
        };
    }

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
        if matches!(row, SettingsRowKind::ConfirmQuit) {
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
                    self.font_size_digits = None;
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
                    self.font_size_digits = None;
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
            Section::SettingsRows => self.push_font_size_digits(text, now),
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
                if SettingsRowKind::ALL[self.selected_row] != SettingsRowKind::FontSize {
                    return;
                }
                if let Some(digits) = &mut self.font_size_digits {
                    digits.pop();
                    if let Ok(value) = digits.parse::<f32>() {
                        self.set_font_size(value, now);
                    }
                }
            }
        }
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
            SettingsRowKind::CursorStyle => {
                let RowDraft::CursorStyle(current) = self.rows[idx].draft else {
                    return RowEffect::None;
                };
                let new = cycle(
                    &[CursorShape::Block, CursorShape::Bar, CursorShape::Underline],
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

    fn recompute_filtered(&mut self) {
        self.filtered = filter_themes(&self.filter);
        self.highlighted = 0;
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
}

/// `cursor-style` config value for `shape` (mirrors
/// `noa_config::parser::values::parse_cursor_style`'s inverse — that parser
/// has no matching serializer, so the write side owns this mapping).
fn cursor_shape_config_value(shape: CursorShape) -> &'static str {
    match shape {
        CursorShape::Block => "block",
        CursorShape::Bar => "bar",
        CursorShape::Underline => "underline",
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
    matches.sort_by(|a, b| b.0.cmp(&a.0));
    matches.into_iter().map(|(_, m)| m).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn init() -> ThemeSettingsInit {
        ThemeSettingsInit {
            current_theme: "3024 Day".to_string(),
            font_size: 14.0,
            cursor_style: CursorShape::Block,
            background_opacity: 1.0,
            background_blur_radius: 0,
            window_padding_x: 2.0,
            window_padding_y: 2.0,
            macos_titlebar_style: MacosTitlebarStyle::Native,
            sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
            confirm_quit: true,
            font_family: "Menlo".to_string(),
            available_font_families: vec![
                "Menlo".to_string(),
                "Monaco".to_string(),
                "Courier New".to_string(),
            ],
        }
    }

    fn transparent_init() -> ThemeSettingsInit {
        ThemeSettingsInit {
            background_opacity: 0.9,
            ..init()
        }
    }

    // AC-3 (R-5): the sample pane's data carries all 16 ANSI slots plus
    // fg/bg/cursor/selection plus a truecolor sample, for a known theme.
    #[test]
    fn sample_swatches_cover_ansi_and_semantic_and_truecolor() {
        let theme = noa_theme::resolve("3024 Day").expect("bundled theme exists");
        let swatches = sample_swatches(theme);

        let ansi_count = swatches
            .iter()
            .filter(|s| matches!(s, Swatch::Ansi(_, _)))
            .count();
        assert_eq!(ansi_count, 16);
        for i in 0..16u8 {
            assert!(
                swatches
                    .iter()
                    .any(|s| matches!(s, Swatch::Ansi(idx, color) if *idx == i && *color == theme.palette[i as usize])),
                "missing ANSI slot {i}"
            );
        }
        assert!(swatches.contains(&Swatch::Foreground(theme.default_fg)));
        assert!(swatches.contains(&Swatch::Background(theme.default_bg)));
        assert!(swatches.contains(&Swatch::Cursor(theme.cursor)));
        assert!(swatches.contains(&Swatch::Selection(theme.selection_bg)));
        assert!(swatches.iter().any(|s| matches!(s, Swatch::Truecolor(_))));
    }

    // AC-21-adjacent (R-1): opening seeds the picker with the initial
    // highlight on the currently active theme and previews nothing yet.
    #[test]
    fn open_highlights_current_theme_and_previews_nothing_until_moved() {
        let settings = ThemeSettings::open(init());
        assert_eq!(settings.highlighted_theme_name(), Some("3024 Day"));
        assert!(!settings.should_preview());
        assert!(!settings.badge_visible());
        assert_eq!(settings.section(), Section::ThemePicker);
    }

    // AC-22 (R-2): Tab toggles section; ↑↓ navigates only (theme highlight in
    // ThemePicker, row selection in SettingsRows); ←→ adjusts only the
    // focused settings row's value and is a no-op in ThemePicker.
    #[test]
    fn tab_toggles_section_and_arrows_route_by_section() {
        let mut settings = ThemeSettings::open(init());
        assert_eq!(settings.section(), Section::ThemePicker);

        // ←→ is a no-op while the theme list owns the section.
        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::None);
        assert!(!settings.rows()[3].touched);

        // ↑↓ moves the theme highlight, one step from wherever the initial
        // highlight (the active theme's catalog position) landed.
        let initial_highlight = settings.highlighted_index();
        settings.move_down();
        assert_eq!(settings.highlighted_index(), initial_highlight + 1);
        assert!(settings.should_preview());

        settings.toggle_section();
        assert_eq!(settings.section(), Section::SettingsRows);
        assert_eq!(settings.selected_row(), 0);

        // ↑↓ now moves row selection, not the (unaffected) theme highlight.
        settings.move_down();
        settings.move_down();
        assert_eq!(settings.selected_row(), 2);
        assert_eq!(
            settings.highlighted_index(),
            initial_highlight + 1,
            "theme highlight untouched"
        );

        settings.toggle_section();
        assert_eq!(settings.section(), Section::ThemePicker);
    }

    // AC-5 (R-8, R-10): adjusting the cursor-style row cycles it and reports
    // an immediate-apply effect.
    #[test]
    fn cursor_style_row_cycles_and_applies_immediately() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..3 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::CursorStyle
        );

        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Bar));
        assert!(settings.rows()[3].touched);
        assert!(settings.badge_visible());

        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Underline));

        // Wraps back to the front.
        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::CursorStyle(CursorShape::Block));
    }

    // AC-7a (R-11): starting opaque disables live opacity/blur apply and
    // flags the restart-required note, while the draft edit itself still
    // proceeds (the value can still be committed later).
    #[test]
    fn opaque_startup_disables_live_opacity_and_blur_but_keeps_draft() {
        let mut settings = ThemeSettings::open(init()); // opacity 1.0 = opaque
        assert!(settings.opaque_at_startup());
        assert!(settings.restart_note(SettingsRowKind::BackgroundOpacity));
        assert!(settings.restart_note(SettingsRowKind::BackgroundBlurRadius));
        assert!(!settings.restart_note(SettingsRowKind::CursorStyle));

        settings.toggle_section();
        settings.move_down(); // row 1: BackgroundOpacity

        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::None, "no live apply while opaque");
        assert_eq!(
            settings.rows()[1].draft,
            RowDraft::BackgroundOpacity(1.0),
            "already at the 1.0 ceiling, so the draft itself doesn't move"
        );

        // A transparent start does apply live.
        let mut transparent = ThemeSettings::open(transparent_init());
        assert!(!transparent.opaque_at_startup());
        transparent.toggle_section();
        transparent.move_down();
        let effect = transparent.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::Opacity(0.95));
    }

    // Amended spec (UX consistency): the commit-only rows persist to
    // config on commit but only take effect on the next launch, same as
    // opaque-startup opacity/blur — so a touched edit shows the same
    // "applies after restart" note. Untouched, no note; and the note is
    // independent of `opaque_at_startup` (a transparent-started session
    // still shows it for these rows).
    #[test]
    fn touched_commit_only_rows_show_restart_note() {
        let mut settings = ThemeSettings::open(transparent_init());
        assert!(!settings.restart_note(SettingsRowKind::FontFamily));
        assert!(!settings.restart_note(SettingsRowKind::WindowPadding));
        assert!(!settings.restart_note(SettingsRowKind::MacosTitlebarStyle));
        assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));
        assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));

        settings.toggle_section();
        for _ in 0..4 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::FontFamily
        );
        settings.adjust(1, Instant::now());
        assert!(settings.restart_note(SettingsRowKind::FontFamily));
        assert!(!settings.restart_note(SettingsRowKind::WindowPadding));
        assert!(!settings.restart_note(SettingsRowKind::MacosTitlebarStyle));
        assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));
        assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));
    }

    // AC-4a: the badge is invisible until either the theme highlight moves
    // or a live row is actually edited, and stays visible afterward.
    #[test]
    fn badge_tracks_preview_and_live_row_edits() {
        let mut settings = ThemeSettings::open(init());
        assert!(!settings.badge_visible());

        settings.move_down(); // theme highlight moves
        assert!(settings.badge_visible());
    }

    #[test]
    fn badge_visible_from_a_live_row_edit_alone() {
        let mut settings = ThemeSettings::open(transparent_init());
        settings.toggle_section();
        settings.move_down(); // BackgroundOpacity row
        assert!(!settings.badge_visible());
        settings.adjust(1, Instant::now());
        assert!(settings.badge_visible());
    }

    // touched-flag discipline: navigation alone (no value-changing key) must
    // never mark any row touched, live or commit-only.
    #[test]
    fn navigation_alone_never_marks_a_row_touched() {
        let mut settings = ThemeSettings::open(init());
        settings.move_up();
        settings.move_down();
        settings.toggle_section();
        for _ in 0..10 {
            settings.move_down();
            settings.move_up();
        }
        settings.toggle_section();
        assert!(settings.rows().iter().all(|row| !row.touched));
    }

    // AC-16 (R-4): filtering to zero matches empties the list without
    // resetting the preview flag that a prior highlight change had already
    // set — `App` simply keeps whatever `gpu.preview_theme` it last set,
    // since `highlighted_theme_name` returns `None` and `App` never
    // overwrites the preview on a `None`.
    #[test]
    fn zero_match_filter_keeps_previous_preview_state() {
        let mut settings = ThemeSettings::open(init());
        settings.move_down(); // establish a preview
        assert!(settings.should_preview());

        settings.push_text("zzzzzznosuchtheme", Instant::now());
        assert_eq!(settings.filtered_len(), 0);
        assert_eq!(settings.highlighted_theme_name(), None);
        // The flag that gates whether `App` resolves a preview at all stays
        // set — `App` just has nothing new to resolve into it this frame.
        assert!(settings.should_preview());
    }

    // AC-6 (R-9), exercised through the overlay's own font-size row rather
    // than `Debouncer` directly (already covered in `debounce.rs`): a burst
    // of ←→ presses fires once, 150ms after the last one, with the final
    // value.
    #[test]
    fn font_size_row_debounces_a_burst_of_adjustments() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section(); // row 0 = FontSize, already selected
        let t0 = Instant::now();

        settings.adjust(1, t0); // 14.5
        settings.adjust(1, t0 + Duration::from_millis(50)); // 15.0
        settings.adjust(1, t0 + Duration::from_millis(100)); // 15.5

        assert_eq!(
            settings.poll_font_size(t0 + Duration::from_millis(200)),
            None
        );
        assert_eq!(
            settings.poll_font_size(t0 + Duration::from_millis(250)),
            Some(15.5)
        );
        assert_eq!(
            settings.rows()[0].draft,
            RowDraft::FontSize(15.5),
            "the draft tracks live, independent of when the debounce fires"
        );
    }

    // Direct digit entry (R-2's "数値行は直接入力も可"): typing digits sets
    // the font-size row directly, and Backspace edits the same buffer.
    #[test]
    fn font_size_row_accepts_direct_digit_entry() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        let now = Instant::now();

        settings.push_text("2", now);
        settings.push_text("2", now);
        assert_eq!(settings.rows()[0].draft, RowDraft::FontSize(22.0));

        settings.backspace(now);
        assert_eq!(
            settings.rows()[0].draft,
            RowDraft::FontSize(FONT_SIZE_MIN),
            "typed \"2\" clamps up to the row's minimum"
        );
    }

    // AC-8-partial (R-16): Esc reverts to the pre-open snapshot values and
    // cancels a pending font-size debounce so it can never fire afterward —
    // no writer/config call is involved at this layer at all (the pure
    // module has no way to reach one).
    #[test]
    fn revert_returns_the_snapshot_and_cancels_pending_debounce() {
        let mut settings = ThemeSettings::open(init());
        settings.move_down(); // preview drifted
        settings.toggle_section();
        settings.adjust(1, Instant::now()); // font-size debounce now pending

        let values = settings.revert();
        assert_eq!(values.theme_name, "3024 Day");
        assert_eq!(values.font_size, 14.0);
        assert_eq!(values.cursor_style, CursorShape::Block);
        assert_eq!(values.background_opacity, 1.0);
        assert_eq!(values.background_blur_radius, 0);
        assert_eq!(
            values.sidebar_preview_lines,
            noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES
        );

        // The pending font-size value must never fire after revert.
        assert_eq!(
            settings.poll_font_size(Instant::now() + Duration::from_secs(1)),
            None
        );
    }

    // Font-family and titlebar-style rows cycle through their fixed/injected
    // option sets and wrap both directions (commit-only rows still track
    // touched correctly).
    #[test]
    fn font_family_and_titlebar_rows_cycle_and_wrap() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..4 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::FontFamily
        );
        settings.adjust(1, Instant::now());
        assert_eq!(
            settings.rows()[4].draft,
            RowDraft::FontFamily("Monaco".to_string())
        );
        settings.adjust(-1, Instant::now());
        settings.adjust(-1, Instant::now());
        assert_eq!(
            settings.rows()[4].draft,
            RowDraft::FontFamily("Courier New".to_string()),
            "wraps backward past the front"
        );

        settings.move_down();
        settings.move_down();
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::MacosTitlebarStyle
        );
        settings.adjust(1, Instant::now());
        assert_eq!(
            settings.rows()[6].draft,
            RowDraft::MacosTitlebarStyle(MacosTitlebarStyle::Transparent)
        );
        assert!(settings.rows()[6].touched);
    }

    // Window-padding row moves both axes together on one ←→ step (the
    // documented single-row-two-values simplification).
    #[test]
    fn window_padding_row_adjusts_both_axes_together() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..5 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::WindowPadding
        );
        settings.adjust(1, Instant::now());
        assert_eq!(settings.rows()[5].draft, RowDraft::WindowPadding(3.0, 3.0));
    }

    #[test]
    fn sidebar_preview_lines_row_adjusts_clamps_and_commits() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..7 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::SidebarPreviewLines
        );

        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::SidebarPreviewLines(4));
        assert_eq!(settings.rows()[7].draft, RowDraft::SidebarPreviewLines(4));
        assert!(!settings.restart_note(SettingsRowKind::SidebarPreviewLines));

        for _ in 0..20 {
            settings.adjust(1, Instant::now());
        }
        assert_eq!(
            settings.rows()[7].draft,
            RowDraft::SidebarPreviewLines(noa_config::MAX_SIDEBAR_PREVIEW_LINES)
        );
        for _ in 0..20 {
            settings.adjust(-1, Instant::now());
        }
        assert_eq!(settings.rows()[7].draft, RowDraft::SidebarPreviewLines(0));

        let updates = settings.commit_updates();
        assert_eq!(
            updates.iter().find(|(k, _)| k == "sidebar-preview-lines"),
            Some(&("sidebar-preview-lines".to_string(), "0".to_string()))
        );
    }

    #[test]
    fn confirm_quit_row_toggles_and_commits_without_restart_note() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..8 {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::ConfirmQuit
        );

        let effect = settings.adjust(1, Instant::now());
        assert_eq!(effect, RowEffect::None);
        assert_eq!(settings.rows()[8].draft, RowDraft::ConfirmQuit(false));
        assert!(!settings.restart_note(SettingsRowKind::ConfirmQuit));

        let updates = settings.commit_updates();
        assert_eq!(
            updates.iter().find(|(k, _)| k == "confirm-quit"),
            Some(&("confirm-quit".to_string(), "false".to_string()))
        );
    }

    // R-17/NFR-6 (commit_updates half of AC-14): an untouched row's draft can
    // equal the live session value even when that value came from a CLI
    // override — `commit_updates` must still omit it. Only a real edit
    // (`touched`) makes a row eligible for the update list; the theme
    // updates only when the highlight actually moved away from the snapshot.
    #[test]
    fn commit_updates_includes_only_the_changed_theme_and_touched_rows() {
        let settings = ThemeSettings::open(init());
        // Nothing touched, highlight never moved: no updates at all.
        assert!(settings.commit_updates().is_empty());

        let mut settings = ThemeSettings::open(init());
        settings.move_down(); // theme highlight moves away from the snapshot
        settings.toggle_section();
        settings.adjust(1, Instant::now()); // touches row 0: FontSize 14.0 -> 14.5

        let updates = settings.commit_updates();
        assert_eq!(
            updates
                .iter()
                .find(|(k, _)| k == "theme")
                .map(|(_, v)| v.as_str()),
            settings.highlighted_theme_name(),
            "theme update carries the new highlight, not the snapshot"
        );
        assert_eq!(
            updates.iter().find(|(k, _)| k == "font-size"),
            Some(&("font-size".to_string(), "14.5".to_string()))
        );
        // Every other row stayed untouched and must not appear, even though
        // e.g. cursor-style's draft is a perfectly valid config value.
        assert!(!updates.iter().any(|(k, _)| k == "cursor-style"));
        assert!(!updates.iter().any(|(k, _)| k == "background-opacity"));
        assert_eq!(updates.len(), 2, "theme + font-size only");
    }

    // Re-highlighting back onto the snapshot theme must not emit a `theme`
    // update — `commit_updates` compares against the pre-open value, not
    // "did the highlight ever move".
    #[test]
    fn commit_updates_omits_theme_when_highlight_returns_to_the_snapshot() {
        let mut settings = ThemeSettings::open(init());
        settings.move_down();
        settings.move_up();
        assert_eq!(settings.highlighted_theme_name(), Some("3024 Day"));
        assert!(!settings.commit_updates().iter().any(|(k, _)| k == "theme"));
    }

    // Window-padding is the one row that writes two keys from a single
    // touched flag.
    #[test]
    fn commit_updates_writes_both_padding_axes_from_one_row() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        for _ in 0..5 {
            settings.move_down();
        }
        settings.adjust(1, Instant::now());
        let updates = settings.commit_updates();
        assert_eq!(
            updates.iter().find(|(k, _)| k == "window-padding-x"),
            Some(&("window-padding-x".to_string(), "3".to_string()))
        );
        assert_eq!(
            updates.iter().find(|(k, _)| k == "window-padding-y"),
            Some(&("window-padding-y".to_string(), "3".to_string()))
        );
    }

    // AC-23: a failing injected writer records the display error, is called
    // exactly once, and leaves every other observable bit of state — rows,
    // touched flags, highlight/preview selection — exactly as it was. The
    // production caller (`App::commit_theme_settings`) never gets a
    // `Some(updates)` to act on, so it structurally cannot reach the
    // theme/chrome swap either.
    #[test]
    fn commit_with_failing_writer_sets_error_and_changes_nothing_else() {
        let mut settings = ThemeSettings::open(init());
        settings.move_down();
        settings.toggle_section();
        settings.adjust(1, Instant::now()); // touch FontSize
        let before_rows = settings.rows().clone();
        let before_highlighted = settings.highlighted_index();
        assert!(settings.commit_error().is_none());

        let mut calls = 0;
        let mut writer = |_: &Path, _: &[(String, String)]| {
            calls += 1;
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        };
        let result = settings.commit(Path::new("/nonexistent/noa/config"), &mut writer);

        assert!(result.is_none());
        assert_eq!(calls, 1);
        assert!(settings.commit_error().is_some());
        assert_eq!(
            *settings.rows(),
            before_rows,
            "drafts/touched untouched on failure"
        );
        assert_eq!(
            settings.highlighted_index(),
            before_highlighted,
            "preview selection untouched on failure"
        );
    }

    // A successful commit clears any error left over from an earlier failed
    // attempt (retry-after-fix flow) and hands back exactly the updates that
    // were passed to the writer.
    #[test]
    fn commit_success_clears_a_prior_error_and_returns_the_written_updates() {
        let mut settings = ThemeSettings::open(init());
        settings.toggle_section();
        settings.adjust(1, Instant::now()); // touch FontSize

        let mut fail_once = true;
        let mut writer = |_: &Path, _: &[(String, String)]| {
            if fail_once {
                fail_once = false;
                Err(io::Error::other("transient"))
            } else {
                Ok(())
            }
        };
        assert!(settings.commit(Path::new("/x"), &mut writer).is_none());
        assert!(settings.commit_error().is_some());

        let result = settings.commit(Path::new("/x"), &mut writer);
        assert!(
            settings.commit_error().is_none(),
            "success clears the error"
        );
        assert_eq!(
            result,
            Some(vec![("font-size".to_string(), "14.5".to_string())])
        );
    }

    // AC-8: Esc (`revert`) takes no writer parameter at all, so it is
    // structurally impossible for the Esc path to invoke one — this pins
    // that down with an actual spy closure that stays untouched across the
    // same edit sequence AC-23's failing-writer test exercises.
    #[test]
    fn esc_path_never_reaches_the_writer() {
        let mut settings = ThemeSettings::open(init());
        settings.move_down();
        settings.toggle_section();
        settings.adjust(1, Instant::now());

        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let spy_calls = calls.clone();
        let _writer = move |_: &Path, _: &[(String, String)]| -> io::Result<()> {
            spy_calls.set(spy_calls.get() + 1);
            Ok(())
        };

        let _ = settings.revert();
        assert_eq!(calls.get(), 0);
        assert!(
            settings.commit_error().is_none(),
            "Esc must not touch the commit-error flag either"
        );
    }

    // AC-14 (R-17, NFR-6) [integration, tempdir]: a config file on disk has
    // `font-size = 12` (X). The session opens with a CLI-overridden runtime
    // value of 20 (Y) — the overlay seeds its font-size draft from that
    // live value, exactly like a real `--font-size 20` launch would. The
    // user edits a *different* row (cursor-style) and commits: the written
    // file must keep `font-size = 12` (X), never `20` (Y) — the CLI value
    // never leaked in just because it was active. A second session then
    // edits font-size itself to Z and commits: now the file must contain
    // the edited value, not X or Y.
    #[test]
    fn ac14_cli_override_value_never_leaks_only_touched_rows_reach_disk() {
        let dir = std::env::temp_dir().join(format!(
            "noa-theme-settings-ac14-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config");
        std::fs::write(&config_path, "font-size = 12\ntheme = 3024 Day\n").unwrap();

        // Session 1: runtime font-size is 20 (as if `--font-size 20` had
        // overridden the file's 12), untouched; the user only edits
        // cursor-style.
        let mut untouched_session = ThemeSettings::open(ThemeSettingsInit {
            font_size: 20.0,
            ..init()
        });
        untouched_session.toggle_section();
        for _ in 0..3 {
            untouched_session.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[untouched_session.selected_row()],
            SettingsRowKind::CursorStyle
        );
        untouched_session.adjust(1, Instant::now());
        let mut writer = |path: &Path, updates: &[(String, String)]| {
            noa_config::write_config_updates(path, updates)
        };
        assert!(
            untouched_session
                .commit(&config_path, &mut writer)
                .is_some()
        );

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            contents.contains("font-size = 12"),
            "the CLI-overridden runtime value (20) must never leak in; got: {contents:?}"
        );
        assert!(contents.contains("cursor-style = bar"));

        // Session 2: the user now edits font-size itself and commits — the
        // new value must land, replacing X.
        let mut font_session = ThemeSettings::open(ThemeSettingsInit {
            font_size: 20.0,
            ..init()
        });
        font_session.toggle_section();
        font_session.adjust(2, Instant::now()); // 20.0 -> 21.0
        assert!(font_session.commit(&config_path, &mut writer).is_some());

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            contents.contains("font-size = 21"),
            "the user's committed edit must land; got: {contents:?}"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    // AC-19a (NFR-1): the preview-resolution path — resolving the
    // highlighted theme plus deriving the four color families R-6 calls
    // out — comfortably fits the 16ms@60Hz frame budget. Timed over many
    // iterations to smooth out one-off scheduling noise (the spec's Open
    // Questions explicitly allow relaxing this bound if CI proves flaky).
    #[test]
    fn preview_resolution_path_is_well_under_one_frame_budget() {
        let mut settings = ThemeSettings::open(init());
        let overrides = crate::theme::ThemeOverrides {
            background: None,
            foreground: None,
            cursor: None,
            selection_fg: None,
            selection_bg: None,
            minimum_contrast: 1.0,
        };

        let iterations = 100;
        let start = Instant::now();
        for i in 0..iterations {
            if i % 2 == 0 {
                settings.move_down();
            } else {
                settings.move_up();
            }
            let Some(name) = settings.highlighted_theme_name().map(str::to_string) else {
                continue;
            };
            let theme = crate::theme::resolve_theme_with_overrides(Some(&name), &overrides);
            let _ = noa_render::OverlayStyle::from_theme(&theme);
        }
        let mean = start.elapsed() / iterations;
        assert!(
            mean < Duration::from_millis(16),
            "mean preview-resolution time {mean:?} exceeded the 16ms@60Hz budget"
        );
    }
}
