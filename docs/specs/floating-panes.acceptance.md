# Floating panes acceptance test spec

| | |
|---|---|
| slug | floating-panes |
| spec source | `docs/specs/floating-panes.md` as-of 2026-11-10 |
| status | locked |
| owner | simota |
| reader | The implementer and the acceptance owner. Runs the suite and decides whether to accept the build |
| as-of | 2026-11-10 |
| review trigger | When the spec changes |

## 1. Test scope

- In scope: AC-001 to AC-044 of the spec's L3.
- Out of scope: The items listed under the spec's Out of scope. They are not tested anywhere.
- Not proven by this suite: Smoothness of drawing during a drag. Resource consumption when the limit of 8 is used up. A mix of multiple monitors and HiDPI. IME input (assumed to go through the same input path as split panes, and not checked separately).

## 2. Entry & exit criteria

| | Criterion |
|---|-----------|
| Entry | The spec is `locked`. A build identifiable by commit exists in the §3 environment |
| Exit | All must-have TCs PASS on both macOS and Linux. No S1 or S2 from §8 remains. Every failure that is not must-have is recorded as a defect |

## 3. Environment

| Item | Value | How to obtain / set |
|------|-------|---------------------|
| Environment | Local. (a) macOS Apple Silicon, (b) Linux. Both must have a display and a GPU and be able to open a pty (outside the sandbox) | The concrete configuration of (b) is `TBD(simota)` (spec OQ-3) |
| Build under test | The commit at run time | Record `git rev-parse HEAD` in §9 |
| Data state | Empty. Start without a session file | §4 F-0 |
| Accounts & roles | N/A (local single user) | — |
| Feature flags | N/A (no flags) | — |
| External dependencies | N/A (no external dependencies. Agent state is reproduced by emitting OSC by hand) | §4 F-2 |
| Clock / timezone / locale | N/A (no time-dependent behaviour) | — |

## 4. Test data

| Fixture | Purpose | Source | Reset procedure | Contains personal data |
|---------|---------|--------|-----------------|------------------------|
| F-0 | Empty startup state | Move aside noa's session file and scrollback directory, then start | Put back what was moved aside after the test | no |
| F-1 | Fixed values for geometry unit tests | Make F-1 at the head of the spec's L3 a constant in the test code | N/A (holds no state) | no |
| F-2a | Reproduce `AgentState::Permission` | In a float: `printf '\033]7501;state=blocked:kind=permission:app=test\033\\'` | Close the float | no |
| F-2b | Reproduce `AgentState::Input` | In a float: `printf '\033]7501;state=blocked:app=test\033\\'` | Close the float | no |
| F-2c | Reproduce `AgentState::Error` | In a float: `printf '\033]7501;state=blocked:kind=auth:app=test\033\\'` | Close the float | no |
| F-3 | Corrupt session file: 2 float records, one of which has `NaN` in the origin's x | Generated inside the test code | N/A (temporary file) | no |

## 5. Test cases

Machine-mode cases are implemented as automated tests, and the test name includes the TC's ID. The file path is `TBD(implementer)` (spec OQ-2). `OQ-2` in the Automation ref column refers to this. TC numbers are the same as the numbers of the ACs they verify.

