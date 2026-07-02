# macOS Meta-Key Commands

## Metadata

- slug: macos-meta-key-commands
- feature title: macOS Meta-Key Commands
- status: locked
- owner: noa maintainers
- current phase: LOCKED
- build-path decision: orbit (engine: codex)

## L0 - Vision

### Problem

`noa` の macOS ユーザーは、`Cmd+K` で端末をクリアするような標準的なメタキー操作を、PTY に誤って文字入力として送らず、アプリ層の明示的なコマンドとして使いたい。既存の `AppCommand` / `KeybindEngine` / macOS menu 経路を拡張し、フォーカス中タブだけに作用し、画面・スクロールバック・検索・選択・リセットの意味を混同しない仕様にする。

### Audience

- macOS で `noa` を日常利用する terminal ユーザー。
- `noa-app` のアプリコマンド、キーバインド、メニュー、端末状態操作を拡張する contributor。

### Job To Be Done

When `noa` is focused on macOS, users should be able to invoke familiar Command-key terminal actions such as clear, copy, paste, search, and tab navigation without leaking those shortcuts into the shell as input bytes.

### Success Definition

`Cmd+K` などの標準ショートカットがテスト可能な `AppCommand` として定義され、メニュー・キーバインド・端末状態更新が同じルートで一貫して動く。デフォルトの `Cmd+K` は PTY へ `Ctrl+L` を送らず、アプリ側でフォーカス中タブの表示と scrollback をクリアする。

## Reuse And Constraints

- `AppCommand` は app-level command の中心として既に存在し、menu ID、action name、keybind との往復マッピングを持つ。
- `KeybindEngine` は `cmd`, `command`, `super`, `meta` を同じ modifier として parse できる。
- `KeybindEngine::default()` は `Cmd+Q/T/W/1..9/C/V/F/G/Shift+G` と scrollback navigation を登録済みだが、`Cmd+K` は未登録。
- `WindowEvent::KeyboardInput` は keybind 解決後に `handle_app_command` へ合流し、未登録の `Cmd` combo は PTY に送らず握りつぶす。
- `macos_menu.rs` は `muda` で native menu を構築し、menu selection を `UserEvent::AppCommand` として同じ command handler へ流せる。
- `Terminal` は selection/search/viewport scroll の public API を持つが、app command から直接呼べる clear screen / clear scrollback / reset terminal API はまだない。
- VT/grid には `CSI 3 J` 相当の scrollback erase と `Terminal::full_reset` の内部処理があるが、どちらも app-facing command としては露出していない。
- 既存 `macOS App Menus` spec では clear/reset/font-size などの terminal actions は backing behavior ができるまで deferred とされていた。本 spec はその backing behavior を定義する。
- `keybind` config key は Ghostty config spec で list 型として認識されるが、v1 では値を保持せず warn する対象であり、runtime configurable keybind は本 spec の前提にしない。

## Candidate Directions

- **A. Focused Cmd+K Clear** — v1 は `Cmd+K` だけを追加する。`AppCommand::ClearTerminal` のような単一 command を作り、フォーカス中タブの visible screen と scrollback を app 側で消す。menu item は `Edit` または `View` に `Clear` として追加する。最小で早いが、「など一般的な機能」の広がりは後続 spec に回る。
- **B. Clear / Reset Command Suite** — `Cmd+K` を中心に、clear screen + scrollback、clear scrollback only、reset terminal を action として分ける。default shortcut は `Cmd+K` のみ、他は menu item から開始して accidental destructive shortcut を避ける。既存の deferred terminal actions を解禁する最初のまとまりとして自然だが、reset の意味論を明確にする必要がある。
- **C. Common macOS Terminal Commands Pack** — `Cmd+K` clear に加えて、`Cmd+A` select all、`Cmd++` / `Cmd+-` / `Cmd+0` font-size controls、既存 `Cmd+F/G/Shift+G` search route の整理まで含める。ユーザー体験は最もまとまるが、Terminal API、font grid / atlas / renderer resize、search UI placeholder など複数領域に広がる。
- **D. Configurable Keybind Foundation First** — `keybind = ...` config の保持・読み込み・action registry 連携を先に作り、`Cmd+K` は default keybind の一例にする。Ghostty config 資産との整合は強いが、ユーザーが今欲しい clear behavior より基盤作りが先行し、scope が大きい。
- **E. Shell-Compatible Clear** — `Cmd+K` で app state を直接触らず、PTY に `Ctrl+L` 相当を送る。shell/editor の挙動に任せられるため実装は軽いが、scrollback は消えず、full-screen app で意味が変わり、現在の「Cmd combo は shell input にしない」設計とも衝突する。

