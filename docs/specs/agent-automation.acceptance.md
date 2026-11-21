# Acceptance Test Specification: Agent Automation Surface Upgrade

| | |
|---|---|
| slug | agent-automation |
| spec source | `docs/specs/agent-automation.md` as-of 2026-10-17 |
| status | locked |
| owner | simota |
| as-of | 2026-10-17 |
| review trigger | A change to the spec |

## 1. Test scope
- In scope: AC-001..033 (the new AppleScript verbs and properties, Noa Extensions, AX exposure, CFR-001..003).
- Out of scope: The items in the spec's Out of scope list. The ACs of the existing `applescript.md` are not covered here (AC-024 checks regression only).
- Not proven by this suite: `send key` on layouts other than US and Dvorak (JIS kana input, IME input sources). Reading from computer-use clients other than Claude. AXFrame on multiple monitors. The wait time of a read during the reflow of a huge scrollback (spec OQ-2).

## 2. Entry & exit criteria
| | Criterion |
|---|-----------|
| Entry | The spec is `locked`. A signed Noa.app built from the target commit with `scripts/bundle-macos.sh` is launched from a location other than `/Applications`. The capabilities in §3 are met |
| Exit | Every must-have TC is PASS. The results of the §6 procedures are recorded. There is no unresolved S1/S2 defect |

## 3. Environment
| Item | Value | How to obtain / set |
|------|-------|---------------------|
| Environment | A local Apple Silicon Mac, macOS 13 or later, one screen | OQ-1 (the assignment is TBD(simota)) |
| Build under test | A bundle of the release build of the target commit | `scripts/bundle-macos.sh` |
| Data state | A fresh launch. A state with nothing for session restore to use | Move aside the session files in `~/Library/Application Support/noa` before launching |
| Accounts & roles | The terminal used to run the tests (Ghostty or Terminal.app) has TCC Automation (→ Noa, System Events) and Accessibility granted | System Settings › Privacy & Security |
| Feature flags | `macos-applescript = true` (false only for TC-026) | Noa's config |
| External dependencies | None | — |
| Clock / timezone / locale | The input source is U.S. (Dvorak only for TC-014) | System Settings › Keyboard |

## 4. Test data
| Fixture | Purpose | Source | Reset procedure | Contains personal data |
|---------|---------|--------|-----------------|------------------------|
| snapshot fixture | AC-001 | Built inside the unit test | Generated per test | no |
| Terminal fixture (200×60 wide characters / 300 KiB scrollback) | AC-017, AC-022 | Written into a `Terminal` inside the unit test | Generated per test | no |
| Output of `seq 1 5000` | The scrollback of AC-016 and AC-031 | Run in a terminal | Close that terminal | no |

