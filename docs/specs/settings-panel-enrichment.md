# Spec: Settings Panel Enrichment & Optimization (settings-panel-enrichment)

## Metadata

- **slug:** settings-panel-enrichment
- **title:** Settings Panel Enrichment & Optimization
- **status:** **draft** (magi verdict scope B+ finalized 2026-07-11; the spec body itself is not yet signed off)
- **owner:** simota
- **scope mode:** **Standard** (10 requirements [8 must + 2 nice] + 3 NFRs, medium complexity — matches the Accord template selection criteria)
- **upstream:** `theme-settings-ui.md` (locked — the foundation for the overlay's launch entry point, preview mechanism, commit sequence, and config writes. This spec is an increment on top of it that adds "quality of information design" and does not re-litigate upstream decisions)
- **traceability:** 10 R + 3 NFR = 13 items, each with 1-3 ACs, full coverage across all items (100%, exceeding the Standard scope's 85% floor)

## L0 — Vision

`noa`'s Settings overlay (`crates/noa-app/src/theme_settings/`) is already implemented per `theme-settings-ui.md`, and functions with live/next-launch preview across 16 settings rows, fuzzy search across 574 themes, and surgical config writes. However, "enrichment" here is defined not as visual decoration but as **quality of information design** — the user should always know, with zero misleading display, which rows take effect immediately and which are waiting for next launch; be able to quickly find the setting they want among the 16 rows; confirm what it means; and safely reset an accidentally changed value back to default. At the same time, the existing implementation carries a concrete performance debt — "a full clone every frame" (explicitly noted in the doc comment at `theme_settings/state.rs:68-79`) — which this spec resolves.

Per the magi verdict (2026-07-11, not to be re-litigated), the adopted scope is **B+**: 8 must-have items + 2 nice-to-have items (the nice-to-haves start only after all musts are green). This spec concretizes and substantiates that magi verdict through a direct audit of the implementation code. The audit found that several items the magi verdict assumed were "unimplemented" are actually **already implemented, partially or in a different form** (see "Code audit results" below). Each requirement in this spec has been refined to reflect that audit, without altering the intent of magi's Critical#1/#2.

### Code audit results (delta from the magi verdict)

| magi item | assumption | audit result | how this spec handles it |
|---|---|---|---|
| Critical#1 (opaque restart note) | unimplemented | **partially implemented**: `ThemeSettings::restart_note()` (`theme_settings/state.rs:306-332`) already detects both the opacity/blur row case at opaque-at-startup and the touched commit-only-row case, both as a `bool`. However, both cases display the same string, `"(restart to apply)"` (`app/sidebar/palette.rs:923-924`, `macos_overlay/imp/appkit.rs:1099-1100`), with no distinction between the reasons | R-1: reuse the detection logic; scope the diff to adding reason presentation |
| Critical#2 (menu item addition) | unimplemented | **partially implemented**: `AppCommand::OpenSettings` already exists and is wired to `open_theme_settings(Settings mode)` (`app/commands.rs:146`), but `menu_id()` returns an empty string (`commands/command.rs:217`), so it isn't registered in the native menu. Meanwhile, the existing "Settings..." (⌘,) menu item is wired to `AppCommand::Preferences`, which launches the external editor (`open_config_file()`, `app/commands.rs:66`) — this was a deliberate decision in `theme-settings-ui.md` R-1 ("keep the existing ⌘, unchanged and let it coexist") | R-2: add a new menu item and wire up `OpenSettings`. Leave the existing ⌘, untouched (preserving the locked upstream decision) |
| Must#3 (badges on all rows) | unimplemented | **unimplemented (as expected)**: `restart_note()` shows nothing for commit-only rows unless `touched` — before any edit, commit-only rows are indistinguishable from live rows | new implementation as R-3 |
| Must#4 (per-frame clone) | unimplemented | **unresolved, as expected**: `session.state.clone()` confirmed at `app/render.rs:48`. The doc comment at `state.rs:68-79` explicitly notes this as "accepted debt, including the filtered field (up to 574 entries)" | new implementation as R-4 (`Arc` + `make_mut` approach) |
| Must#5 (fuzzy search) | unimplemented | **unimplemented (as expected)**: in Settings mode, `push_text` only handles direct digit/path entry for the `FontSize`/`BackgroundImage` rows; there is no filter over the row list | new implementation as R-5 |
| Must#6 (descriptions) | unimplemented | **unimplemented (as expected)**: `SettingsRowKind` has no description function | new implementation as R-6 |
| Must#7 (Reset) | unimplemented | **unimplemented (as expected)**: no reset operation exists | new implementation as R-7 |
| Must#8 (zero existing regressions) | hard gate | confirmed current state of the existing `theme_settings/tests.rs` (981 lines) + tests within `app/input_ops/theme_settings.rs` | hard-gated as R-8 |
| Nice#9 (4 C-safe keys) | unimplemented | `scrollback_limit`/`cursor_style_blink`/`minimum_contrast`/`macos_option_as_alt` are all real fields of `StartupConfig` (`noa-config/src/lib.rs:611-625`) | new implementation as R-9 (after all musts are green) |
| Nice#10 (tokenizing contrast) | assumed: migrate hardcoded colors | **audit found almost nothing to migrate**: the AppKit-side selected-row background already uses `colors.selected_bg` (from `OverlayColors`, `macos_overlay/imp/appkit.rs:941,1114`), and the wgpu-side selected-row foreground already uses the `accent: Rgb` argument (resolved from the theme at the call site). No hardcoded RGB literals were found | redefine the scope for R-10: limited to adding regression tests that guard the existing token paths |

## Out of scope (scope boundary, finalized by magi, not to be re-litigated)

- Theme light/dark pairing (`theme = light:X,dark:Y`)
- Adding section headings inside the Settings overlay
- Mouse operation support (click-to-select, scroll-drag, etc.)
- VoiceOver / accessibility tree support
- Changing the transparency mechanism (the winit-creation-time fixed constraint on `background_opacity`, the ALPHA_REPLACE path — leave entirely unchanged to avoid reigniting the magenta-stripe RCA [see `[[noa-nativetab-magenta]]` memory])
- Changing the wiring of the existing ⌘, ("Settings...") menu item
- Changing the preview mechanism, commit sequence, or config write format from `theme-settings-ui.md`

### Failure conditions (finalized by magi, hard gate)

- F1: any of the existing 16 rows' behavior regresses
- F2: reintroduces a transparency-mechanism or magenta-stripe-family bug
- F3: work begins on any of the out-of-scope items above
- F4: any of search (R-5), Reset (R-7), or descriptions (R-6) is marked complete while only partially implemented
- F5: work begins on the nice-to-haves (R-9, R-10) before the must-haves (R-1 through R-8) are all green

## L1 — Requirements

### Must-have (magi Critical/required, 8 items)

- **R-1 (restart note with reason)**: Replace `ThemeSettings::restart_note(kind) -> bool` (`theme_settings/state.rs:306-332`) with a type that distinguishes the reason (e.g., `RestartReason::None | RestartReason::OpaqueStartup | RestartReason::CommitOnly`). Display distinct copy for the opacity/blur row at opaque startup versus a touched commit-only row (e.g., font-family). Do not change the transparency mechanism itself (out of scope).
- **R-2 (native menu item)**: Give `AppCommand::OpenSettings` (existing, already wired to `open_theme_settings(Settings mode)` at `app/commands.rs:146`) a non-empty `menu_id()`, and add it as a new item in the native menu in `macos_menu.rs`. Do not change the existing ⌘,/"Settings..." (`AppCommand::Preferences` → `open_config_file()`) in any way.
- **R-3 (always-on badges for all rows)**: Derive "Live"/"Next launch" badges from `SettingsRowKind::is_live()` (existing, static classification) and display them at all times on all 16 rows (20 after nice#9 is adopted), regardless of `touched` state. Keep this as a signal independent of R-1's reason-annotated note, so both coexist (zero misleading display — resolving the current issue where an unedited commit-only row is indistinguishable from a live row at a glance).
- **R-4 (eliminate per-frame clone)**: Remove `session.state.clone()` (a full clone of `ThemeSettings`, including `filtered: Vec<ThemeMatch>` up to 574 entries) at `app/render.rs:48`. Make `ThemeSettingsSession.state` an `Arc<ThemeSettings>`; the render path uses `Arc::clone` (a refcount bump only), and mutating methods go through `Arc::make_mut`.
- **R-5 (fuzzy search over settings rows)**: Repurpose `command_palette::fuzzy_match` to fuzzy-search the 16 (20) Settings-mode rows by label. Empty query shows all rows; no matches shows zero rows.
- **R-6 (description for the selected row)**: Add a static one-line description to every `SettingsRowKind`, and display it in the view for the currently selected row (both the AppKit card and the wgpu text card).
- **R-7 (Reset to Default)**: Add an operation to reset the currently selected row, per-row, to the default value derived from `noa_config::StartupConfig::default()`.
- **R-8 (zero existing regressions, hard gate)**: Do not change the externally observable behavior of the existing 16 rows' values, key operations, commit/revert, or restart-note detection logic in any way. All existing tests in `theme_settings/tests.rs` (981 lines) and `app/input_ops/theme_settings.rs` must stay green without modification.

### Nice-to-have (only after all musts are green, 2 items)

- **R-9 (unlock 4 C-safe keys)**: Add `scrollback-limit` / `cursor-style-blink` / `minimum-contrast` / `macos-option-as-alt` (all real fields of `StartupConfig`) as new rows. Each row includes the "5-piece set" from R-2 through R-6 (a `RowDraft` variant, `is_live()` classification, `commit_updates()` mapping, label, and description). `SettingsRowKind::COUNT` goes from 16 to 20.
- **R-10 (regression protection for selected-row contrast)**: The code audit confirmed that the selected row's background color (`OverlayColors::selected_bg`) and foreground color (`accent: Rgb`) already go through existing UI tokens in both render paths (AppKit/wgpu). Since there is no code left to migrate, this requirement is redefined as "add contrast-ratio regression tests that guarantee the existing token paths never regress to hardcoded values in the future."

### Non-functional requirements (NFR)

- **NFR-1 (allocation)**: Across consecutive redraws between frames with no input (no selection or edit occurring), no deep clone of `ThemeSettings` may occur (directly tied to R-4).
- **NFR-2 (no 60fps degradation)**: The settings-row fuzzy search (R-5, up to 20 rows) uses the same `fuzzy_match` as the existing 574-theme fuzzy search, and recomputes only on text-input events (the same trigger discipline as the existing `refilter_and_mark`). It must not introduce a per-frame recompute path while idle.
- **NFR-3 (no config-writer expansion)**: None of R-1/R-3/R-6/R-7/R-9 may introduce a new config write path other than `noa_config::write_config_updates` (existing, introduced in `theme-settings-ui.md` R-14).

## L2 — Detail

### R-1: Restart note with reason

- Target files: `crates/noa-app/src/theme_settings/state.rs` (`restart_note` method), `crates/noa-app/src/macos_overlay/model.rs` (`ThemeSettingsViewModel::rows` tuple), `crates/noa-app/src/app/sidebar/palette.rs:923-924`, `crates/noa-app/src/macos_overlay/imp/appkit.rs:1099-1100`.
- Replace `restart_note(kind: SettingsRowKind) -> bool` with `restart_reason(kind: SettingsRowKind) -> RestartReason`. Reuse the existing condition logic as-is (opaque_at_startup && Opacity/BlurRadius, or a non-live row being touched); only widen the return type from `bool` to a 3-value enum.
- Change the third element of the `ThemeSettingsViewModel::rows` tuple (currently `bool`) to `RestartReason` (or already-resolved copy, `Option<&'static str>`). Branch the copy at both render-path sites (`app/sidebar/palette.rs:923-924`, `appkit.rs:1099-1100`) into two strings, one for `RestartReason::OpaqueStartup` and one for `RestartReason::CommitOnly`.
- Example copy (to be finalized at implementation time): `CommitOnly` → `"(restart to apply)"` (keep the existing string), `OpaqueStartup` → something like `"(opaque at launch — restart to preview)"`, explaining why it's waiting for next launch.
- Do not change the transparency mechanism, the `opaque_at_startup` determination, or the `background_opacity >= 1.0` threshold itself.

### R-2: Native menu item

- Target files: `crates/noa-app/src/commands/command.rs` (`menu_id()` function, add `OPEN_SETTINGS_MENU_ID` constant, add to `from_menu_id`), `crates/noa-app/src/macos_menu.rs` (`MacosMenu::install`).
- Assign `AppCommand::OpenSettings` a `OPEN_SETTINGS_MENU_ID` constant (same naming convention as the other `*_MENU_ID` constants), and replace `AppCommand::OpenSettings => ""` in `menu_id()` with the real ID. Also add it to the reverse lookup in `from_menu_id`.
- Build a new `MenuItem` and add it to `view_menu` (the block in `macos_menu.rs` where the "Command Palette"/"Session Overview" items live). Match the label wording used in the command palette ("Open Settings…"). To avoid confusion with the existing "Settings..." (⌘,), assign no accelerator (unbound, same as the existing command-palette entry point).
- Do not change `AppCommand::Preferences`'s menu_id, label, accelerator (⌘,), or wiring to `open_config_file()` in any way — preserving `theme-settings-ui.md` R-1's decision that "the existing ⌘, stays unchanged and coexists."
- Follow the existing test patterns `preferences_menu_item_spec`/`fullscreen_menu_item_spec` (in the `#[cfg(test)] mod tests` at the end of `macos_menu.rs`) — extract an `open_settings_menu_item_spec()`-equivalent as a plain, window/GPU-free function, unit-testable.

### R-3: Always-on badges for all rows

- Target files: `theme_settings/state.rs`, `macos_overlay/model.rs` (`ThemeSettingsViewModel`).
- Use `SettingsRowKind::is_live()` (existing, static) directly as the source of truth — do not build new classification logic.
- Add a `live: bool` field (= `kind.is_live()`) to the `ThemeSettingsViewModel::rows` tuple, separate from R-1's `RestartReason`. Draw both independently: `live` is a badge visible at all times, even without selecting the row (e.g., a small "Live"/"Restart" label at the row's end), while `RestartReason` is R-1's supplementary "(restart to apply)"-style copy (meaningful only once touched).
- Satisfy the "zero misleading display" requirement by ensuring the `live` badge never depends on the `touched` value — all 20 rows' classification is visible immediately after opening the overlay, even before anything has been edited.

### R-4: Eliminate per-frame clone

- Target files: `crates/noa-app/src/app/state.rs` (`ThemeSettingsSession` definition, `app/state.rs:537`), `crates/noa-app/src/app/render.rs:44-48`, `crates/noa-app/src/app/input_ops/theme_settings.rs` (all mutating call sites equivalent to `session.state.xxx_mut`), `crates/noa-app/src/app/sidebar/palette.rs` (the `state: &ThemeSettings` argument of `draw_theme_settings_card`/`theme_settings_overlay_text`), `crates/noa-app/src/macos_overlay/mod.rs` (the `&ThemeSettings` argument of `sync_theme_settings`).
- Change `ThemeSettingsSession.state: ThemeSettings` to `state: Arc<ThemeSettings>`.
- Replace `session.state.clone()` (the deep clone) at `render.rs:48` with `Arc::clone(&session.state)` (a refcount bump only). Since `sidebar::draw_theme_settings_card` and `macos_overlay::sync_theme_settings` already take `&ThemeSettings`, no change is needed beyond resolving the reference from `Arc<ThemeSettings>` to `&ThemeSettings` (Deref or an explicit `&*`).
- Route the call sites of mutating methods (`move_up`/`move_down`/`adjust`/`push_text`/`backspace`/`commit`/`revert`, etc., called from `input_ops/theme_settings.rs`) through `Arc::make_mut(&mut session.state)`. On the single-threaded winit event loop, the `Arc` clone taken by `redraw()` is dropped within the same frame, and the refcount returns to 1 before the next keypress, so `make_mut`'s actual-clone branch almost never fires (redraw itself never mutates `session.state`, so rendering and mutation structurally never hold the same `Arc` at once).
- Why share the entire `Arc<ThemeSettings>` (rather than splitting off a view-model): the wgpu-side `theme_picker_overlay_text`/`settings_rows_overlay_text` (`app/sidebar/palette.rs`) perform variable windowing sized to the actual pane (clamping `THEME_SETTINGS_COLS`/`ROWS` to the pane's real cols/rows), so the AppKit-side fixed-8-row windowing `ThemeSettingsViewModel` (`macos_overlay/model.rs`) cannot be reused as-is as a common snapshot type for both paths (the windowing granularity differs). Therefore this spec adopts "Arc sharing + make_mut" rather than "split into a lightweight view-model type" (the latter of the two candidates presented in the task).

### R-5: Fuzzy search over settings rows

- Target files: `theme_settings/state.rs` (`ThemeSettings` struct, `push_text`/`backspace`), `app/input_ops/theme_settings.rs` (Tab key handling).
- Add new fields specific to `Section::SettingsRows`: `settings_search_active: bool`, `settings_filter: String`, and `settings_filtered: Vec<usize>` (indices into `SettingsRowKind::ALL`, ordered with the same scoring as `ThemeMatch`).
- **Search enter/exit gesture**: currently in `Section::SettingsRows`, the `Tab` key merely calls `toggle_section()` (a dead code path that's a no-op due to DEC-2, see the doc comment at `state.rs:240-244`). Reuse this dead hook and repurpose `Tab`, only within a `ThemeSettingsMode::Settings` session, as "toggle search mode" (leave Theme mode's `Tab` unchanged — it only ever meant section switching there, and is out of scope for this spec).
- **Input routing while search is active**: while `settings_search_active == true`, `push_text`/`backspace` prioritize appending to/deleting from `settings_filter`, regardless of the currently selected row's kind (digit entry for `FontSize`, path entry for `BackgroundImage`). Exiting search (Tab again, or Enter) returns to normal row-edit input routing.
- An empty query shows all of `SettingsRowKind::ALL` in original display order (following the same empty-query behavior as `fuzzy_match`, `command_palette_matches`/`filter_themes`). No matches shows zero rows, and — following the same pattern as `ThemePicker`'s `filtered.is_empty()` guard (`move_up`/`move_down`, `state.rs:347-379`) — makes `move_up`/`move_down` a no-op when the list is empty.
- On exiting search, select whichever filtered-result row was highlighted at that moment (the same approach as the command palette's confirm operation).

### R-6: Description for the selected row

- Target files: `theme_settings/rows.rs` (`SettingsRowKind`), `macos_overlay/model.rs` (`ThemeSettingsViewModel`), `app/sidebar/palette.rs` (wgpu text card), `macos_overlay/imp/appkit.rs` (AppKit card).
- Add `SettingsRowKind::description(self) -> &'static str` as a static match function shaped like `label()` (`rows.rs:98-118`). Give all 20 kinds (after R-9 is adopted) a one-line English description.
- Add a `selected_description: &'static str` field to `ThemeSettingsViewModel`, derived in `theme_settings_view_model()` from `SettingsRowKind::ALL[state.selected_row()].description()`.
- Add one line directly below the selected row (or above the footer area) in both render paths. If this touches the card's minimized vertical-height floor (the existing "card min-3 row-count shrink" constraint from the `theme-settings-ui spec` memory), absorb it by prioritizing the description display and reducing the theme-list or row-list display count by one line, coexisting with the existing `overlay_scroll_window` display row count (via the existing `THEME_SETTINGS_ROWS`/`THEME_LIST_ROWS` constant adjustment).

### R-7: Reset to Default

- Target files: `theme_settings/state.rs` (new `ThemeSettings::reset_selected_row`), `theme_settings/rows.rs` (new `RowDraft::default_for(kind) -> RowDraft`), `app/input_ops/theme_settings.rs` (Delete key handling).
- Default value source: `noa_config::StartupConfig::default()` (`noa-config/src/lib.rs:590-649`, confirmed to exist). Symmetric with how `ThemeSettingsInit` assembles initial values from `StartupConfig`'s fields, add `RowDraft::default_for(kind: SettingsRowKind) -> RowDraft` to `rows.rs` as a pure function that assembles a `RowDraft` from the corresponding field of `StartupConfig::default()` (to avoid duplicate maintenance against the `ThemeSettingsInit` assembly logic, design `open_theme_settings`'s initial-value mapping and `default_for` to reference a shared conversion function — reconcile against the field mapping in `app/input_ops/theme_settings.rs:open_theme_settings` at implementation time).
- `ThemeSettings::reset_selected_row(&mut self, now: Instant) -> RowEffect`: replace the selected row's `draft` with `RowDraft::default_for(kind)` and set `touched = true` (written on commit only if the default differs from the snapshot, reusing the existing `touched` gate in `commit_updates()`). For live rows (`FontSize`/`BackgroundOpacity`/`BackgroundBlurRadius`/`CursorStyle`/`SidebarPreviewLines`), return the same `RowEffect` as `adjust()`, joining the same apply path as `adjust_theme_settings_row` in `app/input_ops/theme_settings.rs`.
- **Key binding**: since `Backspace` is already used for digit entry in `FontSize`/path entry in `BackgroundImage`, avoid a conflict and assign `NamedKey::Delete` (forward delete, a physical key distinct from `Backspace`, present in `winit::keyboard::NamedKey`) for Reset (added to the top-level match in `handle_theme_settings_key`, alongside `Escape`/`Enter`/`Tab`). No confirmation dialog is added, since Esc (the existing R-16 whole-session revert) already provides recovery from a mistaken press (a reversible operation — defaulting to no confirmation per the Ambiguous+reversible principle).
- If Reset is pressed while a FontSize debounce (the existing R-9 debouncer) is in flight, route it through the same debounce-submission path as `set_font_size` (submit to the debouncer rather than writing the value directly, staying consistent with the existing R-9 (old spec) font-size handling).

### R-8: Zero existing regressions (hard gate)

- Add new code without changing the signature or return-value meaning of any existing public method (R-1 is the sole exception, changing `restart_note`'s return type from `bool` to an enum; this single method's type change is explicitly called out as an accepted change, and all 3 call sites [inside `state.rs`, `model.rs`, and both render paths] must be updated to follow).
- The existing test functions in `theme_settings/tests.rs` (981 lines) and `#[cfg(test)] mod commit_theme_settings_tests` in `app/input_ops/theme_settings.rs` must stay green with zero changes to their assertion bodies. New tests may only be added.
- `cargo test -p noa-app` is the gate.

### R-9: Unlock 4 C-safe keys (after all musts are green)

- The 4 target keys: `scrollback-limit` (`StartupConfig::scrollback_limit`, `DEFAULT_SCROLLBACK_LIMIT`), `cursor-style-blink` (`cursor_style_blink: Option<bool>`), `minimum-contrast` (`minimum_contrast`, `DEFAULT_MINIMUM_CONTRAST`), `macos-option-as-alt` (`macos_option_as_alt: MacosOptionAsAlt`). All confirmed to exist as real fields in `StartupConfig::default()` at `noa-config/src/lib.rs:590-649`.
- Each row has a new `RowDraft` variant, a new `SettingsRowKind` variant, a `label()`/`description()` (R-6) entry, `is_live() == false` (the "C-safe" designation means no runtime-apply path exists, following the same "persist-only, applies on next launch" pattern as the existing `FontFamily`/`WindowPadding`/`MacosTitlebarStyle`, consistent with the existing design decision documented in `commit_theme_settings`'s comment), and a mapping in `commit_updates()` to the corresponding config key.
- With `SettingsRowKind::COUNT` going from 16 to 20, mechanically update the `ALL` array, the `rows` array, and all code assuming `SettingsRowKind::ALL[idx]` (a repetition of the existing 16-row implementation pattern, involving no new design decisions).
- Runtime application (making them live) is out of scope for this spec — explicitly noted as a future increment candidate (see Open Questions below).

### R-10: Regression protection for selected-row contrast

- Target file: `macos_overlay/model.rs` (`OverlayColors`).
- Using the WCAG relative-luminance formula (no external crate needed, pure arithmetic over `[f32;4]` RGBA components), add unit tests within the existing test suite verifying that the contrast ratio between `OverlayColors::selected_bg` vs `surface_fg`, and between `accent` vs `surface_bg`, meets a minimum floor (e.g., 3.0:1, equivalent to WCAG AA Large Text for UI decoration elements).
- Reuse theme/color fixtures already used by the existing tests for verification (e.g., `"3024 Day"` in `theme_settings/tests.rs`); do not perform exhaustive light/dark-pair matrix verification (out of scope).
- Position this requirement not as new tokenization work but as a regression guard ensuring the existing token-based paths (`colors.selected_bg`, `accent: Rgb`) never regress to hardcoded values in the future.

## Edge cases / unverified items (explicit)

- **Interaction between search and selected_row/font_size_digits/background_image_text**: it's undecided how to handle the `FontSize` row being mid-edit (`font_size_digits: Some(..)`) when entering search mode via R-5 (whether to commit or discard the unconfirmed digit entry when search starts). Decide at implementation time, together with the call timing of `clear_row_input_state()` (existing, `state.rs:439-442`).
- **Index stability when exiting search**: when returning from a filtered `settings_filtered` state to showing all rows, it's undecided whether `selected_row` (a raw index into `SettingsRowKind::ALL`) keeps pointing at the pre-search row or preserves its relative position within the search results. Confirm consistency with the command palette's confirm behavior at implementation time.
- **Runtime application of R-9's 4 rows**: this spec treats all of them as persist-only (applied on next launch), but since `cursor-style-blink` is closely related to the existing live `CursorStyle` row, there may be a future opportunity to integrate it with `apply_live_cursor_style`'s `blinking` argument (currently statically derived from `initial_cursor_style`, `app/input_ops/theme_settings.rs:518-528`) and make it live — recorded as a future increment candidate outside this spec's scope.
- **Final text of R-1's copy**: subject to copy review at implementation time; this spec only contracts the meaning (distinguishing the reason).

## L3 — Acceptance Criteria

Verification-method legend: [unit] = GPU-free unit test / [integration] = integration test / [code-review] = implementation inspection / [manual-visual] = manual confirmation.

### R-1 (restart note with reason)
- **AC-1** [unit]: Given opaque startup (`opaque_at_startup=true`) with the `BackgroundOpacity` row unedited. When calling `restart_reason(BackgroundOpacity)`. Then it returns `RestartReason::OpaqueStartup`.
- **AC-2** [unit]: Given the `FontFamily` row (commit-only) has been touched. When calling `restart_reason(FontFamily)`. Then it returns `RestartReason::CommitOnly`, a different variant from AC-1's case.
- **AC-3** [manual-visual]: An opaque-startup session's opacity/blur row displays copy distinct from other commit-only rows' "(restart to apply)".

### R-2 (native menu item)
- **AC-4** [unit]: `AppCommand::OpenSettings.menu_id()` returns a non-empty string, and `AppCommand::from_menu_id` reverses it back to `OpenSettings` (a window-free test of the same shape as the existing `preferences_menu_item_spec` test).
- **AC-5** [code-review]: Confirm via diff that `AppCommand::Preferences`'s menu_id, label, accelerator, and wiring to `open_config_file()` show no changes.
- **AC-6** [manual-visual]: The new menu item opens the Settings overlay (Settings mode), and does not launch the external editor.

### R-3 (always-on badges for all rows)
- **AC-7** [unit]: Given the overlay was just opened (all rows `touched=false`). When building the view model. Then all 20 rows' `live` field matches `SettingsRowKind::is_live()`'s value, one for one.
- **AC-8** [unit]: Given editing a live row. When rebuilding the view model. Then that row's `live` field remains `true`, unchanged (independent of R-1's `RestartReason`).

### R-4 (eliminate per-frame clone)
- **AC-9** [unit]: Given a session is open, and two consecutive redraw-equivalent read-only snapshot fetches (`Arc::clone`) occur without any mutating method being called in between. When comparing the two `Arc` pointers with `Arc::ptr_eq`. Then they are equal (no deep clone occurred).
- **AC-10** [unit]: Given the prior snapshot `Arc` has gone out of scope and been dropped. When calling a mutating method such as `move_down`. Then all existing (R-8-frozen) behavior tests still pass unchanged (regression proof that semantics are unchanged even via `Arc::make_mut`).
- **AC-11** [code-review]: Confirm that the `theme_settings_card` construction code in `app/render.rs` contains no `.clone()` on the `ThemeSettings` value itself.

### R-5 (fuzzy search over settings rows)
- **AC-12** [unit]: Given pressing `Tab` in Settings mode. When inspecting state. Then `settings_search_active` becomes `true`.
- **AC-13** [unit]: Given search is active and typing "curs". When inspecting `settings_filtered`. Then only rows whose label fuzzy-matches "curs" appear, sorted by descending score.
- **AC-14** [unit]: Given a search query with zero matches. When inspecting the list. Then the list is empty, but `move_up`/`move_down` do not panic (no-op).
- **AC-15** [unit]: Given an empty query. When inspecting `settings_filtered`. Then it contains all 20 entries, in the same order as `SettingsRowKind::ALL`.

### R-6 (description for the selected row)
- **AC-16** [unit]: For every kind in `SettingsRowKind::ALL`, `description()` returns a non-empty string that also differs from `label()`.
- **AC-17** [unit]: The `selected_description` returned by `theme_settings_view_model()` always matches `SettingsRowKind::ALL[state.selected_row()].description()`.

### R-7 (Reset to Default)
- **AC-18** [unit]: Given the `FontSize` row (live) has been changed from its default. When executing the `Delete`-key-equivalent operation (`reset_selected_row`). Then the row's `draft` reverts to the value equivalent to `StartupConfig::default().font_size`, `touched=true` is set, and the corresponding `RowEffect` is returned for live application.
- **AC-19** [unit]: Given the `FontFamily` row (commit-only) is unedited. When executing Reset. Then `draft` becomes the default value and `touched=true` is set (touched is set even if the value equals the existing snapshot — to preserve the intent of an explicit reset operation).

### R-8 (zero existing regressions, hard gate)
- **AC-20** [unit/integration]: When running `cargo test -p noa-app`, all existing test functions under `theme_settings::tests` and `app::input_ops::theme_settings::commit_theme_settings_tests` (981 lines' worth) pass with no changes to their assertion bodies.

### R-9 (unlock 4 C-safe keys)
- **AC-21** [unit]: For each of the 4 new rows, confirm the `RowDraft` variant, `is_live() == false`, and that `commit_updates()` maps to the corresponding config key (`scrollback-limit`/`cursor-style-blink`/`minimum-contrast`/`macos-option-as-alt`).
- **AC-22** [unit]: `SettingsRowKind::COUNT == 20`, and the existing `SettingsRowKind::ALL[i]` invariant (matching `rows[i]`'s draft variant) holds for all 20 rows.

### R-10 (regression protection for selected-row contrast)
- **AC-23** [unit]: For `OverlayColors` derived from an existing test fixture theme (e.g., "3024 Day"), verify that the WCAG relative-luminance contrast ratios of `selected_bg` vs `surface_fg` and `accent` vs `surface_bg` both meet or exceed the defined minimum floor.

### NFR
- **AC-24 (NFR-1)** [unit]: Same as AC-9 (direct proof that no allocation occurs).
- **AC-25 (NFR-2)** [code-review]: Confirm via code review that settings-row fuzzy-search recomputation is triggered only by text-input events (the same discipline as the existing `refilter_and_mark`), and that no per-frame recompute path exists while idle.
- **AC-26 (NFR-3)** [code-review]: Confirm that none of the diffs for R-1/R-3/R-6/R-7/R-9 add any new config-write function other than `noa_config::write_config_updates`.

### Traceability

| Requirement | AC |
|---|---|
| R-1 | AC-1, AC-2, AC-3 |
| R-2 | AC-4, AC-5, AC-6 |
| R-3 | AC-7, AC-8 |
| R-4 | AC-9, AC-10, AC-11 |
| R-5 | AC-12, AC-13, AC-14, AC-15 |
| R-6 | AC-16, AC-17 |
| R-7 | AC-18, AC-19 |
| R-8 | AC-20 |
| R-9 | AC-21, AC-22 |
| R-10 | AC-23 |
| NFR-1 | AC-24 |
| NFR-2 | AC-25 |
| NFR-3 | AC-26 |

10 R + 3 NFR = 13 items total, each covered by at least one AC, for 26 ACs total (AC-1 through 26). Traceability completeness is **100%** (exceeding the Standard scope's 85% floor). Only AC-3/AC-6 are [manual-visual]; the other 24 are verifiable automatically via unit/integration tests or through implementation inspection.

## L4 — Reversibility / Learning / Disqualification

```yaml
L4:
  reversibility:
    classification: HIGH
    # All 10 requirements are additive changes within a single crate (noa-app), with no change to config format, DB, or public API.
    # Only R-4 (Arc conversion) involves an internal type signature change, but `ThemeSettingsSession` is a private internal type within noa-app that crosses no external boundary.
    revert_procedure: "Revert the relevant commits, or discard the feature branch wholesale. Since neither the config format nor the semantics of the existing 16 rows are touched, there is zero impact on the user's config file."
    revert_time_estimate: "minutes (equivalent to a single git revert command)"
    revert_blast_radius: "Settings overlay only. Does not propagate to the core terminal functionality or other overlays (command palette, overview, etc.) (the R-3 non-functional requirement's mutual-exclusion guard remains as-is)."

  learning:
    hypothesis: "Always displaying a live/next-launch classification badge and description on every row of the Settings overlay, and adding fuzzy search plus a Reset operation, makes it easier to recover from mistakes and reach the desired setting."
    success_threshold:
      metric: "Pass rate of the existing test suite (theme_settings::tests, 981 lines) with zero modification"
      value: 100
      window: "At implementation completion (one CI run)"
    fail_threshold:
      metric: "Number of existing tests modified or failing"
      value: 1
      window: "At implementation completion (one CI run)"
    learning_capture_plan:
      win_capture: "After implementation completes, record the results of cargo test -p noa-app and manual spot-checks (AC-3, AC-6) in the commit message / PR description."
      loss_capture: "If any of failure conditions F1-F5 is triggered, split the offending requirement out of the PR and append the cause to this spec's Open Questions."
      decision_horizon: "At completion of the implementation loop (this spec assumes a single run on either the apex or feature build-path)"

  disqualification:
    conditions:
      - id: DISQ-001
        description: "Any of the existing 16 rows' values, key operations, or commit/revert behavior changes (F1)"
        check: "AC-20 (cargo test -p noa-app existing tests pass unmodified)"
        on_trigger: REJECT
      - id: DISQ-002
        description: "A change is introduced to the transparency mechanism (ALPHA_REPLACE path) or the opaque determination logic (F2)"
        check: "code-review — confirm R-1's diff is limited to restart_reason's return type"
        on_trigger: REJECT
      - id: DISQ-003
        description: "Work begins on any out-of-scope item (light/dark pairing, section headings, mouse operation, VoiceOver, existing ⌘, wiring) (F3)"
        check: "code-review — cross-check the diff's target file list against this spec's L2 file list"
        on_trigger: REJECT
      - id: DISQ-004
        description: "R-5/R-6/R-7 is marked complete while only partially implemented (F4)"
        check: "AC-12 through 15 (R-5), AC-16-17 (R-6), AC-18-19 (R-7) all green"
        on_trigger: REJECT
      - id: DISQ-005
        description: "Work begins on R-9/R-10 before R-1 through R-8 are all green (F5)"
        check: "code-review of implementation order — confirm via commit history that the R-9/R-10 start commit comes after the R-1-R-8-all-green commit"
        on_trigger: REJECT
```

## Meta

- **status:** draft (spec body pending sign-off. magi verdict scope is finalized and not to be re-litigated)
- **version:** 1.0
- **authored by:** Accord agent (including code audit, 2026-07-11). L3 is a refinement pass only — a formal Three Amigos review (product/dev/QA) has not been performed — a human review is recommended before implementation begins.
- **reviews:** none performed
- **upstream lock:** `theme-settings-ui.md` (locked) — this spec increments on top of it and does not change upstream's R/AC/L2 decisions.
- **next:** parallel design by atlas + vision (per the magi verdict's "Next" instruction). Both agents should reference this spec's L2 (particularly R-4's Arc design and R-5's search state machine) as the starting point for implementation design.

## Open Questions / Deferred Decisions

- The design for eventually making one of R-9's 4 rows live (particularly `cursor-style-blink`), and how to integrate it with the existing `apply_live_cursor_style`'s `blinking` argument.
- Index stability when exiting R-5's search (pre-search row vs. relative position within filtered results).
- R-1's final display copy (only the meaning is fixed by this spec; the copy is reviewed at implementation time).
- How R-6's description coexists with the existing "card min-3 row-count shrink" constraint (see the `theme-settings-ui spec` memory), and the adjustment range for `THEME_SETTINGS_ROWS`/`THEME_LIST_ROWS`.

---

## Addendum A — Tech design (Atlas ADR-0001, binding)

- **R-4 (per-frame clone)**: `Arc<ThemeSettings>` + `Arc::make_mut` CONFIRMED — strictly better than status quo.
  - `render.rs:48` `session.state.clone()` → `Arc::clone`. wgpu path unchanged (deref coercion); macOS sync path: `(ts.as_ref(), r)` one-token change.
  - 9 mutation sites in `app/input_ops/theme_settings.rs` (move_up/down, backspace, push_text, adjust, toggle_section, revert, commit, +poll path) go through `Arc::make_mut`.
  - CoW fires effectively never: render's Arc clone is frame-local (dropped at frame end); mutations see refcount==1.
  - **New invariant (code-review gate)**: the render path must NEVER store its Arc clone back into `self` across event-loop turns — that would silently re-enable deep copies via make_mut forks. AC-9 (ptr_eq) covers only the happy path.
  - Rejected: per-field Arc (whack-a-mole, spine still cloned), thin snapshot type (wgpu windowing needs full state — `render.rs:439`).
- **R-5 (search)**: modal sub-state — `settings_search_active: bool` + `settings_filter: String` + `settings_highlight` (symmetric with ThemePicker's filtered/highlighted). In-progress FontSize digits / BackgroundImage text buffers are DISCARDED on search enter+exit via `clear_row_input_state()` (safe: drafts already committed per keystroke). Exit leaves selection on the highlighted row. Empty query = all rows in ALL order; 0 matches = ↑↓ no-op (same guard pattern as `state.rs:367`). Tab toggles search only when `section == SettingsRows`.
- **R-9 (4 new keys)**: use a **6-point set** per key: ALL entry / label / is_live / RowDraft variant / RowEffect+apply path / restart_reason classification. New 4 keys have no runtime-apply → `RestartReason::CommitOnly`; cursor-style-blink is persist-only. COUNT is type-enforced (`[SettingsRow; COUNT]` + open()'s array literal); zero existing tests assume 16.

## Addendum B — UX design (Vision, binding)

- **Search row**: below the section header (`settings_top` slot), hidden entirely when inactive; active row = mono 12pt muted `/{query}` (byte-identical convention to Theme filter). `needed()` unconditionally adds `description_h(19)`, plus `16` when search active.
- **Badges**: label column slack — label w 220→170; badge x=196 w=44 right-aligned 9.5pt semibold. `LIVE` (accent) for `is_live()==true` rows; `ON LAUNCH` (muted) for the rest. Never depends on `touched`. Do NOT use "●" (semantic collision with section-focus glyph).
- **RestartReason display**: `None` → nothing; `CommitOnly` → `(restart to apply)` (existing string); `OpaqueStartup` → `(opaque window — restart to preview)`.
- **Descriptions**: fixed one-line slot directly above the footer (12pt regular muted); never positioned under the selected row. 16 static strings per Vision's table (SettingsRowKind::description()).
- **Keys**: `Tab` toggles search (SettingsRows only; Theme mode keeps no-op). `Enter` in search = confirm highlighted row + exit search; `Tab` again = exit restoring pre-search selection; `Esc` unchanged (whole-overlay cancel, never search-only). Reset = bare `Delete` (NamedKey::Delete; rejected: `r` collides with text entry, Backspace-hold needs a new primitive, cmd+Delete breaks bare-key vocabulary).
- **Footer hint**: `↑↓ navigate   ←→ adjust   Tab search   Delete reset   Esc cancel   Enter save`.
- **Empty state**: `No settings match "{query}"` centered in the list area (muted 12.5pt).
- **wgpu fallback row format**: `{badge:<10}{label:<22}{value}{reason}` — badge words identical to AppKit. Search/description/footer lines mirror AppKit content.
- Implementation-time recommendation (non-mandatory): brief highlight feedback on Reset (reuse tint_layer pattern).

## Addendum C — Risk-Gate conditions (binding, incorporated before implementation)

From Ripple (Conditional-Go conditions):
- **C-1**: R-4's file list additionally includes `app/timers.rs:490-501` (`tick_theme_settings_debounce` calls `poll_font_size` — the 9th mutation site; 8 are in `app/input_ops/theme_settings.rs`). `macos_overlay/sync.rs` needs no change (deref).
- **C-2**: R-1/R-8 amendment — `restart_note(&self, row) -> bool` is KEPT as a thin compatibility wrapper (`self.restart_reason(row) != RestartReason::None`); the 28 existing test call sites in `tests.rs` stay untouched. Only new code (model.rs, palette.rs, appkit.rs) calls the new `restart_reason(&self, row) -> RestartReason`.

From Echo (adopted design refinements):
- **C-3** (MAJOR-1): while search is active, the footer hint switches to a search-specific string (e.g. `Enter confirm row   Tab exit search   Esc cancel`) — Enter's row-confirm (vs save) meaning must be visible in the moment.
- **C-4** (MAJOR-2): Reset accepts BOTH `NamedKey::Delete` (forward delete) and `Cmd+Backspace` (laptop-reachable alias; bare Backspace stays text-delete). Footer text stays `Delete reset`.
- **C-5** (MAJOR-3): the brief Reset highlight feedback (tint_layer pattern) is MANDATORY, not optional — it is the only misfire detection cue.
- **C-6** (MODERATE-4): the badge derives from *effective* liveness: a live-class row downgraded by `RestartReason::OpaqueStartup` shows `ON LAUNCH` (muted) for that session, not `LIVE`. Zero-lie display is per-session truth.
- **C-7** (MODERATE-5, accepted deviation): Settings search row stays hidden when inactive (differs from Theme's always-visible filter row) — accepted for vertical-budget reasons; revisit only if user feedback contradicts.
- **C-8** (MINOR-6): visual-review checklist item: with a selected row, at most badge + reason + description + footer are simultaneously relevant — reviewer confirms this reads as layered, not cluttered.

## Addendum D — Gate aggregate: FM-01 spec correction + orbit contract clauses (binding)

**Risk Gate verdict: Conditional-Go (omen PASS-w/-conditions, ripple Conditional-Go, echo PASS). Conditions below are part of the implementation contract.**

### D-1. FM-01 spec correction (supersedes R-9's classification, RPN 567)
Code-verified: `app/config_reload.rs` (ConfigWatcher, 500ms poll) live-applies `scrollback_limit` (`apply_reloaded_terminal_policies`, :382), `cursor_style_blink` (:174-176), and `minimum_contrast` (`theme_inputs_changed`, :446) after any config-file write — including the Settings panel's own Enter commit. Only `macos-option-as-alt` is genuinely persist-only (read at pty spawn).
- Badge classes become THREE: `LIVE` (accent; applies as you adjust), `ON SAVE` (muted; applies within ~a moment of saving — the 3 reload-applied keys), `ON LAUNCH` (muted; needs restart).
- `RestartReason` for the 3 reload-applied keys = `None` (no "(restart to apply)" text). `macos-option-as-alt` = `CommitOnly`.
- AC-21 is amended accordingly; add a test asserting the 3 keys ARE picked up by the reload diff functions and that `macos-option-as-alt` is absent from them.
- Do NOT suppress or special-case the ConfigWatcher for app-originated writes (it serves external editors; out of scope).

### D-2. Authoritative badge geometry (FM-05 — absolute pt, resolves Addendum B ambiguity)
label x=20 w=170 · badge x=196 w=44 (right edge 240, right-aligned) · value column x=250 (pad+230, UNCHANGED). 10pt gutter, zero overlap. These absolute numbers win over any other reading.

### D-3. Orbit-loop contract clauses (from omen mitigations; all mandatory)
1. (FM-02) Search-mode key routing lives at the ROUTER: `handle_theme_settings_key`'s Enter/Tab/Backspace arms must check search-active state BEFORE falling through to `commit_theme_settings()`/legacy paths. ↑↓ during search navigate a `settings_highlight` over `settings_filtered` — a separate index space from `selected_row`; both exist without cross-contamination. Integration test: Enter mid-search must NOT commit/close.
2. (FM-06) `reset_selected_row` calls `clear_row_input_state()` (mirroring move_up/move_down). Compound test: digits → reset → one more digit derives from post-reset draft.
3. (FM-04) The description line (19pt) and search line (16pt when active) are added into `settings_top`/the `needed()` baseline so the min-3 shrink loop re-solves correctly; on genuinely too-small panes, drop description/search lines before violating the row floor. Unit test `needed(3) <= avail` at the smallest supported pane.
4. (FM-09) wgpu path: implement extra vertical budget as mode-specific offsets inside `settings_rows_overlay_text` only; do NOT touch shared `THEME_SETTINGS_ROWS`/clamp math used by Theme mode.
5. (FM-07) Hard commit boundary: R-1..R-8 (must-have) committed with `cargo test -p noa-app` green BEFORE any R-9/R-10 diff is authored.
6. (FM-08) `RestartReason` derives `Clone, Copy, Debug, PartialEq, Eq, Hash` (view-model cache dedup).
7. (Atlas invariant) Render path never stores its `Arc<ThemeSettings>` clone back into `self` across turns — code-review gate.
8. (Ripple C-1) `app/timers.rs:490-501` is the 9th `Arc::make_mut` site; `macos_overlay/sync.rs` unchanged. (ime.rs:92 also touches the session — audit it during implementation.)
</content>
</invoke>
