# Spec: Theme Live Preview + Settings UI (theme-settings-ui)

## Metadata

- **slug:** theme-settings-ui
- **title:** Theme Live Preview + Settings UI (Theme Live Preview + Settings UI)
- **status:** **locked** (sign-off 2026-07-06)
- **owner:** simota
- **build-path:** **apex (single continuous run)** — see "Build-path decision" at the end for details
- **recipe:** /nexus spec — FRAME ✓ / EXPAND ✓ / CHALLENGE ✓ / SHAPE ✓ / SPECIFY ✓ / Quality Gate PASS (re-inspected by Judge) / LOCK ✓
- **upstream:** `theme-selection.md` (locked — theme catalog, applied at startup), `ghostty-config.md` (locked — config format and writer approach to conform to)

## L0 — Vision

- **Problem:** noa has 574 Ghostty-compatible themes (`noa-theme`) and a config file foundation (`noa-config`, `~/.config/noa/config`), but the only way to change settings is "edit the file in an external editor → restart." The theme is resolved once at `GpuState` initialization, and there is no path to switch it at runtime.
- **Value delivered:** Introduce a general-purpose in-app settings UI (theme, font, transparency, etc.). The theme picker offers a live preview with a list + sample pane + immediate reflection onto the actual screen. On commit, it writes back to `~/.config/noa/config`.
- **Target:** The noa user themselves (single-user, local app).
- **Definition of success:** The user can browse, try, and commit a theme without restarting, and the committed value remains in effect on the next launch (config write-back).

## FRAME — Reusable assets and constraints (Lens scan finalized)

### Reusable assets
- `noa-theme`: compile-time catalog of 574 Ghostty-compatible themes (`ThemeDef`, binary-search `resolve(name)`)
- `noa-config`: `~/.config/noa/config` key=value parser, already supports the `theme` key, CLI > file > default merge, one-shot Ghostty config import
- `noa-app/src/theme.rs`: `resolve_theme_with_overrides(name, &ThemeOverrides)` → `noa_render::Theme`
- `noa-render/src/theme.rs`: `OverlayStyle::from_theme()` — derives the modal UI palette (computed on demand, so it automatically follows theme switches)
- OSC 4/10/11 dynamic color path: `TerminalColors` → `Theme::resolve_with_colors` resolved every frame — a candidate to piggyback the live preview on
- UI container precedents: command_palette (fuzzy list card) / tab_overview, overview (independent window-style overlay)

### Technical constraints
1. `gpu.theme` is assigned exactly once at startup (`noa-app/src/app.rs:922-942`) — a new update path is required
2. `chrome::ACTIVE_PALETTE` is a single-write `OnceLock` (`noa-app/src/chrome.rs:129-147`) — must be made mutable
3. The sidebar/palette GPU texture set has theme colors baked in (`app.rs:943-954`) — must be rebuilt on theme switch
4. There is no config file watcher — "live preview (in-session)" and "persistence (config write)" must be designed as separate mechanisms
5. The 574-theme catalog is `&'static` — sufficient for the picker list, but user-defined themes need a separate path (out of scope this time)
6. `minimum_contrast`, search highlighting, and `OverlayStyle` are all derived from the theme — keep them in sync via a single bulk re-resolve; individual patching is prohibited

## Findings confirmed during hearing (FRAME)

- Scope: **general-purpose settings UI** (theme-centric, but also lists/changes major settings such as font and transparency)
- Theme source: Ghostty-bundled compatible set (= use the existing `noa-theme` 574-theme catalog as-is)
- Preview UX: **both** — list + sample pane + immediate reflection of the selected theme onto the real screen (Esc to revert / Enter to commit)
- Persistence: **write-back to the config file** (update the relevant key in `~/.config/noa/config`, loaded on startup)

## Out-of-scope (finalized in FRAME)

