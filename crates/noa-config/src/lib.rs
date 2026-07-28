//! Startup configuration discovery, parsing, validation, and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use noa_core::Rgb;

mod ghostty;
mod import;
mod parser;
mod writer;

pub use ghostty::{ghostty_config_candidates, ghostty_config_candidates_from};
pub use import::{
    ImportOutcome, ImportStats, build_import_output, import_ghostty_config,
    import_ghostty_config_at,
};
pub use parser::{Diagnostic, Directive, parse_directives, parse_overrides};
pub use writer::{apply_updates, write_config_updates};

pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_FONT_SIZE: f32 = 14.0;
/// `scrollback-limit` default: 10 MB of scrollback storage, matching Ghostty.
pub const DEFAULT_SCROLLBACK_LIMIT: usize = 10_000_000;
/// `image-storage-limit` default: 320 MB of decoded image data, matching
/// Ghostty/Kitty's per-terminal graphics storage budget.
pub const DEFAULT_IMAGE_STORAGE_LIMIT: usize = 320_000_000;
/// `scrollback-persist-limit` default: 1 MiB of *encoded* scrollback per pane.
/// The budget is measured before deflate, so the file on disk is smaller.
pub const DEFAULT_SCROLLBACK_PERSIST_LIMIT: usize = 1 << 20;
/// `scrollback-persist-total-limit` default: 64 MiB of persisted scrollback
/// across every pane, enforced against actual on-disk file sizes.
pub const DEFAULT_SCROLLBACK_PERSIST_TOTAL_LIMIT: usize = 64 << 20;
/// `scrollback-persist-max-age-days` default: persisted scrollback older than
/// a week is dropped at launch. `0` disables expiry.
pub const DEFAULT_SCROLLBACK_PERSIST_MAX_AGE_DAYS: u64 = 7;
/// `minimum-contrast` default: 1.0 means no automatic adjustment, matching
/// Ghostty's contrast-ratio scale where 1 permits identical colors.
pub const DEFAULT_MINIMUM_CONTRAST: f32 = 1.0;
/// `quick-terminal-size` default: 40% of the primary axis, no secondary side
/// (fills the cross axis). (Ghostty's own default is 25%; noa opts for a
/// slightly taller default drop-down.)
pub const DEFAULT_QUICK_TERMINAL_SIZE: QuickTerminalSize = QuickTerminalSize {
    primary: Some(QuickTerminalSizeDim::Percent(40.0)),
    secondary: None,
};
/// `quick-terminal-animation-duration` default: 0.2s, matching Ghostty's
/// slide-in/out duration.
pub const DEFAULT_QUICK_TERMINAL_ANIMATION_DURATION: f32 = 0.2;
/// `quick-terminal-hotkey` default: `cmd+grave` (⌘`). (Ghostty ships no
/// default; noa binds one so the drop-down works out of the box. Set
/// `quick-terminal-hotkey = none` to disable it.)
pub const DEFAULT_QUICK_TERMINAL_HOTKEY: &str = "cmd+grave";
/// `scratch-terminal-key` default: `cmd+shift+t`, an in-app keybind (not a
/// global hotkey — see [`crate::KeybindConfig`]/`noa-app`'s `KeybindEngine`).
/// Set `scratch-terminal-key = none` to disable it.
pub const DEFAULT_SCRATCH_TERMINAL_KEY: &str = "cmd+shift+t";
/// `scratch-terminal-size` default: 100x25 cells.
pub const DEFAULT_SCRATCH_TERMINAL_SIZE: ScratchTerminalSize = ScratchTerminalSize {
    cols: 100,
    rows: 25,
};
/// `sidebar-width` default: the session sidebar's width in points when visible.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 360.0;
/// Smallest supported `sidebar-width` value. Narrower widths leave no room
/// for session card content.
pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
/// Largest supported `sidebar-width` value. Wider widths crowd out the
/// terminal viewport.
pub const MAX_SIDEBAR_WIDTH: f32 = 600.0;
/// `sidebar-font-size` default: the session sidebar's own font size in
/// points, independent of the terminal grid's `font-size`.
pub const DEFAULT_SIDEBAR_FONT_SIZE: f32 = 11.5;
/// Smallest supported `sidebar-font-size` value. Smaller sizes make card
/// text illegible.
pub const MIN_SIDEBAR_FONT_SIZE: f32 = 8.0;
/// Largest supported `sidebar-font-size` value. Larger sizes make cards too
/// tall for the sidebar's dense session-list use case.
pub const MAX_SIDEBAR_FONT_SIZE: f32 = 20.0;
/// `sidebar-preview-lines` default: card last-output preview rows.
pub const DEFAULT_SIDEBAR_PREVIEW_LINES: usize = 5;
/// Largest supported `sidebar-preview-lines` value. Higher values make each
/// card too tall for the sidebar's dense session-list use case.
pub const MAX_SIDEBAR_PREVIEW_LINES: usize = 20;
/// `glassmorphism` level: `off`, or one of five increasing degrees of
/// transparency. `One` is the toggle's original fixed look — kept
/// byte-identical to what `glassmorphism = true` has always resolved to, so
/// existing configs don't change appearance — while `Two`..`Five` trade
/// progressively more window/chrome opacity for more of the desktop showing
/// through. See [`glass_background_opacity`]/[`glass_background_blur_radius`]
/// for the per-level window pair, and `noa-app`'s `chrome::glassify` for the
/// per-level chrome alpha tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlassLevel {
    /// Opaque chrome, unmodified window transparency keys. Default.
    #[default]
    Off,
    /// The original `glassmorphism = true` look.
    One,
    Two,
    Three,
    Four,
    /// The structural ceiling: the faces are barely a breath of tint and the
    /// panes are held together by their rims alone, which have reached the
    /// foreground color outright (`chrome::glassify`'s rim mix is `1.0` here
    /// and cannot go further). A sixth level would have nothing left to take
    /// away and no edge left to compensate with.
    Five,
}

impl GlassLevel {
    /// Every level, in ascending-transparency order — the order the Settings
    /// panel cycles through and the order any "does this ladder go the right
    /// way" test walks.
    ///
    /// Exists because the alternative is a hand-written `[Off, One, Two, ..]`
    /// at each of those sites, and those are *slices*, not `match`es: adding
    /// a variant leaves them compiling and silently short. That is exactly
    /// how level `4` was unreachable from the panel for one build. Anything
    /// that enumerates levels reads this array instead, so the compiler's
    /// arity check on the const is the one place the count is asserted.
    pub const ALL: [GlassLevel; 6] = [
        GlassLevel::Off,
        GlassLevel::One,
        GlassLevel::Two,
        GlassLevel::Three,
        GlassLevel::Four,
        GlassLevel::Five,
    ];

    /// [`Self::ALL`] without [`GlassLevel::Off`] — the levels that actually
    /// install glass, for the per-level tables that have no meaningful `Off`
    /// entry.
    pub const ON_LEVELS: [GlassLevel; 5] = [
        GlassLevel::One,
        GlassLevel::Two,
        GlassLevel::Three,
        GlassLevel::Four,
        GlassLevel::Five,
    ];

    /// Whether this level turns glassmorphism on at all — `Off` is the only
    /// level that doesn't, so every "is glass on" call site collapses to this
    /// one check instead of a `!= GlassLevel::Off` scattered everywhere.
    pub fn is_on(self) -> bool {
        self != GlassLevel::Off
    }
}

impl std::fmt::Display for GlassLevel {
    /// Renders exactly the spelling `glassmorphism` accepts back for that
    /// level — what config-file writeback (the Settings panel's commit/undo)
    /// and `noa --config` print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GlassLevel::Off => "off",
            GlassLevel::One => "1",
            GlassLevel::Two => "2",
            GlassLevel::Three => "3",
            GlassLevel::Four => "4",
            GlassLevel::Five => "5",
        })
    }
}

/// `background-opacity` installed when `glassmorphism` is on, replacing
/// whatever the config resolved to, keyed by level. Frosted chrome only reads
/// as glass when there is something behind the window to show through, and a
/// window is only see-through below `1.0` — leaving the user's value in place
/// is what made `glassmorphism = true` look like it did nothing at all.
/// Level `1` (`0.50`) is deliberately aggressive already — half the window is
/// the desktop behind it, because the point of the toggle is the glass, not a
/// hint of it; `2`..`5` push further still (`0.35`/`0.20`/`0.12`/`0.06`) for
/// users who want more of the desktop to show through. The top two are a
/// pane you read *through* rather than one you read *on*: at `5` the window
/// contributes six percent of its own pixels and everything holding the
/// terminal together is the text, which is never faded, and the rim. What
/// keeps text readable at every level is the companion blur, not the
/// opacity: see
/// [`glass_background_blur_radius`], which is pinned to its maximum for
/// exactly that reason. Users who want a heavier pane turn `glassmorphism`
/// off and set `background-opacity` themselves. `Off` is never read through
/// this function by a real caller (see [`resolved_background_opacity`]), but
/// returns `1.0` — fully opaque — rather than an arbitrary placeholder.
pub fn glass_background_opacity(level: GlassLevel) -> f32 {
    match level {
        GlassLevel::Off => 1.0,
        GlassLevel::One => 0.50,
        GlassLevel::Two => 0.35,
        GlassLevel::Three => 0.20,
        GlassLevel::Four => 0.12,
        GlassLevel::Five => 0.06,
    }
}
/// `background-blur-radius` installed when `glassmorphism` is on: the
/// maximum the key accepts, at every on-level alike. At level `1`'s opacity
/// ([`glass_background_opacity`]) the desktop is already half the pixels on
/// screen, so it has to be blurred past recognition — diffuse color instead
/// of shapes — or wallpaper detail reads as noise under the text; the more
/// transparent higher levels only need *at least* that much blur, and the key
/// has no higher value left to give them. Frosted glass, not clear glass, at
/// every step. `0` (no blur) for `Off`.
pub fn glass_background_blur_radius(level: GlassLevel) -> u16 {
    if level.is_on() { 64 } else { 0 }
}
/// `server-port` default (noa-server spec DEC-3: fixed value, no discovery).
pub const DEFAULT_SERVER_PORT: u16 = 61771;
/// Default bind address for the `noa-server` socket: loopback-only. LAN
/// exposure is opt-in via `server-bind` (noa-server spec v2 — LAN opt-in
/// was deferred from the locked v1 spec's FR-2/DEC-3 area).
pub const DEFAULT_SERVER_BIND: &str = "127.0.0.1";
/// `background-image-interval` default: rotate directory-backed background
/// images every 30 seconds.
pub const DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS: u64 = 30;
/// Smallest positive `background-image-interval` value. Lower positive values
/// are clamped so the feature cannot become a display-rate animation loop.
pub const MIN_BACKGROUND_IMAGE_INTERVAL_SECS: u64 = 5;
/// `cursor-stop-blinking-after` default: stop blinking (cursor solid) after
/// this many idle seconds. See `StartupConfig::cursor_stop_blinking_after_secs`
/// for the deviation rationale; `0` restores Ghostty parity (never stop).
pub const DEFAULT_CURSOR_STOP_BLINKING_AFTER_SECS: u64 = 10;

/// `clipboard-read` policy for OSC 52 clipboard *read* (query) requests.
/// Mirrors Ghostty, whose default is `ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardAccess {
    /// Never honor a read request.
    Deny,
    /// Prompt the user before revealing clipboard contents.
    #[default]
    Ask,
    /// Always honor a read request.
    Allow,
}

/// A single OpenType feature toggle, e.g. `calt` (enabled) or `-liga`
/// (`enabled: false`, explicitly disabled). Consumed for real in WP2; WP0
/// only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub enabled: bool,
}

/// A single variable-font axis coordinate, e.g. `wght=700`. Consumed for
/// real in WP2; WP0 only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f32,
}

/// `font-synthetic-style` mode: whether faux-bold/faux-italic synthesis is
/// enabled, and whether either style is individually disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticStyleMode {
    Both,
    Neither,
    NoBold,
    NoItalic,
}

/// `theme = light:NAME,dark:NAME`: resolves at the app layer by the current
/// system appearance (`noa-config` has no notion of macOS appearance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeAppearancePair {
    pub light: String,
    pub dark: String,
}

/// One `palette = N=#rrggbb` 256-color override. Repeatable; later entries
/// for the same index win (see [`merge_list`] wholesale-replace semantics
/// for cross-source precedence — within one source, [`crate::parser`]
/// simply pushes each in file order, so a later same-index entry appended
/// downstream shadows an earlier one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteOverride {
    pub index: u8,
    pub color: Rgb,
}

/// `cursor-style` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    /// A hollow rectangle outline.
    BlockHollow,
}

/// `background-image-position`: the 9-anchor grid used to place the image
/// within the surface for `contain`/`none` fits (and the crop anchor for
/// `cover`). Mirrors Ghostty's `background-image-position`. Default `center`
/// (matches Ghostty — see spec OQ-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundImagePosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    #[default]
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// `background-image-fit`: how the image is scaled into the surface. Mirrors
/// Ghostty's `background-image-fit`. Default `contain` (matches Ghostty — see
/// spec OQ-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundImageFit {
    /// Native pixel size, no scaling.
    None,
    /// Fit inside the surface preserving aspect (letterbox).
    #[default]
    Contain,
    /// Fill the surface preserving aspect, cropping overflow.
    Cover,
    /// Fill the surface ignoring aspect.
    Stretch,
}

/// `window-save-state`: whether to persist and restore the window/tab/split
/// topology across launches. Ghostty accepts `default | never | always`; noa
/// treats `default` as `always` (there is no OS-level "restore on relaunch"
/// signal to defer to), which [`WindowSaveState::restores`] encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowSaveState {
    /// Save and restore (noa's default behavior).
    #[default]
    Default,
    /// Never save or restore session state.
    Never,
    /// Always save and restore.
    Always,
}

impl WindowSaveState {
    /// Whether session state should be saved on exit and restored on launch.
    /// Both `default` and `always` restore; only `never` opts out.
    pub fn restores(self) -> bool {
        !matches!(self, WindowSaveState::Never)
    }
}

