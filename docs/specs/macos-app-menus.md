# macOS App Menus

## Metadata

- slug: macos-app-menus
- feature title: macOS App Menus
- status: locked
- owner: noa maintainers
- current phase: LOCKED
- build-path decision: apex

## L0 - Vision

### Problem

`noa` already starts as a foreground macOS application with a Dock icon and
winit's default menu bar, but it does not yet have an explicit app-command and
native menu design for terminal-specific actions.

### Audience

- macOS users launching `noa` from either a `.app` bundle or `cargo run`.
- Contributors extending the macOS application layer without leaking GUI
  dependencies into lower-level terminal crates.

### Job To Be Done

When `noa` runs as a macOS app, users should see and use familiar application
menus for app-level commands without those commands being forwarded to the PTY
as terminal input.

### Success Definition

The app presents a coherent macOS menu experience, routes supported menu and
Cmd-key commands through the app layer, preserves terminal input behavior, and
keeps `winit`/native macOS concerns contained in `crates/noa-app`.

## Reuse And Constraints

- Existing entry point: `noa_app::run()` builds a `winit` event loop in
  `crates/noa-app/src/lib.rs`.
- Existing macOS setup: `ActivationPolicy::Regular` and
  `with_default_menu(true)` are already enabled for macOS.
- Existing app loop: `App` implements `winit::application::ApplicationHandler`
  and owns the window, renderer, terminal, PTY writer, IO thread, and resize
  bridge.
- Existing shortcut behavior: Cmd-key combinations are intercepted in
  `crates/noa-app/src/app.rs`; `Cmd+Q` and `Cmd+W` currently exit the app and
  Cmd combinations are not forwarded to the PTY.
- Existing event bridge: `UserEvent` only models redraw and PTY exit; there is
  no menu-command event or command abstraction yet.
- Dependency boundary: only `noa-app` should use `winit`; lower crates remain
  GUI-agnostic.
- Current product shape: single window, single terminal session, no tabs,
  runtime settings UI, or persisted settings model.
- Bundle metadata is split between `scripts/bundle-macos.sh` and
  `bin/noa/Cargo.toml`; any About/menu metadata should identify the source of
  truth.

## Candidate Directions

- A. Minimal Native Baseline: keep winit's default menu and clarify existing
  foreground-app and Cmd shortcut behavior.
- B. App Command Layer: add a thin `AppCommand` layer and route both menu
  events and Cmd shortcuts through it.
- C. Terminal Essentials Menu: extend B into File/Edit/View/Window terminal
  menus, including copy/paste/reset/font-size actions where supported.
- D. Preferences-First: design a settings model and make `Preferences...` the
  primary menu entry point.
- E. Full Native Cocoa Menu: replace the default menu with a custom native
  macOS menu using Cocoa/AppKit-level integration or a native menu utility
  crate.

Selected direction: E1. Full Native Cocoa Menu via `muda`.

Rationale: `muda` provides native desktop menu primitives and can bridge menu
events into the existing `winit` application loop with less unsafe AppKit code
than direct `objc2-app-kit` integration.

## L1 - Requirements

### Functional Requirements

- `REQ-001` Native custom menu: On macOS, `noa` must install a custom native
  application menu instead of relying on winit's default menu.
- `REQ-002` App command routing: Menu selections and supported Cmd-key
  shortcuts must route through one app-command path before any terminal input
  encoding occurs.
- `REQ-003` Initial menu slice: The first custom menu must expose app-level
  commands for About, Preferences, Close Window, and Quit.
- `REQ-004` Launch parity: The menu behavior must work when launched from both
  `cargo run -p noa` and a bundled `Noa.app`.
- `REQ-005` Shortcut preservation: Cmd-key app shortcuts must not be forwarded
  to the PTY.
- `REQ-006` Standard macOS menu bar shape: The custom menu bar must expose the
  top-level menus `Noa`, `File`, `Edit`, `View`, `Window`, and `Help`.

### Cross-Functional Requirements

- `CFR-001` Dependency boundary: Native menu dependencies and `winit`/AppKit
  integration must remain inside `crates/noa-app`.
- `CFR-002` Platform isolation: Non-macOS builds must not require macOS menu
  crates or compile macOS-only modules.
- `CFR-003` Main-thread integration: Native menu initialization must happen in
  the macOS application lifecycle without racing window, renderer, or PTY
  initialization.
- `CFR-004` Maintainability: The first slice must avoid direct unsafe AppKit
  menu construction unless `muda` cannot satisfy a required behavior.

## L2 - Detail

### Product Behavior

- The menu bar should identify the app as `noa`.
- The top-level menu bar should follow standard macOS terminal-app shape:
  `noa`, `File`, `Edit`, `View`, `Window`, and `Help`.
- The first slice should include these user-visible commands:
  - `About noa`
  - `Preferences...`
  - `Close Window`
  - `Quit noa`
- `Preferences...` should be present but disabled until a settings model exists.
- `Close Window` should preserve the current single-window behavior by exiting
  the app; the label remains a compatibility bridge for future multi-window
  support.
