# AppleScript Integration

- slug: applescript
- status: locked (2026-07-10)
- build-path: undecided (to be selected at the post-LOCK checkpoint)
- owner: simota

## L0 — Vision

Fills the Ghostty parity gap `REQ-MACOS-003` / `IMPL-MACOS-003` (AppleScript dictionary and command bridge).
Ghostty introduced AppleScript support in 1.3.0 (treated as preview):

- Hierarchy: application → windows → tabs → terminals
- Commands: new window / new tab / split / focus / select tab / close* / input text / send key / send mouse * / perform action / new surface configuration
- Config: `macos-applescript` (default true), protected by TCC Automation permission
- Source: https://ghostty.org/docs/features/applescript

## FRAME — Reuse Scan / Constraints (frame-lens 2026-07-10)

- objc2 foundation already in place: `noa-app/Cargo.toml:32-40` (objc2 0.6 / objc2-foundation / objc2-app-kit; features still need to be added). Many existing precedents for raw msg_send! (macos_window.rs, notification.rs, etc.)
- Injection path already established: `macos_hotkey.rs:388,582` is a precedent for a callback outside winit → `EventLoopProxy::send_event(UserEvent::…)`. The AppleScript handler can use the same structure.
- Command vocabulary: `commands/command.rs:9`'s `AppCommand` (NewTab/NewWindow/CloseTab/SelectTab/NewSplit*/ToggleFullscreen, etc.) can be mapped directly to verbs.
- **Constraint 1**: window/tab creation requires `ActiveEventLoop` → not directly possible from an Apple Event callback; must always be handled via `UserEvent` injection inside `user_event` (event_loop.rs:36).
- **Constraint 2**: `input text` has no equivalent `AppCommand` today → a new `UserEvent::WriteText` is needed. The plumbing can reuse the existing `queue_pane_pty_bytes` (input_ops/terminal.rs:234).
- **Constraint 3**: winit owns the NSApp delegate → a full Cocoa Scripting object model (`NSScriptSuiteRegistry`) risks conflicting with delegate replacement. Manual `NSAppleEventManager` registration is lower risk but affects how faithfully object-model queries (`every terminal whose …`) can be reproduced.
- Bundle: Info.plist is a hand-written heredoc in bundle-macos.sh. Adding `NSAppleScriptEnabled`/`OSAScriptingDefinition` plus placing a .sdef under Resources is required. No .sdef exists in the repo yet.

## L1 — Requirements

### Functional
- **R-1 Wiring**: create a new `Noa.sdef` and place it under `Contents/Resources/`. Add `NSAppleScriptEnabled=true` and `OSAScriptingDefinition=Noa.sdef` to Info.plist in `bundle-macos.sh`. The sdef's terminology (class names/verb names/parameter names) must match Ghostty 1.3.0's spelling exactly.
- **R-2 Handler registration**: register an Apple Event handler at startup via `NSAppleEventManager.sharedAppleEventManager().setEventHandler(...)`. If config `macos-applescript` (bool, default true) is false, skip registration.
- **R-3 Creation verbs**: accept `new window` / `new tab` and map them to `AppCommand::NewWindow/NewTab`. Support optional parameters `initial working directory` (alias/POSIX path) and `command` (string).
- **R-4 Split verb**: accept `split` with a direction (`right`/`left`/`down`/`up`) and map it to `AppCommand::NewSplit*`.
- **R-5 Focus verbs**: accept `focus`(terminal) / `activate window` / `select tab`. `activate` calls `activateIgnoringOtherApps:` and brings the target window to the front.
- **R-6 Close verbs**: accept `close`(terminal) / `close tab` / `close window` and map them to the existing close paths.
- **R-7 Text input**: accept `input text` and route it through the new `UserEvent::WriteText { window_id, pane_id, text }` to `queue_pane_pty_bytes`. Behaves identically to paste (bracketed-paste wrapping applied when in bracketed paste mode, newlines sent as-is).
- **R-8 Action execution**: accept `perform action "<ghostty-action>"` and map it via a conversion table to `AppCommand`. **Closed rule: only actions enumerated in the L2 mapping table are accepted**; anything else returns the AE error `errAEEventNotHandled (-1708)`.
- **R-9 Property reads**: respond to get-type events — application: `name`/`version`/`frontmost`; window: `id`/`name`; tab: `id`/`name`/`index`/`selected`; terminal: `id`/`name`/`working directory`. Object specifiers support **only index form and id form** (`whose` filters are not supported).
- **R-10 Error responses**: events that can't be parsed, target objects that don't exist, or unsupported forms must return an AE error reply instead of being silently dropped (unsupported verb/action → `errAEEventNotHandled (-1708)`, missing target → `errAENoSuchObject (-1728)`, invalid parameter → `errAEParamMissed (-1715)` as the baseline).