Selected direction: **C. Common macOS Terminal Commands Pack**.

Checkpoint result: ユーザー裁定により、`Cmd+K` clear だけでなく、`Cmd+A` select all、`Cmd++` / `Cmd+-` / `Cmd+0` font-size controls、既存 search route の整理まで含める。

Convergence decision: C は仕様対象として採用する。ただし実装は単一の大きな変更ではなく、clear command、select all、font-size controls、search route cleanup の独立したスライスに分ける。検索プロンプトの新規 UI と runtime configurable keybind は本 spec では扱わない。

## Challenge Notes

- Magi: **conditional approve**. C は macOS terminal としての期待値に合うが、完了条件を `Cmd+K`、`Cmd+A`、`Cmd++` / `Cmd+-` / `Cmd+0`、search route cleanup に固定する必要がある。font-size は grid/window resize と絡むため受け入れ条件を明文化する。
- Void: **simplify pressure**. 最小価値は `Cmd+K` clear と search placeholder cleanup。`Cmd+A` と font-size は保持コストが高いので、C に含める場合も独立スライスとして扱い、runtime configurable keybind は除外する。
- Ripple: **medium-high impact**. C 全体の推定影響は 8-10 files / 250-450 LOC 程度。最大リスクは font-size controls で、`FontGrid`、renderer atlas、grid resize、PTY winsize、複数タブ共有挙動に波及する。分割実装が必須。
- Nexus ruling: ユーザー指定に従って C まで含める。リスクは「除外」ではなく「仕様内の段階化」と「AC 分離」で管理する。

## Shape Proposal

### Feature

Common macOS Terminal Commands Pack.

### Target Persona

macOS で `noa` を日常利用し、Terminal.app / iTerm2 / Ghostty に近い Command-key 操作を期待する terminal user。

### Proposed Solution

既存の `AppCommand` / `KeybindEngine` / native menu / `handle_app_command` 経路を拡張し、標準的な macOS terminal 操作を PTY input ではなく app command として扱う。各 command はフォーカス中タブを対象にし、端末状態の変更は `noa-grid` の public API を通して行う。

### In Scope

- `Cmd+K` を default keybind として追加し、フォーカス中タブの main screen 表示と scrollback を app 側で clear する。
- `Clear Scrollback` を menu-only command として追加する。`Cmd+K` は clear screen + scrollback、menu-only clear scrollback は scrollback だけを消す。
- `Cmd+A` を terminal select all として追加する。main screen では scrollback + live grid 全体、alt screen では visible alt screen のみを対象にする。
- `Cmd++` / `Cmd+-` / `Cmd+0` を font-size increase/decrease/reset として追加する。v1 は shared `GpuState.font` に合わせて app/session-wide に作用する。
- `Cmd+F` / `Cmd+G` / `Cmd+Shift+G` の search command route を整理し、未実装 UI が有効 command として見える状態を避ける。
- Native menu selection と keybind は同じ `AppCommand` に合流する。

### Out Of Scope

- Runtime configurable keybind (`keybind = ...`) の保持・解釈・UI。
- 新しい検索プロンプト UI または検索入力 overlay。
- `Cmd+K` で PTY に `Ctrl+L` を送る shell-compatible clear。
- `Reset Terminal` の実装。破壊的で意味論が広いため、別 spec へ延期する。
- Per-tab font-size state。v1 は app/session-wide font size に限定する。
- Cross-platform shortcut parity。macOS-first の Command-key 仕様に限定する。

### Priority And Sequencing

ICE は実装順を決めるための相対スコアであり、スコアが低い項目も C の in-scope から外さない。

| Slice | Impact | Confidence | Ease | ICE | Sequencing |
|-------|-------:|-----------:|-----:|----:|------------|
| `Cmd+K` clear + clear scrollback API | 9 | 9 | 7 | 567 | 1 |
| Search route cleanup | 6 | 8 | 8 | 384 | 2 |
| `Cmd+A` select all | 6 | 6 | 5 | 180 | 3 |
| Font-size controls | 7 | 5 | 3 | 105 | 4 |

### Hypothesis

If common macOS terminal commands are represented as app commands instead of PTY input, daily macOS users will experience fewer broken shortcut moments and contributors will be able to add terminal actions through one testable command path.

### Fail Condition

The spec should be reconsidered if implementation requires a new keybinding configuration system, per-tab font state, or search UI work before `Cmd+K` can ship; those dependencies would mean the pack has exceeded its intended boundary.

### Assumptions

