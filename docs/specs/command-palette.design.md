# Command Palette — Implementation Design + Risk Gate (ADR)

> **Historical baseline:** This accepted design retains its implementation-time
> file list and line references. The renderer implementation now lives in
> `noa-render/src/renderer/overlay.rs`, while app dispatch and input routing live
> under `noa-app/src/app/`; use symbol names below as the stable anchors.

- status: **Accepted** (Atlas Phase 5, 2026-07-04)
- spec (authoritative): `docs/specs/command-palette.md` (locked). This design does not re-litigate scope.
- gate verdict: **Conditional-Go** (4 conditions, §5). Phase 6 (orbit/builder) may proceed.
- scope of change: **`noa-app` + `noa-render` only.** `noa-grid`/`noa-vt`/`noa-core`/`noa-font`/`noa-pty` untouched (NFR-3).

---

## 1. Context / forces

The palette exposes the existing `AppCommand` registry as a `cmd+shift+p` searchable modal. The nearest analog — `SearchPrompt` / `SearchPromptSession` — already solves every hard sub-problem: single app-wide modal session, keystroke pre-emption ahead of pty encoding, leak-free cleanup on close, and a `FrameSnapshot` overlay that holds no terminal lock. The design **mirrors search_prompt at every seam** rather than inventing parallel machinery. The one genuine delta is multi-row list rendering (search_prompt is single-row), which extends the existing `CellInstance` overlay path (no new GPU pipeline, no new std140/bind-group surface — per the spec's rejected alternative).

### Reuse confirmation (search_prompt parity)

| Concern | search_prompt mechanism | palette reuse |
|---|---|---|
| Session ownership | `App.search_prompt: Option<SearchPromptSession>` (`app.rs:285`) | `App.command_palette: Option<CommandPaletteSession>` — **window-bound only, no `pane_id`** |
| Keystroke pre-emption | `KeyboardInput` branch `app.rs:1890-1897` | new branch inserted **immediately after** it, before keybind-resolve |
| Modal key handler | `handle_search_prompt_key` `app.rs:2453-2509` | `handle_command_palette_key` (same shape) |
| Leak cleanup | `close_tab` `app.rs:848-854`, `close_pane` `app.rs:928-935` | `close_tab` only (window-bound ⇒ `close_pane` not needed) |
| GPU seam | `FrameSnapshot.search_prompt: Option<String>` (`snapshot.rs:59`), built at `app.rs:399` | `FrameSnapshot.command_palette: Option<CommandPaletteSnapshot>`, built at same point |
| Overlay draw | `append_search_prompt_instances` (`renderer.rs:1016`) | `append_command_palette_instances` (multi-row extension of the same fn) |

**Conclusion:** the palette reuses search_prompt's overlay + cleanup machinery wholesale. The only *new* code is (a) the pure title/entry/filter logic, (b) multi-row overlay emission, (c) the window-bound (vs pane-bound) session variance.

---

## 2. Module layout

- **New file `crates/noa-app/src/command_palette.rs`** (GUI-agnostic pure logic, unit-testable without window/GPU):
  - `command_palette_title(AppCommand) -> &'static str` — **exhaustive `match`, no `_` wildcard** (NFR-4 compile gate). Covers all 49 registry rows incl. `SelectTab(1..9)` and `ToggleCommandPalette`.
  - `command_palette_entries() -> &'static [AppCommand]` — deterministic declaration-order list, **excluding** `ToggleCommandPalette` (self) and `SelectTab(1..9)` (R-3 CUT).
  - `command_palette_filter(query: &str) -> Vec<AppCommand>` — walks `command_palette_entries()`, keeps subsequence matches, preserves order (R-7).
  - `is_subsequence_ci(needle, haystack) -> bool` — hand-written case-insensitive subsequence (NFR-1, no `nucleo`/`fuzzy-matcher`).
- **`commands.rs`**: enum variant + roundtrip arms + keybind spec + reverse-lookup API (§4.1).
- **`app.rs`**: `CommandPaletteSession` struct, field, scope arms, toggle, routing, key handler, cleanup, snapshot build.
- **`snapshot.rs` (noa-render)**: `CommandPaletteSnapshot` type + `FrameSnapshot` field. This is a render-facing DTO (titles + keybind hints already resolved in the app layer); noa-render stays terminal-agnostic.

Rationale for a new file over extending `commands.rs`: keeps the ~40-arm title registry and filter logic cohesive and separately testable; `commands.rs` is already a large enum-plumbing module (God-module pressure). Register with `mod command_palette;` in `lib.rs`.

---

## 3. State struct + ownership / cleanup (the #1 must-have)

```
struct CommandPaletteSession {
    window_id: WindowId,        // window-bound; NO pane_id (R-11 simplification)
    query: String,
    filtered: Vec<AppCommand>,
    selected: usize,
}
```
- `App.command_palette: Option<CommandPaletteSession>`, init `None`, declared next to `search_prompt`.
- **Single session invariant:** `toggle_command_palette()` — if `Some` → set `None`; if `None` → bind to `self.focused` window, `query: ""`, `filtered = command_palette_entries().to_vec()`, `selected: 0`, redraw (R-5).
- **Leak-free contract (R-11, the parity must-have):** in `close_tab` (`app.rs:848-854` region, beside the search_prompt clear) add: if `command_palette` targets the closing `window_id` → `None`. `close_pane` needs **no** change because the session is window-bound; a pane-only close leaves the palette valid (AC-17), and a whole-tab close always routes through `close_tab` (AC-16). This is strictly simpler than search_prompt's two-site cleanup.

---

## 4. Wiring detail

### 4.1 Registry + keybind (R-1, R-2, R-4) — `commands.rs`
- Add `AppCommand::ToggleCommandPalette` variant. Add one arm each to the **four exhaustive matches**: `menu_id`, `from_menu_id`, `action_name` (`"command-palette.toggle"`), `from_action_name`. Add `TOGGLE_COMMAND_PALETTE_MENU_ID: &str = "noa.view.toggle-command-palette"`. *(These four matches fail to compile until the arm is added — the compile gate that guarantees roundtrip completeness.)*
- Add `("cmd+shift+p", AppCommand::ToggleCommandPalette)` to the `KeybindEngine::default()` spec array.
- **Reverse keybind lookup (R-4):** add `KeybindEngine::chord_for(AppCommand) -> Option<String>` that scans `self.bindings` (ordered ⇒ deterministic first-match) and reconstructs the chord text from `TriggerMods` + `KeyToken` via a new `impl Display for KeyTrigger`. Single source of truth = the engine; **do not** hardcode a second keybind table. `command_palette_keybind(cmd)` in `command_palette.rs` calls it through a thin accessor.

### 4.2 Scope (R-1, R-10) — `app.rs`
- `command_scope(ToggleCommandPalette) = CommandScope::App` (open from any tab).
- `overview_command_scope(ToggleCommandPalette) = CommandScope::Overview` (no-op while overview focused — AC-15).
- `handle_app_command` gains a `ToggleCommandPalette => self.toggle_command_palette(...)` arm.

### 4.3 Modal routing (R-6) — `app.rs:1890` region
Insert the palette branch **exactly between** the search_prompt branch (`1897`) and the keybind-resolve (`1900`):
```
if self.command_palette.as_ref().is_some_and(|s| s.window_id == window_id) {
    self.handle_command_palette_key(event_loop, window_id, &event);
    return;
}
```
Order is load-bearing: IME-preedit → search_prompt → **palette** → keybind-resolve → overview → cmd-swallow. Because search_prompt is checked first, **a palette cannot open while search_prompt is open** (its keys are consumed there and `cmd+shift+p` swallowed). Because the palette branch consumes all keys while open, **search_prompt cannot open via keybind while the palette is open**. (The one escape hatch — executing a modal-opening command — is handled in §4.4/§5-FM1.)

### 4.4 Key handler (R-7/8/9/10) — `handle_command_palette_key`
Mirror `handle_search_prompt_key`:
- `Esc` → `command_palette = None` (no execution).
- `Enter` → if `filtered` empty → no-op (R-9, no panic, no empty index); else take `filtered[selected]`, **set `command_palette = None` first**, then `handle_app_command(cmd)` (R-10 close-before-side-effect; satisfies AC-14 ordering).
- `Arrow Up/Down` → clamp-move `selected` (no wrap), redraw.
- `Backspace` → `query.pop()`, `filtered = command_palette_filter(&query)`, `selected = 0`, redraw.
- resolved-command combos: if `== ToggleCommandPalette` → dispatch (re-toggle closes); else swallow (mirror of search_prompt's FindNext/Prev pass-through).
- `super_key()` held with no binding → swallow. Else printable `event.text` → append, re-filter, `selected = 0`, redraw.

### 4.5 Snapshot (R-12) — `app.rs:399` region + `snapshot.rs`
- `CommandPaletteSnapshot { query: String, rows: Vec<(String /*title*/, Option<String> /*keybind*/)>, selected: usize }` in `snapshot.rs`; `FrameSnapshot.command_palette: Option<CommandPaletteSnapshot>` (default `None`).
- Build in the per-pane snapshot loop, filtered by **`window_id == window_id && pane_id == state.focused_pane`** so the window-bound palette draws exactly once (over the focused pane), not once per split. No terminal lock taken (palette is terminal-independent).

### 4.6 Renderer (R-12) — `renderer.rs`
`append_command_palette_instances` extends the `append_search_prompt_instances` pattern: a centered block = query row + one row per `rows` entry (title left, keybind hint right-aligned), selected row drawn with an inverted/highlight bg. Pure `CellInstance` bg-rects + shaped glyph runs — **no new pipeline, no new bind-group/std140 exposure** (keeps `pipeline.rs` invariants unchanged). Call at the two existing search_prompt call sites (`renderer.rs:697, 1001`).

### 4.7 Menu (R-1) — `macos_menu.rs`
Add a View-menu `MenuItem::with_id(AppCommand::ToggleCommandPalette.menu_id(), "Command Palette", true, Some(cmd_shift_accelerator(Code::KeyP)))` near the Tab-Overview item (`macos_menu.rs:198`).

---

## 5. Risk Gate (omen FMEA + ripple)

### omen — FMEA (RPN = Severity × Occurrence × Detection, 1-10 each)

| # | Failure mode | S | O | D | RPN | Band |
|---|---|---|---|---|---|---|
| FM1 | **Double-modal**: a modal-opening command (`Search(Find)`) dispatched via **menu bar** (bypasses palette `Enter` close path) while palette open → two overlays own the keyboard | 6 | 4 | 6 | **144** | High |
| FM2 | Routing branch mis-ordered (placed after keybind-resolve, or before IME) → keys leak to pty / IME preedit eaten | 6 | 3 | 5 | **90** | High |
| FM3 | Session leak on window close — `close_tab` clear forgotten (search_prompt's *original* bug) → dead-window ref, open-guard blocks all future `cmd+shift+p` | 7 | 3 | 4 | **84** | Med-High |
| FM4 | Snapshot not filtered to focused pane → palette redrawn N× across splits | 3 | 4 | 5 | 60 | Med |
| FM5 | `command_palette_title` uses `_` wildcard → NFR-4 gate defeated, later variants render blank | 5 | 4 | 3 | 60 | Med |
| FM6 | `cmd+shift+p` keybind collision | 6 | 2 | 2 | 24 | Low |

**Mitigations for High-RPN items (Detection / Prevention / Recovery):**

- **FM1 (RPN 144)** — *Prevention:* at the top of `handle_app_command`, after the overview guard, **close the palette if open and `command != ToggleCommandPalette`** ("dispatching any command implies leaving the palette"). This is idempotent with the explicit `Enter`-path close (§4.4) and closes the menu-bar hole. *Detection:* AC-14 asserts `command_palette == None` before `Search(Find)` dispatches; add a menu-path unit case. *Recovery:* `Esc` always clears `command_palette`; search_prompt's own `is_some()` open-guard prevents a second search prompt regardless.
- **FM2 (RPN 90)** — *Prevention:* insert the branch at the exact seam in §4.3 (between `1897` and `1900`). *Detection:* AC-7 routing test — a printable key with palette open must not reach keybind-resolve/pty. *Recovery:* none needed if test present.
- **FM3 (RPN 84)** — *Prevention:* wire the `close_tab` clear beside the existing search_prompt clear (§3). *Detection:* AC-16 (reopen after close). *Recovery:* toggle always rebuilds a fresh session, so even a stale-clear bug self-heals on next successful close.

### ripple — blast radius

- **Files changed (7):** `commands.rs`, `command_palette.rs` (new), `app.rs`, `snapshot.rs`, `renderer.rs`, `macos_menu.rs`, `lib.rs`.
- **Compile-forced breaks (intended gates):** the 4 exhaustive matches in `commands.rs` + `command_palette_title` won't compile until the new arm is added — this is the NFR-4 completeness guarantee, not a regression.
- **Tests touched:** add roundtrip cases to `commands.rs` tests (AC-1/2/3/5); new modal tests in `app.rs` (AC-6..17); optional headless assert in `noa-render/tests/pipeline.rs` (AC-19). Existing tests are unaffected (their command lists are hardcoded, not exhaustive).
- **Dependency rule:** no `noa-grid`-and-below edits; no new crates/FFI (NFR-1); `cargo tree -p noa-grid/-p noa-vt` stays `wgpu`/`winit`-free (AC-22).
- **UX-friction (folded in, no dedicated agent):** keyboard-only with `Esc`-cancel, incremental subsequence filter, `selected→0` reset per keystroke, clamp-no-wrap arrows — all match user expectation. Keybind hints as text chords (not ⌘ glyphs) is an accepted v1 DEFER. **No blocking friction.**

### Ripple verdict: **Conditional-Go**

Conditions folded into the Builder change list (all four are low-effort, single-site):
- **C1 (FM1):** defensive palette-close at `handle_app_command` entry for any non-toggle command.
- **C2 (FM2):** routing branch inserted exactly between the search_prompt branch and keybind-resolve.
- **C3 (FM4):** snapshot build filtered by `window_id && pane_id == focused_pane`.
- **C4 (FM3):** `close_tab` clears a palette targeting the closed window; `close_pane` intentionally untouched.

---

## 6. Ordered file-by-file change list (for Builder)

1. **`crates/noa-app/src/commands.rs`** — add `ToggleCommandPalette` variant; 4 roundtrip arms (`menu_id`/`from_menu_id`/`action_name`/`from_action_name`); `TOGGLE_COMMAND_PALETTE_MENU_ID`; keybind spec `("cmd+shift+p", …)`; `impl Display for KeyTrigger` + `KeybindEngine::chord_for`. Tests: AC-1, AC-2, AC-5.
2. **`crates/noa-app/src/command_palette.rs`** *(new)* — `command_palette_title` (exhaustive, no `_`), `command_palette_entries`, `command_palette_filter`, `is_subsequence_ci`, `command_palette_keybind`. Tests: AC-3, AC-4, AC-8, AC-9, AC-23.
3. **`crates/noa-render/src/snapshot.rs`** — `CommandPaletteSnapshot` struct; `FrameSnapshot.command_palette` field + `None` default.
4. **`crates/noa-render/src/renderer/overlay.rs`** — `append_command_palette_instances` (multi-row); the symbol, rather than the historical call-site line numbers, is the stable anchor.
5. **`crates/noa-app/src/app.rs`** — `CommandPaletteSession`; `App.command_palette` field + init; scope arms (`command_scope`, `overview_command_scope`); `handle_app_command` toggle arm + **C1** defensive close; `toggle_command_palette`; **C2** routing branch; `handle_command_palette_key`; **C4** `close_tab` cleanup; **C3** snapshot build. Tests: AC-6, AC-7, AC-10..17, AC-18.
6. **`crates/noa-app/src/macos_menu.rs`** — View-menu item + `cmd_shift_accelerator(Code::KeyP)`.
7. **`crates/noa-app/src/lib.rs`** — `mod command_palette;` (+ any re-export needed for `CommandPaletteSnapshot` used across app↔render).
8. **`crates/noa-render/tests/pipeline.rs`** *(optional)* — headless one-frame draw with a palette payload (AC-19).
9. **Gate:** `cargo test --workspace --offline` + `cargo clippy --workspace --offline` clean, no new `#[allow(...)]` (NFR-5 / AC-24).

---

## 7. Rollback

Fully additive and feature-isolated. Rollback = revert the 7 files; `AppCommand::ToggleCommandPalette` and its `FrameSnapshot` field are the only cross-module surfaces, both removable without touching the fidelity core. No data migration, no persisted state.
</content>
</invoke>