/// `scrollback-persist`: whether each pane's scrollback tail is written to
/// disk on exit and restored on launch. noa-specific key (no Ghostty analog —
/// Ghostty restores topology only, which is why the default is `never`:
/// persisting terminal output changes the threat model, so it is opt-in).
/// See `docs/specs/scrollback-persistence.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollbackPersist {
    /// Never write or read persisted scrollback (Ghostty-parity behavior).
    #[default]
    Never,
    /// Persist the tail of each pane's scrollback, capped by
    /// `scrollback-persist-limit`.
    Tail,
}

impl ScrollbackPersist {
    /// Whether scrollback should be captured on exit and restored on launch.
    pub fn persists(self) -> bool {
        matches!(self, ScrollbackPersist::Tail)
    }
}

/// One `keybind = ...` directive from config. The config crate stores chord
/// and action text verbatim; `noa-app` owns chord parsing and action lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindConfig {
    /// `keybind = clear`: remove all default and previously configured
    /// bindings before applying later entries.
    Clear,
    /// `keybind = <chord>=<action>`: bind or replace a chord.
    Bind { trigger: String, action: String },
    /// `keybind = <chord>=unbind`: remove any binding for the chord.
    Unbind { trigger: String },
}

impl KeybindConfig {
    pub fn config_value(&self) -> String {
        match self {
            Self::Clear => "clear".to_string(),
            Self::Bind { trigger, action } => format!("{trigger}={action}"),
            Self::Unbind { trigger } => format!("{trigger}=unbind"),
        }
    }
}

/// `macos-option-as-alt`: which macOS Option key(s) should be treated as
/// terminal Alt instead of producing macOS alternate characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacosOptionAsAlt {
    /// Preserve the platform default: Option may produce alternate characters.
    #[default]
    None,
    /// Treat only the left Option key as Alt.
    Left,
    /// Treat only the right Option key as Alt.
    Right,
    /// Treat both Option keys as Alt.
    Both,
}

/// `macos-titlebar-style`: native macOS titlebar presentation for ordinary
/// terminal windows. No-op outside macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacosTitlebarStyle {
    /// Standard AppKit titlebar/tabs.
    #[default]
    Native,
    /// Transparent titlebar with full-size content view.
    Transparent,
}

/// `macos-titlebar-proxy-icon`: whether the titlebar shows the folder/file
/// proxy icon derived from the focused pane's OSC 7 pwd. No-op outside macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacosTitlebarProxyIcon {
    /// Show the proxy icon (Ghostty parity).
    #[default]
    Visible,
    /// Never show the proxy icon.
    Hidden,
}

/// `quick-terminal-screen`: which display the drop-down quick terminal
/// appears on. Resolved fresh every time the quick terminal is shown (never
/// cached), matching Ghostty. No-op outside macOS. Ghostty semantics:
/// `main` -> `NSScreen.mainScreen`, `mouse` -> the screen whose frame
/// contains `NSEvent.mouseLocation` (no match falls back like an
/// unresolvable screen), `macos-menu-bar` -> `NSScreen.screens.first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuickTerminalScreen {
    /// `NSScreen.mainScreen` — the screen holding the key window. Ghostty's
    /// default.
    Main,
    /// The screen under the mouse pointer (`NSEvent.mouseLocation`).
    ///
    /// **Deviation from Ghostty** (whose default is `main`): noa's global
    /// hotkey fires while Noa is usually *not* the active app, and in that
    /// state `NSScreen.mainScreen` degrades to the screen holding Noa's
    /// existing main window — reproducing exactly the "quick terminal opens
    /// on the wrong screen" bug this key exists to fix. Tracking the mouse
    /// instead follows the screen the user is actually looking at, so noa
    /// makes this the default instead.
    #[default]
    Mouse,
    /// `NSScreen.screens.first` — the screen with the menu bar.
    MacosMenuBar,
}

/// `quick-terminal-position`: which edge of the target screen the drop-down
/// quick terminal slides in from. Mirrors Ghostty's `quick-terminal-position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuickTerminalPosition {
    /// Slides down from the top edge, full width. Ghostty's default.
    #[default]
    Top,
    /// Slides up from the bottom edge, full width.
    Bottom,
    /// Slides in from the left edge, full height.
    Left,
    /// Slides in from the right edge, full height.
    Right,
    /// Centered on screen. **Deviation from Ghostty**: Ghostty fades this
    /// position in/out via window alpha; noa has no window-alpha animation
    /// machinery, so `center` never slides *or* fades — it simply appears and
    /// disappears in place (see `noa-app`'s `quick_terminal_position_geometry`).
    Center,
}

/// One side (`primary` or `secondary`) of a `quick-terminal-size` value: a
/// percentage of the parent (monitor) dimension, or an absolute pixel count
/// in AppKit points (`noa-app` scales these to physical px at use).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QuickTerminalSizeDim {
    Percent(f32),
    Pixels(u32),
}

/// `quick-terminal-size`: the drop-down panel's footprint, Ghostty's
/// `<primary>[,<secondary>]` format (e.g. `40%`, `400px`, `40%,300px`). Which
/// side maps to width and which to height depends on `quick-terminal-position`
/// — see `noa-app`'s `quick_terminal_size_footprint` (a port of Ghostty's
/// `QuickTerminalSize.calculate`): `top`/`bottom` treat `primary` as height
/// and `secondary` as width; `left`/`right` the reverse; `center` treats
/// `primary` as its long axis and `secondary` as its short axis. An absent
/// `secondary` fills the cross axis for `top`/`bottom`/`left`/`right`; for
/// `center` (which has no "fill" axis) it falls back to a fixed default, same
/// as an absent `primary` does everywhere.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuickTerminalSize {
    pub primary: Option<QuickTerminalSizeDim>,
    pub secondary: Option<QuickTerminalSizeDim>,
}

/// `scratch-terminal-size`: the scratch terminal's footprint in terminal
/// cells (not points/px like [`QuickTerminalSize`] — the scratch popup's grid
/// is what matters, not its on-screen size), `<cols>x<rows>` (e.g. `100x25`).
/// Clamped to at most 90% of the focused window's inner grid at spawn time
/// (`noa-app`'s scratch-terminal spawn path), not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchTerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// `resize-overlay`: whether the `cols × rows` grid-size toast shows during a
/// live resize. Mirrors Ghostty's `resize-overlay`. Default `after-first`
/// (every resize except the window's initial layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeOverlay {
    /// Show on every grid-size change except the window's very first layout.
    #[default]
    AfterFirst,
    /// Show on every grid-size change, including the initial layout.
    Always,
    /// Never show the overlay.
    Never,
}

/// `alpha-blending` mode. `Native` is a real value; `Linear` /
/// `LinearCorrected` are parsed-but-fallback (REQ-CFG-4) — `noa-config`
/// emits a diagnostic and the renderer falls back to `Native` (WP3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaBlendingMode {
    Native,
    Linear,
    LinearCorrected,
}

/// Font configuration parsed from `font-*` / `alpha-blending` directives.
///
/// This is a `noa-config`-local type, distinct from `noa_font::FontConfig`
/// (ADR-R1): `noa-config` must not depend on `noa-font`/swash/font-kit, so
/// the two crates' `FontConfig` types stay separate. The `noa-app` layer
/// maps this type to `noa_font::FontConfig` before calling `FontGrid::new`.
///
/// Repeatable keys (`font-family*`, `font-feature`, `font-variation*`)
/// accumulate into `Vec`s across directives in one source (parser.rs); a
/// higher-priority source (CLI over file) replaces a base source's list
/// wholesale rather than concatenating, mirroring this file's scalar
/// last-wins semantics. Scalar keys (`font-synthetic-style`,
/// `alpha-blending`, `font-thicken`, `font-thicken-strength`) are
/// straightforward last-wins `Option`s.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FontConfig {
    pub families: Vec<String>,
    pub families_bold: Vec<String>,
    pub families_italic: Vec<String>,
    pub families_bold_italic: Vec<String>,
    pub features: Vec<FontFeature>,
    pub variations: Vec<FontVariation>,
    pub variations_bold: Vec<FontVariation>,
    pub variations_italic: Vec<FontVariation>,
    pub variations_bold_italic: Vec<FontVariation>,
    pub synthetic_style: Option<SyntheticStyleMode>,
    pub alpha_blending: Option<AlphaBlendingMode>,
    pub thicken: Option<bool>,
    pub thicken_strength: Option<u8>,
}

impl FontConfig {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            families: merge_list(self.families, higher_priority.families),
            families_bold: merge_list(self.families_bold, higher_priority.families_bold),
            families_italic: merge_list(self.families_italic, higher_priority.families_italic),
            families_bold_italic: merge_list(
                self.families_bold_italic,
                higher_priority.families_bold_italic,
            ),
            features: merge_list(self.features, higher_priority.features),
            variations: merge_list(self.variations, higher_priority.variations),
            variations_bold: merge_list(self.variations_bold, higher_priority.variations_bold),
            variations_italic: merge_list(
                self.variations_italic,
                higher_priority.variations_italic,
            ),
            variations_bold_italic: merge_list(
                self.variations_bold_italic,
                higher_priority.variations_bold_italic,
            ),
            synthetic_style: higher_priority.synthetic_style.or(self.synthetic_style),
            alpha_blending: higher_priority.alpha_blending.or(self.alpha_blending),
            thicken: higher_priority.thicken.or(self.thicken),
            thicken_strength: higher_priority.thicken_strength.or(self.thicken_strength),
        }
    }

    pub fn apply_to(self, base: Self) -> Self {
        // `apply_to` composes the same way `merge` does: `self` (the
        // override) wins over `base` (the resolved default).
        base.merge(self)
    }
}

fn merge_list<T>(base: Vec<T>, higher_priority: Vec<T>) -> Vec<T> {
    if higher_priority.is_empty() {
        base
    } else {
        higher_priority
    }
}

