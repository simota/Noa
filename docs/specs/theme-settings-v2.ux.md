# UX Design: theme-settings-v2 — 9 Richness Elements

## Metadata

- **target spec:** `docs/specs/theme-settings-v2.md` (the 9 elements involving new UI, from among R-25–R-33)
- **premise:** v1's visual language (AppKit-native cards + TUI fallback, tokens in `crates/noa-app/src/chrome.rs`) is already established. This document invents no new visual language, designing purely as increments to the existing tokens and existing layout coordinate system.
- **design principles:** (1) No mouse support — keyboard-driven is the spec (`handle_theme_settings_key` is the sole input path). (2) Conservation Constraint 5 (single source of truth across the 3 rendering sync points) — new display elements must go through a shared function on par with `RowDraft::display_value`/`settings_row_display_value`, not forking the wgpu path and the native path. (3) Single-accent-hue principle (`chrome.rs`'s `CHROME_ACCENT` doc comment: "chrome/overlay-selection/pane-focus are unified under the same hue") — no new hue is introduced.

## 0. Two factual corrections discovered while grounding

Discrepancies between the spec text (`theme-settings-v2.md`) and the actual code that should be resolved before implementation.

1. **R-27's claim of "same value as `DEFAULT_MINIMUM_CONTRAST`" is incorrect.** The actual value at `noa-render/src/theme.rs:13` is `DEFAULT_MINIMUM_CONTRAST: f32 = 1.0` (a value meaning "no auto-adjustment," see the comment at `noa-config/src/lib.rs:30`), not 4.5. 4.5 is merely a number used as the WCAG AA threshold in test code (`theme.rs:466-472`). **This design treats 4.5:1 as an independent literal constant dedicated to this feature** (does not reference or compare against `DEFAULT_MINIMUM_CONTRAST`).
2. **The native-side footer (`macos_overlay/model.rs:229-236`) is currently mode-independent, always showing text including `←→ adjust` even in Theme mode.** The wgpu side (`theme_picker_overlay_text` vs `settings_rows_overlay_text`) already correctly differentiates by mode. This design's footer refresh (§2) resolves this discrepancy (also filling the gap of "both paths must return the same value," from the perspective of Conservation Constraint 5).

## 1. Match count display (R-26)

### Content / format
- `{highlighted_index + 1} / {filtered_len}` (e.g. `3 / 12`). The denominator is the count **after currently-active narrowing** — not just the fuzzy-text count, but including favorites filter (§4) and light/dark filter (§5) when ON (not fixed at 574). This definition matches R-26's own definition of `filtered_len()` directly.
- At 0 entries: not `0 / 0` but **`No matches`** (there is no precedent for this empty-state text in the existing codebase, so it is newly coined here. Title Case follows the same convention as existing row labels like `Font Size`).

### Placement
- **wgpu**: added to the **right end** of the existing filter row (`LIST_TOP_ROW - 1` = row 3, currently showing `/{filter}` starting at `LIST_COL`). Right-aligned at the right edge (column 27) of the list column width (`LIST_WIDTH` = 28). Even when `/{filter}` is long, right-align the count fixed and truncate the filter side to fit the remaining width, so they don't overlap (following the existing `cols.saturating_sub(...)`-style truncation pattern).
- **Native**: add a new label at the right end of the same row as the filter label (`y=64, x=pad, w=col_split-pad-8`) (`x = col_split - pad - 60, w = 60`-ish, right-aligned, using `mono_digit_font` so digits align — using tabular figures so the text doesn't jitter left-right as digit counts change during scrubbing).
- **Color**: muted (same as the existing filter row). Not an emphasis element, so not always-accent.

## 2. Key-hint footer (refresh common to all modes)

### Policy
New operations (Tab reopen, favorite toggle) are being added, while cramming all keys into a single footer line raises cognitive load (the palette-skill principle). **Only high-frequency, non-obvious operations are pinned to the footer**; low-frequency operations (attribute-filter cycling, favorites-only display) get a local hint next to the control itself (proximity — a hint placed where the eye already is discovers better than cramming into the footer).

### Theme mode footer (new)
```
↑↓ Navigate   ⇥ Settings   ⌃F Favorite   ⏎ Save   Esc Cancel
```
### Settings mode footer (new)
```
↑↓ Navigate   ⇥ Theme   ←→ Adjust   ⏎ Save   Esc Cancel
```
- Add a new `⇥` (Tab) segment: since Tab was previously unresponsive (`toggle_section` empty implementation), and R-25 now gives it new meaning, this is **a change that is undiscoverable without stating it explicitly in the footer**. Write the destination mode name directly (`⇥ Settings` / `⇥ Theme`) so pressing Tab is predictable.
- The `⌃` (Ctrl) glyph directly reuses the existing macOS-standard modifier-key symbol set already adopted by this repo's existing code (`command_palette.rs`'s `keybind_symbols`, `\u{2303}` = ⌃) — no new notation is invented.
- The attribute filter (⌃D) and favorites-only display (⌃⇧F) hints are not included in the footer. They are shown as local captions on §5's chip row itself (below).
- **Implementation-level width concern**: on the wgpu path's fixed `THEME_SETTINGS_COLS = 56` width, the Theme mode's footer text will end up right at the edge of this column count. In a narrow pane it may shrink further via `cols.min(pane_cols - 4)`, so **priority order (trim from the end in this order if trimming is needed): Esc Cancel > Enter Save > ↑↓ Navigate > (Settings only) ←→ Adjust > ⇥ switch > ⌃F Favorite**. The native side has flexible label width, so this is not a practical concern there.
- **Error state unchanged**: retain the existing behavior of replacing the entire footer with danger-colored error text while `commit_error` is set (`Tone::Danger`).
- **Implementation contract**: both wgpu and native must call the same "single shared function returning mode-specific footer text" (to avoid the discrepancy noted in §0-2 from recurring).

## 3. Contrast ratio display + low-contrast warning (R-27)

### Number format
- `Contrast 4.8:1` (1 decimal place, `{ratio:.1}:1`). Call `noa_render::theme::contrast_ratio(default_fg, default_bg)` directly (no new calculation logic — reuse of the existing function only, per R-27's constraint).

### Warning visual representation — a correction to the spec text
The R-27 original text says "either color or icon" as an either/or choice, but **this violates WCAG 2.2 SC 1.4.1 (use of color)** (a color-only warning does not convey to users without color vision). This design **requires both an icon and a text-content change**, with color serving only as a supplement:
- Normal (≥4.5): `Contrast 4.8:1` (muted color, no icon)
- Warning (<4.5): `⚠ Contrast 2.1:1 — low` (danger color = the same family as `crate::chrome::palette().dot_red`, with a `⚠` icon, plus the literal text `— low` appended at the end so the message doesn't depend on color)
- Implement the 4.5 threshold as a literal constant dedicated to this feature (see §0-1; do not reference `DEFAULT_MINIMUM_CONTRAST`).

### Placement
Place it **directly below** the existing fg/bg swatch (the semantic swatches column, `Foreground`/`Background`) in the sample area, so it's clear at a glance what exactly is being contrasted against what. Below the existing truecolor ramp, and below the newly added sample text lines (§7) — a top-to-bottom order of "color swatch → in-context appearance → summary number," moving from abstract to concrete to conclusion.
- **wgpu**: row `LIST_TOP_ROW + 4` (existing: 2 ANSI rows + semantic row + truecolor row = immediately after `LIST_TOP_ROW`–`+3`), starting at `SAMPLE_COL`.
- **Native**: `+14pt` further below `semantic_top + sw + gap + 4.0` (the truecolor-ramp band), starting at `sample_x`, 11pt, `system_font` (same family as existing labels).

## 4. Favorite star mark (R-29)

### In-list-row display
- The star (`★`, U+2605) is shown **only on favorited rows**. Non-favorited rows do not permanently show an outline like `☆` — showing an empty icon on all 574 rows would add visual noise (the cognitive-load principle). "Star present = favorite, absent = not" as a simple binary is sufficient.
- **Position**: right end of the row (after the theme name, right-aligned). The left end of the name is the fuzzy-highlight target, so leave it untouched.
  - wgpu: 1 character at `LIST_COL + LIST_WIDTH - 1` (the list column's final column) of each theme row.
  - Native: add a `14×14pt` small label at the right end of the row label rect (roughly `x = col_split - pad - 16`).
- **Color**: accent (`CHROME_ACCENT`). No new hue (gold/yellow) is introduced, preserving the single-accent-hue principle (design principle 3 above). Even adjacent to the highlighted row's accent-colored text, they are distinguishable by shape (icon vs. character), so there is no clash.

### Toggle key assignment
- **`⌃F` (Ctrl+F)** — toggles the favorite status of the highlighted row. Ctrl-modified doesn't conflict with fuzzy text entry (bare character keys), since Ctrl+character is not inserted into the fuzzy filter's text buffer. Confirmed that the native side's filter label is in fact a non-editable `NSTextField` (`make_label`, non-focusable), so it does not conflict with Cocoa's standard Emacs-style `Ctrl+F` (cursor-forward) text-editing binding — all key input is exclusively handled by `handle_theme_settings_key` on the Rust side via winit.
- **`⌃⇧F` (Ctrl+Shift+F)** — toggles the "favorites only" display filter (placed on §5's chip row). Designed as a consistent pair extending the same `F` mnemonic with Shift, from "per-row operation" to "whole-list view switch."

### Filter-ON state display
Shown as a single chip on §5's chip row: `★ Favorites` (ON: accent text color + a `selected_bg`-equivalent background wash) / `☆ Favorites` (OFF: muted, no background).

## 5. Light/dark attribute filter chip (R-30)

### UI form
A 3-value segment: `All · Dark · Light`. **No new primitive (rounded-pill background) is invented** — directly reuse the existing "selected row" representation (native: `selected_bg` background wash via `tint_layer` + accent text color; wgpu: accent text color + marker), applying the same treatment to the active segment.

- **wgpu**: `All  [Dark]  Light` — only the active segment is bracketed with `[` `]` in accent color, inactive ones are plain muted-color text (following the existing convention of "text differentiated by color only," as in footer/hint text, no new rule-line primitive).
- **Native**: 3 small labels laid out horizontally, with only the active one having a `selected_bg` `tint_layer` behind it (same `6.0` radius as the existing selected row).

### Placement
Newly added as a "filter chip row" below §1's match-count row. **Left**: the light/dark chip `All · Dark · Light`. **Right**: the favorites chip (§4) `☆/★ Favorites` (following the same right-aligned pattern as §1's match count, matching the visual rhythm to the count row).
- wgpu: insert the new row at `LIST_TOP_ROW`, shifting the existing theme list down by one row (effectively `LIST_TOP_ROW`+1; whether to reduce `LIST_ROWS` from 8→7 or expand `THEME_SETTINGS_ROWS` from 25→26 is an implementation decision — the former is preferable if card-height stability is prioritized).
- Native: add a 20pt row before `list_top` (currently 86pt), moving `list_top` to 106pt, and folding this +20pt into the card-height calculation (`needed()`). Since native already computes card height dynamically, growing the height by 20pt is more natural than shrinking the list — it's fine for the fixed-cell-grid wgpu path and the dynamic-layout native path to use different degradation strategies here (they already differ for the same reason elsewhere).

### Key assignment
- **`⌃D` (Ctrl+D)** — cycles All → Dark → Light → All. The mnemonic is weak (D = display mode), but `⌃L` was avoided because of the strong association with a terminal's "clear screen" — that association would clash. A `⌃D cycle` local caption is placed next to the chip row to aid discoverability (not listed in the footer, see §2).

## 6. Post-commit undo toast (R-31)

### Relationship to the existing toast system
The existing resize toast (`state.resize_overlay: Option<(String, Instant)>`, `RESIZE_OVERLAY_DURATION = 750ms`) is an information-only pill (`draw_toast_card`/`rebuild_toast`) — **centered, fixed rectangular size, single-line label, no scrim, no buttons**. The undo toast reuses this visual language directly, inventing no new appearance. In implementation, `resize_overlay` is generalized by tagging it with `ToastKind` (as noted in the spec body's L2).
- Since `commit_theme_settings` **closes the session** on successful commit (`self.theme_settings.take()`), the undo toast appears **after** the overlay has fully closed, on the same centered pane position as a normal toast — there is no positional conflict (the design of sharing the same slot as the resize toast is valid).

### Text (right after a successful Enter Save)
- **When committed in Theme mode**: `Theme set to "{name}" · ⌘Z to undo`
- **When committed in Settings mode**: `Settings saved · ⌘Z to undo` (since multiple fields can change at once, don't enumerate individual names as with Theme)
- Matched to the same concise English tone as the existing `Chrome/tabs update on Save` and `Failed to save settings: {err}` (no punctuation, abbreviated verbs).

### Key assignment
- **`⌘Z` (Cmd+Z)** — the most predictable assignment, matching the system-standard undo convention. Confirmed cmd+z is unused in the existing `KeybindEngine::default()` list. Introduced as a new global command that takes effect after the overlay has closed (re-commits the snapshot values using the same write function as `commit` — no new write path, per spec R-31's constraint).
- Once the undo toast's display time has elapsed, `⌘Z` silently no-ops ("toast gone = undo window closed" is the mental model reflected directly — no additional modal or "cannot be undone" notification is shown, matching magi scope's interaction-cost-minimization policy).

### Display duration
The resize toast's 750ms is intentionally short, for a use case where it appears repeatedly during continuous dragging. The undo toast demands a **one-time decision**, so the same 750ms is too short (not enough time to read → decide → physically press a key). Drawing on other macOS apps' undo-toast conventions (Mail's "Undo Send," etc., ranging several to a dozen-plus seconds), **6000ms (6 seconds) is the recommended value** (an answer to the value the spec body's Open Questions left undecided). If it overlaps in display with the resize toast, the newer one (undo) immediately replaces the older, as noted in the spec body's L2 edge case.

## 7. Preview representative sample lines (R-33)

### Design approach
The current `sample_swatches` (16 ANSI color blocks + fg/bg/cursor/selection blocks + truecolor ramp) is **kept as-is** (it still functions as an accurate color reference). **Directly below it**, add 3 lines reconstructing the same color data as actual text lines — stacking "in-context appearance (concrete)" beneath "color swatches (abstract)." No new color-derivation logic is added (the existing `Swatch` enum values are reused directly, per R-33's constraint).

Content of the 3 lines (all monospace, spanning the full width of the sample area):
1. **Normal text line**: `default_fg` on `default_bg`. Text: `Sample text on background`
2. **Emphasized (ANSI) text line**: 4 words that carry a common log-color association, colored with the foreground colors of the ANSI base color slots (1=red, 2=green, 3=yellow, 4=blue), on a `default_bg` background. Text: `error` (red) `warning` (yellow) `info` (blue) `ok` (green) — mimicking a color pattern actually seen in shell/log output, giving a more practically useful check than a plain color chip.
3. **Selection line**: `default_fg` on `selection_bg` (since `ThemeDef` has no dedicated selection foreground color, follow the same constraint as the existing `sample_swatches`'s `Selection` swatch, using the default fg as-is). Text: `Selected text`

### Placement
In the sample area, directly below the truecolor ramp (above §3's contrast display). Each line is about 1 line-height of 7–8 characters (wgpu: 1 text row, native: 1 `system_font(12.5)` label row). All 3 lines together comfortably fit in newly available space (currently unused space beyond 170pt on native, and rows 12–23 unused on wgpu).

### 3-rendering-sync-point contract
Both wgpu (`theme_picker_overlay_text`) and native (`theme_settings_view_model`) must call a **single shared function** that generates these 3 lines (e.g. `sample_text_lines(theme) -> [(String, Rgb, Rgb); 3]`, added to `sample.rs`). Neither may fork on its own (a contract AC-48 explicitly verifies).

## 8. Visual continuity on Tab switching (R-25)

### Design goal
Tab is not "opening a new modal" but "changing the presentation of the same session" (R-25's architectural premise: neither `preview_theme` nor any live-applied value is altered). The visuals must not betray this either — **repeating the initial-open fade-in (`DUR_FAST` = 120ms opacity 0→1 tween, used in `app/render.rs`) on every Tab switch would make it look like "reopened every time," contradicting the continuity goal.**

### Behavior
- **Scrim/opacity does not change** (stays fully opaque at all times). The initial-open-only fade-in tween does not fire on Tab switching.
- **Only the card frame size transitions**: Theme mode and Settings mode have different card heights (Theme has a variable height depending on theme count; Settings is fixed at 16 rows). With the center point fixed, ease from the old mode's size to the new mode's size over `DUR_FAST` (120ms, reusing the existing constant — no new value is created).
- **Content switches instantly** (at t=0, swap directly to the new mode's labels/list. Do not perform "content-side cross-fading," which would require holding both modes' data simultaneously in a single frame — not worth the implementation cost, and the session inherently only ever holds one mode's data at a time anyway).
- **Badge (`Chrome/tabs update on Save`) / real color during preview**: since `gpu.preview_theme` does not change, the exact same value continues to be shown before and after the switch. While guaranteeing this is the data layer's responsibility per R-25, **visually too, the badge must not flicker as if it briefly vanished and reappeared** — let the existing badge-visibility condition (`badge_visible()`) pass through unchanged, so the Tab transition itself never triggers a false recomputation.

## 9. Full microcopy list (English)

Unified to match the existing panel's text tone (minimal punctuation, Title Case for row labels only, concise short phrases for hints/toasts).

| Usage | Text | Notes |
|---|---|---|
| Match count (normal) | `{n} / {m}` | e.g. `3 / 12` |
| Match count (0 results) | `No matches` | Newly coined (no existing precedent) |
| Footer (Theme, normal) | `↑↓ Navigate   ⇥ Settings   ⌃F Favorite   ⏎ Save   Esc Cancel` | See §2; trimmed from the end when width is exceeded |
| Footer (Settings, normal) | `↑↓ Navigate   ⇥ Theme   ←→ Adjust   ⏎ Save   Esc Cancel` | Same as above |
| Footer (error) | (unchanged — uses the existing `commit_error` text as-is) | |
| Contrast (normal) | `Contrast {x.x}:1` | e.g. `Contrast 4.8:1` |
| Contrast (warning) | `⚠ Contrast {x.x}:1 — low` | Not color-only dependent (§3) |
| Favorites chip (ON) | `★ Favorites` | accent color |
| Favorites chip (OFF) | `☆ Favorites` | muted color |
| Light/dark chip | `All · Dark · Light` (wgpu form: `All  [Dark]  Light`) | Only the active one is emphasized |
| Light/dark chip local hint | `⌃D cycle` | Not listed in footer |
| Sample line 1 | `Sample text on background` | |
| Sample line 2 | `error` `warning` `info` `ok` | ANSI 1/3/4/2 foreground colors |
| Sample line 3 | `Selected text` | selection_bg background |
| Undo toast (Theme) | `Theme set to "{name}" · ⌘Z to undo` | |
| Undo toast (Settings) | `Settings saved · ⌘Z to undo` | |

## Open Questions (require implementation-time judgment)

1. **How to absorb the wgpu card's added row count** (§5): whether to reduce `LIST_ROWS` from 8→7 or expand `THEME_SETTINGS_ROWS` from 25→26. If card-height stability (compatibility with existing windows) is prioritized, the former is recommended, but the implementation cost / impact on existing tests should be judged in the atlas/implementation loop.
2. **`⌃D`'s weak mnemonic**: `D` is somewhat arbitrary as the light/dark filter cycle key (considered, but `L` was avoided due to its strong association with a terminal's "clear screen"). A better assignment is welcome to substitute here — since the chip row's local caption already lowers reliance on memorization, the key's own learning cost is judged acceptable.
3. **The undo toast's 6-second value**: a design-side proposal answering a value the spec body's Open Questions left undecided; not a hard requirement. If the UX proves too short/too long in practice at implementation time, it may be adjusted (the design should require changing only a single constant).
4. **Word choice for sample line 2** (`error`/`warning`/`info`/`ok`) is a proposal based on log-color convention; it would also work with the other 4 words (e.g. using the literal ANSI names `red`/`green`/`yellow`/`blue`). Implementation-time discretion is permitted.

---

## Addendum A — Risk Gate (echo) reflected, 2026-07-11

### A-1. Local hint for ⌃⇧F (echo pre-implementation fix #3)
Add a local caption to §5's chip row's favorites chip on the right, symmetric with the light/dark chip's `⌃D cycle`:
- Display: `☆ Favorites ⌃⇧F` (OFF) / `★ Favorites ⌃⇧F` (ON). The caption part `⌃⇧F` is the same muted color and size as `⌃D cycle`.
- Both wgpu and native may draw this as a single string, joined with a single space after the chip body's own label (no new primitive needed).
- Still not listed in the footer (§2's policy maintained).

### A-2. Highlight/preview behavior on filter switching (echo pre-implementation fix #4)
When `filtered` is recomputed due to a toggle of the attribute filter (⌃D), the favorites filter (⌃⇧F), or Tab carryover application:
1. **If the currently highlighted theme remains in the new filtered set**: the highlight follows that theme (index recalculated). Preview unchanged.
2. **If it was excluded but the list is non-empty**: `preview_theme` is not changed (an extension of the AC-16 "retain previous preview" pattern). The highlight moves to the front (index 0), but `highlight_moved` is reset so that **a new preview does not fire until the user explicitly moves up/down**. This structurally forbids "preview changing on its own just from toggling a filter."
3. **If the list becomes empty**: apply the existing AC-16 behavior as-is (empty list, previous preview retained).
