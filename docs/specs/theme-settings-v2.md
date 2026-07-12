# Spec: Theme Settings Panel v2 — Richness + Optimization Increment (theme-settings-v2)

## Metadata

- **slug:** theme-settings-v2
- **title:** Theme Settings Panel v2 — Richness + Optimization Increment (theme-settings-v2)
- **status:** draft (magi 3-0 unanimous — the "B−" package is the locked scope. Sign-off not yet performed)
- **owner:** simota
- **scope:** **Standard** (traceability ≥85% target). Rationale: the number of new requirement clusters is 16 (R-19–R-34), which reaches the rule-of-thumb threshold for Full scope (12+), but the magi ruling has locked this as a single monolithic unit of work — "performance remediation for one feature + richness increment" — so it does not involve the kind of multi-team coordination that would warrant splitting L2 by team (Biz/Dev/Design). While the Standard structure (involved L2, primary L3) is adopted, per the upstream task instruction, L3 covers all Rs/NFRs with ≥1 AC each (no thinning down to "main scenarios only").
- **upstream:** `theme-settings-ui.md` (locked, v1 — assumes the terminology and R-1–R-18/NFR-1–NFR-6/AC-1–24 defined there). This spec describes only the increment on top of it and does not restate that content.
- **magi ruling:** 2026-07-11, 3-0 unanimous, package "B−" (performance remediation + all of richness A + low-risk group B, 1 data-safety item, 2 conditional stretch items). This spec formalizes that locked scope bullet list as R-19–R-33/NFR-7–NFR-9/AC-25 onward, and does not transcribe the informal AC numbers from the magi worksheet (AC-P1–P3, AC-1–15, AC-C1/C2) verbatim — the mapping from those to this spec's ID scheme (below) is annotated as the source for each L1 item. Only the two labels AC-C1/AC-C2 are carried over unchanged from the magi ruling document (since they are named explicitly in the scope table).
- **ID scheme deviation (intentional):** Rather than Accord's default ID scheme (`REQ-*`/`CFR-*`/`AC-{FEATURE}-{NNN}`), this spec continues the `R-*`/`NFR-*`/`AC-*` sequential numbering carried over from v1 (R-19–, NFR-7–, AC-25–). Rationale: v1's IDs such as R-9/AC-6/AC-8/AC-23 are already directly cited in doc comments in the implementation code (`state.rs`/`debounce.rs`/`writer.rs`), and we judged that preserving cross-referenceability with the actual codebase takes priority over switching to Accord's new ID scheme (per CLAUDE.md's autonomy rule: for reversible ambiguous decisions, proceed with the default choice documented inline).

## L0 — Vision