/// Resolved, validated startup settings.
#[derive(Clone, PartialEq)]
pub struct StartupConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
    /// `theme = light:NAME,dark:NAME`: when set, `noa-app` resolves the
    /// active theme from this pair by system appearance instead of
    /// [`Self::theme`] (mutually exclusive in practice — the parser only
    /// ever sets one of the two from a single `theme` directive).
    pub theme_appearance: Option<ThemeAppearancePair>,
    pub font: FontConfig,
    /// `palette = N=#rrggbb` 256-color overrides, applied over the resolved
    /// theme's palette. Repeatable; later entries win.
    pub palette: Vec<PaletteOverride>,
    /// OSC 52 clipboard read (query) policy.
    pub clipboard_read: ClipboardAccess,
    /// Whether to confirm before pasting content that could run commands
    /// (`clipboard-paste-protection`). Ghostty default is on.
    pub clipboard_paste_protection: bool,
    /// `confirm-quit`: whether app quit (`cmd+q`, menu, command palette)
    /// prompts before exiting. Default is on.
    pub confirm_quit: bool,
    /// `title-report`: whether `CSI 21 t` (XTWINOPS) may report the window
    /// title back to the running program. Ghostty default is off — the reply
    /// echoes attacker-controllable text (OSC 0/2) into the pty as input.
    pub title_report: bool,
    /// `window-padding-x`: horizontal padding (left = right) in physical
    /// pixels. `None` keeps the built-in default for that axis; the concrete
    /// `GridPadding` is derived in `noa-app`.
    pub window_padding_x: Option<f32>,
    /// `window-padding-y`: vertical padding (top = bottom) in physical pixels.
    pub window_padding_y: Option<f32>,
    /// `background` / `foreground`: theme default color overrides. `None`
    /// keeps the resolved theme's value.
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    /// `cursor-color`: theme cursor color override.
    pub cursor_color: Option<Rgb>,
    /// `selection-foreground` / `selection-background`: theme selection color
    /// overrides.
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    /// `minimum-contrast`: WCAG contrast-ratio floor for foreground text
    /// against its resolved background. `1.0` disables adjustment; valid
    /// configured values are `1.0..=21.0`.
    pub minimum_contrast: f32,
    /// `cursor-style` shape and `cursor-style-blink` toggle. `None` keeps the
    /// terminal default (Ghostty: blinking block).
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    /// `cursor-stop-blinking-after`: seconds of focused-surface inactivity
    /// (no keyboard input, no pty output) after which a blinking cursor
    /// settles solid (visible); any activity resumes blinking. `0` never
    /// stops. noa-specific key (no Ghostty analog).
    ///
    /// **Deviation from Ghostty** (which blinks forever, matching the
    /// default here being non-zero rather than the key existing): an
    /// eternally blinking cursor forces a redraw wake-up every blink
    /// interval even when the terminal is completely idle, keeping idle CPU
    /// and context-switch rates measurably above terminals that quiesce
    /// (kitty stops blinking after 15 idle seconds for the same reason).
    /// Default [`DEFAULT_CURSOR_STOP_BLINKING_AFTER_SECS`]; set `0` to
    /// restore Ghostty-parity behavior.
    pub cursor_stop_blinking_after_secs: u64,
    /// `background-opacity`: 0.0..=1.0, clamped. Default is fully opaque.
    /// **Ignored while `glassmorphism` is on** — that toggle installs
    /// [`glass_background_opacity`] (at the configured level) instead (see
    /// [`apply_glassmorphism_defaults`]).
    pub background_opacity: f32,
    /// `background-blur-radius`: native macOS window background blur radius in
    /// points, `0..=64` (0 = no blur). Only visible with `background_opacity`
    /// below 1.0. No-op on non-macOS. **Ignored while `glassmorphism` is on**
    /// — that toggle installs [`glass_background_blur_radius`] instead.
    pub background_blur_radius: u16,
    /// What `background-opacity` / `background-blur-radius` resolved to
    /// *before* [`apply_glassmorphism_defaults`] took them over — equal to
    /// the effective values whenever `glassmorphism` is off.
    ///
    /// Kept because the effective values are derived while the toggle is on,
    /// and anything that writes config back (the Settings panel's Undo) must
    /// restore what the user actually had, not the derived pair. Without
    /// this, one undo would silently rewrite the fallback appearance that
    /// turning `glassmorphism` off is supposed to return to.
    pub configured_background_opacity: f32,
    pub configured_background_blur_radius: u16,
    /// `glassmorphism`: render noa's own chrome (session sidebar, tab
    /// overview) as translucent frosted panes instead of opaque ones, so the
    /// blurred desktop behind a translucent window shows through — the same
    /// visual language the native AppKit overlays already use. A 4-step level
    /// (`off`/`1`/`2`/`3`, higher = more transparent) rather than a plain
    /// on/off flag — see [`GlassLevel`]. Default [`GlassLevel::Off`]; `Off`
    /// installs the byte-identical opaque chrome palette, so it costs
    /// nothing. Any on-level *takes over* `background-opacity` and
    /// `background-blur-radius` — frosted chrome over an opaque window shows
    /// nothing through, so those two keys resolve to
    /// [`glass_background_opacity`] / [`glass_background_blur_radius`] at
    /// this level regardless of what the config set them to (a diagnostic
    /// names any value that was overridden). noa-specific key (no Ghostty
    /// analog).
    pub glassmorphism: GlassLevel,
    /// `background-image`: path to a PNG laid behind the terminal grid, or the
    /// reserved value `noa` for Noa's bundled wallpaper directory. `None`
    /// leaves the background as the clear color only. Values are stored
    /// verbatim (leading `~` expanded); resolution and decode happen in
    /// `noa-app`.
    pub background_image: Option<PathBuf>,
    /// `background-image-opacity`: `0.0..=1.0`, clamped, default `1.0`. Scales
    /// the background image quad's alpha, independent of `background-opacity`.
    pub background_image_opacity: f32,
    /// `background-image-position`: 9-anchor placement within the surface.
    pub background_image_position: BackgroundImagePosition,
    /// `background-image-fit`: how the image scales into the surface.
    pub background_image_fit: BackgroundImageFit,
    /// `background-image-repeat`: tile the image across the surface when it
    /// does not fill it (primarily meaningful with `fit = none`).
    pub background_image_repeat: bool,
    /// `background-image-interval`: seconds between rotations when
    /// `background-image` resolves to a directory. Noa-specific extension.
    pub background_image_interval_secs: u64,
    /// `scrollback-limit`: total bytes of scrollback storage retained before
    /// page-granular eviction (`0` disables scrollback). Ghostty default 10 MB.
    pub scrollback_limit: usize,
    /// `image-storage-limit`: total bytes of decoded Kitty/SIXEL image data
    /// retained before oldest-first eviction. Ghostty default 320 MB.
    pub image_storage_limit: usize,
    /// `window-save-state`: whether the window/tab/split session is persisted
    /// and restored across launches. Default restores.
    pub window_save_state: WindowSaveState,
    /// `scrollback-persist`: whether each pane's scrollback tail is persisted
    /// alongside the session topology. noa-specific key (no Ghostty analog);
    /// defaults to `never` so noa's observable behavior matches Ghostty until
    /// the user opts in.
    pub scrollback_persist: ScrollbackPersist,
    /// `scrollback-persist-limit`: per-pane cap on persisted scrollback, in
    /// bytes of *encoded* payload (measured before deflate). noa-specific key.
    pub scrollback_persist_limit: usize,
    /// `scrollback-persist-total-limit`: cap on the total on-disk size of all
    /// persisted scrollback; the oldest panes are dropped first at launch.
    /// noa-specific key.
    pub scrollback_persist_total_limit: usize,
    /// `scrollback-persist-max-age-days`: persisted scrollback older than this
    /// is discarded at launch (`0` never expires). noa-specific key.
    pub scrollback_persist_max_age_days: u64,
    /// `macos-option-as-alt`: which Option key(s) should be rewritten as
    /// terminal Alt by the macOS window layer. Default preserves existing
    /// platform text behavior.
    pub macos_option_as_alt: MacosOptionAsAlt,
    /// `macos-titlebar-style`: titlebar presentation for ordinary terminal
    /// windows. Default is native.
    pub macos_titlebar_style: MacosTitlebarStyle,
    /// `macos-non-native-fullscreen`: use borderless-window fullscreen instead
    /// of the native macOS fullscreen Space. No-op outside macOS.
    pub macos_non_native_fullscreen: bool,
    /// `macos-titlebar-proxy-icon`: whether the titlebar shows the focused
    /// pane's OSC 7 pwd as a folder/file proxy icon. Default shows it.
    pub macos_titlebar_proxy_icon: MacosTitlebarProxyIcon,
    /// `macos-applescript`: install the AppleScript / Apple Event bridge on
    /// launch (Ghostty parity, default **true**). When false the Apple Event
    /// handlers are never registered, so scripting the app is a no-op. No-op
    /// outside macOS.
    pub macos_applescript: bool,
    /// `quick-terminal-hotkey`: the global hotkey chord that toggles the
    /// drop-down quick terminal (e.g. `cmd+grave`). Defaults to
    /// [`DEFAULT_QUICK_TERMINAL_HOTKEY`]; set the config value to `none` (or
    /// leave it empty) to register no hotkey and disable the feature. An empty
    /// string is the "explicitly disabled" sentinel. noa-specific key; Ghostty
    /// expresses the same thing as `keybind = global:<chord>=toggle_quick_terminal`.
    pub quick_terminal_hotkey: Option<String>,
    /// `quick-terminal-size`: the quick terminal's footprint — see
    /// [`QuickTerminalSize`]. Default [`DEFAULT_QUICK_TERMINAL_SIZE`] (Ghostty
    /// default is 25%; noa's is 40%).
    pub quick_terminal_size: QuickTerminalSize,
    /// `quick-terminal-autohide`: hide the quick terminal when it loses focus.
    /// Ghostty default is on.
    pub quick_terminal_autohide: bool,
    /// `quick-terminal-screen`: which display the quick terminal appears on,
    /// resolved fresh on every show. Default [`QuickTerminalScreen::Mouse`]
    /// (a deliberate deviation from Ghostty's `main` — see that variant's
    /// doc comment).
    pub quick_terminal_screen: QuickTerminalScreen,
    /// `quick-terminal-position`: which screen edge the quick terminal slides
    /// from. Default [`QuickTerminalPosition::Top`] (Ghostty parity).
    pub quick_terminal_position: QuickTerminalPosition,
    /// `quick-terminal-animation-duration`: how long the quick terminal takes
    /// to slide fully in or out, in seconds. `0` disables the animation (the
    /// panel shows/hides instantly). Default
    /// [`DEFAULT_QUICK_TERMINAL_ANIMATION_DURATION`], matching Ghostty.
    pub quick_terminal_animation_duration: f32,
    /// `scratch-terminal-key`: the in-app chord that toggles the scratch
    /// terminal popup (scratch-terminal R1). Unlike
    /// [`Self::quick_terminal_hotkey`] it is *not* a system-wide hotkey — it
    /// only fires while noa is focused. Defaults to
    /// [`DEFAULT_SCRATCH_TERMINAL_KEY`]; the empty-string sentinel (from
    /// `none`/`off`/`false`/empty) disables it.
    pub scratch_terminal_key: Option<String>,
    /// `scratch-terminal-size`: the scratch terminal popup's grid size — see
    /// [`ScratchTerminalSize`]. Default [`DEFAULT_SCRATCH_TERMINAL_SIZE`]
    /// (100x25 cells), clamped to the focused window's inner size at spawn.
    pub scratch_terminal_size: ScratchTerminalSize,
    /// `sidebar-enabled`: app-wide initial visibility of the session sidebar.
    /// Per-window visibility is toggled from this starting value at runtime.
    /// Default off. noa-specific key (no Ghostty analog).
    pub sidebar_enabled: bool,
    /// `sidebar-width`: the session sidebar's width in points when visible,
    /// converted to a grid inset during the grid-first resize. Default
    /// [`DEFAULT_SIDEBAR_WIDTH`].
    pub sidebar_width: f32,
    /// `sidebar-font-size`: the session sidebar's own font size in points,
    /// independent of the terminal grid's [`Self::font_size`]. Default
    /// [`DEFAULT_SIDEBAR_FONT_SIZE`].
    pub sidebar_font_size: f32,
    /// `sidebar-hotkey`: the in-app chord that toggles the session sidebar
    /// for the focused window. Unlike [`Self::quick_terminal_hotkey`] it is
    /// *not* a system-wide hotkey — it rebinds the keybind engine's
    /// `ToggleSidebar` chord (default `cmd+shift+s`), so it only fires while
    /// noa is focused. `none`/`off`/empty normalize to the empty-string
    /// sentinel (keep the default binding). Defaults to `None`.
    pub sidebar_hotkey: Option<String>,
    /// `sidebar-preview-lines`: how many trailing output rows each sidebar card
    /// extracts and renders. `0` disables last-output preview rows.
    pub sidebar_preview_lines: usize,
    /// `resize-overlay`: whether the `cols × rows` toast shows during a live
    /// resize. Ghostty-parity key; default `after-first`.
    pub resize_overlay: ResizeOverlay,
    /// `visual-bell`: flash the focused window briefly when its terminal
    /// rings BEL (the desktop notification is suppressed there). Default off.
    /// noa-specific key (no Ghostty analog).
    pub visual_bell: bool,
    /// `audible-bell`: play the platform bell when a terminal rings BEL.
    /// Default off.
    pub audible_bell: bool,
    /// `audible-bell-when-unfocused`: when set, suppress the audible bell for
    /// the OS-focused window, but keep it for other windows/backgrounded app
    /// state. Default off.
    pub audible_bell_when_unfocused: bool,
    /// `audible-bell-dock-bounce`: bounce the Dock for an audible BEL that
    /// targets an unfocused window. Default off. No-op outside macOS.
    pub audible_bell_dock_bounce: bool,
    /// `auto-approve`: seed new tabs with agent-CLI auto approval enabled.
    /// Runtime use is still per-tab opt-in; default off.
    pub auto_approve: bool,
    /// `send-selection-send-enter`: after the send-selection picker pastes
    /// the selection into the target pane, also send Enter so the pasted
    /// text is submitted immediately. Default off. noa-specific key (no
    /// Ghostty analog).
    pub send_selection_send_enter: bool,
    /// `keybind`: repeatable in-app keybinding edits applied to the default
    /// [`noa-app`] keybinding engine in config order.
    pub keybinds: Vec<KeybindConfig>,
    /// `server-enable`: start the `noa-ipc` JSON-RPC-over-WebSocket server
    /// (noa-server spec FR-1). Default off — no port is ever opened unless
    /// explicitly enabled.
    pub server_enable: bool,
    /// `server-port`: loopback TCP port the server binds (FR-2). Default
    /// [`DEFAULT_SERVER_PORT`].
    pub server_port: u16,
    /// `server-bind`: interface address the server binds (v2 of the
    /// noa-server spec — LAN opt-in was out-of-scope for the locked v1 spec's
    /// FR-2/DEC-3 area, which fixed `127.0.0.1`-only). Default
    /// [`DEFAULT_SERVER_BIND`] (loopback); set e.g. `0.0.0.0` to opt into
    /// LAN exposure. Token auth (FR-3) is required either way.
    pub server_bind: String,
    /// `server-token`: bearer token override (FR-3). When set, no token file
    /// is generated/read; the configured value is used verbatim. Default
    /// `None` (auto-generate and persist to the token file).
    pub server_token: Option<String>,
    /// `server-scopes`: comma-separated subset of
    /// `read`/`control`/`input`/`attach` grantable to a connecting client
    /// (FR-6). Default `"read"` only.
    pub server_scopes: String,
    /// `client-remote`: target noa-server address (`host:port`). Default
    /// `None`; the attach flow may prompt for an ad hoc endpoint instead.
    pub client_remote: Option<String>,
    /// `client-token`: bearer token used for a remote connection. A direct
    /// configured value takes precedence over [`Self::client_token_file`].
    pub client_token: Option<String>,
    /// `client-token-file`: path to a bearer token file. The file is read
    /// only when [`Self::client_token`] is not configured directly.
    pub client_token_file: Option<PathBuf>,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            font_size: DEFAULT_FONT_SIZE,
            theme: None,
            theme_appearance: None,
            font: FontConfig::default(),
            palette: Vec::new(),
            clipboard_read: ClipboardAccess::default(),
            clipboard_paste_protection: true,
            confirm_quit: true,
            title_report: false,
            window_padding_x: None,
            window_padding_y: None,
            background: None,
            foreground: None,
            cursor_color: None,
            selection_foreground: None,
            selection_background: None,
            minimum_contrast: DEFAULT_MINIMUM_CONTRAST,
            cursor_style: None,
            cursor_style_blink: None,
            cursor_stop_blinking_after_secs: DEFAULT_CURSOR_STOP_BLINKING_AFTER_SECS,
            background_opacity: 1.0,
            background_blur_radius: 0,
            configured_background_opacity: 1.0,
            configured_background_blur_radius: 0,
            glassmorphism: GlassLevel::Off,
            background_image: None,
            background_image_opacity: 1.0,
            background_image_position: BackgroundImagePosition::default(),
            background_image_fit: BackgroundImageFit::default(),
            background_image_repeat: false,
            background_image_interval_secs: DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            image_storage_limit: DEFAULT_IMAGE_STORAGE_LIMIT,
            window_save_state: WindowSaveState::default(),
            scrollback_persist: ScrollbackPersist::default(),
            scrollback_persist_limit: DEFAULT_SCROLLBACK_PERSIST_LIMIT,
            scrollback_persist_total_limit: DEFAULT_SCROLLBACK_PERSIST_TOTAL_LIMIT,
            scrollback_persist_max_age_days: DEFAULT_SCROLLBACK_PERSIST_MAX_AGE_DAYS,
            macos_option_as_alt: MacosOptionAsAlt::default(),
            macos_titlebar_style: MacosTitlebarStyle::default(),
            macos_non_native_fullscreen: false,
            macos_titlebar_proxy_icon: MacosTitlebarProxyIcon::default(),
            macos_applescript: true,
            quick_terminal_hotkey: Some(DEFAULT_QUICK_TERMINAL_HOTKEY.to_string()),
            quick_terminal_size: DEFAULT_QUICK_TERMINAL_SIZE,
            quick_terminal_autohide: true,
            quick_terminal_screen: QuickTerminalScreen::default(),
            quick_terminal_position: QuickTerminalPosition::default(),
            quick_terminal_animation_duration: DEFAULT_QUICK_TERMINAL_ANIMATION_DURATION,
            scratch_terminal_key: Some(DEFAULT_SCRATCH_TERMINAL_KEY.to_string()),
            scratch_terminal_size: DEFAULT_SCRATCH_TERMINAL_SIZE,
            sidebar_enabled: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_font_size: DEFAULT_SIDEBAR_FONT_SIZE,
            sidebar_hotkey: None,
            sidebar_preview_lines: DEFAULT_SIDEBAR_PREVIEW_LINES,
            resize_overlay: ResizeOverlay::default(),
            visual_bell: false,
            audible_bell: false,
            audible_bell_when_unfocused: false,
            audible_bell_dock_bounce: false,
            auto_approve: false,
            send_selection_send_enter: false,
            keybinds: Vec::new(),
            server_enable: false,
            server_port: DEFAULT_SERVER_PORT,
            server_bind: DEFAULT_SERVER_BIND.to_string(),
            server_token: None,
            server_scopes: "read".to_string(),
            client_remote: None,
            client_token: None,
            client_token_file: None,
        }
    }
}

