# Ghostty Parity Roadmap

## Document Info

| Field | Value |
|-------|-------|
| Version | v0.1 |
| Status | Draft |
| Author | noa maintainers |
| Reviewers | noa maintainers |
| Audience | Maintainers, implementers, QA |
| Source Baseline | Ghostty public docs, retrieved 2026-07-02 |
| Related Docs | `README.md`, `docs/specs/macos-app-menus.md` |

## Change History

| Date | Version | Author | Change |
|------|---------|--------|--------|
| 2026-07-02 | v0.1 | noa maintainers | Initial Ghostty parity roadmap and checklist |

## 1. Purpose

This document converts the observed Ghostty-to-noa feature gap into an
implementation roadmap and checklist.

The roadmap is scoped to observable parity. It does not require copying Ghostty
internals. `noa` should continue to implement terminal behavior idiomatically in
Rust while preserving the dependency boundaries described in `README.md`.

## 2. Source References

- Ghostty docs: https://ghostty.org/docs
- Ghostty features: https://ghostty.org/docs/features
- Ghostty configuration reference: https://ghostty.org/docs/config/reference
- Ghostty keybinding overview: https://ghostty.org/docs/config/keybind
- Ghostty keybinding action reference:
  https://ghostty.org/docs/config/keybind/reference
- Ghostty VT reference: https://ghostty.org/docs/vt/reference
- Ghostty shell integration:
  https://ghostty.org/docs/features/shell-integration
- Ghostty SSH integration: https://ghostty.org/docs/features/ssh
- Ghostty AppleScript docs:
  https://ghostty.org/docs/features/applescript

## 3. Current noa Baseline

`noa` currently has an Increment 1 vertical slice:

- native macOS foreground app and menu baseline
- `winit` window and `wgpu` instanced-cell renderer
- PTY-backed shell with reader and writer threads
- from-scratch VT parser and stream dispatch
- basic grid, cursor, erase, tab stop, scroll region, resize, DA and DSR
- SGR 16-color, 256-color, and truecolor decoding
- OSC payload parsing with OSC 0/2 title handling
- basic keyboard input, Cmd+Q/Cmd+W app shortcuts, and Shift-key viewport
  scroll navigation
- system monospace font discovery, per-char rasterization, and glyph atlas

Notable constraints:

- The product shape is one window and one terminal session.
- There is no persisted configuration model.
- Tabs, splits, shell integration, Kitty protocols, and native polish are still
  planned work.

## 4. Goals

- `REQ-PARITY-001`: `noa` must converge toward Ghostty-compatible observable
  terminal behavior, starting with core VT and daily-use terminal features.
- `REQ-PARITY-002`: Every parity increment must include focused tests or a
  documented manual verification path.
- `REQ-PARITY-003`: Lower-level crates must remain GUI-agnostic. Only
  `noa-app` may depend on `winit`; only `noa-app` and `noa-render` may depend
  on `wgpu`.
- `REQ-PARITY-004`: Public user-facing features must be represented in docs,
  config, or menus only after backing behavior exists.

## 5. Non-Goals

- Reusing or vendoring Ghostty source code.
- Matching Ghostty internal architecture, Zig APIs, or private implementation.
- Implementing all Ghostty features in a single release.
- Adding feature placeholders that imply working behavior before implementation.

## 6. Roadmap

### Phase 1 - Terminal Compatibility Foundation

