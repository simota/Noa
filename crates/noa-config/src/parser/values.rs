use std::path::{Path, PathBuf};

use noa_core::Rgb;

use crate::{
    AlphaBlendingMode, BackgroundImageFit, BackgroundImagePosition, ClipboardAccess, CursorShape,
    FontFeature, FontVariation, MAX_SIDEBAR_FONT_SIZE, MAX_SIDEBAR_PREVIEW_LINES,
    MAX_SIDEBAR_WIDTH, MIN_BACKGROUND_IMAGE_INTERVAL_SECS, MIN_SIDEBAR_FONT_SIZE,
    MIN_SIDEBAR_WIDTH, MacosOptionAsAlt, MacosTitlebarProxyIcon, MacosTitlebarStyle,
    PaletteOverride, QuickTerminalPosition, QuickTerminalScreen, QuickTerminalSize,
    QuickTerminalSizeDim, ResizeOverlay, SyntheticStyleMode, ThemeAppearancePair, WindowSaveState,
};

use super::diagnostics::*;
use super::{Diagnostic, Directive};

pub(super) fn parse_u16(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u16> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<i64>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    let Ok(parsed) = u16::try_from(parsed) else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    Some(parsed)
}

/// Parse and validate an IP address directive (`server-bind`), returning the
/// original string (not a re-serialized form) on success so hand-edited
/// config values round-trip verbatim.
pub(super) fn parse_ip_addr_string(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let value = directive.value.as_deref()?;
    if value.parse::<std::net::IpAddr>().is_err() {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(value.to_string())
}

/// Parse a non-negative integer byte count (`scrollback-limit`). `0` is valid
/// and disables scrollback; a negative or non-numeric value diagnoses.
pub(super) fn parse_usize(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<usize> {
    let value = directive.value.as_deref()?;
    match value.parse::<usize>() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
            None
        }
    }
}

pub(super) fn parse_font_size(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || parsed <= 0.0 {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Outcome of parsing the `theme` directive: either a single theme name, or
/// a `light:X,dark:Y` appearance-paired pair. `None` covers both "key
/// absent" and "malformed pair" (a diagnostic is pushed for the latter).
pub(super) enum ThemeSetting {
    Single(String),
    Pair(ThemeAppearancePair),
}

pub(super) fn parse_theme(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ThemeSetting> {
    let value = directive.value.as_deref()?;
    if value.starts_with("light:") || value.starts_with("dark:") {
        return match parse_theme_pair(value) {
            Some(pair) => Some(ThemeSetting::Pair(pair)),
            None => {
                diagnostics.push(theme_pair_diagnostic(path));
                None
            }
        };
    }
    Some(ThemeSetting::Single(value.to_string()))
}

/// Parse `light:NAME,dark:NAME` (either order, both required, comma-
/// separated) into a [`ThemeAppearancePair`].
fn parse_theme_pair(value: &str) -> Option<ThemeAppearancePair> {
    let mut light = None;
    let mut dark = None;
    for part in value.split(',') {
        let part = part.trim();
        if let Some(name) = part.strip_prefix("light:") {
            if name.is_empty() {
                return None;
            }
            light = Some(name.to_string());
        } else if let Some(name) = part.strip_prefix("dark:") {
            if name.is_empty() {
                return None;
            }
            dark = Some(name.to_string());
        } else {
            return None;
        }
    }
    Some(ThemeAppearancePair {
        light: light?,
        dark: dark?,
    })
}

pub(super) fn parse_family(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<String>,
) {
    match directive.value.as_deref() {
        Some(value) if !value.is_empty() => target.push(value.to_string()),
        _ => diagnostics.push(empty_family_diagnostic(path, &directive.key)),
    }
}

pub(super) fn parse_font_feature(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<FontFeature>,
) {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_font_feature_diagnostic(path, ""));
        return;
    };
    let (enabled, tag_str) = match value.strip_prefix('-') {
        Some(rest) => (false, rest),
        None => (true, value),
    };
    let Some(tag) = ascii_tag4(tag_str) else {
        diagnostics.push(invalid_font_feature_diagnostic(path, value));
        return;
    };
    target.push(FontFeature { tag, enabled });
}

pub(super) fn parse_font_variation(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<FontVariation>,
) {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_font_variation_diagnostic(path, &directive.key, ""));
        return;
    };
    let Some((axis, value_str)) = value.split_once('=') else {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    };
    let (Some(tag), Ok(parsed)) = (ascii_tag4(axis), value_str.parse::<f32>()) else {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    };
    if !parsed.is_finite() {
        diagnostics.push(invalid_font_variation_diagnostic(
            path,
            &directive.key,
            value,
        ));
        return;
    }
    target.push(FontVariation { tag, value: parsed });
}

