//! Shared chrome palette for noa's own UI surfaces (session sidebar, tab
//! overview). Both surfaces previously carried private near-duplicate color
//! tables; this module is the single source so the dot semantics, attention
//! treatment, and card face colors stay visually unified. GUI-agnostic (no
//! `winit`/`wgpu`): plain `Rgb` values plus a const converter to the straight
//! display-space RGBA the overview's non-sRGB surface expects.

use noa_core::Rgb;

/// Near-black navy backdrop behind every card (overview mockup: "暗色の背景").
pub const CHROME_BG: Rgb = Rgb::new(0x09, 0x0c, 0x15);
/// Card face — one step lighter than [`CHROME_BG`].
pub const CHROME_CARD: Rgb = Rgb::new(0x14, 0x17, 0x20);
/// The selected card's background — brighter still, paired with the accent ring.
pub const CHROME_CARD_SELECTED: Rgb = Rgb::new(0x1f, 0x25, 0x33);
/// Title-bar / pill band — distinguishable from the card face.
pub const CHROME_BAND: Rgb = Rgb::new(0x1e, 0x21, 0x2d);
/// Thin resting card border.
pub const CHROME_BORDER: Rgb = Rgb::new(0x4c, 0x51, 0x61);
/// Hairline seam between a chrome surface and the terminal panes — only a
/// hair lighter than [`CHROME_BG`] so the edge reads as a faint depth cue
/// rather than a drawn line competing with the card strokes.
pub const CHROME_DIVIDER: Rgb = Rgb::new(0x14, 0x18, 0x22);
/// Blue accent: focus ring, selection, hover — the shared
/// [`noa_render::UI_ACCENT`], so the chrome, the overlay (palette/dialog)
/// selection cues, and the pane focus indicator all read as one hue.
pub const CHROME_ACCENT: Rgb = noa_render::UI_ACCENT;
/// Chrome pill face (overview search / hint bars, sidebar menu popup).
pub const CHROME_PILL: Rgb = Rgb::new(0x21, 0x23, 0x36);
/// Thin border around chrome pills.
pub const CHROME_PILL_BORDER: Rgb = Rgb::new(0x40, 0x46, 0x64);
/// Primary chrome text.
pub const CHROME_FG: Rgb = Rgb::new(0xd8, 0xdc, 0xe4);
/// Secondary/dim chrome text.
pub const CHROME_DIM_FG: Rgb = Rgb::new(0x8a, 0x90, 0x9c);

// Status-dot semantics shared by the sidebar cards and the overview title
// bands (FR-11/FR-16): blue = busy, green = idle, yellow = unread bell,
// red = pending attention (a program awaits the user's reply).
pub const CHROME_DOT_BLUE: Rgb = Rgb::new(0x4c, 0x9a, 0xff);
pub const CHROME_DOT_GREEN: Rgb = Rgb::new(0x46, 0xc4, 0x66);
pub const CHROME_DOT_YELLOW: Rgb = Rgb::new(0xe6, 0xb4, 0x50);
pub const CHROME_DOT_RED: Rgb = Rgb::new(0xff, 0x4d, 0x4d);

// Shared shape tokens for every rounded chrome/overlay card (logical px,
// scaled at draw time). Three radius steps — small transient chrome (menus,
// buttons), mid surfaces (overview tiles, pills), large elevated cards
// (sidebar cards, command palette) — and one ring-width scale so "hovered <
// selected < needs-attention" reads consistently across surfaces.
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 8.0;
pub const RADIUS_LG: f32 = 10.0;
/// Thin accent border over a hovered (not selected) card.
pub const RING_HOVER: f32 = 1.5;
/// The selected/focused card's accent ring.
pub const RING_SELECTED: f32 = 2.0;
/// The red needs-attention ring — thicker than selection, paired with
/// [`GLOW_ATTENTION`], so a pending interaction request is unmissable.
pub const RING_ATTENTION: f32 = 2.5;
/// Outer glow radius accompanying [`RING_SELECTED`].
pub const GLOW_SELECTED: f32 = 8.0;
/// Outer glow radius accompanying [`RING_ATTENTION`].
pub const GLOW_ATTENTION: f32 = 12.0;

