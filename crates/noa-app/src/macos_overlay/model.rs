use std::hash::{Hash, Hasher};

use noa_render::OverlayStyle;

/// Opaque `CGColorRef` for `msg_send!` returns/arguments. The `-CGColor`
/// property returns `^{CGColor=}`, not an object (`@`) — typing it as
/// `*mut AnyObject` trips objc2's debug-mode encoding verification and
/// aborts. Shared with `macos_window.rs`.
#[cfg(target_os = "macos")]
pub(crate) mod cg {
    #[repr(C)]
    pub(crate) struct CGColor {
        _priv: [u8; 0],
    }
    // SAFETY: `CGColor` is a zero-sized opaque marker only ever used behind a
    // raw pointer; the encoding matches CoreGraphics' `CGColorRef`.
    unsafe impl objc2::encode::RefEncode for CGColor {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGColor", &[]));
    }
}

/// Last-synced model hash per overlay kind, stored per window. A `sync_*`
/// call whose model hashes identically is a no-op; `None` means the overlay
/// is currently absent.
#[derive(Default)]
pub(crate) struct NativeOverlayCache {
    pub(crate) palette: Option<u64>,
    pub(crate) theme_settings: Option<u64>,
    pub(crate) confirm: Option<u64>,
    pub(crate) title_prompt: Option<u64>,
    pub(crate) toast: Option<u64>,
}

/// Key legend under the "Set Tab Title" prompt's input row (tab-title
/// REQ-TTL-3's empty-clears affordance). Shared with the non-macOS fallback
/// card in `app.rs`.
pub(crate) const TITLE_PROMPT_HINT: &str = "Enter to set \u{b7} Empty clears \u{b7} Esc to cancel";

/// A pane rectangle in AppKit points, top-left origin relative to the
/// window's content view (i.e. physical px / scale factor).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneRectPt {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl PaneRectPt {
    pub(crate) fn from_px(x: u32, y: u32, w: u32, h: u32, scale: f64) -> Self {
        PaneRectPt {
            x: x as f64 / scale,
            y: y as f64 / scale,
            w: w as f64 / scale,
            h: h as f64 / scale,
        }
    }

    pub(crate) fn hash_into(&self, hasher: &mut impl Hasher) {
        self.x.to_bits().hash(hasher);
        self.y.to_bits().hash(hasher);
        self.w.to_bits().hash(hasher);
        self.h.to_bits().hash(hasher);
    }
}

/// Theme-derived colors for the native overlays, resolved from the same
/// [`OverlayStyle`] the wgpu cards use so the two paths share one visual
/// language (and the preview theme keeps working).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayColors {
    pub(crate) surface_fg: [f32; 4],
    pub(crate) muted: [f32; 4],
    pub(crate) accent: [f32; 4],
    pub(crate) danger: [f32; 4],
    pub(crate) selected_bg: [f32; 4],
    pub(crate) surface_bg: [f32; 4],
    pub(crate) border: [f32; 4],
}

impl OverlayColors {
    pub(crate) fn from_style(style: &OverlayStyle, danger: noa_core::Rgb) -> Self {
        OverlayColors {
            surface_fg: style.surface_fg(),
            muted: style.muted_fg(),
            accent: style.accent(),
            danger: [
                danger.r as f32 / 255.0,
                danger.g as f32 / 255.0,
                danger.b as f32 / 255.0,
                1.0,
            ],
            selected_bg: style.selected_bg(),
            surface_bg: style.surface_bg(),
            border: style.border(),
        }
    }

    /// Whether the theme surface is dark — picks the vibrant appearance for
    /// the blur material so it harmonizes with the terminal theme.
    pub(crate) fn is_dark(&self) -> bool {
        let [r, g, b, _] = self.surface_bg;
        0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5
    }

    pub(crate) fn hash_into(&self, hasher: &mut impl Hasher) {
        for c in [
            self.surface_fg,
            self.muted,
            self.accent,
            self.danger,
            self.selected_bg,
            self.surface_bg,
            self.border,
        ] {
            for v in c {
                v.to_bits().hash(hasher);
            }
        }
    }
}

/// Windowing shared with the wgpu path's policy: show up to `capacity` rows,
/// keeping `selected` centered once the list overflows. Returns
/// `(offset, shown)`.
pub(crate) fn overlay_scroll_window(
    len: usize,
    selected: usize,
    capacity: usize,
) -> (usize, usize) {
    if len <= capacity {
        return (0, len);
    }
    let offset = selected.saturating_sub(capacity / 2).min(len - capacity);
    (offset, capacity)
}

/// The footer line's emphasis: the muted key hint or the danger-colored
/// commit error.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Tone {
    Muted,
    Danger,
}