- All commands target the focused tab; no command broadcasts terminal-state changes to unfocused tabs.
- Unknown `Cmd` combos remain swallowed rather than sent to the PTY.
- Font-size changes recompute grid dimensions and PTY winsize for affected windows after rebuilding font metrics.
- Selection/search state may be cleared by destructive terminal state operations where needed to avoid stale coordinates.

## L1 - Requirements

Scope mode: **Standard**. This feature has 7 functional requirements and 3 cross-functional requirements, touches app/grid/render surfaces, and needs traceable acceptance criteria, but does not introduce a new data model, external integration, or persistent configuration surface.

### User Stories

#### US-001: Clear focused terminal state

As a macOS terminal user, I want `Cmd+K` to clear the focused terminal tab without sending bytes to the shell, so that I can reset my visible workspace predictably.

Linked requirements: REQ-001, REQ-002, REQ-003, REQ-007, CFR-001, CFR-003.

#### US-002: Select all terminal output

As a macOS terminal user, I want `Cmd+A` to select terminal output in the focused tab, so that I can copy a complete terminal transcript without manual drag selection.

Linked requirements: REQ-001, REQ-002, REQ-004, REQ-007, CFR-001.

#### US-003: Adjust terminal font size at runtime

As a macOS terminal user, I want `Cmd++`, `Cmd+-`, and `Cmd+0` to adjust the terminal font size, so that I can quickly adapt readability during a session.

Linked requirements: REQ-001, REQ-002, REQ-005, REQ-007, CFR-001, CFR-002.

#### US-004: Use search shortcuts predictably

As a macOS terminal user, I want `Cmd+F`, `Cmd+G`, and `Cmd+Shift+G` to have consistent search routing, so that enabled shortcuts never appear to work while silently doing nothing.

Linked requirements: REQ-001, REQ-002, REQ-006, REQ-007, CFR-001, CFR-003.

#### US-005: Extend terminal commands through one path

As a `noa-app` contributor, I want keybinds, menu actions, and command handling to share one `AppCommand` route, so that new terminal actions remain testable and do not fork behavior.

Linked requirements: REQ-001, REQ-007, CFR-001, CFR-003.

### Functional Requirements

#### REQ-001: App command coverage for terminal meta-key actions

- **Description:** Add explicit `AppCommand` representations for clear, select all, font-size controls, and search route cleanup where those commands do not already exist.
- **Input:** Native menu events or `KeybindEngine` matches for the supported command set.
- **Output:** A resolved `AppCommand` dispatched through `handle_app_command`.
- **Constraints:** Commands must not be implemented as ad hoc keyboard special cases outside the command engine.
- **Priority:** Must.
- **Linked:** US-001, US-002, US-003, US-004, US-005.

#### REQ-002: Focused-tab targeting and PTY isolation

- **Description:** Every terminal meta-key command acts only on the focused tab and must not emit shortcut bytes to the PTY.
- **Input:** A supported `Cmd` shortcut while a terminal window is focused.
- **Output:** The focused tab changes state, or the command no-ops when no focused terminal exists.
- **Constraints:** Existing unknown `Cmd` combo behavior remains PTY-isolated.
- **Priority:** Must.
- **Linked:** US-001, US-002, US-003, US-004.

#### REQ-003: Clear terminal commands

- **Description:** Provide a default `Cmd+K` command that clears the focused tab's active display and, when the primary screen is active, its scrollback; also provide a menu-only `Clear Scrollback` command that clears primary scrollback without clearing the live screen.
- **Input:** `Cmd+K`, menu `Clear`, or menu `Clear Scrollback`.
- **Output:** Terminal state is cleared according to the command variant; stale selection/search/viewport state is reconciled.
- **Constraints:** `Cmd+K` must not send `Ctrl+L` or any other bytes to the PTY.
- **Priority:** Must.
- **Linked:** US-001.

#### REQ-004: Terminal select all

- **Description:** Provide `Cmd+A` terminal select-all behavior for the focused tab.
- **Input:** `Cmd+A` or menu `Select All`.
- **Output:** Main screen selects scrollback + live grid content; alt screen selects the visible alt-screen content only.
- **Constraints:** Selection must feed the existing copy path and avoid selecting evicted scrollback rows.
- **Priority:** Should.
- **Linked:** US-002.

#### REQ-005: Runtime font-size controls

- **Description:** Provide `Cmd++`, `Cmd+-`, and `Cmd+0` commands for font-size increase, decrease, and reset.
- **Input:** Keyboard shortcuts or matching menu commands.
- **Output:** App/session-wide font size updates, font metrics rebuild, grid dimensions recompute, affected renderers redraw, and PTY winsize is synchronized.
- **Constraints:** v1 does not persist per-tab font size and does not add config keys.
- **Priority:** Should.
- **Linked:** US-003.

