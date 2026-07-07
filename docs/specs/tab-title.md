# Tab Title

## Metadata

- slug: tab-title
- feature title: Manual Tab Title
- status: locked
- owner: noa maintainers
- current phase: LOCKED (user-confirmed 2026-07-08)
- parent specs: tabs.md (native tab substrate), tab-overview.md (title
  consumer), session-restore.md (persistence substrate)
- direction picks (user-confirmed 2026-07-07):
  - **Independent of sidebar card rename** — manual tab title affects the
    native tab label + overview tile only; `SessionCard.name_override`
    (session-sidebar FR-7) stays a separate, untouched mechanism.
    **Amended 2026-07-08 (user request, post-LOCK)**: the tab title now
    also shows on the tab's sidebar cards, one-way (tab → card display).
    Precedence: per-card rename (FR-7) > tab title > shell name; the
    card's `name_override` mechanism itself is unchanged (REQ-TTL-11).
  - **Persisted** — manual titles survive quit/relaunch via session
    restore.
  - **Modal prompt UI** — one-line text input on the existing modal
    overlay stack (search-prompt/palette family), not a native NSAlert.

## L0 — Vision

### Problem

A tab's label is fully determined by the shell: the focused pane's
OSC 0/2 title, falling back to `"Noa"` when empty
(crates/noa-app/src/app.rs:449-461,
crates/noa-app/src/app/helpers/geometry.rs:107-113). Users cannot name a
tab after its purpose ("api server", "logs", "scratch"); every prompt
repaint or TUI launch overwrites the label. Ghostty solves this with the
`prompt_surface_title` keybind action and the macOS "Change Title…"
affordance: a manually set title sticks and shell-initiated title
changes stop repainting the label.

### Audience

- macOS users of noa who keep many tabs open and navigate by tab label
  (native tab bar, tab overview).
- Ghostty users who expect `prompt_surface_title` to work when imported
  via ghostty-config.

### Job To Be Done

Give a tab a stable, human-chosen name that survives shell title churn
— and the app relaunch — and clear it again to hand the label back to
the shell.

### Success Definition

Behavior matches Ghostty's manual-title semantics: once set, the tab
label never changes until the user changes or clears it; clearing
reverts to the live shell title. noa extension beyond Ghostty parity:
the manual title is restored after relaunch (Ghostty has no session
restore, so this is net-new surface, not a deviation).

## Scope

### In scope

- A per-tab manual title override (`WindowState`-level, i.e. one native
  tab = one override).
- Modal one-line prompt to set/clear it (IME-capable, Enter commit /
  Esc cancel).
- Command-palette entry, macOS menu item, and a bindable `AppCommand`
  (no default chord — Ghostty ships `prompt_surface_title` unbound).
- Ghostty config import: `keybind = <chord>=prompt_surface_title` maps
  to the new command.
- Override masks OSC 0/2 (and title-stack pop) label updates while set.
- Persistence in `TabSession` → restored on relaunch.
- Overview tile shows the override (it already reads the applied
  `WindowState.title`).

### Out of scope

- Sidebar session-card rename (`name_override`) — existing FR-7 stays
  as-is; no linkage in either direction.
- Per-pane (split leaf) titles — the override names the tab, not a
  pane.
- Ghostty's `title` config key (fixed app-wide title) — separate
  ghostty-config concern, not pulled in here.
- Editing the title by double-clicking the native tab label (AppKit
  offers no hook winit exposes; the prompt is the only entry point).
- Changing what the shell-driven title path does when no override is
  set (fallback `"Noa"`, focused-pane sourcing stay untouched).

## Reuse / constraint findings

Enablers:

- `SidebarRenameSession` (crates/noa-app/src/app/state.rs:430-435,
  app/sidebar/interaction.rs:135-208) is a complete model for a
  buffered, IME-capable, Enter/Esc modal text input.
- `ModalImeTarget` (state.rs:438-445) + input_ops/ime.rs:34-86 already
  route keyboard/IME per modal; adding a variant is the established
  pattern.
- Title application is a single choke point: computed at
  app.rs:449-461, applied diff-only at app.rs:510-513
  (`window.set_title` + `state.title`). Overview tiles read the applied
  `state.title` (app/overview/layout.rs:46-71), so an override applied
  at computation propagates everywhere for free.
- `AppCommand` wiring is a known 4-touch recipe: variant
  (commands/command.rs:9) → dispatch match (app.rs:815) → palette
  entries (command_palette.rs:102-119) → keymap/menu
  (commands/keybind.rs:27, macos_menu.rs).