- **Problem:** `theme-settings-ui` (v1, signed off 2026-07-06) has been fully implemented and merged to main (as of 2026-07-11; the theme picker and Settings row are split into separate `ThemeSettingsMode::Theme`/`Settings` sessions, both rendered as AppKit-native cards — the v1 "single overlay + Tab switching" design was revised to a split during implementation, and `toggle_section` remains as an empty implementation). However, a post-implementation audit revealed three performance debts: (1) the per-frame redraw path (`render.rs:44-48`) unconditionally `clone()`s the entire `ThemeSettings` state, (2) the native overlay's idempotent sync (`macos_overlay/sync.rs:61-83`) builds the `ThemeSettings` view model **before** the hash comparison every time, incurring construction cost even on frames with no change, and (3) fuzzy search over the 574-theme catalog unconditionally rescans the entire set on every keystroke (no debounce). In addition, on the richness front there are gaps: Cmd+, still opens the external editor as before rather than the GUI panel; the `OpenThemePicker`/`OpenSettings` commands are reachable only from the command palette with no menu bar item or default keybinding; reopening in the reverse mode across Tab is not implemented; and match count, contrast ratio, favorites, light/dark filtering, post-commit undo, and wheel scrolling are all unimplemented. Furthermore, on the data-safety front, an implemented bug has been confirmed where, under a `theme = light:X,dark:Y` pair config, committing a single theme from the panel causes `noa-config`'s surgical writer (`apply_updates`) to fail to recognize the pair syntax and unconditionally overwrite the line with a single theme name, silently losing one side of the appearance configuration.
- **Value delivered:** Performance remediation (F1–F3, non-negotiable core) stabilizes frame time during panel operation; richness (all of magi package B−'s A group plus the low-risk B group) improves discoverability and usability; and the AC-13 data-safety fix structurally makes silent destruction of pair configs impossible.
- **Audience:** Same as v1 — the noa user themself (single-user, local app).
- **Success definition:** The three performance fixes land with zero regressions (existing tests: `theme_settings/tests.rs` 34 cases + `app/input_ops/theme_settings.rs` 6 cases + `noa-config/src/writer.rs` 11 cases, 51 total). Each richness item is implemented without violating R-12's commit-order invariant (config write → chrome swap) or the touched-row boundary. And it is guaranteed by unit/integration tests that a panel commit under a `theme = light:X,dark:Y` environment does not destroy the pair syntax.

## Conservation Constraints (invariants carried over from v1 — must not be broken by this increment)

1. **R-12 ordering**: commit must preserve the order "config write (the only step that can fail)" → "chrome swap". On write failure, the chrome swap must not occur and `preview_theme` must be preserved (`ThemeSettings::commit`, `theme_settings/state.rs:894-910`).
2. **touched boundary**: `SettingsRow.touched` must become true only from an actual edit, and must never change from navigation/redraw (pre-mortem RPN 252 comment at `rows.rs:205-207`). Any new row (new UI state such as favorites) must also honor this boundary.
3. **Preview non-contamination**: `preview_theme` must not be injected into `TerminalColors` (AC-2). New preview extensions (multi-line samples) must reuse this same path, not establish a separate one.
4. **Do not break the existing 51 tests**: `theme_settings/tests.rs` (34), `app/input_ops/theme_settings.rs` (6), `noa-config/src/writer.rs` (11).
5. **Single source of truth for the 3 rendering sync points**: see L2 below. `RowDraft::display_value`/`settings_row_display_value` must not be forked by individual rendering paths.
6. **CLI non-contamination (NFR-6)**: new rows/new features must not let CLI override values leak into config writes.

## FRAME correction — implementation drift from v1

- The v1 SHAPE proposal's "single overlay + Tab switching" was revised during implementation to "mode-specific sessions" (`ThemeSettingsMode::Theme`/`Settings`, with `open_theme_settings(mode)` opening a fresh session each time). `toggle_section` (`state.rs:244`) is an intentional empty implementation, with a doc comment stating "a session's `Section` is fixed for its lifetime by `ThemeSettingsMode`" — this is not a bug. This spec's R-24 (Tab reverse-mode reopen) is designed against this new architecture (it does not simply restore v1's Tab spec).
- The "ChromeTextures rebuild counter" pattern mentioned in v1's L2 (a debug-only `AtomicUsize`) has already been implemented (`gpu.chrome_textures.record_rebuild()` is called inside the draw helper in `app/sidebar/palette.rs`). This spec's NFR-7 measurement reuses this existing pattern.
- The native AppKit card treatment (`macos_overlay/`) is an addition that did not exist in v1, and the F1/F2 performance debt newly arose from this native treatment.

## L1 — Requirements

### Performance Remediation (non-negotiable core)

- **R-19 (F1, source: magi package performance-remediation item 1)**: The construction of `theme_settings_card` in `App::redraw` (`app/render.rs:44-48`) currently duplicates the entire `ThemeSettings` (including the already-scanned `filtered: Vec<ThemeMatch>` over 574 entries) via `session.state.clone()` every frame. Replace this with a rendering-only lightweight snapshot type, freeing the per-frame duplication cost from the full copy of `filtered`.
- **R-20 (F2, source: magi package performance-remediation item 2)**: `macos_overlay::sync_theme_settings` (`macos_overlay/sync.rs:61-83`) currently builds `theme_settings_view_model(state)` unconditionally **before** the hash comparison (line 69), and builds it again if the hash changed (line 80). Compare a lightweight identity key first (derivable without assembling the ViewModel — filter string, highlighted/selected_row index, catalog epoch value, rect, colors hash, etc.) before constructing the ViewModel, so that an idempotent sync (state unchanged from the previous frame) results in zero calls to `theme_settings_view_model`.
- **R-21 (F3, source: magi package performance-remediation item 3)**: `push_text`/`backspace` in the theme picker (`theme_settings/state.rs:384-437`) unconditionally runs `recompute_filtered` → `filter_themes` (a full scan of all 574 entries applying `fuzzy_match` to each) on every keystroke. Reuse the existing `Debouncer<T>` pattern from `debounce.rs` (the same module as F1–F3; `ThemeSettings::font_size_debounce` already uses it) so that (a) fast successive keystrokes are debounced and only the trailing value fires, and (b) when the new filter string is an extension (prefix continuation) of the immediately preceding filter string, rescanning is narrowed to only the previous `filtered` result set. When the filter string shortens (a Backspace breaks the prefix relationship), fall back to a full rescan of all 574 entries.

### Menu / Keybindings

- **R-22 (source: magi package "change Cmd+, to launch the GUI overlay")**: Keep the `AppCommand::Preferences` identifier, `PREFERENCES_MENU_ID`, and the Cmd+, accelerator (`macos_menu.rs:551-558`) unchanged, and only swap out the dispatch body (`app/commands.rs:66`, currently `AppCommand::Preferences => crate::app_actions::open_config_file()`) for `self.open_theme_settings(ThemeSettingsMode::Settings)`. The existing `preferences_menu_item_is_enabled_and_routes_to_preferences` test (`macos_menu.rs:729`) only verifies the menu item's identity/routing and does not inspect the dispatch target's implementation detail, so it will not fail from this change (reconfirm at implementation time).
- **R-23 (source: magi package "retain the legacy behavior of opening the config file as a separate command")**: Extract the existing external-editor launch (`open_config_file()`) into a new, independent `AppCommand::EditConfigFile`, and add a new menu item (e.g. label "Edit Config File...") near the existing Preferences menu item. Also add a new entry to the command palette (a one-line addition matching the shape of `command_palette.rs`'s `AppCommand::Preferences => "Open Preferences"`). Assign no default keybinding (since Cmd+,'s meaning change alters the operational flow, leave it unassigned as a chattering-prevention measure for now; users may assign an arbitrary chord only via config keybind).
- **R-24 (source: magi package "list OpenThemePicker/OpenSettings in the menu + assign default keybindings")**: Add a menu item (e.g. label "Open Theme...") and a default keybinding `cmd+shift+,` to `AppCommand::OpenThemePicker` (add to the `specs` array in `KeybindEngine::default()`; confirmed that `cmd+shift+,` is unused in the current default bindings list). Since `AppCommand::OpenSettings` becomes reachable from Cmd+, (via Preferences) to the same `ThemeSettingsMode::Settings` per R-22, do not additionally assign a duplicate default chord to the `OpenSettings` variant itself (direct reachability from the command palette / config keybind action names `settings.open`/`open_settings` remains unchanged). This decision is the default choice under the "ambiguous, reversible" category; the rationale is recorded in Open Questions.

### Session / UX (richness)

- **R-25 (source: magi package "Tab reverse-mode reopen, carry over filter/scroll")**: Currently the Tab key is a no-op (`toggle_section` is an empty implementation). Change this to "reopen the current session in the reverse `ThemeSettingsMode` and carry over the filter string (when going Theme→Settings→Theme) or the scroll position (`selected_row`)". This reopen is a third kind of transition distinct from both Esc (revert) and Enter (commit), and must not alter `gpu.preview_theme` or the runtime state of any live-applied row (font-size/opacity/blur/cursor-style/sidebar-preview-lines) at all (no config write either).
- **R-26 (source: magi package "live match-count display")**: Add a match count in the format `highlighted position + 1 / filtered_len()` (e.g. `12 / 574`) to the Theme mode's display. The key-hint footer (`ThemeSettingsViewModel::footer`) is already implemented; this requirement is only the differential addition of the count display onto the existing footer.
- **R-27 (source: magi package "contrast ratio display + low-contrast warning")**: Reuse `noa_render::theme::contrast_ratio(a: Rgb, b: Rgb) -> f32` (existing public function, `noa-render/src/theme.rs:177`) to add the contrast ratio between `default_fg`/`default_bg` of the highlighted/previewed theme to the Theme mode's display. Show a warning (either color or icon, whichever is expressible in both the native and wgpu paths) when it falls below the WCAG AA equivalent (4.5:1, same value as `noa-render`'s `DEFAULT_MINIMUM_CONTRAST`). Do not implement new contrast-calculation logic (call the existing function only).
- **R-28 (source: magi package "font-family fuzzy search")**: Make the `SettingsRowKind::FontFamily` row (currently only ←→ cycling via `cycle_font_family`, `state.rs:719-731`) fuzzy-searchable. Reuse `command_palette::fuzzy_match` (existing, the same matcher used by the theme picker); do not implement a second matcher. Do not change the existing classification as a commit-only row (`is_live() == false`).
- **R-29 (source: magi package "favorites: persisted in a separate state file")**: Add a "favorite" toggle for themes. Persist it to a state file separate from `~/.config/noa/config` (not involved at all in the config writer's surgical-update contract — R-12/R-14), functioning only as an additional filter ("show favorites only" toggle) in Theme mode. Must not touch the commit path (`commit_updates`/`write`) at all.
- **R-30 (source: magi package "light/dark attribute filter: derived on the fly from fg/bg luminance")**: Compute relative luminance from each theme's `default_fg`/`default_bg` (reuse existing luminance-calculation logic in `noa_render::theme` — equivalent to `relative_luminance` — without changing the `ThemeDef` schema), and add a "Light/Dark" attribute filter to Theme mode. Whether to maintain a precomputed cache (574 entries × luminance) or compute on the fly is an implementation-time decision (free choice as long as it satisfies NFR-8's scrubbing performance requirement).
- **R-31 (source: magi package "post-commit undo toast")**: Immediately after a successful Enter commit, display an undo toast holding the pre-commit snapshot (`RevertValues`). The undo operation uses the existing commit path (the same write function as `ThemeSettings::commit`) to re-commit the values from the immediately preceding snapshot ("commit path invariant" — do not create a new write/apply mechanism). Reuse (by generalizing) the existing generic toast card mechanism (`draw_toast_card`/`macos_overlay::sync_toast`; currently the sole caller is `WindowState.resize_overlay: Option<(String, Instant)>`, a resize-only field) for the toast's display/rendering. Define the priority for cases where the resize toast and undo toast overlap in time (the newer one replaces the older) at implementation time.
- **R-32 (source: magi package "mouse wheel support")**: Currently, `App::on_mouse_wheel` (`app/event_loop.rs:1124`), aside from the sidebar band (`handle_sidebar_wheel`), does not consider the open/closed state of the theme settings overlay at all, so wheel events flow straight through to pane terminal scrolling even while the panel is open. Add an early branch that routes to a new `handle_theme_settings_wheel` under the same "(bool) return true if consumed" contract as `handle_sidebar_wheel`, when `self.active_overlay(window_id) == ActiveOverlay::ThemeSettings`, mapping the wheel delta onto the existing up/down navigation (highlight movement in Theme mode / row selection in Settings mode). Clicking is out of scope (magi scope boundary). Follow the `WHEEL_PAGE_THRESHOLD` accumulation pattern from `app/overview/interaction.rs::apply_overview_wheel` as the reference implementation for the wheel accumulation logic.
- **R-33 (source: magi package "preview: multiple representative sample lines using real colors")**: The current `sample_swatches` (`theme_settings/sample.rs`) only provides color patches for the 16 ANSI colors + fg/bg/cursor/selection + one truecolor entry. Turn this into "text sample lines using the actual fg/bg/selection colors" (e.g. a normal text line, an emphasized text line, a selection-highlighted line, etc., across multiple lines), reflected within the same frame at both of the existing 3 rendering sync points (below in L2) (wgpu `theme_picker_overlay_text` and native `theme_settings_view_model`). Reuse the color data returned by `sample_swatches` itself; do not add new color-derivation logic.