/// Optional values from a config file or explicit CLI flags.
#[derive(Default, Clone, PartialEq)]
pub struct ConfigOverrides {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    pub theme_appearance: Option<ThemeAppearancePair>,
    pub font: FontConfig,
    pub palette: Vec<PaletteOverride>,
    pub clipboard_read: Option<ClipboardAccess>,
    pub clipboard_paste_protection: Option<bool>,
    pub confirm_quit: Option<bool>,
    pub title_report: Option<bool>,
    pub window_padding_x: Option<f32>,
    pub window_padding_y: Option<f32>,
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor_color: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub minimum_contrast: Option<f32>,
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    pub cursor_stop_blinking_after_secs: Option<u64>,
    pub background_opacity: Option<f32>,
    pub background_blur_radius: Option<u16>,
    pub glassmorphism: Option<GlassLevel>,
    pub background_image: Option<PathBuf>,
    pub background_image_opacity: Option<f32>,
    pub background_image_position: Option<BackgroundImagePosition>,
    pub background_image_fit: Option<BackgroundImageFit>,
    pub background_image_repeat: Option<bool>,
    pub background_image_interval_secs: Option<u64>,
    pub scrollback_limit: Option<usize>,
    pub image_storage_limit: Option<usize>,
    pub window_save_state: Option<WindowSaveState>,
    pub scrollback_persist: Option<ScrollbackPersist>,
    pub scrollback_persist_limit: Option<usize>,
    pub scrollback_persist_total_limit: Option<usize>,
    pub scrollback_persist_max_age_days: Option<u64>,
    pub macos_option_as_alt: Option<MacosOptionAsAlt>,
    pub macos_titlebar_style: Option<MacosTitlebarStyle>,
    pub macos_non_native_fullscreen: Option<bool>,
    pub macos_titlebar_proxy_icon: Option<MacosTitlebarProxyIcon>,
    pub macos_applescript: Option<bool>,
    pub quick_terminal_hotkey: Option<String>,
    pub quick_terminal_size: Option<QuickTerminalSize>,
    pub quick_terminal_autohide: Option<bool>,
    pub quick_terminal_screen: Option<QuickTerminalScreen>,
    pub quick_terminal_position: Option<QuickTerminalPosition>,
    pub quick_terminal_animation_duration: Option<f32>,
    pub scratch_terminal_key: Option<String>,
    pub scratch_terminal_size: Option<ScratchTerminalSize>,
    pub sidebar_enabled: Option<bool>,
    pub sidebar_width: Option<f32>,
    pub sidebar_font_size: Option<f32>,
    pub sidebar_hotkey: Option<String>,
    pub sidebar_preview_lines: Option<usize>,
    pub resize_overlay: Option<ResizeOverlay>,
    pub visual_bell: Option<bool>,
    pub audible_bell: Option<bool>,
    pub audible_bell_when_unfocused: Option<bool>,
    pub audible_bell_dock_bounce: Option<bool>,
    pub auto_approve: Option<bool>,
    pub send_selection_send_enter: Option<bool>,
    pub keybinds: Vec<KeybindConfig>,
    pub server_enable: Option<bool>,
    pub server_port: Option<u16>,
    pub server_bind: Option<String>,
    pub server_token: Option<String>,
    pub server_scopes: Option<String>,
    pub client_remote: Option<String>,
    pub client_token: Option<String>,
    pub client_token_file: Option<PathBuf>,
}

macro_rules! impl_redacted_config_debug {
    ($config:ty) => {
        impl std::fmt::Debug for $config {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let server_token = self.server_token.as_ref().map(|_| "<redacted>");
                let client_token = self.client_token.as_ref().map(|_| "<redacted>");
                f.debug_struct(stringify!($config))
                    .field("cols", &self.cols)
                    .field("rows", &self.rows)
                    .field("font_size", &self.font_size)
                    .field("theme", &self.theme)
                    .field("theme_appearance", &self.theme_appearance)
                    .field("font", &self.font)
                    .field("palette", &self.palette)
                    .field("clipboard_read", &self.clipboard_read)
                    .field(
                        "clipboard_paste_protection",
                        &self.clipboard_paste_protection,
                    )
                    .field("confirm_quit", &self.confirm_quit)
                    .field("title_report", &self.title_report)
                    .field("window_padding_x", &self.window_padding_x)
                    .field("window_padding_y", &self.window_padding_y)
                    .field("background", &self.background)
                    .field("foreground", &self.foreground)
                    .field("cursor_color", &self.cursor_color)
                    .field("selection_foreground", &self.selection_foreground)
                    .field("selection_background", &self.selection_background)
                    .field("minimum_contrast", &self.minimum_contrast)
                    .field("cursor_style", &self.cursor_style)
                    .field("cursor_style_blink", &self.cursor_style_blink)
                    .field(
                        "cursor_stop_blinking_after_secs",
                        &self.cursor_stop_blinking_after_secs,
                    )
                    .field("background_opacity", &self.background_opacity)
                    .field("background_blur_radius", &self.background_blur_radius)
                    .field("glassmorphism", &self.glassmorphism)
                    .field("background_image", &self.background_image)
                    .field("background_image_opacity", &self.background_image_opacity)
                    .field("background_image_position", &self.background_image_position)
                    .field("background_image_fit", &self.background_image_fit)
                    .field("background_image_repeat", &self.background_image_repeat)
                    .field(
                        "background_image_interval_secs",
                        &self.background_image_interval_secs,
                    )
                    .field("scrollback_limit", &self.scrollback_limit)
                    .field("image_storage_limit", &self.image_storage_limit)
                    .field("window_save_state", &self.window_save_state)
                    .field("scrollback_persist", &self.scrollback_persist)
                    .field("scrollback_persist_limit", &self.scrollback_persist_limit)
                    .field(
                        "scrollback_persist_total_limit",
                        &self.scrollback_persist_total_limit,
                    )
                    .field(
                        "scrollback_persist_max_age_days",
                        &self.scrollback_persist_max_age_days,
                    )
                    .field("macos_option_as_alt", &self.macos_option_as_alt)
                    .field("macos_titlebar_style", &self.macos_titlebar_style)
                    .field(
                        "macos_non_native_fullscreen",
                        &self.macos_non_native_fullscreen,
                    )
                    .field("macos_titlebar_proxy_icon", &self.macos_titlebar_proxy_icon)
                    .field("macos_applescript", &self.macos_applescript)
                    .field("quick_terminal_hotkey", &self.quick_terminal_hotkey)
                    .field("quick_terminal_size", &self.quick_terminal_size)
                    .field("quick_terminal_autohide", &self.quick_terminal_autohide)
                    .field("quick_terminal_screen", &self.quick_terminal_screen)
                    .field("quick_terminal_position", &self.quick_terminal_position)
                    .field(
                        "quick_terminal_animation_duration",
                        &self.quick_terminal_animation_duration,
                    )
                    .field("scratch_terminal_key", &self.scratch_terminal_key)
                    .field("scratch_terminal_size", &self.scratch_terminal_size)
                    .field("sidebar_enabled", &self.sidebar_enabled)
                    .field("sidebar_width", &self.sidebar_width)
                    .field("sidebar_font_size", &self.sidebar_font_size)
                    .field("sidebar_hotkey", &self.sidebar_hotkey)
                    .field("sidebar_preview_lines", &self.sidebar_preview_lines)
                    .field("resize_overlay", &self.resize_overlay)
                    .field("visual_bell", &self.visual_bell)
                    .field("audible_bell", &self.audible_bell)
                    .field(
                        "audible_bell_when_unfocused",
                        &self.audible_bell_when_unfocused,
                    )
                    .field("audible_bell_dock_bounce", &self.audible_bell_dock_bounce)
                    .field("auto_approve", &self.auto_approve)
                    .field("send_selection_send_enter", &self.send_selection_send_enter)
                    .field("keybinds", &self.keybinds)
                    .field("server_enable", &self.server_enable)
                    .field("server_port", &self.server_port)
                    .field("server_bind", &self.server_bind)
                    .field("server_token", &server_token)
                    .field("server_scopes", &self.server_scopes)
                    .field("client_remote", &self.client_remote)
                    .field("client_token", &client_token)
                    .field("client_token_file", &self.client_token_file)
                    .finish()
            }
        }
    };
}

impl_redacted_config_debug!(StartupConfig);
impl_redacted_config_debug!(ConfigOverrides);

impl ConfigOverrides {
    pub fn merge(self, higher_priority: Self) -> Self {
        let mut keybinds = self.keybinds;
        keybinds.extend(higher_priority.keybinds);
        Self {
            cols: higher_priority.cols.or(self.cols),
            rows: higher_priority.rows.or(self.rows),
            font_size: higher_priority.font_size.or(self.font_size),
            theme: higher_priority.theme.or(self.theme),
            theme_appearance: higher_priority.theme_appearance.or(self.theme_appearance),
            font: self.font.merge(higher_priority.font),
            palette: merge_list(self.palette, higher_priority.palette),
            clipboard_read: higher_priority.clipboard_read.or(self.clipboard_read),
            clipboard_paste_protection: higher_priority
                .clipboard_paste_protection
                .or(self.clipboard_paste_protection),
            confirm_quit: higher_priority.confirm_quit.or(self.confirm_quit),
            title_report: higher_priority.title_report.or(self.title_report),
            window_padding_x: higher_priority.window_padding_x.or(self.window_padding_x),
            window_padding_y: higher_priority.window_padding_y.or(self.window_padding_y),
            background: higher_priority.background.or(self.background),
            foreground: higher_priority.foreground.or(self.foreground),
            cursor_color: higher_priority.cursor_color.or(self.cursor_color),
            selection_foreground: higher_priority
                .selection_foreground
                .or(self.selection_foreground),
            selection_background: higher_priority
                .selection_background
                .or(self.selection_background),
            minimum_contrast: higher_priority.minimum_contrast.or(self.minimum_contrast),
            cursor_style: higher_priority.cursor_style.or(self.cursor_style),
            cursor_style_blink: higher_priority
                .cursor_style_blink
                .or(self.cursor_style_blink),
            cursor_stop_blinking_after_secs: higher_priority
                .cursor_stop_blinking_after_secs
                .or(self.cursor_stop_blinking_after_secs),
            background_opacity: higher_priority
                .background_opacity
                .or(self.background_opacity),
            background_blur_radius: higher_priority
                .background_blur_radius
                .or(self.background_blur_radius),
            glassmorphism: higher_priority.glassmorphism.or(self.glassmorphism),
            background_image: higher_priority.background_image.or(self.background_image),
            background_image_opacity: higher_priority
                .background_image_opacity
                .or(self.background_image_opacity),
            background_image_position: higher_priority
                .background_image_position
                .or(self.background_image_position),
            background_image_fit: higher_priority
                .background_image_fit
                .or(self.background_image_fit),
            background_image_repeat: higher_priority
                .background_image_repeat
                .or(self.background_image_repeat),
            background_image_interval_secs: higher_priority
                .background_image_interval_secs
                .or(self.background_image_interval_secs),
            scrollback_limit: higher_priority.scrollback_limit.or(self.scrollback_limit),
            image_storage_limit: higher_priority
                .image_storage_limit
                .or(self.image_storage_limit),
            window_save_state: higher_priority.window_save_state.or(self.window_save_state),
            scrollback_persist: higher_priority.scrollback_persist.or(self.scrollback_persist),
            scrollback_persist_limit: higher_priority
                .scrollback_persist_limit
                .or(self.scrollback_persist_limit),
            scrollback_persist_total_limit: higher_priority
                .scrollback_persist_total_limit
                .or(self.scrollback_persist_total_limit),
            scrollback_persist_max_age_days: higher_priority
                .scrollback_persist_max_age_days
                .or(self.scrollback_persist_max_age_days),
            macos_option_as_alt: higher_priority
                .macos_option_as_alt
                .or(self.macos_option_as_alt),
            macos_titlebar_style: higher_priority
                .macos_titlebar_style
                .or(self.macos_titlebar_style),
            macos_non_native_fullscreen: higher_priority
                .macos_non_native_fullscreen
                .or(self.macos_non_native_fullscreen),
            macos_titlebar_proxy_icon: higher_priority
                .macos_titlebar_proxy_icon
                .or(self.macos_titlebar_proxy_icon),
            macos_applescript: higher_priority.macos_applescript.or(self.macos_applescript),
            quick_terminal_hotkey: higher_priority
                .quick_terminal_hotkey
                .or(self.quick_terminal_hotkey),
            quick_terminal_size: higher_priority
                .quick_terminal_size
                .or(self.quick_terminal_size),
            quick_terminal_autohide: higher_priority
                .quick_terminal_autohide
                .or(self.quick_terminal_autohide),
            quick_terminal_screen: higher_priority
                .quick_terminal_screen
                .or(self.quick_terminal_screen),
            quick_terminal_position: higher_priority
                .quick_terminal_position
                .or(self.quick_terminal_position),
            quick_terminal_animation_duration: higher_priority
                .quick_terminal_animation_duration
                .or(self.quick_terminal_animation_duration),
            scratch_terminal_key: higher_priority
                .scratch_terminal_key
                .or(self.scratch_terminal_key),
            scratch_terminal_size: higher_priority
                .scratch_terminal_size
                .or(self.scratch_terminal_size),
            sidebar_enabled: higher_priority.sidebar_enabled.or(self.sidebar_enabled),
            sidebar_width: higher_priority.sidebar_width.or(self.sidebar_width),
            sidebar_font_size: higher_priority.sidebar_font_size.or(self.sidebar_font_size),
            sidebar_hotkey: higher_priority.sidebar_hotkey.or(self.sidebar_hotkey),
            sidebar_preview_lines: higher_priority
                .sidebar_preview_lines
                .or(self.sidebar_preview_lines),
            resize_overlay: higher_priority.resize_overlay.or(self.resize_overlay),
            visual_bell: higher_priority.visual_bell.or(self.visual_bell),
            audible_bell: higher_priority.audible_bell.or(self.audible_bell),
            audible_bell_when_unfocused: higher_priority
                .audible_bell_when_unfocused
                .or(self.audible_bell_when_unfocused),
            audible_bell_dock_bounce: higher_priority
                .audible_bell_dock_bounce
                .or(self.audible_bell_dock_bounce),
            auto_approve: higher_priority.auto_approve.or(self.auto_approve),
            send_selection_send_enter: higher_priority
                .send_selection_send_enter
                .or(self.send_selection_send_enter),
            keybinds,
            server_enable: higher_priority.server_enable.or(self.server_enable),
            server_port: higher_priority.server_port.or(self.server_port),
            server_bind: higher_priority.server_bind.or(self.server_bind),
            server_token: higher_priority.server_token.or(self.server_token),
            server_scopes: higher_priority.server_scopes.or(self.server_scopes),
            client_remote: higher_priority.client_remote.or(self.client_remote),
            client_token: higher_priority.client_token.or(self.client_token),
            client_token_file: higher_priority.client_token_file.or(self.client_token_file),
        }
    }