### Non-functional
- **R-11 Threading discipline**: AE callbacks never touch winit objects directly. All operations go through `EventLoopProxy<UserEvent>` injection → executed in `user_event` (in `ActiveEventLoop` context). Property reads either synchronously query the main thread (holding the reply) or respond from a thread-safe snapshot of App state.
- **R-12 Permissions**: leave the TCC Automation prompt to the OS. Don't build a custom prompt or custom permission UI.
- **R-13 Verifiability**: ship an `osascript`-driven smoke-test script as `scripts/applescript-smoke.sh` (assumes real-device, manual execution; not part of CI).

## L2 — Detail (Implementation Sketch)

- New module `noa-app/src/macos_applescript.rs`: handler registration (a `Registration` struct that Box-owns the proxy — same shape as macos_hotkey.rs:388), AEDesc parsing, reply construction.
- Add `UserEvent::WriteText` (events.rs). Window/pane resolution defaults to the focused one if no id is given.
- id scheme: window id = existing `WindowGroupId`/winit WindowId stable integer; tab/terminal id = reuses the existing session_store id (no new id scheme is introduced).
- Add objc2-foundation features (NSAppleEventManager / NSAppleEventDescriptor).
- **perform action mapping table (initial set, closed)**: `new_tab`→NewTab, `new_window`→NewWindow, `new_split:right|left|up|down`→NewSplit*, `close_tab`→CloseTab, `close_window`→CloseWindow, `next_tab`→NextTab, `previous_tab`→PrevTab, `goto_tab:<n>`→SelectTab(n-1), `toggle_fullscreen`→ToggleFullscreen, `copy_to_clipboard`→Copy, `paste_from_clipboard`→Paste, `reload_config`→ReloadConfig, `quit`→Quit. Actions not in the table return -1708. Place the table next to `commands/command.rs`; share it with the keybind action-string parser if one exists.
- **Path for pane(terminal)-targeted verbs**: since `focus`(terminal) and `close`(terminal) have no AppCommand variant, introduce new `UserEvent::FocusPane { window_id, pane_id }` / `UserEvent::ClosePane { window_id, pane_id }` and connect them to the existing `split_tree` focus-move / `request_close_pane` paths (direct calls are prohibited per R-11).

## L3 — Acceptance Criteria