- Native overlay card compositor (macos_overlay.rs) renders the modal
  chrome; search prompt (search_prompt.rs) shows the snapshot plumbing
  for a live text buffer + preedit.

Constraints:

- `TabSession` (crates/noa-app/src/session.rs:50) has no title field;
  serialize/parse (session.rs:99,245) must be extended
  backward-compatibly (old session files load with no overrides).
- `Terminal.title` (noa-grid) must keep tracking OSC 0/2 while masked,
  so clearing the override reveals the *latest* shell title, not a
  stale one. The mask lives in noa-app only; noa-grid is untouched.
- Title stack (CSI 22/23 t, terminal.rs:99-101) operates on
  `Terminal.title` below the mask; no interaction with the override.

## L1 — Requirements

### Functional

- **REQ-TTL-1**: A "Set Tab Title" command opens a modal one-line
  prompt for the focused tab, seeded with that tab's current effective
  title (override if set, else the applied shell title).
- **REQ-TTL-2**: Committing non-empty text (Enter) sets the tab's
  manual title; the native tab label and its overview tile show exactly
  that text from the next frame on.
- **REQ-TTL-3**: Committing empty text clears the override; the label
  reverts to the shell-driven path (latest `Terminal.title`, `"Noa"`
  fallback) from the next frame on.
- **REQ-TTL-4**: Esc cancels the prompt with no state change.
- **REQ-TTL-5**: While an override is set, OSC 0/2 updates and title
  stack pops keep updating `Terminal.title` but do not change the tab
  label or overview tile.
- **REQ-TTL-6**: The prompt accepts IME composition (preedit shown,
  commit inserts) like the sidebar rename and search prompt do.
- **REQ-TTL-7**: The override is per tab: setting it on one tab changes
  no other tab's label; switching focused panes (splits) within the tab
  does not bypass it.
- **REQ-TTL-8**: The command is reachable from the command palette and
  a macOS menu item, and is bindable via the keybind config; no default
  chord ships (Ghostty parity: `prompt_surface_title` is unbound by
  default).
- **REQ-TTL-9**: Ghostty config import maps the `prompt_surface_title`
  keybind action to this command.
- **REQ-TTL-10**: Manual titles persist in the session state and are
  reapplied to their tabs on session restore; pre-existing session
  files without the field still load (no override restored).
- **REQ-TTL-11** (added 2026-07-08): While a tab's manual title is set,
  every sidebar card of that tab displays it in place of the
  shell-driven session name — unless the card has its own FR-7 rename
  (`name_override`), which wins. Display-only: the store's card names
  are not mutated, so clearing the tab title reverts the cards.

### Non-Functional

- **REQ-TTL-NF-1**: Override resolution (override vs shell title vs
  fallback) is a pure function unit-testable without a `Window`.
- **REQ-TTL-NF-2**: Session serialize/parse round-trips the title
  field, including titles containing spaces, Unicode (CJK/emoji), and
  characters needing escaping in the session format.
- **REQ-TTL-NF-3**: `cargo test --workspace` and
  `cargo clippy --workspace` stay green; existing title behavior with
  no override set is bit-identical (REQ-TAB-7 of tabs.md unaffected).
- **REQ-TTL-NF-4**: While the prompt is open, all keyboard/IME input is
  consumed by the modal (no leakage to the terminal), matching the
  existing `ModalImeTarget` contract.

## L2 — Detail

### noa-app (only crate touched)

- `WindowState` gains `title_override: Option<String>`
  (app/state.rs:209-).
- Title computation (app.rs:449-461): if `title_override` is
  `Some(t)`, the computed title is `t` verbatim, skipping
  `tab_title(&term.title)`; the diff-only apply at app.rs:510-513 is
  unchanged. Overview labels (overview/layout.rs) need no edits.
- New modal: `TabTitlePromptSession { window_id, buffer }` on `App`,
  modeled on `SidebarRenameSession`; new `ModalImeTarget::TabTitlePrompt`
  variant, IME routing in input_ops/ime.rs, key handling (Enter/Esc/
  Backspace/text) mirroring app/sidebar/interaction.rs:154-208; rendered
  as a native overlay card via the existing compositor.
- Command wiring: `AppCommand::PromptTabTitle` — display name
  "Set Tab Title…", menu_id/action_name following the existing `tab.*`
  pattern; dispatch arm in app.rs:815; palette entry in
  `command_palette_entries()` under the tab category; menu item next to
  the existing tab items in macos_menu.rs; NO entry in
  `Keymap::default()`.
- Ghostty import (noa-config/src/import.rs): `prompt_surface_title` →
  the new action name.
