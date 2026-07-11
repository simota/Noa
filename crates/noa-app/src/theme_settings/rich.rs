//! Pure leaf functions for the v2 rich-UI display elements (R-26/27/28/30/33)
//! — ADR-5's answer to keeping the 3 draw-sync-point contract (保全制約5)
//! intact while adding new display data: each function here is called by
//! *both* `app/sidebar/palette.rs` (wgpu ANSI-text path) and
//! `macos_overlay/model.rs` (native `ThemeSettingsViewModel` builder), so
//! neither path ever forks its own formatting/derivation logic (AC-48).

use noa_core::Rgb;

/// R-26: `"{n} / {m}"` (1-indexed highlight position over the *currently
/// filtered* count — not the fixed 574-entry catalog size, since favorites
/// (R-29) / attribute (R-30) filters narrow `total` further), or ux.md §1's
/// empty-state text when nothing matches.
pub(crate) fn match_count_label(highlighted: usize, total: usize) -> String {
    if total == 0 {
        return "No matches".to_string();
    }
    format!("{} / {}", highlighted + 1, total)
}

/// R-27: this feature's own WCAG-AA threshold. Deliberately **not**
/// `noa_render::theme::DEFAULT_MINIMUM_CONTRAST` — grounding correction
/// (ux.md §0-1): that constant is `1.0` ("no auto-adjustment"), an unrelated
/// knob to the user's `minimum-contrast` config setting, not a WCAG number.
const LOW_CONTRAST_THRESHOLD: f32 = 4.5;

/// R-27: `("Contrast 4.8:1", false)` normally, or the ux.md §3 warning form
/// (icon + trailing `— low`, not color-only — WCAG SC 1.4.1) with `true`
/// once the ratio drops below [`LOW_CONTRAST_THRESHOLD`]. Wraps
/// `noa_render::contrast_ratio` — no new contrast math (R-27's constraint).
pub(crate) fn contrast_label(fg: Rgb, bg: Rgb) -> (String, bool) {
    let ratio = noa_render::contrast_ratio(fg, bg);
    if ratio < LOW_CONTRAST_THRESHOLD {
        (format!("\u{26a0} Contrast {ratio:.1}:1 \u{2014} low"), true)
    } else {
        (format!("Contrast {ratio:.1}:1"), false)
    }
}

/// R-30: a theme's light/dark classification, derived on the fly from
/// `default_bg`'s relative luminance (`crate::theme::relative_luminance`,
/// promoted `pub(crate)` for this reuse — `noa-render` untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Attribute {
    Light,
    Dark,
}

pub(crate) fn attribute_of(theme: &noa_theme::ThemeDef) -> Attribute {
    if crate::theme::relative_luminance(theme.default_bg) > 0.5 {
        Attribute::Light
    } else {
        Attribute::Dark
    }
}

/// One colored run of text within a [`SampleLine`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SampleSpan {
    pub(crate) text: &'static str,
    pub(crate) fg: Rgb,
}

/// One row of R-33's representative sample text, all on one background.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SampleLine {
    pub(crate) spans: Vec<SampleSpan>,
    pub(crate) bg: Rgb,
}

/// R-33: three representative text rows built from `theme`'s real
/// fg/bg/ANSI/selection colors — no hardcoded placeholder colors (AC-47).
///
/// Grounding correction (beyond ux.md §0's two): ux.md's line 3 spec reads
/// "`ThemeDef`には選択専用の前景色が無いため…既定fgをそのまま使う", but
/// `noa_theme::ThemeDef` *does* carry a dedicated `selection_fg` (vendored
/// per-theme, independent of `default_fg` — see the vendoring script's
/// `selection-foreground` key). Using it here is strictly more correct
/// (matches what a real selection actually looks like) for no added
/// complexity, so this reuses `theme.selection_fg` instead of `default_fg`
/// for line 3 — still "reusing existing theme-def color data, no new
/// derivation" (R-33's actual constraint), just from the field the ux.md
/// author apparently missed.
pub(crate) fn sample_lines(theme: &noa_theme::ThemeDef) -> Vec<SampleLine> {
    vec![
        SampleLine {
            spans: vec![SampleSpan {
                text: "Sample text on background",
                fg: theme.default_fg,
            }],
            bg: theme.default_bg,
        },
        SampleLine {
            spans: vec![
                SampleSpan {
                    text: "error ",
                    fg: theme.palette[1], // ANSI red
                },
                SampleSpan {
                    text: "warning ",
                    fg: theme.palette[3], // ANSI yellow
                },
                SampleSpan {
                    text: "info ",
                    fg: theme.palette[4], // ANSI blue
                },
                SampleSpan {
                    text: "ok",
                    fg: theme.palette[2], // ANSI green
                },
            ],
            bg: theme.default_bg,
        },
        SampleLine {
            spans: vec![SampleSpan {
                text: "Selected text",
                fg: theme.selection_fg,
            }],
            bg: theme.selection_bg,
        },
    ]
}