#### REQ-006: Search command route cleanup

- **Description:** Ensure `Cmd+F`, `Cmd+G`, and `Cmd+Shift+G` resolve to honest, testable behavior while the search prompt UI remains out of scope.
- **Input:** Existing search shortcuts or menu search items.
- **Output:** `Find` is disabled in the native menu and is not registered as a default keybind until a prompt exists; find-next and find-previous operate only when search state exists.
- **Constraints:** This requirement must not add a new search overlay or prompt.
- **Priority:** Should.
- **Linked:** US-004.

#### REQ-007: Unified keybind and native menu integration

- **Description:** Default keybinds, action names, menu IDs, and native menu selections use the same command identities.
- **Input:** A default shortcut, action-name lookup, or menu selection.
- **Output:** The same `AppCommand` reaches the same handler with identical behavior.
- **Constraints:** Existing copy, paste, tab, and scroll navigation bindings remain unchanged.
- **Priority:** Must.
- **Linked:** US-001, US-002, US-003, US-004, US-005.

### Cross-Functional Requirements

#### CFR-001: Testability and traceability

- **Requirement:** Every in-scope command has unit or integration coverage for command mapping and behavior, and every L1 requirement maps to at least one L3 acceptance criterion.
- **Target:** Standard-package traceability from REQ/CFR to `AC-META-*` is present before lock.
- **Priority:** Must.

#### CFR-002: Resize and renderer consistency

- **Requirement:** Runtime font-size changes preserve renderer, grid, and PTY size consistency.
- **Target:** Font-size commands trigger deterministic font metric rebuild, grid-size recompute, redraw request, and PTY winsize update without panics.
- **Priority:** Should.

#### CFR-003: Scope and safety boundaries

- **Requirement:** The implementation must preserve explicit scope boundaries for destructive or deferred behavior.
- **Target:** Runtime keybind config, terminal reset, search prompt UI, PTY `Ctrl+L` clear, and per-tab font persistence remain out of scope unless a later spec changes them.
- **Priority:** Must.

### MoSCoW Priority Matrix

All listed requirements are part of the selected C scope. The priority column indicates sequencing and release risk, not permission to drop Should items from the locked spec.

| Priority | Requirements | Reason |
|----------|--------------|--------|
| Must | REQ-001, REQ-002, REQ-003, REQ-007, CFR-001, CFR-003 | These define the common command route, focused-tab safety, `Cmd+K`, menu/keybind consistency, and scope guardrails. |
| Should | REQ-004, REQ-005, REQ-006, CFR-002 | These complete the selected command pack but should be built as separate slices because select-all, font-size, and search cleanup have distinct risk profiles. |
| Could | None | C is already the chosen feature pack; additional extras would blur scope. |
| Won't | Runtime keybind config, terminal reset, search prompt UI, PTY clear bytes, per-tab font persistence | Explicitly deferred to prevent scope creep. |

## L2 - Detail

### L2-Biz: Product Behavior

- The command pack makes existing app-level shortcut behavior more complete rather than introducing a new interaction mode.
- `Cmd+K` is the first value slice and must be shippable without select-all, font-size, or search UI work being complete.
- Every command targets the focused tab. No command in this spec changes unfocused terminal content.
- `Cmd+K` is app-side clear, not shell clear. It must behave consistently regardless of the foreground shell, editor, or full-screen program.
- Search cleanup is intentionally conservative: this spec does not promise a search prompt, only honest routing and no false completed UI.

### L2-Dev: Command Model

Extend `crates/noa-app/src/commands.rs` through the existing command model.

Suggested command shape:

```rust
pub enum AppCommand {
    // existing variants...
    Terminal(TerminalAction),
    FontSize(FontSizeAction),
}

pub enum TerminalAction {
    Clear,
    ClearScrollback,
    SelectAll,
}

pub enum FontSizeAction {
    Increase,
    Decrease,
    Reset,
}
```

Stable action names:

| Command | Action name |
|---------|-------------|
| `Terminal(Clear)` | `terminal.clear` |
| `Terminal(ClearScrollback)` | `terminal.clear-scrollback` |
| `Terminal(SelectAll)` | `terminal.select-all` |
| `FontSize(Increase)` | `font-size.increase` |
| `FontSize(Decrease)` | `font-size.decrease` |
| `FontSize(Reset)` | `font-size.reset` |

Stable menu IDs:

| Command | Menu ID |
|---------|---------|
| `Terminal(Clear)` | `noa.view.clear` |
| `Terminal(ClearScrollback)` | `noa.view.clear-scrollback` |
| `Terminal(SelectAll)` | `noa.edit.select-all` |
| `FontSize(Increase)` | `noa.view.font-size-increase` |
| `FontSize(Decrease)` | `noa.view.font-size-decrease` |
| `FontSize(Reset)` | `noa.view.font-size-reset` |