- Persistence: `TabSession` gains `title: Option<String>`
  (session.rs:50); serialize/parse (session.rs:99,245) extended;
  restore path sets `title_override` when spawning the tab. Missing
  field parses as `None`.

### Untouched crates

noa-vt, noa-grid (OSC handling, title stack), noa-render, noa-font,
noa-pty. The mask is display-side only.

## L3 — Acceptance Criteria

- **AC-TTL-1** (REQ-TTL-1) [manual-visual] — Given a focused tab whose
  shell set the title "zsh", When the user runs "Set Tab Title…" from
  the palette, Then a modal prompt opens seeded with "zsh" and the
  terminal stops receiving keystrokes.
- **AC-TTL-2** (REQ-TTL-2) [manual-visual] — Given the prompt open,
  When the user types "api server" and presses Enter, Then the native
  tab label and the overview tile both read "api server".
- **AC-TTL-3** (REQ-TTL-3, REQ-TTL-5) [manual-visual] — Given a tab
  overridden to "api server" whose shell later set its title to
  "vim", When the user reopens the prompt, clears the text, and presses
  Enter, Then the label shows "vim" (the latest shell title, not the
  pre-override one).
- **AC-TTL-4** (REQ-TTL-4) [manual-visual] — Given the prompt open with
  edited text, When the user presses Esc, Then the label is unchanged
  and the terminal receives keystrokes again.
- **AC-TTL-5** (REQ-TTL-5) [unit] — Given the pure title-resolution
  helper with `override=Some("x")`, When evaluated against any
  `Terminal.title` value (including empty), Then it returns "x"; and
  with `override=None` it returns the existing shell-title/fallback
  result.
- **AC-TTL-6** (REQ-TTL-6) [manual-visual] — Given the prompt open,
  When the user composes "日本語" via IME and commits, Then the preedit
  is visible during composition and the committed text lands in the
  buffer.
- **AC-TTL-7** (REQ-TTL-7) [manual-visual] — Given two tabs A and B,
  When A is overridden to "logs", Then B's label still tracks B's shell
  title; and switching focus between A's split panes leaves A's label
  "logs".
- **AC-TTL-8** (REQ-TTL-8) [manual-visual] — Given the command palette
  and the macOS menu, When inspected, Then both expose "Set Tab
  Title…", and a user keybind bound to the action opens the prompt;
  pressing no default chord opens it.
- **AC-TTL-9** (REQ-TTL-9) [unit] — Given a Ghostty config line
  `keybind = cmd+i=prompt_surface_title`, When imported, Then the
  resulting keymap binds cmd+i to `PromptTabTitle`.
- **AC-TTL-10** (REQ-TTL-10) [integration] — Given a session state with
  one overridden tab ("api server", CJK/emoji variants included), When
  serialized and re-parsed, Then the override round-trips; and a
  session payload predating the field parses with `override=None`.
- **AC-TTL-11** (REQ-TTL-10) [manual-visual] — Given a tab overridden
  to "api server", When the app quits and relaunches with session
  restore, Then the restored tab's label is "api server" and clearing
  it reverts to the live shell title.
- **AC-TTL-12** (REQ-TTL-NF-3) [integration] — Given the feature
  landed, When `cargo test --workspace` and `cargo clippy --workspace`
  run, Then both are green and existing title tests pass unchanged.
- **AC-TTL-13** (REQ-TTL-NF-4) [unit] — Given the modal-input routing
  with `TabTitlePrompt` active, When key/IME events arrive, Then the
  routing resolves to the prompt and never to the terminal writer.
- **AC-TTL-14** (REQ-TTL-11) [unit] — Given the pure card-line
  formatter, When evaluated with (no rename, tab title), (rename, tab
  title), and (no rename, no tab title), Then the name resolves to the
  tab title, the rename, and the shell name respectively.
- **AC-TTL-15** (REQ-TTL-11) [manual-visual] — Given a tab overridden
  to "api server" whose sidebar card shows a shell-driven name, When
  the sidebar is inspected, Then the card's name row reads "api
  server"; and clearing the tab title reverts it to the shell name.

## Open Questions / Deferred Decisions

(resolved at LOCK, 2026-07-08)

- Menu placement: implementer's choice following macos-app-menus.md
  conventions — next to the existing tab items.
- Prompt hint line ("empty clears the title"): **yes**, matching
  existing overlay hint styling.