/// R-29 (ux.md §4) + Addendum A-1/AC-53: the favorites chip's full label,
/// including its local `⌃⇧F` caption placed on the chip itself (mirrors
/// `ATTRIBUTE_CHIP_HINT`'s local placement next to the attribute chip,
/// rather than footer — ux.md §2's "low-frequency controls get a local
/// hint, not a footer slot" rule). The whole label carries one color
/// (accent when on, muted when off) — unlike the attribute chip, there's
/// no per-segment split to represent.
pub(crate) fn favorites_chip_label(favorites_only: bool) -> &'static str {
    if favorites_only {
        "\u{2605} Favorites \u{2303}\u{21e7}F"
    } else {
        "\u{2606} Favorites \u{2303}\u{21e7}F"
    }
}

/// R-30 (ux.md §5): the attribute chip's local hint, shown next to the
/// segment row (never in the footer).
pub(crate) const ATTRIBUTE_CHIP_HINT: &str = "\u{2303}D cycle";

/// One segment of the All/Dark/Light attribute chip row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AttributeChipSegment {
    pub(crate) label: &'static str,
    pub(crate) active: bool,
}

/// R-30 (ux.md §5): the 3-segment attribute chip row, with exactly one
/// segment marked `active`. Reuses "the existing selected-row look" per
/// segment (wgpu: bracket + accent text; native: `tint_layer` background) —
/// each draw path applies its own styling to whichever segment is `active`,
/// so this leaf only decides *which one*, not how it's drawn (ADR-5: no new
/// visual primitive).
pub(crate) fn attribute_chip_segments(active: Option<Attribute>) -> [AttributeChipSegment; 3] {
    [
        AttributeChipSegment {
            label: "All",
            active: active.is_none(),
        },
        AttributeChipSegment {
            label: "Dark",
            active: active == Some(Attribute::Dark),
        },
        AttributeChipSegment {
            label: "Light",
            active: active == Some(Attribute::Light),
        },
    ]
}

