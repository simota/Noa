# macOS Titlebar Proxy Icon & Quick Look

## Metadata

- slug: macos-quick-look-proxy-icon
- feature title: Titlebar Proxy Icon + Force-Click Quick Look
- status: draft
- owner: noa maintainers
- current phase: IMPLEMENTED (2026-07-10; unit ACs verified, manual-visual
  ACs pending a live-app pass — see L3 "Manual verification")
- parent specs: tab-title.md (title/diff-apply pipeline neighbor — proxy icon
  reuses its choke-point pattern), ghostty-config.md (config key conventions
  — proxy icon reuses the `macos-*` 6-site key pattern)

## L0 — Vision

### Problem

Two macOS-native affordances that Ghostty exposes are missing in noa:

1. **Titlebar proxy icon** — the small folder/file glyph in a native
   window's titlebar that represents the window's current directory,
   supports Cmd-click path-chain navigation and drag-to-Finder, and is
   standard `NSWindow.representedURL` behavior. Ghostty derives it from the
   focused surface's OSC 7 pwd.
2. **Quick Look word lookup** — force-clicking (or deep-pressing) a word in
   the terminal opens the system dictionary/definition popup for that word,
   without disturbing the user's existing text selection.

Both are pure host-OS affordances layered on state noa already tracks
(`Terminal.cwd` from OSC 7) or can derive read-only from the grid (word
under the pointer). Neither requires new terminal semantics.

### Audience

- macOS users of noa who navigate by directory via the titlebar (Finder
  drag, Cmd-click breadcrumb) the way they do in Terminal.app/iTerm2/
  Ghostty.
- macOS users who force-click/deep-press words to check spelling or
  meaning without leaving the terminal.

### Job To Be Done

Let a user see and act on "what directory is this window in" from the
titlebar, and let them look up a word under the pointer with a physical
gesture — both without breaking existing selection, title, or shell-input
behavior.

### Success Definition