### Data Safety (non-negotiable core)

- **R-34 (AC-13, source: magi package "implicit overwrite forbidden under theme = light:X,dark:Y")**: `open_theme_settings` (`app/input_ops/theme_settings.rs:32-96`) currently passes only `self.config.theme` (`Option<String>`, where the pair has already been resolved into a single name by appearance) to `ThemeSettingsInit.current_theme`, and never references `self.config.theme_appearance: Option<noa_config::ThemeAppearancePair>` (`app/config.rs:23`, which holds the pair information itself) at all. As a result, when `ThemeSettings::commit_updates` (`state.rs:806-812`) produces `updates.push(("theme".to_string(), name))`, `noa_config::apply_updates` (`writer.rs:27-78`) unconditionally replaces the final line of the `theme` key (the very `light:X,dark:Y` pair line) with the single theme name, destroying the pair syntax. This requirement mandates that when committing a single theme from the panel while the original config was a pair setting, the implementation **rewrite only the currently-active appearance side, preserving the pair syntax while retaining the other appearance's value unchanged** (adopting the latter of the magi ruling's two options — "present + explicit confirm" vs. "rewrite only the current appearance side" — as the default implementation; rationale in L2).

### Non-Functional Requirements

- **NFR-7 (performance, F1/F2)**: A steady-state frame with no state change (idempotent sync) while the panel is open must complete with zero `ThemeSettings` `clone()` calls and zero `theme_settings_view_model` calls. Add a measurement mechanism following the existing `ChromeTextures` rebuild counter (debug-only `AtomicUsize`) pattern.
- **NFR-8 (performance, F3)**: A burst of fast successive keystrokes against the 574-entry catalog must fit into one full/differential fuzzy scan per debounce window. As long as the new filter string is a prefix extension of the immediately preceding filter, the scan target is limited to the previous `filtered` result set (not the full 574 entries).
- **NFR-9 (data safety, R-34)**: After committing a single theme from the panel against a config containing `theme = light:X,dark:Y`, a valid pair syntax containing both the `light:`/`dark:` tokens must remain, parseable by `noa_config::parser::values::parse_theme_pair`. After the write, values other than the changed appearance side must match byte-for-byte with the pre-write state (extending the spirit of the existing NFR-5 to the pair case).