/// R-26/R-30/R-53 (ux.md §2): the mode-specific footer key-hint line — the
/// single shared function §0-2's grounding correction calls out (native was
/// mode-blind, wgpu already correct; this closes that gap for both).
/// `commit_error` (when `Some`) takes over regardless of mode, matching the
/// pre-existing behavior both draw paths already had independently.
pub(crate) fn footer_text(
    mode: super::ThemeSettingsMode,
    commit_error: Option<&str>,
) -> (String, bool) {
    if let Some(error) = commit_error {
        return (error.to_string(), true);
    }
    let hint = match mode {
        super::ThemeSettingsMode::Theme => {
            "\u{2191}\u{2193} Navigate   \u{21e5} Filter   \u{21e7}\u{21e5} Settings   \u{2303}F Favorite   \u{23ce} Save   Esc Cancel"
        }
        super::ThemeSettingsMode::Settings => {
            "\u{2191}\u{2193} Navigate   \u{21e5} Search   \u{2190}\u{2192} Adjust   \u{23ce} Save   Esc Cancel"
        }
    };
    (hint.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-37 (R-26).
    #[test]
    fn match_count_label_formats_one_indexed_position_over_total() {
        assert_eq!(match_count_label(2, 12), "3 / 12");
    }

    // AC-37 (R-26) empty state (ux.md §1).
    #[test]
    fn match_count_label_is_no_matches_when_total_is_zero() {
        assert_eq!(match_count_label(0, 0), "No matches");
    }

    // AC-38 (R-27): matches `noa_render::contrast_ratio`'s own return value
    // and flags anything under 4.5:1.
    #[test]
    fn contrast_label_matches_contrast_ratio_and_flags_low_contrast() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        let (label, warn) = contrast_label(black, white);
        let ratio = noa_render::contrast_ratio(black, white);
        assert_eq!(label, format!("Contrast {ratio:.1}:1"));
        assert!(!warn);

        // Two similar grays: low contrast.
        let a = Rgb::new(0x80, 0x80, 0x80);
        let b = Rgb::new(0x90, 0x90, 0x90);
        let (label, warn) = contrast_label(a, b);
        let ratio = noa_render::contrast_ratio(a, b);
        assert!(ratio < 4.5);
        assert_eq!(
            label,
            format!("\u{26a0} Contrast {ratio:.1}:1 \u{2014} low")
        );
        assert!(warn);
    }

    // AC-42 (R-30): a clearly-light and a clearly-dark background classify
    // correctly.
    #[test]
    fn attribute_of_classifies_by_background_luminance() {
        let light = noa_theme::ThemeDef {
            name: "test-light",
            default_fg: Rgb::new(0x10, 0x10, 0x10),
            default_bg: Rgb::new(0xf5, 0xf5, 0xf5),
            cursor: Rgb::new(0, 0, 0),
            selection_fg: Rgb::new(0, 0, 0),
            selection_bg: Rgb::new(0xd0, 0xd0, 0xd0),
            palette: [Rgb::new(0, 0, 0); 256],
        };
        let dark = noa_theme::ThemeDef {
            default_bg: Rgb::new(0x10, 0x10, 0x10),
            default_fg: Rgb::new(0xf5, 0xf5, 0xf5),
            ..light
        };
        assert_eq!(attribute_of(&light), Attribute::Light);
        assert_eq!(attribute_of(&dark), Attribute::Dark);
    }

    // AC-53: the favorites chip carries its local `⌃⇧F` caption in both
    // states.
    #[test]
    fn favorites_chip_label_carries_the_local_caption_in_both_states() {
        assert!(favorites_chip_label(false).ends_with("\u{2303}\u{21e7}F"));
        assert!(favorites_chip_label(true).ends_with("\u{2303}\u{21e7}F"));
        assert_ne!(favorites_chip_label(false), favorites_chip_label(true));
    }

    // R-30: exactly one segment is active per state, cycling correctly.
    #[test]
    fn attribute_chip_segments_mark_exactly_one_active() {
        for active in [None, Some(Attribute::Dark), Some(Attribute::Light)] {
            let segments = attribute_chip_segments(active);
            assert_eq!(segments.iter().filter(|s| s.active).count(), 1);
        }
        assert!(attribute_chip_segments(None)[0].active);
        assert!(attribute_chip_segments(Some(Attribute::Dark))[1].active);
        assert!(attribute_chip_segments(Some(Attribute::Light))[2].active);
    }

    // AC-47 (R-33): every generated line uses a real theme color, never a
    // hardcoded placeholder.
    #[test]
    fn sample_lines_use_only_real_theme_colors() {
        let theme = noa_theme::resolve("3024 Day").expect("bundled theme exists");
        let lines = sample_lines(theme);
        assert_eq!(lines.len(), 3);

        assert_eq!(lines[0].bg, theme.default_bg);
        assert_eq!(lines[0].spans[0].fg, theme.default_fg);

        assert_eq!(lines[1].bg, theme.default_bg);
        let known = [
            theme.palette[1],
            theme.palette[2],
            theme.palette[3],
            theme.palette[4],
        ];
        for span in &lines[1].spans {
            assert!(known.contains(&span.fg), "unexpected color {:?}", span.fg);
        }

        assert_eq!(lines[2].bg, theme.selection_bg);
        assert_eq!(lines[2].spans[0].fg, theme.selection_fg);
    }
}