Default keybind additions:

| Trigger | Command |
|---------|---------|
| `cmd+k` | `Terminal(Clear)` |
| `cmd+a` | `Terminal(SelectAll)` |
| `cmd+=` | `FontSize(Increase)` |
| `cmd+shift+plus` | `FontSize(Increase)` |
| `cmd+-` | `FontSize(Decrease)` |
| `cmd+0` | `FontSize(Reset)` |

`KeyTrigger::parse` currently splits on `+`, so direct `cmd++` text is ambiguous. The implementation should add a `plus` key alias that matches logical `Key::Character("+")`, and keep `cmd+=` for keyboards where zoom-in is represented by `=` without Shift. Tests must cover both paths.

### L2-Dev: Native Menu Shape

Preserve the top-level menu shape from `macOS App Menus`: `noa`, `File`, `Edit`, `View`, `Window`, `Help`.

- `Edit`:
  - Enable `Select All` with `Cmd+A`.
  - Keep `Copy` / `Paste` behavior unchanged.
  - Disable `Find` and remove its accelerator until a search prompt exists.
  - Keep `Find Next`, `Find Previous`, and `Clear Search` only as honest commands over existing search state.
- `View`:
  - Add `Clear` with `Cmd+K`.
  - Add `Clear Scrollback` without a default shortcut.
  - Add `Increase Font Size`, `Decrease Font Size`, and `Reset Font Size` with the font-size shortcuts above.
  - Keep existing scroll navigation items unchanged.

`Reset Terminal` must not be enabled by this spec.

### L2-Dev: App Routing

Extend `App::handle_app_command` with two focused helper paths:

- `handle_terminal_action(TerminalAction)`:
  - resolves `focused` through the same focused-tab command target pattern used by copy/search/scroll;
  - locks only the focused tab's `Terminal`;
  - calls app-facing `Terminal` APIs;
  - drops the lock before requesting redraw.
- `handle_font_size_action(FontSizeAction)`:
  - updates app/session-wide point size;
  - rebuilds the shared `FontGrid`;
  - recomputes each window's `GridSize` using existing `grid_size_for_physical_size`;
  - calls `resize_grid` so PTY winsize remains synchronized;
  - requests redraw for affected windows.

Unsupported or unavailable command targets no-op. They must not panic and must not write to `pty_input_tx`.

### L2-Dev: Terminal / Grid APIs

Add app-facing APIs to `noa-grid` instead of driving behavior through VT byte sequences.

Suggested APIs:

```rust
impl Terminal {
    pub fn clear_active_display_and_scrollback(&mut self);
    pub fn clear_scrollback(&mut self);
    pub fn select_all(&mut self);
}
```

Clear semantics:

- When primary screen is active, `clear_active_display_and_scrollback` blanks the live grid, clears primary scrollback, sets viewport offset to live output, and clears selection/search state.
- When alternate screen is active, `clear_active_display_and_scrollback` blanks only the alternate visible grid, clears alternate selection/search state, and does not mutate primary scrollback.
- `clear_scrollback` always clears primary scrollback and resets primary viewport offset. If the current selection/search result may point into removed rows, clear selection/search.
- Clear commands do not reset terminal modes, cursor-key mode, bracketed paste mode, title, dynamic colors, pending report writes, or pending OSC 52 clipboard writes.

Select-all semantics:

- On the primary screen, `select_all` selects the full retained storage range: current scrollback plus live grid rows.
- On the alternate screen, `select_all` selects only the visible alternate screen grid.
- `selected_text()` remains the single copy payload source. Existing wrapped-row behavior, trailing-space trimming, and wide-cell spacer skipping remain authoritative.

### L2-Dev: Runtime Font Size

Runtime font-size changes are app/session-wide in v1.

- `Increase` adds `1.0` point.
- `Decrease` subtracts `1.0` point.
- `Reset` returns to the startup `AppConfig.font_size`, including values supplied by CLI or config file.
- The runtime point size is clamped to `6.0..=96.0`.
- Clamping at either boundary is a no-op except for preserving redraw consistency.
- The feature does not add, modify, or persist any config key.

Because `GpuState.font` is shared across windows/tabs, the implementation follows the existing shared-font model. If windows have different scale factors, v1 uses the focused window scale factor as the rebuild source and then recomputes all affected grids from the resulting shared metrics. Per-window font grids are out of scope.

### L2-Dev: Search Route Cleanup

Search prompt UI remains deferred. This spec only removes misleading completed behavior.