Goal: complete the terminal behavior needed by common shells, TUIs, editors, and
CLI programs.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-VT-001` | CSI edit set | Complete | `IL`, `DL`, `DCH`, `ICH`, `ECH`, `REP`, `SU`, `SD`, `CHT`, `CBT`, and `TBC` are implemented with grid coverage |
| `REQ-VT-002` | Cursor and margins | Complete | `DECSLRM`, `DECSCUSR`, keypad modes, and cursor style state are implemented with focused coverage |
| `REQ-VT-003` | Alternate screen | Complete | Primary/alternate screen switching, cursor save/restore, and resize behavior are implemented with grid coverage |
| `REQ-VT-004` | Scrollback and reflow | Complete | Scrollback storage, viewport, clear-scrollback semantics, and soft-wrap reflow are implemented with grid coverage |
| `REQ-VT-005` | Unicode cell width | Complete | Scalar cell width, zero-width non-advance, and wide-cell lead/spacer behavior are implemented; shaped grapheme clusters remain in `REQ-FONT-002` |
| `REQ-VT-006` | Bracketed paste | Complete | Mode state, paste wrapping, and bracket marker sanitization are implemented; clipboard paste action uses this encoding through `REQ-UX-002` |
| `REQ-VT-007` | OSC color/title surface | Complete | OSC 0/2 title handling plus bounded OSC 4/10/11/12 query/change/reset behavior is implemented where safe |

Exit criteria:

- `cargo test --workspace` passes.
- VT/grid tests cover each implemented sequence.
- A manual parity script can run the same byte fixtures in Ghostty and `noa`.

### Phase 2 - Interaction Basics

Goal: make `noa` usable as a normal terminal for copy, paste, selection,
scrolling, and key customization.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-UX-001` | Selection model | Partial | Selected-range storage, selection rendering, mouse drag, word selection, and line selection are implemented; selection-to-clipboard copy is tracked in `REQ-UX-002` |
| `REQ-UX-002` | Clipboard | Partial | Native copy and paste actions plus OSC 52 write policy and limits are implemented through `noa-app`; paste protection and full OSC 52 read policy remain |
| `REQ-UX-003` | Mouse reporting | Complete | SGR mouse reporting, DECSET 1000/1002/1003 + 1006 mode accessors, wheel/motion encodings, and Shift local-selection override are implemented |
| `REQ-UX-004` | Keybind engine | Complete | Config-style keybind parsing, action names, default action dispatch, and app-vs-PTY consumption rules are implemented |
| `REQ-UX-005` | Search | Partial | Search state, match computation, highlight projection, navigation, and command hooks are implemented; an interactive query prompt remains future work |
| `REQ-UX-006` | Scroll navigation | Complete | Page, line, top, and bottom viewport scrolling are implemented with app command routing and View menu entries |
| `REQ-UX-009` | IME text input | Partial | System IME commit input is enabled and sent to the PTY; inline preedit rendering remains future work |

Exit criteria:

- Edit menu items are enabled only when their backing action is implemented.
- Copy, paste, selection, search, and scroll actions have unit or integration
  tests where practical.
- Cmd shortcuts continue to avoid forwarding app commands to the PTY.

### Phase 3 - Configuration, Themes, and Fonts

Goal: support user customization without hardcoding behavior in `noa-app`.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-CONFIG-001` | Config file | Complete | Add config discovery, parsing, defaults, validation, and error reporting |
| `REQ-CONFIG-002` | Runtime reload | Missing | Define reloadable vs restart-only settings and implement reload command |
| `REQ-THEME-001` | Theme catalog | Partial | Add theme loading, built-in themes, custom themes, and light/dark selection |
| `REQ-FONT-001` | Font fallback | Partial | Add fallback families, emoji fallback, and codepoint mapping |
| `REQ-FONT-002` | Font shaping | Partial | Replace per-char rasterization with shaped runs for ligatures and grapheme clusters |
| `REQ-FONT-003` | Font options | Missing | Add font feature, variation, synthetic style, and metric adjustment config |

Exit criteria:

- Invalid config produces actionable diagnostics without panicking.
- Theme and font settings are covered by parser tests.
- Ligature and emoji fixtures render without cursor desync.

### Phase 4 - Windows, Tabs, Splits, and Session Model

Goal: move beyond one terminal surface while preserving crate boundaries.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-SURFACE-001` | Surface model | Missing | Introduce windows, tabs, splits, and focused terminal identity |
| `REQ-SURFACE-002` | Split tree | Missing | Add split creation, focus movement, resize, equalize, and zoom |
| `REQ-SURFACE-003` | Tab management | Missing | Add new/close/move/goto tab and tab title override |
| `REQ-SURFACE-004` | Window management | Partial | Add new window, close window policy, fullscreen, visibility, and restoration hooks |
| `REQ-SURFACE-005` | Undo/redo surface actions | Missing | Add bounded undo/redo for recently closed windows, tabs, and splits |

Exit criteria:

- Each surface owns an isolated PTY, terminal state, renderer snapshot, and
  command routing path.
- Surface lifecycle tests verify close and focus behavior.
- macOS menu items reflect implemented surface actions.

### Phase 5 - Modern Terminal Protocols