| ID | Verifies | Title | Precondition | Steps | Expected result | Mode | Automation ref | Must-have | Priority | Result |
|----|----------|-------|--------------|-------|-----------------|------|----------------|-----------|----------|--------|
| TC-001 | AC-001 | Creation does not change the splits | F-1 (tab 1 has 1 split pane, tab 2 has 2), 0 floats | `float.new` in tab 1 | All split pane rectangles unchanged. Float (200,100)-(1000,600), 80×24, has focus, cwd matches | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-002 | AC-002 | Shift of the 2nd and 3rd | Right after TC-001 | `float.new` ×2 | (180,80)-(980,580), (160,60)-(960,560). Separately, make the 1st a tab anchor and then `float.new` → (180,80)-(980,580). With an A/B split, when only (600,100) 300×200 px exists over B, `float.new` on A → (50,100) 450×500 px (no shift) | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-003 | AC-003 | Shrink on a small anchor | Anchor 600×300 px | `float.new` | 540×270 px, 54×12 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-005 | AC-005 | Clamp on move | F-1, a 400×300 px float at (100,100) | (a) band by (+900,+900) (b) from the initial state, (−900,−900) | (a) top-left (600,300) (b) (0,0) | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-006 | AC-006 | Display on tab switch and pty continuation | 2 tabs, one float of each of the 3 kinds (the pane anchor is in tab 1) | Show tab 2 and deliver `x` to the pty of the 2 hidden floats | Only the window anchor is displayed, position unchanged. `x` has entered the terminals of the 2 hidden floats | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-007 | AC-007 | Hidden while zoomed | F-1, a float on A | Zoom B, release | Hidden while zoomed, at its original position after release | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-008 | AC-008 | Anchor switch | F-1, pane anchor (A) 300×200 px at (100,100) | `float.cycle-anchor`, band by (+500,0), `float.cycle-anchor`, click the icon | tab → window → pane (B). On each switch the on-screen rectangle is unchanged, and right after it is front-most of the new layer | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-009 | AC-009 | Ratio following and lower limit | F-1, origin (0.1,0.1) size (0.2,0.2), band being dragged | Anchor to 500×300 px, then 1000×600 px | (50,30) 200×80 px, then (100,60) 200×120 px. The drag state continues | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-010 | AC-010 | Hidden below the lower limit | One float | Anchor to 150×60 px, then 1000×600 px | Hidden but existing, then displayed at its original ratio | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-011 | AC-011 | Layered z-order | Create P1, P2, T1, W1 in that order and overlap them. Click a split pane | Click P1, `float.focus-next` | W1,T1,P1,P2, then W1,T1,P2,P1 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-012 | AC-012 | Collapse round trip (3 methods) | F-1, a normal float with focus at 80×24 | Collapse → expand with each of the collapse button, band double-click and `float.toggle-collapse` | For each method: band 20 px, width and top-left unchanged, focus goes to the return pane → to the float on expand, 0 pty size notifications, 80×24. Separately, press the collapse button with focus on a split pane → focus stays on the split pane | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-013 | AC-013 | Maximize round trip | F-1, (200,100) 400×300 px | `float.toggle-maximize`, maximize button | (0,0) 1000×600 px, 100×29, band present, 1 notification, then (200,100) 400×300 px | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-014 | AC-014 | Restrictions while maximized | A maximized float | (a) collapse → expand (b) band by (+50,+50) (c) right edge by (−50,0) (d) collapse a normal-state float, maximize button, `float.toggle-maximize`, expand | (a) maximized (b)(c) rectangle unchanged (d) stays collapsed, normal state on expand | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-015 | AC-015 | Focus movement | F-1, create F1,F2,F3 anchored to pane B and collapse F3. Click F1, click A. "Click B" is at (520,10) | `float.toggle-focus`, `float.focus-next` ×2, `float.focus-previous`, `float.toggle-focus`, click B, `float.focus-next`, click B, `float.focus-previous`, click B, click F1's content area, wheel and mouse move over F1 | Focus F1→F2→F1→F2→A→B→F1→B→F2→B→F1. Click, wheel and mouse move do not reach B, and F1 scrolls | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-017 | AC-017 | Close confirmation | `sleep 600` running in the split pane and its float | (a) close button (b) recreate and `tab.close` (c) recreate and close the split pane | (a)(b) confirmation → on approval only the float closes and focus goes to the return pane (c) confirmation with 2 running programs → both close | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-018 | AC-018 | Auto-approve | Auto-approve enabled, the same approval-wait screen prepared in a split pane and a float | Run auto-approve | The decisions are equal, and the approval input is written to the float's pty | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-020 | AC-020 | Session round trip | The Given of AC-020 | Save → read back | The Then of AC-020 (the tab anchor is restored collapsed and, on expand, is in the normal state at (300,200) 300×200 px. The window anchor is in the normal state at (200,100) 400×300 px. `float.toggle-focus` right after restore goes to the window-anchored float) | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-021 | AC-021 | Corrupt record, old file, disabled | (a) F-3 (b) a version-3 file (c) 1 float with `window-save-state` disabled | (a)(b) read (c) save process | (a) 1 restored (b) 0, normal (c) not written. The version written is 4 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-022 | AC-022 | Limit | (a) 8 on the same split pane (b) 8 tab anchors + 1 pane anchor (c) 8 floats, 8 split panes | (a) `float.new` (b) `float.cycle-anchor` on the pane anchor (c) add a split pane | (a) no increase, toast (b) stays pane, toast (c) 9 split panes | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-023 | AC-023 | No-op with 0 floats | 0 floats | The 6 actions other than `float.new` | State and focus unchanged | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-024 | AC-024 | pty creation failure | A substitute that makes pty creation fail | `float.new` | Float count and focus unchanged, 1 error log line | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-025 | AC-025 | Discard during a drag | A float is being dragged | (a) close the anchor's split pane (b) discard by shell exit | No drag state, no float | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-026 | AC-026 | Resize | F-1, a float at (300,100) 400×300 px. (e) is a tab-anchored float at (500,100) 400×300 px with an A/B split. (f) creates F1 (100,100) 300×300 px and F2 (350,150) 300×200 px in that order on the same split pane (F2 is in front), then clicks (50,50) on the split pane to move focus to the split pane | (a) bottom-right corner (−2000,−2000) (b) bottom-right corner (+2000,+2000) (c) left edge (−50,0) (d) 8 handles outward by 10 px (e) press (497,200) and drag (−50,0) (f) press (402,250) and drag (+50,0) | (a) content area 200×60 px (b) bottom-right (1000,600) (c) 450 px, +5 columns, the last notification is the new grid (d) only the matching edge, by 10 px (e) float width 450 px, the divider stays at x=500 (f) F1 stays 300×300 px, F2 gains focus | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-027 | AC-027 | Key bindings | (a) defaults (b) 7 `keybind =` lines assigning `cmd+alt+1` to `cmd+alt+7` to `float.new` through `float.cycle-anchor` | Build the engine; for (b), look up each code | (a) the defaults are 2 actions, no collisions, `cmd+shift+f` unassigned (b) all 7 resolve | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-028 | AC-028 | Split operations on a float | A float has focus | Enumerate and run every action name that starts with `split.` | Split tree, zoom and focus unchanged | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-029 | AC-029 | Excluded windows | (a) Scratch Terminal (b) Quick Terminal has focus | `float.new` | Float count unchanged | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-031 | AC-031 | Not on external surfaces | One float | Generate the AppleScript, IPC and AX snapshots and the Overview layout | None contains the float's `PaneId` | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-032 | AC-032 | Existing gates | Built | `cargo test --workspace`, `cargo clippy --workspace` (outside the sandbox) | Both exit code 0, 0 clippy warnings | machine | The commands themselves | yes | CRITICAL | NOT_RUN |
| TC-033 | AC-033 | Hidden floats are not drawn | (a) another tab (b) another pane zoomed (c) anchor 150×60 px | Build the draw plan | In every case the float is not in the plan | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-036 | AC-036 | Shell exit | Default settings, a float and a split pane | `exit` in each | The float's result is the same as the split pane's | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-037 | AC-037 | Anchor movement and disappearance | The Given of AC-037. `sleep 600` is running in the floats | (a) split pane to another tab (b) tab to another window (c) separately: close tab 1 / close the window / quit the app | (a) attaches to the same split pane at the same ratio (b) moves with the tab (c) running-program counts 2 / 3 / 3, ptys released after approval | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-038 | AC-038 | Focus on becoming hidden | (a) tab 1's float on A has focus, tab 2 has C (b) A's float has focus | (a) to tab 2, to tab 1 (b) anchor to 150×60 px, 1000×600 px | (a) C, and A on return (b) A, and A even after restoring | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-039 | AC-039 | Operations while collapsed | F-1, collapse a (500,250) 400×300 px float | (a) band by (+900,+900) (b) drag the right edge (c) `float.cycle-anchor` (d) expand | (a) band top-left (600,580) (b) unchanged (c) tab (d) top-left (600,300) | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-040 | AC-040 | Anchor switch while maximized | F-1, window anchor, normal-state (0,100) 800×500 px, maximized | `float.cycle-anchor`, `float.toggle-maximize` | (0,0) 500×600 px maximized, after unmaximizing (0,100) 500×500 px | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-041 | AC-041 | New creation from a float | F-1, A's float has focus | `float.new` | The new float's anchor is A | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-042 | AC-042 | Title band click | F1 (back), F2 (front, collapsed), focus is on a split pane | Click F1's band, then F2's band in turn | F1 front-most + focus, then F2 front-most with focus staying on F1 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-043 | AC-043 | Jump from the sidebar (special states) | (a) A's float while B is zoomed (b) a float in another window (c) a pane anchor with anchor 150×60 px | Select each card | (a) zoom released + float gets focus (b) window to front + float gets focus (c) the anchor's split pane gets focus | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-044 | AC-044 | Font size change | F-1, (200,100) 400×300 px, 40×14 | Make the cell 20×40 px | Rectangle unchanged, title band 40 px, 20×6, the last notification is 20×6 | machine | OQ-2 | yes | HIGH | NOT_RUN |