/// The full chrome color set as one value, so the sidebar and overview can
/// follow the terminal theme's light/dark polarity (a light theme gets light
/// chrome) instead of staying hardwired dark. [`CHROME_DARK`] reproduces the
/// individual `CHROME_*` constants above exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromePalette {
    pub bg: Rgb,
    pub card: Rgb,
    pub card_selected: Rgb,
    pub band: Rgb,
    pub border: Rgb,
    pub divider: Rgb,
    pub accent: Rgb,
    pub pill: Rgb,
    pub pill_border: Rgb,
    pub fg: Rgb,
    pub dim_fg: Rgb,
    pub dot_blue: Rgb,
    pub dot_green: Rgb,
    pub dot_yellow: Rgb,
    pub dot_red: Rgb,
    /// Alpha for the chrome backdrop (sidebar panel fill, overview surface
    /// clear). `1.0` in both opaque palettes; below `1.0` only under
    /// [`glassify`] (`glassmorphism = true`).
    pub backdrop_alpha: f32,
    /// Alpha for large chrome surfaces — sidebar/overview cards and title
    /// bands. `1.0` in both opaque palettes.
    pub surface_alpha: f32,
    /// Alpha for small transient chrome — search/hint pills, menu popups.
    /// Kept above [`Self::surface_alpha`] so short text on a pill stays
    /// legible against whatever shows through. `1.0` in both opaque palettes.
    pub pill_alpha: f32,
}

impl ChromePalette {
    /// Whether this palette is a glass variant. Most alpha-aware call sites
    /// multiply unconditionally rather than branching on this; the ones that
    /// do branch are the two the multiply cannot express — the sidebar band's
    /// clear alpha (`0.0` opaque, `backdrop_alpha` glass: scaling `0.0` would
    /// stay `0.0`) and the blend state its composite pipeline is built with,
    /// which is fixed at pipeline creation.
    pub fn is_glass(&self) -> bool {
        self.surface_alpha < 1.0
    }

    /// Straight display-space RGBA for a backdrop fill.
    pub fn backdrop_rgba(&self, color: Rgb) -> [f32; 4] {
        with_alpha(color, self.backdrop_alpha)
    }

    /// Straight display-space RGBA for a card / band face.
    pub fn surface_rgba(&self, color: Rgb) -> [f32; 4] {
        with_alpha(color, self.surface_alpha)
    }

    /// Straight display-space RGBA for a pill / popup face.
    pub fn pill_rgba(&self, color: Rgb) -> [f32; 4] {
        with_alpha(color, self.pill_alpha)
    }
}

/// Backdrop alpha under `glassmorphism = true`. The chrome composites with
/// `CardPipeline::ALPHA_REPLACE`, so this *is* the final window alpha over
/// the chrome's area rather than a factor on top of `background-opacity` —
/// most of the blurred desktop reads straight through the panel. At this
/// level the face is barely a tint and the pane is held together by its rim
/// and its text — which is the look, not a compromise on the way to it.
const GLASS_BACKDROP_ALPHA: f32 = 0.18;
/// Card / band alpha under `glassmorphism = true`.
const GLASS_SURFACE_ALPHA: f32 = 0.16;
/// Pill / popup alpha under `glassmorphism = true` — deliberately the most
/// opaque of the three; pills carry the smallest text.
const GLASS_PILL_ALPHA: f32 = 0.34;
/// Alpha for the shared overlay surfaces — command palette, search prompt,
/// confirm dialogs — under `glassmorphism = true`. Higher than the chrome
/// alphas above because these cards float over the *terminal grid* rather
/// than over the desktop: they blend with running output, so they keep more
/// weight than the chrome faces above — enough that the text under a dialog
/// reads as texture behind glass rather than as competing content.
const GLASS_OVERLAY_ALPHA: f32 = 0.68;
/// How far the frosted rim pulls the border tokens toward [`ChromePalette::fg`].
/// A translucent face loses the face-vs-backdrop luminance step that normally
/// draws the card edge, so the edge has to be carried by the stroke instead —
/// and the more transparent the face, the more of that job the rim inherits.
const GLASS_RIM_MIX: f32 = 0.70;

/// Derive the frosted-glass variant of an opaque palette: translucent faces
/// plus a brightened rim so each surface still reads as a distinct plane once
/// the face alone no longer separates it from the backdrop. Hues are
/// untouched, so a glass palette keeps its light/dark polarity.
pub fn glassify(base: ChromePalette) -> ChromePalette {
    ChromePalette {
        border: mix(base.border, base.fg, GLASS_RIM_MIX),
        pill_border: mix(base.pill_border, base.fg, GLASS_RIM_MIX),
        backdrop_alpha: GLASS_BACKDROP_ALPHA,
        surface_alpha: GLASS_SURFACE_ALPHA,
        pill_alpha: GLASS_PILL_ALPHA,
        ..base
    }
}