    pub fn apply_to(self, base: StartupConfig) -> StartupConfig {
        let mut keybinds = base.keybinds;
        keybinds.extend(self.keybinds);
        StartupConfig {
            cols: self.cols.unwrap_or(base.cols),
            rows: self.rows.unwrap_or(base.rows),
            font_size: self.font_size.unwrap_or(base.font_size),
            theme: self.theme.or(base.theme),
            theme_appearance: self.theme_appearance.or(base.theme_appearance),
            font: self.font.apply_to(base.font),
            palette: if self.palette.is_empty() {
                base.palette
            } else {
                self.palette
            },
            clipboard_read: self.clipboard_read.unwrap_or(base.clipboard_read),
            clipboard_paste_protection: self
                .clipboard_paste_protection
                .unwrap_or(base.clipboard_paste_protection),
            confirm_quit: self.confirm_quit.unwrap_or(base.confirm_quit),
            title_report: self.title_report.unwrap_or(base.title_report),
            window_padding_x: self.window_padding_x.or(base.window_padding_x),
            window_padding_y: self.window_padding_y.or(base.window_padding_y),
            background: self.background.or(base.background),
            foreground: self.foreground.or(base.foreground),
            cursor_color: self.cursor_color.or(base.cursor_color),
            selection_foreground: self.selection_foreground.or(base.selection_foreground),
            selection_background: self.selection_background.or(base.selection_background),
            minimum_contrast: self.minimum_contrast.unwrap_or(base.minimum_contrast),
            cursor_style: self.cursor_style.or(base.cursor_style),
            cursor_style_blink: self.cursor_style_blink.or(base.cursor_style_blink),
            cursor_stop_blinking_after_secs: self
                .cursor_stop_blinking_after_secs
                .unwrap_or(base.cursor_stop_blinking_after_secs),
            background_opacity: self.background_opacity.unwrap_or(base.background_opacity),
            background_blur_radius: self
                .background_blur_radius
                .unwrap_or(base.background_blur_radius),
            // Overwritten wholesale by `apply_glassmorphism_defaults` at the
            // end of resolution; the values here are placeholders that never
            // reach a caller.
            configured_background_opacity: base.configured_background_opacity,
            configured_background_blur_radius: base.configured_background_blur_radius,
            glassmorphism: self.glassmorphism.unwrap_or(base.glassmorphism),
            background_image: self.background_image.or(base.background_image),
            background_image_opacity: self
                .background_image_opacity
                .unwrap_or(base.background_image_opacity),
            background_image_position: self
                .background_image_position
                .unwrap_or(base.background_image_position),
            background_image_fit: self
                .background_image_fit
                .unwrap_or(base.background_image_fit),
            background_image_repeat: self
                .background_image_repeat
                .unwrap_or(base.background_image_repeat),
            background_image_interval_secs: self
                .background_image_interval_secs
                .unwrap_or(base.background_image_interval_secs),
            scrollback_limit: self.scrollback_limit.unwrap_or(base.scrollback_limit),
            image_storage_limit: self.image_storage_limit.unwrap_or(base.image_storage_limit),
            window_save_state: self.window_save_state.unwrap_or(base.window_save_state),
            scrollback_persist: self.scrollback_persist.unwrap_or(base.scrollback_persist),
            scrollback_persist_limit: self
                .scrollback_persist_limit
                .unwrap_or(base.scrollback_persist_limit),
            scrollback_persist_total_limit: self
                .scrollback_persist_total_limit
                .unwrap_or(base.scrollback_persist_total_limit),
            scrollback_persist_max_age_days: self
                .scrollback_persist_max_age_days
                .unwrap_or(base.scrollback_persist_max_age_days),
            macos_option_as_alt: self.macos_option_as_alt.unwrap_or(base.macos_option_as_alt),
            macos_titlebar_style: self
                .macos_titlebar_style
                .unwrap_or(base.macos_titlebar_style),
            macos_non_native_fullscreen: self
                .macos_non_native_fullscreen
                .unwrap_or(base.macos_non_native_fullscreen),
            macos_titlebar_proxy_icon: self
                .macos_titlebar_proxy_icon
                .unwrap_or(base.macos_titlebar_proxy_icon),
            macos_applescript: self.macos_applescript.unwrap_or(base.macos_applescript),
            quick_terminal_hotkey: self.quick_terminal_hotkey.or(base.quick_terminal_hotkey),
            quick_terminal_size: self.quick_terminal_size.unwrap_or(base.quick_terminal_size),
            quick_terminal_autohide: self
                .quick_terminal_autohide
                .unwrap_or(base.quick_terminal_autohide),
            quick_terminal_screen: self
                .quick_terminal_screen
                .unwrap_or(base.quick_terminal_screen),
            quick_terminal_position: self
                .quick_terminal_position
                .unwrap_or(base.quick_terminal_position),
            quick_terminal_animation_duration: self
                .quick_terminal_animation_duration
                .unwrap_or(base.quick_terminal_animation_duration),
            scratch_terminal_key: self.scratch_terminal_key.or(base.scratch_terminal_key),
            scratch_terminal_size: self
                .scratch_terminal_size
                .unwrap_or(base.scratch_terminal_size),
            sidebar_enabled: self.sidebar_enabled.unwrap_or(base.sidebar_enabled),
            sidebar_width: self.sidebar_width.unwrap_or(base.sidebar_width),
            sidebar_font_size: self.sidebar_font_size.unwrap_or(base.sidebar_font_size),
            sidebar_hotkey: self.sidebar_hotkey.or(base.sidebar_hotkey),
            sidebar_preview_lines: self
                .sidebar_preview_lines
                .unwrap_or(base.sidebar_preview_lines),
            resize_overlay: self.resize_overlay.unwrap_or(base.resize_overlay),
            visual_bell: self.visual_bell.unwrap_or(base.visual_bell),
            audible_bell: self.audible_bell.unwrap_or(base.audible_bell),
            audible_bell_when_unfocused: self
                .audible_bell_when_unfocused
                .unwrap_or(base.audible_bell_when_unfocused),
            audible_bell_dock_bounce: self
                .audible_bell_dock_bounce
                .unwrap_or(base.audible_bell_dock_bounce),
            auto_approve: self.auto_approve.unwrap_or(base.auto_approve),
            send_selection_send_enter: self
                .send_selection_send_enter
                .unwrap_or(base.send_selection_send_enter),
            keybinds,
            server_enable: self.server_enable.unwrap_or(base.server_enable),
            server_port: self.server_port.unwrap_or(base.server_port),
            server_bind: self.server_bind.unwrap_or(base.server_bind),
            server_token: self.server_token.or(base.server_token),
            server_scopes: self.server_scopes.unwrap_or(base.server_scopes),
            client_remote: self.client_remote.or(base.client_remote),
            client_token: self.client_token.or(base.client_token),
            client_token_file: self.client_token_file.or(base.client_token_file),
        }
    }
}

pub fn load_startup_config(
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (Some(config_path), Some(legacy_path)) = (default_config_path(), legacy_toml_config_path())
    else {
        return load_startup_config_without_files(cli);
    };
    load_startup_config_from(&config_path, &legacy_path, cli)
}

/// Resolve the startup config from built-in defaults + CLI overrides only,
/// never reading any config file (Ghostty parity:
/// `--config-default-files=false`).
pub fn load_startup_config_without_files(
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let level = cli.glassmorphism.unwrap_or_default();
    let diagnostics = Vec::from_iter(glass_override_diagnostic(
        level,
        &glass_overridden_keys(&cli),
    ));
    let config = finalize_startup_config(cli.apply_to(StartupConfig::default()))?;
    Ok((config, diagnostics))
}

pub fn load_startup_config_from(
    config_path: &Path,
    legacy_path: &Path,
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (file, mut diagnostics) = if config_path.exists() {
        load_overrides_from_path(config_path)?
    } else {
        (ConfigOverrides::default(), Vec::new())
    };

    if legacy_path.exists() {
        diagnostics.push(Diagnostic {
            message: format!(
                "legacy TOML config {} is no longer read; move settings to {}",
                legacy_path.display(),
                config_path.display()
            ),
        });
    }

    let merged = file.merge(cli);
    let level = merged.glassmorphism.unwrap_or_default();
    diagnostics.extend(glass_override_diagnostic(
        level,
        &glass_overridden_keys(&merged),
    ));
    let config = finalize_startup_config(merged.apply_to(StartupConfig::default()))?;
    Ok((config, diagnostics))
}

fn finalize_startup_config(config: StartupConfig) -> anyhow::Result<StartupConfig> {
    finalize_startup_config_with_home(config, dirs::home_dir().as_deref())
}

/// `glassmorphism` at any on-level takes over the window-transparency keys
/// outright: [`glass_background_opacity`] / [`glass_background_blur_radius`]
/// (at the configured level) replace whatever `background-opacity` /
/// `background-blur-radius` resolved to, including explicitly configured
/// values. Frosted chrome over an opaque window is a no-op (nothing shows
/// through), so the two keys are not really independent of the toggle —
/// honoring them would only reproduce the "glassmorphism does nothing"
/// report that motivated this.
///
/// Applied once here, at the end of resolution, so every consumer — window
/// transparency at creation, the renderer, macOS blur, the Settings panel,
/// the `config` dump — sees the same values, on startup and on every live
/// reload alike. [`glass_overridden_keys`] names the keys this silences so
/// the loaders can say so in a diagnostic.
fn apply_glassmorphism_defaults(config: &mut StartupConfig) {
    // Recorded whether or not the takeover happens, so the pair is always
    // "what the config actually asked for" and no caller has to branch on
    // the toggle to know that.
    config.configured_background_opacity = config.background_opacity;
    config.configured_background_blur_radius = config.background_blur_radius;
    config.background_opacity =
        resolved_background_opacity(config.glassmorphism, config.configured_background_opacity);
    config.background_blur_radius = resolved_background_blur_radius(
        config.glassmorphism,
        config.configured_background_blur_radius,
    );
}

/// The `background-opacity` [`apply_glassmorphism_defaults`] resolves to,
/// exposed standalone so a caller that needs the *effective* value ahead of
/// the next full resolution pass — the Settings panel's own commit of the
/// `glassmorphism` row, which mirrors the level into `AppConfig` directly
/// rather than going through `StartupConfig` resolution again — can derive
/// it without re-implementing this branch at a second site where it could
/// drift from the rule above. `apply_glassmorphism_defaults` itself is
/// written in terms of this function precisely so there is exactly one
/// place the rule lives.
pub fn resolved_background_opacity(level: GlassLevel, configured_background_opacity: f32) -> f32 {
    if level.is_on() {
        glass_background_opacity(level)
    } else {
        configured_background_opacity
    }
}

/// As [`resolved_background_opacity`], for `background-blur-radius`.
pub fn resolved_background_blur_radius(
    level: GlassLevel,
    configured_background_blur_radius: u16,
) -> u16 {
    if level.is_on() {
        glass_background_blur_radius(level)
    } else {
        configured_background_blur_radius
    }
}

/// Which explicitly configured keys [`apply_glassmorphism_defaults`] is about
/// to override, given the merged overrides that produced the config. Empty
/// unless `glassmorphism` resolves to an on-level *and* the user actually set
/// one of them — an untouched key is a default, not something to warn about.
fn glass_overridden_keys(overrides: &ConfigOverrides) -> Vec<&'static str> {
    if !overrides.glassmorphism.unwrap_or_default().is_on() {
        return Vec::new();
    }
    let mut keys = Vec::new();
    if overrides.background_opacity.is_some() {
        keys.push("background-opacity");
    }
    if overrides.background_blur_radius.is_some() {
        keys.push("background-blur-radius");
    }
    keys
}