| ID | Maps to R | Criterion |
|---|---|---|
| AC-1 | R-1 | Opening the library in Script Editor for a built Noa.app shows Noa's dictionary (window/tab/terminal classes and all verbs) [manual] |
| AC-2 | R-2 | With `macos-applescript = false`, `new window` from osascript errors and the app doesn't react (no crash/no creation) [manual] |
| AC-3 | R-3 | `tell app "Noa" to make new window`-equivalent increases the window count. When `initial working directory` is given, the new terminal's cwd matches [manual] |
| AC-4 | R-3 | When `command` is given, the specified command runs in the new surface [manual] |
| AC-5 | R-4 | `split right/left/down/up` splits the focused terminal in the corresponding direction [manual] |
| AC-6 | R-5 | `select tab 2 of window 1` / `activate window` match the UI's tab selection / bring-to-front behavior [manual] |
| AC-7 | R-6 | `close tab` / `close window` behave with the same confirmation policy as closing via the UI [manual] |
| AC-8 | R-7 | `input text "echo hi\n"` delivers the same byte sequence to the pty as the paste path (ESC[200~/201~ wrapping applied during bracketed paste) — implemented as a unit-testable conversion function [unit] |
| AC-9 | R-8 | `perform action "toggle_fullscreen"` works; the unknown action `"nonexistent"` returns an AE error [manual] |
| AC-10 | R-9 | `working directory of terminal 1 of tab 1 of window 1` returns the real cwd. Also, application `frontmost`/`version` and tab `index`/`selected` match actual state. Resolves correctly via both index and id forms [manual] |
| AC-11 | R-10 | Sending a verb with an invalid parameter causes osascript to receive the error code specified by R-10, and the app doesn't crash [manual] |
| AC-12 | R-11 | No code inside the AE handler directly calls create_window or similar (everything goes through UserEvent) — code-review criterion [review] |
| AC-13 | R-13 | `scripts/applescript-smoke.sh` runs AC-3/5/6/9/10/15/16 in one batch and prints PASS/FAIL (input text is observed via screen/pty output after sending) [manual] |
| AC-14 | R-12 | No code generates a custom permission prompt or custom permission UI (TCC is left to the OS) — code-review criterion [review] |
| AC-15 | R-5 | `focus terminal 2 of tab 1 of window 1` moves focus to the target pane (cursor rendering and input target match) [manual] |
| AC-16 | R-6 | `close terminal 2 of ...` closes only the target pane, and the split layout re-tiles identically to closing a pane via the UI [manual] |

## Scope

**In:** all verbs/properties and config keys in R-1..R-10, non-functional requirements R-11..R-12, smoke script R-13.
**Out:** full object model via `whose` queries / `send key` / `send mouse *` / the remaining fields of `new surface configuration` (font size, env vars, initial input, wait after command) / Shortcuts App Intents / changing settings from AppleScript.

## Open Questions / Deferred Decisions

- OQ-1: revisit the full object model (Cocoa Scripting) after Ghostty 1.4's API stabilizes (equivalent to option C).
- OQ-2: `send key` / `send mouse` will get a separate spec when needed.
- OQ-3: the synchronization approach for property reads (holding the reply vs. state snapshot) will be decided during implementation — free to choose as long as it satisfies R-11.

## Assumption Ledger

- ASSUME-1 (ratified): sdef terminology matches Ghostty's exact spelling — included in the user-approved proposal at SHAPE.
- ASSUME-2 (elicited): `send key` out of scope — explicitly raised as a confirmation item at the SHAPE checkpoint and approved with "ok".
- ASSUME-3 (silent → needs confirmation): reusing the existing session/tab id scheme for ids has not been confirmed by the user (kept as an implementation detail in L2 only).

## Decision (CHALLENGE)

**Pick: A — verb-first subset** (sdef + manual NSAppleEventManager registration, UserEvent injection). The problem statement was already confirmed by the user (2026-07-10).

Considered but rejected:
- B. Full Cocoa Scripting object model — high integration risk with the winit-owned delegate + high implementation cost for NSScriptObjectSpecifier in objc2. Ghostty's own API is also unstable as a 1.3 preview.
- C. Staged hybrid — Phase 1 is identical to A. The object model is recorded in this spec's Open Questions and deferred.
- D. URL scheme / CLI socket — incompatible with the Ghostty dictionary, unsuitable for the parity goal.

## Amendment 1 — Risk Gate Decisions (2026-07-10, omen/ripple)

Implementation constraints confirmed that don't conflict with L1. Where this section conflicts with L2, this section takes precedence:

1. **OQ-3 resolved: property reads confirmed as snapshot-based** (holding the reply is prohibited). Measurement shows the AE handler is dispatched **on the main thread** while NSApp is processing events — reinterpret the R-11 discipline as "inside the AE handler, never touch winit objects directly and never block on a reply via channel recv/condvar/block_on; mutating operations go through UserEvent injection, and read operations reply synchronously from an `Arc<Mutex<AppStateSnapshot>>` (window/tab/terminal id/name/index/selected/cwd — cwd is the existing OSC7-tracked value) that the main thread keeps updated."
2. **R-3's parameterized creation gets a dedicated new `UserEvent::SpawnTab { window_target, cwd, command }`** to carry it. `AppCommand::NewTab/NewWindow` stay unit variants, unchanged (turning them into payload-carrying variants was rejected due to the blast radius across 450+ command_palette/tests). Creation without parameters still goes through AppCommand as before. The spawn path for running `command` is new plumbing (spawn_tab_with_cwd currently only carries cwd).
3. **Handler registration happens once, in winit's `resumed`** (after finishLaunching), following the same pattern as `hotkey_install_attempted` (app.rs:257). Registration's Drop removes the event handler and reclaims the Box (same shape as macos_hotkey.rs:401-528).
4. **AE four-char codes come from a single const table**, registered from it, with a unit test that cross-checks it against the sdef XML (to prevent silent no-ops). The catch-all returns at minimum -1708.
5. **WriteText convention**: window_id/pane_id are frozen at AE-resolution time (re-resolving to focused at processing time is prohibited); if the target has disappeared, the event is dropped. Bracketed-paste wrapping is applied based on the pane mode at processing time. AE input text has a size cap (matches the existing paste limit).
6. Config key: mirrors `macos_non_native_fullscreen` as the 10th touch point (note default is **true**) + adds `is_supported_scalar_key` (overrides.rs:432) so the import path picks it up too.
7. The perform action table reuses the existing `command_from_keybind_action`/`ghostty_action_alias` (commands/keybind.rs:237,244) (no AppCommand changes needed; unknown → None → -1708).

## Amendment 2 — Implementation-Time Decisions (2026-07-10, attest/judge)

- **Deviation from 1.7 (justified)**: instead of reusing the keybind parser, a dedicated closed table `command_from_applescript_action` was introduced. R-8's closed rule (L1) conflicts with reusing the keybind's broad vocabulary, so L1 takes precedence. The table contents match the L2 mapping table.
- **goto_tab correction**: L2's "`goto_tab:<n>`→SelectTab(n-1)" was a mistaken 0-based assumption. The implementation's select_tab is 1-based, so `SelectTab(n)` is correct.
- **make new window return value**: since creation is asynchronous via UserEvent, the created object is not returned synchronously (`set w to make new window` yields missing value). Accepted as a design constraint since R-3 doesn't require otherwise.
- **activate implementation**: confirmed as an explicit call to `activateIgnoringOtherApps:` plus window ordering (fixed per judge's High-severity finding).

## Quality Gate Record

Spec Quality Gate performed 2026-07-10 (spec-gate). Initial result: 2 PASS / 4 FAIL → 4 required fixes applied (closing the perform action rule, fixing error codes, adding pane-verb paths + AC-15/16, adding R-12's AC-14/expanding AC-10). All factual grounding (file:line) verified.