Goal: support modern applications that rely on terminal protocols beyond xterm
basics.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-PROTO-001` | OSC 8 hyperlinks | Partial | Hyperlink range state is stored; hover/activation affordances and copy/open actions remain |
| `REQ-PROTO-002` | OSC 7 cwd | Partial | Per-terminal cwd tracking is implemented; title, new surface inheritance, and AppleScript integration remain |
| `REQ-PROTO-003` | OSC 9 notifications | Missing | Implement notification and progress policy hooks |
| `REQ-PROTO-004` | OSC 52 clipboard | Partial | OSC 52 write policy, base64 decode, decoded-size limit, and read-deny default are implemented; full read/query policy remains |
| `REQ-PROTO-005` | Kitty graphics | Missing | Implement image storage, placement, rendering, limits, and cleanup |
| `REQ-PROTO-006` | Kitty keyboard | Missing | Implement enhanced keyboard protocol negotiation and encoding |
| `REQ-PROTO-007` | DCS passthrough | Complete | Bounded DCS dispatch plus selected `DECRQSS`, `XTGETTCAP`, `XTVERSION`, and `DECRQM` responses are implemented |
| `REQ-PROTO-008` | Synchronized rendering | Complete | DECSET 2026 synchronized output state and redraw suppression/release behavior are implemented |

Exit criteria:

- Protocol state is bounded and cannot grow unbounded from untrusted PTY input.
- Protocol-specific tests cover malformed and oversized payloads.
- Image protocol limits are configurable.

### Phase 6 - Shell, Remote, and Native Integration

Goal: reach Ghostty-like daily workflow integration on macOS-first targets.

| ID | Capability | Current State | Target |
|----|------------|---------------|--------|
| `REQ-SHELL-001` | Shell integration | Missing | Add bash, fish, zsh, nushell, and elvish integration scripts or injection plan |
| `REQ-SHELL-002` | Prompt marks | Missing | Add prompt boundary tracking for jump-to-prompt, prompt selection, and resize behavior |
| `REQ-SHELL-003` | SSH helper | Missing | Add terminfo/environment strategy for remote hosts |
| `REQ-MACOS-001` | Quick Terminal | Done | Toggle, top-edge positioning, sizing, animation, autohide, and global keybind integration are implemented |
| `REQ-MACOS-002` | Command palette | Missing | Add action registry UI and searchable execution surface |
| `REQ-MACOS-003` | AppleScript | Missing | Expose windows, tabs, terminals, input, focus, split, and action commands |
| `REQ-MACOS-004` | Secure input | Missing | Add secure keyboard entry policy and indication |
| `REQ-MACOS-005` | Background opacity/blur | Planned | Add opacity toggle, blur policy, and renderer/window integration |
| `REQ-MACOS-006` | Quick Look and proxy icon | Missing | Add macOS-specific affordances where feasible |

Exit criteria:

- Native features degrade gracefully when platform APIs are unavailable.
- AppleScript automation is covered by scriptable smoke checks.
- Global shortcuts request permissions explicitly and fail safely.

## 7. Implementation Checklist

### Phase 1 Checklist

- [x] `IMPL-VT-001`: Add CSI edit operations in `noa-vt` dispatch and
  `noa-grid` screen mutation. References `REQ-VT-001`.
- [x] `IMPL-VT-002`: Add tests for insert/delete chars and lines, scroll up/down,
  repeat char, erase char, and tab clearing. References `REQ-VT-001`.
- [x] `IMPL-VT-003`: Implement alternate screen mode state and screen switching.
  References `REQ-VT-003`.
- [x] `IMPL-VT-004`: Add scrollback storage, viewport model, and soft-wrap
  reflow on resize. References `REQ-VT-004`.
- [x] `IMPL-VT-005`: Implement grapheme and wide-cell print behavior. References
  `REQ-VT-005`.
- [x] `IMPL-VT-006`: Add bracketed paste mode and paste encoding. References
  `REQ-VT-006`.
- [x] `IMPL-VT-007`: Implement safe OSC color query/change handling. References
  `REQ-VT-007`.

### Phase 2 Checklist

- [x] `IMPL-UX-001`: Add terminal selection data model and rendering.
  References `REQ-UX-001`.
- [x] `IMPL-UX-002`: Wire mouse press, drag, release, double-click, and
  triple-click behavior. References `REQ-UX-001`.
- [x] `IMPL-UX-003`: Add clipboard copy and paste actions with platform
  adapters contained in `noa-app`. References `REQ-UX-002`.
- [x] `IMPL-UX-004`: Add OSC 52 policy and limits. References `REQ-UX-002`.
- [x] `IMPL-UX-005`: Implement mouse reporting modes and encodings. References
  `REQ-UX-003`.
- [x] `IMPL-UX-006`: Add configurable keybind parser and action registry.
  References `REQ-UX-004`.
- [x] `IMPL-UX-007`: Add search state, match computation, highlighting, and
  navigation actions. References `REQ-UX-005`.
- [x] `IMPL-UX-008`: Add viewport scrolling commands. References `REQ-UX-006`.
- [x] `IMPL-UX-009`: Enable system IME commit input for Japanese text.
  References `REQ-UX-009`.

### Phase 3 Checklist

- [x] `IMPL-CONFIG-001`: Define config schema, default values, validation, and
  diagnostic format. References `REQ-CONFIG-001`.
- [x] `IMPL-CONFIG-002`: Add config file discovery and CLI override precedence.
  References `REQ-CONFIG-001`.
- [ ] `IMPL-CONFIG-003`: Add reload command and reloadability metadata.
  References `REQ-CONFIG-002`.
- [ ] `IMPL-THEME-001`: Add theme file format and built-in theme catalog.
  References `REQ-THEME-001`.
- [ ] `IMPL-FONT-001`: Add fallback font selection and codepoint mapping.
  References `REQ-FONT-001`.
- [ ] `IMPL-FONT-002`: Introduce shaped text runs and ligature-aware cursor
  mapping. References `REQ-FONT-002`.
- [ ] `IMPL-FONT-003`: Add font feature, variation, and metric options.
  References `REQ-FONT-003`.

### Phase 4 Checklist

- [ ] `IMPL-SURFACE-001`: Define window, tab, split, and terminal surface model.
  References `REQ-SURFACE-001`.
- [ ] `IMPL-SURFACE-002`: Add split tree operations and focus movement.
  References `REQ-SURFACE-002`.
- [ ] `IMPL-SURFACE-003`: Add tab lifecycle and title override. References
  `REQ-SURFACE-003`.
- [ ] `IMPL-SURFACE-004`: Add multi-window lifecycle and close policy.
  References `REQ-SURFACE-004`.
- [ ] `IMPL-SURFACE-005`: Add bounded undo/redo for surface lifecycle actions.
  References `REQ-SURFACE-005`.

### Phase 5 Checklist

- [ ] `IMPL-PROTO-001`: Complete OSC 8 hover/activation affordances and copy/open actions
  over stored hyperlink ranges.
  References `REQ-PROTO-001`.
- [x] `IMPL-PROTO-002`: Implement OSC 7 working directory tracking. References
  `REQ-PROTO-002`.
- [ ] `IMPL-PROTO-003`: Add notification and progress event hooks. References
  `REQ-PROTO-003`.
- [ ] `IMPL-PROTO-004`: Complete OSC 52 clipboard read/write policy. References
  `REQ-PROTO-004`.
- [ ] `IMPL-PROTO-005`: Add Kitty graphics parser, storage, renderer integration,
  and resource limits. References `REQ-PROTO-005`.
- [ ] `IMPL-PROTO-006`: Add Kitty keyboard negotiation and encoding. References
  `REQ-PROTO-006`.
- [x] `IMPL-PROTO-007`: Implement selected DCS query/response protocols.
  References `REQ-PROTO-007`.
- [x] `IMPL-PROTO-008`: Add synchronized rendering state and flush behavior.
  References `REQ-PROTO-008`.

### Phase 6 Checklist

- [ ] `IMPL-SHELL-001`: Add shell integration packaging and injection strategy.
  References `REQ-SHELL-001`.
- [ ] `IMPL-SHELL-002`: Track prompt marks and expose jump-to-prompt.
  References `REQ-SHELL-002`.
- [ ] `IMPL-SHELL-003`: Define remote terminfo and environment forwarding
  strategy. References `REQ-SHELL-003`.
- [x] `IMPL-MACOS-001`: Add Quick Terminal window mode and toggle action.
  References `REQ-MACOS-001`.
- [ ] `IMPL-MACOS-002`: Add command palette backed by the action registry.
  References `REQ-MACOS-002`.
- [ ] `IMPL-MACOS-003`: Add AppleScript dictionary and command bridge.
  References `REQ-MACOS-003`.
- [ ] `IMPL-MACOS-004`: Add secure keyboard entry controls. References
  `REQ-MACOS-004`.
- [ ] `IMPL-MACOS-005`: Add background opacity and blur controls. References
  `REQ-MACOS-005`.
- [ ] `IMPL-MACOS-006`: Evaluate Quick Look and proxy icon feasibility.
  References `REQ-MACOS-006`.

## 8. Acceptance Criteria

### `AC-PARITY-001` - Implemented features are testable

```gherkin
Scenario: A roadmap item is marked complete
  Given an implementation checklist item is checked
  When a reviewer inspects the pull request
  Then the pull request links the requirement ID
  And the pull request includes an automated test or documented manual parity check