/// One settings row's display data (R-1/R-3/R-6): label, current value, the
/// always-visible [`crate::theme_settings::Liveness`] badge and
/// [`crate::theme_settings::RestartReason`] note (independent signals, R-3),
/// and whether this row is the currently selected/highlighted one.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct SettingsRowView {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) liveness: crate::theme_settings::Liveness,
    pub(crate) restart_reason: crate::theme_settings::RestartReason,
    pub(crate) selected: bool,
}

/// A plain-data description of the theme-settings card, mirroring
/// `theme_settings_overlay_text` (the wgpu path) so the two renderings show
/// the same content. Structured instead of ANSI so the native layer can lay
/// out real labels/swatches.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct ThemeSettingsViewModel {
    /// Which overlay this is — "Theme" picker or "Settings" rows. The
    /// picker fields (`filter`/`themes`/`*_swatches`) are only meaningful
    /// (and only rendered) in [`crate::theme_settings::ThemeSettingsMode::Theme`];
    /// `rows`/`settings_*` only in `Settings`.
    pub(crate) mode: crate::theme_settings::ThemeSettingsMode,
    pub(crate) badge: Option<&'static str>,
    pub(crate) theme_section_focused: bool,
    pub(crate) filter: String,
    /// Windowed theme list: (name, highlighted).
    pub(crate) themes: Vec<(String, bool)>,
    /// ANSI 16 sample swatches (rgb), in palette order.
    pub(crate) ansi_swatches: Vec<(u8, u8, u8)>,
    /// Semantic swatches (fg/bg/cursor/selection), in order.
    pub(crate) semantic_swatches: Vec<(u8, u8, u8)>,
    pub(crate) show_truecolor_ramp: bool,
    pub(crate) settings_focused: bool,
    /// Every settings row, in `SettingsRowKind::ALL` order (not the search
    /// filter's order — see [`Self::settings_visible`] for which of these to
    /// actually show right now).
    pub(crate) rows: Vec<SettingsRowView>,
    /// Indices into [`Self::rows`] currently shown: every row in `ALL` order
    /// while search is inactive, or the R-5 fuzzy-filtered subset (best
    /// match first) while it is.
    pub(crate) settings_visible: Vec<usize>,
    /// R-5: whether the row-search modal sub-state is active.
    pub(crate) search_active: bool,
    pub(crate) search_query: String,
    /// R-6: the currently selected row's description — always
    /// `SettingsRowKind::ALL[selected_row].description()`, independent of
    /// search/highlight state (AC-17).
    pub(crate) selected_description: &'static str,
    /// R-7/C-5: whether the post-Reset highlight is still showing.
    pub(crate) reset_flash: bool,
    /// The footer line: commit error (danger) or the key hint (muted).
    pub(crate) footer: (String, Tone),
}

/// Rows the theme list shows at once in the native card.
const THEME_LIST_ROWS: usize = 8;