/// Linear channel mix (`t` = 0 → `a`, 1 → `b`).
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Rgb::new(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b))
}

fn with_alpha(color: Rgb, alpha: f32) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        alpha,
    ]
}

/// The original dark chrome — byte-identical to the `CHROME_*` constants.
pub const CHROME_DARK: ChromePalette = ChromePalette {
    bg: CHROME_BG,
    card: CHROME_CARD,
    card_selected: CHROME_CARD_SELECTED,
    band: CHROME_BAND,
    border: CHROME_BORDER,
    divider: CHROME_DIVIDER,
    accent: CHROME_ACCENT,
    pill: CHROME_PILL,
    pill_border: CHROME_PILL_BORDER,
    fg: CHROME_FG,
    dim_fg: CHROME_DIM_FG,
    dot_blue: CHROME_DOT_BLUE,
    dot_green: CHROME_DOT_GREEN,
    dot_yellow: CHROME_DOT_YELLOW,
    dot_red: CHROME_DOT_RED,
    backdrop_alpha: 1.0,
    surface_alpha: 1.0,
    pill_alpha: 1.0,
};

/// Light-polarity chrome for light terminal themes: the same relationships as
/// the dark set (backdrop < card < selected card, hairline seam, dim vs
/// primary text) mirrored around a light neutral, with the status-dot hues
/// darkened enough to keep ≥3:1 contrast against the light card face.
pub const CHROME_LIGHT: ChromePalette = ChromePalette {
    bg: Rgb::new(0xec, 0xee, 0xf4),
    card: Rgb::new(0xf7, 0xf8, 0xfb),
    card_selected: Rgb::new(0xe3, 0xeb, 0xf8),
    band: Rgb::new(0xe2, 0xe5, 0xee),
    border: Rgb::new(0xb8, 0xbf, 0xcf),
    divider: Rgb::new(0xdf, 0xe2, 0xec),
    accent: CHROME_ACCENT,
    pill: Rgb::new(0xe8, 0xea, 0xf2),
    pill_border: Rgb::new(0xc2, 0xc8, 0xda),
    fg: Rgb::new(0x23, 0x29, 0x3a),
    dim_fg: Rgb::new(0x6a, 0x72, 0x84),
    dot_blue: Rgb::new(0x2f, 0x7f, 0xe0),
    dot_green: Rgb::new(0x2c, 0x9e, 0x50),
    dot_yellow: Rgb::new(0xb9, 0x8a, 0x1e),
    dot_red: Rgb::new(0xe0, 0x31, 0x31),
    backdrop_alpha: 1.0,
    surface_alpha: 1.0,
    pill_alpha: 1.0,
};

/// The chrome polarity chosen from the resolved terminal theme, set at
/// GPU/theme init (before any chrome surface draws) and swappable afterward
/// so a runtime theme change (theme-settings-ui R-13) can replace it in
/// place. `parking_lot::RwLock` (not `std::sync::RwLock`) so callers never
/// have to reason about lock poisoning, matching the rest of the crate.
static ACTIVE_PALETTE: parking_lot::RwLock<Option<ChromePalette>> = parking_lot::RwLock::new(None);

/// Select light or dark chrome from the terminal theme's polarity. The first
/// call initializes the active palette; every later call (a theme-settings
/// confirm swapping in a newly resolved theme, or a second window reusing
/// the shared GPU) now replaces it rather than no-op'ing — see
/// [`swap_palette`] to install an already-built [`ChromePalette`] directly.
/// `glass` installs the [`glassify`]'d variant of the chosen polarity
/// (`glassmorphism = true`); `false` installs the byte-identical opaque
/// palette this function has always installed, so the default path is
/// unchanged.
pub fn select_palette(theme_is_light: bool, glass: bool) {
    let base = if theme_is_light {
        CHROME_LIGHT
    } else {
        CHROME_DARK
    };
    swap_palette(if glass { glassify(base) } else { base });
}

/// Replace the active chrome palette in place (theme-settings-ui R-13's
/// chrome swap). Every reader observes the new value on its next [`palette`]
/// call; no GPU/renderer state lives here, so this alone never needs a
/// texture rebuild (that is [`super::state::ChromeTextures::reset`]'s job).
pub fn swap_palette(new: ChromePalette) {
    // The overlay surfaces (command palette, prompts, dialogs) are painted
    // from `noa_render::OverlayStyle`, not from this palette, but they are
    // the same UI language and must frost together — so the one place that
    // installs a palette also installs their alpha. Doing it here rather
    // than at each `select_palette` call site means no path can install a
    // glass palette and leave the overlays opaque.
    noa_render::set_overlay_surface_alpha(if new.is_glass() {
        GLASS_OVERLAY_ALPHA
    } else {
        1.0
    });
    *ACTIVE_PALETTE.write() = Some(new);
}