```

### `AC-PARITY-002` - Unsupported menu actions stay disabled

```gherkin
Scenario: A user opens the native macOS menu
  Given a terminal action is not implemented
  When the menu item for that action is visible
  Then the item is disabled
  And selecting it cannot imply a working feature
```

### `AC-PARITY-003` - VT behavior is fixture-driven

```gherkin
Scenario: A VT sequence is added
  Given a Ghostty-supported sequence is implemented in noa
  When `cargo test --workspace` runs
  Then at least one byte-sequence fixture verifies the resulting grid, mode, or reply behavior
```

### `AC-PARITY-004` - Protocol state is bounded

```gherkin
Scenario: A protocol accepts PTY-provided payloads
  Given the payload is malformed or oversized
  When noa parses the payload
  Then noa rejects or truncates it safely
  And memory usage remains bounded by a documented limit
```

## 9. Verification Commands

Run these before marking a roadmap item complete:

```bash
cargo fmt --all
cargo test --workspace
```

For visual or platform-native items, also run a focused manual parity check
against Ghostty and record the command, fixture, viewport size, macOS version,
and result in the pull request.

## 10. Traceability Matrix

| Roadmap Area | Requirements | Implementation Items | Primary Crates |
|--------------|--------------|----------------------|----------------|
| VT foundation | `REQ-VT-*` | `IMPL-VT-*` | `noa-vt`, `noa-grid`, `noa-app` |
| Interaction basics | `REQ-UX-*` | `IMPL-UX-*` | `noa-app`, `noa-grid`, `noa-render` |
| Config/themes/fonts | `REQ-CONFIG-*`, `REQ-THEME-*`, `REQ-FONT-*` | `IMPL-CONFIG-*`, `IMPL-THEME-*`, `IMPL-FONT-*` | `noa-app`, `noa-render`, `noa-font` |
| Surfaces | `REQ-SURFACE-*` | `IMPL-SURFACE-*` | `noa-app`, `noa-pty`, `noa-grid`, `noa-render` |
| Protocols | `REQ-PROTO-*` | `IMPL-PROTO-*` | `noa-vt`, `noa-grid`, `noa-app`, `noa-render` |
| Shell and macOS | `REQ-SHELL-*`, `REQ-MACOS-*` | `IMPL-SHELL-*`, `IMPL-MACOS-*` | `noa-app`, `scripts`, `assets` |

## 11. Open Questions

- Should `noa` use `xterm-ghostty`, `xterm-256color`, or a future `noa`
  terminfo entry as the default `TERM`?
- Which Ghostty configuration options should be treated as required parity
  versus intentionally unsupported?
- Should shell integration scripts be Ghostty-compatible, noa-specific, or a
  compatibility layer with explicit naming?
- What visual parity threshold should be required before adding built-in themes?
- Which macOS-native features require AppKit-level integration beyond `winit`
  and the current menu support?

## 12. Review Checklist

- [ ] Each checked item links to a requirement ID.
- [ ] Each requirement has at least one acceptance or verification path.
- [ ] Dependency boundaries remain intact.
- [ ] User-visible docs do not claim unimplemented features.
- [ ] Security-sensitive features, especially clipboard, OSC payloads, shell
  injection, AppleScript, and secure input, have explicit policy checks.
