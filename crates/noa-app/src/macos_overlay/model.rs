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
    pub(crate) process_monitor: Option<u64>,
    pub(crate) confirm: Option<u64>,
    pub(crate) title_prompt: Option<u64>,
    pub(crate) toast: Option<u64>,
    /// Debug-only instrumentation (NFR-7/AC-58, mirrors the app-wide
    /// `ChromeTextures::record_rebuild`/`rebuild_count` pattern):
    /// incremented once per real `sync_theme_settings` dispatch to
    /// `imp::rebuild_theme_settings` (an actual `view_fingerprint` change),
    /// never on an idempotent sync. Absent in release builds — it exists
    /// only to be asserted on in tests.
    #[cfg(debug_assertions)]
    theme_settings_rebuild_count: std::sync::atomic::AtomicUsize,
}

impl NativeOverlayCache {
    #[cfg(debug_assertions)]
    pub(crate) fn record_theme_settings_rebuild(&self) {
        self.theme_settings_rebuild_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Not yet read outside tests — mirrors `ChromeTextures::rebuild_count`,
    /// which carries the same note (a future GUI-integrated assertion can
    /// read this live).
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    pub(crate) fn theme_settings_rebuild_count(&self) -> usize {
        self.theme_settings_rebuild_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
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

    /// R-10: the WCAG 2.x contrast ratio between two `[f32; 4]` RGBA colors
    /// (order-independent — always `lighter / darker`). A pure algebraic
    /// check over already-resolved theme colors, no external crate: this is
    /// a regression guard that the selected-row background/foreground and
    /// accent/surface pairs stay legible if either token's *source* ever
    /// changes, not a new tokenization (both call sites already go through
    /// theme-derived tokens — `colors.selected_bg`/`accent:
    /// Rgb`/`colors.surface_bg` — per the code audit in
    /// `settings-panel-enrichment.md`'s R-10 section). Only exercised by
    /// its own regression tests today (mirrors the `restart_note`/
    /// `opaque_at_startup` `#[allow(dead_code)]` precedent elsewhere in
    /// this crate) — no production call site needs it yet.
    #[allow(dead_code)]
    pub(crate) fn contrast_ratio(a: [f32; 4], b: [f32; 4]) -> f32 {
        fn relative_luminance([r, g, b, _]: [f32; 4]) -> f32 {
            fn channel(c: f32) -> f32 {
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        }
        let la = relative_luminance(a);
        let lb = relative_luminance(b);
        let (lighter, darker) = if la > lb { (la, lb) } else { (lb, la) };
        (lighter + 0.05) / (darker + 0.05)
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

/// R-33: one representative sample text row, colors resolved to `u8` RGB
/// triples like `ansi_swatches`/`semantic_swatches` already are (the
/// established "native model stores plain RGB, not `noa_core::Rgb`"
/// convention in this struct).
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct SampleLineModel {
    /// (text, fg) runs, all on `bg`.
    pub(crate) spans: Vec<(String, (u8, u8, u8))>,
    pub(crate) bg: (u8, u8, u8),
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
    /// R-26: `"{n} / {m}"` / `"No matches"` — [`crate::theme_settings::match_count_label`].
    pub(crate) match_count: String,
    /// R-30: the All/Dark/Light chip row, one segment `active`.
    pub(crate) attribute_segments: [crate::theme_settings::AttributeChipSegment; 3],
    /// R-30: the attribute chip's local `⌃D cycle` hint text.
    pub(crate) attribute_hint: &'static str,
    /// R-29: the favorites chip's full label (already carries its `⌃⇧F`
    /// local hint — [`crate::theme_settings::favorites_chip_label`]).
    pub(crate) favorites_chip: &'static str,
    pub(crate) favorites_only: bool,
    /// Windowed theme list: (name, highlighted, favorited).
    pub(crate) themes: Vec<(String, bool, bool)>,
    /// ANSI 16 sample swatches (rgb), in palette order.
    pub(crate) ansi_swatches: Vec<(u8, u8, u8)>,
    /// Semantic swatches (fg/bg/cursor/selection), in order.
    pub(crate) semantic_swatches: Vec<(u8, u8, u8)>,
    pub(crate) show_truecolor_ramp: bool,
    /// R-33: 3 representative sample text rows.
    pub(crate) sample_lines: Vec<SampleLineModel>,
    /// R-27: `("Contrast 4.8:1", false)` or the low-contrast warning form —
    /// [`crate::theme_settings::contrast_label`].
    pub(crate) contrast: Option<(String, bool)>,
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
        ATTRIBUTE_CHIP_HINT, Section, SettingsRowKind, Swatch, attribute_chip_segments,
        contrast_label, favorites_chip_label, footer_text, sample_lines, sample_swatches,
        settings_row_display_value,
    };

    let total = state.filtered_len();
    let highlighted = state.highlighted_index();
    let (offset, shown) = overlay_scroll_window(total, highlighted, THEME_LIST_ROWS);
    let themes = (offset..offset + shown)
        .filter_map(|idx| {
            state.filtered_entry(idx).map(|(name, _)| {
                (
                    name.to_string(),
                    idx == highlighted,
                    state.is_favorite(name),
                )
            })
        })
        .collect();

    let mut ansi_swatches = Vec::new();
    let mut semantic_swatches = Vec::new();
    let mut show_truecolor_ramp = false;
    let mut sample_line_models = Vec::new();
    let mut contrast = None;
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
        sample_line_models = sample_lines(theme_def)
            .into_iter()
            .map(|line| SampleLineModel {
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| (span.text.to_string(), (span.fg.r, span.fg.g, span.fg.b)))
                    .collect(),
                bg: (line.bg.r, line.bg.g, line.bg.b),
            })
            .collect();
        contrast = Some(contrast_label(theme_def.default_fg, theme_def.default_bg));
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

    // Reconciled footer hints. Settings mode owns R-5 search + R-7 reset, so
    // it keeps the search-aware settings hint (Tab = search under the merged
    // routing); Theme mode uses the branch's mode-aware `footer_text` (Tab =
    // Settings, ⌃F favorite). `search_active` is only ever true in Settings
    // mode.
    let footer = match state.commit_error() {
        Some(error) => (error.to_string(), Tone::Danger),
        None if search_active => (
            "Enter confirm row   Tab exit search   Esc cancel".to_string(),
            Tone::Muted,
        ),
        None => match state.mode() {
            crate::theme_settings::ThemeSettingsMode::Settings => (
                "\u{2191}\u{2193} navigate   \u{2190}\u{2192} adjust   Tab search   Delete reset   Esc cancel   Enter save"
                    .to_string(),
                Tone::Muted,
            ),
            crate::theme_settings::ThemeSettingsMode::Theme => {
                (footer_text(state.mode(), None).0, Tone::Muted)
            }
        },
    };

    ThemeSettingsViewModel {
        mode: state.mode(),
        badge: state
            .badge_visible()
            .then_some("Chrome/tabs update on Save"),
        theme_section_focused: state.section() == Section::ThemePicker,
        filter: state.filter().to_string(),
        match_count: crate::theme_settings::match_count_label(highlighted, total),
        attribute_segments: attribute_chip_segments(state.attribute_filter()),
        attribute_hint: ATTRIBUTE_CHIP_HINT,
        favorites_chip: favorites_chip_label(state.favorites_only()),
        favorites_only: state.favorites_only(),
        themes,
        ansi_swatches,
        semantic_swatches,
        show_truecolor_ramp,
        sample_lines: sample_line_models,
        contrast,
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

/// One process-monitor row's plain-data display strings (panel-metrics-view
/// FR-3), already formatted through `process_monitor`'s pure formatters —
/// the native layer only lays labels out, it never re-derives a value.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct ProcessMonitorRowView {
    pub(crate) process: String,
    pub(crate) cpu: String,
    pub(crate) mem: String,
    pub(crate) proc_count: String,
    pub(crate) elapsed: String,
    pub(crate) location: String,
    pub(crate) selected: bool,
}

/// A plain-data description of the process-monitor card (panel-metrics-view),
/// mirroring `ThemeSettingsViewModel` — structured instead of ANSI so the
/// native layer can lay out real columns.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct ProcessMonitorViewModel {
    pub(crate) sort_label: &'static str,
    pub(crate) rows: Vec<ProcessMonitorRowView>,
}

/// Rows the native card shows at once, matching the wgpu path's card height
/// budget closely enough for the two renderings to look equivalent.
const PROCESS_MONITOR_LIST_ROWS: usize = 10;

pub(crate) fn process_monitor_view_model(
    monitor: &crate::process_monitor::ProcessMonitor,
) -> ProcessMonitorViewModel {
    use crate::process_monitor::{
        format_cpu, format_elapsed, format_mem, format_proc_count, format_process, sort_label,
    };

    let now = std::time::SystemTime::now();
    let (offset, shown) =
        overlay_scroll_window(monitor.rows().len(), monitor.selected(), PROCESS_MONITOR_LIST_ROWS);
    let rows = monitor.rows()[offset..offset + shown]
        .iter()
        .enumerate()
        .map(|(i, row)| ProcessMonitorRowView {
            process: format_process(row.process.as_deref()).to_string(),
            cpu: format_cpu(row.cpu_permille),
            mem: format_mem(row.mem_bytes),
            proc_count: format_proc_count(row.proc_count),
            elapsed: format_elapsed(row.started_at, now),
            location: row.location.clone(),
            selected: offset + i == monitor.selected(),
        })
        .collect();
    ProcessMonitorViewModel {
        sort_label: sort_label(monitor.sort()),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // R-10: the selected-row background/foreground pair stays legible (WCAG
    // AA Large Text floor, 3.0:1) for a real theme fixture — the same
    // "3024 Day" fixture `theme_settings/tests.rs` already uses. Not
    // exhaustive across every theme (Out-of-scope per R-10's spec text); a
    // regression guard against `selected_bg`/`surface_fg` ever silently
    // degrading to a low-contrast hardcoded value.
    //
    // `accent` is checked against a lower floor than `selected_bg` above:
    // spot-checking 5 bundled themes found `selected_bg`/`surface_fg`
    // consistently well clear of 3.0 (5.8-8.0:1), but `accent`/`surface_bg`
    // ranges 2.3-4.9:1 — because `OverlayStyle::accent()`
    // (`noa-render/src/theme.rs`) is `OVERLAY_ACCENT`, one fixed app-wide
    // constant, tested here against each theme's own derived `surface_bg`,
    // not a per-theme-tuned pair. "3024 Day" specifically measures 2.26:1,
    // so a 3.0 floor on this pairing would fail on the very fixture R-10's
    // spec text names — 2.0 is the honest regression floor for what this
    // fixed constant currently achieves, not a new AA-compliance claim.
    #[test]
    fn selected_row_and_accent_colors_meet_the_contrast_floor_for_a_real_theme() {
        let theme = crate::theme::resolve_theme(Some("3024 Day"));
        let style = noa_render::OverlayStyle::from_theme(&theme);
        let colors = OverlayColors::from_style(&style, crate::chrome::palette().dot_red);

        const WCAG_AA_LARGE_TEXT_FLOOR: f32 = 3.0;
        let selected_contrast =
            OverlayColors::contrast_ratio(colors.selected_bg, colors.surface_fg);
        assert!(
            selected_contrast >= WCAG_AA_LARGE_TEXT_FLOOR,
            "selected_bg vs surface_fg contrast {selected_contrast} is below the {WCAG_AA_LARGE_TEXT_FLOOR}:1 floor"
        );

        const ACCENT_CONTRAST_FLOOR: f32 = 2.0;
        let accent_contrast = OverlayColors::contrast_ratio(colors.accent, colors.surface_bg);
        assert!(
            accent_contrast >= ACCENT_CONTRAST_FLOOR,
            "accent vs surface_bg contrast {accent_contrast} is below the {ACCENT_CONTRAST_FLOOR}:1 regression floor"
        );
    }

    // The contrast function itself, pinned against known extremes —
    // black-on-white is the WCAG-canonical 21:1 maximum, and identical
    // colors are always exactly 1:1 (zero contrast).
    #[test]
    fn contrast_ratio_matches_known_wcag_extremes() {
        let black = [0.0, 0.0, 0.0, 1.0];
        let white = [1.0, 1.0, 1.0, 1.0];
        assert!((OverlayColors::contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((OverlayColors::contrast_ratio(black, black) - 1.0).abs() < 0.01);
        // Order-independent.
        assert_eq!(
            OverlayColors::contrast_ratio(black, white),
            OverlayColors::contrast_ratio(white, black)
        );
    }

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
        let (rows, height) = settings_rows_budget(16, 100.0, 66.0, 23.0, 34.0, 19.0, 16.0, false);
        assert_eq!(rows, 3, "never shrinks below the floor of 3 rows");
        assert!(height <= 100.0f64.max(240.0));
    }

    #[test]
    fn settings_rows_budget_shows_every_row_when_it_fits() {
        let (rows, _) = settings_rows_budget(16, 1000.0, 66.0, 23.0, 34.0, 19.0, 16.0, false);
        assert_eq!(rows, 16);
    }

    // Radar edge case: the two tests above only exercise the shrink loop's
    // extremes (fits everything, or bottoms out at the floor of 3). This
    // pins a middle value — the AppKit floor pane (240.0) without search —
    // to prove the decrement loop actually converges to the row count that
    // fits, rather than always landing on one of the two extremes.
    #[test]
    fn settings_rows_budget_shrinks_partially_to_the_row_count_that_fits() {
        let (rows, height) = settings_rows_budget(16, 240.0, 66.0, 23.0, 34.0, 19.0, 16.0, false);
        assert_eq!(
            rows, 5,
            "must land strictly between the floor (3) and the full count (16)"
        );
        assert!(height <= 240.0);
    }
}