- `Quit noa` should exit the app.
- Unsupported terminal actions such as copy, paste, clear, reset, font-size
  changes, tabs, and new window may appear only as disabled placeholders until
  the backing behavior exists.

### Development Design

- Add `muda` as a macOS-only dependency of `noa-app`, with default Linux-oriented
  features disabled:

  ```toml
  [target.'cfg(target_os = "macos")'.dependencies]
  muda = { version = "0.19", default-features = false }
  ```

- Add a macOS-only menu module inside `crates/noa-app`, for example
  `crates/noa-app/src/macos_menu.rs`.
- Replace the current explicit `with_default_menu(true)` macOS setup with a
  custom-menu path that prevents duplicate default and custom menus.
- Define an app-command enum in the app layer, for example:

  ```rust
  enum AppCommand {
      About,
      Preferences,
      CloseWindow,
      Quit,
  }
  ```

- Extend the event bridge so menu events can become app commands, for example
  by adding a `UserEvent::AppCommand(AppCommand)` variant or an equivalent
  app-layer command channel.
- Initialize the `muda` menu after the `winit` event loop exists and before
  `run_app`, using `Menu::init_for_nsapp()` on macOS.
- Use `muda::MenuEvent::set_event_handler` to forward menu item IDs through
  `EventLoopProxy<UserEvent>`.
- Keep menu item IDs stable and map them to `AppCommand` in one place.
- Add `File`, `Edit`, `View`, `Window`, and `Help` as top-level `muda`
  submenus. Only supported app commands should be enabled; unsupported actions
  should be disabled placeholders.
- Route Cmd-key handling in `App::window_event` through the same command
  handler used by menu selections.
- The command handler must return before `input::encode_key` for supported
  Cmd-key app shortcuts.

### Documentation

- Update README macOS app documentation to describe the custom native menu,
  including the intentionally disabled `Preferences...` item.
- Keep bundle metadata wording consistent with `scripts/bundle-macos.sh` and
  `bin/noa/Cargo.toml`.

## L3 - Acceptance Criteria

### `AC-MACOS-MENUS-001` - Custom native menu replaces default menu

- Linked requirements: `REQ-001`, `REQ-004`, `CFR-003`
- Priority: CRITICAL
- Testability: TESTABLE
- V&V method: INSPECTION + DEMONSTRATION

```gherkin
Scenario: SC-AC-MACOS-MENUS-001-HP-001 - Native custom menu is installed
  Given noa is running on macOS as a foreground application
  When the app finishes startup
  Then the macOS menu bar shows a noa application menu owned by the custom menu implementation
  And the app does not show a duplicate winit default app menu
```

### `AC-MACOS-MENUS-002` - Initial app-level commands are present

- Linked requirements: `REQ-003`
- Priority: HIGH
- Testability: TESTABLE
- V&V method: INSPECTION + DEMONSTRATION

```gherkin
Scenario: SC-AC-MACOS-MENUS-002-HP-001 - Initial menu commands are visible
  Given noa is running on macOS
  When the user opens the application menu
  Then the menu includes About noa
  And the menu includes Preferences...
  And the menu includes Close Window
  And the menu includes Quit noa
```

### `AC-MACOS-MENUS-003` - Preferences is explicitly non-functional

- Linked requirements: `REQ-003`
- Priority: MEDIUM
- Testability: TESTABLE
- V&V method: INSPECTION + DEMONSTRATION

```gherkin
Scenario: SC-AC-MACOS-MENUS-003-HP-001 - Preferences does not imply settings support
  Given noa has no persisted settings model
  When the user opens the application menu
  Then Preferences... is disabled
  And selecting Preferences... does not open a misleading settings interface
```

### `AC-MACOS-MENUS-004` - Menu events route through app commands

- Linked requirements: `REQ-002`, `CFR-001`, `CFR-004`
- Priority: CRITICAL
- Testability: TESTABLE
- V&V method: INSPECTION

```gherkin
Scenario: SC-AC-MACOS-MENUS-004-HP-001 - Menu selection becomes an app command
  Given the native menu has stable command item IDs
  When a supported menu item is selected
  Then the app maps that item ID to exactly one AppCommand
  And the command is handled by the winit app layer
```

### `AC-MACOS-MENUS-005` - Cmd shortcuts do not reach the PTY

- Linked requirements: `REQ-002`, `REQ-005`
- Priority: CRITICAL
- Testability: TESTABLE
- V&V method: TEST

```gherkin
Scenario: SC-AC-MACOS-MENUS-005-HP-001 - Supported Cmd shortcut is consumed by app
  Given noa is running with an active PTY
  When the user presses Cmd+Q
  Then noa handles Quit as an app command
  And no Cmd+Q bytes are written to the PTY
```

### `AC-MACOS-MENUS-006` - Close Window preserves current single-window behavior

- Linked requirements: `REQ-003`
- Priority: HIGH
- Testability: TESTABLE
- V&V method: DEMONSTRATION + TEST