### TC-004: Composition of the title band
- Verifies: `AC-004` (`REQ-002`)
- Mode: human · Must-have: yes · Automation ref: N/A (visual) · Result: `NOT_RUN`
- Precondition: Start with F-0.
- Steps:
  1. Run `float.new`.
  2. Take a screenshot that includes the float.
- Expected result: The implementer can confirm the 6 elements in the title band, from the left: anchor kind icon, title, the agent state area, maximize, collapse, close. FAIL if any is missing or out of order.
- Teardown: Close the float.

### TC-016: Waiting-for-input display while collapsed
- Verifies: `AC-016` (`REQ-014`)
- Mode: human · Must-have: yes · Automation ref: N/A (visual) · Result: `NOT_RUN`
- Precondition: Start with F-0. Open the sidebar. Run F-2a in a split pane and note that card's attention colour.
- Steps:
  1. Create a float and run `sleep 5; ` followed by the F-2a command.
  2. Collapse the float within 5 seconds.
  3. Wait 10 seconds and take a screenshot.
  4. Close the float, and repeat 1 to 3 with F-2b and F-2c.
- Expected result: The implementer can confirm the following 3 points each time: the float stays collapsed, the title band's background colour is the same as the noted attention colour, and the state label is `approval needed` for F-2a, `reply needed` for F-2b and `error` for F-2c. FAIL if even one point is missing.
- Teardown: Close the float.