/// Parse a 4-ASCII-char OpenType tag (feature tag or variation axis).
fn ascii_tag4(tag_str: &str) -> Option<[u8; 4]> {
    let bytes = tag_str.as_bytes();
    if bytes.len() != 4 || !tag_str.is_ascii() {
        return None;
    }
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub(super) fn parse_synthetic_style(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SyntheticStyleMode> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(SyntheticStyleMode::Both),
        "false" => Some(SyntheticStyleMode::Neither),
        "no-bold" => Some(SyntheticStyleMode::NoBold),
        "no-italic" => Some(SyntheticStyleMode::NoItalic),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_alpha_blending(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<AlphaBlendingMode> {
    let value = directive.value.as_deref()?;
    match value {
        "native" => Some(AlphaBlendingMode::Native),
        "linear" => Some(AlphaBlendingMode::Linear),
        "linear-corrected" => Some(AlphaBlendingMode::LinearCorrected),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_font_thicken(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    let value = directive.value.as_deref()?;
    let parsed = match value {
        "true" => true,
        "false" => false,
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            return None;
        }
    };
    Some(parsed)
}

pub(super) fn parse_font_thicken_strength(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u8> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<u8>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    Some(parsed)
}

pub(super) fn parse_clipboard_read(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ClipboardAccess> {
    let value = directive.value.as_deref()?;
    match value {
        "deny" | "false" => Some(ClipboardAccess::Deny),
        "ask" => Some(ClipboardAccess::Ask),
        "allow" | "true" => Some(ClipboardAccess::Allow),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_bool_directive(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(true),
        "false" => Some(false),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_non_negative_f32(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || parsed < 0.0 {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Parse `sidebar-width`: the session sidebar's width in points, bounded to
/// [`MIN_SIDEBAR_WIDTH`, `MAX_SIDEBAR_WIDTH`] so the sidebar never shrinks
/// past usable content or crowds out the terminal viewport.
pub(super) fn parse_sidebar_width(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || !(MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&parsed) {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Parse `sidebar-font-size`: the session sidebar's own font size in points,
/// bounded to [`MIN_SIDEBAR_FONT_SIZE`, `MAX_SIDEBAR_FONT_SIZE`] — independent
/// of the terminal grid's `font-size`.
pub(super) fn parse_sidebar_font_size(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || !(MIN_SIDEBAR_FONT_SIZE..=MAX_SIDEBAR_FONT_SIZE).contains(&parsed) {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Clamp an `f32` to `0.0..=1.0`. Ghostty clamps out-of-range values without
/// complaint, so only an unparseable value produces a diagnostic.
pub(super) fn parse_opacity(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed.clamp(0.0, 1.0))
}

/// Parse `minimum-contrast`: a WCAG contrast ratio from 1.0 through 21.0.
/// Unlike opacity/quick-terminal-size, Ghostty documents this as a bounded
/// ratio rather than a clamped percentage, so invalid values diagnose and fall
/// back to the default.
pub(super) fn parse_minimum_contrast(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<f32> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<f32>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if !parsed.is_finite() || !(1.0..=21.0).contains(&parsed) {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Maximum `background-blur-radius`. Beyond this the CGS blur stops looking
/// different and just costs compositor time.
const MAX_BLUR_RADIUS: u16 = 64;
/// Ghostty maps the boolean `true` form of `background-blur-radius` to 20.
const DEFAULT_BLUR_RADIUS: u16 = 20;

/// Parse `background-blur-radius`: a non-negative integer, or the boolean
/// shorthand Ghostty also accepts (`true` = 20, `false` = 0). Out-of-range
/// integers clamp to `0..=MAX_BLUR_RADIUS`; only an unparseable value produces
/// a diagnostic.
pub(super) fn parse_blur_radius(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u16> {
    let value = directive.value.as_deref()?;
    match value {
        "true" => Some(DEFAULT_BLUR_RADIUS),
        "false" => Some(0),
        _ => match value.parse::<u32>() {
            Ok(parsed) => Some(parsed.min(u32::from(MAX_BLUR_RADIUS)) as u16),
            Err(_) => {
                diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
                None
            }
        },
    }
}

/// Parse `background-image`: a filesystem path to a PNG, stored verbatim. Path
/// resolution (leading `~` expansion) and decode happen downstream in `noa-app`
/// — this module stays IO-free (no `dirs`/`fs`/`env`). Missing/undecodable
/// files surface a diagnostic there and disable the image, so an empty value is
/// the only thing that resets the key here.
pub(super) fn parse_background_image(directive: &Directive) -> Option<PathBuf> {
    let value = directive.value.as_deref()?;
    Some(PathBuf::from(value))
}

pub(super) fn parse_background_image_position(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<BackgroundImagePosition> {
    let value = directive.value.as_deref()?;
    match value {
        "top-left" => Some(BackgroundImagePosition::TopLeft),
        "top-center" => Some(BackgroundImagePosition::TopCenter),
        "top-right" => Some(BackgroundImagePosition::TopRight),
        "center-left" => Some(BackgroundImagePosition::CenterLeft),
        "center" => Some(BackgroundImagePosition::Center),
        "center-right" => Some(BackgroundImagePosition::CenterRight),
        "bottom-left" => Some(BackgroundImagePosition::BottomLeft),
        "bottom-center" => Some(BackgroundImagePosition::BottomCenter),
        "bottom-right" => Some(BackgroundImagePosition::BottomRight),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_background_image_fit(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<BackgroundImageFit> {
    let value = directive.value.as_deref()?;
    match value {
        "none" => Some(BackgroundImageFit::None),
        "contain" => Some(BackgroundImageFit::Contain),
        "cover" => Some(BackgroundImageFit::Cover),
        "stretch" => Some(BackgroundImageFit::Stretch),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

/// Parse `background-image-interval`: positive integer seconds for Noa's
/// directory-backed background-image slideshow. Values below the minimum clamp
/// upward; zero, negative, and non-integers diagnose and fall back to default.
pub(super) fn parse_background_image_interval(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u64> {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, ""));
        return None;
    };
    let Ok(parsed) = value.parse::<i64>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if parsed <= 0 {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some((parsed as u64).max(MIN_BACKGROUND_IMAGE_INTERVAL_SECS))
}

/// Parse one `quick-terminal-size` side: a percentage (`40%`) or a pixel
/// count in AppKit points (`400px`). `None` for anything else, including a
/// non-positive magnitude.
fn parse_quick_terminal_size_dim(text: &str) -> Option<QuickTerminalSizeDim> {
    if let Some(percent) = text.strip_suffix('%') {
        let pct = percent.trim().parse::<f32>().ok()?;
        return (pct.is_finite() && pct > 0.0).then_some(QuickTerminalSizeDim::Percent(pct));
    }
    if let Some(pixels) = text.strip_suffix("px") {
        let px = pixels.trim().parse::<u32>().ok()?;
        return (px > 0).then_some(QuickTerminalSizeDim::Pixels(px));
    }
    None
}

/// Parse `quick-terminal-size`: Ghostty's `<primary>[,<secondary>]` format,
/// each side a percentage (`40%`) or a pixel count (`400px`, AppKit points —
/// `noa-app` scales these to physical px at use). For noa back-compat, a bare
/// fraction in `(0.0, 1.0]` — the pre-Ghostty-parity noa format, e.g. `0.4`
/// — is accepted as `primary = Percent(value * 100)` with no secondary side.
/// Unparseable values, non-positive magnitudes, and out-of-range bare
/// fractions all diagnose.
pub(super) fn parse_quick_terminal_size(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<QuickTerminalSize> {
    let value = directive.value.as_deref()?;

    // Ghostty's format always carries a `%`/`px` unit or a `,` side
    // separator, so a bare number never collides with it.
    if !value.contains(['%', ',']) && !value.contains("px") {
        return match value.parse::<f32>() {
            Ok(fraction) if fraction.is_finite() && fraction > 0.0 && fraction <= 1.0 => {
                Some(QuickTerminalSize {
                    primary: Some(QuickTerminalSizeDim::Percent(fraction * 100.0)),
                    secondary: None,
                })
            }
            _ => {
                diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
                None
            }
        };
    }

    let mut sides = value.splitn(2, ',').map(str::trim);
    let Some(primary) = sides.next().and_then(parse_quick_terminal_size_dim) else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    let secondary = match sides.next() {
        Some(text) => {
            let Some(dim) = parse_quick_terminal_size_dim(text) else {
                diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
                return None;
            };
            Some(dim)
        }
        None => None,
    };
    Some(QuickTerminalSize {
        primary: Some(primary),
        secondary,
    })
}

/// Parse `sidebar-preview-lines`: the number of trailing output rows shown on
/// each sidebar card. `0` disables preview rows; larger values are bounded so a
/// config typo cannot create enormous cards.
pub(super) fn parse_sidebar_preview_lines(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<usize> {
    let value = directive.value.as_deref()?;
    let Ok(parsed) = value.parse::<usize>() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    };
    if parsed > MAX_SIDEBAR_PREVIEW_LINES {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return None;
    }
    Some(parsed)
}

/// Parse a `#RRGGBB` or `RRGGBB` (case-insensitive) hex color.
pub(super) fn parse_color(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Rgb> {
    let value = directive.value.as_deref()?;
    match rgb_from_hex(value) {
        Some(rgb) => Some(rgb),
        None => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
            None
        }
    }
}

/// Parse one `palette = N=#rrggbb` directive (index `0..=255`, hex color)
/// and push it onto `target`. Repeatable; a later same-index entry shadows
/// an earlier one when the palette is applied (last wins, in file order).
pub(super) fn parse_palette_entry(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
    target: &mut Vec<PaletteOverride>,
) {
    let Some(value) = directive.value.as_deref() else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, ""));
        return;
    };
    let Some((index_str, color_str)) = value.split_once('=') else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return;
    };
    let (Ok(index), Some(color)) = (
        index_str.trim().parse::<u8>(),
        rgb_from_hex(color_str.trim()),
    ) else {
        diagnostics.push(invalid_value_diagnostic(path, &directive.key, value));
        return;
    };
    target.push(PaletteOverride { index, color });
}

fn rgb_from_hex(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb::new(r, g, b))
}

pub(super) fn parse_cursor_style(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<CursorShape> {
    let value = directive.value.as_deref()?;
    match value {
        "block" => Some(CursorShape::Block),
        "bar" => Some(CursorShape::Bar),
        "underline" => Some(CursorShape::Underline),
        "block_hollow" => Some(CursorShape::BlockHollow),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_window_save_state(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<WindowSaveState> {
    let value = directive.value.as_deref()?;
    match value {
        "default" => Some(WindowSaveState::Default),
        "never" => Some(WindowSaveState::Never),
        "always" => Some(WindowSaveState::Always),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_resize_overlay(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResizeOverlay> {
    let value = directive.value.as_deref()?;
    match value {
        "after-first" => Some(ResizeOverlay::AfterFirst),
        "always" => Some(ResizeOverlay::Always),
        "never" => Some(ResizeOverlay::Never),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_macos_option_as_alt(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MacosOptionAsAlt> {
    let value = directive.value.as_deref()?;
    match value {
        "false" | "none" => Some(MacosOptionAsAlt::None),
        "true" | "both" => Some(MacosOptionAsAlt::Both),
        "left" | "only-left" => Some(MacosOptionAsAlt::Left),
        "right" | "only-right" => Some(MacosOptionAsAlt::Right),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_macos_titlebar_style(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MacosTitlebarStyle> {
    let value = directive.value.as_deref()?;
    match value {
        "native" | "tabs" => Some(MacosTitlebarStyle::Native),
        "transparent" => Some(MacosTitlebarStyle::Transparent),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_macos_titlebar_proxy_icon(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MacosTitlebarProxyIcon> {
    let value = directive.value.as_deref()?;
    match value {
        "visible" | "true" => Some(MacosTitlebarProxyIcon::Visible),
        "hidden" | "false" => Some(MacosTitlebarProxyIcon::Hidden),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_quick_terminal_screen(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<QuickTerminalScreen> {
    let value = directive.value.as_deref()?;
    match value {
        "main" => Some(QuickTerminalScreen::Main),
        "mouse" => Some(QuickTerminalScreen::Mouse),
        "macos-menu-bar" => Some(QuickTerminalScreen::MacosMenuBar),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}

pub(super) fn parse_quick_terminal_position(
    path: &Path,
    directive: &Directive,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<QuickTerminalPosition> {
    let value = directive.value.as_deref()?;
    match value {
        "top" => Some(QuickTerminalPosition::Top),
        "bottom" => Some(QuickTerminalPosition::Bottom),
        "left" => Some(QuickTerminalPosition::Left),
        "right" => Some(QuickTerminalPosition::Right),
        "center" => Some(QuickTerminalPosition::Center),
        other => {
            diagnostics.push(invalid_value_diagnostic(path, &directive.key, other));
            None
        }
    }
}