/// The active chrome palette (dark until [`select_palette`]/[`swap_palette`]
/// runs), returned by value.
///
/// **Deadlock hazard**: this copies the palette out and drops the read guard
/// before returning specifically so no caller can be tempted to hold onto a
/// `&ChromePalette` across another call into this module — `ChromePalette`
/// is `Copy`, so there is never a reason to borrow it instead of copying it.
/// A caller that held the old `&'static` reference across a nested
/// `palette()` call would have deadlocked the instant `palette()` started
/// taking a lock; returning an owned copy makes that class of bug
/// unrepresentable.
pub fn palette() -> ChromePalette {
    ACTIVE_PALETTE.read().unwrap_or(CHROME_DARK)
}

/// Convert a chrome `Rgb` to straight display-space RGBA. The overview and
/// sidebar surfaces use non-sRGB formats (`Bgra8Unorm`), so the components
/// are a plain `/255` with no gamma re-encode.
pub const fn rgba(color: Rgb) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_maps_channels_to_unit_range() {
        assert_eq!(rgba(Rgb::new(0, 0, 0)), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(rgba(Rgb::new(255, 255, 255)), [1.0, 1.0, 1.0, 1.0]);
        let mid = rgba(CHROME_ACCENT);
        assert!((mid[0] - 0x14 as f32 / 255.0).abs() < f32::EPSILON);
        assert_eq!(mid[3], 1.0);
    }

    // This module is shared by GUI-agnostic pure modules; keep it free of
    // windowing/GPU imports (same rule as sidebar.rs / session_store.rs).
    #[test]
    fn chrome_is_gui_agnostic() {
        let source = include_str!("chrome.rs");
        for forbidden in [
            ["use ", "winit"].concat(),
            ["use ", "wgpu"].concat(),
            ["winit", "::"].concat(),
            ["wgpu", "::"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "chrome.rs must not reference `{forbidden}`"
            );
        }
    }

    // `ACTIVE_PALETTE` is a shared process-wide static; cargo runs tests in
    // parallel by default, so any test that swaps it must serialize against
    // every other such test or their swap+assert sequences interleave and
    // become flaky.
    static PALETTE_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    // AC-9: swapping the active palette is visible to the next read, with no
    // `GpuState`/GPU involved at all.
    #[test]
    fn swap_palette_is_visible_to_next_read() {
        let _guard = PALETTE_TEST_LOCK.lock();
        swap_palette(CHROME_DARK);
        assert_eq!(palette(), CHROME_DARK);
        swap_palette(CHROME_LIGHT);
        assert_eq!(palette(), CHROME_LIGHT);
        // Leave the shared static in its default polarity for any other test
        // in this process that reads it via `palette()`.
        swap_palette(CHROME_DARK);
    }

    // The whole point of the default-off contract: `glassmorphism = false`
    // must install exactly the palette that existed before the flag did, so
    // every alpha-aware call site multiplies by 1.0 and every color is
    // untouched. A regression here is a silent visual/perf change for users
    // who never opted in.
    #[test]
    fn opaque_palettes_are_fully_opaque_and_unmodified() {
        for base in [CHROME_DARK, CHROME_LIGHT] {
            assert_eq!(base.backdrop_alpha, 1.0);
            assert_eq!(base.surface_alpha, 1.0);
            assert_eq!(base.pill_alpha, 1.0);
            assert!(!base.is_glass());
            assert_eq!(base.surface_rgba(base.card), rgba(base.card));
            assert_eq!(base.backdrop_rgba(base.bg), rgba(base.bg));
            assert_eq!(base.pill_rgba(base.pill), rgba(base.pill));
        }
    }

    #[test]
    fn select_palette_off_installs_the_opaque_palette() {
        let _guard = PALETTE_TEST_LOCK.lock();
        select_palette(false, false);
        assert_eq!(palette(), CHROME_DARK);
        select_palette(true, false);
        assert_eq!(palette(), CHROME_LIGHT);
        swap_palette(CHROME_DARK);
    }

    #[test]
    fn select_palette_on_installs_the_glass_palette() {
        let _guard = PALETTE_TEST_LOCK.lock();
        select_palette(false, true);
        let dark_glass = palette();
        assert_eq!(dark_glass, glassify(CHROME_DARK));
        assert!(dark_glass.is_glass());
        select_palette(true, true);
        assert_eq!(palette(), glassify(CHROME_LIGHT));
        swap_palette(CHROME_DARK);
    }

    // Glass changes alpha and the rim, never the face hues — so a glass
    // palette keeps its light/dark polarity and every hue-derived cue (status
    // dots, accent ring, text) stays exactly where the opaque palette put it.
    #[test]
    fn glassify_preserves_hues_and_only_lightens_the_rim() {
        for base in [CHROME_DARK, CHROME_LIGHT] {
            let glass = glassify(base);
            assert_eq!(glass.bg, base.bg);
            assert_eq!(glass.card, base.card);
            assert_eq!(glass.card_selected, base.card_selected);
            assert_eq!(glass.band, base.band);
            assert_eq!(glass.accent, base.accent);
            assert_eq!(glass.fg, base.fg);
            assert_eq!(glass.dim_fg, base.dim_fg);
            assert_eq!(glass.dot_red, base.dot_red);
            assert_ne!(glass.border, base.border);
            assert_ne!(glass.pill_border, base.pill_border);
            assert!(glass.backdrop_alpha < 1.0);
            assert!(glass.surface_alpha < 1.0);
            // Pills carry the smallest text, so they stay the most opaque.
            assert!(glass.pill_alpha > glass.surface_alpha);
        }
    }

    // The overlay surfaces frost with the chrome, from the same install:
    // a glass palette installs the overlay alpha, and the opaque palettes
    // put it back to 1.0 so turning `glassmorphism` off leaves no
    // half-translucent command palette behind.
    #[test]
    fn swapping_a_palette_installs_the_matching_overlay_alpha() {
        let _guard = PALETTE_TEST_LOCK.lock();
        swap_palette(glassify(CHROME_DARK));
        assert_eq!(noa_render::overlay_surface_alpha(), GLASS_OVERLAY_ALPHA);

        swap_palette(CHROME_DARK);
        assert_eq!(noa_render::overlay_surface_alpha(), 1.0);

        select_palette(true, true);
        assert_eq!(noa_render::overlay_surface_alpha(), GLASS_OVERLAY_ALPHA);
        select_palette(true, false);
        assert_eq!(noa_render::overlay_surface_alpha(), 1.0);
        swap_palette(CHROME_DARK);
    }

    // The overlay cards float over the terminal grid rather than over the
    // desktop, so they must keep more weight than the chrome surfaces that
    // sit directly on the blurred background — while still being glass.
    #[test]
    fn overlay_alpha_stays_above_the_chrome_surface_alphas() {
        let _guard = PALETTE_TEST_LOCK.lock();
        swap_palette(glassify(CHROME_DARK));
        let glass = palette();
        let overlay = noa_render::overlay_surface_alpha();
        assert!(overlay > glass.pill_alpha, "overlay={overlay}");
        assert!(overlay < 1.0, "overlay={overlay}");
        swap_palette(CHROME_DARK);
    }

    #[test]
    fn glass_alphas_reach_the_rgba_helpers() {
        let glass = glassify(CHROME_DARK);
        assert_eq!(glass.surface_rgba(glass.card)[3], glass.surface_alpha);
        assert_eq!(glass.backdrop_rgba(glass.bg)[3], glass.backdrop_alpha);
        assert_eq!(glass.pill_rgba(glass.pill)[3], glass.pill_alpha);
        // RGB is untouched by the alpha helpers.
        assert_eq!(glass.surface_rgba(glass.card)[..3], rgba(glass.card)[..3]);
    }

    // Deadlock regression: `palette()` must copy the value out and drop its
    // read guard before returning, so a caller can safely call `palette()`
    // again from inside a closure that already "holds" a previous read
    // (i.e. holds the copied value, not a guard). Before this change,
    // `palette()` returned `&'static ChromePalette` borrowed from the lock;
    // a swappable lock behind that same signature would have deadlocked here
    // the moment `swap_palette` needed exclusive access while a read guard
    // was still alive across the nested call.
    #[test]
    fn nested_read_does_not_deadlock() {
        let _guard = PALETTE_TEST_LOCK.lock();
        swap_palette(CHROME_DARK);
        let outer = palette();
        // `outer` is an owned copy, not a guard, so this nested read (and a
        // concurrent write, if one raced in) cannot block on `outer`.
        let inner = palette();
        assert_eq!(outer, inner);
        swap_palette(CHROME_LIGHT);
        assert_eq!(palette(), CHROME_LIGHT);
        swap_palette(CHROME_DARK);
    }
}