## 5. Test cases
The smoke tests are gathered in `scripts/agent-automation-smoke.sh` (new), and print each TC as a `PASS <TC-id>` / `FAIL <TC-id>` line. The existing `scripts/applescript-smoke.sh` is not changed, for TC-024. The smoke script decides absolute paths on the script side before handing them to the terminal (it does not depend on Noa's `$TMPDIR`). Markers follow the rule in spec L3.

| ID | Verifies | Title | Precondition | Steps | Expected result | Mode | Automation ref | Must-have | Priority | Result |
|----|----------|-------|--------------|-------|-----------------|------|----------------|-----------|----------|--------|
| TC-001 | AC-001 | Resolving references | — | `cargo test -p noa-app applescript_resolve` | Every Then of AC-001 is asserted | machine | unit | yes | CRITICAL | NOT_RUN |
| TC-002 | AC-002 | pid/tty | One terminal | Get `pid`/`tty`. On the smoke side, run `ps -o pid=,comm= -t ttysNNN` | The pid of the shell's line and the tty match | human | smoke | yes | HIGH | NOT_RUN |
| TC-003 | AC-003 | focused terminal e2e | The Given of AC-003 | The When of AC-003 | The same id | human | smoke | yes | HIGH | NOT_RUN |
| TC-004 | AC-004 | Parsing a record | — | `cargo test -p noa-app surface_configuration` | The Then of AC-004 is asserted | machine | unit | yes | HIGH | NOT_RUN |
| TC-005 | AC-005 | Configuration on 3 verbs | — | Run the When of AC-005 for each verb, wait 2 seconds, and read each `contents` | All three contain `CFG-/usr-bar` | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-006 | AC-006 | wait after command | — | Run (a) to (c) of AC-006 in order. For (d), press one key by hand | The Then of AC-006 | human | smoke + by eye | yes | HIGH | NOT_RUN |
| TC-007 | AC-007 | font size | A window at the default 14pt exists | Run the When of AC-007 and take a screenshot | The row-height ratio is 1.8–2.2, and the split is the same as its parent | human | — | yes | MEDIUM | NOT_RUN |
| TC-008 | AC-008 | new tab in | 2 windows | Get `count tabs` in both windows → `new tab in window 2` → wait 1 second → count again | window 2 is +1, window 1 is unchanged | human | smoke | yes | HIGH | NOT_RUN |
| TC-009 | AC-009 | Target of split and the 2 forms | The Given of AC-009 | The When of AC-009. Read the AX frames 250 ms after each operation | The Then of AC-009 | human | smoke | yes | HIGH | NOT_RUN |
| TC-010 | AC-010 | Using the return value at once | Noa has been `activate`d, 1 window | Run the When of AC-010 in one osascript | The Then of AC-010 | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-011 | AC-011 | perform action on | The Given of AC-011 | The When of AC-011. Read the AX frames 250 ms later | The Then of AC-011 | human | smoke | yes | HIGH | NOT_RUN |
| TC-012 | AC-012 | Parsing and encoding keys | — | `cargo test -p noa-app applescript_send_key` | The Then of AC-012 is asserted | machine | unit | yes | HIGH | NOT_RUN |
| TC-013 | AC-013 | send key e2e and DECCKM | US layout | (1) and (2) of AC-013. Wait 1 second at each stage and read `contents` | `^[[A`, `^[OA`, and the prompt returns | human | smoke | yes | HIGH | NOT_RUN |
| TC-014 | AC-014 | Translation by layout | The input source is Dvorak | The When of AC-014 → `contents` | The last line is `o` | human | — | yes | MEDIUM | NOT_RUN |
| TC-015 | AC-015 | With mouse tracking | The Given of AC-015 | The When of AC-015 → wait 1 second → `contents` and the cursor position | The 3 sequences are contained with the same c;r (c>1, r>1), and the cursor position is unchanged | human | smoke | yes | HIGH | NOT_RUN |
| TC-016 | AC-016 | Without mouse tracking | `seq 1 5000` has been run | Save `contents` → the When of AC-016 → `contents` | The number of the first line of the second read is smaller | human | smoke | yes | HIGH | NOT_RUN |
| TC-017 | AC-017 | Text extraction | — | `cargo test -p noa-app applescript_text` | The Then of AC-017 is asserted | machine | unit | yes | HIGH | NOT_RUN |
| TC-018 | AC-018 | Wide characters and secure input | Secure Keyboard Entry is ON in Noa's menu | The When of AC-018 | Contains `あいう-AC18` exactly once | human | smoke | yes | HIGH | NOT_RUN |
| TC-019 | AC-019 | AX tree | The Given of AC-019 | The When of AC-019, twice: with the palette closed and with it open (`cmd+shift+p`) | Both meet the Then of AC-019 | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-020 | AC-020 | AX focus, frame and following | The Given of AC-019 | (a) to (f) of AC-020 | The Then of AC-020 | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-021 | AC-021 | Selection and insertion point | The Given of AC-021 | The When of AC-021 | `hello`, `4`, `hello` | human | smoke | yes | MEDIUM | NOT_RUN |
| TC-024 | AC-024 | Regression of the existing smoke | — | Run the items of the smoke script as it was before the change | Every existing item PASSes | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-026 | AC-026 | Disabling | Put `macos-applescript = false` in the config and restart | The When of AC-026. Grep the diff to confirm there is no permission-UI code | The Then of AC-026 | human | smoke + review | yes | HIGH | NOT_RUN |
| TC-027 | AC-027 | Invalid input | One terminal. Close another terminal and record its id | Run the When of AC-027 one at a time | The error numbers come back in the order of the Then of AC-027. The Noa process is still alive | human | smoke | yes | HIGH | NOT_RUN |
| TC-028 | AC-028 | Checking the sdef | Appendix B has been transcribed to `crates/noa-app/tests/fixtures/ghostty-sdef-terms.txt` | `cargo test -p noa-app sdef` | Both check tests pass | machine | unit | yes | HIGH | NOT_RUN |
| TC-029 | AC-029 | close tab/window, count, exists | The Given of AC-029 | The When of AC-029 | The Then of AC-029 | human | smoke | yes | HIGH | NOT_RUN |
| TC-030 | AC-030 | pending registry | — | `cargo test -p noa-app applescript_pending` | The Then of AC-030 is asserted | machine | unit | yes | CRITICAL | NOT_RUN |
| TC-031 | AC-031 | scrollback and selection e2e | The Given of AC-031 | The When of AC-031 | The Then of AC-031 | human | smoke | yes | HIGH | NOT_RUN |
| TC-032 | AC-032 | point-to-cell conversion | — | `cargo test -p noa-app applescript_mouse_cell` | The Then of AC-032 is asserted | machine | unit | yes | HIGH | NOT_RUN |
| TC-033 | AC-033 | Compiling the close forms | A bundled Noa.app | `scripts/applescript-sdef-check.sh target/release/Noa.app` (outside the sandbox) | The Then of AC-033 | machine | script | yes | HIGH | NOT_RUN |

## 6. Non-functional procedures (`CFR-n`)
| ID | Verifies | CFR | Measure / criterion | Required outcome (from spec) | Procedure | Tool / observer | Conditions / sample | Observed result | Result |
|----|----------|-----|---------------------|------------------------------|-----------|-----------------|---------------------|-----------------|--------|
| TC-022 | AC-022 | CFR-001 | p95/p99 of (a)(b)(c) | see spec CFR-001 | `cargo test -p noa-app --release applescript_read_perf -- --ignored --nocapture` | The test output | The conditions (a)(b)(c) of spec CFR-001, 1000 runs each. The measuring thread has the same user-interactive QoS as the main thread (set inside the test) | — | NOT_RUN |
| TC-023 | AC-023 | CFR-002 | The idle RSS difference, and the number of new timers and threads | see spec CFR-002 | Run the idle measurement in `bench/` 3 times each on main and on the target build → compare the medians. Grep the diff for `spawn`/`Timer`/`NSTimer` | Harness + reviewer | 60 seconds after a fresh launch, one pane | — | NOT_RUN |
| TC-025 | AC-025 | CFR-003 | Waits inside handlers | see spec CFR-003 | Review the functions reachable from the AE and AX handlers, and check `recv`/`block_on`/`wait`/winit types | Reviewer | The diff of the target commit | — | NOT_RUN |

## 7. Coverage matrix
| AC | Must-have | Covered by | Mode | Gap |
|----|-----------|-----------|------|-----|
| AC-001 | yes | TC-001 | machine | — |
| AC-002 | yes | TC-002 | human | — |
| AC-003 | yes | TC-003 | human | — |
| AC-004 | yes | TC-004 | machine | — |
| AC-005 | yes | TC-005 | human | — |
| AC-006 | yes | TC-006 | human | — |
| AC-007 | yes | TC-007 | human | — |
| AC-008 | yes | TC-008 | human | — |
| AC-009 | yes | TC-009 | human | — |
| AC-010 | yes | TC-010 | human | — |
| AC-011 | yes | TC-011 | human | — |
| AC-012 | yes | TC-012 | machine | — |
| AC-013 | yes | TC-013 | human | — |
| AC-014 | yes | TC-014 | human | — |
| AC-015 | yes | TC-015 | human | — |
| AC-016 | yes | TC-016 | human | — |
| AC-017 | yes | TC-017 | machine | — |
| AC-018 | yes | TC-018 | human | — |
| AC-019 | yes | TC-019 | human | — |
| AC-020 | yes | TC-020 | human | — |
| AC-021 | yes | TC-021 | human | — |
| AC-022 | yes | TC-022 | machine | — |
| AC-023 | yes | TC-023 | machine + review | — |
| AC-024 | yes | TC-024 | human | — |
| AC-025 | yes | TC-025 | review | — |
| AC-026 | yes | TC-026 | human | — |
| AC-027 | yes | TC-027 | human | — |
| AC-028 | yes | TC-028 | machine | — |
| AC-029 | yes | TC-029 | human | — |
| AC-030 | yes | TC-030 | machine | — |
| AC-031 | yes | TC-031 | human | — |
| AC-032 | yes | TC-032 | machine | — |
| AC-033 | yes | TC-033 | machine | — |

## 8. Defect handling
| Severity | Definition | Blocks exit |
|----------|-----------|-------------|
| S1 | Does not meet a must-have AC, or crashes or hangs | yes |
| S2 | Meets a must-have AC only with a workaround | yes |
| S3 | Does not meet a non-must-have AC | no (record it) |
| S4 | Display and wording | no (record it) |

If a must-have TC fails, decide whether it is a defect in the build or the AC cannot be built as written. If it cannot be built, send it back with `spec-dialogue agent-automation`.

## 9. Execution record
| Run | Date | Build | Executed by | Passed | Failed | Blocked | Skipped | Defects raised |
|-----|------|-------|-------------|--------|--------|---------|---------|----------------|
| 1 | | | | | | | | |

## 10. Residual risk
- Most of the smoke tests are human procedures that need a signed bundle and TCC permissions. They do not run in CI.
- AX is checked through System Events. Reading from Claude computer use itself is not verified in this suite.
- TC-007 and TC-014 are judged by eye.

## 11. Sign-off
| | |
|---|---|
| accepted by | |
| date | |
| verdict | |