- Native tab context-menu "Change Title…" (right-click on tab label):
  **rejected** (re-examined 2026-07-08) — AppKit builds the tab strip's
  menu internally with no public extension hook; winit exposes nothing.
  Instead, "Set Tab Title…" was added to the pane right-click context
  menu (the split context menu), matching where Ghostty surfaces
  "Change Title" (user-confirmed 2026-07-08).

## Implementation record (2026-07-08)

Landed on main in one pass; `cargo test --workspace` (pty excluded per
sandbox constraint), `cargo clippy --workspace`, and `cargo fmt --check`
green. Touch points, matching L2:

- `WindowState.title_override` + `resolved_tab_title()` pure helper
  (app/helpers/geometry.rs) at the title choke point in `App::redraw`.
- `TabTitlePromptSession` + `ModalImeTarget::TabTitlePrompt`;
  key/IME/commit logic in `app/input_ops/tab_title.rs`; event-loop
  branch directly below the confirm dialog.
- `AppCommand::SetTabTitle` — palette ("Set Tab Title…", Tabs
  category), Window-menu item, pane right-click context menu (added
  2026-07-08, below a separator after the split items), menu_id
  `noa.window.set-tab-title`, action name `tab.set-title`.
- Native overlay card `sync_title_prompt`/`rebuild_title_prompt`
  (macos_overlay.rs); non-macOS falls back to the confirm-dialog card.
- `TabSession.title` (+ null/absent-tolerant parse) with capture/restore
  in `app/session_restore.rs`; commit calls `persist_session()`.
- REQ-TTL-11 (2026-07-08 amendment): `card_lines()` takes a `tab_title`
  parameter (precedence: `name_override` > tab title > shell name);
  `App::tab_title_override_for_card` resolves it from `WindowState` at
  draw-model build time (display-only, store untouched). The FR-7
  inline-rename editor seeds from the displayed name, so renaming a
  card showing a tab title starts from that title.

### Deviations recorded at implementation

- **REQ-TTL-8 (partial) / REQ-TTL-9 (deferred)**: noa has no
  user-configurable keybinds at all — `keybind =` config values are
  recognized and discarded (KEYBINDINGS.md), and the Ghostty importer
  comments out every `keybind =` line wholesale. "Bindable via the
  keybind config" and the `prompt_surface_title` import mapping are
  therefore blocked on a future user-keybind feature, not built here.
  The action name `tab.set-title` is registered so both light up when
  that lands. AC-TTL-8 narrows to palette + menu; AC-TTL-9 is waived.
- The L1 spec assumed keybind config support existed; the FRAME-stage
  exploration overstated it (the `keybind = global:...` doc comment in
  noa-config describes an equivalence, not an implemented key).
- Manual-visual ACs (AC-TTL-1..4, 6..8, 11) remain a human pass;
  AC-TTL-5/10/12 are covered by `resolved_tab_title` unit tests and the
  session round-trip/backward-compat tests. AC-TTL-13 downgrades from
  [unit] to structural: `App::modal_ime_target` needs a live `App`
  (not constructible headless), so — like the SidebarRename precedent —
  modal consumption is enforced by the shared routing pattern plus the
  manual pass, not a dedicated unit test.

## Traceability

| Requirement | Design (L2) | Test (L3) |
|---|---|---|
| REQ-TTL-1 | TabTitlePromptSession, palette/dispatch wiring | AC-TTL-1 |
| REQ-TTL-2 | title computation branch | AC-TTL-2 |
| REQ-TTL-3 | empty-commit clears override | AC-TTL-3 |
| REQ-TTL-4 | Esc path in key handling | AC-TTL-4 |
| REQ-TTL-5 | display-side mask; noa-grid untouched | AC-TTL-3, AC-TTL-5 |
| REQ-TTL-6 | ModalImeTarget::TabTitlePrompt + ime.rs | AC-TTL-6 |
| REQ-TTL-7 | override on WindowState (per tab) | AC-TTL-7 |
| REQ-TTL-8 | AppCommand 4-touch wiring, no default chord | AC-TTL-8 |
| REQ-TTL-9 | import.rs mapping | AC-TTL-9 |
| REQ-TTL-10 | TabSession.title + serialize/parse/restore | AC-TTL-10, AC-TTL-11 |
| REQ-TTL-11 | card_lines tab_title param + per-card resolution | AC-TTL-14, AC-TTL-15 |
| REQ-TTL-NF-1 | pure resolution helper | AC-TTL-5 |
| REQ-TTL-NF-2 | session round-trip | AC-TTL-10 |
| REQ-TTL-NF-3 | regression gate | AC-TTL-12 |
| REQ-TTL-NF-4 | modal input consumption | AC-TTL-13 |
