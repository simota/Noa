# Agent Automation Surface Upgrade (AppleScript v2 + Accessibility)

| | |
|---|---|
| slug | agent-automation |
| status | locked (2026-10-17, owner sign-off) |
| owner | simota |
| as-of | 2026-10-17 |
| review trigger | A change to Ghostty's `macos/Ghostty.sdef`, a winit major update, or a revision of `docs/specs/applescript.md` |
| depth | standard |
| build path | one-shot autonomous build |

## L0: Vision
- Problem: To operate Noa from outside and read its output back, an agent today has to use noa-server (`noa.getText`/`sendText`/`split`, `docs/specs/noa-server.md:70-76`). noa-server is disabled by default, and using it requires starting it and issuing a token. The AppleScript that works without setup is narrower than the Ghostty 1.3 dictionary: it cannot read output and cannot send real key events (`input text` is treated as a paste). Terminal panes do not appear in the AX tree at all, so an assistant or agent that reads AX cannot see the inside of Noa. (FRAME revised 2026-10-17: rewritten after the falsification panel's findings, ratified.)
- Audience: (a) Agents that drive Noa from a shell with `osascript`, using no config and no token. (b) Assistants that read the macOS AX tree, and AX-based agent frameworks. AX adds nothing for computer-use clients that work only from screenshots and coordinate actions (Anthropic's public specification of the computer use tool). (elicited 2026-10-17, revision ratified)
- Job to be done: Without any setup, an agent identifies a Noa terminal, sends it keys or text, and reads the result back as text.
- Success definition: One osascript runs "split → input text → read contents" to completion (AC-010). An AX-reading client can get each pane's screen text and position (AC-019/020). Every term in Ghostty.sdef exists in Noa's dictionary (AC-028).
- Existing assets & constraints (Reuse scan 2026-10-17, HEAD 25577d2)
  - Existing AppleScript: `assets/Noa.sdef`, `crates/noa-app/src/macos_applescript.rs`, `crates/noa-app/src/app/applescript.rs`. The spec is `docs/specs/applescript.md` (locked). Today Noa registers only `coregetd`, `coreclos`, `miscactv` and the Noa Suite itself (`macos_applescript.rs:640-662`). Cocoa Scripting answers the other standard events (such as `count`) because of `NSAppleScriptEnabled`.
  - C1: Apple Events are dispatched on the main thread. A handler must not wait for the event loop to respond. Reads come from `Arc<Mutex<AppStateSnapshot>>` (applescript Amendment 1.1). `sync_applescript_snapshot` replaces the whole snapshot when its signature changes or 500 ms have passed.
  - C2: `PaneId` is a global `AtomicU64` at `split_tree/tree/types.rs:31`. The tab id is currently `u64::from(WindowId)`. `WindowGroupId` is numbered by `App::next_group_id` (`lifecycle.rs:729`). The existing `UserEvent::WriteText`/`RaiseWindow`/`ClosePane` carry winit's `WindowId` (`events.rs:100-125`). `UserEvent` derives `Eq`.
  - C3: The macOS implementation of winit 0.30.13 has no accessibility implementation. An NSView is ignored by AX by default, so the AX parent of a child element is the nearest ancestor that is not ignored.
  - C4: Font size is per tab (`WindowState.font_px`, `state.rs:262`).
  - C5: `PtyConfig` (`noa-pty/src/pty.rs:21`) cannot take environment variables. `Pty` exposes neither the child's pid nor its tty name. wait-after-command is not implemented.
  - C6: The io thread holds the `Terminal` lock for each chunk of up to 64 KiB (`io_thread/feed.rs:220`). Reflow on resize also runs inside the lock.
  - Reuse: `screen_text()` at `ipc_bridge.rs:498` (currently private; make it `pub(crate)`), `Terminal::scrollback_text_tail()` (`noa-grid/src/terminal.rs:427`, the tail of the whole history including the active screen), `Terminal::selected_text()`, `encode_key_with_modes` (`input/key.rs:82`), `encode_mouse_input/motion/wheel` (`mouse.rs:244-300`), `APPLESCRIPT_INPUT_TEXT_CAP` (8 MiB), `noa_ipc::DEFAULT_TEXT_MAX_BYTES` (256 KiB), `noa_ipc::truncate_tail`.
  - Conformance baseline: Ghostty `macos/Ghostty.sdef` and `macos/Sources/Features/AppleScript/*.swift` (main, fetched 2026-10-17). The parts we need are included in this document as Appendix A/B.

## Glossary
| Term | Definition | Not to be called | Code identifier |
|------|-----------|------------------|-----------------|
| window | One tab group that bundles native tabs | tab group | `WindowGroupId` |
| tab | One native NSWindow tab | window (in an AppleScript context) | `WindowState` / new `TabId` |
| terminal | One pane of a split | pane, surface (in the dictionary and in ACs) | `PaneId` |
| surface configuration | An AE record (6 fields) that gathers the settings used at creation | config, profile | `SurfaceConfiguration` |
| pending object | A window/tab/terminal that has an id assigned in advance but whose creation has not finished | — | element of `PendingRegistry` |
| contents | The text of the viewport the terminal is **showing now**. If the user has scrolled back, these are the lines at that position | screen text, buffer | `screen_text(visible_rows())` |
| scrollback | The last 256 KiB of the whole history, including the active screen | history | `scrollback_text_tail` |
| AX element | The NSAccessibility element that corresponds to one terminal | accessibility node | `TerminalAxElement` |

## L1: Requirements

### Functional (`REQ-n`)
| ID | Requirement | Priority | Source |
|----|-------------|----------|--------|
| REQ-001 | Reference properties: application `front window` (the window that the key tab belongs to; if there is no key tab, the window that was key last), window `selected tab`, tab `focused terminal`, terminal `pid` and `tty`. Add a `terminals` element to application and window. These resolve by index form, id form and every form (`terminals of window 1` returns a list). Index order: windows by first appearance in `window_order`; tabs by relative order within `window_order`; terminals by ascending `PaneId` | MUST | elicited (scope 1) |
| REQ-002 | Register the commands `close tab <tab>`, `close window <window>`, `count` (`corecnte`) and `exists` (`coredoex`) ourselves (replacing Cocoa Scripting's answer). `count windows` returns the number of windows (tab groups). The old form `close tab 2 of window 1` is a breaking change under Amendment 2 | MUST | grounded (Ghostty.sdef) + elicited (answer on the close conflict, Amendment 2) |
| REQ-003 | `new surface configuration [from <record>]` returns a record with 6 fields. `new window`, `new tab` and `split` accept `with configuration <record>`. The existing `initial working directory` / `command` parameters stay accepted | MUST | elicited (scope 1) |
| REQ-004 | The meaning of surface configuration follows L2.4. `font size` applies only to the tab created by new window/new tab, and is ignored by split | MUST | elicited (font size answer) |
| REQ-005 | When `wait after command` is true, keep the pane after the child process exits (whether or not `command` is given) and show `Process exited. Press any key to close the terminal.` The next key press (physical or `send key`) closes the pane | MUST | elicited (wait after answer) |
| REQ-006 | `new tab in <window>` adds a tab to the given window (it may be pending). When `in` is omitted, the tab is added to the live `front window` at the moment the AE is received (a pending window is not a target; if there is no live window, a new window is created). Tabs created by new window and new tab become key and selected inside Noa, the same as UI `cmd+n`/`cmd+t`. The app is not activated | MUST | elicited (scope 1) + ASSUME-6 |
| REQ-007 | `split <terminal> direction <dir> [with configuration]` splits the given terminal. The old form `split <dir>` splits the focused terminal | MUST | elicited + compatibility (REQ-013) |
| REQ-008 | A creation command returns an id-form reference to the object it created (new window→window, new tab→tab, split→terminal). The returned reference (and its elements: `selected tab of w`, `focused terminal of b`) can be resolved right away by the next Apple Event and used as a target | MUST | elicited (choice of D2) |
| REQ-009 | `perform action <s> on <terminal>`. Move focus to the target terminal, then dispatch (ASSUME-3). With `on` omitted, the focused terminal is the target. Returns `true` once dispatched | MUST | elicited (scope 1) |
| REQ-010 | `send key <name> [action press\|release] [modifiers <s>] [to <terminal>]`. Key names are in Appendix A. Translate once with the current keyboard layout and pass through the same encoder path as a physical key press (including DECCKM, kitty, modifyOtherKeys and `macos-option-as-alt`) | MUST | elicited (layout answer) + grounded |
| REQ-011 | `send mouse button`, `send mouse position` and `send mouse scroll` have the same effect as a physical event at the same position on the target terminal. The OS cursor does not move | MUST | elicited (scope 2) |
| REQ-012 | Noa Extensions suite: the terminal properties `contents`, `selection` and `scrollback` (read-only; as defined in the Glossary) | MUST | elicited (scope 3) |
| REQ-013 | Every existing verb and property in `docs/specs/applescript.md` keeps working unchanged | MUST | derived from REQ-002/003/007 |
| REQ-014 | Expose each terminal as an AX element. The attributes are as in the AX table in L2.2, all read-only | MUST | elicited (scope 4) |
| REQ-015 | The AX child list and frame are updated inside the main-thread code that applies a change of pane layout (split, close, divider move, window resize) | MUST | derived from REQ-014 |

### Cross-functional (`CFR-n`)
| ID | Requirement | Measure / criterion | Target / required outcome | Verified how | Priority | Source |
|----|-------------|---------------------|---------------------------|--------------|----------|--------|
| CFR-001 | Main-thread read cost | Time of one extraction including lock acquisition (release build). (a) `contents` of a 200×60 all-wide-character grid, no contention; (b) the same as (a), but another thread keeps feeding 10 MB into `Stream` in 64 KiB chunks; (c) 256 KiB `scrollback`, no contention | (a) p95 ≤ 1 ms, (b) p99 ≤ 8.3 ms (one frame at 120 Hz), (c) p95 ≤ 4.2 ms (half a frame). 1000 runs each | Output of `#[ignore]` measurement tests | HIGH | derived (C1, C6) |
| CFR-002 | Zero cost when nobody asks | AX and AE values are computed only inside getters, and no periodic work or thread is added / idle RSS | 0 new timers or threads (review). Idle RSS no more than +1 MiB against main | Code review + idle measurement in `bench/` | HIGH | derived (Noa's low-memory policy) |
| CFR-003 | Thread discipline | AE and AX handlers do not wait on the event loop, a channel or a condvar, and do not touch winit objects (the `Terminal` mutex is allowed within CFR-001) | 0 occurrences (review) | Code review | CRITICAL | grounded (applescript Amendment 1.1) |

## L2: Detail

### L2.1 Data model
| Entity | Field | Type | Constraints | Default | Nullable | Owned by |
|--------|-------|------|-------------|---------|----------|----------|
| `PendingRegistry` | Provisional entries for window/tab/terminal | A separate `Arc<Mutex<…>>` (apart from the snapshot) | AE handlers insert into it. `sync_applescript_snapshot` merges the pending entries behind the real ones on every run. Removal happens in two ways: (1) a sync where the same id appears in the real topology; (2) explicit removal by the UserEvent handler when creation fails | empty | — | AE handler / main |
| Position of a provisional entry | — | — | A window goes last, a tab goes last in its window, a terminal goes last in its tab. It never becomes the result of `front window`, `selected tab` or `focused terminal` of a live container. Inside a pending container, the pending child added last becomes that container's `selected tab` / `focused terminal` (so `selected tab of w` and `focused terminal of b` resolve) | — | — | — |
| TerminalSnapshot | `terminal` | `Arc<Mutex<Terminal>>` | For contents/selection/scrollback. None while pending (reads give `""`) | — | yes | main (sync) |
| TerminalSnapshot | `pid` / `tty` | `u32` / `String` | Taken from `Pty` at spawn (newly exposed). If unavailable, missing value | — | yes | main |
| TabId | — | `u64` (new global `AtomicU64`) | The AppleScript tab id. Replaces `u64::from(WindowId)` | — | no | `WindowState` |
| WindowGroupId numbering | — | Change `next_group_id` to a global `AtomicU64` | Lets AE handlers number ahead of time | — | no | `lifecycle.rs` |
| UserEvent destination | — | New events, and events that target a pending object, carry a `PaneId` (creation events additionally carry the pre-numbered `TabId`/`WindowGroupId`) | The main side resolves by searching pane → tab. The `WindowId` field of existing events is not used when it can be resolved from the pane | — | — | — |
| `SurfaceConfigPayload` | 6 fields | A type that satisfies `Eq`. Font size is a `u32` in units of 1/100 pt | Carried on `SpawnTab` and `Split` events | — | — | — |

### L2.2 Interface contract
AppleScript. Terms are spelled as in Ghostty. For four-character codes, existing terms keep Noa's existing codes, and new terms use Ghostty's codes from Appendix B as they are. Noa Extensions are `contents=NEct`, `selection=NEsl`, `scrollback=NEsb` (suite `noaE`).

| Operation | Input | Output | Errors |
|-----------|-------|--------|--------|
| get `front window` / `selected tab` / `focused terminal` | specifier | id-form reference | no target → -1728 |
| get `terminals of …` | every form | list of references (an empty list if there are 0 elements) | no container → -1728 |
| get `pid` / `tty` / `contents` / `selection` / `scrollback` | terminal specifier | integer / text | -1728 |
| `close tab` / `close window` | specifier | — | -1728 |
| `count` / `exists` | specifier | integer / boolean | no container → -1728 (`exists` gives false) |
| `new surface configuration [from r]` | record? | record (L2.4) | bad field → -1700 |
| `new window [with configuration r] [initial working directory s] [command s]` | | window reference | -1700 |
| `new tab [in w] [with configuration r] […]` | | tab reference | no w → -1728 |
| `split <terminal\|dir> [direction d] [with configuration r]` | | terminal reference | no target → -1728. No direction (when in specifier form) → -1715 |
| `perform action s [on t]` | | boolean `true` | action not in the table → -1708 |
| `send key name [action a] [modifiers m] [to t]` | Defaults: press / no modifiers / focused terminal | — | unknown key or modifier → -1700 |
| `send mouse button b [action a] [modifiers m] [to t]` | b is `left button`/`right button`/`middle button` | — | -1700 / -1728 |
| `send mouse position x x y y [modifiers m] [to t]` | real. Point coordinates with the origin at the top-left of the pane | — | missing → -1715 / -1728 |
| `send mouse scroll x dx y dy [precision p] [momentum m] [to t]` | y>0 scrolls up. The default of precision is false. When false, dx/dy are integer wheel steps rounded toward 0 (1 step is handled as one notch of a physical wheel). When true, they are in points and handled like a trackpad delta. momentum validates the enumeration value (invalid → -1700) and is accepted, but has no effect (Noa does not use the physical wheel's phase either; Amendment 1) | — | missing → -1715 / -1728 |

AX (`TerminalAxElement`, a subclass of `NSAccessibilityElement`):

| Attribute | Value |
|-----------|-------|
| AXRole / AXRoleDescription | `AXTextArea` / `terminal` |
| AXIdentifier | `noa-terminal-<terminal id>` (the same value as the AppleScript terminal id) |
| AXTitle | terminal `name` |
| AXValue | The same string as `contents` |
| AXNumberOfCharacters | UTF-16 length of AXValue |
| AXSelectedText | The same string as `selection` |
| AXInsertionPointLineNumber | The cursor row within the viewport (0-based). No attribute value if the cursor is outside the viewport |
| AXFrame | The pane rectangle (screen coordinates) |
| AXFocused | true when that tab is the key window and the pane is the focused pane |
| AXParent | The nearest ancestor that is not ignored (the NSWindow, or the AXGroup in the alternative) |

How to attach: the first plan is to attach to winit's NSView (`ns_view` from raw-window-handle) with `setAccessibilityChildren:`. If the elements do not show up in the AX tree, the alternative is to place a transparent NSView that is not hit-tested as a child and hang the elements on it. Either way the observable result of REQ-014 is the same (AC-019/020 search with `entire contents`).

### L2.3 State & transitions
| From | Event | To | Guard | Side effect |
|------|-------|----|-------|-------------|
| (none) | AE of a creation command | pending | numbering succeeded | Insert into `PendingRegistry` and carry the pre-numbered id on the UserEvent |
| pending | main handles the UserEvent | live | spawn succeeded | Create the real object with the pre-numbered id. It disappears from the registry on the next sync |
| pending | spawn failed | (removed) | — | The handler removes it from the registry. Later UserEvents that target that id are dropped |
| live (wait after=true) | child process exits | exited | — | Show the exit message. contents is kept |
| exited | key press (physical or `send key`) | (closed) | — | The normal close-pane path |

### L2.4 Validation rules
| Field | Rule | On violation | Message source |
|-------|------|--------------|----------------|
| font size | real. Only 0 counts as unspecified; any other finite value (including negative and huge values) is clamped to the range of `clamp_runtime_font_size` (6–96 pt) before conversion to f32 | not a number → -1700 | errAECoercionFail |
| initial working directory | text. An empty string counts as unspecified. A directory that does not exist falls back as the existing spawn does | wrong type → -1700 | same |
| command | text. An empty string counts as unspecified. Starts `/bin/sh -c <command>` as the pty child in place of the shell (the same as the CLI's `-e`; Amendment 1) | wrong type → -1700 | same |
| initial input | text. Written to the pty as raw bytes right after spawn (not wrapped in bracketed paste). Truncated to 8 MiB (`APPLESCRIPT_INPUT_TEXT_CAP`) | wrong type → -1700 | same |
| wait after command | boolean (a number is 0 → false, anything else → true) | wrong type → -1700 | same |
| environment variables | A list of `KEY=VALUE` text. KEY must be non-empty. Split at the first `=` | invalid entry → -1700 | same |
| both `with configuration` and the old parameters | The record's field wins if it is non-empty; if it is empty the old parameter is used | — | — |
| send key name | The closed set of Appendix A (case-sensitive). For a key that has a key code, apply UCKeyTranslate once with the current input source (`TISCopyCurrentASCIICapableKeyboardLayoutInputSource` if there is no `kTISPropertyUnicodeKeyLayoutData`) to get the character. Dead keys are not composed. The `-` key is encoded with only its `KeyCode`, with no character | -1700 | errAECoercionFail |
| modifiers | Separated by `,`, surrounding spaces removed, case-insensitive. Only `shift`/`control`/`option`/`command` | -1700 | same |
| scroll x/y | Clamp one amount to ±1000 for steps and ±100000 for points, then pass it to the existing wheel handling (so that a huge finite value cannot overflow the row conversion) | missing → -1715 | errAEParamMissed |
| mouse x/y | The cell is `floor((p − padding) / cell) + 1` (a point on a boundary belongs to the cell on the right / below). Outside the pane, clamp to the cell at the pane edge (the same as a physical drag that leaves the pane). The cursor is not moved, and the event is delivered directly to the target pane (resolved by the target pane's rectangle, as with capture during a drag, and it never starts a divider drag). For a button or wheel, when the window's pointer is over a different pane, return to the position last sent to that pane by script (held as a pane-relative point and converted again with the current rectangle and scale) before sending. A pane that has never been sent a position gets the top-left | missing → -1715 | errAEParamMissed |

## Behavior matrix
| Condition | Applies to | Expected behavior | User-visible | Verifies |
|-----------|-----------|-------------------|--------------|----------|
| Empty state: 0 windows | REQ-001/002 | `front window` → -1728. `count windows` → 0. `terminals` → empty list | osascript error | AC-001 |
| Empty state: no selection | REQ-012/014 | `selection` and AXSelectedText are `""` | — | AC-017 |
| Empty state: reading a pending terminal | REQ-008/012 | `contents`/`selection`/`scrollback` are `""`, `pid`/`tty` are missing value | — | AC-030 |
| Invalid input: unknown key name, modifier or env entry; wrong type | REQ-003/010 | -1700. The app does nothing | osascript error | AC-027 |
| Invalid input: a required parameter is missing | REQ-007/011 | -1715 | osascript error | AC-027 |
| Unauthorized: TCC Automation denied, no Accessibility permission | all | The OS refuses. Noa shows no permission UI of its own (applescript R-12) | OS dialog | N/A (enforced by the OS; that there is no permission-UI code is checked in the AC-026 review) |
| Unauthorized: `macos-applescript = false` | REQ-001..013 | Noa registers no handlers. Noa Suite verbs error and nothing happens. Cocoa Scripting answers standard events (such as `count windows`) as before. AX is not affected (no config key, ratified) | osascript error | AC-026 |
| Dependency unavailable: the target closed between resolve and handling | REQ-009..011 | The UserEvent is dropped. A property read gives -1728 | — | AC-027 |
| Dependency unavailable: creation of a pending object failed | REQ-008 | The handler removes it from the registry and drops later events. Reading the reference gives -1728 | — | AC-030 |
| Concurrent modification: using a reference returned right after creation | REQ-008 | Resolved through the registry. UserEvents are FIFO, so they are handled after the creation | — | AC-010 |
| Concurrent modification: a snapshot rebuild overlaps with a pending insert | REQ-008 | The rebuild merges the registry, so the provisional entry does not disappear | — | AC-030 |
| Concurrent modification: several scripts send at once | all | AEs are processed serially on the main thread. No extra control is needed | — | N/A (guaranteed by structure) |
| Concurrent modification: the io thread holds the lock | REQ-012/014 | The read waits for the lock (within CFR-001). Waiting during reflow is outside CFR-001 (OQ-2) | brief wait | AC-022 |
| Limit exceeded: scrollback is over 256 KiB | REQ-012 | Truncate at a UTF-8 boundary, keeping the tail | the head is missing | AC-017 |
| Limit exceeded: initial input / input text is over 8 MiB | REQ-004 | Truncate, the same as the existing `cap_input_text` | — | AC-017 |
| Domain: wide and combining characters | REQ-012/014 | The wide spacer is not output, the same as `screen_text` | — | AC-018 |
| Domain: during Secure Keyboard Entry | REQ-012/014 | Exposed as it is, not hidden (ratified 2026-10-17) | — | AC-018 |
| Domain: the user has scrolled back | REQ-012/014 | `contents` and AXValue return the lines being shown (Glossary) | — | AC-016 |
| Domain: key release (kitty report-events disabled) | REQ-010 | Sends nothing, the same as a physical key | — | AC-012 |
| Domain: font size of a split | REQ-004 | Ignored; the parent tab's size is used | — | AC-007 |
| Domain: a modal (palette, settings) is open | REQ-014 | The AX pane elements stay as they are. The modal itself is not exposed (out of scope) | — | AC-019 |
| Domain: Noa is not the key window | REQ-014 | AXFocused is false for all | — | AC-020 |

## Dependencies & integration points
| Dependency | Kind | We control? | Contract / version | Failure mode | Fallback |
|------------|------|-------------|--------------------|--------------|----------|
| NSAccessibility (AppKit) | OS API | no | `NSAccessibilityElement` on macOS 13+ (`LSMinimumSystemVersion`) | Child elements do not show up in winit's view | The transparent NSView plan in L2.2 |
| TIS / UCKeyTranslate (HIToolbox) | OS API | no | `kTISPropertyUnicodeKeyLayoutData` of the current input source | An input source with no layout data (such as an IME) | An ASCII-capable input source (L2.4) |
| Cocoa Scripting (`NSAppleScriptEnabled`) | OS | no | Events we register ourselves are handled by our own handlers first | — | — |
| portable-pty | crate | no | The child's `process_id()` and the tty name | unavailable | pid/tty are missing value |
| Terms of Ghostty.sdef | external spec | no | Appendix B (main 2026-10-17) | Ghostty renames a term | Re-check at the review trigger |

## Migration & rollout
- Existing data: There is no persistent data.
- Behavior changes: (1) The tab id changes from being derived from `WindowId` to `TabId`. An id is valid only while the app runs, so there is no compatibility problem. (2) `count windows` changes from the number of NSWindows that Cocoa Scripting counted (one per native tab) to the number of windows (tab groups). This aligns with the definition of Noa's `window` class, and is recorded in the CHANGELOG.
- Staged release: No flag is used. The existing `macos-applescript` decides whether AppleScript is enabled.
- Rollback: Revert the commits. Nothing is irreversible.

## L3: Acceptance Criteria
Build the marker string so that it does not appear in the typed command line and appears only in the result of running it (for example, the output of `printf 'NOA-%s\n' AC10` is `NOA-AC10`).

| ID | Given | When | Then | Verifies | Mode | Oracle | Must-have |
|----|-------|------|------|----------|------|--------|-----------|
| AC-001 | A fixture snapshot (2 windows. Window 1 has 2 tabs, tab 2 is selected and has 2 panes, and pane 2 has focus. Also a fixture with 0 windows) | Resolve `front window`, `selected tab of window 1`, `focused terminal of tab 2 of window 1`, `terminals of window 1`, `terminals` (app) and `count windows` with the resolver | They give window 1, tab 2, pane 2, all panes of window 1 (ascending PaneId), all panes, and 2. With the 0-window fixture: -1728, an empty list, and 0 | REQ-001, REQ-002 | machine | Unit test in `macos_applescript.rs` | yes |
| AC-002 | A real machine. One terminal (a login shell is running) | Get `pid of t` and `tty of t` | pid equals the pid of the line in `ps -o pid=,comm= -t <the ttysNNN part of the tty>` whose comm basename (without the leading `-`) matches the basename of `$SHELL`. tty equals `/dev/` + that ttysNNN | REQ-001 | human | smoke | yes |
| AC-003 | Noa is in front, with a tab of 2 panes. Pane 2 has focus | Get the id of `focused terminal of selected tab of front window`. With System Events, get the AXIdentifier of the element whose AXFocused is true | Both point at the same id | REQ-001, REQ-014 | human | smoke | yes |
| AC-004 | — | Give the record parse/serialize functions: 6 fields, duplication by `from`, an invalid env `"NOEQ"`, wrong types, and precedence against old parameters (an empty and a non-empty string in the record) | Values survive a round trip. Invalid input gives -1700. Precedence is as in L2.4 | REQ-003, REQ-004 | machine | unit test | yes |
| AC-005 | — | For each of new window, new tab and split (`direction right`): `with configuration {initial working directory:"/usr", environment variables:{"FOO=bar"}, initial input:"printf 'CFG-%s-%s\\n' \"$PWD\" \"$FOO\"\n"}` | Within 2 seconds, the `contents` of each of the 3 terminals contains `CFG-/usr-bar` | REQ-003, REQ-004 | human | smoke | yes |
| AC-006 | — | (a) Create a new tab with `command:"/bin/echo done", wait after command:true`. (b) No command, `wait after command:true`, `initial input:"exit\n"`. (c) The same as (a) but without wait. (d) From the state of (a), press a physical key | (a) and (b): after 2 seconds the terminal still exists and `contents` contains the exit message ((a) also contains `done`). `send key "a" to` closes it. (c) disappears within 2 seconds. (d) also closes it | REQ-005 | human | smoke ((d) by eye) | yes |
| AC-007 | Default 14pt | `new window with configuration {font size:28}`. In that pane, `split … with configuration {font size:8}` | The row height of the new window is 1.8–2.2 times that of a default window. The row height of the split pane is the same as its parent | REQ-004 | human | Compare row heights in screenshots | yes |
| AC-008 | 2 windows. Window 1 has focus | `new tab in window 2` | The tab count of window 2 is +1, and window 1 is unchanged | REQ-006 | human | smoke | yes |
| AC-009 | Noa is in front. In a one-pane tab, `split right` was run to make a right pane (left = terminal 1). The right pane has focus | `split (terminal 1 of selected tab of front window) direction down`, then the old form `split right` | After the first, the AX frames show the left pane split top and bottom. After the second, the focused terminal is split left and right. The pane count is 4 | REQ-007, REQ-013 | human | smoke (decided by AX frame) | yes |
| AC-010 | Noa has been `activate`d. 1 window | In one osascript: `set t to split (focused terminal of selected tab of front window) direction right` → `input text "printf 'NOA-%s\\n' AC10\n" to t`. `set w to new window` → `input text "printf 'NOA-%s\\n' W10\n" to focused terminal of selected tab of w`. `set b to new tab in w` → `input text "printf 'NOA-%s\\n' B10\n" to focused terminal of b`. Wait 2 seconds and read the 3 `contents`. Finally compare `id of w` with `id of front window`, and `id of b` with `id of selected tab of front window` | No step gives -1728. The 3 contents contain `NOA-AC10`, `NOA-W10` and `NOA-B10` respectively. w is the window that was key last, so `id of w` = `id of front window`. b is the tab selected last in w, so `id of b` = `id of selected tab of front window` | REQ-006, REQ-008 | human | smoke | yes |
| AC-011 | Noa is in front. In a one-pane tab, `split right` was run to make a right pane (left = terminal 1). The right pane has focus | `perform action "new_split:down" on terminal 1 of selected tab of front window`. Then `perform action "nonexistent"` | The first returns `true`, and the AX frames show the left pane split top and bottom. The second gives -1708 | REQ-009 | human | smoke (decided by AX frame) | yes |
| AC-012 | — | Give the parse/encode functions `enter`, `arrowUp`, `a`, `digit1`, `f21`, `foo`, `"shift, Control"`, `hyper`. Encode a release with kitty flags 0 and with report-events on. Translate `semicolon`+shift with fixed US layout data | Known names map to the Appendix A key codes and to winit `KeyCode`. `foo` and `hyper` are errors. A release is empty with flags 0, and is a kitty release sequence with report-events. The US translation result is `:` | REQ-010 | machine | unit test | yes |
| AC-013 | US layout | (1) Run `cat -v`, then `send key "arrowUp"` → `send key "enter"` → `send key "c" modifiers "control"`. (2) In one line run `printf '\e[?1h'; cat -v`, and send the same three | After (1) `contents` contains `^[[A`, and after (2) `^[OA`. In both, cat exits and the prompt returns | REQ-010 | human | smoke | yes |
| AC-014 | Set the input source to Dvorak | For `cat -v`, `send key "s"` and `send key "enter"` | `o` appears, the same as pressing the physical S key | REQ-010 | human | by eye | yes |
| AC-015 | Noa is in front. A terminal is running `printf '\e[?1000h\e[?1006h'; cat -v`. `NSEvent.mouseLocation` has been recorded | `send mouse position x 300 y 120` → `send mouse button left button` → the same with `action release` → `send mouse scroll x 0 y 1` | `contents` contains `^[[<0;c;rM`, `^[[<0;c;rm` and `^[[<64;c;rM`, with the same c;r. c>1 and r>1 (not clamped to an edge). `NSEvent.mouseLocation` has not changed. The correctness of the point-to-cell conversion is checked in AC-032 | REQ-011 | human | smoke | yes |
| AC-016 | No mouse tracking. There is `seq 1 5000` output | `send mouse scroll x 0 y 5` | The number of the first line of `contents` is smaller than before the scroll (it scrolls back, the same as a physical wheel) | REQ-011, REQ-012 | human | smoke | yes |
| AC-017 | A fixture Terminal | Call the extraction functions for contents, selection and scrollback (no selection / a selection / 300 KiB of history / 9 MiB of initial input) | contents equals `screen_text(visible_rows)`. selection is `""` with no selection, and equals `selected_text()` with a selection. scrollback is at most 256 KiB and equals the tail of the whole history. initial input is truncated to 8 MiB | REQ-012, REQ-004 | machine | unit test | yes |
| AC-018 | A real machine. Secure Keyboard Entry is ON | `input text "printf '%s-%s\\n' あいう AC18\n"` → `contents` | `あいう-AC18` is contained exactly once (no spacer, not hidden) | REQ-012 | human | smoke | yes |
| AC-019 | A tab with 2 panes is in front. In each pane `printf 'AXM-%s\n' <n>` has been run (once more with the command palette open) | With System Events, collect the elements whose role is `AXTextArea` from `entire contents of window 1 of process "Noa"`, and read `value`, `role description` and `AXIdentifier` | 2 elements are returned. Each `value` contains `AXM-<n>`. The role description is `terminal`. AXIdentifier is `noa-terminal-<the AppleScript id>` | REQ-014 | human | smoke (run from a terminal that has Accessibility permission) | yes |
| AC-020 | The same as AC-019 | (a) `activate` Noa, focus pane 2 and read AXFocused. (b) Bring the originating terminal to the front and read again. (c) Add a split. (d) Close one pane. (e) Change the size with `tell application "System Events" to set size of window 1 of process "Noa" to {900, 600}`. (f) 250 ms after the AE of each operation returns, read `position` and `size` of all elements | (a) true for pane 2 only. (b) All false. (c)(d) The element count goes up and down. (e)(f) The frames of all elements do not overlap, and together fill the window's content area (excluding divider width). They follow the new size after the resize | REQ-014, REQ-015 | human | smoke | yes |
| AC-021 | One pane. Running `clear; printf 'hello\e[5;1H'; cat` (cat does not move the cursor and prints nothing). `hello` on line 1 is selected by double-click | Read `AXSelectedText`, `AXInsertionPointLineNumber` and the AE `selection of t` | `hello`, `4`, `hello` | REQ-012, REQ-014 | human | smoke | yes |
| AC-022 | A release build | Measure (a)(b)(c) of CFR-001 | The targets of CFR-001 are met | CFR-001 | machine | `#[ignore]` measurement test | yes |
| AC-023 | No AX client, no AE | Compare the idle measurement with main. Review the diff | CFR-002 is met | CFR-002 | machine + review | The idle harness in `bench/` and the review record | yes |
| AC-024 | — | Run the existing `scripts/applescript-smoke.sh` without changing it | Every existing item PASSes | REQ-013 | human | smoke | yes |
| AC-025 | at review | Read the functions reachable from the AE and AX handlers | 0 occurrences of `recv`, `block_on`, condvar waits, or direct access to winit objects | CFR-003 | review | Reviewer's checklist | yes |
| AC-026 | Launched with `macos-applescript = false` | `tell app "Noa" to perform action "new_tab"`, `tell app "Noa" to count windows`, and the AC-019 procedure | The first errors and the tab count does not change. The second is answered by Cocoa Scripting (no error). AX can be read, the same as in AC-019. There is no code for a permission UI of our own (review) | REQ-014, Behavior matrix | human + review | smoke | yes |
| AC-027 | A real machine | `send key "foo"`, `send key "a" modifiers "hyper"`, env `"NOEQ"`, `send mouse position x 1` (no y), and `send key` and `contents` to the id of a closed terminal | -1700, -1700, -1700, -1715, -1728, -1728. The app does not crash | REQ-003, REQ-010, REQ-011 | human | smoke | yes |
| AC-028 | at build | A test that checks the sdef against the Rust code table. A test that checks the sdef against Appendix B (transcribed into a fixture file in the repository) | Noa's sdef contains every name in Appendix B, the codes of new terms match Appendix B, and every code is in the Rust table | REQ-001..012 | machine | `sdef_codes_are_all_in_the_rust_table` and the new check test | yes |
| AC-029 | Window 1 with 2 tabs. The shell is idle (no close confirmation appears) | `count tabs of window 1` → `exists tab 3 of window 1` → `exists tab 1 of window 1` → `close tab (tab 2 of window 1)` → create a new tab and then `close (tab 2 of window 1)` → `close window (window 1)` | 2 → false → true → 1 → 1 → the window count drops by 1 | REQ-002, REQ-013 | human | smoke | yes |
| AC-030 | — | Unit tests of `PendingRegistry` and the snapshot rebuild: rebuild right after inserting a pending entry / the real object appears / remove on creation failure / read a pending terminal | The pending entry survives the rebuild. It disappears when the real object appears. It disappears on failure, and resolving it afterwards gives -1728. A pending read is as in the Behavior matrix | REQ-008 | machine | unit test | yes |
| AC-031 | A terminal with `seq 1 5000` output. The output of `printf 'SEL-%s\n' X` is selected with the mouse | Read `scrollback of t` and `selection of t` | scrollback contains `5000`, is at most 256 KiB, and contains the line that ends with `SEL-X`. selection is `SEL-X` | REQ-012 | human | smoke | yes |
| AC-032 | — | Give the point-to-cell function (300,120), (0,0) and (10000,10000), with an 8×16 pt cell, padding 4 pt left / 2 pt top, and an 80×24 pane | (38, 8), (1, 1), (80, 24). The rule is `floor((p − padding) / cell) + 1` clamped to 1..cols / 1..rows (a point on a boundary belongs to the cell on the right / below) | REQ-011 | machine | unit test | yes |
| AC-033 | at build | Using the bundled sdef as the dictionary, compile `tell app "Noa" to close tab 2 of window 1`, `close tab (tab 2 of window 1)` and `close (tab 2 of window 1)` with `osacompile`, and decompile them with `osadecompile` | The old form gives a compile error (the breaking change of Amendment 2 is real). The two new forms compile and decompile back to the original statement | REQ-002, REQ-013 | machine | `scripts/applescript-sdef-check.sh` (run outside the sandbox) | yes |

## Scope
- In scope: REQ-001..015, CFR-001..003. The new smoke script `scripts/agent-automation-smoke.sh` and the sdef compile check `scripts/applescript-sdef-check.sh`. The sdef update. The fixture file for Appendix B. A CHANGELOG entry for the `count windows` change.
- Out of scope: `wait for`-style wait primitives, AX value-change notifications, AX for the self-drawn UI (sidebar, Overview, settings, palette, modals), `whose` filters, VoiceOver tuning, Shortcuts (App Intents), changing settings from AppleScript, AX range attributes (such as `AXBoundsForRange`), per-pane font size, writing AXValue, masking during Secure Input, a config key for AX exposure, changes on the noa-server side.

## Considered but rejected
- D1 Strict Ghostty conformance only: It cannot read output back, so it does not meet the agent's JTBD (only the part isolated into our own suite is adopted).
- D3 Make the whole app AX with AccessKit: The largest dependency and amount of work. The self-drawn UI was put out of scope.
- D4 Leave reading to noa-server: It needs startup and a token, so it is not a no-setup osascript path. Reconsidered in the falsification panel, but the user chose to keep the scope (2026-10-17).
- D5 Make AX the only reading surface: The caller would need Accessibility permission, and going through System Events is fragile.
- Shrinking the scope (dropping AX and send mouse, or keeping only conformance and send key): Presented after the falsification panel, but the user chose to keep the scope (2026-10-17).
- `wait for`: Under constraint C1 it needs an AE suspend/resume mechanism. Polling `contents` can stand in for it.
- AX value-change notifications: VoiceOver is out of scope. The CPU cost during heavy output is not worth it.
- Per-pane font size: The impact on rendering and layout is large (user choice 2026-10-17).
- Key translation fixed to US-ANSI: The results would differ from Ghostty (user choice 2026-10-17).
- Not adding `close tab`/`close window`: Matching Ghostty-targeted scripts took priority (user choice 2026-10-17).

## Assumption Ledger
| ID | Assumption | Default chosen | Why | Status |
|----|-----------|----------------|-----|--------|
| ASSUME-1 (delegated) | The choice at CHALLENGE | D2 + the suite isolation of D1, minus `wait for`, minus notifications | "Go with the recommendation" (2026-10-17) | ratified |
| ASSUME-2 | The type of id | Stay integer (Ghostty uses text) | Compatibility with existing scripts | ratified (2026-10-17) |
| ASSUME-3 | How `perform action … on t` is realized | Move focus to the target terminal, then dispatch | Noa's AppCommand is structured to target the focused window | ratified (2026-10-17) |
| ASSUME-4 | The meaning of `pid` | The pid of the pty's direct child (the shell) | Judged to be the same meaning as the pid of a Ghostty surface | ratified (2026-10-17) |
| ASSUME-5 | The sign of scroll | y>0 scrolls up (the same as NSEvent) | Ghostty passes the NSEvent delta through as it is | ratified (2026-10-17) |
| ASSUME-6 | The target of `new tab` with `in` omitted, and which tab is key after creation | The live front window at the moment the AE is received. The tab that was created becomes key inside Noa, but the app is not activated | The same behavior as UI `cmd+t`. If a pending object were the target, the result would change with the order of the script | ratified (2026-10-17) |
| ASSUME-7 | What to do if the old form `close tab 2 of window 1` cannot be kept (arose in AC-033; settled as a breaking change in Amendment 2) | If AC-033 fails, do not work around it in the implementation: go back to the spec, and the user decides whether to revise REQ-002 and AC-029/033 as a breaking change (the old form is replaced by `close (tab 2 of window 1)`) | A consequence of the choice to prioritize matching Ghostty (2026-10-17) | ratified (2026-10-17) |

## Open Questions / Deferred Decisions
| ID | Question | Blocks | Owner | Resolve by | Impact if wrong |
|----|----------|--------|-------|-----------|-----------------|
| OQ-1 | Assigning the Mac for the real-machine tests (a machine with the Dvorak input source and Accessibility and Automation permissions granted) | nothing (the capability requirements are settled in acceptance §3; only the assignment remains) | simota | before-ship | The acceptance run cannot be executed |
| OQ-2 | Whether to put an upper bound on how long an AE/AX read waits during the reflow of a huge scrollback (about 76 ms measured at 10 MB), such as `try_lock` and answering with a stale value | nothing (CFR-001 is settled with reflow excluded. It is rare and limited to user interaction, so it does not affect this build) | simota | deferred | If an AX client reads during a resize, it can stall for up to several tens of ms |

## Build-path decision
one-shot autonomous build (elicited 2026-10-17). The machine ACs (AC-001/004/012/017/022/028/030/032/033) and `cargo build/test/clippy --workspace` are the build gates. The human ACs wait for real-machine confirmation, handing over `scripts/agent-automation-smoke.sh` (OQ-1).

## Appendix A: send key key names (Ghostty `Ghostty.Input.Key` rawValue, main 2026-10-17)

Format: `name=macOS virtual key code (hex)`. `-` marks a key that has no key code on a Mac. The name is mapped directly to winit's `KeyCode` and encoded without going through layout translation (L2.4).

```
backquote=0x32 backslash=0x2a bracketLeft=0x21 bracketRight=0x1e comma=0x2b digit0=0x1d digit1=0x12 
digit2=0x13 digit3=0x14 digit4=0x15 digit5=0x17 digit6=0x16 digit7=0x1a digit8=0x1c digit9=0x19 equal=0x18 
intlBackslash=0x0a intlRo=0x5e intlYen=0x5d a=0x00 b=0x0b c=0x08 d=0x02 e=0x0e f=0x03 g=0x05 h=0x04 i=0x22 
j=0x26 k=0x28 l=0x25 m=0x2e n=0x2d o=0x1f p=0x23 q=0x0c r=0x0f s=0x01 t=0x11 u=0x20 v=0x09 w=0x0d x=0x07 
y=0x10 z=0x06 minus=0x1b period=0x2f quote=0x27 semicolon=0x29 slash=0x2c altLeft=0x3a altRight=0x3d 
backspace=0x33 capsLock=0x39 contextMenu=0x6e controlLeft=0x3b controlRight=0x3e enter=0x24 metaLeft=0x37 
metaRight=0x36 shiftLeft=0x38 shiftRight=0x3c space=0x31 tab=0x30 convert=- kanaMode=- nonConvert=- 
delete=0x75 end=0x77 help=- home=0x73 insert=0x72 pageDown=0x79 pageUp=0x74 arrowDown=0x7d arrowLeft=0x7b 
arrowRight=0x7c arrowUp=0x7e numLock=0x47 numpad0=0x52 numpad1=0x53 numpad2=0x54 numpad3=0x55 numpad4=0x56 
numpad5=0x57 numpad6=0x58 numpad7=0x59 numpad8=0x5b numpad9=0x5c numpadAdd=0x45 numpadBackspace=- 
numpadClear=- numpadClearEntry=- numpadComma=0x5f numpadDecimal=0x41 numpadDivide=0x4b numpadEnter=0x4c 
numpadEqual=0x51 numpadMemoryAdd=- numpadMemoryClear=- numpadMemoryRecall=- numpadMemoryStore=- 
numpadMemorySubtract=- numpadMultiply=0x43 numpadParenLeft=- numpadParenRight=- numpadSubtract=0x4e 
numpadSeparator=- numpadUp=- numpadDown=- numpadRight=- numpadLeft=- numpadBegin=- numpadHome=- numpadEnd=- 
numpadInsert=- numpadDelete=- numpadPageUp=- numpadPageDown=- escape=0x35 f1=0x7a f2=0x78 f3=0x63 f4=0x76 
f5=0x60 f6=0x61 f7=0x62 f8=0x64 f9=0x65 f10=0x6d f11=0x67 f12=0x6f f13=0x69 f14=0x6b f15=0x71 f16=0x6a 
f17=0x40 f18=0x4f f19=0x50 f20=0x5a f21=- f22=- f23=- f24=- f25=- fn=- fnLock=- printScreen=- scrollLock=- 
pause=- browserBack=- browserFavorites=- browserForward=- browserHome=- browserRefresh=- browserSearch=- 
browserStop=- eject=- launchApp1=- launchApp2=- launchMail=- mediaPlayPause=- mediaSelect=- mediaStop=- 
mediaTrackNext=- mediaTrackPrevious=- power=- sleep=- audioVolumeDown=0x49 audioVolumeMute=0x4a 
audioVolumeUp=0x48 wakeUp=- copy=- cut=- paste=-
```

## Appendix B: Ghostty.sdef terms and four-character codes (main 2026-10-17. New terms use these codes as they are)

```
application=capp; name=pnam; frontmost=pisf; front window=GFWn; version=vers; window=Gwnd; id=ID  ; 
name=pnam; selected tab=GWsT; tab=Gtab; id=ID  ; name=pnam; index=pidx; selected=GTsl; focused terminal=GTfT; 
terminal=Gtrm; id=ID  ; name=pnam; working directory=Gwdr; pid=Gpid; tty=Gtty; surface configuration=GScf; 
font size=GScF; initial working directory=GScD; command=GScC; initial input=GScI; wait after command=GScW; 
environment variables=GScE; split direction=GSpD; right=GSrt; left=GSlf; down=GSdn; up=GSup; input 
action=GIAc; press=GIpr; release=GIrl; mouse button=GMBt; left button=GMlf; right button=GMrt; middle 
button=GMmd; scroll momentum=GSMo; none=SMno; began=SMbg; changed=SMch; ended=SMen; cancelled=SMcn; may 
begin=SMmb; stationary=SMst; perform action=GhstPfAc; on=GonT; new surface configuration=GhstNSCf; from=GScS; 
new window=GhstNWin; with configuration=GNwS; new tab=GhstNTab; in=GNtW; with configuration=GNtS; 
split=GhstSplt; direction=GSpd; with configuration=GSpS; focus=GhstFcus; close=GhstClos; activate 
window=GhstAcWn; select tab=GhstSlTb; close tab=GhstClTb; close window=GhstClWn; input text=GhstInTx; 
to=GItT; send key=GhstSKey; action=GKeA; modifiers=GKeM; to=GKeT; send mouse button=GhstSMBt; action=GMbA; 
modifiers=GMbM; to=GMbT; send mouse position=GhstSMPs; x=GMpX; y=GMpY; modifiers=GMpM; to=GMpT; send mouse 
scroll=GhstSMSc; x=GMsX; y=GMsY; precision=GMsP; momentum=GMsM; to=GMsT; count=corecnte; each=kocl; 
exists=coredoex; quit=aevtquit
```

## Amendment 1: Settled at implementation time (2026-10-17)
- How `command` runs: L2.4 said "the same as the existing `SpawnTab.command` (type one line into the shell)", which contradicted AC-006(c) (without wait, it disappears within 2 seconds). By the user's choice, this is settled as Ghostty's "start it in place of the shell" (elicited 2026-10-17). The old parameter `new tab command "x"` now has the same meaning, which changes the previous behavior of "type it into the shell and stay". It is recorded in the CHANGELOG. AC-4 of `docs/specs/applescript.md` (the given command runs in the new surface) is still met in this sense.
- `momentum`: The argument stays for compatibility with the Ghostty dictionary (AC-028), and its enumeration value is validated (invalid → -1700), but it has no effect. Noa's wheel handling (`MouseWheel { delta, .. }` in `event_loop.rs`) does not use the physical event's phase either, so there is nowhere to pass it. This is also stated in the sdef description (review finding 2026-10-17).
- Lifetime of `font size`: A size set on a tab goes back to the global size on a scale change of that window, or on a runtime font-size change (global operations such as `cmd+=`, and config reload). REQ-004 only requires it to apply at creation.
- Keys while `wait after command` holds: A lone modifier key and Cmd shortcuts do not close it (so that output can be copied and apps switched). IME input (a non-empty Preedit or Commit) also closes it, and input being composed is discarded.
- `close tab`: Closes the whole target tab through the same confirming path as the UI (`request_close_tab`). `AppCommand::CloseTab` is not used because in a split tab it closes only the focused pane.
- The `space` key: Space also goes through layout translation (Option+Space on the US layout is U+00A0). It is handled as the named key Space only when the result is a plain space.
- Retiring pending entries: When creation succeeds, that id is removed from the registry (so a short-lived command that closes before the snapshot rebuild does not stay pending). Only that id is removed; a split promised earlier on the same tab is kept.
- UserEvent payload: Instead of the `SurfaceConfigPayload` that satisfies `Eq` in L2.1, use the same shape as noa-server's `IpcAction` (the payload goes in a shared wait table, and `UserEvent` carries only a request id). This can carry f64 and the like without the `Eq` constraint. Observable behavior does not change.

## Amendment 2: Handling of the old form `close tab N of window M` (2026-10-17)
- AC-033 measured that adding Ghostty's `close tab` command to the dictionary makes the old form `close tab 2 of window 1` fail to compile (AppleScript reads it as the `close tab` command plus `2 of window 1`, giving `Can't get 2 of window 1 (-1723)`). `close window 1` and the new forms `close tab (tab 2 of window 1)` / `close (tab 2 of window 1)` work.
- Following ASSUME-7, this went back to the spec, and the user accepted it as a breaking change (elicited 2026-10-17). "The old form gives the same result" is removed from REQ-002, and the CHANGELOG points the old form to its replacement (`close (tab 2 of window 1)` or `close tab (tab 2 of window 1)`). The old-form step of AC-029 is replaced with `close (tab 2 of window 1)`, and the Then of AC-033 is changed to "the old form is a compile error and the two new forms compile".

## Quality Gate Record
- 2026-10-17 first run: 3 PASS / 5 FAIL (BLOCKING 7) → fixed. Falsification panel, 2 members: "the problem is real" was partly refuted with evidence → went back to FRAME and rewrote L0 (the user chose to keep the scope). "The ACs prove the requirements" found 18 gaps → ACs fixed and added.
- Second run: 7 items resolved, 4 new BLOCKING → fixed. Third run: 1 expected value in AC-032 → fixed. Everything else is resolved, Completeness PASS.