### TC-019: Jump from the sidebar and notifications
- Verifies: `AC-019` (`REQ-016`)
- Mode: human · Must-have: yes · Automation ref: N/A (visual) · Result: `NOT_RUN`
- Precondition: Start with F-0. In tab 1, create a float, run `sleep 8; ` followed by the F-2a command, and collapse it. Create and show tab 2. Open the sidebar and wait 8 seconds.
- Steps:
  1. Check that float's card in the sidebar, and select it.
  2. Redo the Precondition and run `NextNotification` (macOS `cmd+shift+j`, Linux `ctrl+shift+j`).
- Expected result: The implementer can confirm the following: the card carries the float mark. With both operations, tab 1 is shown, the float is expanded, and keystrokes go into the float. FAIL if even one is missing.
- Teardown: Close the tabs.

### TC-030: The background does not show through
- Verifies: `AC-030` (`REQ-022`)
- Mode: human · Must-have: yes · Automation ref: N/A (visual) · Result: `NOT_RUN`
- Precondition: `background-opacity = 0.5` in the settings. Run `yes ABCDEFGHIJ | head -200` in a split pane.
- Steps:
  1. Run `float.new`.
  2. Take a screenshot with the float's content area magnified to 400%.
- Expected result: The implementer cannot read `ABCDEFGHIJ` inside the float's rectangle. FAIL if it can be read.
- Teardown: Restore the settings.