- `SearchAction::Find` may remain as an action identity for future compatibility, but the native menu item must not be enabled as a working Find UI until a prompt exists.
- The default `cmd+f` binding must be removed while the prompt is absent. Pressing `Cmd+F` therefore follows existing unknown-`Cmd` behavior: swallowed by the app layer and not sent to the PTY.
- `FindNext` and `FindPrevious` may continue to call `Terminal::search_next` / `search_previous`; when no search query exists, they no-op without creating a search query.
- `Clear Search` continues to clear existing search state.

### L2-Design: Interaction Rules

- Commands that mutate terminal state should provide no additional in-app text, overlay, or confirmation.
- Destructive-but-common commands in this spec are limited to focused-tab state and must be undo-free.
- `Clear Scrollback` has no default shortcut to avoid accidental data loss.
- `Reset Terminal` remains unavailable because its effect is broader than common meta-key cleanup.

### Dependencies And Risks

| Risk | Mitigation |
|------|------------|
| Font-size changes desynchronize renderer, grid, and PTY winsize | Require AC coverage for grid recompute, `resize_grid`, redraw request, and no panic. |
| `Cmd++` key parsing breaks because `+` is a separator | Add `plus` key alias and test `cmd+=` plus `cmd+shift+plus`. |
| Select-all semantics are ambiguous across main and alt screens | Define primary as scrollback + live grid, alt as visible alt grid only. |
| Search route cleanup silently keeps a broken Find UI | Disable `Find` in the native menu and remove the default `cmd+f` binding until prompt exists. |
| Scope expands into config, reset, or search UI | Lock explicit out-of-scope guardrails in L3. |

## L3 - Acceptance Criteria

### BDD Scenarios

#### AC-META-001: New commands have stable identities — Linked: REQ-001, REQ-007, CFR-001

**Given** the app command registry includes terminal and font-size commands
**When** each new command is converted to a menu ID and action name and parsed back
**Then** the round trip resolves to the same `AppCommand`
**And** unknown menu IDs or action names still return `None`.

Verification: unit tests in `crates/noa-app/src/commands.rs`.

#### AC-META-002: Command-key shortcuts resolve or swallow without leaking to PTY — Linked: REQ-001, REQ-002, REQ-007, CFR-001, CFR-003

**Given** a focused terminal window and the default `KeybindEngine`
**When** the user presses `Cmd+K`, `Cmd+A`, `Cmd+=`, `Cmd+Shift+Plus`, `Cmd+-`, or `Cmd+0`
**Then** the shortcut resolves to the expected `AppCommand`
**And** no encoded key bytes are sent to the PTY for those commands
**And** an unsupported `Cmd` shortcut is swallowed without sending bytes to the PTY.

Verification: command resolution unit tests plus app routing test or focused manual smoke test.

#### AC-META-003: Cmd+K clears primary display and scrollback — Linked: REQ-003, CFR-001, CFR-003

**Given** the focused tab is on the primary screen with visible content, scrollback rows, a non-zero viewport offset, selection, and search state
**When** `Terminal(Clear)` is invoked through `Cmd+K` or the `Clear` menu item
**Then** the live grid is blanked
**And** primary scrollback length becomes `0`
**And** viewport offset becomes `0`
**And** stale selection and search state are cleared
**And** no bytes are written to the PTY.

Verification: `noa-grid` unit tests plus app command routing test.

#### AC-META-004: Clear Scrollback preserves live screen — Linked: REQ-003, CFR-001

**Given** the focused tab has primary live grid content and primary scrollback rows
**When** `Terminal(ClearScrollback)` is invoked from the menu
**Then** primary scrollback length becomes `0`
**And** the live grid content remains visible
**And** viewport offset becomes `0`
**And** any selection/search state that referenced removed rows is cleared.

Verification: `noa-grid` unit tests.

#### AC-META-005: Clear is safe on alternate screen — Linked: REQ-003, CFR-003

**Given** the focused tab is on the alternate screen and the primary screen has retained scrollback
**When** `Terminal(Clear)` is invoked
**Then** the alternate visible grid is blanked
**And** primary scrollback is not modified
**And** terminal modes, title, dynamic colors, pending report writes, and pending clipboard writes are not reset.

Verification: `noa-grid` unit tests.

#### AC-META-006: Cmd+A selects primary scrollback plus live grid — Linked: REQ-004, CFR-001

**Given** the focused tab is on the primary screen with retained scrollback and live grid text
**When** `Terminal(SelectAll)` is invoked through `Cmd+A` or the `Select All` menu item
**Then** the active selection spans the retained scrollback and live grid rows
**And** `selected_text()` returns the same payload that `Copy` would place on the clipboard
**And** existing wrapped-row, trailing-space, and wide-cell behavior remains unchanged.