pub(crate) fn theme_settings_view_model(
    state: &crate::theme_settings::ThemeSettings,
) -> ThemeSettingsViewModel {
    use crate::theme_settings::{
        Section, SettingsRowKind, Swatch, sample_swatches, settings_row_display_value,
    };

    let total = state.filtered_len();
    let highlighted = state.highlighted_index();
    let (offset, shown) = overlay_scroll_window(total, highlighted, THEME_LIST_ROWS);
    let themes = (offset..offset + shown)
        .filter_map(|idx| {
            state
                .filtered_entry(idx)
                .map(|(name, _)| (name.to_string(), idx == highlighted))
        })
        .collect();

    let mut ansi_swatches = Vec::new();
    let mut semantic_swatches = Vec::new();
    let mut show_truecolor_ramp = false;
    if let Some(theme_def) = state.highlighted_theme_name().and_then(noa_theme::resolve) {
        for swatch in sample_swatches(theme_def) {
            match swatch {
                Swatch::Ansi(_, color) => ansi_swatches.push((color.r, color.g, color.b)),
                Swatch::Foreground(color)
                | Swatch::Background(color)
                | Swatch::Cursor(color)
                | Swatch::Selection(color) => {
                    semantic_swatches.push((color.r, color.g, color.b));
                }
                Swatch::Truecolor(_) => show_truecolor_ramp = true,
            }
        }
    }

    // R-5: while search is active, the highlighted row (a `settings_filtered`
    // index) is the visually "selected" one instead of `selected_row` (which
    // only updates on confirm, Addendum B) — both converge to the same
    // `SettingsRowKind::ALL` index space so `SettingsRowView::selected` stays
    // a single flag regardless of which mode is driving it.
    let search_active = state.settings_search_active();
    let highlighted_all_index = if search_active {
        state.settings_filtered_row_index(state.settings_highlighted_index())
    } else {
        Some(state.selected_row())
    };

    let rows: Vec<SettingsRowView> = SettingsRowKind::ALL
        .iter()
        .enumerate()
        .map(|(idx, kind)| {
            let editing = !search_active
                && idx == state.selected_row()
                && state.section() == Section::SettingsRows;
            SettingsRowView {
                label: kind.label().to_string(),
                value: settings_row_display_value(*kind, &state.rows()[idx].draft, editing),
                liveness: state.liveness(*kind),
                restart_reason: state.restart_reason(*kind),
                selected: Some(idx) == highlighted_all_index,
            }
        })
        .collect();

    let settings_visible: Vec<usize> = if search_active {
        (0..state.settings_filtered_len())
            .filter_map(|i| state.settings_filtered_row_index(i))
            .collect()
    } else {
        (0..SettingsRowKind::COUNT).collect()
    };

    let selected_description = SettingsRowKind::ALL[state.selected_row()].description();

    let footer = match state.commit_error() {
        Some(error) => (error.to_string(), Tone::Danger),
        None if search_active => (
            "Enter confirm row   Tab exit search   Esc cancel".to_string(),
            Tone::Muted,
        ),
        None => (
            "\u{2191}\u{2193} navigate   \u{2190}\u{2192} adjust   Tab search   Delete reset   Esc cancel   Enter save"
                .to_string(),
            Tone::Muted,
        ),
    };

    ThemeSettingsViewModel {
        mode: state.mode(),
        badge: state
            .badge_visible()
            .then_some("Chrome/tabs update on Save"),
        theme_section_focused: state.section() == Section::ThemePicker,
        filter: state.filter().to_string(),
        themes,
        ansi_swatches,
        semantic_swatches,
        show_truecolor_ramp,
        settings_focused: state.section() == Section::SettingsRows,
        rows,
        settings_visible,
        search_active,
        search_query: state.settings_filter().to_string(),
        selected_description,
        reset_flash: state.reset_flash_active(std::time::Instant::now()),
        footer,
    }
}

/// Settings-mode row-shrink policy (Addendum D-3/FM-04): how many rows fit
/// `avail` height once the footer, the always-on description line, and
/// (while active) the search line are reserved — the same floor-of-3
/// degradation the pre-existing Theme-mode list policy uses, extended to
/// account for R-5/R-6's new fixed lines so the shrink loop re-solves with
/// them included. Returns `(row_count, card_height)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settings_rows_budget(
    total_rows: usize,
    avail: f64,
    settings_top: f64,
    row_h: f64,
    footer_h: f64,
    description_h: f64,
    search_h: f64,
    search_active: bool,
) -> (usize, f64) {
    let extra = description_h + if search_active { search_h } else { 0.0 };
    let needed = |rows: usize| settings_top + rows as f64 * row_h + footer_h + extra;
    let mut rows = total_rows.max(1);
    while needed(rows) > avail && rows > 3 {
        rows -= 1;
    }
    (rows, needed(rows).min(avail))
}

#[cfg(test)]
mod tests {
    use super::*;

    // FM-04: at the smallest supported pane, the floor-of-3 row budget must
    // still fit `avail` with the description line (and, while searching,
    // the search line) included — the AppKit card's `avail` floor is 240.0
    // (`(pane.h - 24.0).max(240.0)`), so this proves the shrink loop never
    // needs the "drop description/search before violating the row floor"
    // fallback D-3 describes; the numbers alone already guarantee it holds.
    #[test]
    fn settings_rows_budget_floor_of_three_fits_smallest_pane_with_search() {
        let avail = 240.0_f64;
        let settings_top = 66.0;
        let row_h = 23.0;
        let footer_h = 34.0;
        let description_h = 19.0;
        let search_h = 16.0;

        let (rows, height) = settings_rows_budget(
            3,
            avail,
            settings_top,
            row_h,
            footer_h,
            description_h,
            search_h,
            true,
        );
        assert_eq!(rows, 3);
        assert!(height <= avail, "needed(3) with search must fit avail");
    }

    #[test]
    fn settings_rows_budget_shrinks_to_floor_of_three_when_too_small() {
        let (rows, height) =
            settings_rows_budget(16, 100.0, 66.0, 23.0, 34.0, 19.0, 16.0, false);
        assert_eq!(rows, 3, "never shrinks below the floor of 3 rows");
        assert!(height <= 100.0f64.max(240.0));
    }

    #[test]
    fn settings_rows_budget_shows_every_row_when_it_fits() {
        let (rows, _) = settings_rows_budget(16, 1000.0, 66.0, 23.0, 34.0, 19.0, 16.0, false);
        assert_eq!(rows, 16);
    }
}