## L2 — Detail

### The 3 rendering sync points (particularly relevant to F2/R-33)

theme-settings has 3 independent rendering paths, bound by a contract that they agree on values via a shared pure function. This increment preserving that contract is a concretization of Conservation Constraint 5.

1. **wgpu fallback text path** — `theme_settings_overlay_text` → `theme_picker_overlay_text`/`settings_rows_overlay_text` in `app/sidebar/palette.rs`. The text representation drawn as ANSI terminal cells.
2. **Shared value formatter** — `RowDraft::display_value`/`settings_row_display_value` in `theme_settings/rows.rs`. The single value-formatting function called by **both** (1) above and (3) below. New display elements from R-26 (count), R-27 (contrast), R-29 (favorite mark), etc. must reach both paths via this function (or an equally-shared new function) — neither path may be forked on its own.
3. **Native ViewModel builder** — `theme_settings_view_model` in `macos_overlay/model.rs`. The structured data actually rendered by the AppKit card. R-20 (F2)'s hash-precheck optimization targets the call cost of this function itself.

### noa-app: state machine / session

- Add the following to `ThemeSettings` (`theme_settings/state.rs`):
  - For R-25: `carryover()` (extracts the current `filter`/`highlighted` or `selected_row`), and an `Option<ThemeSettingsCarryover>` field on `ThemeSettingsInit` (when non-`None`, `ThemeSettings::open` overrides the default filter/highlighted initialization with the carryover value).
  - For R-28: while keeping the `FontFamily` row's `draft` as a plain string, add a fuzzy-listing function equivalent to `filter_font_families(query) -> Vec<FontMatch>` alongside `cycle_font_family` (same shape as the `filter_themes` pattern).
  - For R-29/R-30: add `favorites: &FavoritesStore` (or an equivalent reference) and `attribute_filter: Option<Light|Dark>` to the session state. Do not let these affect `commit_updates()` at all (favorites/attribute filter only join into `filter_themes`'s narrowing as filter conditions).
  - For R-31: separately from the `Vec<(String, String)>` returned on successful `commit`, return a clone of `self.snapshot: RevertValues` right before the commit to the `App` side (the target the undo toast re-commits).
- Add the following to `App::open_theme_settings` (`app/input_ops/theme_settings.rs:32`):
  - For R-34: add a `theme_appearance: Option<noa_config::ThemeAppearancePair>` field to `ThemeSettingsInit` and pass `self.config.theme_appearance.clone()` (currently only `self.config.theme` is passed).
  - Determine which appearance side is "currently active" by reusing the same appearance-resolution logic as the existing `effective_theme_name`/`app/config.rs:367` (winit's `Theme::Light`/`Theme::Dark`); do not build new resolution logic.
- The new Tab (R-25) handler must not branch into either `close_theme_settings` (Esc equivalent) or `commit_theme_settings` (Enter equivalent); as a third transition it reconstructs `ThemeSettingsSession` for the new mode. `gpu.preview_theme` and live-applied runtime state (runtime_font_size, etc.) must not be touched at all.

### noa-app: input

- Add an `ActiveOverlay::ThemeSettings` branch to `on_mouse_wheel` (`app/event_loop.rs:1124`) at the same position as `handle_sidebar_wheel` (before pane-scroll-related routing) (R-32).

### noa-app: menu / keybindings

- `macos_menu.rs`: two new menu items (`EditConfigFile`, `OpenThemePicker`). Add a `_menu_item_spec()` function with the same shape as `preferences_menu_item_spec()`.
- `commands/command.rs`: add a new `AppCommand::EditConfigFile` variant (register `menu_id()`/`action_name()`/palette title). Change `OpenThemePicker.menu_id()` from `""` to a real ID.
- `commands/keybind.rs`: add `("cmd+shift+,", AppCommand::OpenThemePicker)` to the `specs` array in `KeybindEngine::default()`.

### noa-config: writer / pair safety (R-34)

- `apply_updates` in `noa-config/src/writer.rs` itself keeps its current contract of "replace the key's final line" unchanged (does not affect any other key or NFR-5's byte-precision guarantee).
- Add a new preprocessing layer at the call site (`noa-app`): immediately before commit, determine whether the target config's `theme` directive is a pair (determinable via `ThemeSettingsInit.theme_appearance.is_some()`, reusing already-parsed information — no need to re-parse the raw text), and if it is a pair, push into `updates` a `("theme", "light:<new-or-kept>,dark:<new-or-kept>")` value — the pair syntax string itself — instead of `("theme", name)`. Use the original value from `ThemeSettingsInit.theme_appearance` unchanged for the inactive side. This approach requires no changes to `apply_updates`'s logic itself, only changing the `value` passed in to a pair string — satisfying NFR-9 with a minimal diff.
- Why the "present + explicit confirm" option (the magi ruling's other choice) was not adopted: (a) adding a new modal type is inconsistent with the other items in the magi scope ("full mouse click is out of scope", etc., a policy of minimizing interaction cost), (b) rewriting only the currently-active side is always reversible via Esc/undo toast (R-31), (c) it naturally fits the existing touched-row model (Conservation Constraint 2) (the pair-rewrite decision completes within L2 as a side effect of the touched flag, requiring no new UI state). AC-C2 (conditional stretch, below) adds "a UI for editing the pair itself within the panel" on top of this, and is not a substitute for this requirement's "rewrite only the active side" but an extension of it.

### noa-render

No changes (as in v1, `OverlayStyle::from_theme()` computes on demand and so automatically tracks `Theme` swaps on the `noa-app` side). `contrast_ratio` (R-27) is only a call to the existing public function.

### Edge cases

- **`filtered` becomes empty during carryover (R-25)**: In theory, carrying over the filter string across a Theme→Settings→Theme round trip cannot result in zero matches if the 574-entry catalog's state has not changed (the catalog is static), but carrying over while favorites/attribute filters (R-29/R-30) are ON can result in zero matches. Follow the same "list is empty, previous `preview_theme` is retained" behavior as AC-16.
- **Favorites state file unreadable (R-29)**: A load failure at startup falls back to an empty favorites set and does not block the panel itself from launching (same "best-effort, warn-log only" policy as config load errors).
- **One side of a pair's name doesn't exist in the 574-entry catalog (R-34)**: Since validation of `theme_appearance` is already performed on the `noa-config` side (`theme_pair_diagnostic`), the `theme_appearance` passed at the time the panel opens is guaranteed to have both sides present as strings (name-resolution success/failure remains the responsibility of the config layer; this increment does not re-validate it).
- **Simultaneous resize toast and undo toast (R-31)**: The newer one immediately replaces the older (keep a single `WindowState` toast display slot, tagging only the kind with something like `enum ToastKind { Resize, Undo }`).

## L3 — Acceptance Criteria

Verification-method legend carried over from v1: [unit] = unit test requiring no GPU / [integration] = integration test using e.g. tempdir / [code-review] = implementation inspection / [measurement] = quantitative measurement via a debug counter etc. / [visual GUI check] = manual confirmation.

### Performance Remediation

- **AC-25 (R-19)** [measurement]: Given the panel is open, render 10 consecutive frames. When measuring each frame's `ThemeSettings` duplication path. Then only a conversion to the new rendering-only snapshot type occurs, and no full duplication including `filtered: Vec<ThemeMatch>` occurs (a substantially smaller amount of duplicated data compared to the old implementation; the concrete measurement mechanism is the debug counter shared with NFR-7).
- **AC-26 (R-20, NFR-7)** [unit]: Given two consecutive calls to logic equivalent to `sync_theme_settings` with a `ThemeSettings` whose state is completely identical to the previous frame. When measuring the second call. Then the number of calls to `theme_settings_view_model` (or its equivalent construction function) is 0.
- **AC-27 (R-20)** [unit]: Given a call to the same logic with a `ThemeSettings` whose state has actually changed (e.g. highlighted moved). When inspecting the call result. Then the change is detected by the lightweight-key comparison and the ViewModel is rebuilt (verifying no false negatives on change detection).
- **AC-28 (R-21, NFR-8)** [unit]: Given a burst of fast successive prefix-extension keystrokes on the filter string (e.g. "3"→"30"→"302"→"3024", intervals < debounce window). When simulating elapsed time across the debounce window. Then a full scan of all 574 entries occurs only once, and intermediate states scan only within the previous `filtered` result set (test by measuring scan count, or recording call count).
- **AC-29 (R-21)** [unit]: Given an input that is not a prefix extension (Backspace breaking the prefix relationship, or replacement with an entirely different string). When re-filtering. Then it falls back to a full scan of all 574 entries (preventing false positives in the differential-narrowing logic).

### Menu / Keybindings

- **AC-30 (R-22)** [unit]: Given `AppCommand::Preferences` is dispatched. When inspecting the dispatch result. Then `open_config_file()` is not called, and the overlay opens with `ThemeSettingsMode::Settings`.
- **AC-31 (R-22)** [code-review]: Confirm that the `preferences_menu_item_is_enabled_and_routes_to_preferences` test in `macos_menu.rs:729` passes unchanged after the R-22 implementation (confirming the menu item's identity/accelerator itself is unchanged).
- **AC-32 (R-23)** [unit]: Given `AppCommand::EditConfigFile` is dispatched. When inspecting the dispatch result. Then the same side effect as the existing `open_config_file()` occurs (launching the external editor).
- **AC-33 (R-24)** [unit]: Given `KeybindEngine::default()` is constructed. When resolving the command corresponding to `cmd+shift+,`. Then `AppCommand::OpenThemePicker` is returned.

### Session / UX

- **AC-34 (R-25)** [unit]: Given filter string "abc" has been entered in Theme mode and Tab is pressed. When inspecting the new session's state. Then `mode == Settings`, and the filter string previously entered in Theme mode does not apply to Settings mode (ignored, since Settings mode has no filter concept), but when Tab is pressed once more to return to Theme mode, the filter string "abc" is restored.
- **AC-35 (R-25)** [unit]: Given `selected_row` is 5 in Settings mode, Tab is pressed into Theme mode, then Tab is pressed once more back into Settings mode. When inspecting `selected_row` after returning. Then 5 is preserved.
- **AC-36 (R-25)** [unit]: Given a state equivalent to `gpu.preview_theme` and live-applied runtime values such as font-size, before and after a Tab transition. When Tab is round-tripped. Then these values do not change at all before and after the Tab transition (verifying that neither the revert nor commit code path is taken).
- **AC-37 (R-26)** [unit]: Given a filter result of 12 out of 574 entries with highlighted at the 3rd position (0-index 2). When retrieving the data for the count display. Then a value equivalent to "3 / 12" is obtained.
- **AC-38 (R-27)** [unit]: Given a theme with a known fg/bg color pair (with a precomputed contrast-ratio fixed value) is highlighted. When calling the contrast-ratio display logic. Then it matches the return value of `noa_render::theme::contrast_ratio`, and the warning flag is set when it is below 4.5.
- **AC-39 (R-28)** [unit]: Given a fuzzy search query is entered against `available_font_families`. When inspecting the results. Then the same scoring/highlight positions as `command_palette::fuzzy_match` are obtained (verifying that a second matcher is not implemented).
- **AC-40 (R-29)** [unit]: Given theme A is added to favorites and the filter is switched to "favorites only". When inspecting the list. Then only theme A (or the intersection of the favorites set and the fuzzy-match condition) is shown, and the output of `commit_updates()` contains no favorites-related key at all.
- **AC-41 (R-29)** [integration]: Given a favorites state file does not exist on a tempdir and one favorite is added. When inspecting the state file. Then a new file is created, and config write (`write_config_updates`) is never called.
- **AC-42 (R-30)** [unit]: Given two themes with known fg/bg luminance (one clearly light, one clearly dark). When the attribute filter is set to "Light". Then the theme judged dark is excluded from the list.
- **AC-43 (R-31)** [unit]: Given a state immediately after a successful commit. When inspecting the undo toast's trigger condition. Then a toast-display flag holding the pre-commit `RevertValues` snapshot is set.
- **AC-44 (R-31)** [unit]: Given the undo toast is displayed and the undo action is performed. When inspecting the write function call. Then the same write function as commit (`write_config_updates`) is called with the pre-commit snapshot's values, and no new write path is used.
- **AC-45 (R-32)** [unit]: Given `ActiveOverlay::ThemeSettings` is open and a wheel event is sent. When calling logic equivalent to `on_mouse_wheel`. Then the event is consumed (return value equivalent to `true`) and does not propagate to pane terminal scrolling.
- **AC-46 (R-32)** [unit]: Given a wheel event is sent in Theme mode. When inspecting the accumulation logic. Then highlighted movement occurs via the same shape of threshold accumulation as `apply_overview_wheel` (not a simple 1-notch = 1-item mapping, for consistency with the existing pattern).
- **AC-47 (R-33)** [unit]: Given the fg/bg/selection color data returned by `sample_swatches`. When calling the multi-line sample generation logic. Then each generated line actually uses one of the real theme's fg/bg/selection colors (verifying no hardcoded placeholder colors are included).
- **AC-48 (R-33)** [code-review]: Confirm via implementation inspection that both `theme_picker_overlay_text` (wgpu path) and `theme_settings_view_model` (native path) call the same multi-line sample generation function (verifying that the 3-rendering-sync-point contract is preserved for the new feature as well).

### Data Safety

- **AC-49 (R-34, NFR-9)** [unit]: Given a session opened with `theme_appearance = Some(ThemeAppearancePair { light: "A", dark: "B" })`, with the current appearance = Light, theme "C" is highlighted and committed. When inspecting the output of `commit_updates()`. Then it contains `("theme", "light:C,dark:B")` (with the dark side "B" preserved), and does not contain the simple-overwrite value `("theme", "C")`.
- **AC-50 (R-34, NFR-9)** [integration]: Given a config file containing `theme = light:A,dark:B` exists on a tempdir, actually write the same commit as AC-49 to the file. When parsing the written file with `parse_theme_pair`. Then it parses as a valid pair, with `light` as the new value and `dark` unchanged as the old value "B".
- **AC-51 (R-34)** [unit]: Given a session opened with `theme_appearance = None` (a normal, non-pair config), commit a theme. When inspecting the output of `commit_updates()`. Then the simple value `("theme", "<name>")` is output as before (verifying no regression in the non-pair existing behavior).

### Conditional Stretch (only if the implementation loop judges it cheap)

- **AC-C1** (real cell-renderer preview): [code-review]+[visual GUI check]. Keeping `noa-render/tests/pipeline.rs` green is a precondition for implementation. If it cannot be kept green, only this one item is dropped, with no effect on the other ACs.
- **AC-C2** (in-panel pair editing UI): [visual GUI check]. Undertaken only if, after implementing AC-49/AC-50 (R-34)'s "rewrite only the active side", it is judged at implementation time that UI for explicitly editing both sides of a pair can be added at near-zero cost. If not undertaken, AC-49/AC-50 are still independently satisfied.

## Traceability Table

| Requirement | AC |
|---|---|
| R-19 | AC-25 |
| R-20 | AC-26, AC-27 |
| R-21 | AC-28, AC-29 |
| R-22 | AC-30, AC-31 |
| R-23 | AC-32 |
| R-24 | AC-33 |
| R-25 | AC-34, AC-35, AC-36 |
| R-26 | AC-37 |
| R-27 | AC-38 |
| R-28 | AC-39 |
| R-29 | AC-40, AC-41 |
| R-30 | AC-42 |
| R-31 | AC-43, AC-44 |
| R-32 | AC-45, AC-46 |
| R-33 | AC-47, AC-48 |
| R-34 | AC-49, AC-50, AC-51 |
| NFR-7 | AC-26 |
| NFR-8 | AC-28 |
| NFR-9 | AC-49, AC-50 |

All 16 Rs + 3 NFRs have ≥1 AC each, for 27 ACs total (AC-25–51) + 2 conditional ACs (AC-C1/C2). 16 of 16 requirements (R-19–R-34) have AC linkage = **traceability 100%** (exceeding Standard's 85% target. Rationale: since the upstream task instruction explicitly required AC concretization for all Rs/NFRs, coverage was made broader than Standard's usual "primary L3 only" operation). [visual GUI check] is the primary verification method only for AC-C1/AC-C2; all others are automatically verifiable via [unit]/[integration]/[code-review]/[measurement].

## Open Questions / Deferred Decisions

- **R-24's decision not to assign a default keybinding to OpenSettings**: `AppCommand::OpenSettings` itself is not given a new default chord, reachable only via Cmd+, (through Preferences). If real-world user feedback reveals a desire for a direct chord for OpenSettings alone, adding an available chord (e.g. `cmd+alt+,`) is an increment absorbable within this spec's scope (no structural change needed).
- **R-31's toast display duration**: The concrete millisecond value (consistency with the resize toast, whether to follow the existing `resize_overlay` timeout or set a longer value specific to undo) is left to implementation-time judgment.
- **Whether to undertake AC-C2**: Depends on the estimate from the implementation loop after R-34 is complete. Alternative if not undertaken: pair-config users manually edit the other side of the pair from outside Cmd+, (via the retained `AppCommand::EditConfigFile`, R-23).
- **Favorites state file path/format**: Assumed to be something like `~/.config/noa/theme-favorites`, but consistency with the existing path conventions for other noa state files (session saves, etc., `noa-app/src/session.rs`) will be confirmed at implementation time.

## Build-path decision

Undecided (build-path selection was not performed at the time of the magi ruling). For the next step, we recommend handing off to **atlas (Tech design)** per Accord's AUTORUN contract — while the 3 performance-remediation items (F1/F2/F3) are local optimizations within the existing architecture with low atlas design cost, R-34 (data safety) adds a new preprocessing layer to the config writer's call contract, so we recommend firming up the design decisions (where to place the preprocessing layer, the type design for extending `ThemeSettingsInit`) in atlas before starting.

---

## Amendments (Phase 5 Risk Gate reflected, 2026-07-11)

- **AC-52 (new, must)**: When filtered is recomputed due to a filter-state change (⌃D/⌃⇧F/Tab carryover), (a) if the highlighted theme remains, the highlight follows it and preview is unchanged; (b) if it is excluded, preview_theme is unchanged + highlight moves to the front + `highlight_moved` is reset (preview does not fire until an explicit up/down); (c) at 0 entries, follow AC-16. Verification: unit test (asserting preview_theme/highlighted before and after toggling the filter). Details: theme-settings-v2.ux.md Addendum A-2.
- **AC-53 (new, must)**: Display the local caption `⌃⇧F` on the favorites chip (symmetric with the ⌃D cycle). Verification: unit test on the string-generation function shared by both rendering paths. Details: ux.md Addendum A-1.
- **AC-28 implementation-shape adjustment (per ADR-3)**: Satisfy "no full 574-entry fuzzy rescan per keystroke" via prefix-differential narrowing rather than timer debounce. Verification: assert that the scan scope on a forward keystroke is within the previous filtered set.

## Amendments 2 (omen FMEA reflected, 2026-07-11) — additional ACs for the R-34 group

- **AC-54 (must)**: Given `theme_appearance = Some(light:A,dark:B)` and system appearance Light, opening Settings mode yields `current_theme == "A"` (or `"B"` under Dark). It must never be an empty string. Verification: unit.
- **AC-55 (must)**: Under the same premise, if the theme picker is untouched and only non-theme rows are touched before commit, the output of `commit_updates()` must not contain the `"theme"` key. Verified by a new test duplicating the existing `settings_mode_commit_updates_never_includes_a_theme_change` with a pair-resolution fixture.
- **AC-56 (must)**: Invariant test that `highlight_moved` is always false in Settings mode (FM-01 defense in depth).
- **AC-57 (must)**: At least one integration scenario test for pair × carryover × favorites toggle × commit (FM-03).
- **AC-58 (must)**: Add a debug-only rebuild counter (following the ChromeTextures.record_rebuild() pattern) to `rebuild_theme_settings`, and pin rebuild=1 per genuine state change via sync in a test (FM-05). Record one manual measurement note of release-build filter-keystroke latency in the PR/journal.
- **AC-59 (must)**: After multiple Tab round trips, Esc must roll back to the **very first** open point (FM-04, extends AC-36).
- **AC-60 (must)**: Property test that every mutator changes view_fingerprint (FM-02 upgraded).
- **AC-61 (code-review)**: Confirm the wgpu/native degradation strategy for the added chip row explicitly addresses this (FM-06).
- **AC-62 (must)**: Interruption guard on undo re-commit (invalidated if another commit/reopen occurs after the toast is shown) (FM-08). Favorites write failures must not be silent (FM-09).
- **FM-10 is ACCEPT-RISK**: The contrast threshold 4.5 as an independent literal constant (unrelated to the user's minimum-contrast setting) is valid.