Verification: `noa-grid` selection unit tests and copy-path smoke test.

#### AC-META-007: Cmd+A selects only the alternate screen when active — Linked: REQ-004, CFR-001

**Given** the focused tab is on the alternate screen and primary scrollback exists
**When** `Terminal(SelectAll)` is invoked
**Then** the selection spans only the visible alternate-screen grid
**And** primary scrollback text is not included in `selected_text()`.

Verification: `noa-grid` selection unit tests.

#### AC-META-008: Font-size commands update runtime point size — Linked: REQ-005, CFR-002

**Given** the app started with `AppConfig.font_size = 15.0`
**When** `FontSize(Increase)`, `FontSize(Decrease)`, and `FontSize(Reset)` are invoked
**Then** the runtime font size changes by `1.0` point per increase/decrease
**And** the value is clamped to `6.0..=96.0`
**And** reset returns to `15.0`
**And** no config file or CLI setting is modified.

Verification: app helper unit tests.

#### AC-META-009: Font-size changes synchronize render and PTY sizing — Linked: REQ-005, CFR-002

**Given** at least one window is open with a renderer, terminal grid, and PTY resize channel
**When** a font-size command changes the runtime point size
**Then** the shared `FontGrid` is rebuilt
**And** each affected window recomputes `GridSize` from current physical size and new metrics
**And** each affected terminal is resized before PTY winsize is sent
**And** redraw is requested
**And** the app does not panic if no GPU or no focused window is available.

Verification: app unit tests for pure helpers, integration smoke where practical, and manual macOS smoke test.

#### AC-META-010: Search route cleanup does not expose fake Find UI — Linked: REQ-006, CFR-003

**Given** the search prompt UI is not implemented
**When** the user inspects the native menu and uses search shortcuts
**Then** `Find` is disabled in the native menu and has no accelerator
**And** `Cmd+F` is not registered as a default keybind and is not sent to the PTY
**And** `Find Next` / `Find Previous` only operate on existing search state
**And** invoking search cleanup commands without a query does not create a query, mutate terminal content, or send PTY input.

Verification: menu construction tests or source inspection plus `Terminal` search unit tests.

#### AC-META-011: Native menu and keybind paths share behavior — Linked: REQ-007, CFR-001

**Given** a command is available from both keybind and native menu
**When** the user invokes either route
**Then** both routes dispatch the same `AppCommand` variant
**And** both routes reach the same `handle_app_command` branch
**And** copy, paste, tab, and scroll navigation commands keep their existing behavior.

Verification: command/menu ID unit tests and regression tests for existing action names.

#### AC-META-012: Scope guardrails remain intact — Linked: CFR-003

**Given** the implementation is complete for this spec
**When** the diff is reviewed
**Then** it does not add runtime `keybind` config storage or interpretation
**And** it does not enable `Reset Terminal`
**And** it does not add a search prompt UI
**And** it does not implement clear by sending `Ctrl+L` or other clear bytes to the PTY
**And** it does not persist per-tab font-size state.

Verification: source inspection and review checklist.

### Edge Case List

| Case | Input | Expected behavior | Linked |
|------|-------|-------------------|--------|
| No focused window | Any new command | No-op, no panic, no PTY write | REQ-002 |
| Active alternate screen | `Cmd+K` | Clear alt visible grid only; preserve primary scrollback | REQ-003 |
| Empty scrollback | `Clear Scrollback` | No-op except viewport/selection/search reconciliation | REQ-003 |
| Empty terminal | `Cmd+A` | Selection may remain empty; copy still no-ops | REQ-004 |
| Font at lower clamp | `Cmd+-` | Stays at `6.0`; no panic | REQ-005 |
| Font at upper clamp | `Cmd++` | Stays at `96.0`; no panic | REQ-005 |
| Missing search query | `Cmd+G` / `Cmd+Shift+G` | No-op, no query creation, no PTY write | REQ-006 |
| Existing unsupported `Cmd` combo | `Cmd+,` or equivalent | Remains swallowed; no shell input | REQ-002 |

### Traceability Matrix