### TC-034: Run on both OSes
- Verifies: `AC-034` (`CFR-004`)
- Mode: human · Must-have: yes · Automation ref: N/A (checking the execution record) · Result: `NOT_RUN`
- Precondition: The environments (a) and (b) of §3.
- Steps:
  1. Run this suite on (a) and record one row in §9.
  2. Run it on (b) at the same commit and record one row in §9.
- Expected result: §9 has two rows with the same Build, and both have must-have Failed and Blocked of 0.
- Teardown: none required

### TC-035: Overlays are above floats
- Verifies: `AC-035` (`REQ-008`)
- Mode: human · Must-have: yes · Automation ref: N/A (visual) · Result: `NOT_RUN`
- Precondition: Start with F-0. Create a float and maximize it. Run `sleep 600` in the float.
- Steps:
  1. Open the command palette, take a screenshot, and close it.
  2. Press the float's close button to bring up the confirmation dialog, take a screenshot, and cancel.
  3. Open Overview, take a screenshot, and close it.
- Expected result: In each of the 3 screenshots, the implementer can confirm that no float text or title band overlaps the overlay's area. FAIL if it overlaps in even one.
- Teardown: Close the float.

## 6. Non-functional procedures (`CFR-n`)

N/A. There is no measurement that involves load or sampling. CFR-001 to CFR-004 are covered by TC-032, TC-012, TC-033 and TC-034 in §5.

## 7. Coverage matrix

| AC | Must-have | Covered by | Mode | Gap |
|----|-----------|-----------|------|-----|
| AC-001 to AC-044 (excluding the 6 below) | yes | The TC with the same number | machine | — |
| AC-004, AC-016, AC-019, AC-030, AC-034, AC-035 | yes | The TC with the same number | human | — |

## 8. Defect handling

| Severity | Definition | Blocks exit |
|----------|-----------|-------------|
| S1 | Does not satisfy a must-have AC, or a process inside a float exits unintentionally | yes |
| S2 | A must-have AC can be satisfied only by a workaround procedure | yes |
| S3 | Does not satisfy an AC that is not must-have | no (record) |
| S4 | Appearance or wording | no (record) |

A must-have failure is sorted into either a build defect (fix and re-run) or an AC that cannot be built as written. The latter is not reinterpreted here; the spec is revised with `spec-dialogue floating-panes`.

## 9. Execution record

| Run | Date | Build | Executed by | Passed | Failed | Blocked | Skipped | Defects raised |
|-----|------|-------|-------------|--------|--------|---------|---------|----------------|
| 1 | | | | | | | | |

## 10. Residual risk

- Machine-mode cases check the computation of state and geometry. That the rectangles are actually drawn as computed on the real screen is checked only to the extent the 5 human cases (TC-004, 016, 019, 030, 035) touch it.
- The flicker at the moment a window-anchored float is redrawn on a macOS native tab switch is not judged.
- Human cases depend on one person's visual check.
- Linux is run on only one configuration (OQ-3).

## 11. Sign-off

| | |
|---|---|
| accepted by | |
| date | |
| verdict | |