- Config reload keybinding (equivalent of Ghostty's cmd+shift+,)
- `theme = light:X,dark:Y` auto-switch syntax
- User-defined theme files (`~/.config/noa/themes/`)
- Config file watching (auto-reflecting external edits)

## EXPAND — Candidate directions (completed, with user feedback)

- **Candidate 1: OSC-override-piggybacked preview** ← chosen by the user (primary direction). An in-window overlay (command-palette style). The preview injects the full palette into the existing OSC 4/10/11 dynamic color path (body text is live, chrome only updates on commit). Settings items form a second section within the same overlay.
- Candidate 2: independent settings window with a separate preview (overview precedent — a dedicated mini-terminal) — rejected
- Candidate 3: fully live swap of the actual session (rebuilding textures on every highlight change) — rejected
- Candidate 4: A/B roulette comparison (side-by-side pair comparison) — rejected
- Flux's proposal of "a curated top-20 list" and "persistence as a side effect (write immediately on Enter)" — both rejected. Go with a **flat list of all 574 + an explicit save action**.

## CHALLENGE — Selection (complete, ruled on by the user)

**Finalized direction: Candidate 1, revised — in-window overlay + `preview_theme` swap-in preview + Enter-to-commit write**

User ruling (2026-07-06):
1. **Sample pane: keep it** (Magi's ruling adopted) — fixed swatches for the 16 ANSI colors + truecolor. All colors can be checked even on a blank screen
2. **v1 settings scope: somewhat broad** — 4 live-apply rows (font-size / background-opacity / background-blur-radius / cursor-style; opacity and blur are conceptually paired) plus additional rows that apply only on commit (font-family, window-padding, etc.) are also listed
3. **Save: Enter-to-commit is a single gesture that applies + writes to config**, Esc discards everything. No dedicated Save button
4. **Preview mechanism: `preview_theme: Option<Theme>` is referenced in place of `gpu.theme` at draw time** (TerminalColors injection rejected) — previews everything down to selection color, search color, and OverlayStyle, with no contamination of terminal state

Magi's ruling (adopted):
- While previewing, the overlay shows a "chrome/tabs update on Save" badge (no GPU work such as chrome dimming)
- Deferring the chrome update to a restart was rejected — commit performs a one-shot full swap (OnceLock → made mutable + resetting roughly 12 Option textures to None → auto-rebuild via lazy-init)
- The line between live and commit-only is drawn on a mechanical basis of "can it be resolved every frame": opacity/cursor are immediate, font-size is debounced (~150ms), font-family is commit-only

Ripple feasibility check (key points):
- `preview_theme` swap-in: since `Theme` is referenced every frame via `resolve_with_colors`, swapping it in is low risk. Only baked-in chrome is out of scope
- Blast radius of the commit swap: ~3 files (`chrome.rs` / `app.rs` / `app/state.rs`), MEDIUM
- **Config write is new work**: `noa-config` is currently parse-only. Per the locked spec `ghostty-config.md`, it must follow Ghostty's line-oriented `key = value` format (TOML has been dropped). A surgical update that preserves comments and unknown keys is required
- Live application of font-size can reuse the existing `runtime_font_size` path (`app/input_ops.rs:30-70`)
- **The opaque→transparent transition for background-opacity is not possible** (fixed at winit window creation) — needs a decision in SPECIFY
- The priority relationship between CLI flags (--font-size, etc.) and the written value needs a product decision — needs a decision in SPECIFY

### Considered but rejected
- Candidate 2, independent settings window with separate preview — fails to satisfy "immediate reflection onto the real screen"; would create two separate swap paths
- Candidate 3, fully live swap on every highlight — the scrub-speed performance of texture rebuilds is unverified and the risk hits the live session directly
- Candidate 4, A/B pair comparison — high rendering cost, does not generalize to general settings
- TerminalColors injection preview — does not cover selection color or search color, and requires care around conflicts with program OSC state (`preview_theme` swap-in is superior)
- Showing a curated top-~20 theme list by default — rejected by the user (flat list of 574 + fuzzy search)
- Persistence as a side effect (kitty-style) — rejected by the user (explicit Enter-to-commit)
- Deferring the chrome update to a restart — rejected by Magi (a half-applied state right after Save looks like a bug)
- Related: the existing locked spec `theme-selection.md` (the catalog and startup application are an already-implemented foundation). This spec takes over "browsing/trying on (JTBD-3, v1 DEFER)," which was out of scope for that spec

## SHAPE — Proposal (approved 2026-07-06, all recommended defaults adopted)

### Proposed solution
- **Launch trigger:** a new command palette entry "Open Theme & Settings." The existing ⌘, (external editor for config) is left unchanged and coexists.
- **Layout:** an in-window overlay in command_palette style. Section 1 = theme picker (left: flat list of 574 with fuzzy search / right: fixed sample pane for 16 ANSI + truecolor). Section 2 = settings rows (below, in the same overlay, scrollable).
- **Key operations:** ↑↓ to select a row, typing to type-filter, Tab to switch focus between the theme/settings sections, numeric rows use ←→ or direct input. Enter to commit, Esc to discard all preview state and revert.
- **Preview mechanism:** `preview_theme: Option<Theme>` is referenced in place of `gpu.theme` at draw time. Covers body text, selection color, search color, and OverlayStyle. Settings rows branch into immediate/debounced/commit-only based on whether they can be resolved every frame. While previewing, a "chrome/tabs update on Save" badge is shown.
- **Enter-to-commit sequence:** ① full chrome swap (`ACTIVE_PALETTE` made mutable and swapped to the new theme value; roughly 12 baked-in textures reset to None → rebuilt via lazy-init) ② surgical config write (ghostty-config format, replacing only the changed key lines, preserving comments, unknown keys, and other lines). Both run as a single gesture, never leaving a half-applied state.

### Settings rows (v1, 7 rows total — finalized)
| Key | Widget | Live | Notes |
|---|---|---|---|
| font-size | inline numeric (←→ / direct input) | live, debounce ~150ms | Applied via the existing `runtime_font_size` path (app/input_ops.rs:30-70). The debounce itself is a new small timer state machine (pure logic decoupled from GPU calls) |
| background-opacity | numeric (0.0–1.0) | live, immediate (only when launched transparent) | paired with blur-radius |
| background-blur-radius | numeric | live, immediate (same condition) | disabled display when opacity=1.0 |
| cursor-style | cycle row (block/bar/underline) | live, immediate | reuses the existing cursor mode switch |
| font-family | cycle row (no fuzzy — revised 2026-07-06) | commit-only (persisted only, applied on next launch) | rebuilding the font is expensive |
| window-padding-x/y | numeric (2 keys in 1 row, both axes step together — revised 2026-07-06) | commit-only (same as above) | involves grid recalculation |
| macos-titlebar-style | cycle row | commit-only (same as above) | involves rebuilding the window chrome |

### Finalized decisions (formerly Open Questions, all recommended options adopted)
1. CLI flags are session-only overrides. When the settings UI commits a config write, it does not overwrite it with the CLI value (preserves the existing precedence model)
2. When launched opaque (opacity=1.0): editing/writing the opacity/blur rows is allowed, but no preview — show a "takes effect after restart" note
3. Commit propagates immediately to all windows (a natural consequence of single-process state; feasibility to be verified in SPECIFY)
4. The sample pane is laid out as list on the left, sample on the right
5. Settings rows are fixed at 7 for v1; additional requests go into a separate increment
6. Surgical write follows the same approach as the ghostty-config import writer (`build_import_output`) — "preserve original line text, replace only the target key's line" — no new parser mechanism

### Assumptions
- The overlay opens on a single window. Commit swap propagates automatically to other windows because it is single-process state
- The font-family list uses the existing font-kit discovery as-is, with the same fuzzy-search UX as the theme list
- The existing constraint that runtime opaque→transparent transition is not possible (fixed at winit window creation) is unchanged

## L1 — Requirements

### Overlay launch and composition
- **R-1**: Add a new command palette entry "Open Theme & Settings" that opens a single in-window overlay. Since selecting a palette entry synchronously closes the palette before dispatching the command (existing palette behavior), this does not conflict with the mutual exclusion in R-3 or the launch path. The existing ⌘, (launch external editor) is left unchanged and coexists.
- **R-2**: The overlay is in command_palette style (not an independent window). Section 1 = theme picker (left: flat list of 574 + fuzzy search / right: sample pane), Section 2 = settings rows (scrollable). Tab switches focus between the two sections. The key-operation model is unified across all rows: ↑↓ = select row (not used for value adjustment), ←→ = adjust the value of the focused row (step increment/decrement for numeric rows, cycle for cycle rows); numeric rows also accept direct input.
- **R-3**: The theme & settings overlay cannot be started while another overlay (command palette, search) is open (mutual exclusion). Conversely, while this overlay is open, launch shortcuts for other overlays are ignored.

### Theme picker / preview
- **R-4**: Display the flat list of 574 entries, with real-time fuzzy filtering as the user types (no curated display).
- **R-5**: The sample pane always shows fixed swatches for the 16 ANSI colors + truecolor (all colors can be checked even on a blank screen).
- **R-6**: Reflect list highlight changes into `preview_theme: Option<Theme>`, so that body text, selection color, search color, and `OverlayStyle` all switch by the next frame. The terminal's actual state (`TerminalColors`) remains uncontaminated.
- **R-7**: While previewing, show a "chrome/tabs update on Save" badge within the overlay (the chrome's own appearance does not change).

### Settings rows (v1, 7 rows fixed)
- **R-8**: The 4 rows font-size / background-opacity / background-blur-radius / cursor-style are subject to live preview. The 3 rows font-family / window-padding-x,y / macos-titlebar-style **only persist to config on commit, taking effect on the next launch** (revised 2026-07-06, user ruling — because no runtime application path exists). When any of these 3 rows is touched, show the same "takes effect after restart" note as in R-11 on that row. Each row's classification is fixed per widget and cannot be switched at runtime.
- **R-9**: The font-size row applies via the existing `runtime_font_size` path (`noa-app/src/app/input_ops.rs:30-70`) after a ~150ms debounce. The debounce is implemented as a new small timer state machine (no debounce mechanism currently exists in the codebase), as pure logic decoupled from GPU calls, unit-testable.
- **R-10**: background-opacity / background-blur-radius apply immediately (except when launched opaque). cursor-style applies immediately.
- **R-11**: When launched opaque (opacity=1.0), the opacity/blur rows can still be edited and written on commit, but are not previewed; show a "takes effect after restart" note on the row (the existing constraint that opaque→transparent runtime transition is impossible remains unchanged).

### Commit sequence
- **R-12**: Enter executes, as a single gesture, "config write" → "full chrome swap," in that order. The write (the only step that can fail) comes first: if the write fails, the chrome swap is skipped, the preview state is retained, and an error is shown within the overlay. Because the chrome swap after a successful write is an in-memory-only operation that effectively cannot fail, a "half-applied" intermediate state cannot structurally occur.
- **R-13**: The chrome swap makes `chrome::ACTIVE_PALETTE` mutable and swaps in the new theme value, resets the baked-in GPU texture set (`app.rs:943-954`, roughly 12 of them) to `None`, and lets lazy-init rebuild them on the next draw.
- **R-14**: The config write replaces, in `~/.config/noa/config` (ghostty-config format), only the text of lines that changed, preserving comments, unknown keys, other key lines, and the original line order (surgical update). The write is performed atomically via a temp file + rename.
- **R-15**: If the config file does not exist, create it and write only the committed setting values (since no existing keys exist, all lines become newly appended lines).
- **R-16**: Esc resets `preview_theme` to `None`, discards the draft values of the settings rows, and fully restores the state to what it was immediately before the overlay was opened. No config write occurs.

### Priority / propagation
- **R-17**: Session-only overrides from CLI flags (`--font-size` etc.) are not reflected in the config value written on commit (the written value is always the config value before the overlay operation plus this round's changes only). The existing CLI > file > default model is unchanged.
- **R-18**: On commit, walk all in-process window states and request a redraw (`request_redraw`) for each, so the new theme/settings are reflected on each window's next draw frame (no background window is left with stale chrome).

### Non-functional requirements
- **NFR-1 (preview latency)**: Highlight changes (theme list, cursor-style, opacity/blur) are reflected by the next draw frame. Preview resolution + redraw request processing must complete within a single frame budget (roughly 16ms at 60Hz).
- **NFR-2 (scrub performance)**: While scrubbing through the list highlight or waiting on the font-size debounce, GPU texture rebuilds (regenerating atlas/baked-in textures) must not occur. Texture rebuild happens exactly once, on Enter-to-commit.
- **NFR-3 (write atomicity)**: Config writes use a temp file + rename, so a process termination or crash mid-write does not corrupt the config file.
- **NFR-4 (continuity when config is absent)**: A commit from a state with no config file present does not fail; it creates a new file.
- **NFR-5 (surgicality)**: After the write, lines other than the changed keys (comments, unknown keys, key order) are byte-for-byte identical before and after the write.
- **NFR-6 (CLI non-contamination)**: Committing the overlay in a session with an active CLI override does not let the CLI-derived value leak into the written config value.

## L2 — Detail

### noa-app (main changes)
- A new module (e.g. `noa-app/src/theme_settings.rs`) holds the overlay state: `preview_theme: Option<Theme>`, draft values for the settings rows (font_size/opacity/blur/cursor_style for immediate application; font_family/padding/titlebar as commit-only draft holders).
- Draw path: replace direct references to `GpuState.theme` (currently assigned once at startup in `app.rs:922-942`) with a resolver function that goes through `preview_theme.as_ref().unwrap_or(&gpu.theme)`. The `Theme::resolve_with_colors` call (composited with OSC dynamic colors, run every frame) just receives the resolved `Theme` and needs no change.
- Change `chrome::ACTIVE_PALETTE` (`chrome.rs:129-147`, currently a single-write `OnceLock`) to a mutable holder (`RwLock`/`Mutex`, etc.) so it can be swapped on commit. The swap logic is unit-testable in `chrome.rs` alone, without needing `GpuState` (AC-9).
- Consolidate the roughly 12 baked-in textures (the `Option` field group in `app.rs:943-954`) into a resettable substructure (e.g. `ChromeTextures`) with a `reset()` method. Since resetting `Option` fields to `None` is a pure operation, this structure is unit-testable without a GPU device (AC-20). The existing draw path continues to handle lazy-init rebuilding.
- Give the substructure a debug-build-only rebuild counter (e.g. `AtomicUsize`) so the number of lazy-init rebuilds can be measured (the verification mechanism for NFR-2/AC-18; confirmed at the Gate that no such measurement hook exists in the current code, so this is a deliverable of the implementation).
- Commit ordering (R-12): (1) config write via the new `noa-config` writer — on failure, abort here and show an error, (2) after a successful write, swap `ACTIVE_PALETTE` + `ChromeTextures::reset()` + update `gpu.theme`, (3) walk all windows and call `request_redraw` (R-18). Executed synchronously within a single function.
- The font-size debounce is implemented as a new small timer state machine (input: a timestamped sequence of value changes → output: the final value that should fire) in a module decoupled from GPU calls, calling the existing `runtime_font_size` path (`app/input_ops.rs:30-70`) when it fires.
- No changes to where each tab/window's `Terminal` is created (`preview_theme` is not injected into the grid's `TerminalColors` — per ruling 4).

### noa-config (new: writer)
- New module `src/writer.rs` (tentative name). Reuses the ghostty-config-era `parser.rs` (`parse_directives` → line-numbered `Directive`), and follows the same "pure function (string processing) + thin I/O wrapper" structure as `build_import_output` (`import.rs`), but works in the reverse direction: it reads the existing config's raw text and, for the key(s) being changed, replaces only the line at the corresponding `Directive.line` with a new `key = value` line, while preserving every other line (comments, unknown keys, list-type key lines included) exactly as in the original text. If a target key does not exist in the original text, it is appended as a new line at the end.
- The I/O layer guarantees atomicity via a temp-file write + `rename` (NFR-3). The write target is `default_config_path()` (as defined in the ghostty-config spec, `<config_dir>/noa/config`).
- If the config file itself does not exist, feed an empty string through the same replacement logic as input (all lines become new appends, NFR-4).
- CLI-derived values are never included in this writer's input (the caller, `noa-app`, passes only the overlay's draft values, R-17/NFR-6).

### noa-render
- No changes. `OverlayStyle::from_theme()` is computed on demand, so it automatically follows whenever `noa-app` passes in a swapped `Theme` (pure reuse of an existing asset).

### Edge cases
- **Fuzzy search yields zero matches**: show an empty list, and keep the sample pane showing the previously highlighted theme (or the active theme at the moment the overlay was opened). Do not reset `preview_theme` to `None` on its own.
- **Opening the overlay while running with a theme name not present in config**: the existing startup fallback (default theme + warn, `theme-selection.md` R-3) has already resolved an active theme, and that is what is shown. The overlay does not reproduce the invalid name.
- **Opening while a CLI override is in effect on top of config**: the picker/settings rows' initial selection shows the session's active value (including CLI-derived ones), but the config write on commit does not write the CLI-derived value (R-17).
- **Esc during the font-size debounce**: discard the debounce timer (any unfired value change is dropped), and if a change has already fired, restore `runtime_font_size` to the value it had when the overlay was opened.
- **A launch request while another overlay is open**: per R-3, the launch request itself is simply ignored (no error shown, silently ignored).

## L3 — Acceptance Criteria

Legend for verification method: [unit] = GPU-free unit test / [integration] = integration test using e.g. a tempdir / [code-review] = implementation inspection / [GUI visual check] = manual verification.

### Launch / layout
- **AC-21 (R-1)** [unit]: Given selecting "Open Theme & Settings" in the command palette. When inspecting state after the command dispatch. Then the palette is closed and the theme & settings overlay is open.
- **AC-22 (R-2)** [unit]: Given the overlay is open. When Tab is pressed. Then the focused section toggles between theme picker ⇄ settings rows. Also, while settings rows are focused, ↑↓ moves the row selection and ←→ changes the value of the focused row (a state-machine test of key routing).
- **AC-17 (R-3)** [unit]: Given the command palette is open. When requesting the theme & settings overlay to open. Then nothing happens (the overlay does not open).

### Preview
- **AC-1 (R-6)** [unit]: Given setting `preview_theme` to a different theme. When passing the resolver function's output (equivalent to `preview_theme.as_ref().unwrap_or(&gpu.theme)`) through `resolve_with_colors` and inspecting it. Then all four of (a) default body fg/bg (b) selection color (c) search highlight color (d) `OverlayStyle::from_theme` output match the new theme's values (no draw loop needed, resolver called directly).
- **AC-2 (R-6)** [unit]: Given `preview_theme` is `Some`. When inspecting `TerminalColors`' internal state. Then it is unchanged from before the preview (no contamination).
- **AC-3 (R-5)** [unit]: Verify at the data-structure level that the sample pane's display data includes all 16 ANSI colors plus the truecolor sample. Actual glyph rendering is supplemented (optionally) by a GUI visual spot check before/after commit.
- **AC-4 (R-7)**: (a) [unit] The badge display flag is set when `preview_theme.is_some()` and clears when `None`. (b) [GUI visual check] The chrome's own appearance does not change during preview, until commit.

### Settings rows
- **AC-5 (R-8, R-10)** [unit]: Given changing the value of the cursor-style row. When inspecting overlay state. Then the cursor draw mode (enum) has switched to the new value immediately.
- **AC-6 (R-9)** [unit]: Given feeding a timestamped sequence of value changes (intervals < 150ms) into the debounce timer state machine. When simulating 150ms elapsing. Then it fires exactly once, outputting the last value (a test of pure logic decoupled from GPU calls).
- **AC-7 (R-11)**: (a) [unit] When editing the opacity/blur rows while launched with opacity=1.0, the preview-apply flag does not get set and the "takes effect after restart" note flag is set. (b) [GUI visual check] No preview occurs on the real screen. The write itself is verified by the AC-11 series.

### Commit / Esc
- **AC-8 (R-16)** [unit]: Given `preview_theme=Some` and settings-row drafts have been changed. When Esc is pressed. Then `preview_theme` reverts to `None`, the draft values are discarded, and the injected mock writer's config write call count is 0.
- **AC-9 (R-13)** [unit]: In `chrome.rs` alone, swap the made-mutable `ACTIVE_PALETTE` to a new palette, and reads return the new value (no `GpuState` needed).
- **AC-20 (R-13)** [unit]: Calling `ChromeTextures::reset()` sets all `Option` fields to `None` (a pure operation, no GPU device needed).
- **AC-23 (R-12)** [unit]: Given an injected mock writer that returns a write failure. When executing Enter-to-commit. Then the chrome swap and `gpu.theme` update do not run, `preview_theme` is retained, and the error-display flag is set.
- **AC-10 (R-12)** [code-review]: Confirm by implementation inspection that the commit function synchronously executes config write success → chrome swap → redraw request within a single function, with no structure that allows a frame draw to interleave partway through (plus an optional GUI visual spot check).

### Config write
- **AC-11 (R-14, NFR-5)** [unit]: Given config text containing comments, unknown keys, and multiple existing keys. When running it through the writer changing only one key. Then the output is byte-for-byte identical to the input except for the changed key's line (a pure-function round-trip test, of the same shape as the `build_import_output` test suite).
- **AC-12 (R-15, NFR-4)** [integration]: Given no config file exists in a tempdir. When changing one setting and committing. Then a new file is created, the changed value is written, and the process does not fail.
- **AC-13 (NFR-3)** [code-review]: Confirm the design writes to a temp file and then replaces it via `rename`, such that an in-progress write state is never observable externally (precedent: `noa-app/src/session.rs:347-354`).
- **AC-14 (R-17, NFR-6)** [integration]: Given a state equivalent to a session with the `--font-size` CLI flag active, committing a different value via the settings row. When inspecting the output config. Then it contains the value committed via the settings row, not the CLI-derived value.

### Propagation / edge cases
- **AC-24 (R-18)** [unit]: Given multiple (mocked) window states. When executing commit. Then a redraw request has been issued for every window (verified via call records).
- **AC-15 (R-18)** [GUI visual check]: With multiple windows open, commit from one window's overlay and do a final check that every window's chrome/body switches to the new theme and settings.
- **AC-16 (R-4)** [unit]: Given entering a fuzzy search string with zero matches. When checking the list. Then the list is empty but `preview_theme` retains its previous value.

### Performance
- **AC-18 (NFR-2)** [unit]: Given a `ChromeTextures` with the debug rebuild counter (deliverable specified in L2). When simulating 10+ consecutive highlight changes in the theme list. Then the counter stays at 0, and only after Enter-to-commit does the next draw perform the bulk rebuild for the first time (the counter increments by exactly one commit's worth).
- **AC-19 (NFR-1)**: (a) [unit] A timed test confirming preview resolution + redraw request processing completes within a single frame budget (roughly 16ms @ 60Hz). (b) [GUI visual check] Confirming (as a supplement) that there is no perceptible lag.

### Traceability

| Requirement | AC |
|---|---|
| R-1 | AC-21 |
| R-2 | AC-22 |
| R-3 | AC-17 |
| R-4 | AC-16 |
| R-5 | AC-3 |
| R-6 | AC-1, AC-2 |
| R-7 | AC-4 |
| R-8 | AC-5 |
| R-9 | AC-6 |
| R-10 | AC-5 |
| R-11 | AC-7 |
| R-12 | AC-10, AC-23 |
| R-13 | AC-9, AC-20 |
| R-14 | AC-11 |
| R-15 | AC-12 |
| R-16 | AC-8 |
| R-17 | AC-14 |
| R-18 | AC-15, AC-24 |
| NFR-1 | AC-19 |
| NFR-2 | AC-18 |
| NFR-3 | AC-13 |
| NFR-4 | AC-12 |
| NFR-5 | AC-11 |
| NFR-6 | AC-14 |

All 18 R + 6 NFR have ≥1 AC each, 24 ACs total (AC-1 through 24). [GUI visual check] is the primary verification method only for AC-15; supplementary visual checks appear in AC-3/4b/7b/10/19b.

## Revision history

- **2026-07-06 (revised during implementation, signed off by the user)**: (1) R-8 — revised the 3 commit-only rows from "applied on commit" to "persisted only, applied on next launch + restart note shown" (because the runtime-application path is not yet implemented). (2) Removed the fuzzy sub-filter for the font-family row (simple cycle instead). (3) Simplified window-padding x/y independent editing to a single combined-axis step. The original specs for (2) and (3) are recorded in Open Questions as candidates for a future increment.

## Open Questions / Deferred Decisions

- **Future increment candidates (simplified during 2026-07-06 implementation)**: fuzzy-search sub-filter for the font-family row / independent in-row editing of window-padding x/y / runtime application of the 3 commit-only rows (font-family = FontGrid rebuild, padding = compositing resize, titlebar = AppKit call).

- **Ghostty fidelity positioning**: this feature is a noa-specific extension not present in Ghostty, and is an explicit exception to the fidelity principle in `theme-selection.md` L0 ("GUI theme editors and other features not present in Ghostty are out of scope"). The UI surface is not included in the Parity Map (the set of Ghostty observable behaviors to replicate).
- **CI robustness of the AC-19(a) timing test**: 16ms@60Hz is a local-execution baseline. If it is flaky in the CI environment, relaxing the threshold or allowing a skip is acceptable (implementation-time judgment call).
- **Future increment candidates (confirmed out-of-scope)**: config reload keybinding (equivalent of cmd+shift+,) / `theme = light:X,dark:Y` auto-switch / user-defined themes (`~/.config/noa/themes/`) / config file watching. Additional requests for settings rows are also reviewed as separate increments (per finalized ruling 5).

## Build-path decision

**apex (single continuous run)** — chosen at sign-off (2026-07-06). This spec is fed as input to `/nexus apex`, running design → risk gate → implementation loop → L3 AC verification → shipping in a single supervised run. L3's AC-1 through 24 (with the traceability table) form the verification contract. Fallback: `/nexus feature` (supervised single build).