/// `level`'s actual resolved pair is interpolated into the message (not a
/// fixed 0.50/64) so the diagnostic stays accurate at every on-level, not
/// just the original level `1`.
fn glass_override_diagnostic(level: GlassLevel, keys: &[&'static str]) -> Option<Diagnostic> {
    (!keys.is_empty()).then(|| {
        let opacity = glass_background_opacity(level);
        let blur = glass_background_blur_radius(level);
        Diagnostic {
            message: format!(
                "glassmorphism = {level} overrides {} with the recommended {opacity:.2} \
                 / {blur}; unset glassmorphism to control {} yourself",
                keys.join(" and "),
                if keys.len() == 1 { "it" } else { "them" }
            ),
        }
    })
}

fn finalize_startup_config_with_home(
    mut config: StartupConfig,
    home: Option<&Path>,
) -> anyhow::Result<StartupConfig> {
    apply_glassmorphism_defaults(&mut config);
    if config.client_token.is_none()
        && let Some(path) = config.client_token_file.as_deref()
    {
        let resolved_path = expand_tilde(path, home);
        let token = fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read `client-token-file` {}", path.display()))?;
        config.client_token = Some(token.trim().to_string());
    }
    validate_startup_config(&config, "resolved startup config")?;
    Ok(config)
}

fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match (home, path.strip_prefix("~")) {
        (Some(home), Ok(relative)) => home.join(relative),
        _ => path.to_path_buf(),
    }
}

pub fn load_file_overrides() -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let Some(path) = default_config_path() else {
        return Ok((ConfigOverrides::default(), Vec::new()));
    };
    if !path.exists() {
        return Ok((ConfigOverrides::default(), Vec::new()));
    }
    load_overrides_from_path(&path)
}

/// XDG-style config root: `$XDG_CONFIG_HOME`, defaulting to `~/.config`.
/// Used instead of `dirs::config_dir()` because on macOS that resolves to
/// `~/Library/Application Support` and noa standardizes on `~/.config/noa`.
fn xdg_config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|home| home.join(".config")),
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| default_config_path_in(&path))
}

pub fn default_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config")
}

pub fn legacy_toml_config_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| legacy_toml_config_path_in(&path))
}

pub fn legacy_toml_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config.toml")
}

/// Path to the theme-settings-v2 favorites file (R-29/ADR-5): a plain
/// newline-delimited list of favorited theme names, living beside the
/// config file (a UI preference, not session topology — so it survives
/// `window-save-state = never`, unlike `session_state_path`).
pub fn theme_favorites_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| theme_favorites_path_in(&path))
}

pub fn theme_favorites_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("theme-favorites")
}

/// Path to the auto-provisioned `noa-ipc` bearer token file (noa-server spec
/// FR-3), beside the config file. Only read/written when `server-token` is
/// not configured explicitly.
pub fn server_token_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| server_token_path_in(&path))
}

pub fn server_token_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("server-token")
}

/// Path to the persisted session-state file
/// (`<data-dir>/noa/session.json`; on macOS `<data-dir>` is
/// `~/Library/Application Support`). Holds the window/tab/split topology and
/// per-pane cwd restored on launch when `window-save-state` is not `never`.
pub fn session_state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|path| session_state_path_in(&path))
}

pub fn session_state_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("noa").join("session.json")
}

/// Directory holding per-pane persisted scrollback snapshots
/// (`<data-dir>/noa/scrollback/`). Only created when `scrollback-persist` is
/// not `never` — the default leaves no trace of terminal output on disk.
/// See `docs/specs/scrollback-persistence.md`.
pub fn scrollback_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|path| scrollback_dir_in(&path))
}

pub fn scrollback_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("noa").join("scrollback")
}

pub fn load_overrides_from_path(path: &Path) -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    Ok(parse_overrides(path, &source))
}

pub fn validate_startup_config(config: &StartupConfig, context: &str) -> anyhow::Result<()> {
    validate_grid_dimension(config.cols, context, "cols")?;
    validate_grid_dimension(config.rows, context, "rows")?;
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        bail!("invalid {context}: `font_size` must be a positive finite number");
    }
    if !config.minimum_contrast.is_finite() || !(1.0..=21.0).contains(&config.minimum_contrast) {
        bail!("invalid {context}: `minimum_contrast` must be between 1 and 21");
    }
    if !config.sidebar_width.is_finite()
        || !(MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&config.sidebar_width)
    {
        bail!(
            "invalid {context}: `sidebar-width` must be between {MIN_SIDEBAR_WIDTH} and {MAX_SIDEBAR_WIDTH}"
        );
    }
    if !config.sidebar_font_size.is_finite()
        || !(MIN_SIDEBAR_FONT_SIZE..=MAX_SIDEBAR_FONT_SIZE).contains(&config.sidebar_font_size)
    {
        bail!(
            "invalid {context}: `sidebar-font-size` must be between {MIN_SIDEBAR_FONT_SIZE} and {MAX_SIDEBAR_FONT_SIZE}"
        );
    }
    if config.sidebar_preview_lines > MAX_SIDEBAR_PREVIEW_LINES {
        bail!(
            "invalid {context}: `sidebar-preview-lines` must be between 0 and {}",
            MAX_SIDEBAR_PREVIEW_LINES
        );
    }
    Ok(())
}