Behavior matches Ghostty's observable behavior: the proxy icon reflects
the *focused* pane's pwd, follows focus across splits/tabs, respects a
`visible|hidden` config switch, and offers no file-existence guarantee.
Quick Look triggers only on a genuine OS-level force click (respecting the
system's `forceClick` preference), extracts the word under the mouse
without touching the current selection, and degrades to a no-op when there
is no word or no font. OSC 7 parsing is hardened to Ghostty's anti-spoofing
rules (empty value ⇒ pwd reset, hostname must be local) as a prerequisite
for trusting the pwd that drives the icon.

## Scope

### In scope

- Config key `macos-titlebar-proxy-icon` (`visible|hidden`, default
  `visible`), wired through the standard 6-site noa-config pattern.
- `NSWindow.representedURL` set from the focused pane's `Terminal.cwd`,
  diff-cached like the title choke point, updated when focus changes
  across splits/tabs and when the focused pane's cwd changes.
- OSC 7 hardening: empty value resets pwd (not `Malformed`); hostname must
  be empty, `localhost`, or match the local hostname (else the update is
  ignored); `kitty-shell-cwd://` accepted as an equivalent scheme.
- Force-click (`TouchpadPressure` stage-2, first transition, gated by the
  `com.apple.trackpad.forceClick` OS preference) detection in the event
  loop.
- A read-only word-extraction API returning the word under a viewport
  point and its start point, without mutating selection.
- `showDefinitionForAttributedString:atPoint:` popup invocation with
  correct coordinate conversion (physical px → NSView points → AppKit
  bottom-left origin) and a best-effort `NSFont` attribute.
- Unit tests for the OSC 7 edge cases and for word-extraction
  non-mutation.

### Out of scope (non-goals)

- **pwd → title fallback.** Ghostty falls back the window title to the
  pwd when no OSC 0/2 title was ever set. This conflicts with the locked
  precedence in tab-title.md (override > shell title > `"Noa"` fallback,
  with no pwd tier). Recorded as `#TODO(agent): evaluate a pwd-as-title
  fallback in a follow-up spec once tab-title precedence can be revisited`
  — not built here.
- `QLPreviewPanel` file-content preview (Quick Look's other, unrelated
  meaning — full-file preview panel). This spec covers only the
  dictionary/definition word-lookup popup, matching what Ghostty
  implements.
- Non-macOS platforms (proxy icon and force click are AppKit/trackpad
  concepts; no Linux/Windows equivalent is specified).
- Shell-integration auto-injection of OSC 7 (assumes the user's shell
  already emits it, as noa's OSC 7 parsing already assumes today).
- File-existence validation before setting `representedURL` (Ghostty does
  not check either; a stale/deleted pwd still shows the icon).

## Reuse / constraint findings

Enablers:

- Title choke-point pattern to mirror for the proxy icon: computed once
  per frame from the focused pane, applied diff-only
  (`crates/noa-app/src/app/render.rs:165-167`, `if state.title != title {
  window.set_title(...) }`); title derivation for the focused pane is
  `crates/noa-app/src/app/render.rs:99-115`. The proxy icon needs the same
  shape: derive from the focused pane's `Terminal.cwd`, diff-cache, apply
  via a new macOS-native setter.
- OSC 7 is already wired end-to-end: `crates/noa-grid/src/osc.rs:373`
  (`parse_cwd_osc`) → `crates/noa-grid/src/terminal/handler.rs:454-458`
  sets `Terminal.cwd: Option<String>`
  (`crates/noa-grid/src/terminal.rs:57`). Reset to `None` on `full_reset`
  (`handler.rs:292`). Existing tests: `crates/noa-grid/src/tests/osc.rs:56`.
- macOS native-call pattern: `crates/noa-app/src/macos_window.rs` (508
  lines), `objc2` `msg_send![ns_view, window]` → setters on `AnyObject`.
  New `set_represented_url(window, Option<&str>)` and the Quick Look
  `showDefinition` wrapper both go here. Dependencies already present:
  `objc2` 0.6, `objc2-foundation` 0.3 (`NSString`, `NSURL` features),
  `objc2-app-kit` 0.3 (`crates/noa-app/Cargo.toml:32-40`).
- Config key 6-site pattern, exemplified by `macos-titlebar-style`:
  `crates/noa-config/src/lib.rs:386` (resolved field), `:531` (override
  `Option`), `:608-610` (precedence), `:691-693` (default);
  `crates/noa-config/src/parser/overrides.rs:218-220` + `:431` (key
  registration); `crates/noa-config/src/parser/values.rs:570` (value
  parser); consumption in `crates/noa-app/src/app/config.rs`; CLI dump in
  `crates/noa-app/src/cli.rs:344`. Existing `macos-*` keys:
  `macos-option-as-alt`, `macos-titlebar-style`,
  `macos-non-native-fullscreen`.
- `winit` 0.30.13 delivers `WindowEvent::TouchpadPressure { pressure,
  stage }` on macOS from `pressureChangeWithEvent:` (verified in winit
  source, `platform_impl/macos/view.rs:758-765`). Noa currently swallows
  it in the generic wildcard `_ => {}` arm at
  `crates/noa-app/src/app/event_loop.rs:488`; a dedicated
  `WindowEvent::TouchpadPressure { pressure, stage }` arm must be added
  above that wildcard (risk-gate finding #1 corrected an earlier citation
  of lines 296-306, which are unrelated mouse/cursor arms).
- Coordinate mapping already exists for mouse→cell:
  `crates/noa-app/src/mouse.rs:150`
  (`physical_position_to_grid_point`). Word-boundary logic already
  exists: `crates/noa-grid/src/screen/text.rs:381` (`word_bounds`).

Constraints:

- **PARITY GAPS in current OSC 7 parser**
  (`crates/noa-grid/src/osc.rs:437`, `parse_file_uri_path`):
  1. an empty OSC 7 value is currently classified `Malformed` instead of
     triggering a pwd reset;
  2. any hostname is accepted unconditionally — no local-host validation
     (SSH sessions can spoof a remote pwd as if local);
  3. the `kitty-shell-cwd://` scheme is not accepted.
  These become `REQ-OSC-*` below.
- `select_word_at_viewport_point` (`crates/noa-grid/src/terminal.rs:238`)
  is the only existing word-extraction entry point and it **mutates
  selection** (it is the double-click-to-select-word implementation).
  Quick Look must not touch selection, so a new **read-only**
  `word_at_viewport_point` is required — it must not call into the
  selection-setting path at all, not merely restore selection afterward.
- Noa grid math is physical-pixel based; AppKit `NSView` points are
  physical px divided by `scale_factor`; AppKit's coordinate origin is
  bottom-left, so the y coordinate handed to `showDefinitionForAttributedString:atPoint:`
  must be flipped (`view_height - y`) relative to noa's top-left-origin
  grid math.
- `representedURL` must track the **focused** pane specifically — in a
  split, switching focus between panes with different cwds must update
  the icon even if no OSC 7 sequence fires in that moment (mirrors how
  title-tracking already follows focused-pane changes, per
  `render.rs:99-115`).
- Runtime config changes to `macos-titlebar-proxy-icon` only take effect
  on the *next* cwd change (Ghostty's own documented quirk: the setter is
  only invoked from the OSC 7 handling path, not from a config-reload
  hook) — this is preserved as-is, not treated as a bug.

## L1 — Requirements

### Functional — Proxy icon (`REQ-PXI-*`)

- **REQ-PXI-1** (MUST): A config key `macos-titlebar-proxy-icon` accepts
  `visible` or `hidden`, defaults to `visible`, and is wired through the
  standard 6-site noa-config pattern (resolved field, override, precedence,
  default, key registration, value parser) plus CLI dump.
- **REQ-PXI-2** (MUST): When the config value is `visible`, the focused
  window's `representedURL` is set to the focused pane's `Terminal.cwd`
  whenever that value is `Some`; when `Terminal.cwd` is `None`, or the
  config value is `hidden`, `representedURL` is cleared (`nil`).
- **REQ-PXI-3** (MUST): The icon tracks the *focused* pane specifically —
  switching focus between split panes or tabs with different cwds updates
  the icon to the newly focused pane's cwd.
- **REQ-PXI-4** (MUST): The update is diff-cached (mirrors
  `render.rs:165-167`'s `if state.title != title` guard): the native
  setter is invoked only when the resolved cwd for the focused pane
  actually changed since the last frame, not on every redraw.
- **REQ-PXI-5** (MUST): No file-existence check is performed before
  setting `representedURL` — a pwd pointing at a deleted/renamed directory
  still populates the icon (Ghostty parity).
- **REQ-PXI-6** (SHOULD): Document that a runtime config toggle between
  `visible`/`hidden` only visibly applies on the *next* cwd change for the
  focused pane, matching Ghostty's own documented behavior (not treated as
  a defect).

### Functional — Quick Look (`REQ-QLK-*`)

- **REQ-QLK-1** (MUST): A force click is detected as a
  `WindowEvent::TouchpadPressure` transition into `stage == 2` (first
  transition only — repeated events at the same stage do not re-trigger),
  gated by the OS preference `com.apple.trackpad.forceClick`: the feature
  fires when the key is **absent or `true`**, and is suppressed only when
  the key is explicitly `false` (`objectForKey:` nil-check before the
  bool read — a bare `boolForKey:` returns `false` for an absent key and
  would silently disable Quick Look on never-customized systems, even
  though Apple's factory default is force-click *enabled*). This is a
  deliberate robustness refinement over Ghostty's bare `bool(forKey:)`
  read (risk-gate finding #2, RPN 448).
- **REQ-QLK-2** (MUST): A new read-only API returns the word (and its
  start point) at a given viewport point without mutating selection state
  — it does not call any selection-setting code path, including
  temporarily.
- **REQ-QLK-3** (MUST): The mouse position at force-click time is
  converted to a grid viewport point using the existing
  `physical_position_to_grid_point` mapping, then the word is extracted
  via `REQ-QLK-2`'s API.
- **REQ-QLK-4** (MUST): When a non-empty word is found, its screen
  position is converted from physical-pixel/top-left-origin coordinates to
  AppKit point/bottom-left-origin coordinates
  (`view_points = physical / scale_factor`, `y_flipped = view_height -
  y`), and `showDefinitionForAttributedString:atPoint:` is invoked on the
  content `NSView` with that word and position.
- **REQ-QLK-5** (MUST): When no word is found at the force-click point
  (empty result), no popup is shown and no further action is taken (the
  event is treated as a no-op for this feature — it does not propagate to
  or trigger any other selection/menu behavior).
- **REQ-QLK-6** (SHOULD): The attributed string passed to
  `showDefinitionForAttributedString:atPoint:` carries an `NSFont`
  attribute derived from the configured font family and point size,
  looked up via `fontWithName:size:`; if the lookup fails, the string is
  still shown without a font attribute (graceful degradation, never a
  hard failure).

### Functional — OSC 7 hardening (`REQ-OSC-*`)

- **REQ-OSC-1** (MUST): An OSC 7 sequence with an empty value is treated
  as a pwd reset (`Terminal.cwd` becomes `None`, clearing any proxy icon),
  not as a `Malformed` sequence.
- **REQ-OSC-2** (MUST): The hostname component of the OSC 7 URL is
  validated: an empty host, `localhost`, or a host matching the machine's
  local hostname is accepted and the pwd is applied; any other hostname
  (e.g. a remote SSH host) causes the update to be ignored, leaving
  `Terminal.cwd` at its previous value. **Match rule** (risk-gate finding
  #3, RPN 336 — a naive exact match would regress the *existing* sidebar
  cwd pipeline fed by `Terminal.cwd`): the comparison is case-insensitive
  and accepts a match between either side's full string **or** first
  dot-separated label (so `sg-h-0001`, `SG-H-0001.local`, and an FQDN all
  match a machine named `SG-H-0001`); if the local hostname cannot be
  resolved at all, validation **fails open** (accepts the update) — a
  false accept only risks the icon, a false reject breaks the shipped
  sidebar feature.
- **REQ-OSC-3** (SHOULD): The `kitty-shell-cwd://` URL scheme is accepted
  as an equivalent to `file://` for the purposes of pwd extraction. Its
  host is validated the same way as REQ-OSC-2; its path is taken raw,
  with no percent-decoding — kitty's own semantics for this scheme,
  unlike `file://`.

### Non-Functional

- **REQ-NF-1**: The read-only word-extraction API (`REQ-QLK-2`) is
  unit-testable directly against a `Terminal`/`Screen` without a live
  `NSView` or window, and a test asserts selection state is unchanged
  before/after a call.
- **REQ-NF-2**: The OSC 7 edge cases (`REQ-OSC-1`, `REQ-OSC-2`,
  `REQ-OSC-3`) are unit-testable in `noa-grid` without any app/GUI
  dependency.
- **REQ-NF-3**: `cargo test --workspace` and `cargo clippy --workspace`
  stay green; existing OSC 7 tests
  (`crates/noa-grid/src/tests/osc.rs:56`) continue to pass except where
  this spec intentionally changes their expected outcome (empty-value
  case).
- **REQ-NF-4**: The `macos-titlebar-proxy-icon` config key follows the
  existing `macos-*` 6-site pattern exactly — no new precedence mechanism,
  no new config-file syntax.

## L2 — Detail

### noa-config

- New resolved field + override `Option` for `macos-titlebar-proxy-icon`
  (`visible|hidden`, default `visible`), following the exact touch points
  of `macos-titlebar-style`: `lib.rs:386`-style resolved field, `:531`-style
  override, `:608-610`-style precedence, `:691-693`-style default;
  key registration in `parser/overrides.rs:218-220` + `:431`; value parser
  in `parser/values.rs:570`; CLI dump entry alongside `cli.rs:344`.

### noa-grid

- `crates/noa-grid/src/osc.rs:437` (`parse_file_uri_path`): add the
  empty-value ⇒ reset branch (REQ-OSC-1); add hostname validation against
  empty/`localhost`/local-hostname (REQ-OSC-2); add `kitty-shell-cwd://`
  scheme acceptance (REQ-OSC-3). No change to `Terminal.cwd`'s type or the
  existing reset-on-`full_reset` behavior (`handler.rs:292`).
- New public, read-only `word_at_viewport_point(&self, point) ->
  Option<(String, ViewportPoint)>` on `Terminal` (or `Screen`), built on
  the existing `word_bounds` (`screen/text.rs:381`) logic but returning
  the word/start-point pair without calling
  `select_word_at_viewport_point`'s selection-mutating path.

### noa-app (only crate touching wgpu/winit/objc2 for this feature)

- `crates/noa-app/src/macos_window.rs`: new
  `set_represented_url(window: &AnyObject, path: Option<&str>)` (NSURL
  fileURLWithPath / clear to nil); new `show_definition(ns_view:
  &AnyObject, text: &str, font: Option<&AnyObject>, point: NSPoint)`
  wrapping `showDefinitionForAttributedString:atPoint:`.
  **Convention mandate** (risk-gate finding #6): construct
  `NSAttributedString` / `NSFont` / `NSDictionary` via the codebase's
  existing raw `AnyClass::get` + `msg_send!` pattern (as `NSColor`,
  `NSView` already are) — do **not** use objc2-foundation/app-kit typed,
  feature-gated APIs. The `NSAttributedString` cargo feature is only
  transitively enabled via `muda` today; relying on it is fragile and
  declaring it is an unnecessary Cargo.toml change. `NSPoint`
  struct-passing via `msg_send!` is already proven at 30+ call sites in
  `macos_overlay/imp/appkit.rs`.
- Proxy icon apply site: alongside the title diff-apply in
  `app/render.rs` (mirrors `render.rs:165-167`) — compute the focused
  pane's resolved cwd once per frame, diff-cache against the last-applied
  value per window, call `set_represented_url` only on change, gated by
  the resolved `macos-titlebar-proxy-icon` config value.
- Force-click detection: a new `TouchpadPressure` arm above
  `app/event_loop.rs`'s wildcard `_ => {}` gains stage-transition tracking
  (state: last stage per window) and the `com.apple.trackpad.forceClick`
  UserDefaults read (read live on each stage-2 transition — force-clicks
  are rare, `NSUserDefaults` caches internally, and Ghostty reads it per
  pressure event, so live System Settings changes take effect without a
  restart).
- On a qualifying force-click: map the event's physical position via
  `mouse.rs:150` (`physical_position_to_grid_point`) → call the new
  `word_at_viewport_point` → on `Some`, compute the AppKit point (scale
  and y-flip) → resolve an `NSFont` from the configured font family/size
  (best-effort) → call `show_definition`.

### Untouched crates

noa-vt, noa-render, noa-font (consulted read-only for the font-lookup
attempt, not modified), noa-pty. Neither feature touches VT parsing
beyond the existing OSC 7 path, nor the render pipeline.

## L3 — Acceptance Criteria

- **AC-PXI-1** (REQ-PXI-1) [unit] — Given a config file with
  `macos-titlebar-proxy-icon = hidden`, When parsed, Then the resolved
  config value is `hidden`; given no key present, the resolved value is
  `visible`.
- **AC-PXI-2** (REQ-PXI-2) [manual-visual] — Given config `visible` and a
  focused pane whose shell just emitted `OSC 7` for `/Users/x/project`,
  When the window is inspected, Then the titlebar shows the proxy icon and
  Cmd-clicking it shows the path chain up to `/Users/x/project`.
- **AC-PXI-3** (REQ-PXI-2) [manual-visual] — Given config `hidden`, When a
  pane's cwd changes, Then no proxy icon appears in the titlebar.
- **AC-PXI-4** (REQ-PXI-3) [manual-visual] — Given a split with pane A at
  `/a` and pane B at `/b`, When focus moves from A to B, Then the proxy
  icon updates to `/b` without a new OSC 7 sequence from B.
- **AC-PXI-5** (REQ-PXI-4) [unit] — Given the diff-cache helper with the
  same resolved cwd across two consecutive frames, When evaluated, Then
  the native setter call is skipped on the second frame; given a changed
  cwd, the setter call happens.
- **AC-PXI-6** (REQ-PXI-5) [unit] — Given `Terminal.cwd = Some("/does/not/exist")`,
  When the proxy-icon resolution helper runs, Then it still resolves to
  that path (no existence check).
- **AC-QLK-1** (REQ-QLK-1) [manual-visual] — Given `forceClick` is enabled
  in system preferences, When the user force-clicks a word, Then the
  definition popup appears exactly once per press (not once per pressure
  sample within the same press).
- **AC-QLK-2** (REQ-QLK-1) [manual-visual] — Given `forceClick` is
  disabled in system preferences, When the user presses hard on a word,
  Then no popup appears.
- **AC-QLK-3** (REQ-QLK-2) [unit] — Given a `Terminal` with an active
  selection, When `word_at_viewport_point` is called at any point, Then
  the selection afterward is byte-for-byte identical to before the call.
- **AC-QLK-4** (REQ-QLK-2, REQ-QLK-3) [unit] — Given a grid row containing
  `"hello world"`, When `word_at_viewport_point` is called at a point
  inside `"world"`, Then it returns `("world", <start point of 'w'>)`.
- **AC-QLK-5** (REQ-QLK-5) [unit] — Given a viewport point over blank
  cells, When `word_at_viewport_point` is called, Then it returns `None`.
- **AC-QLK-6** (REQ-QLK-4) [unit] — Given a physical point and a known
  `scale_factor`/view height, When the AppKit-coordinate conversion helper
  runs, Then the result matches `(physical.x / scale, view_height -
  physical.y / scale)`.
- **AC-QLK-7** (REQ-QLK-4) [manual-visual] — Given a force-click on a word
  near the bottom of the terminal view, When the popup appears, Then it is
  anchored at that word's on-screen position (not inverted/offset).
- **AC-QLK-8** (REQ-QLK-6) [unit] — Given a font family name that fails
  `fontWithName:size:` lookup, When the font-resolution helper runs, Then
  it returns `None` without panicking, and the caller still invokes
  `show_definition` (without a font attribute).
- **AC-OSC-1** (REQ-OSC-1) [unit] — Given `OSC 7 ; ST` (empty value), When
  parsed, Then the result is a pwd-reset action (`Terminal.cwd` becomes
  `None`), not `Malformed`.
- **AC-OSC-2** (REQ-OSC-2) [unit] — Given `OSC 7 ;
  file://evil-remote-host/tmp ST` where `evil-remote-host` is not the
  local hostname, When parsed, Then the pwd update is ignored and
  `Terminal.cwd` is unchanged from its prior value.
- **AC-OSC-3** (REQ-OSC-2) [unit] — Given `OSC 7 ; file:///Users/x ST`
  (empty host) and `OSC 7 ; file://localhost/Users/x ST`, When parsed,
  Then both set `Terminal.cwd = Some("/Users/x")`.
- **AC-OSC-4** (REQ-OSC-3) [unit] — Given `OSC 7 ;
  kitty-shell-cwd:///Users/x ST`, When parsed, Then `Terminal.cwd =
  Some("/Users/x")`.
- **AC-NF-1** (REQ-NF-3) [integration] — Given the feature landed, When
  `cargo test --workspace` and `cargo clippy --workspace` run, Then both
  are green and pre-existing OSC 7 tests pass (except the empty-value case
  intentionally updated per AC-OSC-1).

### Manual verification (GUI, cannot be driven headlessly)

- Proxy icon appears when a shell `cd`s and emits OSC 7; updates again on
  subsequent `cd`s (covers AC-PXI-2 end-to-end in a live app).
- Cmd-click on the proxy icon shows the Finder path-chain menu.
- Dragging the proxy icon to a Finder window/the desktop drags the
  directory (standard `representedURL` behavior, no custom code needed —
  verify it isn't accidentally suppressed).
- Setting `macos-titlebar-proxy-icon = hidden` in config and relaunching
  hides the icon entirely.
- Force-clicking a word with system `forceClick` enabled shows the
  dictionary popup at the correct on-screen location; force-clicking empty
  space or with `forceClick` disabled shows nothing.

## Open Questions / Deferred Decisions

- pwd → title fallback: deferred, see Out of scope
  (`#TODO(agent): evaluate a pwd-as-title fallback in a follow-up spec
  once tab-title precedence can be revisited`).
- Whether `word_at_viewport_point` lives on `Terminal` or `Screen`: an
  implementer choice; either satisfies REQ-QLK-2 as long as it is public,
  read-only, and reachable from `noa-app` without a lock-order hazard.

## Traceability

| Requirement | Planned implementation site | Test (L3) |
|---|---|---|
| REQ-PXI-1 | noa-config 6-site key wiring (`lib.rs`, `parser/overrides.rs`, `parser/values.rs`, `cli.rs`) | AC-PXI-1 |
| REQ-PXI-2 | `app/render.rs` diff-apply + `macos_window.rs::set_represented_url` | AC-PXI-2, AC-PXI-3 |
| REQ-PXI-3 | focused-pane cwd resolution at render time (mirrors `render.rs:99-115`) | AC-PXI-4 |
| REQ-PXI-4 | per-window diff cache before calling `set_represented_url` | AC-PXI-5 |
| REQ-PXI-5 | no-op path resolution (no `Path::exists` check) | AC-PXI-6 |
| REQ-PXI-6 | documentation only (no dedicated code path) | manual verification |
| REQ-QLK-1 | `app/event_loop.rs:296-306` `TouchpadPressure` stage tracking + UserDefaults gate | AC-QLK-1, AC-QLK-2 |
| REQ-QLK-2 | new `word_at_viewport_point` on `noa-grid` `Terminal`/`Screen` | AC-QLK-3, AC-QLK-4, AC-QLK-5 |
| REQ-QLK-3 | `mouse.rs:150` mapping + call into `word_at_viewport_point` | AC-QLK-4 |
| REQ-QLK-4 | coordinate-conversion helper + `macos_window.rs::show_definition` | AC-QLK-6, AC-QLK-7 |
| REQ-QLK-5 | early-return on `None` word in the force-click handler | AC-QLK-5 |
| REQ-QLK-6 | font-resolution helper (`fontWithName:size:`) with graceful `None` fallback | AC-QLK-8 |
| REQ-OSC-1 | `crates/noa-grid/src/osc.rs:437` empty-value branch | AC-OSC-1 |
| REQ-OSC-2 | `crates/noa-grid/src/osc.rs:437` hostname validation | AC-OSC-2, AC-OSC-3 |
| REQ-OSC-3 | `crates/noa-grid/src/osc.rs:437` `kitty-shell-cwd://` scheme acceptance | AC-OSC-4 |
| REQ-NF-1 | unit test asserting selection unchanged around `word_at_viewport_point` | AC-QLK-3 |
| REQ-NF-2 | `crates/noa-grid/src/tests/osc.rs` new edge-case tests | AC-OSC-1, AC-OSC-2, AC-OSC-3, AC-OSC-4 |
| REQ-NF-3 | regression gate | AC-NF-1 |
| REQ-NF-4 | 6-site pattern reuse, no new precedence code | AC-PXI-1 |