| Requirement | User Story | Acceptance Criteria |
|-------------|------------|---------------------|
| REQ-001 | US-001, US-002, US-003, US-004, US-005 | AC-META-001, AC-META-002 |
| REQ-002 | US-001, US-002, US-003, US-004 | AC-META-002 |
| REQ-003 | US-001 | AC-META-003, AC-META-004, AC-META-005 |
| REQ-004 | US-002 | AC-META-006, AC-META-007 |
| REQ-005 | US-003 | AC-META-008, AC-META-009 |
| REQ-006 | US-004 | AC-META-010 |
| REQ-007 | US-001, US-002, US-003, US-004, US-005 | AC-META-001, AC-META-002, AC-META-011 |
| CFR-001 | US-001, US-002, US-003, US-004, US-005 | AC-META-001, AC-META-002, AC-META-003, AC-META-004, AC-META-006, AC-META-007, AC-META-011 |
| CFR-002 | US-003 | AC-META-008, AC-META-009 |
| CFR-003 | US-001, US-004, US-005 | AC-META-002, AC-META-005, AC-META-010, AC-META-012 |

## Scope

In scope:

- Common macOS terminal command pack with four separately verifiable slices: clear commands, select all, font-size controls, and search route cleanup.
- App command, keybind, native menu, and terminal state API changes required for those slices.
- Focused-tab targeting for every command.

Out of scope:

- Runtime configurable keybindings.
- New search prompt UI.
- Terminal reset behavior.
- Per-tab font-size persistence.
- Non-macOS shortcut mapping.

## Considered But Rejected

- **A. Focused Cmd+K Clear** — rejected as too narrow for the requested "など一般的な機能"; kept as the first implementation slice inside C.
- **B. Clear / Reset Command Suite** — rejected as the sole direction because it does not cover select-all, font-size, and search command expectations; clear command semantics remain part of C.
- **D. Configurable Keybind Foundation First** — rejected because `keybind` config is intentionally recognized-but-unimplemented in the Ghostty config spec, and would make this feature a configuration-platform project.
- **E. Shell-Compatible Clear** — rejected because sending `Ctrl+L` to the PTY would not clear scrollback, would vary by foreground program, and would conflict with the current app-command handling of unknown `Cmd` combos.
- **Monolithic C implementation** — rejected because font-size and select-all have different risk profiles from `Cmd+K`; C should be specified as one user-facing pack but built as separate verifiable slices.

## Open Questions / Deferred Decisions

- No blocking open questions at SHAPE checkpoint.
- Deferred: exact behavior of a future `Reset Terminal` command.
- Deferred: runtime configurable keybind semantics.
- Deferred: per-tab font-size persistence.

## Spec Quality Gate

### Attest Extraction Check

| Check | Result |
|-------|--------|
| AC count | 12 |
| L1 requirement coverage | PASS — 7 functional requirements and 3 cross-functional requirements all map to at least one `AC-META-*`. |
| Testability | PASS — every AC has a concrete trigger, observable expected outcome, and verification method. |
| Ambiguity flags | PASS — no blocking ambiguity remains. Search `Find` behavior is fixed as disabled and unbound until prompt support exists. |
| Scenario count | PASS — 12 scenarios, within the Standard-package target. |

### Nexus Quality Gate

| Dimension | Result | Notes |
|-----------|--------|-------|
| Ambiguity | PASS | Cmd/keybind mappings, clear semantics, alt-screen behavior, select-all range, font-size clamp, and search deferral are explicit. |
| Completeness | PASS | Every in-scope slice has L1 requirements, L2 component detail, L3 ACs, edge cases, and traceability. |
| Consistency | PASS | Scope, out-of-scope guardrails, and ACs agree that runtime keybind config, reset terminal, search prompt UI, PTY clear bytes, and per-tab font persistence are excluded. |
| Testability | PASS | ACs are verifiable by unit tests, integration/smoke tests, manual macOS smoke tests, or source inspection where appropriate. |
| Scope coherence | PASS | In-scope and out-of-scope are mutually exclusive and collectively cover the selected C direction. |

Lock preconditions: satisfied for spec sign-off. The draft remains unpromoted until the user explicitly says to lock it.

## Build-Path Decision

Selected: **orbit**.

Executor engine: **codex**.

Handoff recommendation:

- Use this locked spec as the loop goal source.
- Use `docs/specs/macos-meta-key-commands.traceability.yaml` as the initial AC ledger.
- Generate an `orbit` loop contract whose external DONE gate is all 12 `AC-META-*` criteria passing or being explicitly verified by the agreed method.
- Configure the Codex executor with the latest-model mandate from Orbit's executor reference, for example:

  ```bash
  EXEC_CMD='codex exec --full-auto -m gpt-5.5 "Read goal.md and complete the task described in it"'
  ```

- Before running the loop, verify Codex CLI prerequisites: non-interactive `codex exec` works, `--full-auto` is acceptable for the workspace, and Codex agent depth is configured for autonomous loop execution.
- The spec step stops here. Runner generation/execution should be handled by the `orbit` build workflow, not by this spec document.