pub fn validate_grid_dimension(value: u16, context: &str, key: &'static str) -> anyhow::Result<()> {
    if value == 0 {
        bail!(
            "invalid {context}: `{key}` must be an integer between 1 and {}",
            u16::MAX
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> &'static Path {
        Path::new("/tmp/noa-test-config")
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-config-lib-{name}-{}", std::process::id()))
    }

    #[test]
    fn defaults_match_existing_startup_behavior() {
        assert_eq!(
            StartupConfig::default(),
            StartupConfig {
                cols: 80,
                rows: 24,
                font_size: 14.0,
                theme: None,
                theme_appearance: None,
                font: FontConfig::default(),
                palette: Vec::new(),
                clipboard_read: ClipboardAccess::Ask,
                clipboard_paste_protection: true,
                confirm_quit: true,
                title_report: false,
                window_padding_x: None,
                window_padding_y: None,
                background: None,
                foreground: None,
                cursor_color: None,
                selection_foreground: None,
                selection_background: None,
                minimum_contrast: DEFAULT_MINIMUM_CONTRAST,
                cursor_style: None,
                cursor_style_blink: None,
                cursor_stop_blinking_after_secs: DEFAULT_CURSOR_STOP_BLINKING_AFTER_SECS,
                background_opacity: 1.0,
                background_blur_radius: 0,
                configured_background_opacity: 1.0,
                configured_background_blur_radius: 0,
                glassmorphism: GlassLevel::Off,
                background_image: None,
                background_image_opacity: 1.0,
                background_image_position: BackgroundImagePosition::default(),
                background_image_fit: BackgroundImageFit::default(),
                background_image_repeat: false,
                background_image_interval_secs: DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
                scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
                image_storage_limit: DEFAULT_IMAGE_STORAGE_LIMIT,
                window_save_state: WindowSaveState::default(),
                scrollback_persist: ScrollbackPersist::Never,
                scrollback_persist_limit: DEFAULT_SCROLLBACK_PERSIST_LIMIT,
                scrollback_persist_total_limit: DEFAULT_SCROLLBACK_PERSIST_TOTAL_LIMIT,
                scrollback_persist_max_age_days: DEFAULT_SCROLLBACK_PERSIST_MAX_AGE_DAYS,
                macos_option_as_alt: MacosOptionAsAlt::default(),
                macos_titlebar_style: MacosTitlebarStyle::default(),
                macos_non_native_fullscreen: false,
                macos_titlebar_proxy_icon: MacosTitlebarProxyIcon::default(),
                macos_applescript: true,
                quick_terminal_hotkey: Some(DEFAULT_QUICK_TERMINAL_HOTKEY.to_string()),
                quick_terminal_size: DEFAULT_QUICK_TERMINAL_SIZE,
                quick_terminal_autohide: true,
                quick_terminal_screen: QuickTerminalScreen::Mouse,
                quick_terminal_position: QuickTerminalPosition::Top,
                quick_terminal_animation_duration: DEFAULT_QUICK_TERMINAL_ANIMATION_DURATION,
                scratch_terminal_key: Some(DEFAULT_SCRATCH_TERMINAL_KEY.to_string()),
                scratch_terminal_size: DEFAULT_SCRATCH_TERMINAL_SIZE,
                sidebar_enabled: false,
                sidebar_width: DEFAULT_SIDEBAR_WIDTH,
                sidebar_font_size: DEFAULT_SIDEBAR_FONT_SIZE,
                sidebar_hotkey: None,
                sidebar_preview_lines: DEFAULT_SIDEBAR_PREVIEW_LINES,
                resize_overlay: ResizeOverlay::AfterFirst,
                visual_bell: false,
                audible_bell: false,
                audible_bell_when_unfocused: false,
                audible_bell_dock_bounce: false,
                auto_approve: false,
                send_selection_send_enter: false,
                keybinds: Vec::new(),
                server_enable: false,
                server_port: DEFAULT_SERVER_PORT,
                server_bind: DEFAULT_SERVER_BIND.to_string(),
                server_token: None,
                server_scopes: "read".to_string(),
                client_remote: None,
                client_token: None,
                client_token_file: None,
            }
        );
    }

    #[test]
    fn send_selection_send_enter_defaults_off_and_cli_wins_over_file() {
        assert!(!StartupConfig::default().send_selection_send_enter);

        let file = ConfigOverrides {
            send_selection_send_enter: Some(false),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            send_selection_send_enter: Some(true),
            ..Default::default()
        };
        let config = file.merge(cli).apply_to(StartupConfig::default());
        assert!(config.send_selection_send_enter);
    }

    #[test]
    fn quick_terminal_hotkey_defaults_to_cmd_grave() {
        assert_eq!(
            StartupConfig::default().quick_terminal_hotkey.as_deref(),
            Some("cmd+grave")
        );
    }

    #[test]
    fn quick_terminal_screen_defaults_to_mouse() {
        assert_eq!(
            StartupConfig::default().quick_terminal_screen,
            QuickTerminalScreen::Mouse
        );
    }

    #[test]
    fn quick_terminal_position_defaults_to_top() {
        assert_eq!(
            StartupConfig::default().quick_terminal_position,
            QuickTerminalPosition::Top
        );
    }

    #[test]
    fn quick_terminal_size_defaults_to_forty_percent_primary_only() {
        assert_eq!(
            StartupConfig::default().quick_terminal_size,
            QuickTerminalSize {
                primary: Some(QuickTerminalSizeDim::Percent(40.0)),
                secondary: None,
            }
        );
    }

    #[test]
    fn scratch_terminal_key_defaults_to_cmd_shift_t() {
        assert_eq!(
            StartupConfig::default().scratch_terminal_key.as_deref(),
            Some("cmd+shift+t")
        );
    }

    #[test]
    fn scratch_terminal_size_defaults_to_100x25() {
        assert_eq!(
            StartupConfig::default().scratch_terminal_size,
            ScratchTerminalSize {
                cols: 100,
                rows: 25,
            }
        );
    }

    #[test]
    fn quick_terminal_animation_duration_defaults_to_point_two_seconds() {
        assert_eq!(
            StartupConfig::default().quick_terminal_animation_duration,
            DEFAULT_QUICK_TERMINAL_ANIMATION_DURATION
        );
    }

    #[test]
    fn parses_supported_config_keys() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            r#"
window-width = 100
window-height = 30
font-size = 15.5
"#,
        );

        assert!(diagnostics.is_empty());
        assert_eq!(
            overrides,
            ConfigOverrides {
                cols: Some(100),
                rows: Some(30),
                font_size: Some(15.5),
                theme: None,
                font: FontConfig::default(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn cli_overrides_config_file_values() {
        let file = ConfigOverrides {
            cols: Some(100),
            rows: Some(30),
            font_size: Some(15.5),
            theme: Some("3024 Day".to_string()),
            font: FontConfig::default(),
            keybinds: vec![KeybindConfig::Bind {
                trigger: "cmd+t".to_string(),
                action: "tab.new".to_string(),
            }],
            ..Default::default()
        };
        let cli = ConfigOverrides {
            cols: Some(120),
            rows: None,
            font_size: Some(16.0),
            theme: None,
            font: FontConfig::default(),
            keybinds: vec![KeybindConfig::Unbind {
                trigger: "cmd+t".to_string(),
            }],
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(
            config,
            StartupConfig {
                cols: 120,
                rows: 30,
                font_size: 16.0,
                theme: Some("3024 Day".to_string()),
                font: FontConfig::default(),
                keybinds: vec![
                    KeybindConfig::Bind {
                        trigger: "cmd+t".to_string(),
                        action: "tab.new".to_string(),
                    },
                    KeybindConfig::Unbind {
                        trigger: "cmd+t".to_string(),
                    },
                ],
                ..Default::default()
            }
        );
    }

    #[test]
    fn confirm_quit_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "confirm-quit = false");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.confirm_quit, Some(false));

        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert!(default.confirm_quit);

        let file = ConfigOverrides {
            confirm_quit: Some(false),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            confirm_quit: Some(true),
            ..Default::default()
        };
        assert!(
            file.merge(cli)
                .apply_to(StartupConfig::default())
                .confirm_quit
        );
    }

    #[test]
    fn appearance_keys_flow_through_parse_and_apply() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "window-padding-x = 8\n\
             window-padding-y = 4\n\
             background = #101010\n\
             minimum-contrast = 3.5\n\
             cursor-style = bar\n\
             cursor-style-blink = false\n\
             background-opacity = 0.8",
        );
        assert!(diagnostics.is_empty());

        let config = overrides.apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(8.0));
        assert_eq!(config.window_padding_y, Some(4.0));
        assert_eq!(config.background, Some(Rgb::new(0x10, 0x10, 0x10)));
        assert_eq!(config.minimum_contrast, 3.5);
        assert_eq!(config.cursor_style, Some(CursorShape::Bar));
        assert_eq!(config.cursor_style_blink, Some(false));
        assert_eq!(config.background_opacity, 0.8);
    }

    #[test]
    fn scrollback_limit_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "scrollback-limit = 2000000");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.scrollback_limit, Some(2_000_000));

        // Absent key keeps the default; a CLI override wins over the file.
        assert_eq!(
            ConfigOverrides::default()
                .apply_to(StartupConfig::default())
                .scrollback_limit,
            DEFAULT_SCROLLBACK_LIMIT
        );
        let file = ConfigOverrides {
            scrollback_limit: Some(2_000_000),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            scrollback_limit: Some(0),
            ..Default::default()
        };
        assert_eq!(
            file.merge(cli)
                .apply_to(StartupConfig::default())
                .scrollback_limit,
            0
        );
    }

    #[test]
    fn background_image_interval_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "background-image-interval = 12");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.background_image_interval_secs, Some(12));

        assert_eq!(
            ConfigOverrides::default()
                .apply_to(StartupConfig::default())
                .background_image_interval_secs,
            DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS
        );
        let file = ConfigOverrides {
            background_image_interval_secs: Some(12),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            background_image_interval_secs: Some(60),
            ..Default::default()
        };
        assert_eq!(
            file.merge(cli)
                .apply_to(StartupConfig::default())
                .background_image_interval_secs,
            60
        );
    }

    #[test]
    fn window_save_state_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "window-save-state = never");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.window_save_state, Some(WindowSaveState::Never));

        // Absent key keeps the default (which restores).
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.window_save_state, WindowSaveState::Default);
        assert!(default.window_save_state.restores());

        // CLI wins over the file.
        let file = ConfigOverrides {
            window_save_state: Some(WindowSaveState::Never),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            window_save_state: Some(WindowSaveState::Always),
            ..Default::default()
        };
        let resolved = file.merge(cli).apply_to(StartupConfig::default());
        assert_eq!(resolved.window_save_state, WindowSaveState::Always);
        assert!(!WindowSaveState::Never.restores());
    }

    #[test]
    fn scrollback_persist_keys_flow_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "scrollback-persist = tail\n\
             scrollback-persist-limit = 4096\n\
             scrollback-persist-total-limit = 8192\n\
             scrollback-persist-max-age-days = 30",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.scrollback_persist, Some(ScrollbackPersist::Tail));
        assert_eq!(overrides.scrollback_persist_limit, Some(4096));
        assert_eq!(overrides.scrollback_persist_total_limit, Some(8192));
        assert_eq!(overrides.scrollback_persist_max_age_days, Some(30));

        // Absent keys keep the opt-out default: noa persists nothing until asked.
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.scrollback_persist, ScrollbackPersist::Never);
        assert!(!default.scrollback_persist.persists());
        assert!(ScrollbackPersist::Tail.persists());
        assert_eq!(
            default.scrollback_persist_limit,
            DEFAULT_SCROLLBACK_PERSIST_LIMIT
        );
        assert_eq!(
            default.scrollback_persist_total_limit,
            DEFAULT_SCROLLBACK_PERSIST_TOTAL_LIMIT
        );
        assert_eq!(
            default.scrollback_persist_max_age_days,
            DEFAULT_SCROLLBACK_PERSIST_MAX_AGE_DAYS
        );

        // CLI wins over the file.
        let file = ConfigOverrides {
            scrollback_persist: Some(ScrollbackPersist::Tail),
            scrollback_persist_limit: Some(1),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            scrollback_persist: Some(ScrollbackPersist::Never),
            ..Default::default()
        };
        let resolved = file.merge(cli).apply_to(StartupConfig::default());
        assert_eq!(resolved.scrollback_persist, ScrollbackPersist::Never);
        assert_eq!(resolved.scrollback_persist_limit, 1);
    }

    #[test]
    fn scrollback_persist_rejects_an_unknown_mode() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "scrollback-persist = everything");
        assert_eq!(overrides.scrollback_persist, None);
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn scrollback_dir_sits_beside_the_session_state_file() {
        let data_dir = Path::new("/tmp/data");
        assert_eq!(
            scrollback_dir_in(data_dir),
            session_state_path_in(data_dir)
                .parent()
                .expect("session state lives in a directory")
                .join("scrollback")
        );
    }

    #[test]
    fn macos_native_keys_flow_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "macos-option-as-alt = left\n\
             macos-titlebar-style = transparent\n\
             macos-non-native-fullscreen = true\n\
             macos-titlebar-proxy-icon = hidden",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.macos_option_as_alt, Some(MacosOptionAsAlt::Left));
        assert_eq!(
            overrides.macos_titlebar_style,
            Some(MacosTitlebarStyle::Transparent)
        );
        assert_eq!(overrides.macos_non_native_fullscreen, Some(true));
        assert_eq!(
            overrides.macos_titlebar_proxy_icon,
            Some(MacosTitlebarProxyIcon::Hidden)
        );

        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.macos_option_as_alt, MacosOptionAsAlt::None);
        assert_eq!(default.macos_titlebar_style, MacosTitlebarStyle::Native);
        assert!(!default.macos_non_native_fullscreen);
        assert_eq!(
            default.macos_titlebar_proxy_icon,
            MacosTitlebarProxyIcon::Visible
        );

        let file = ConfigOverrides {
            macos_option_as_alt: Some(MacosOptionAsAlt::Left),
            macos_titlebar_style: Some(MacosTitlebarStyle::Transparent),
            macos_non_native_fullscreen: Some(true),
            macos_titlebar_proxy_icon: Some(MacosTitlebarProxyIcon::Hidden),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            macos_option_as_alt: Some(MacosOptionAsAlt::Both),
            macos_non_native_fullscreen: Some(false),
            ..Default::default()
        };
        let resolved = file.merge(cli).apply_to(StartupConfig::default());
        assert_eq!(resolved.macos_option_as_alt, MacosOptionAsAlt::Both);
        assert_eq!(
            resolved.macos_titlebar_style,
            MacosTitlebarStyle::Transparent
        );
        assert!(!resolved.macos_non_native_fullscreen);
        // CLI didn't touch this key, so the file's value still wins.
        assert_eq!(
            resolved.macos_titlebar_proxy_icon,
            MacosTitlebarProxyIcon::Hidden
        );
    }

    #[test]
    fn macos_applescript_parses_and_defaults_true() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "macos-applescript = false");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.macos_applescript, Some(false));

        // Default is on (Ghostty parity): unset config leaves the bridge enabled.
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert!(default.macos_applescript);

        // A file `false` survives a CLI that leaves the key unset, and a CLI
        // `false` still wins over a file `true` (precedence via `.or()`).
        let file = ConfigOverrides {
            macos_applescript: Some(false),
            ..Default::default()
        };
        let resolved = file
            .clone()
            .merge(ConfigOverrides::default())
            .apply_to(StartupConfig::default());
        assert!(!resolved.macos_applescript);

        let cli = ConfigOverrides {
            macos_applescript: Some(false),
            ..Default::default()
        };
        let resolved = ConfigOverrides {
            macos_applescript: Some(true),
            ..Default::default()
        }
        .merge(cli)
        .apply_to(StartupConfig::default());
        assert!(!resolved.macos_applescript);
    }

    #[test]
    fn server_keys_parse_and_default_to_disabled_read_only() {
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert!(!default.server_enable);
        assert_eq!(default.server_port, DEFAULT_SERVER_PORT);
        assert_eq!(default.server_bind, DEFAULT_SERVER_BIND);
        assert_eq!(default.server_token, None);
        assert_eq!(default.server_scopes, "read");

        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "server-enable = true\nserver-port = 9999\nserver-bind = 0.0.0.0\nserver-token = abc123\nserver-scopes = read,control",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.server_enable, Some(true));
        assert_eq!(overrides.server_port, Some(9999));
        assert_eq!(overrides.server_bind, Some("0.0.0.0".to_string()));
        assert_eq!(overrides.server_token, Some("abc123".to_string()));
        assert_eq!(overrides.server_scopes, Some("read,control".to_string()));

        let resolved = overrides.apply_to(StartupConfig::default());
        assert!(resolved.server_enable);
        assert_eq!(resolved.server_port, 9999);
        assert_eq!(resolved.server_bind, "0.0.0.0");
        assert_eq!(resolved.server_token.as_deref(), Some("abc123"));
        assert_eq!(resolved.server_scopes, "read,control");
    }

    #[test]
    fn server_bind_rejects_invalid_ip_and_falls_back_to_loopback_default() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "server-bind = not-an-ip");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(overrides.server_bind, None);

        let resolved = overrides.apply_to(StartupConfig::default());
        assert_eq!(resolved.server_bind, DEFAULT_SERVER_BIND);
    }

    #[test]
    fn client_keys_parse_and_default_to_unset() {
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.client_remote, None);
        assert_eq!(default.client_token, None);
        assert_eq!(default.client_token_file, None);

        let token_path = PathBuf::from("/tmp/noa-client-token");
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "client-remote = host.example:61771\nclient-token = abc123\nclient-token-file = /tmp/noa-client-token",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(
            overrides.client_remote.as_deref(),
            Some("host.example:61771")
        );
        assert_eq!(overrides.client_token.as_deref(), Some("abc123"));
        assert_eq!(overrides.client_token_file.as_ref(), Some(&token_path));

        let resolved = overrides.apply_to(StartupConfig::default());
        assert_eq!(
            resolved.client_remote.as_deref(),
            Some("host.example:61771")
        );
        assert_eq!(resolved.client_token.as_deref(), Some("abc123"));
        assert_eq!(resolved.client_token_file.as_ref(), Some(&token_path));
    }

    #[test]
    fn client_keys_preserve_source_precedence_through_merge_and_apply() {
        let file_token_path = PathBuf::from("/tmp/file-client-token");
        let cli_token_path = PathBuf::from("/tmp/cli-client-token");
        let file = ConfigOverrides {
            client_remote: Some("file.example:61771".to_string()),
            client_token: Some("file-direct-token".to_string()),
            client_token_file: Some(file_token_path),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            client_remote: Some("cli.example:61771".to_string()),
            client_token_file: Some(cli_token_path.clone()),
            ..Default::default()
        };

        let resolved = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(resolved.client_remote.as_deref(), Some("cli.example:61771"));
        assert_eq!(resolved.client_token.as_deref(), Some("file-direct-token"));
        assert_eq!(resolved.client_token_file, Some(cli_token_path));
    }

    #[test]
    fn load_startup_config_reads_client_token_file() {
        let dir = unique_temp_dir("client-token-file");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let token_path = dir.join("client-token");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&token_path, "  file-secret\n").unwrap();
        fs::write(
            &config_path,
            format!("client-token-file = {}", token_path.display()),
        )
        .unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert!(diagnostics.is_empty());
        assert_eq!(config.client_token.as_deref(), Some("file-secret"));
        assert_eq!(
            config.client_token_file.as_deref(),
            Some(token_path.as_path())
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn client_token_file_expands_tilde_against_the_home_directory() {
        let home = unique_temp_dir("client-token-home");
        let token_path = home.join(".config/noa/remote-server-token");
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        fs::write(&token_path, "home-secret\n").unwrap();
        let config = StartupConfig {
            client_token_file: Some(PathBuf::from("~/.config/noa/remote-server-token")),
            ..StartupConfig::default()
        };

        let resolved = finalize_startup_config_with_home(config, Some(&home)).unwrap();

        assert_eq!(resolved.client_token.as_deref(), Some("home-secret"));
        assert_eq!(
            resolved.client_token_file.as_deref(),
            Some(Path::new("~/.config/noa/remote-server-token"))
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn direct_client_token_skips_higher_priority_token_file_read() {
        let dir = unique_temp_dir("client-token-direct");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let missing_token_path = dir.join("missing-client-token");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config_path, "client-token = direct-secret").unwrap();
        let cli = ConfigOverrides {
            client_token_file: Some(missing_token_path.clone()),
            ..Default::default()
        };

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, cli).unwrap();

        assert!(diagnostics.is_empty());
        assert_eq!(config.client_token.as_deref(), Some("direct-secret"));
        assert_eq!(config.client_token_file, Some(missing_token_path));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_client_token_file_error_does_not_expose_contents() {
        let dir = unique_temp_dir("client-token-invalid");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let token_path = dir.join("client-token");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&token_path, b"secret-marker\xff").unwrap();
        fs::write(
            &config_path,
            format!("client-token-file = {}", token_path.display()),
        )
        .unwrap();

        let error =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("client-token-file"));
        assert!(message.contains(token_path.to_string_lossy().as_ref()));
        assert!(!message.contains("secret-marker"));
        fs::remove_dir_all(dir).unwrap();
    }

    // Regression lock for a stale-titlebar-backdrop bug (P2, noa-app): the
    // fix derives the *effective* opacity/blur at a Settings-panel
    // `glassmorphism` commit via these two functions instead of
    // open-coding the branch a second time. Deliberately uses a configured
    // opacity that is NOT `glass_background_opacity(GlassLevel::One)` (0.50)
    // — the bug's original repro happened to configure exactly 0.50, which
    // made the stale (pre-derivation) value coincidentally correct and hid
    // the bug from that one input. A configured value of `1.0` (this test)
    // would have caught it: the stale value and the resolved value disagree.
    #[test]
    fn resolved_background_opacity_and_blur_take_over_only_while_glass_is_on() {
        assert_eq!(
            resolved_background_opacity(GlassLevel::One, 1.0),
            glass_background_opacity(GlassLevel::One)
        );
        assert_eq!(resolved_background_opacity(GlassLevel::Off, 1.0), 1.0);
        assert_eq!(
            resolved_background_blur_radius(GlassLevel::One, 0),
            glass_background_blur_radius(GlassLevel::One)
        );
        assert_eq!(resolved_background_blur_radius(GlassLevel::Off, 0), 0);

        // `apply_glassmorphism_defaults` must agree with these standalone
        // functions — it is written in terms of them, but pin the
        // equivalence directly so the two can't silently diverge.
        let mut config = StartupConfig {
            glassmorphism: GlassLevel::One,
            background_opacity: 1.0,
            background_blur_radius: 0,
            ..StartupConfig::default()
        };
        apply_glassmorphism_defaults(&mut config);
        assert_eq!(
            config.background_opacity,
            resolved_background_opacity(GlassLevel::One, 1.0)
        );
        assert_eq!(
            config.background_blur_radius,
            resolved_background_blur_radius(GlassLevel::One, 0)
        );
    }

    // `glassmorphism = true` (level `1`) owns the window-transparency keys:
    // an explicit `background-opacity = 1.00` (the exact config that made
    // the frosted chrome look like it did nothing — an opaque window shows
    // nothing through) resolves to the recommended pair instead, and the
    // ignored keys are named in a diagnostic rather than silently dropped.
    #[test]
    fn glassmorphism_replaces_configured_background_opacity_and_blur() {
        let dir = unique_temp_dir("glass-overrides");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &config_path,
            "glassmorphism = true\nbackground-opacity = 1.00\nbackground-blur-radius = 0\n",
        )
        .unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(
            config.background_opacity,
            glass_background_opacity(GlassLevel::One)
        );
        assert_eq!(
            config.background_blur_radius,
            glass_background_blur_radius(GlassLevel::One)
        );
        // Below 1.0 is the whole point: that is what makes the window
        // transparent at creation, so the frosted panes have something
        // behind them.
        assert!(config.background_opacity < 1.0);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("background-opacity"), "{message}");
        assert!(message.contains("background-blur-radius"), "{message}");
        fs::remove_dir_all(dir).unwrap();
    }

    // The override is unconditional, not a floor: a deliberately *more*
    // transparent value is replaced too, so "glassmorphism on" always means
    // one known-good look per level.
    #[test]
    fn glassmorphism_replaces_a_more_transparent_configured_value_too() {
        let mut config = StartupConfig {
            glassmorphism: GlassLevel::One,
            background_opacity: 0.4,
            background_blur_radius: 5,
            ..StartupConfig::default()
        };
        apply_glassmorphism_defaults(&mut config);
        assert_eq!(
            config.background_opacity,
            glass_background_opacity(GlassLevel::One)
        );
        assert_eq!(
            config.background_blur_radius,
            glass_background_blur_radius(GlassLevel::One)
        );
    }

    // Default-off contract: with the toggle off the two keys are exactly
    // what the config said, and nothing is reported.
    #[test]
    fn glassmorphism_off_leaves_the_background_keys_alone() {
        let dir = unique_temp_dir("glass-off");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &config_path,
            "background-opacity = 1.00\nbackground-blur-radius = 20\n",
        )
        .unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config.glassmorphism, GlassLevel::Off);
        assert_eq!(config.background_opacity, 1.0);
        assert_eq!(config.background_blur_radius, 20);
        assert!(diagnostics.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    // Nothing to warn about when the user never set the keys — the values
    // being replaced are defaults, not choices.
    #[test]
    fn glassmorphism_alone_forces_the_pair_without_a_diagnostic() {
        let dir = unique_temp_dir("glass-only");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config_path, "glassmorphism = true\n").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(
            config.background_opacity,
            glass_background_opacity(GlassLevel::One)
        );
        assert_eq!(
            config.background_blur_radius,
            glass_background_blur_radius(GlassLevel::One)
        );
        assert!(diagnostics.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    // `--config-default-files=false` resolves through a different loader;
    // the takeover must not be file-path-specific.
    #[test]
    fn glassmorphism_forces_the_pair_without_config_files_too() {
        let cli = ConfigOverrides {
            glassmorphism: Some(GlassLevel::One),
            background_opacity: Some(1.0),
            ..Default::default()
        };

        let (config, diagnostics) = load_startup_config_without_files(cli).unwrap();

        assert_eq!(
            config.background_opacity,
            glass_background_opacity(GlassLevel::One)
        );
        assert_eq!(
            config.background_blur_radius,
            glass_background_blur_radius(GlassLevel::One)
        );
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("background-opacity"));
    }

    // AC-4: `glassmorphism = true` (level `1`) must keep resolving to exactly
    // the pair it always has — existing configs that opted in before the
    // 4-level split shipped must not change appearance.
    #[test]
    fn level_one_matches_the_original_true_resolution() {
        assert_eq!(glass_background_opacity(GlassLevel::One), 0.50);
        assert_eq!(glass_background_blur_radius(GlassLevel::One), 64);
    }

    // AC-5: each level strictly more transparent than the last, so the
    // 4-step split is actually a *step* and not four names for one look.
    // Blur has no headroom left above `1`'s maximum, so it stays flat.
    #[test]
    fn glass_background_opacity_strictly_decreases_by_level() {
        for pair in GlassLevel::ON_LEVELS.windows(2) {
            let (lower, higher) = (
                glass_background_opacity(pair[0]),
                glass_background_opacity(pair[1]),
            );
            assert!(
                lower > higher,
                "{:?}={lower} {:?}={higher}",
                pair[0],
                pair[1]
            );
        }
        let top = glass_background_opacity(
            *GlassLevel::ON_LEVELS
                .last()
                .expect("ON_LEVELS is non-empty"),
        );
        assert!(
            top > 0.0,
            "the top level must still be a real, visible pane"
        );

        for level in GlassLevel::ON_LEVELS {
            assert_eq!(glass_background_blur_radius(level), 64);
        }
        assert_eq!(glass_background_blur_radius(GlassLevel::Off), 0);
        assert_eq!(glass_background_opacity(GlassLevel::Off), 1.0);
    }

    // `resolved_background_opacity` must track the same per-level pair for
    // every on-level, not just `1` (the case the regression-lock test above
    // already pins).
    #[test]
    fn resolved_background_opacity_tracks_every_on_level() {
        for level in GlassLevel::ON_LEVELS {
            assert_eq!(
                resolved_background_opacity(level, 1.0),
                glass_background_opacity(level)
            );
            assert_eq!(
                resolved_background_blur_radius(level, 0),
                glass_background_blur_radius(level)
            );
        }
    }

    // A level-3 config resolves strictly more transparent than a level-1
    // config, end to end through the real loader — not just the standalone
    // functions above.
    #[test]
    fn a_higher_level_resolves_more_transparent_end_to_end() {
        let resolved = |level| {
            let (config, _) = load_startup_config_without_files(ConfigOverrides {
                glassmorphism: Some(level),
                ..Default::default()
            })
            .unwrap();
            config.background_opacity
        };
        for pair in GlassLevel::ON_LEVELS.windows(2) {
            let (lower, higher) = (resolved(pair[0]), resolved(pair[1]));
            assert!(
                higher < lower,
                "{:?}={lower} must be less transparent than {:?}={higher}",
                pair[0],
                pair[1]
            );
        }
    }

    // The diagnostic must interpolate the actual level's pair, not a fixed
    // 0.50/64 — a level-`2` override has to say `0.35`, not the level-1 value.
    #[test]
    fn glass_override_diagnostic_names_the_configured_levels_pair() {
        let cli = ConfigOverrides {
            glassmorphism: Some(GlassLevel::Two),
            background_opacity: Some(1.0),
            ..Default::default()
        };
        let (config, diagnostics) = load_startup_config_without_files(cli).unwrap();
        assert_eq!(
            config.background_opacity,
            glass_background_opacity(GlassLevel::Two)
        );
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("glassmorphism = 2"), "{message}");
        assert!(message.contains("0.35"), "{message}");
        assert!(!message.contains("0.50"), "{message}");
    }

    #[test]
    fn config_debug_redacts_server_and_client_tokens() {
        let startup = StartupConfig {
            server_token: Some("startup-server-secret".to_string()),
            client_remote: Some("remote.example:61771".to_string()),
            client_token: Some("startup-client-secret".to_string()),
            client_token_file: Some(PathBuf::from("/tmp/client-token")),
            ..Default::default()
        };
        let overrides = ConfigOverrides {
            server_token: Some("override-server-secret".to_string()),
            client_token: Some("override-client-secret".to_string()),
            ..Default::default()
        };

        let startup_debug = format!("{startup:?}");
        let overrides_debug = format!("{overrides:?}");

        for output in [&startup_debug, &overrides_debug] {
            assert!(output.contains("server_token: Some(\"<redacted>\")"));
            assert!(output.contains("client_token: Some(\"<redacted>\")"));
            assert!(output.contains("client_token_file:"));
            assert!(output.contains("send_selection_send_enter:"));
        }
        for secret in [
            "startup-server-secret",
            "startup-client-secret",
            "override-server-secret",
            "override-client-secret",
        ] {
            assert!(!startup_debug.contains(secret));
            assert!(!overrides_debug.contains(secret));
        }
    }

    #[test]
    fn cli_overrides_win_for_appearance_keys() {
        let file = ConfigOverrides {
            window_padding_x: Some(2.0),
            background_opacity: Some(0.5),
            minimum_contrast: Some(3.0),
            cursor_style: Some(CursorShape::Block),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            window_padding_x: Some(9.0),
            background_opacity: Some(0.9),
            minimum_contrast: Some(4.5),
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(9.0));
        assert_eq!(config.background_opacity, 0.9);
        assert_eq!(config.minimum_contrast, 4.5);
        // Not overridden by CLI: the file value survives.
        assert_eq!(config.cursor_style, Some(CursorShape::Block));
    }

    #[test]
    fn theme_key_is_accepted() {
        for source in ["theme = 3024 Day", "theme = \"3024 Day\""] {
            let (overrides, diagnostics) = parse_overrides(test_path(), source);

            assert!(diagnostics.is_empty());
            assert_eq!(
                overrides,
                ConfigOverrides {
                    cols: None,
                    rows: None,
                    font_size: None,
                    theme: Some("3024 Day".to_string()),
                    font: FontConfig::default(),
                    ..Default::default()
                }
            );
        }
    }

    #[test]
    fn invalid_file_value_warns_and_uses_default() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "window-width = abc\nwindow-height = 30");

        assert_eq!(overrides.cols, None);
        assert_eq!(overrides.rows, None);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("window-width"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("abc"))
        );
    }

    #[test]
    fn invalid_type_warns_and_uses_default() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "font-size = large");

        assert_eq!(overrides.font_size, None);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("font-size"));
        assert!(diagnostics[0].message.contains("large"));
    }

    #[test]
    fn unknown_key_warns_and_parsing_continues() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "bogus-key = x\nfont-size = 15");

        assert_eq!(overrides.font_size, Some(15.0));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("bogus-key"));
    }

    #[test]
    fn light_dark_syntax_parses_into_theme_appearance() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "theme = light:Foo,dark:Bar");

        assert_eq!(overrides.theme, None);
        assert_eq!(
            overrides.theme_appearance,
            Some(ThemeAppearancePair {
                light: "Foo".to_string(),
                dark: "Bar".to_string(),
            })
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn light_dark_syntax_rejects_a_missing_side() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "theme = light:Foo");

        assert_eq!(overrides.theme, None);
        assert_eq!(overrides.theme_appearance, None);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("light:"));
        assert!(message.contains("dark:"));
    }

    #[test]
    fn invalid_file_values_are_non_fatal() {
        for (source, key) in [
            ("font-size = -1.0", "font-size"),
            ("font-size = inf", "font-size"),
            ("window-height = abc", "window-height"),
        ] {
            let (_, diagnostics) = parse_overrides(test_path(), source);

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(key)),
                "{source:?} should produce {key} diagnostic: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn default_and_legacy_paths_are_hermetic() {
        let base = Path::new("/tmp/noa-config-root");

        assert_eq!(
            default_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config")
        );
        assert_eq!(
            legacy_toml_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config.toml")
        );
    }

    #[test]
    fn load_startup_config_from_preserves_precedence_and_diagnostics() {
        let dir = unique_temp_dir("precedence");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &config_path,
            "bogus-key = x\nfont-size = bad\nfont-size = 16",
        )
        .unwrap();
        let cli = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(18.0),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, cli).unwrap();

        assert_eq!(config.font_size, 18.0);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].message.contains("bogus-key"));
        assert!(diagnostics[1].message.contains("font-size"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_startup_config_from_uses_defaults_when_files_are_absent() {
        let dir = unique_temp_dir("defaults");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// `--config-default-files=false` path: defaults + CLI only, no file IO.
    /// CLI overrides must still win over the built-in defaults.
    #[test]
    fn load_startup_config_without_files_applies_cli_over_defaults() {
        let cli = ConfigOverrides {
            cols: Some(200),
            font_size: Some(21.0),
            ..Default::default()
        };

        let (config, diagnostics) = load_startup_config_without_files(cli).unwrap();

        assert_eq!(config.cols, 200);
        assert_eq!(config.font_size, 21.0);
        assert_eq!(config.rows, StartupConfig::default().rows);
        assert!(config.background_image.is_none());
        assert!(!config.server_enable);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn cli_cols_remain_independent_of_config_pair_rule() {
        let dir = unique_temp_dir("cli-cols");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let cli = ConfigOverrides {
            cols: Some(50),
            rows: None,
            font_size: None,
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let (config, diagnostics) = load_startup_config_from(&config_path, &legacy_path, cli)
            .expect("CLI-only config is valid");

        assert_eq!(config.cols, 50);
        assert_eq!(config.rows, DEFAULT_ROWS);
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_toml_config_warns_without_being_read() {
        let dir = unique_temp_dir("legacy");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_toml_config_warns_even_when_new_config_exists() {
        let dir = unique_temp_dir("legacy-and-new");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config_path, "font-size = 16").unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config.font_size, 16.0);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_structs_do_not_carry_diagnostics() {
        let StartupConfig {
            cols,
            rows,
            font_size,
            theme,
            font,
            ..
        } = StartupConfig::default();
        let ConfigOverrides {
            cols: override_cols,
            rows: override_rows,
            font_size: override_font_size,
            theme: override_theme,
            font: override_font,
            ..
        } = ConfigOverrides::default();

        assert_eq!((cols, rows, font_size, theme), (80, 24, 14.0, None));
        assert_eq!(font, FontConfig::default());
        assert_eq!(
            (
                override_cols,
                override_rows,
                override_font_size,
                override_theme
            ),
            (None, None, None, None)
        );
        assert_eq!(override_font, FontConfig::default());
    }

    #[test]
    fn validates_cli_grid_values_after_merge() {
        let error = validate_startup_config(
            &StartupConfig {
                cols: 0,
                rows: 24,
                font_size: 14.0,
                theme: None,
                font: FontConfig::default(),
                ..Default::default()
            },
            "resolved startup config",
        )
        .unwrap_err();

        assert!(error.to_string().contains("cols"));
    }

    #[test]
    fn validates_cli_font_size_after_merge() {
        let config = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(f32::NAN),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("font_size"));
    }

    #[test]
    fn validates_minimum_contrast_after_merge() {
        let config = ConfigOverrides {
            minimum_contrast: Some(0.5),
            ..Default::default()
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("minimum_contrast"));
    }
}