```gherkin
Scenario: SC-AC-MACOS-MENUS-006-HP-001 - Close Window exits the current app session
  Given noa is running with its current single-window model
  When the user selects Close Window or presses Cmd+W
  Then noa exits the current app session
  And the behavior matches the pre-menu Cmd+W behavior
```

### `AC-MACOS-MENUS-007` - Dependency boundary is preserved

- Linked requirements: `CFR-001`, `CFR-002`
- Priority: HIGH
- Testability: TESTABLE
- V&V method: INSPECTION + TEST

```gherkin
Scenario: SC-AC-MACOS-MENUS-007-HP-001 - Menu dependencies stay in noa-app
  Given the feature has been implemented
  When the workspace dependency graph is inspected
  Then native menu dependencies are only used by noa-app
  And no lower-level crate depends on winit, muda, or AppKit menu APIs
```

### `AC-MACOS-MENUS-008` - Launch modes preserve menu behavior

- Linked requirements: `REQ-004`
- Priority: HIGH
- Testability: TESTABLE
- V&V method: DEMONSTRATION

```gherkin
Scenario: SC-AC-MACOS-MENUS-008-HP-001 - Cargo and bundle launches both show the menu
  Given the same build of Noa is available as a cargo-run binary and as Noa.app
  When Noa is launched by each supported launch method on macOS
  Then both launches show the custom native menu
  And both launches support Quit Noa through the menu
```

### `AC-MACOS-MENUS-009` - Standard top-level menus are visible

- Linked requirements: `REQ-006`
- Priority: HIGH
- Testability: TESTABLE
- V&V method: INSPECTION + DEMONSTRATION

```gherkin
Scenario: SC-AC-MACOS-MENUS-009-HP-001 - Terminal-style top-level menus are visible
  Given noa is running on macOS
  When the menu bar is visible
  Then the top-level menu bar includes noa
  And the top-level menu bar includes File
  And the top-level menu bar includes Edit
  And the top-level menu bar includes View
  And the top-level menu bar includes Window
  And the top-level menu bar includes Help
```

## Scope

### In Scope

- Custom native macOS app menu using `muda`.
- Top-level `File`, `Edit`, `View`, `Window`, and `Help` menu shells.
- macOS-only dependency wiring for menu support.
- `noa-app` command routing for menu selections and supported Cmd shortcuts.
- Initial app-level commands: About, disabled Preferences,
  Close Window, Quit.
- README update for the custom menu behavior and known limitations.

### Out Of Scope

- Multi-window, tabs, sessions, and window restoration.
- Settings persistence or a real Preferences UI.
- Clipboard integration and enabled Edit menu behavior.
- Terminal actions such as clear, reset, font-size changes, zoom, or command
  palette.
- Direct `objc2-app-kit` menu construction unless the selected `muda` approach
  proves insufficient.
- Changes to `noa-core`, `noa-vt`, `noa-grid`, `noa-font`, `noa-render`, or
  `noa-pty` for menu support.

## Considered But Rejected

- A. Minimal Native Baseline: rejected because the project already enables
  winit's default macOS menu and the requested direction is richer native menu
  control.
- B. App Command Layer only: rejected as the whole solution because it does not
  itself create a native custom menu, but its command-routing idea remains part
  of the selected design.
- C. Terminal Essentials Menu: deferred because copy/paste/reset/font-size
  actions require additional terminal semantics and clipboard/runtime settings
  design.
- D. Preferences-First: deferred because the project has no persisted settings
  model or preferences UI yet.
- E2. Direct `objc2-app-kit`/AppKit menu implementation: rejected for the first
  slice because it increases unsafe and main-thread/lifetime complexity without
  a clear need over `muda`.

## Open Questions / Deferred Decisions

- Whether `About noa` should use bundle metadata from `scripts/bundle-macos.sh`,
  `bin/noa/Cargo.toml`, Cargo package metadata, or a single new source of truth.
- Which later menu actions should be prioritized after the first slice:
  copy/paste, clear/reset terminal, font size, new window, tabs, or preferences.

## Spec Quality Gate

- Ambiguity: PASS. Remaining uncertainty is explicitly parked in Open Questions
  and does not block the first slice.
- Completeness: PASS. Every in-scope `REQ-*` and `CFR-*` has at least one linked
  `AC-MACOS-MENUS-*` criterion.
- Consistency: PASS. `Preferences...` is fixed as disabled for this slice, and
  `Close Window` is fixed to preserve current single-window exit behavior.
- Testability: PASS. Every AC is marked testable and has an inspection,
  demonstration, or test verification method.
- Scope coherence: PASS. Multi-window, preferences implementation, clipboard,
  and terminal action menus are explicitly out of scope.

## Build-path Decision

Selected: `apex`.

Rationale: the feature is bounded to `noa-app`, has a locked first-slice scope,
and carries nine traceable acceptance criteria suitable for a single sustained
implementation run.

Recommended handoff: `/nexus apex docs/specs/macos-app-menus.md`.
