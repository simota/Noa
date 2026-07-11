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
    /// `rows` only in `Settings`.
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
    /// Settings rows: (label, value, restart_note, selected).
    pub(crate) rows: Vec<(String, String, bool, bool)>,
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

    let rows = SettingsRowKind::ALL
        .iter()
        .enumerate()
        .map(|(idx, kind)| {
            let selected = idx == state.selected_row();
            let editing = selected && state.section() == Section::SettingsRows;
            (
                kind.label().to_string(),
                settings_row_display_value(*kind, &state.rows()[idx].draft, editing),
                state.restart_note(*kind),
                selected,
            )
        })
        .collect();

    let (footer_line, footer_is_error) = footer_text(state.mode(), state.commit_error());
    let footer = (
        footer_line,
        if footer_is_error {
            Tone::Danger
        } else {
            Tone::Muted
        },
    );

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
        footer,
    }
}
